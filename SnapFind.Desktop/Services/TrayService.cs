using Avalonia;
using Avalonia.Controls;
using Avalonia.Media;
using Avalonia.Media.Imaging;

namespace SnapFind.Desktop.Services;

/// <summary>
/// 通知区域图标（Avalonia 内置 <see cref="TrayIcon"/>）。
/// 菜单语义：显示 / 设置 / 退出；单击图标恢复主窗口。
/// </summary>
public sealed class TrayService : IDisposable
{
    private TrayIcon? _icon;
    private bool _disposed;

    public event Action? ShowRequested;
    public event Action? SettingsRequested;
    public event Action? QuitRequested;

    public bool Start()
    {
        try
        {
            var icon = new TrayIcon
            {
                Icon = CreateIcon(),
                ToolTipText = "SnapFind — 本地全文搜索",
                IsVisible = true,
            };

            var menu = new NativeMenu();
            var show = new NativeMenuItem("显示");
            show.Click += (_, _) => ShowRequested?.Invoke();
            var settings = new NativeMenuItem("设置");
            settings.Click += (_, _) => SettingsRequested?.Invoke();
            var quit = new NativeMenuItem("退出");
            quit.Click += (_, _) => QuitRequested?.Invoke();
            menu.Items.Add(show);
            menu.Items.Add(settings);
            menu.Items.Add(new NativeMenuItemSeparator());
            menu.Items.Add(quit);
            icon.Menu = menu;

            // 单击即恢复（与 Rust 托盘 v4 协议的语义一致：v4 不区分单击/双击，
            // 双击自然表现为两次单击，同样会恢复窗口）。
            icon.Clicked += (_, _) => ShowRequested?.Invoke();

            if (Application.Current is { } app)
            {
                TrayIcon.SetIcons(app, new TrayIcons { icon });
            }
            _icon = icon;
            return true;
        }
        catch (Exception ex)
        {
            Helpers.Log.Write($"tray: create failed {ex.GetType().Name} {ex.Message}");
            return false;
        }
    }

    /// <summary>
    /// 程序 / 通知区域图标：透明圆角渐变底 + 白色放大镜（代码绘制，不依赖资源文件）。
    ///
    /// 用 256px 渲染再交给系统缩放，缩到 16px 托盘尺寸时仍是清晰的"蓝紫渐变方块 + 白色镜片"。
    /// </summary>
    public static WindowIcon CreateIcon()
    {
        const int size = 256;
        var rtb = new RenderTargetBitmap(new PixelSize(size, size), new Vector(96, 96));
        using (var ctx = rtb.CreateDrawingContext())
        {
            // 圆角渐变底（四角透明，托盘里不带生硬的黑底方块）
            var gradient = new LinearGradientBrush
            {
                StartPoint = new RelativePoint(0, 0, RelativeUnit.Relative),
                EndPoint = new RelativePoint(1, 1, RelativeUnit.Relative),
                GradientStops =
                {
                    new GradientStop(Color.Parse("#4C8DFF"), 0),
                    new GradientStop(Color.Parse("#8B5CF6"), 1),
                },
            };
            var body = new Rect(size * 0.03, size * 0.03, size * 0.94, size * 0.94);
            ctx.DrawRectangle(gradient, null, new RoundedRect(body, size * 0.24));

            // 镜片
            var ring = new Pen(Brushes.White, size * 0.082, lineCap: PenLineCap.Round);
            var center = new Point(size * 0.44, size * 0.43);
            var radius = size * 0.205;
            ctx.DrawEllipse(null, ring, center, radius, radius);

            // 手柄
            var handle = new Pen(Brushes.White, size * 0.105, lineCap: PenLineCap.Round);
            ctx.DrawLine(handle, new Point(size * 0.595, size * 0.585),
                new Point(size * 0.775, size * 0.765));

            // 镜片内的高光点，暗示"命中目标"
            ctx.DrawEllipse(new SolidColorBrush(Color.FromArgb(235, 255, 255, 255)), null,
                new Point(size * 0.375, size * 0.363), size * 0.042, size * 0.042);
        }

        using var ms = new MemoryStream();
        rtb.Save(ms);
        ms.Position = 0;
        return new WindowIcon(ms);
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        try
        {
            if (_icon is not null)
            {
                _icon.IsVisible = false;
                if (Application.Current is { } app) TrayIcon.SetIcons(app, new TrayIcons());
                _icon.Dispose();
            }
        }
        catch { /* ignore */ }
    }
}
