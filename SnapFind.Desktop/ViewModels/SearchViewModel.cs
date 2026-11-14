using System.Collections.ObjectModel;
using System.Text;
using Avalonia.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using SnapFind.Desktop.Helpers;
using SnapFind.Desktop.Ipc;
using SnapFind.Desktop.Services;

namespace SnapFind.Desktop.ViewModels;

/// <summary>类型筛选标签。</summary>
public sealed partial class TypeTag : ObservableObject
{
    public TypeTag(string ext)
    {
        Ext = ext;
        _label = ext;
    }

    public string Ext { get; }
    [ObservableProperty] private string _label;
    [ObservableProperty] private bool _selected;

    /// <summary>由 <see cref="SearchViewModel"/> 注入，避免在模板里写跨层级的命令绑定。</summary>
    public IRelayCommand<string>? ToggleCommand { get; set; }
}

/// <summary>
/// 搜索页：查询、结果累积、类型本地筛选、本地排序、预览联动。
///
/// 与 Rust 版语义严格对齐的三条关键规则：
/// 1. <c>filters.types</c> 永远传空 —— 类型标签是**纯本地过滤**，不会重新搜索；
/// 2. 路径筛选是**服务端条件**（<c>filters.paths</c>），改动会重新搜索；
/// 3. 标签上的数字来自本轮服务端命中总数，可能大于筛选后可见条数。
/// </summary>
public sealed partial class SearchViewModel : ObservableObject
{
    /// <summary>
    /// 每批取回的条数（不是上限）。
    ///
    /// 首屏先拿一批让界面立刻可用，之后列表滚到底部再续取下一批，
    /// 结果条数不设上限，能一直加载到命中总数为止。
    /// </summary>
    public const int PageSize = 500;

    private static readonly string[] CommonExts =
    {
        "txt", "md", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
        "pdf", "rtf", "csv", "json", "ini", "cfg", "log", "xml", "html", "epub",
    };

    /// <summary>可预览（内容可抽取）的扩展名，用于「更多」里的第一段。</summary>
    private static readonly string[] SupportedExts =
    {
        "txt", "md", "log", "nfo", "ini", "cfg", "conf", "env", "properties",
        "json", "yaml", "yml", "toml", "xml", "csv", "tsv",
        "html", "htm", "css", "scss", "less", "vue", "svelte",
        "doc", "docx", "rtf", "odt", "wps", "xls", "xlsx", "ods", "ppt", "pptx", "odp",
        "pdf", "epub", "mobi", "azw3", "chm",
        "rs", "py", "js", "jsx", "ts", "tsx", "c", "h", "cc", "cpp", "hpp", "cs",
        "java", "go", "rb", "php", "swift", "kt", "lua", "sh", "bat", "ps1", "sql",
    };

    private readonly Func<IpcClient?> _client;
    private readonly Action<string> _notice;

    private readonly DispatcherTimer _debounce;
    private readonly DispatcherTimer _watchdog;
    private DateTime _searchStarted;
    private bool _dirty;
    private string? _activeRequest;
    private long _seq;
    private bool _suppressPreview;

    private readonly List<HitItem> _all = new();
    /// <summary>复用的状态栏拼装缓冲（每次输入/结果变化都会刷新）。</summary>
    private readonly StringBuilder _statusSb = new();
    private readonly Dictionary<string, long> _typeCounts = new(StringComparer.OrdinalIgnoreCase);
    private readonly HashSet<string> _typeFilter = new(StringComparer.OrdinalIgnoreCase);
    private readonly List<string> _warnings = new();
    /// <summary>查询预算（毫秒）。后端不会因此中止搜索，只用于「耗时超预算」提醒；
    /// 可在设置页修改（UpdateQueryBudget），无需重启。</summary>
    private int _queryTimeoutMs;

    public SearchViewModel(Func<IpcClient?> client, Action<string> notice, int debounceMs, int queryTimeoutMs)
    {
        _client = client;
        _notice = notice;
        _queryTimeoutMs = Math.Clamp(queryTimeoutMs, 1000, 60000);
        Preview = new PreviewViewModel(client);

        foreach (var e in CommonExts)
        {
            TypeTags.Add(new TypeTag(e) { ToggleCommand = ToggleTypeCommand });
        }

        _debounce = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(Math.Max(30, debounceMs)) };
        _debounce.Tick += (_, _) =>
        {
            _debounce.Stop();
            if (_dirty)
            {
                _dirty = false;
                _ = RunSearchAsync();
            }
        };

        _watchdog = new DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
        _watchdog.Tick += (_, _) =>
        {
            if (!Searching) return;
            // 看门狗 = 预算 × 3 且 ≥ 10 秒。以前写死 30 秒：预算改小后仍要等 30s
            // 才报超时，出现「显示 10s 预算、30s 才超时」的错位
            var watchdogSec = Math.Max(10, _queryTimeoutMs * 3 / 1000);
            if ((DateTime.UtcNow - _searchStarted).TotalSeconds < watchdogSec) return;
            Searching = false;
            _activeRequest = null;
            _notice("查询超时：索引可能正在重建或过忙，请稍后重试，或缩小搜索范围。");
        };
        _watchdog.Start();
    }

    /// <summary>设置页修改「查询时间预算」后同步生效（提醒阈值与看门狗一起变，无需重启）。</summary>
    public void UpdateQueryBudget(int ms) => _queryTimeoutMs = Math.Clamp(ms, 1000, 60000);

    /// <summary>搜索页局部提示（复制失败等），走 Shell 的底部通知条。</summary>
    public void Notify(string message) => _notice(message);

    public PreviewViewModel Preview { get; }

    // ---- 输入 ------------------------------------------------------------

    [ObservableProperty] private string _query = "";
    [ObservableProperty] private string _pathFilter = "";
    [ObservableProperty] private bool _pathFilterOpen;

    // ---- 结果 ------------------------------------------------------------

    public ObservableCollection<HitItem> Hits { get; } = new();
    public ObservableCollection<string> Warnings { get; } = new();
    public ObservableCollection<TypeTag> TypeTags { get; } = new();

    [ObservableProperty] private int _selectedIndex = -1;
    [ObservableProperty] private int _total;
    [ObservableProperty] private ulong _tookMs;
    [ObservableProperty] private bool _searching;
    /// <summary>还有没取回的结果（滚动到底会继续加载）。</summary>
    [ObservableProperty] private bool _hasMore;
    /// <summary>正在续载下一批。</summary>
    [ObservableProperty] private bool _loadingMore;
    [ObservableProperty] private string _statusText = "输入关键词开始搜索";
    /// <summary>默认按修改时间倒序（最新改动排最前）。</summary>
    [ObservableProperty] private string _sortKey = "mtime";
    [ObservableProperty] private bool _sortDesc = true;

    /// <summary>结果表列宽：表头与行共享，拖动列边界时只改这一份。</summary>
    public ResultLayout Layout { get; } = new();

    // ---- 表头排序指示 ----------------------------------------------------

    public string NameSortMark => SortMark("name");
    public string SizeSortMark => SortMark("size");
    public string MtimeSortMark => SortMark("mtime");
    public bool SequencedOrder => SortKey is "relevance" or "";

    private string SortMark(string key) => SortKey == key ? (SortDesc ? "▼" : "▲") : "";

    partial void OnSortKeyChanged(string value) => RaiseSortMarks();
    partial void OnSortDescChanged(bool value) => RaiseSortMarks();

    private void RaiseSortMarks()
    {
        OnPropertyChanged(nameof(NameSortMark));
        OnPropertyChanged(nameof(SizeSortMark));
        OnPropertyChanged(nameof(MtimeSortMark));
        OnPropertyChanged(nameof(SequencedOrder));
    }
    [ObservableProperty] private int _visibleCount;
    [ObservableProperty] private string _moreLabel = "更多";

    public IReadOnlyList<MoreType> MoreTypes { get; private set; } = Array.Empty<MoreType>();
    public bool HasMoreTypes => MoreTypes.Count > 0;

    public HitItem? SelectedItem => SelectedIndex >= 0 && SelectedIndex < Hits.Count ? Hits[SelectedIndex] : null;

    public bool HasResults => Hits.Count > 0;
    public bool ShowEmptyState => !Searching && Hits.Count == 0;
    public bool EmptyStateNoQuery => ShowEmptyState && string.IsNullOrWhiteSpace(Query);
    public bool ShowPreview => SelectedItem is not null;

    // ---- 类型筛选 --------------------------------------------------------

    /// <summary>「更多」菜单里的一项：动态生成，只含本次结果里真正出现的类型。</summary>
    public sealed record MoreType(string Ext, long Count, bool Unreadable);

    [RelayCommand]
    private void ToggleType(string? ext)
    {
        if (string.IsNullOrWhiteSpace(ext)) return;
        if (!_typeFilter.Remove(ext)) _typeFilter.Add(ext);
        SyncTagSelection();
        RebuildDisplay();
    }

    [RelayCommand]
    private void ClearTypes()
    {
        _typeFilter.Clear();
        SyncTagSelection();
        RebuildDisplay();
    }

    private void SyncTagSelection()
    {
        foreach (var tag in TypeTags) tag.Selected = _typeFilter.Contains(tag.Ext);
        OnPropertyChanged(nameof(FilterSummary));
        OnPropertyChanged(nameof(HasFilter));
    }

    public string FilterSummary => _typeFilter.Count == 0 ? "" : $"已选 {_typeFilter.Count} 类";

    public bool HasFilter => _typeFilter.Count > 0;

    private void RefreshTagLabels()
    {
        var hasQuery = !string.IsNullOrWhiteSpace(Query);
        foreach (var tag in TypeTags)
        {
            tag.Label = hasQuery && _typeCounts.TryGetValue(tag.Ext, out var n) && n > 0
                ? $"{tag.Ext}（{n}）"
                : tag.Ext;
        }
        RefreshMoreTypes();
    }

    /// <summary>
    /// 「更多」菜单改为动态列表：只列本次结果里真正出现（计数 &gt; 0）、
    /// 且不在常用标签里的类型，按命中数降序；不再预置 52 个固定扩展名。
    /// </summary>
    private void RefreshMoreTypes()
    {
        var common = CommonExts.ToHashSet(StringComparer.OrdinalIgnoreCase);
        var withCounts = _typeCounts
            .Where(kv => kv.Value > 0 && !common.Contains(kv.Key))
            .OrderByDescending(kv => kv.Value)
            .ThenBy(kv => kv.Key, StringComparer.OrdinalIgnoreCase)
            .ToList();

        MoreTypes = withCounts
            .Select(kv => new MoreType(
                kv.Key, kv.Value,
                Unreadable: !Format.ContentReadable(kv.Key)))
            .ToList();
        MoreLabel = MoreTypes.Count > 0 ? $"更多（{MoreTypes.Count}）" : "更多";
        OnPropertyChanged(nameof(HasMoreTypes));
    }

    // ---- 搜索 ------------------------------------------------------------

    partial void OnQueryChanged(string value)
    {
        _dirty = true;
        _debounce.Stop();
        _debounce.Start();
    }

    partial void OnPathFilterChanged(string value)
    {
        _dirty = true;
        _debounce.Stop();
        _debounce.Start();
    }

    [RelayCommand]
    private void SearchNow()
    {
        _dirty = false;
        _debounce.Stop();
        _ = RunSearchAsync();
    }

    /// <summary>清空搜索框并立即复位结果（不走防抖，反馈更直接）。</summary>
    [RelayCommand]
    private void ClearQuery()
    {
        _debounce.Stop();
        _dirty = false;
        Query = "";
        ResetResults();
    }

    private async Task RunSearchAsync()
    {
        var q = (Query ?? "").Trim();
        if (q.Length == 0)
        {
            ResetResults();
            return;
        }

        var client = _client();
        if (client is null)
        {
            _notice("索引进程未运行，无法搜索。");
            return;
        }

        // 取消上一轮未完成的请求
        if (_activeRequest is { Length: > 0 } old)
        {
            try { await client.CancelAsync(old); } catch { /* ignore */ }
        }

        var requestId = $"q-{Environment.ProcessId}-{Interlocked.Increment(ref _seq)}";
        _activeRequest = requestId;

        _searchStarted = DateTime.UtcNow;
        Searching = true;
        LoadingMore = false;
        HasMore = false;
        _all.Clear();
        _typeCounts.Clear();
        Hits.Clear();
        Warnings.Clear();
        _warnings.Clear();
        Total = 0;
        TookMs = 0;
        SelectedIndex = -1;
        _suppressPreview = true;
        Preview.Clear();
        _suppressPreview = false;
        RefreshTagLabels();
        UpdateStatus();

        try
        {
            // 首屏只取一批，滚到底再续取：结果数量不设上限
            await client.QueryAsync(BuildRequest(requestId, 0, PageSize), chunk =>
            {
                Dispatcher.UIThread.Post(() => OnChunk(chunk, requestId, append: false));
            });
        }
        catch (Exception ex)
        {
            await Dispatcher.UIThread.InvokeAsync(() =>
            {
                if (_activeRequest != requestId) return;
                Searching = false;
                _notice($"查询失败: {ex.Message}");
                UpdateStatus();
            });
        }
    }

    private QueryRequest BuildRequest(string requestId, int offset, int limit)
        => new()
        {
            RequestId = requestId,
            Query = (Query ?? "").Trim(),
            Sort = new SortSpec { Key = SortKey, Order = SortDesc ? "desc" : "asc" },
            Paging = new Paging { Offset = offset, Limit = limit },
            TimeoutMs = _queryTimeoutMs,
            Filters = new QueryFilters
            {
                Types = new List<string>(),
                Paths = string.IsNullOrWhiteSpace(PathFilter)
                    ? new List<string>()
                    : new List<string> { PathFilter.Trim() },
            },
        };

    /// <summary>
    /// 列表滚到底时续取下一批。
    ///
    /// indexd 的 offset 语义是「每个卷多取 offset 条」，并不跳过，所以这里按卷记已加载数量，
    /// 续载时把每卷重发的前缀跳过，保证索引不重复也不丢。
    /// </summary>
    public async Task LoadMoreAsync()
    {
        if (LoadingMore || Searching || !HasMore) return;

        var client = _client();
        if (client is null) return;

        LoadingMore = true;
        UpdateStatus();

        var requestId = $"q-{Environment.ProcessId}-{Interlocked.Increment(ref _seq)}";
        _activeRequest = requestId;

        // 续载窗口 = 已加载条数之后，服务端全局有序，保证接续不重不漏
        var offset = _all.Count;

        try
        {
            await client.QueryAsync(BuildRequest(requestId, offset, PageSize), chunk =>
            {
                Dispatcher.UIThread.Post(() => OnChunk(chunk, requestId, append: true));
            });
        }
        catch (Exception ex)
        {
            await Dispatcher.UIThread.InvokeAsync(() =>
            {
                LoadingMore = false;
                _notice($"加载更多失败: {ex.Message}");
                UpdateStatus();
            });
        }
    }

    private void OnChunk(QueryChunk chunk, string requestId, bool append)
    {
        // 过期帧直接丢弃（用户已经改了关键词）
        if (_activeRequest != requestId) return;

        Total = chunk.TotalEstimate;
        if (!append) TookMs = chunk.TookMs;

        foreach (var w in chunk.Warnings)
        {
            if (_warnings.Contains(w)) continue;
            _warnings.Add(w);
            Warnings.Add(FriendlyWarning(w));
        }
        // indexd 已在各卷取候选后做跨卷全局排序并按 offset/limit 切片，
        // 这里收到的就是全局有序的下一段，直接追加即可（不要再按卷分组）。
        var appended = 0;
        foreach (var hit in chunk.Hits)
        {
            if (!string.IsNullOrEmpty(hit.Ext))
            {
                _typeCounts.TryGetValue(hit.Ext, out var n);
                _typeCounts[hit.Ext] = n + 1;
            }

            _all.Add(new HitItem(hit, _all.Count, Layout));
            appended++;
        }

        if (appended > 0)
        {
            RefreshTagLabels();

            if (append && _typeFilter.Count == 0)
            {
                // 增量追加：不整表重建，滚动位置不会跳
                for (var i = Hits.Count; i < _all.Count; i++)
                {
                    var item = _all[i];
                    item.Seq = i + 1;
                    Hits.Add(item);
                }
                UpdateStatus();
            }
            else
            {
                _suppressPreview = true;
                RebuildDisplay();
                _suppressPreview = false;

                // 首批结果到达后自动预览第一条
                if (!append && Preview.Hit is null && Hits.Count > 0) SelectRow(0);
            }
        }

        if (!string.Equals(chunk.Phase, "partial", StringComparison.Ordinal))
        {
            Searching = false;
            LoadingMore = false;
            _activeRequest = null;

            // 还有没取完的：本批确实拿到了新行，且总条数还没到命中总数
            HasMore = appended > 0 && Hits.Count < Total;
        }

        UpdateStatus();
    }

    private void ResetResults()
    {
        _activeRequest = null;
        Searching = false;
        LoadingMore = false;
        HasMore = false;
        _all.Clear();
        _typeCounts.Clear();
        Hits.Clear();
        Warnings.Clear();
        _warnings.Clear();
        Total = 0;
        TookMs = 0;
        SelectedIndex = -1;
        Preview.Clear();
        RefreshTagLabels();
        UpdateStatus();
    }

    /// <summary>把后端警告 key 翻译成用户能看懂的提示。</summary>
    private static string FriendlyWarning(string w) => w switch
    {
        // 后端不会因超时中止搜索：这条只是「耗时超过预算」的提醒，结果仍是完整的
        "timeout_partial" => "本次查询耗时超过预算，结果仍为完整结果（宽泛查询或首次搜索冷缓存时常见）",
        _ => w,
    };

    private void UpdateStatus()
    {
        var sb = _statusSb;
        sb.Clear();
        if (Searching)
        {
            sb.Append("搜索中…");
        }
        else if (Total > 0)
        {
            sb.Append($"找到约 {Total} 个结果");
            if (TookMs > 0) sb.Append($" · {TookMs} ms");
            if (_typeFilter.Count > 0)
            {
                sb.Append($" · 当前筛选 {Hits.Count} 条");
            }
            else if (LoadingMore)
            {
                sb.Append($" · 已加载 {Hits.Count} 条，正在加载更多…");
            }
            else if (HasMore)
            {
                // 不设上限：明确告诉用户还剩多少没取，滚到底会自动续上
                sb.Append($" · 已加载 {Hits.Count} 条（滚动到底继续加载）");
            }
        }
        else
        {
            sb.Append($"{Hits.Count} 条命中 · 配置保存于程序目录");
        }

        StatusText = sb.ToString();
        VisibleCount = Hits.Count;
        OnPropertyChanged(nameof(HasResults));
        OnPropertyChanged(nameof(ShowEmptyState));
        OnPropertyChanged(nameof(EmptyStateNoQuery));
        OnPropertyChanged(nameof(ShowPreview));
    }

    // ---- 本地筛选与排序 --------------------------------------------------

    private void RebuildDisplay()
    {
        var keepPath = SelectedItem?.Path;
        var source = _typeFilter.Count == 0
            ? _all
            : _all.Where(h => _typeFilter.Contains(h.Ext)).ToList();

        Hits.Clear();
        var i = 0;
        foreach (var h in source)
        {
            h.Seq = ++i;
            Hits.Add(h);
        }

        var idx = keepPath is null ? -1 : source.FindIndex(h => h.Path == keepPath);

        var prevSuppress = _suppressPreview;
        _suppressPreview = true;
        SelectedIndex = idx;
        _suppressPreview = prevSuppress;

        if (!prevSuppress)
        {
            if (SelectedIndex >= 0)
            {
                Preview.QueryTerms = Format.Terms(Query);
                Preview.Load(Hits[SelectedIndex]);
            }
            else
            {
                Preview.Clear();
            }
        }

        UpdateStatus();
        OnPropertyChanged(nameof(ShowPreview));
        OnPropertyChanged(nameof(FilterSummary));
    }

    [RelayCommand]
    private void SortBy(string? key)
    {
        if (string.IsNullOrEmpty(key)) return;

        if (key == "reset")
        {
            SortKey = "relevance";
            SortDesc = true;
        }
        else if (SortKey == key)
        {
            SortDesc = !SortDesc;
        }
        else
        {
            SortKey = key;
            SortDesc = key != "name"; // 名称首次点按升序，其余降序
        }

        // 排序必须重新取数：跨卷的全局序由 indexd 排序后切片决定，
        // 只对已加载的那一批做本地排序会与新加载的续页错位。
        OrderChanged?.Invoke();

        if (!string.IsNullOrWhiteSpace(Query))
        {
            _dirty = false;
            _debounce.Stop();
            _ = RunSearchAsync();
        }
        else
        {
            RebuildDisplay();
        }
    }

    /// <summary>排序完成后触发，由视图把列表滚回顶部。</summary>
    public event Action? OrderChanged;

    // ---- 选择与动作 ------------------------------------------------------

    partial void OnSelectedIndexChanged(int value)
    {
        if (_suppressPreview) return;
        if (value >= 0 && value < Hits.Count)
        {
            Preview.QueryTerms = Format.Terms(Query);
            Preview.Load(Hits[value]);
        }
        else
        {
            Preview.Clear();
        }
        OnPropertyChanged(nameof(ShowPreview));
        OnPropertyChanged(nameof(SelectedPathText));
    }

    public void SelectRow(int index)
    {
        if (index < 0 || index >= Hits.Count) return;
        if (index == SelectedIndex)
        {
            // 同一行重复点击也要刷新预览（例如正文加载失败后重试）
            Preview.QueryTerms = Format.Terms(Query);
            Preview.Load(Hits[index]);
            return;
        }
        SelectedIndex = index;
    }

    public void MoveSelection(int delta)
    {
        if (Hits.Count == 0) return;
        var next = Math.Clamp(SelectedIndex + delta, 0, Hits.Count - 1);
        if (next != SelectedIndex) SelectRow(next);
    }

    public string? OpenSelected()
    {
        var item = SelectedItem;
        if (item is null) return null;
        try
        {
            PlatformService.OpenFile(item.Path);
            return null;
        }
        catch (Exception ex)
        {
            var msg = $"打开失败: {ex.Message}";
            _notice(msg);
            return msg;
        }
    }

    public void RevealSelected()
    {
        var item = SelectedItem;
        if (item is null) return;
        try { PlatformService.RevealInExplorer(item.Path); }
        catch (Exception ex) { _notice($"定位失败: {ex.Message}"); }
    }

    public string SelectedPathText => SelectedItem?.Path ?? "";
}
