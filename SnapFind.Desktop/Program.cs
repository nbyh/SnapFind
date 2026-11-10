using Avalonia;

namespace SnapFind.Desktop;

internal static class Program
{
    [STAThread]
    public static int Main(string[] args)
    {
        try
        {
            BuildAvaloniaApp().StartWithClassicDesktopLifetime(args);
            return 0;
        }
        catch (Exception ex)
        {
            // Linux 上最常见的启动失败是「不在图形会话」或「缺 X11 库」，
            // 默认输出是异常堆栈，对用户没有指导意义。给出人话提示。
            Console.Error.WriteLine($"SnapFind 图形界面启动失败：{ex.Message}");
            Console.Error.WriteLine("常见原因：");
            Console.Error.WriteLine("  · 在 SSH/纯终端环境运行——GUI 必须在桌面会话里启动（需要 DISPLAY 或 WAYLAND_DISPLAY）");
            Console.Error.WriteLine("  · 缺少 X11 库——Debian/Ubuntu: sudo apt install libx11-6 libxrandr2 libxi6 libxcursor1 libfontconfig1");
            Console.Error.WriteLine("                                Fedora/RHEL:  sudo dnf install libX11 libXrandr libXi libXcursor fontconfig");
            Console.Error.WriteLine("  · 只想用命令行管理索引：请运行同目录的小写 ./snapfind（本程序是图形界面）");
            return 1;
        }
    }

    public static AppBuilder BuildAvaloniaApp() => AppBuilder
        .Configure<App>()
        .UsePlatformDetect()
        .LogToTrace();
}
