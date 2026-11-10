#pragma warning disable CA1416
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32;

namespace SnapFind.Desktop.Services;

internal static class Native
{
    public const int SW_SHOW = 5;
    public const int SW_RESTORE = 9;
    public const int SW_MAXIMIZE = 3;
    public const uint MOD_CONTROL = 0x0002;
    public const uint MOD_ALT = 0x0001;
    public const uint MOD_NOREPEAT = 0x4000;
    public const int WM_HOTKEY = 0x0312;

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern IntPtr FindWindowW(string? cls, string? title);

    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr hwnd, int cmd);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hwnd);

    [DllImport("user32.dll")]
    public static extern bool BringWindowToTop(IntPtr hwnd);

    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr hwnd);

    [DllImport("user32.dll")]
    public static extern bool RegisterHotKey(IntPtr hwnd, int id, uint mods, uint vk);

    [DllImport("user32.dll")]
    public static extern bool UnregisterHotKey(IntPtr hwnd, int id);

    [DllImport("user32.dll")]
    public static extern int GetMessageW(out MSG msg, IntPtr hwnd, uint min, uint max);

    [DllImport("user32.dll")]
    public static extern bool TranslateMessage(ref MSG msg);

    [DllImport("user32.dll")]
    public static extern IntPtr DispatchMessageW(ref MSG msg);

    [DllImport("user32.dll")]
    public static extern bool PostThreadMessageW(uint threadId, uint msg, IntPtr w, IntPtr l);

    [DllImport("kernel32.dll")]
    public static extern uint GetCurrentThreadId();

    [DllImport("user32.dll")]
    public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);

    [StructLayout(LayoutKind.Sequential)]
    public struct MSG
    {
        public IntPtr hwnd;
        public uint message;
        public IntPtr wParam;
        public IntPtr lParam;
        public uint time;
        public int ptX;
        public int ptY;
    }
}

public static class PlatformService
{
    /// <summary>窗口标题必须与托盘进程 <c>FindWindowW</c> 的匹配串一致。</summary>
    public const string WindowTitle = "SnapFind — 本地全文搜索";

    public static bool IsWindows => OperatingSystem.IsWindows();

    public static string AppVersion =>
        typeof(PlatformService).Assembly.GetName().Version?.ToString(3) ?? "0.1.0";

    // ---- 文件动作 --------------------------------------------------------

    public static void OpenFile(string path)
        => Process.Start(new ProcessStartInfo(path) { UseShellExecute = true })?.Dispose();

    public static void RevealInExplorer(string path)
    {
        if (IsWindows)
        {
            Process.Start(new ProcessStartInfo("explorer.exe", $"/select,\"{path}\"") { UseShellExecute = true })
                ?.Dispose();
        }
        else
        {
            var dir = Path.GetDirectoryName(path);
            if (!string.IsNullOrEmpty(dir)) OpenFile(dir);
        }
    }

    // ---- 窗口激活（单实例与热键共用）-------------------------------------

    public static IntPtr FindMainWindow() => IsWindows ? Native.FindWindowW(null, WindowTitle) : IntPtr.Zero;

    /// <summary>
    /// 激活已有窗口。先模拟一次 Alt 抬键满足前台转移条件——否则仅
    /// <c>SetForegroundWindow</c> 在后台进程里会被系统静默拒绝。
    /// </summary>
    public static bool ActivateExisting()
    {
        if (!IsWindows) return false;
        var hwnd = FindMainWindow();
        if (hwnd == IntPtr.Zero) return false;

        Native.keybd_event(0x12, 0, 0, UIntPtr.Zero);          // VK_MENU down
        Native.keybd_event(0x12, 0, 0x0002, UIntPtr.Zero);     // VK_MENU up

        Native.ShowWindow(hwnd, Native.SW_SHOW);
        Native.ShowWindow(hwnd, Native.SW_MAXIMIZE);
        Native.BringWindowToTop(hwnd);
        Native.SetForegroundWindow(hwnd);
        return true;
    }
}

/// <summary>开机自启：写入当前用户注册表 Run 项，参数带 <c>--minimized</c>。</summary>
public static class AutostartService
{
    private const string RunKey = @"Software\Microsoft\Windows\CurrentVersion\Run";
    private const string ValueName = "SnapFind";

    public static bool Supported => PlatformService.IsWindows;

    public static bool IsEnabled()
    {
        if (!Supported) return false;
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(RunKey);
            return key?.GetValue(ValueName) is string s && s.Length > 0;
        }
        catch { return false; }
    }

    public static bool Set(bool enabled)
    {
        if (!Supported) return false;
        try
        {
            using var key = Registry.CurrentUser.CreateSubKey(RunKey, writable: true);
            if (key is null) return false;
            if (enabled)
            {
                var exe = Environment.ProcessPath ?? "";
                key.SetValue(ValueName, $"\"{exe}\" --minimized", RegistryValueKind.String);
            }
            else
            {
                key.DeleteValue(ValueName, throwOnMissingValue: false);
            }
            return true;
        }
        catch { return false; }
    }
}

/// <summary>
/// 全局热键 Ctrl+Alt+F。系统直接把 WM_HOTKEY 投给注册线程，不经过 explorer，
/// 因此在托盘图标失效时仍是可靠入口。
/// </summary>
public sealed class HotkeyService : IDisposable
{
    private const int HotkeyId = 0xB001;
    private Thread? _thread;
    private uint _threadId;
    private volatile bool _running;

    public event Action? Pressed;

    public bool Start()
    {
        if (!PlatformService.IsWindows) return false;
        _running = true;
        _thread = new Thread(Loop) { IsBackground = true, Name = "snapfind-hotkey" };
        _thread.SetApartmentState(ApartmentState.STA);
        _thread.Start();
        return true;
    }

    private void Loop()
    {
        _threadId = Native.GetCurrentThreadId();
        var ok = Native.RegisterHotKey(IntPtr.Zero, HotkeyId, Native.MOD_CONTROL | Native.MOD_ALT, 0x46);
        Helpers.Log.Write($"hotkey: RegisterHotKey Ctrl+Alt+F -> {ok}");
        if (!ok)
        {
            _running = false;
            return;
        }
        try
        {
            while (_running && Native.GetMessageW(out var msg, IntPtr.Zero, 0, 0) > 0)
            {
                if (msg.message == Native.WM_HOTKEY && msg.wParam.ToInt32() == HotkeyId)
                {
                    Helpers.Log.Write("hotkey: pressed");
                    try { Pressed?.Invoke(); } catch { /* ignore */ }
                }
                Native.TranslateMessage(ref msg);
                Native.DispatchMessageW(ref msg);
            }
        }
        finally
        {
            Native.UnregisterHotKey(IntPtr.Zero, HotkeyId);
        }
    }

    public void Dispose()
    {
        _running = false;
        if (_threadId != 0 && PlatformService.IsWindows)
        {
            Native.PostThreadMessageW(_threadId, 0x0012 /* WM_QUIT */, IntPtr.Zero, IntPtr.Zero);
        }
        _thread?.Join(500);
        _thread = null;
    }
}
