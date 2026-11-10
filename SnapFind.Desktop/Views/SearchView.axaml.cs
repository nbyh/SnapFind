using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Interactivity;
using Avalonia.Threading;
using Avalonia.VisualTree;
using SnapFind.Desktop.ViewModels;

namespace SnapFind.Desktop.Views;

public partial class SearchView : UserControl
{
    /// <summary>预览正文左侧行号栏宽度（与 XAML 中 Grid 的 52 保持一致）。</summary>
    private const double GutterWidth = 52;
    /// <summary>等宽字体下每个显示宽度单位对应的像素（与 Rust 版 7.6 对齐）。</summary>
    private const double UnitPx = 7.6;
    /// <summary>正文固定行高。</summary>
    private const double LineHeight = 19;

    private PreviewViewModel? _preview;
    private ScrollViewer? _previewScroll;
    private ScrollViewer? _resultScroll;
    private SearchViewModel? _vmHooked;

    /// <summary>预览列用户拖出的宽度（隐藏预览时保存，恢复时还原）。</summary>
    private double _previewWidth = 470;

    // ColumnDefinition 的 x:Name 不生成代码字段，这里按索引取
    private ColumnDefinition MainCol => RootGrid.ColumnDefinitions[0];
    private ColumnDefinition SplitterCol => RootGrid.ColumnDefinitions[1];
    private ColumnDefinition PreviewCol => RootGrid.ColumnDefinitions[2];

    public SearchView()
    {
        InitializeComponent();

        DataContextChanged += (_, _) => AttachPreview();
        ResultList.DoubleTapped += (_, _) => Vm?.OpenSelected();
        PreviewList.SizeChanged += OnPreviewSizeChanged;
        Ruler.SeekRequested += OnRulerSeek;
        RulerHost.Cursor = new Cursor(StandardCursorType.Hand);
        AddHandler(KeyDownEvent, OnKeyDown, RoutingStrategies.Tunnel);

        PreviewSplitter.DragCompleted += (_, _) =>
        {
            // 拖动时 GridSplitter 会把左列也改成像素宽，这里恢复星号，
            // 让主区继续随窗口宽度自适应，预览列保留用户拖出的宽度。
            if (PreviewCol.Width.IsAbsolute && PreviewCol.Width.Value > 1)
            {
                _previewWidth = PreviewCol.Width.Value;
            }
            MainCol.Width = new GridLength(1, GridUnitType.Star);
        };
    }

    private SearchViewModel? Vm => DataContext as SearchViewModel;

    protected override void OnAttachedToVisualTree(VisualTreeAttachmentEventArgs e)
    {
        base.OnAttachedToVisualTree(e);
        EnsureScroll();
        EnsureResultScroll();
        ApplyWrapUnits();
    }

    // ---- 结果列表：滚到底自动续载 ----------------------------------------

    private void EnsureResultScroll()
    {
        if (_resultScroll is not null) return;

        var sv = ResultList.GetVisualDescendants().OfType<ScrollViewer>().FirstOrDefault();
        if (sv is null)
        {
            // 与预览列表同理：模板还没实例化时先挂布局事件重试
            ResultList.LayoutUpdated -= RetryEnsureResultScroll;
            ResultList.LayoutUpdated += RetryEnsureResultScroll;
            return;
        }

        _resultScroll = sv;
        ResultList.LayoutUpdated -= RetryEnsureResultScroll;
        sv.ScrollChanged += OnResultScrollChanged;
    }

    private void RetryEnsureResultScroll(object? sender, EventArgs e)
    {
        if (_resultScroll is not null)
        {
            ResultList.LayoutUpdated -= RetryEnsureResultScroll;
            return;
        }
        EnsureResultScroll();
    }

    private void OnResultScrollChanged(object? sender, ScrollChangedEventArgs e)
    {
        var sv = _resultScroll;
        var vm = Vm;
        if (sv is null || vm is null || !vm.HasMore) return;

        // 虚拟化列表的 Extent 是估算值，留一点余量提前触发，避免用户停在底部等
        if (sv.Offset.Y + sv.Viewport.Height < sv.Extent.Height - 120) return;

        _ = vm.LoadMoreAsync();
    }

    // ---- 预览联动 --------------------------------------------------------

    private void AttachPreview()
    {
        if (_preview is not null)
        {
            _preview.ScrollToLineRequested -= OnScrollToLine;
        }
        if (_vmHooked is not null)
        {
            _vmHooked.PropertyChanged -= OnVmPropertyChanged;
            _vmHooked.OrderChanged -= OnOrderChanged;
        }

        _preview = Vm?.Preview;
        if (_preview is not null)
        {
            _preview.ScrollToLineRequested += OnScrollToLine;
        }

        _vmHooked = Vm;
        if (_vmHooked is not null)
        {
            _vmHooked.PropertyChanged += OnVmPropertyChanged;
            _vmHooked.OrderChanged += OnOrderChanged;
        }

        SyncPreviewColumn();
    }

    private void OnVmPropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(SearchViewModel.ShowPreview))
        {
            SyncPreviewColumn();
        }
    }

    /// <summary>
    /// 排序变化后把结果列表滚回顶部。
    ///
    /// 重建 Hits 会让 ListBox 再次把原选中行滚进视野，因此要跨几帧反复纠正。
    /// </summary>
    private void OnOrderChanged()
    {
        void Reset(int tries)
        {
            var sv = ResultList.GetVisualDescendants().OfType<ScrollViewer>().FirstOrDefault();
            if (sv is null) return;
            if (sv.Offset.Y > 0.5)
            {
                sv.Offset = new Vector(sv.Offset.X, 0);
            }
            if (sv.Offset.Y > 0.5 && tries > 0)
            {
                Dispatcher.UIThread.Post(() => Reset(tries - 1), DispatcherPriority.Background);
            }
        }

        Dispatcher.UIThread.Post(() => Reset(5), DispatcherPriority.Background);
    }

    /// <summary>没有选中项时把预览列收成 0 宽，避免右侧留一条空白。</summary>
    private void SyncPreviewColumn()
    {
        if (PreviewCol.Width.IsAbsolute && PreviewCol.Width.Value > 1)
        {
            _previewWidth = PreviewCol.Width.Value;
        }

        var show = Vm?.ShowPreview ?? false;
        if (show)
        {
            PreviewCol.MinWidth = 320;
            PreviewCol.MaxWidth = 1100;
            PreviewCol.Width = new GridLength(_previewWidth);
            SplitterCol.Width = new GridLength(10);
            SplitterLine.IsVisible = true;
            PreviewSplitter.IsVisible = true;
        }
        else
        {
            PreviewCol.MinWidth = 0;
            PreviewCol.MaxWidth = 0;
            PreviewCol.Width = new GridLength(0);
            SplitterCol.Width = new GridLength(0);
            SplitterLine.IsVisible = false;
            PreviewSplitter.IsVisible = false;
        }
    }

    // ---- 结果表列宽拖动 ------------------------------------------------

    private string? _resizeColumn;
    private double _resizeStartX;
    private double _resizeStartWidth;

    private void OnColumnResizePressed(object? sender, PointerPressedEventArgs e)
    {
        if (sender is not Border { Tag: string column } handle) return;
        if (!e.GetCurrentPoint(this).Properties.IsLeftButtonPressed) return;

        var layout = Vm?.Layout;
        if (layout is null) return;

        // 双击恢复默认列宽
        if (e.ClickCount == 2)
        {
            var def = new ResultLayout();
            if (column == "size") layout.SizeWidth = def.SizeWidth;
            else layout.MtimeWidth = def.MtimeWidth;
            e.Handled = true;
            return;
        }

        _resizeColumn = column;
        _resizeStartX = e.GetPosition(this).X;
        _resizeStartWidth = column == "size" ? layout.SizeWidth : layout.MtimeWidth;
        e.Pointer.Capture(handle);
        e.Handled = true;
    }

    private void OnColumnResizeMoved(object? sender, PointerEventArgs e)
    {
        if (_resizeColumn is null) return;
        var layout = Vm?.Layout;
        if (layout is null) return;

        // 拖的是列的左边界：向左拖（delta < 0）列变宽
        var delta = e.GetPosition(this).X - _resizeStartX;
        var width = Math.Clamp(_resizeStartWidth - delta, ResultLayout.MinColumn, ResultLayout.MaxColumn);
        if (_resizeColumn == "size") layout.SizeWidth = width;
        else layout.MtimeWidth = width;

        e.Handled = true;
    }

    private void OnColumnResizeReleased(object? sender, PointerReleasedEventArgs e)
    {
        if (_resizeColumn is null) return;
        _resizeColumn = null;
        e.Pointer.Capture(null);
        e.Handled = true;
    }

    private void EnsureScroll()
    {
        if (_previewScroll is not null) return;

        var sv = PreviewList.GetVisualDescendants().OfType<ScrollViewer>().FirstOrDefault();
        if (sv is null)
        {
            // 此前在 OnAttachedToVisualTree 里只找一次：那时 ListBox 的模板还没实例化，
            // 拿不到内部 ScrollViewer，标尺就再也收不到 ScrollChanged（永远停在顶部）。
            // 改为挂到 LayoutUpdated 上重试，找到后立刻摘钩。
            PreviewList.LayoutUpdated -= RetryEnsureScroll;
            PreviewList.LayoutUpdated += RetryEnsureScroll;
            return;
        }

        _previewScroll = sv;
        PreviewList.LayoutUpdated -= RetryEnsureScroll;
        sv.ScrollChanged += (_, _) => UpdateRuler();
        UpdateRuler();
    }

    private void RetryEnsureScroll(object? sender, EventArgs e)
    {
        if (_previewScroll is not null)
        {
            PreviewList.LayoutUpdated -= RetryEnsureScroll;
            return;
        }
        EnsureScroll();
    }

    private void UpdateRuler()
    {
        var sv = _previewScroll;
        if (sv is null) return;

        // 用「行数 × 行高」作为真实内容高度：虚拟化列表的 Extent 是估算值，
        // 会让滑块位置与实际阅读行数对不上。
        var content = Math.Max(_preview?.TotalLines ?? 0, 1) * LineHeight;
        Ruler.ViewTop = Math.Clamp(sv.Offset.Y / content, 0, 1);
        Ruler.ViewHeight = Math.Clamp(sv.Viewport.Height / content, 0.02, 1);
    }

    private void OnScrollToLine(int index)
    {
        if (index < 0) return;
        Dispatcher.UIThread.Post(() =>
        {
            PreviewList.ScrollIntoView(index);
            UpdateRuler();
        }, DispatcherPriority.Background);
    }

    private bool _rulerDragging;

    // 指针事件挂在透明 Border 上（自定义 Control 不参与命中测试），
    // 再把坐标换算到标尺内部空间。
    private void OnRulerPressed(object? sender, PointerPressedEventArgs e)
    {
        var point = e.GetCurrentPoint(Ruler);
        if (!point.Properties.IsLeftButtonPressed) return;

        _rulerDragging = true;
        e.Pointer.Capture(RulerHost);
        Ruler.SeekAt(point.Position.Y, snap: true);
        e.Handled = true;
    }

    private void OnRulerMoved(object? sender, PointerEventArgs e)
    {
        if (!_rulerDragging) return;
        var point = e.GetCurrentPoint(Ruler);
        if (!point.Properties.IsLeftButtonPressed) return;
        // 拖动按比例连续滚动，不做吸附（与 Rust 版一致）
        Ruler.SeekAt(point.Position.Y, snap: false);
        e.Handled = true;
    }

    private void OnRulerReleased(object? sender, PointerReleasedEventArgs e)
    {
        _rulerDragging = false;
        e.Pointer.Capture(null);
    }

    /// <summary>
    /// 标尺点击/拖动。
    ///
    /// 用 <see cref="ListBox.ScrollIntoView"/> 按**行**定位而不是按像素比例：
    /// 虚拟化列表的 Extent 在滚动过程中会不断修正，用比例换算会落在偏差几行的位置。
    /// </summary>
    private void OnRulerSeek(int line)
    {
        var total = _preview?.TotalLines ?? 0;
        if (total <= 0) return;

        var target = Math.Clamp(line, 0, total - 1);
        var sv = _previewScroll;
        if (sv is null)
        {
            PreviewList.ScrollIntoView(target);
            return;
        }

        // 两步走：
        // 1) ScrollIntoView 让目标行真正进入渲染范围 —— 虚拟化列表的 Extent 是估算值，
        //    直接设一个远处偏移会被 clamp 回原位；
        // 2) 下一帧再按行高精确对齐到顶部（ScrollIntoView 只保证「可见」）。
        PreviewList.ScrollIntoView(target);

        // 虚拟化列表的 Extent 会随已实现项逐步修正，一次设不到位就再试几帧
        var want = target * LineHeight;
        void Align(int tries)
        {
            var scroll = _previewScroll;
            if (scroll is null) return;
            var max = Math.Max(scroll.Extent.Height - scroll.Viewport.Height, 0);
            var next = Math.Min(want, max);
            scroll.Offset = new Vector(scroll.Offset.X, next);
            if (Math.Abs(scroll.Offset.Y - want) > 1 && tries > 0)
            {
                Dispatcher.UIThread.Post(() => Align(tries - 1), DispatcherPriority.Background);
            }
            UpdateRuler();
        }
        Dispatcher.UIThread.Post(() => Align(6), DispatcherPriority.Background);
    }

    // ---- 软换行 ----------------------------------------------------------

    private void OnPreviewSizeChanged(object? sender, SizeChangedEventArgs e)
    {
        ApplyWrapUnits();
        _ = Dispatcher.UIThread.InvokeAsync(UpdateRuler, DispatcherPriority.Background);
    }

    /// <summary>
    /// 按正文可用宽度换算每行显示宽度，并在变化时重新折行。
    /// 换算公式与 Rust 版 <c>preview_panel</c> 一致：四舍五入到 4 的倍数，范围 [16, 200]。
    /// </summary>
    private void ApplyWrapUnits()
    {
        var vm = _preview;
        if (vm is null) return;

        var width = PreviewList.Bounds.Width;
        if (width <= GutterWidth + 40) return;

        var raw = (int)Math.Round((width - 8 - GutterWidth) / UnitPx);
        if (raw < 16) raw = 16;
        var units = Math.Clamp(((raw + 2) / 4) * 4, 16, 200);
        if (units == vm.WrapUnits) return;

        // 记录当前首行比例，折行后按比例恢复，避免窗口缩放把阅读位置甩飞
        var sv = _previewScroll;
        var oldTotal = Math.Max(vm.TotalLines, 1);
        var ratio = sv is null ? 0 : Math.Clamp(sv.Offset.Y / LineHeight / oldTotal, 0, 1);

        if (!vm.SetWrapUnits(units)) return;

        _ = Dispatcher.UIThread.InvokeAsync(() =>
        {
            var scroll = _previewScroll;
            if (scroll is null) return;
            var newTotal = Math.Max(vm.TotalLines, 1);
            var line = ratio * newTotal;
            var max = Math.Max(scroll.Extent.Height - scroll.Viewport.Height, 0);
            scroll.Offset = new Vector(scroll.Offset.X, Math.Clamp(line * LineHeight, 0, max));
            UpdateRuler();
        }, DispatcherPriority.Background);
    }

    // ---- 键盘 ------------------------------------------------------------

    /// <summary>搜索框内回车立即搜索（跳过防抖）。</summary>
    private void OnQueryKeyDown(object? sender, KeyEventArgs e)
    {
        if (e.Key != Key.Enter) return;
        Vm?.SearchNowCommand.Execute(null);
        e.Handled = true;
    }

    private void OnKeyDown(object? sender, KeyEventArgs e)
    {
        var vm = Vm;
        if (vm is null) return;
        var focused = TopLevel.GetTopLevel(this)?.FocusManager?.GetFocusedElement();
        var inTextBox = focused is TextBox;

        if (e.Key == Key.F && e.KeyModifiers.HasFlag(KeyModifiers.Control))
        {
            if (vm.Hits.Count > 0)
            {
                vm.Preview.OpenFind();
                Dispatcher.UIThread.Post(() => FindBox.Focus(), DispatcherPriority.Background);
            }
            else
            {
                QueryBox.Focus();
            }
            e.Handled = true;
            return;
        }

        if (inTextBox) return;

        switch (e.Key)
        {
            case Key.Down when vm.Hits.Count > 0 && !vm.Preview.FindOpen:
                vm.MoveSelection(1);
                e.Handled = true;
                break;
            case Key.Up when vm.Hits.Count > 0 && !vm.Preview.FindOpen:
                vm.MoveSelection(-1);
                e.Handled = true;
                break;
            case Key.Enter when !vm.Preview.FindOpen:
                vm.OpenSelected();
                e.Handled = true;
                break;
        }
    }

    // ---- 预览动作 --------------------------------------------------------

    private void OnOpenClick(object? sender, RoutedEventArgs e) => Vm?.OpenSelected();

    private void OnRevealClick(object? sender, RoutedEventArgs e) => Vm?.RevealSelected();

    private async void OnCopyClick(object? sender, RoutedEventArgs e)
    {
        var vm = Vm?.Preview;
        var top = TopLevel.GetTopLevel(this);
        if (vm?.Hit is null || top?.Clipboard is null) return;
        try
        {
            await top.Clipboard.SetTextAsync(vm.Hit.Path);
        }
        catch (Exception ex)
        {
            // 剪贴板被占用/权限失败时不再静默：用户点了「复制路径」却没反应会当成 bug
            Helpers.Log.Write($"clipboard set failed: {ex.Message}");
            Vm?.Notify("复制失败：剪贴板被其他程序占用，请重试。");
        }
    }

    private void OnFindOpenClick(object? sender, RoutedEventArgs e)
    {
        Vm?.Preview.OpenFind();
        Dispatcher.UIThread.Post(() => FindBox.Focus(), DispatcherPriority.Background);
    }

    /// <summary>查找框内回车 = 下一个，Shift+回车 = 上一个，Esc = 关闭。</summary>
    private void OnFindBoxKeyDown(object? sender, KeyEventArgs e)
    {
        switch (e.Key)
        {
            case Key.Enter when e.KeyModifiers.HasFlag(KeyModifiers.Shift):
                Vm?.Preview.Step(-1);
                e.Handled = true;
                break;
            case Key.Enter:
                Vm?.Preview.Step(1);
                e.Handled = true;
                break;
            case Key.Escape:
                Vm?.Preview.CloseFind();
                e.Handled = true;
                break;
        }
    }

    private void OnFindPrevClick(object? sender, RoutedEventArgs e) => Vm?.Preview.Step(-1);

    private void OnFindNextClick(object? sender, RoutedEventArgs e) => Vm?.Preview.Step(1);

    private void OnFindCloseClick(object? sender, RoutedEventArgs e) => Vm?.Preview.CloseFind();

    // ---- 「更多」类型菜单 ------------------------------------------------

    private void OnMoreClick(object? sender, RoutedEventArgs e)
    {
        var vm = Vm;
        if (vm is null) return;

        var flyout = new MenuFlyout();
        var readable = vm.MoreTypes.Where(t => !t.Unreadable).ToList();
        var unreadable = vm.MoreTypes.Where(t => t.Unreadable).ToList();

        foreach (var t in readable)
        {
            var item = new MenuItem { Header = $"{t.Ext}（{t.Count}）" };
            item.Click += (_, _) => vm.ToggleTypeCommand.Execute(t.Ext);
            flyout.Items.Add(item);
        }

        if (unreadable.Count > 0)
        {
            if (readable.Count > 0) flyout.Items.Add(new Separator());
            flyout.Items.Add(new MenuItem { Header = "仅按文件名/路径匹配（内容不可读）", IsEnabled = false });
            foreach (var t in unreadable)
            {
                var item = new MenuItem { Header = $"{t.Ext} ·名（{t.Count}）" };
                item.Click += (_, _) => vm.ToggleTypeCommand.Execute(t.Ext);
                flyout.Items.Add(item);
            }
        }

        if (flyout.Items.Count == 0)
        {
            flyout.Items.Add(new MenuItem { Header = "本次结果中没有其他类型", IsEnabled = false });
        }

        flyout.ShowAt(MoreButton);
    }
}
