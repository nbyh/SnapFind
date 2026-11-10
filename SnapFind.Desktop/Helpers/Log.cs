namespace SnapFind.Desktop.Helpers;

/// <summary>
/// 轻量诊断日志：写 <c>%TEMP%\snapfind-net.log</c>。
/// 仅用于排障（托盘/热键/IPC），不参与业务逻辑。
/// </summary>
public static class Log
{
    private static readonly object Gate = new();

    /// <summary>
    /// 与 Rust 侧同一开关策略：默认静默，仅在
    /// <c>SNAPFIND_LOG=1</c> 或 <c>%TEMP%\snapfind-log-enable</c> 存在时落盘。
    /// </summary>
    public static bool Enabled { get; set; } =
        Environment.GetEnvironmentVariable("SNAPFIND_LOG") is { Length: > 0 }
        || File.Exists(Path.Combine(Path.GetTempPath(), "snapfind-log-enable"));

    /// <summary>
    /// indexd 的 stderr 落盘位置（<c>&lt;index&gt;/logs/indexd.log</c>），由 IpcClient 在连接时设置。
    ///
    /// 与 <see cref="Enabled"/> 无关：GUI 进程没有控制台，indexd 的 eprintln 以前只在
    /// 打开诊断开关时进临时日志，默认情况下「索引服务异常退出」没有任何持久线索（R3-2）。
    /// </summary>
    public static string? IndexdLogFile { get; set; }

    /// <summary>追加一行 indexd 输出（带时间戳），超过 10MB 轮转一代。</summary>
    public static void WriteIndexdLine(string line)
    {
        var path = IndexdLogFile;
        if (string.IsNullOrEmpty(path)) return;
        try
        {
            var text = $"[{DateTime.Now:yyyy-MM-dd HH:mm:ss}] {line}{Environment.NewLine}";
            lock (Gate)
            {
                Directory.CreateDirectory(Path.GetDirectoryName(path)!);
                try
                {
                    if (File.Exists(path) && new FileInfo(path).Length > 10 * 1024 * 1024)
                        File.Move(path, path + ".old", overwrite: true);
                }
                catch { /* 轮转失败不影响写入 */ }
                File.AppendAllText(path, text);
            }
        }
        catch { /* 日志不影响主流程 */ }
    }

    public static void Write(string message)
    {
        if (!Enabled) return;
        try
        {
            var path = Path.Combine(Path.GetTempPath(), "snapfind-net.log");
            var line = $"[{DateTime.Now:HH:mm:ss.fff}] {message}{Environment.NewLine}";
            lock (Gate)
            {
                // 10MB 上限轮转：超限时把当前文件改名 .old（覆盖上一代）后重新开始。
                // 以前无上限，长期开着诊断日志的用户会攒出几百 MB 的单文件。
                try
                {
                    if (File.Exists(path) && new FileInfo(path).Length > 10 * 1024 * 1024)
                        File.Move(path, path + ".old", overwrite: true);
                }
                catch { /* 轮转失败不影响写入 */ }
                File.AppendAllText(path, line);
            }
        }
        catch { /* 诊断不影响主流程 */ }
    }
}
