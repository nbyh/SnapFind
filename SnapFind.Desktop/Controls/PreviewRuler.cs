using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Media;

namespace SnapFind.Desktop.Controls;

/// <summary>
/// 预览正文右侧的加粗标尺（兼作滚动条）。
///
/// 与 Rust 版 <c>preview_panel</c> 的标尺一致：
/// * 灰色滑块 = 当前视口；
/// * 蓝色标记 = 搜索关键词命中行；
/// * 橙色标记 = Ctrl+F 命中行（当前命中更亮）；
/// * **单击优先吸附最近的彩色标记**（阈值 6px），拖动则按位置比例连续滚动。
/// </summary>
public class PreviewRuler : Control
{
    public static readonly StyledProperty<int> TotalLinesProperty =
        AvaloniaProperty.Register<PreviewRuler, int>(nameof(TotalLines));

    public static readonly StyledProperty<IReadOnlyList<int>?> QueryHitsProperty =
        AvaloniaProperty.Register<PreviewRuler, IReadOnlyList<int>?>(nameof(QueryHits));

    public static readonly StyledProperty<IReadOnlyList<int>?> FindHitsProperty =
        AvaloniaProperty.Register<PreviewRuler, IReadOnlyList<int>?>(nameof(FindHits));

    public static readonly StyledProperty<int> FindCurrentLineProperty =
        AvaloniaProperty.Register<PreviewRuler, int>(nameof(FindCurrentLine), -1);

    /// <summary>视口顶部占全文的比例（0~1）。</summary>
    public static readonly StyledProperty<double> ViewTopProperty =
        AvaloniaProperty.Register<PreviewRuler, double>(nameof(ViewTop));

    /// <summary>视口高度占全文的比例（0~1）。</summary>
    public static readonly StyledProperty<double> ViewHeightProperty =
        AvaloniaProperty.Register<PreviewRuler, double>(nameof(ViewHeight), 1.0);

    public static readonly StyledProperty<IBrush?> TrackBrushProperty =
        AvaloniaProperty.Register<PreviewRuler, IBrush?>(nameof(TrackBrush));

    public static readonly StyledProperty<IBrush?> HandleBrushProperty =
        AvaloniaProperty.Register<PreviewRuler, IBrush?>(nameof(HandleBrush));

    public static readonly StyledProperty<IBrush?> QueryMarkBrushProperty =
        AvaloniaProperty.Register<PreviewRuler, IBrush?>(nameof(QueryMarkBrush));

    public static readonly StyledProperty<IBrush?> FindMarkBrushProperty =
        AvaloniaProperty.Register<PreviewRuler, IBrush?>(nameof(FindMarkBrush));

    public static readonly StyledProperty<IBrush?> FindCurrentMarkBrushProperty =
        AvaloniaProperty.Register<PreviewRuler, IBrush?>(nameof(FindCurrentMarkBrush));

    static PreviewRuler()
    {
        AffectsRender<PreviewRuler>(
            TotalLinesProperty, QueryHitsProperty, FindHitsProperty, FindCurrentLineProperty,
            ViewTopProperty, ViewHeightProperty, TrackBrushProperty, HandleBrushProperty,
            QueryMarkBrushProperty, FindMarkBrushProperty, FindCurrentMarkBrushProperty);
        ClipToBoundsProperty.OverrideDefaultValue<PreviewRuler>(true);
    }

    public PreviewRuler()
    {
        Cursor = new Cursor(StandardCursorType.Hand);
    }

    public int TotalLines
    {
        get => GetValue(TotalLinesProperty);
        set => SetValue(TotalLinesProperty, value);
    }

    public IReadOnlyList<int>? QueryHits
    {
        get => GetValue(QueryHitsProperty);
        set => SetValue(QueryHitsProperty, value);
    }

    public IReadOnlyList<int>? FindHits
    {
        get => GetValue(FindHitsProperty);
        set => SetValue(FindHitsProperty, value);
    }

    public int FindCurrentLine
    {
        get => GetValue(FindCurrentLineProperty);
        set => SetValue(FindCurrentLineProperty, value);
    }

    public double ViewTop
    {
        get => GetValue(ViewTopProperty);
        set => SetValue(ViewTopProperty, value);
    }

    public double ViewHeight
    {
        get => GetValue(ViewHeightProperty);
        set => SetValue(ViewHeightProperty, value);
    }

    public IBrush? TrackBrush
    {
        get => GetValue(TrackBrushProperty);
        set => SetValue(TrackBrushProperty, value);
    }

    public IBrush? HandleBrush
    {
        get => GetValue(HandleBrushProperty);
        set => SetValue(HandleBrushProperty, value);
    }

    public IBrush? QueryMarkBrush
    {
        get => GetValue(QueryMarkBrushProperty);
        set => SetValue(QueryMarkBrushProperty, value);
    }

    public IBrush? FindMarkBrush
    {
        get => GetValue(FindMarkBrushProperty);
        set => SetValue(FindMarkBrushProperty, value);
    }

    public IBrush? FindCurrentMarkBrush
    {
        get => GetValue(FindCurrentMarkBrushProperty);
        set => SetValue(FindCurrentMarkBrushProperty, value);
    }

    /// <summary>请求滚动到指定显示行（0 基）。</summary>
    public event Action<int>? SeekRequested;


    public override void Render(DrawingContext context)
    {
        var w = Bounds.Width;
        var h = Bounds.Height;
        if (w <= 0 || h <= 0) return;

        var track = new Rect(0, 0, w, h);
        if (TrackBrush is { } tb) context.DrawRectangle(tb, null, track, 3, 3);

        // 视口滑块
        var viewH = Math.Clamp(ViewHeight, 0.02, 1.0);
        var handleH = Math.Max(viewH * h, 14.0);
        var top = Math.Min(Math.Clamp(ViewTop, 0.0, 1.0) * h, h - handleH);
        if (HandleBrush is { } hb)
        {
            context.DrawRectangle(hb, null, new Rect(0, top, w, handleH), 3, 3);
        }

        var total = Math.Max(TotalLines, 1);

        // 蓝色：查询关键词命中行
        if (QueryHits is { Count: > 0 } queryHits && QueryMarkBrush is { } qb)
        {
            foreach (var line in queryHits)
            {
                var y = line / (double)total * h;
                context.FillRectangle(qb, new Rect(0, y, w, 3));
            }
        }

        // 橙色：Ctrl+F 命中行（左侧 7px 短标记，当前命中全亮）
        if (FindHits is { Count: > 0 } findHits)
        {
            foreach (var line in findHits)
            {
                var isCurrent = line == FindCurrentLine;
                var brush = isCurrent ? FindCurrentMarkBrush : FindMarkBrush;
                if (brush is null) continue;
                var y = line / (double)total * h;
                context.FillRectangle(brush, new Rect(0, y, 7, 3));
            }
        }
    }

    /// <summary>
    /// 由外部（承接指针事件的透明 Border）调用：按控件内 y 坐标跳转。
    ///
    /// 说明：自定义 <see cref="Control"/> 不参与 Avalonia 的命中测试，
    /// 所以指针事件由外层带 <c>Background</c> 的 Border 承接，再换算坐标传进来。
    /// </summary>
    public void SeekAt(double y, bool snap)
    {
        Helpers.Log.Write(
            $"ruler: seek y={y:0} h={Bounds.Height:0} total={TotalLines} q={QueryHits?.Count ?? 0} f={FindHits?.Count ?? 0}");
        Seek(Math.Clamp(y, 0, Math.Max(Bounds.Height, 0)), snap);
    }

    private void Seek(double y, bool snap)
    {
        var total = Math.Max(TotalLines, 1);
        var h = Math.Max(Bounds.Height, 1);

        if (snap)
        {
            var best = 6.0;
            int? found = null;
            if (QueryHits is { Count: > 0 } q)
            {
                foreach (var line in q)
                {
                    var d = Math.Abs(line / (double)total * h - y);
                    if (d <= best)
                    {
                        best = d;
                        found = line;
                    }
                }
            }
            if (FindHits is { Count: > 0 } f)
            {
                foreach (var line in f)
                {
                    var d = Math.Abs(line / (double)total * h - y);
                    if (d <= best)
                    {
                        best = d;
                        found = line;
                    }
                }
            }
            if (found is { } hit)
            {
                SeekRequested?.Invoke(hit);
                return;
            }
        }

        // 输入映射必须落在滑块的**可移动范围**内：滑块顶部最高只能到 h - handleH。
        // 若按整个轨道 h 换算，轨道下部近一半区域都会被钳制到最底部（点击失去反应）。
        var handleH = Math.Max(Math.Clamp(ViewHeight, 0.02, 1.0) * h, 14.0);
        var travel = Math.Max(h - handleH, 1);
        var frac = Math.Clamp(y / travel, 0.0, 1.0);

        // 视口顶部可停留的行区间是 [0, total - 视口行数]，按该区间比例换算才与滑块位置一致
        var viewLines = Math.Clamp(ViewHeight, 0.0, 1.0) * total;
        var maxTopLine = Math.Max(total - viewLines, 0);
        var target = (int)Math.Round(frac * maxTopLine);
        SeekRequested?.Invoke(Math.Clamp(target, 0, total - 1));
    }
}
