#pragma warning disable CA1416
using System.Threading;
using SnapFind.Desktop.Helpers;

namespace SnapFind.Desktop.Services;

/// <summary>启动意图，由命令行参数决定。</summary>
public enum LaunchIntent
{
    Normal,
    Show,
    Settings,
    Quit,
    Minimized,
}

public readonly record struct LaunchArgs(LaunchIntent Intent, bool StartMinimized)
{
    public static LaunchArgs Parse(string[] args)
    {
        bool Has(params string[] names) => args.Any(a => names.Contains(a, StringComparer.OrdinalIgnoreCase));

        if (Has("--tray-quit", "--exit", "--quit")) return new(LaunchIntent.Quit, false);
        if (Has("--tray-settings")) return new(LaunchIntent.Settings, false);
        if (Has("--tray-show")) return new(LaunchIntent.Show, false);
        var minimized = Has("--minimized", "--minimize");
        return new(minimized ? LaunchIntent.Minimized : LaunchIntent.Normal, minimized);
    }
}

/// <summary>
/// 单实例守护。
///
/// 重复启动时以「带参数重新启动 exe」的方式驱动已有实例：
/// 互斥体 <c>SnapFind.SingleInstance.Mutex</c>、事件
/// <c>SnapFind.OpenSettings.Event</c> / <c>SnapFind.RequestExit.Event</c>。
/// </summary>
public sealed class SingleInstance : IDisposable
{
    private const string MutexName = "SnapFind.SingleInstance.Mutex";
    private const string SettingsEventName = "SnapFind.OpenSettings.Event";
    private const string ExitEventName = "SnapFind.RequestExit.Event";

    private readonly Mutex _mutex;
    private readonly EventWaitHandle _settingsEvent;
    private readonly EventWaitHandle _exitEvent;

    private SingleInstance(Mutex mutex, EventWaitHandle settings, EventWaitHandle exit)
    {
        _mutex = mutex;
        _settingsEvent = settings;
        _exitEvent = exit;
    }

    /// <summary>
    /// 尝试成为主实例。<paramref name="intent"/> 非 Normal/Minimized 时（说明本进程是被
    /// 托盘拉起的第二个实例）会激活已有窗口、转发事件并返回 <c>null</c>。
    /// </summary>
    public static SingleInstance? TryAcquire(LaunchIntent intent)
    {
        Mutex? mutex = null;
        var createdNew = true;
        try
        {
            mutex = new Mutex(initiallyOwned: true, MutexName, out createdNew);
        }
        catch
        {
            createdNew = true; // 无法创建就以单实例继续，宁可重复也不静默不启动
        }

        if (createdNew)
        {
            return new SingleInstance(
                mutex!,
                new EventWaitHandle(false, EventResetMode.AutoReset, SettingsEventName),
                new EventWaitHandle(false, EventResetMode.AutoReset, ExitEventName));
        }

        // 已有实例：先让窗口可见（隐藏时它不处理事件），再转发意图
        PlatformService.ActivateExisting();

        // 主实例可能还没建好事件（mutex 已占、事件未建是窗口初始化间隙）：
        // 以前一次失败就静默退出，表现为「点了设置/退出却毫无反应」。
        // 重试几轮，仍失败则记日志后放弃。
        for (var attempt = 0; attempt < 5; attempt++)
        {
            try
            {
                switch (intent)
                {
                    case LaunchIntent.Settings:
                        using (var e = EventWaitHandle.OpenExisting(SettingsEventName)) e.Set();
                        break;
                    case LaunchIntent.Quit:
                        using (var e = EventWaitHandle.OpenExisting(ExitEventName)) e.Set();
                        break;
                }
                break; // 转发成功
            }
            catch (WaitHandleCannotBeOpenedException) when (attempt < 4)
            {
                Thread.Sleep(300); // 主实例初始化中，稍后重试
            }
            catch (Exception ex)
            {
                Helpers.Log.Write($"single-instance intent forward failed: {ex.GetType().Name} {ex.Message}");
                break;
            }
        }

        mutex?.Dispose();
        return null;
    }

    /// <summary>取出一个待处理的控制意图；退出优先于设置。</summary>
    public LaunchIntent? Poll()
    {
        if (_exitEvent.WaitOne(0)) return LaunchIntent.Quit;
        if (_settingsEvent.WaitOne(0)) return LaunchIntent.Settings;
        return null;
    }

    public void Dispose()
    {
        _settingsEvent.Dispose();
        _exitEvent.Dispose();
        try { _mutex.ReleaseMutex(); } catch { /* 非持有线程调用时忽略 */ }
        _mutex.Dispose();
    }
}
