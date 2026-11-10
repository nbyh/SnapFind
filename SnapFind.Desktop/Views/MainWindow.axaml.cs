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

    public MainWindow(ShellViewModel shell, bool startMinimized)
    {
        InitializeComponent();
        _shell = shell;
        _startMinimized = startMinimized;
        DataContext = shell;

        try { Icon = TrayService.CreateIcon(); } catch { /* 图标非关键 */ }

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
        if (!PlatformService.IsWindows || !_shell.Config.TrayEnabled) return;

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
