using System.Buffers.Binary;
using System.Collections.Concurrent;
using System.Diagnostics;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using System.Threading.Channels;
using SnapFind.Desktop.Helpers;

namespace SnapFind.Desktop.Ipc;

/// <summary>
/// indexd 客户端：拉起子进程、握手、请求-响应、流式查询与事件队列。
///
/// 与 Rust 侧 <c>snapfind-ipc-client</c> 语义保持一致：
/// 命名管道、4 字节大端长度 + JSON 帧、按 <c>reply_to</c> 路由、
/// 持有子进程 stdin 以便本进程退出时触发其 EOF 自退。
/// </summary>
public sealed class IpcClient : IAsyncDisposable
{
    private const int MaxFrameBytes = 8 * 1024 * 1024;

    private readonly NamedPipeClientStream _pipe;
    private readonly Process _child;
    private readonly CancellationTokenSource _cts = new();
    private readonly ConcurrentDictionary<string, Channel<Envelope>> _pending = new();
    private readonly Channel<Envelope> _events = Channel.CreateUnbounded<Envelope>();
    private readonly SemaphoreSlim _writeLock = new(1, 1);
    private readonly Task _readLoop;
    private int _seq;
    private bool _disposed;
    private int _disconnectedFired;

    /// <summary>
    /// 连接断开（子进程退出 / 管道读写失败 / 心跳失败）时触发。
    /// 只触发一次；主动 <see cref="DisposeAsync"/> 不触发。
    /// UI 层应订阅它自动重启 indexd，而不是等下一次请求报 Pipe is broken。
    /// </summary>
    public event Action? Disconnected;

    /// <summary>诊断日志（写入 %TEMP%\snapfind-net.log）。</summary>
    private static void Diag(string message) => Helpers.Log.Write(message);

    private IpcClient(NamedPipeClientStream pipe, Process child)
    {
        _pipe = pipe;
        _child = child;
        _readLoop = Task.Factory.StartNew(ReadLoop, TaskCreationOptions.LongRunning);
        _ = Task.Run(HeartbeatLoop);
        // 子进程退出监控：indexd 崩溃/被杀时第一时间通知 UI，不必等下一次请求失败
        _ = Task.Run(() =>
        {
            try { _child.WaitForExit(); } catch { /* ignore */ }
            RaiseDisconnected();
        });
    }

    /// <summary>
    /// 心跳：indexd 连续 3 个周期（默认 15s）收不到任何入站帧就判定 UI 失联并主动断开。
    /// 不心跳会表现为「空闲十几秒后所有请求报 Pipe is broken」。
    /// </summary>
    private async Task HeartbeatLoop()
    {
        while (!_cts.IsCancellationRequested)
        {
            try
            {
                await Task.Delay(TimeSpan.FromSeconds(5), _cts.Token).ConfigureAwait(false);
                await HeartbeatAsync(_cts.Token).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                return;
            }
            catch
            {
                Diag("heartbeat failed -> stop");
                RaiseDisconnected();
                return;
            }
        }
    }

    public bool IsAlive => !_cts.IsCancellationRequested && !_child.HasExited;

    private void RaiseDisconnected()
    {
        if (_disposed || _cts.IsCancellationRequested) return;
        if (Interlocked.Exchange(ref _disconnectedFired, 1) != 0) return;
        Diag("connection lost");
        Disconnected?.Invoke();
    }

    // ---- 建立连接 --------------------------------------------------------

    public static async Task<IpcClient> ConnectAsync(string? indexDir, CancellationToken ct = default)
    {
        var indexd = LocateIndexd()
            ?? throw new FileNotFoundException(
                "找不到 snapfind-indexd.exe。请先构建 Rust 侧，或把本程序与 indexd 放在同一目录。");

        // pin 由「本进程 id + 随机数」组成，生成规则必须与 snapfind_core::platform::ipc_endpoint 一致。
        var pin = $"{Environment.ProcessId}-{Random.Shared.Next(0, 100_000)}";
        var token = Convert.ToHexString(System.Security.Cryptography.RandomNumberGenerator.GetBytes(32)).ToLowerInvariant();
        // 端点：Windows 为命名管道 \\.\pipe\snapfind-{pin}；
        // Linux 为 UDS 绝对路径 $XDG_RUNTIME_DIR/snapfind-{uid}/{pin}.sock
        //（无 XDG_RUNTIME_DIR 时 /tmp/snapfind-{uid}/{pin}.sock），与 Rust 侧完全一致。
        var endpoint = OperatingSystem.IsWindows()
            ? $@"\\.\pipe\snapfind-{pin}"
            : BuildUnixSocketEndpoint(pin);

        // R3-2：先把 indexd 的 stderr 落盘位置定下来。解析顺序与 indexd 侧一致：
        // 显式传入 > config.index.root > **indexd 可执行文件同级** index/
        // （最后一步必须按 indexd 自身目录算：开发期它与桌面端不在同一目录，
        //   按桌面端目录猜会写到 indexd 根本不会用的地方）
        var indexRoot = indexDir;
        if (string.IsNullOrWhiteSpace(indexRoot))
        {
            var configured = Services.AppConfig.Load().IndexRoot;
            indexRoot = string.IsNullOrWhiteSpace(configured)
                ? Path.Combine(Path.GetDirectoryName(indexd)!, "index")
                : configured;
        }
        Log.IndexdLogFile = Path.Combine(indexRoot, "logs", "indexd.log");

        var psi = new ProcessStartInfo(indexd)
        {
            UseShellExecute = false,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            CreateNoWindow = true,
            // indexd 的 eprintln 是 UTF-8；不指定时 .NET 按系统控制台编码（GBK）
            // 解码，错误信息全变乱码，无法诊断问题
            StandardOutputEncoding = Encoding.UTF8,
            StandardErrorEncoding = Encoding.UTF8,
        };
        psi.ArgumentList.Add("--pipe");
        psi.ArgumentList.Add(pin);
        psi.ArgumentList.Add("--token");
        psi.ArgumentList.Add(token);
        if (!string.IsNullOrWhiteSpace(indexDir))
        {
            psi.ArgumentList.Add("--index-dir");
            psi.ArgumentList.Add(indexDir);
        }

        // 锁冲突(code=2)自动重试：旧 indexd 收尾时索引锁尚未释放，新实例会立即退出。
        // indexd 侧已改为原地等待旧实例（最多 15s），这里每秒重试一轮兜底。
        var lastLockBusy = new StringBuilder();
        for (var attempt = 0; attempt < 6; attempt++)
        {
            if (attempt > 0) await Task.Delay(1000, ct).ConfigureAwait(false);
            try
            {
                return await SpawnAndConnectAsync(psi, endpoint, token, ct).ConfigureAwait(false);
            }
            catch (IndexdEarlyExitException ex) when (ex.ExitCode == 2)
            {
                lastLockBusy.AppendLine(ex.Message);
                Diag($"indexd lock busy, retry {attempt + 1}/6");
            }
        }
        throw new TimeoutException(
            $"索引服务启动失败：索引目录锁被旧实例占用（旧进程可能正在落盘检查点）。{Environment.NewLine}{lastLockBusy}");
    }

    /// <summary>indexd 启动即退出（带退出码），用于区分锁冲突(code=2)与其他致命错误。</summary>
    private sealed class IndexdEarlyExitException(string message, int exitCode) : InvalidOperationException(message)
    {
        public int ExitCode { get; } = exitCode;
    }

    /// <summary>
    /// 计算 Linux 下的 UDS 端点路径，与 <c>snapfind_core::platform::runtime_dir/ipc_endpoint</c> 保持一致：
    /// 有 XDG_RUNTIME_DIR 时为 <c>$XDG_RUNTIME_DIR/{pin}.sock</c>（直接用运行时目录本身），
    /// 否则 <c>/tmp/snapfind-{uid}/{pin}.sock</c>；/tmp 回退目录不存在则创建并设 0700。
    /// </summary>
    private static string BuildUnixSocketEndpoint(string pin)
    {
        var runtimeDir = Environment.GetEnvironmentVariable("XDG_RUNTIME_DIR");
        if (!string.IsNullOrWhiteSpace(runtimeDir))
        {
            return Path.Combine(runtimeDir, $"{pin}.sock");
        }

        var uid = getuid();
        var dir = Path.Combine("/tmp", $"snapfind-{uid}");
        Directory.CreateDirectory(dir);
        try
        {
            File.SetUnixFileMode(dir, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
        }
        catch { /* 权限尽力而为 */ }
        return Path.Combine(dir, $"{pin}.sock");
    }

    [System.Runtime.InteropServices.DllImport("libc")]
    private static extern uint getuid();

    private static async Task<IpcClient> SpawnAndConnectAsync(
        ProcessStartInfo psi, string endpoint, string token, CancellationToken ct)
    {
        var child = Process.Start(psi) ?? throw new InvalidOperationException("无法启动 snapfind-indexd.exe");

        // stderr 必须持续排空，否则管道缓冲写满会让 indexd 阻塞；
        // 同时留一份到日志，便于诊断「连上但无响应」这类问题。
        var stderr = new StringBuilder();
        child.ErrorDataReceived += (_, e) =>
        {
            if (e.Data is null) return;
            lock (stderr) stderr.AppendLine(e.Data);
            Diag($"indexd stderr: {e.Data}");
            Log.WriteIndexdLine(e.Data); // 落盘（不受诊断开关控制）
        };
        child.BeginErrorReadLine();
        _ = child.StandardOutput.ReadToEndAsync();

        Diag($"indexd started pid={child.Id} endpoint={endpoint}");
        string TakeStderr()
        {
            lock (stderr) return stderr.ToString();
        }

        // 必须用 Asynchronous：同步模式下客户端写入在 UI 线程上会被阻塞
        // （实测 hello 帧 233 字节就卡死，界面随之冻结）。
        // Unix 上 NamedPipeClientStream 的 pipeName 即 UDS socket 路径，
        // 必须传完整绝对路径（不能像 Windows 那样 GetFileName 只留管道名）。
        var pipeName = OperatingSystem.IsWindows() ? Path.GetFileName(endpoint) : endpoint;
        var pipe = new NamedPipeClientStream(".", pipeName, PipeDirection.InOut, PipeOptions.Asynchronous);
        try
        {
            var connected = false;
            // 300×50ms = 15s：与 indexd 侧的等锁上限一致（旧实例收尾时新实例会等到锁释放）
            for (var i = 0; i < 300; i++)
            {
                ct.ThrowIfCancellationRequested();
                if (child.HasExited)
                {
                    throw new IndexdEarlyExitException(
                        $"索引进程启动后立即退出（code={child.ExitCode}）{Environment.NewLine}{TakeStderr()}",
                        child.ExitCode);
                }
                try
                {
                    pipe.Connect(50);
                    connected = true;
                    break;
                }
                catch (TimeoutException)
                {
                    if (i == 299) throw;
                }
            }
            if (!connected) throw new TimeoutException("未能连接管道");
        }
        catch (IndexdEarlyExitException)
        {
            try { child.Kill(true); } catch { /* ignore */ }
            pipe.Dispose();
            throw;
        }
        catch (Exception ex)
        {
            try { child.Kill(true); } catch { /* ignore */ }
            pipe.Dispose();
            throw new TimeoutException($"索引进程启动超时（端点 {endpoint}）：{ex.Message}{Environment.NewLine}{TakeStderr()}", ex);
        }

        Diag("pipe connected");

        var client = new IpcClient(pipe, child);
        try
        {
            await client.HandshakeAsync(token, ct).ConfigureAwait(false);
        }
        catch
        {
            await client.DisposeAsync().ConfigureAwait(false);
            throw;
        }
        return client;
    }

    /// <summary>
    /// 定位 indexd：优先同目录，其次沿父目录向上找 <c>target/release|debug</c>（开发期），
    /// 最后读环境变量 <c>SNAPFIND_INDEXD</c>。
    /// </summary>
    private static string? LocateIndexd()
    {
        var env = Environment.GetEnvironmentVariable("SNAPFIND_INDEXD");
        if (!string.IsNullOrWhiteSpace(env) && File.Exists(env)) return env;

        var exeName = OperatingSystem.IsWindows() ? "snapfind-indexd.exe" : "snapfind-indexd";
        var baseDir = AppContext.BaseDirectory;
        var direct = Path.Combine(baseDir, exeName);
        if (File.Exists(direct)) return direct;

        var dir = new DirectoryInfo(baseDir);
        for (var i = 0; i < 8 && dir is not null; i++, dir = dir.Parent)
        {
            foreach (var cfg in new[] { "release", "debug" })
            {
                var candidate = Path.Combine(dir.FullName, "target", cfg, exeName);
                if (File.Exists(candidate)) return candidate;
            }
        }
        return null;
    }

    private async Task HandshakeAsync(string token, CancellationToken ct)
    {
        var hello = new Hello
        {
            Token = token,
            BuildId = Environment.GetEnvironmentVariable("SNAPFIND_BUILD_ID") ?? "",
            SupportedVersions = new List<uint> { 1 },
            Encodings = new List<string> { "json" },
            Role = "ui",
        };

        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(ct);
        timeout.CancelAfter(TimeSpan.FromSeconds(5));

        Envelope ack;
        try
        {
            Diag("sending hello");
            ack = await RequestAsync(Msg.Hello, hello, timeout.Token).ConfigureAwait(false);
            Diag($"hello reply type={ack.Type}");
        }
        catch (OperationCanceledException)
        {
            Diag($"hello timeout; child exited={_child.HasExited}");
            throw new TimeoutException("握手超时：索引进程未在 5 秒内响应 hello");
        }

        if (ack.Type == Msg.Error)
        {
            var err = ack.Decode<ErrorPayload>();
            throw new InvalidOperationException($"握手失败: {err?.Message ?? ack.Payload.ToString()}");
        }
        if (ack.Type != Msg.HelloAck)
        {
            throw new InvalidOperationException($"握手异常响应: {ack.Type}");
        }
    }

    // ---- 收发 ------------------------------------------------------------

    private string NextId() => $"ui-{Environment.ProcessId}-{Interlocked.Increment(ref _seq)}";

    private async Task WriteFrameAsync(Envelope env)
    {
        var body = JsonSerializer.SerializeToUtf8Bytes(env, IpcJson.Options);
        if (body.Length > MaxFrameBytes) throw new InvalidOperationException("frame too large");

        var frame = new byte[4 + body.Length];
        BinaryPrimitives.WriteUInt32BigEndian(frame, (uint)body.Length);
        body.CopyTo(frame, 4);

        await _writeLock.WaitAsync().ConfigureAwait(false);
        try
        {
            await _pipe.WriteAsync(frame).ConfigureAwait(false);
            await _pipe.FlushAsync().ConfigureAwait(false);
            Diag($"write done type={env.Type} len={body.Length}");
        }
        catch (Exception ex)
        {
            Diag($"write failed type={env.Type}: {ex.GetType().Name} {ex.Message}");
            throw;
        }
        finally
        {
            _writeLock.Release();
        }
    }

    private void ReadLoop()
    {
        var lenBuf = new byte[4];
        try
        {
            while (!_cts.IsCancellationRequested)
            {
                if (!ReadExact(lenBuf, 4)) break;
                var len = (int)BinaryPrimitives.ReadUInt32BigEndian(lenBuf);
                if (len <= 0 || len > MaxFrameBytes) break;

                var body = new byte[len];
                if (!ReadExact(body, len)) break;

                Envelope? env;
                try { env = JsonSerializer.Deserialize<Envelope>(body, IpcJson.Options); }
                catch { continue; }
                if (env is null) continue;

                if (env.ReplyTo is { } rid && _pending.TryGetValue(rid, out var ch))
                {
                    ch.Writer.TryWrite(env);
                    if (env.IsFinal) _pending.TryRemove(rid, out _);
                }
                else
                {
                    _events.Writer.TryWrite(env);
                }
            }
        }
        catch (Exception)
        {
            // 管道关闭 / 进程退出：让等待中的请求和事件消费者结束
        }
        finally
        {
            foreach (var kv in _pending) kv.Value.Writer.TryComplete();
            _pending.Clear();
            _events.Writer.TryComplete();
            RaiseDisconnected();
        }
    }

    private bool ReadExact(byte[] buf, int count)
    {
        var read = 0;
        while (read < count)
        {
            var n = _pipe.Read(buf, read, count - read);
            if (n <= 0) return false;
            read += n;
        }
        return true;
    }

    /// <summary>发请求并等待最终帧（流式响应只取最后一帧）。</summary>
    public async Task<Envelope> RequestAsync(string type, object? payload, CancellationToken ct = default)
    {
        return await CallAsync(type, payload, null, ct).ConfigureAwait(false);
    }

    public async Task<T?> RequestAsync<T>(string type, object? payload, CancellationToken ct = default)
    {
        var env = await RequestAsync(type, payload, ct).ConfigureAwait(false);
        if (env.Type == Msg.Error) return default;
        return env.Decode<T>();
    }

    /// <summary>
    /// 与 <see cref="RequestAsync{T}"/> 相同，但后端返回 error 帧时抛异常。
    /// 用于「暂停/继续/重建/清理」这类必须让用户看到失败原因的操作。
    /// </summary>
    public async Task<T?> RequestStrictAsync<T>(string type, object? payload, CancellationToken ct = default)
    {
        var env = await RequestAsync(type, payload, ct).ConfigureAwait(false);
        if (env.Type == Msg.Error)
        {
            var err = env.Decode<ErrorPayload>();
            throw new InvalidOperationException(err?.Message ?? $"索引服务返回错误（{type}）");
        }
        return env.Decode<T>();
    }

    /// <summary>发请求；<paramref name="onFrame"/> 会收到每一帧（流式查询分批）。</summary>
    public async Task<Envelope> CallAsync(
        string type,
        object? payload,
        Action<Envelope>? onFrame,
        CancellationToken ct = default)
    {
        var id = NextId();
        var env = new Envelope
        {
            Id = id,
            Type = type,
            Ts = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
            Payload = payload is null
                ? JsonSerializer.SerializeToElement(new { })
                : JsonSerializer.SerializeToElement(payload, payload.GetType(), IpcJson.Options),
        };

        var ch = Channel.CreateUnbounded<Envelope>(new UnboundedChannelOptions { SingleReader = true });
        _pending[id] = ch;
        try
        {
            await WriteFrameAsync(env).ConfigureAwait(false);
            while (true)
            {
                Envelope frame;
                try
                {
                    frame = await ch.Reader.ReadAsync(ct).ConfigureAwait(false);
                }
                catch (ChannelClosedException)
                {
                    throw new IOException("索引进程已断开");
                }
                onFrame?.Invoke(frame);
                if (frame.IsFinal || frame.Type == Msg.Error) return frame;
            }
        }
        finally
        {
            _pending.TryRemove(id, out _);
        }
    }

    /// <summary>流式查询：每个 chunk 回调一次，返回最终帧。</summary>
    public Task<Envelope> QueryAsync(QueryRequest req, Action<QueryChunk> onChunk, CancellationToken ct = default)
    {
        return CallAsync(Msg.QueryExecute, req, env =>
        {
            if (env.Type != Msg.QueryResponse) return;
            var chunk = env.Decode<QueryChunk>();
            if (chunk is not null) onChunk(chunk);
        }, ct);
    }

    public Task CancelAsync(string requestId, CancellationToken ct = default)
        => RequestAsync(Msg.QueryCancel, new { request_id = requestId }, ct);

    public Task HeartbeatAsync(CancellationToken ct = default)
        => RequestAsync(Msg.Heartbeat, new { kind = "ping" }, ct);

    /// <summary>取出一个待处理事件（供 UI 线程轮询）。</summary>
    public bool TryNextEvent(out Envelope env) => _events.Reader.TryRead(out env!);

    // ---- 关闭 ------------------------------------------------------------

    public async Task ShutdownAsync(int timeoutMs = 5000)
    {
        try
        {
            using var cts = new CancellationTokenSource(TimeSpan.FromMilliseconds(timeoutMs));
            await RequestAsync(Msg.Shutdown, new { reason = "user", timeout_ms = timeoutMs }, cts.Token)
                .ConfigureAwait(false);
        }
        catch { /* 超时直接强杀 */ }

        try
        {
            if (!_child.HasExited && !_child.WaitForExit(1500)) _child.Kill(true);
        }
        catch { /* ignore */ }
    }

    public async ValueTask DisposeAsync()
    {
        _disposed = true;
        _cts.Cancel();
        foreach (var kv in _pending) kv.Value.Writer.TryComplete();
        try { _pipe.Dispose(); } catch { /* ignore */ }
        try { await _readLoop.ConfigureAwait(false); } catch { /* ignore */ }
        try { if (!_child.HasExited) _child.Kill(true); } catch { /* ignore */ }
        _cts.Dispose();
    }
}
