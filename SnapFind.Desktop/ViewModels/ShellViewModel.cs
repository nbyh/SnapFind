using System.Collections.ObjectModel;
using Avalonia.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using SnapFind.Desktop.Ipc;
using SnapFind.Desktop.Services;

namespace SnapFind.Desktop.ViewModels;

public enum Phase { Boot, Setup, Main }

public enum ShellView { Search, Index, Settings }

/// <summary>
/// 主控：连接 indexd、驱动启动流程、消费事件、承载四个页面、协调托盘与单实例。
/// </summary>
public sealed partial class ShellViewModel : ObservableObject
{
    private IpcClient? _client;
    private readonly DispatcherTimer _pollTimer;
    private DateTime? _pendingStatsAt;
    private bool _forceQuit;
    private int _statsSeq;
    private int _reconnecting;

    public ShellViewModel(AppConfig config, SingleInstance? instance)
    {
        Config = config;
        Instance = instance;

        Search = new SearchViewModel(() => _client, SetNotice, config.DebounceMs, config.QueryTimeoutMs);
        Index = new IndexViewModel(this);
        Settings = new SettingsViewModel(this);
        Setup = new SetupViewModel(this);

        _pollTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(100) };
        _pollTimer.Tick += (_, _) => Poll();
    }

    public AppConfig Config { get; }
    public SingleInstance? Instance { get; }

    public SearchViewModel Search { get; }
    public IndexViewModel Index { get; }
    public SettingsViewModel Settings { get; }
    public SetupViewModel Setup { get; }

    /// <summary>由 App 注入：切换 Avalonia 主题变体。</summary>
    public Action<string>? ThemeApplier { get; set; }

    /// <summary>请求主窗口显示 / 隐藏 / 关闭（由 MainWindow 订阅）。</summary>
    public event Action? ShowWindowRequested;
    public event Action? HideWindowRequested;

    // ---- 顶层状态 --------------------------------------------------------

    [ObservableProperty] private Phase _phase = Phase.Boot;
    [ObservableProperty] private ShellView _view = ShellView.Search;
    [ObservableProperty] private string? _notice;
    [ObservableProperty] private string? _bootError;
    [ObservableProperty] private ScopePreview? _scope;
    [ObservableProperty] private Stats? _stats;
    [ObservableProperty] private bool _trayReady;

    /// <summary>左侧导航是否折叠为图标栏。</summary>
    [ObservableProperty] private bool _navCollapsed;

    public double NavWidth => NavCollapsed ? 60 : 216;
    public bool NavExpanded => !NavCollapsed;

    partial void OnNavCollapsedChanged(bool value)
    {
        OnPropertyChanged(nameof(NavWidth));
        OnPropertyChanged(nameof(NavExpanded));
    }

    [RelayCommand]
    private void ToggleNav() => NavCollapsed = !NavCollapsed;

    public bool HasNotice => !string.IsNullOrEmpty(Notice);

    partial void OnNoticeChanged(string? value) => OnPropertyChanged(nameof(HasNotice));

    public bool IsMain => Phase == Phase.Main;
    public bool IsBoot => Phase == Phase.Boot;
    public bool IsSetup => Phase == Phase.Setup;
    public bool SearchVisible => View == ShellView.Search;
    public bool IndexVisible => View == ShellView.Index;
    public bool SettingsVisible => View == ShellView.Settings;

    /// <summary>标题栏右侧显示的当前页面名。</summary>
    public string CurrentPageTitle => View switch
    {
        ShellView.Index => "索引",
        ShellView.Settings => "设置",
        _ => "搜索",
    };

    partial void OnViewChanged(ShellView value) => OnPropertyChanged(nameof(CurrentPageTitle));

    public string NavSummary
    {
        get
        {
            if (Stats is null) return "";
            return $"{Stats.TotalDocs} 文档 · {Helpers.Format.Human(Stats.TotalIndexBytes)}";
        }
    }

    public string NavMemory
    {
        get
        {
            if (Stats is null) return "";
            return $"内存 {Helpers.Format.Human(Stats.RssBytes)}/{Helpers.Format.Human(Stats.MemoryBudgetBytes)}";
        }
    }

    public string NavVersion => $"v{PlatformService.AppVersion}";

    // ---- 启动 ------------------------------------------------------------

    public async Task InitializeAsync()
    {
        ApplyTheme(Config.Theme);

        try
        {
            _client = await IpcClient.ConnectAsync(null);
        HookClient(_client);
        }
        catch (Exception ex)
        {
            BootError = ex.Message;
            Phase = Phase.Main;
            View = ShellView.Index;
            Index.ScopeError = $"索引服务不可用：{ex.Message}";
            Index.HasScopeError = true;
            OnPhaseChanged();
            // 首连失败（杀软扫描拖慢 indexd 启动、索引目录锁未释放等）不能只靠
            // 用户手动重试：后台自动走重启-重连流程，成功后清除错误提示
            _ = ReconnectAsync();
            return;
        }

        _pollTimer.Start();

        var ok = await Task.WhenAll(
            RefreshScopeAsync(),
            RefreshStatsAsync());

        if (!ok[0] || !ok[1])
        {
            // 启动查询失败：进索引页让用户看到错误并重试
            Phase = Phase.Main;
            View = ShellView.Index;
            OnPhaseChanged();
            return;
        }

        var indexed = Stats?.Volumes.Any(v =>
            v.Docs > 0 || string.Equals(v.State, "Watching", StringComparison.OrdinalIgnoreCase)) ?? false;

        Phase = indexed ? Phase.Main : Phase.Setup;
        View = ShellView.Search;
        OnPhaseChanged();
    }

    private void OnPhaseChanged()
    {
        OnPropertyChanged(nameof(IsMain));
        OnPropertyChanged(nameof(IsBoot));
        OnPropertyChanged(nameof(IsSetup));
        OnPropertyChanged(nameof(SearchVisible));
        OnPropertyChanged(nameof(IndexVisible));
        OnPropertyChanged(nameof(SettingsVisible));
    }

    // ---- IPC 请求 --------------------------------------------------------

    public IpcClient RequireClient()
    {
        if (_client is { IsAlive: true }) return _client;
        // 死管道上发请求只会得到 "Pipe is broken"：立即触发自动重启（幂等），
        // 本次请求仍会失败，但提示可理解，且后续操作大概率已恢复。
        _ = Dispatcher.UIThread.InvokeAsync(() => _ = ReconnectAsync());
        throw new InvalidOperationException("索引进程未运行，正在自动重启索引服务…");
    }

    public async Task<T?> RequestAsync<T>(string type, object? payload)
    {
        var client = RequireClient();
        return await client.RequestAsync<T>(type, payload);
    }

    /// <summary>需要明确失败提示的操作（暂停/继续/重建/清理）走这里：后端 error 帧会抛异常。</summary>
    public async Task<T?> RequestStrictAsync<T>(string type, object? payload)
    {
        var client = RequireClient();
        return await client.RequestStrictAsync<T>(type, payload);
    }

    public async Task<bool> RefreshScopeAsync()
    {
        try
        {
            var scope = await RequestAsync<ScopePreview>(Msg.ScopePreview, new { });
            if (scope is null) return false;
            Scope = scope;
            Setup.ApplyScope(scope);
            Index.ApplyScope(scope);
            return true;
        }
        catch (Exception ex)
        {
            Index.ScopeError = $"索引服务不可用：{ex.Message}";
            Index.HasScopeError = true;
            return false;
        }
    }

    public async Task<bool> RefreshStatsAsync()
    {
        try
        {
            var stats = await RequestAsync<Stats>(Msg.StatsGet, new { });
            if (stats is null) return false;
            Stats = stats;
            Index.ApplyStats(stats);
            OnPropertyChanged(nameof(NavSummary));
            OnPropertyChanged(nameof(NavMemory));
            return true;
        }
        catch (Exception ex)
        {
            Index.ScopeError = $"索引服务不可用：{ex.Message}";
            Index.HasScopeError = true;
            return false;
        }
    }

    /// <summary>延迟刷新统计（index.start 后 1.2s、volume.action 后 0.4s）。</summary>
    public void ScheduleStatsRefresh(int delayMs)
    {
        var my = ++_statsSeq;
        _ = Task.Run(async () =>
        {
            await Task.Delay(delayMs);
            await Dispatcher.UIThread.InvokeAsync(async () =>
            {
                if (my != _statsSeq) return;
                await RefreshStatsAsync();
            });
        });
    }

    // ---- 断线自愈 --------------------------------------------------------

    private void HookClient(IpcClient client) => client.Disconnected += OnClientDisconnected;

    private void OnClientDisconnected()
    {
        if (_forceQuit) return;
        // 事件来自后台线程：切回 UI 线程再更新状态与重连
        _ = Dispatcher.UIThread.InvokeAsync(() => _ = ReconnectAsync());
    }

    /// <summary>
    /// 重启 indexd 并重建连接（最多 5 次、间隔递增）。
    ///
    /// indexd 是单连接服务：UI 断开后服务端即退出，重连等于重启进程；
    /// 旧进程持有的索引目录文件锁要等进程退出才释放，因此前几次重试
    /// 可能遇到「启动后立即退出」，属预期，由退避重试吸收。
    /// </summary>
    public async Task ReconnectAsync()
    {
        if (Interlocked.CompareExchange(ref _reconnecting, 1, 0) != 0) return;
        try
        {
            SetNotice("索引服务已断开，正在重启索引进程…");
            var lastError = "";
            for (var attempt = 1; attempt <= 5; attempt++)
            {
                await Task.Delay(TimeSpan.FromSeconds(attempt));
                try
                {
                    if (_client is not null)
                    {
                        await _client.DisposeAsync();
                        _client = null;
                    }
                    var client = await IpcClient.ConnectAsync(null);
                    _client = client;
                    HookClient(client);
                    _pollTimer.Start();
                    Index.ScopeError = "";
                    Index.HasScopeError = false;
                    await Task.WhenAll(RefreshScopeAsync(), RefreshStatsAsync());
                    SetNotice("索引服务已重新连接。");
                    return;
                }
                catch (Exception ex)
                {
                    lastError = ex.Message;
                    Helpers.Log.Write($"reconnect attempt {attempt} failed: {ex.GetType().Name} {ex.Message}");
                }
            }
            Index.ScopeError = $"索引服务不可用：{lastError}";
            Index.HasScopeError = true;
            SetNotice("索引服务重启失败，30 秒后自动重试（持续尝试直到恢复）…");
            // 不就此放弃：排定下一轮自动重启，直到成功或程序退出。
            // 典型场景是旧 indexd 收尾慢（大检查点落盘）导致锁长时间被占用。
            if (!_forceQuit)
            {
                _ = Task.Run(async () =>
                {
                    await Task.Delay(TimeSpan.FromSeconds(30));
                    await Dispatcher.UIThread.InvokeAsync(() => _ = ReconnectAsync());
                });
            }
        }
        finally
        {
            Interlocked.Exchange(ref _reconnecting, 0);
        }
    }

    /// <summary>确保有可用连接；没有就先走一轮重启，返回是否已连上。</summary>
    public async Task<bool> EnsureConnectedAsync()
    {
        if (_client is { IsAlive: true }) return true;
        await ReconnectAsync();
        return _client is { IsAlive: true };
    }

    // ---- 事件轮询 --------------------------------------------------------

    private void Poll()
    {
        if (Instance is { } inst && inst.Poll() is { } intent)
        {
            switch (intent)
            {
                case LaunchIntent.Quit:
                    _forceQuit = true;
                    HideWindowRequested?.Invoke();
                    RequestExit?.Invoke();
                    return;
                case LaunchIntent.Settings:
                    ShowWindowRequested?.Invoke();
                    GotoSettings();
                    return;
            }
        }

        if (_client is null) return;
        while (_client.TryNextEvent(out var env)) HandleEvent(env);

        if (_pendingStatsAt is { } at && DateTime.UtcNow >= at)
        {
            _pendingStatsAt = null;
            _ = RefreshStatsAsync();
        }
    }

    public event Action? RequestExit;

    private void HandleEvent(Envelope env)
    {
        switch (env.Type)
        {
            case Msg.EventProgress:
            {
                var p = env.Decode<Progress>();
                if (p is not null) Index.ApplyProgress(p);
                break;
            }
            case Msg.EventVolumeState:
            {
                var e = env.Decode<VolumeStateEvent>();
                if (e is null) break;
                // 排队等待并行槽位、或打开索引目录失败的卷不会产生 progress 事件，
                // stats 里也查不到（尚未进入内存卷表）——VolumeState 是它们唯一的
                // 可见信号，必须立即建卡，否则「点了索引却看不到任务」。
                Index.ApplyVolumeState(e.VolumeId, e.Mount, e.State);
                if (string.Equals(e.State, "Watching", StringComparison.OrdinalIgnoreCase))
                {
                    SetNotice($"{e.Mount} 索引就绪：{e.FilesDone} 个文件");
                }
                else if (!string.IsNullOrEmpty(e.Reason))
                {
                    SetNotice($"{e.Mount} 已降级：{e.Reason}");
                }
                _pendingStatsAt = DateTime.UtcNow.AddMilliseconds(300);
                break;
            }
            case Msg.EventIndexDegraded:
            {
                var reason = env.Payload.TryGetProperty("reason", out var r) ? r.GetString() : null;
                SetNotice($"索引目录已降级：{reason}");
                break;
            }
            case Msg.EventWatchOverflow:
                SetNotice("变更过多，已降级为目录重扫。");
                break;
        }
    }

    // ---- 导航与提示 ------------------------------------------------------

    [RelayCommand]
    private void GotoSearch()
    {
        View = ShellView.Search;
        OnPhaseChanged();
    }

    [RelayCommand]
    private void GotoIndex()
    {
        View = ShellView.Index;
        OnPhaseChanged();
        _ = RefreshStatsAsync();
    }

    [RelayCommand]
    private void GotoSettings()
    {
        View = ShellView.Settings;
        OnPhaseChanged();
        Settings.Load();
    }

    public void ShowIndexView()
    {
        View = ShellView.Index;
        OnPhaseChanged();
    }

    public void EnterMain()
    {
        Phase = Phase.Main;
        OnPhaseChanged();
    }

    /// <summary>
    /// 保存 config.json，失败时给出提示而不是让异常从 UI 事件里抛出去。
    /// 设置页每个开关都会走到这里：磁盘只读/已满、文件被杀软占用都可能失败，
    /// 以前直接 <c>Config.Save()</c> 会让异常冒到 UI 线程（还可能带崩应用）。
    /// </summary>
    public bool TrySaveConfig()
    {
        try
        {
            Config.Save();
            return true;
        }
        catch (Exception ex)
        {
            Helpers.Log.Write($"config save failed: {ex.GetType().Name} {ex.Message}");
            SetNotice($"设置保存失败：{ex.Message}");
            return false;
        }
    }

    public void SetNotice(string message)
    {
        Notice = message;
        // 提示是瞬时的：5 秒后自动消失，避免一直挂在状态栏
        var current = message;
        _ = Task.Run(async () =>
        {
            await Task.Delay(5000);
            await Dispatcher.UIThread.InvokeAsync(() =>
            {
                if (Notice == current) Notice = null;
            });
        });
    }

    public void ClearBootError()
    {
        BootError = null;
        Index.ScopeError = "";
        Index.HasScopeError = false;
    }

    public void ApplyTheme(string pref)
        => ThemeApplier?.Invoke(pref);

    // ---- 窗口 ------------------------------------------------------------

    public bool ShouldHideOnClose => Config.CloseToTray && TrayReady && !_forceQuit;

    public void HideToTray()
    {
        SetNotice("已最小化到托盘：点击托盘图标可重新打开。");
        HideWindowRequested?.Invoke();
    }

    public void RequestShowWindow()
    {
        Notice = null;
        ShowWindowRequested?.Invoke();
    }

    public void BeginExit()
    {
        _forceQuit = true;
        RequestExit?.Invoke();
    }

    // ---- 关闭 ------------------------------------------------------------

    public async Task ShutdownAsync()
    {
        _pollTimer.Stop();
        if (_client is not null)
        {
            await _client.ShutdownAsync(5000);
            await _client.DisposeAsync();
            _client = null;
        }
    }
}
