using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Interactivity;
using Avalonia.Threading;
using SnapFind.Desktop.Services;
using SnapFind.Desktop.ViewModels;

namespace SnapFind.Desktop.Views;

public partial class MainWindow : Window
{
    private readonly ShellViewModel _shell;
    private readonly bool _startMinimized;
    private TrayService? _tray;
    private HotkeyService? _hotkey;
    private bool _reallyClose;
    private bool _initialized;
    private bool _noSystemDecorations;

    public MainWindow(ShellViewModel shell, bool startMinimized)
    {
        InitializeComponent();
        _shell = shell;
        _startMinimized = startMinimized;
        DataContext = shell;

        try { Icon = TrayService.CreateIcon(); } catch { /* 图标非关键 */ }

        // Linux 上 fontconfig 默认回退到的字体（Latin 用 DejaVu、中文随机回退）
        // 笔画明显偏细偏淡；显式指定常用黑体族并把整窗字重提到 Medium，
        // 让观感接近 Windows（Segoe UI / 微软雅黑）。Windows 保持默认。
        if (!OperatingSystem.IsWindows())
        {
            FontFamily = new Avalonia.Media.FontFamily(
                "Noto Sans CJK SC, Source Han Sans SC, WenQuanYi Micro Hei, Noto Sans, DejaVu Sans");
            FontWeight = Avalonia.Media.FontWeight.Medium;

            // X11 后端忽略 ExtendClientAreaChromeHints="NoChrome"（空实现），
            // WM 仍会画自己的标题栏按钮，与自绘标题栏叠成两行。
            // 直接关掉系统装饰；拖动/双击最大化由自绘标题栏承担，
            // 边缘调整大小见 OnPointerMoved/OnPointerPressed 的补齐逻辑。
            _noSystemDecorations = true;
            SystemDecorations = SystemDecorations.None;
        }

        _shell.ShowWindowRequested += ShowFromTray;
        _shell.HideWindowRequested += HideToTray;
        _shell.RequestExit += () =>
        {
            _reallyClose = true;
            ShowFromTray();
            Close();
        };

        Opened += OnOpened;
        Closing += OnClosing;
    }

    /// <summary>
    /// 只在首次显示时初始化。
    ///
    /// `Show()`（从托盘恢复）会**再次触发 Opened**，若不加守卫会重复创建托盘图标、
    /// 重复注册热键（第二次必然失败）并多拉起一个 indexd（撞索引锁后自退）。
    /// </summary>
    private async void OnOpened(object? sender, EventArgs e)
    {
        if (_initialized) return;
        _initialized = true;

        StartTray();
        StartHotkey();

        WindowState = _startMinimized ? WindowState.Minimized : WindowState.Maximized;
        if (_startMinimized) Hide();

        await _shell.InitializeAsync();
    }

    // ---- 自绘标题栏 ------------------------------------------------------

    private void OnTitleBarPointerPressed(object? sender, PointerPressedEventArgs e)
    {
        // 双击标题栏 = 最大化 / 还原
        if (e.ClickCount == 2)
        {
            ToggleMaximize();
            e.Handled = true;
            return;
        }

        if (!e.GetCurrentPoint(this).Properties.IsLeftButtonPressed) return;
        if (WindowState == WindowState.Maximized) return; // 最大化状态下交给系统处理

        BeginMoveDrag(e);
    }

    private void OnMinimizeClick(object? sender, RoutedEventArgs e)
        => WindowState = WindowState.Minimized;

    private void OnMaximizeClick(object? sender, RoutedEventArgs e)
        => ToggleMaximize();

    private void OnCloseClick(object? sender, RoutedEventArgs e)
        => Close();

    private void ToggleMaximize()
        => WindowState = WindowState == WindowState.Maximized
            ? WindowState.Normal
            : WindowState.Maximized;

    // ---- 无边框窗口的边缘调整大小 ----------------------------------------
    // SystemDecorations=None 后 WM 不再提供边缘缩放，自行补齐：
    // 指针靠近窗口边缘时改光标，按下即 BeginResizeDrag（X11 走
    // _NET_WM_MOVERESIZE 交给 WM 处理，无边框同样有效）。

    private const double EdgeHit = 6;

    private WindowEdge? HitTestEdge(Point p)
    {
        if (p.X < EdgeHit)
        {
            if (p.Y < EdgeHit) return WindowEdge.NorthWest;
            if (p.Y > Bounds.Height - EdgeHit) return WindowEdge.SouthWest;
            return WindowEdge.West;
        }
        if (p.X > Bounds.Width - EdgeHit)
        {
            if (p.Y < EdgeHit) return WindowEdge.NorthEast;
            if (p.Y > Bounds.Height - EdgeHit) return WindowEdge.SouthEast;
            return WindowEdge.East;
        }
        if (p.Y < EdgeHit) return WindowEdge.North;
        if (p.Y > Bounds.Height - EdgeHit) return WindowEdge.South;
        return null;
    }

    private void UpdateResizeCursor(Point p)
    {
        Cursor = HitTestEdge(p) switch
        {
            WindowEdge.North => new Cursor(StandardCursorType.TopSide),
            WindowEdge.South => new Cursor(StandardCursorType.BottomSide),
            WindowEdge.West => new Cursor(StandardCursorType.LeftSide),
            WindowEdge.East => new Cursor(StandardCursorType.RightSide),
            WindowEdge.NorthWest => new Cursor(StandardCursorType.TopLeftCorner),
            WindowEdge.NorthEast => new Cursor(StandardCursorType.TopRightCorner),
            WindowEdge.SouthWest => new Cursor(StandardCursorType.BottomLeftCorner),
            WindowEdge.SouthEast => new Cursor(StandardCursorType.BottomRightCorner),
            _ => Cursor.Default,
        };
    }

    protected override void OnPointerMoved(PointerEventArgs e)
    {
        if (_noSystemDecorations && WindowState == WindowState.Normal)
            UpdateResizeCursor(e.GetPosition(this));
        base.OnPointerMoved(e);
    }

    protected override void OnPointerPressed(PointerPressedEventArgs e)
    {
        if (_noSystemDecorations && WindowState == WindowState.Normal
            && HitTestEdge(e.GetPosition(this)) is { } edge)
        {
            BeginResizeDrag(edge, e);
        }
        base.OnPointerPressed(e);
    }

    protected override void OnPropertyChanged(AvaloniaPropertyChangedEventArgs change)
    {
        base.OnPropertyChanged(change);
        if (change.Property == WindowStateProperty)
        {
            UpdateMaximizeGlyph();
        }
    }

    private void UpdateMaximizeGlyph()
    {
        if (MaxGlyph is null) return;
        // 原用 Segoe MDL2 字形（E922 最大化 / E923 向下还原），Linux 无该字体；
        // 现切换内置矢量几何资源（R3-1）
        var key = WindowState == WindowState.Maximized ? "IcoRestore" : "IcoMax";
        if (Resources[key] is Avalonia.Media.StreamGeometry g) MaxGlyph.Data = g;
    }

    // ---- 托盘 / 热键 -----------------------------------------------------

    private void StartTray()
    {
        // TrayService 用的是 Avalonia 内置跨平台 TrayIcon（Linux 走 DBus
        // StatusNotifierItem，KDE/UKUI 原生支持，GNOME 需 AppIndicator 扩展），
        // 不再限定 Windows。托盘起不来时 TrayReady=false，关窗会直接退出并提示。
        if (!_shell.Config.TrayEnabled) return;

        try
        {
            var tray = new TrayService();
            tray.ShowRequested += ShowFromTray;
            tray.SettingsRequested += () =>
            {
                ShowFromTray();
                _shell.GotoSettingsCommand.Execute(null);
            };
            tray.QuitRequested += RequestQuit;

            if (tray.Start())
            {
                _tray = tray;
                _shell.TrayReady = true;
                Helpers.Log.Write("tray: started");
            }
            else
            {
                tray.Dispose();
                _shell.TrayReady = false;
                _shell.SetNotice("托盘图标不可用：关闭窗口将直接退出。");
                Helpers.Log.Write("tray: start failed");
            }
        }
        catch (Exception ex)
        {
            // 图标绘制/系统限制抛异常不能带崩启动：降级为「无托盘」并明确告知
            _shell.TrayReady = false;
            _shell.SetNotice($"托盘图标创建失败（{ex.Message}）：关闭窗口将直接退出。");
            Helpers.Log.Write($"tray: ctor/start threw {ex.GetType().Name}: {ex.Message}");
        }
    }

    private void StartHotkey()
    {
        if (!PlatformService.IsWindows) return;
        var hotkey = new HotkeyService();
        hotkey.Pressed += () => Dispatcher.UIThread.Post(ShowFromTray);
        if (hotkey.Start()) _hotkey = hotkey;
        else hotkey.Dispose();
    }

    // ---- 显示 / 隐藏 -----------------------------------------------------

    private void ShowFromTray()
    {
        Helpers.Log.Write($"tray: show requested (visible={IsVisible})");
        if (!IsVisible) Show();
        WindowState = WindowState.Maximized;
        Activate();
        Topmost = true;
        Topmost = false;
        Helpers.Log.Write($"tray: shown visible={IsVisible}");
    }

    private void HideToTray()
    {
        Helpers.Log.Write("tray: hide requested");
        Hide();
    }

    protected override void OnKeyDown(KeyEventArgs e)
    {
        if (ConfirmOverlay.IsVisible)
        {
            if (e.Key == Key.Escape)
            {
                OnConfirmCancel(null, new RoutedEventArgs());
                e.Handled = true;
                return;
            }
            if (e.Key == Key.Enter)
            {
                OnConfirmQuit(null, new RoutedEventArgs());
                e.Handled = true;
                return;
            }
        }
        base.OnKeyDown(e);
    }

    // ---- 退出确认 --------------------------------------------------------

    /// <summary>与会话前的窗口状态，用于取消后恢复原状。</summary>
    private bool _wasHiddenBeforeConfirm;
    private bool _wasMinimizedBeforeConfirm;

    /// <summary>
    /// 托盘右键「退出」：先确认再退出。
    ///
    /// 退出会停掉 indexd，搜索（含托盘与全局热键）随之失效，所以这里必须让用户明确确认；
    /// 从托盘发起时窗口可能是隐藏/最小化的，先显示出来再盖一层确认卡片。
    /// </summary>
    private void RequestQuit()
    {
        _wasHiddenBeforeConfirm = !IsVisible;
        _wasMinimizedBeforeConfirm = WindowState == WindowState.Minimized;

        if (_wasHiddenBeforeConfirm)
        {
            Show();
            WindowState = WindowState.Normal;
            Activate();
        }
        else if (_wasMinimizedBeforeConfirm)
        {
            WindowState = WindowState.Normal;
        }

        ConfirmOverlay.IsVisible = true;
        ConfirmCancel.Focus();
    }

    private void OnConfirmCancel(object? sender, RoutedEventArgs e)
    {
        ConfirmOverlay.IsVisible = false;

        if (_wasHiddenBeforeConfirm)
        {
            _wasHiddenBeforeConfirm = false;
            Hide();
        }
        else if (_wasMinimizedBeforeConfirm)
        {
            _wasMinimizedBeforeConfirm = false;
            WindowState = WindowState.Minimized;
        }
    }

    private void OnConfirmQuit(object? sender, RoutedEventArgs e)
    {
        ConfirmOverlay.IsVisible = false;
        _reallyClose = true;
        Close();
    }

    // ---- 关闭 ------------------------------------------------------------

    private void OnClosing(object? sender, WindowClosingEventArgs e)
    {
        if (_reallyClose)
        {
            ShutdownBlocking();
            return;
        }

        if (_shell.ShouldHideOnClose)
        {
            // 拦截关闭：隐藏到托盘而不是退出进程
            e.Cancel = true;
            _shell.HideToTray();
            return;
        }

        ShutdownBlocking();
    }

    /// <summary>落检查点后退出索引进程；最多等 6 秒，避免关窗口时卡死。</summary>
    private void ShutdownBlocking()
    {
        try
        {
            Task.Run(async () => await _shell.ShutdownAsync()).Wait(TimeSpan.FromSeconds(6));
        }
        catch { /* 退出路径不抛 */ }

        _hotkey?.Dispose();
        _hotkey = null;
        _tray?.Dispose();
        _tray = null;
        _shell.Instance?.Dispose();
    }
}
