using Avalonia;
using Avalonia.Controls.ApplicationLifetimes;
using Avalonia.Markup.Xaml;
using Avalonia.Styling;
using SnapFind.Desktop.Services;
using SnapFind.Desktop.ViewModels;
using SnapFind.Desktop.Views;

namespace SnapFind.Desktop;

public partial class App : Application
{
    private ShellViewModel? _shell;

    public override void Initialize() => AvaloniaXamlLoader.Load(this);

    public override void OnFrameworkInitializationCompleted()
    {
        if (ApplicationLifetime is IClassicDesktopStyleApplicationLifetime desktop)
        {
            var args = LaunchArgs.Parse(Environment.GetCommandLineArgs().Skip(1).ToArray());

            // 单实例：第二个实例只负责把意图转给主实例，然后安静退出
            var instance = SingleInstance.TryAcquire(args.Intent);
            if (instance is null)
            {
                desktop.Shutdown(0);
                base.OnFrameworkInitializationCompleted();
                return;
            }

            InstallCrashHandlers();

            var config = AppConfig.Load(out var configWarning);
            _shell = new ShellViewModel(config, instance) { ThemeApplier = ApplyTheme };
            ApplyTheme(config.Theme);
            // 配置文件损坏/读取失败时明确告知（以前静默恢复默认值，用户只看到设置全没了）
            if (!string.IsNullOrEmpty(configWarning)) _shell.SetNotice(configWarning);

            desktop.MainWindow = new MainWindow(_shell, args.StartMinimized);
        }

        base.OnFrameworkInitializationCompleted();
    }

    /// <summary>
    /// 崩溃兜底（计划 R3-3）：任何未处理异常都追加到 <c>%TEMP%\snapfind-crash.log</c>。
    /// 这个文件不受「诊断日志开关」控制——崩溃日志恰恰要在用户没开诊断时也能拿到。
    /// UI 线程异常只记录并提示，不让应用直接闪退；后台线程异常记录后仍按默认流程终止。
    /// </summary>
    private static readonly object CrashGate = new();

    private void InstallCrashHandlers()
    {
        void Report(string source, Exception ex)
        {
            try
            {
                var path = Path.Combine(Path.GetTempPath(), "snapfind-crash.log");
                var line = $"[{DateTime.Now:yyyy-MM-dd HH:mm:ss.fff}] [{source}] " +
                           $"{ex.GetType().FullName}: {ex.Message}{Environment.NewLine}{ex.StackTrace}" +
                           $"{Environment.NewLine}---{Environment.NewLine}";
                lock (CrashGate) File.AppendAllText(path, line);
            }
            catch { /* 崩溃兜底不能再抛 */ }
        }

        AppDomain.CurrentDomain.UnhandledException += (_, e) =>
            Report("appdomain", (Exception)e.ExceptionObject);
        TaskScheduler.UnobservedTaskException += (_, e) =>
        {
            Report("task", e.Exception);
            e.SetObserved(); // 已落盘，吞掉以免进程被终止
        };
        Avalonia.Threading.Dispatcher.UIThread.UnhandledException += (_, e) =>
        {
            Report("ui", e.Exception);
            e.Handled = true;
            // 尽量给出可见反馈：后台线程异常引发的闪退以前什么线索都没有
            if (_shell is { } shell)
            {
                shell.Notice = shell.Notice is { Length: > 0 } n
                    ? $"{n}　|　发生内部错误（详情见 %TEMP%\\snapfind-crash.log）"
                    : "发生内部错误，界面已恢复；详情见 %TEMP%\\snapfind-crash.log";
            }
        };
    }

    private void ApplyTheme(string pref)
    {
        RequestedThemeVariant = pref switch
        {
            "light" => ThemeVariant.Light,
            "dark" => ThemeVariant.Dark,
            _ => ThemeVariant.Default,
        };
    }
}
