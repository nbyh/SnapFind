using System.Collections.ObjectModel;
using Avalonia.Media;
using Avalonia.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using SnapFind.Desktop.Helpers;
using SnapFind.Desktop.Ipc;

namespace SnapFind.Desktop.ViewModels;

/// <summary>索引页的一行卷/目录。</summary>
public sealed partial class VolumeRow : ObservableObject
{
    [ObservableProperty] private bool _selected;
    [ObservableProperty] private string _stateText = "未索引";
    [ObservableProperty] private string _stateBrushKey = "Text2";
    [ObservableProperty] private IBrush _stateBrush = Brushes.Gray;
    [ObservableProperty] private string _docsText = "—";
    /// <summary>后端原始状态（Scanning/Watching/Paused/…），供批量操作筛选目标。</summary>
    [ObservableProperty] private string _rowState = "Discovered";
    /// <summary>手工添加的目录（fs=="dir"）：可从索引范围移除；磁盘/网络卷不支持。</summary>
    [ObservableProperty] private bool _isCustom;

    public string VolumeId { get; init; } = "";
    public string Mount { get; init; } = "";
    public string KindLabel { get; init; } = "";
    public string TotalText { get; init; } = "—";
}

/// <summary>某卷的索引进度卡片。</summary>
public sealed partial class ProgressRow : ObservableObject
{
    [ObservableProperty] private double _fraction;
    [ObservableProperty] private string _percentText = "0%";
    [ObservableProperty] private string _statsLine = "";
    /// <summary>总量未知（扫描期 files_total 只是「已发现数」）时用不确定进度条。</summary>
    [ObservableProperty] private bool _indeterminate;
    [ObservableProperty] private string _indexLine = "";
    [ObservableProperty] private string _memoryWarning = "";
    [ObservableProperty] private bool _hasMemoryWarning;
    [ObservableProperty] private string _state = "Discovered";
    [ObservableProperty] private IBrush _stateBrush = Brushes.Gray;
    /// <summary>已暂停：卡片显示「继续」，隐藏「暂停」。</summary>
    [ObservableProperty] private bool _isPaused;
    /// <summary>是否可暂停（扫描/索引/重建/监控中）。</summary>
    [ObservableProperty] private bool _canPause;
    /// <summary>是否可恢复（暂停/停止/未索引）。</summary>
    [ObservableProperty] private bool _canResume;

    private DateTime _lastProgressAt = DateTime.MinValue;
    /// <summary>复用的行拼装缓冲：进度事件每 200ms 一次/卷，避免反复分配 StringBuilder。</summary>
    private readonly System.Text.StringBuilder _statsLineSb = new();

    /// <summary>最近一次收到状态/进度的时间；stats 清理卡片时的宽限依据。</summary>
    public DateTime LastStateAt { get; private set; } = DateTime.UtcNow;

    public string VolumeId { get; init; } = "";
    public string Mount { get; set; } = "";

    /// <summary>是否正在实时索引（由进度事件驱动）；就绪后交给 stats 摘要。</summary>
    public bool Live { get; private set; }

    /// <summary>是否处于「扫描/建立索引」活动状态（此时 files_total 仍是滚动累加值）。</summary>
    private static bool IsActive(string state)
        => state.Equals("Scanning", StringComparison.OrdinalIgnoreCase)
           || state.Equals("Indexing", StringComparison.OrdinalIgnoreCase)
           || state.Equals("Rebuilding", StringComparison.OrdinalIgnoreCase);

    public void Apply(Progress p)
    {
        if (Cleaning) return; // 清理中：不让迟到的进度帧覆盖走马灯提示
        if (p.Mount.Length > 0) Mount = p.Mount;
        // 只有活动扫描才进入「stats 不覆盖」宽限期（R2-3）；Watching/Stopped 等
        // 终态立即放行 stats 快照，否则索引完成的 100% 要等 5 秒才显示出来
        _lastProgressAt = IsActive(p.State) ? DateTime.UtcNow : DateTime.MinValue;
        LastStateAt = DateTime.UtcNow;
        State = p.State;
        Live = !string.Equals(p.State, "Watching", StringComparison.OrdinalIgnoreCase);
        IndexLine = Format.StateText(p.State);
        StateBrush = Format.ResolveBrush(Format.StateBrushKey(p.State));
        IsPaused = string.Equals(p.State, "Paused", StringComparison.OrdinalIgnoreCase);
        CanPause = IsActive(p.State)
                   || string.Equals(p.State, "Watching", StringComparison.OrdinalIgnoreCase);
        CanResume = !CanPause;

        var active = IsActive(p.State);
        var total = p.FilesTotal;
        // files_total 是「已发现」的滚动累加值（每发现一个文件就立刻处理它，恒等于
        // done），算不出百分比。百分比用子树进度：根目录一级子树总数开扫前即已知。
        // 只有拿不到子树进度时才退回不确定进度条。
        var known = total > p.FilesDone;
        double? frac = null;
        if (string.Equals(p.State, "Watching", StringComparison.OrdinalIgnoreCase))
        {
            // 进入增量监控 = 全量索引完成：必须是 100%（进度事件里
            // subs 计数可能来自重启前的持久快照，直接按状态兜底）
            frac = 1;
        }
        else if (p.SubsTotal > 0)
        {
            frac = Math.Clamp((double)Math.Min(p.SubsDone, p.SubsTotal) / p.SubsTotal, 0, 1);
        }
        else if (known && total > 0)
        {
            frac = Math.Clamp((double)p.FilesDone / total, 0, 1);
        }

        Indeterminate = active && frac is null;
        Fraction = frac ?? 0;
        PercentText = frac is { } f
            ? $"{f * 100:0}%"
            : (active ? "扫描中…" : (total == 0 ? "—" : "0%"));

        var sb = _statsLineSb;
        sb.Clear();
        if (known) sb.Append($"{p.FilesDone:N0} / {p.FilesTotal:N0} 文件");
        else sb.Append($"已处理 {p.FilesDone:N0} 个文件");
        sb.Append($" · {p.Throughput.FilesPerSec:N0} 文件/秒");
        // IO 限速是索引进程级的（一个节流器管所有卷），每张卡显示同一个数字
        // 既重复又像显示错误——统一挪到「索引进度」标题行，见 IoLine。
        if (p.EtaSec is { } eta) sb.Append($" · 剩余约 {eta}s");
        if (p.FilesFailed > 0) sb.Append($" · 失败 {p.FilesFailed:N0}");
        StatsLine = sb.ToString();

        HasMemoryWarning = p.Memory.Pressure;
        MemoryWarning = HasMemoryWarning
            ? $"内存压力：索引进程内存 {Format.Human(p.Memory.RssBytes)} 超过预算 {Format.Human(p.Memory.BudgetBytes)}，正文抽取已降级（部分文件暂只索引文件名/路径）；内存回落后自动恢复，缺失正文会在后续扫描中补齐"
            : "";
    }

    /// <summary>
    /// 没有实时进度时用 stats 摘要填充。
    ///
    /// 索引完成（Watching）后 indexd 不再推进度事件，重启程序更是一帧都收不到；
    /// 只靠进度事件会让"索引进度"整块凭空消失，所以这里用 stats 兜底。
    /// </summary>
    public void ApplyStats(VolumeStats v)
    {
        if (Mount.Length == 0) Mount = v.Mount;
        LastStateAt = DateTime.UtcNow;

        // 5 秒内收到过实时 progress，说明卡片正被事件驱动，别用 stats 的旧快照覆盖——
        // 否则进度百分比会在两次实时事件之间被 stats 快照打回「—」（进度回退）。
        // （此前用「收到过非 Watching 的进度就永久忽略 stats」，导致暂停/停止后
        //   卡片永远停在最后一条进度、状态不再更新。）
        if ((DateTime.UtcNow - _lastProgressAt).TotalSeconds < 5) return;
        if (Cleaning) return; // 清理中：不让 stats 快照覆盖走马灯提示

        State = v.State;
        StateBrush = Format.ResolveBrush(Format.StateBrushKey(v.State));
        IsPaused = string.Equals(v.State, "Paused", StringComparison.OrdinalIgnoreCase);
        CanPause = IsActive(v.State)
                   || string.Equals(v.State, "Watching", StringComparison.OrdinalIgnoreCase);
        CanResume = !CanPause;

        // 已停止但仍有数据：索引可搜索，只是没在监控，别让用户误以为搜索失效
        IndexLine = v.State switch
        {
            "Stopped" when v.Docs > 0 => "已停止（数据可搜索）",
            _ => Format.StateText(v.State),
        };
        // 只有「已完成扫描、进入监控」才是真的 100%；此前用「文档数 > 0」判定会让
        // 正在索引（或重启后仍在索引）的卷直接显示 100%。
        var finished = string.Equals(v.State, "Watching", StringComparison.OrdinalIgnoreCase);
        if (finished)
        {
            Fraction = v.Docs > 0 ? 1 : 0;
            PercentText = v.Docs > 0 ? "100%" : "—";
        }
        else if (!IsPaused)
        {
            Fraction = 0;
            PercentText = "—";
        }
        // 暂停状态下保留最后一次实时进度，避免进度条归零造成「进度丢了」的错觉

        var sb = new System.Text.StringBuilder();
        sb.Append($"{v.Docs} 个文档");
        sb.Append($" · 索引 {Format.Human(v.IndexBytes)}");
        sb.Append($" · {v.Segments} 段");
        if (!finished) sb.Append(" · 等待实时进度");
        StatsLine = sb.ToString();

        HasMemoryWarning = false;
        MemoryWarning = "";
    }

    /// <summary>只更新状态（控制动作的即时回执，不改变已有进度数值）。</summary>
    public void ApplyState(string state)
    {
        if (string.IsNullOrEmpty(state)) return;
        if (Cleaning) return; // 清理中：状态回执等清理结束（撤卡）再体现
        LastStateAt = DateTime.UtcNow;
        State = state;
        IndexLine = Format.StateText(state);
        StateBrush = Format.ResolveBrush(Format.StateBrushKey(state));
        IsPaused = string.Equals(state, "Paused", StringComparison.OrdinalIgnoreCase);
        CanPause = IsActive(state)
                   || string.Equals(state, "Watching", StringComparison.OrdinalIgnoreCase);
        CanResume = !CanPause;
    }

    /// <summary>清理并删除索引进行中：进度条走马灯 + 明确文案，期间忽略其他更新。</summary>
    public bool Cleaning { get; private set; }

    /// <summary>
    /// 进入「清理中」展示态：删除大索引要花不少时间，原来的进度数字留着不动
    /// 会让用户以为没点上/卡死。切走马灯进度条 + 明确文案，删除完成由调用方撤卡。
    /// </summary>
    public void BeginCleaning()
    {
        Cleaning = true;
        State = "Cleaning";
        Live = false;
        LastStateAt = DateTime.UtcNow;
        IndexLine = "正在清理";
        StateBrush = Format.ResolveBrush("Warn");
        IsPaused = false;
        CanPause = false;
        CanResume = false; // 清理中暂停/继续均无意义
        Indeterminate = true;
        Fraction = 0;
        PercentText = "清理中…";
        StatsLine = "正在删除该范围的索引数据，完成后此卡片自动消失…";
        HasMemoryWarning = false;
        MemoryWarning = "";
    }

    /// <summary>清理失败（索引数据仍在）：退出走马灯，恢复为可操作的静止展示。</summary>
    public void EndCleaning()
    {
        if (!Cleaning) return;
        Cleaning = false;
        Indeterminate = false;
        PercentText = "—";
        StatsLine = "清理未完成，索引数据保留。";
    }
}

/// <summary>
/// 索引页：范围选择、进度、卷操作、添加目录。
///
/// 注意与 Setup 页的差异：这里的复选框**不禁用网络卷**（与 Rust 版一致）。
/// </summary>
public sealed partial class IndexViewModel : ObservableObject
{
    private readonly ShellViewModel _shell;
    private readonly Dictionary<string, ProgressRow> _progressMap = new(StringComparer.OrdinalIgnoreCase);

    public IndexViewModel(ShellViewModel shell)
    {
        _shell = shell;
        // 没有实时进度时也先告诉用户当前档位（配置里的 index.throttle）
        IoLine = $"IO 限速 {Format.ThrottleText(_shell.Config.IoThrottle)}";
    }

    public ObservableCollection<VolumeRow> Rows { get; } = new();
    public ObservableCollection<ProgressRow> ProgressRows { get; } = new();

    /// <summary>
    /// IO 限速与合计速率：整个索引进程共享一个节流器（所有卷加总），
    /// 所以只在这里显示一处，不再往每张进度卡上贴同一个数字。
    /// </summary>
    [ObservableProperty] private string _ioLine = "";

    [ObservableProperty] private bool _busy;
    [ObservableProperty] private string _scopeDirEdit = "";
    [ObservableProperty] private string _scopeError = "";
    [ObservableProperty] private bool _hasScopeError;
    [ObservableProperty] private string _selectionSummary = "已选 0 个范围";
    [ObservableProperty] private string _footerText = "";
    [ObservableProperty] private bool _hasStats;
    [ObservableProperty] private string _emptyText = "尚未建立索引。去“搜索”页或首次向导选择范围。";

    public void ApplyScope(ScopePreview? scope)
    {
        // 重建行时必须保留用户已勾选的范围与已有状态：重扫磁盘/添加目录都会走到这里。
        // 之前只保留勾选——结果点一下「重新扫描磁盘」，所有盘符瞬间变回「未索引」，
        // 看起来像索引全丢了，实际后台仍在索引或早已完成。
        var previouslySelected = Rows
            .Where(r => r.Selected)
            .Select(r => r.VolumeId)
            .ToHashSet(StringComparer.OrdinalIgnoreCase);
        var previousState = Rows.ToDictionary(
            r => r.VolumeId,
            r => (r.StateText, r.StateBrushKey, r.StateBrush, r.DocsText, r.RowState),
            StringComparer.OrdinalIgnoreCase);

        Rows.Clear();
        if (scope is null) return;

        foreach (var v in scope.Volumes)
        {
            var row = new VolumeRow
            {
                VolumeId = v.VolumeId,
                Mount = v.Mount,
                KindLabel = string.Equals(v.Fs, "dir", StringComparison.OrdinalIgnoreCase)
                    ? (v.Network ? "网络目录" : "自定义目录")
                    : v.Network ? "网络卷" : v.Removable ? "可移动" : "本地磁盘",
                IsCustom = string.Equals(v.Fs, "dir", StringComparison.OrdinalIgnoreCase),
                TotalText = v.TotalBytes > 0 ? Format.Human(v.TotalBytes) : "—",
                Selected = previouslySelected.Contains(v.VolumeId),
            };
            if (previousState.TryGetValue(v.VolumeId, out var st))
            {
                row.StateText = st.StateText;
                row.StateBrushKey = st.StateBrushKey;
                row.StateBrush = st.StateBrush;
                row.DocsText = st.DocsText;
                row.RowState = st.RowState;
            }
            Rows.Add(row);
        }
        UpdateSelectionSummary();
    }

    public void ApplyStats(Stats stats)
    {
        HasStats = true;
        FooterText = $"索引目录 {stats.IndexRoot} · 后端 {stats.Backend} · 共 {stats.TotalDocs} 文档";

        foreach (var row in Rows)
        {
            var vol = stats.Volumes.FirstOrDefault(v =>
                string.Equals(v.VolumeId, row.VolumeId, StringComparison.OrdinalIgnoreCase));
            if (vol is null)
            {
                row.RowState = "Discovered";
                row.StateText = "未索引";
                row.StateBrushKey = "Text2";
                row.StateBrush = Format.ResolveBrush(row.StateBrushKey);
                row.DocsText = "—";
            }
            else
            {
                row.RowState = vol.State;
                row.StateText = Format.StateText(vol.State);
                row.StateBrushKey = Format.StateBrushKey(vol.State);
                row.StateBrush = Format.ResolveBrush(row.StateBrushKey);
                row.DocsText = vol.Docs.ToString();
            }
        }

        // 已索引 / 索引中的卷各留一张卡片：没有实时进度（重启或已就绪）时用 stats 摘要
        foreach (var vol in stats.Volumes)
        {
            if (vol.Docs == 0 && string.Equals(vol.State, "Discovered", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }
            GetOrCreateProgress(vol.VolumeId, vol.Mount).ApplyStats(vol);
        }

        // 进度卡片只保留仍存在的卷。
        // 两类例外不能被这轮刷新误删：
        // 1) 刚建卡 10 秒内的卷——排队等待并行槽位 / 打开索引目录期间 stats 里暂时没有它；
        // 2) Degraded 的卷——索引目录打不开时永远不会进入 stats 的内存卷表，
        //    卡片是用户看到失败原因的唯一途径（clean 会主动删卡，不受影响）。
        var ids = stats.Volumes.Select(v => v.VolumeId).ToHashSet(StringComparer.OrdinalIgnoreCase);
        var now = DateTime.UtcNow;
        for (var i = ProgressRows.Count - 1; i >= 0; i--)
        {
            var card = ProgressRows[i];
            if (ids.Contains(card.VolumeId)) continue;
            if ((now - card.LastStateAt).TotalSeconds < 10) continue;
            if (string.Equals(card.State, "Degraded", StringComparison.OrdinalIgnoreCase)) continue;
            _progressMap.Remove(card.VolumeId);
            ProgressRows.RemoveAt(i);
        }
        OnPropertyChanged(nameof(HasProgress));
    }

    public void ApplyProgress(Progress p)
    {
        var level = Format.ThrottleText(p.Io.Throttle);
        IoLine = p.Io.ActualBytesPerSec > 0
            ? $"IO 限速 {level}（全部卷共享）· 合计读写 {Format.Human(p.Io.ActualBytesPerSec)}/秒"
            : $"IO 限速 {level}（全部卷共享）";
        GetOrCreateProgress(p.VolumeId, p.Mount).Apply(p);
        OnPropertyChanged(nameof(HasProgress));
    }

    /// <summary>
    /// VolumeState 事件兜底建卡：排队等待槽位、或索引目录打不开的卷不会产生
    /// progress 事件，stats 也查不到——不在这里建卡，用户就看不到「任务已开始/失败」。
    /// </summary>
    public void ApplyVolumeState(string volumeId, string mount, string state)
    {
        if (string.IsNullOrEmpty(volumeId)) return;
        var card = GetOrCreateProgress(volumeId,
            string.IsNullOrEmpty(mount) ? FindMount(volumeId) : mount);
        card.ApplyState(state);
        OnPropertyChanged(nameof(HasProgress));
    }

    /// <summary>index.start 被接受后立即为这些卷建卡（Scanning），不等事件往返。</summary>
    public void MarkIndexingStarted(IEnumerable<string> volumeIds)
    {
        foreach (var id in volumeIds)
        {
            if (string.IsNullOrEmpty(id)) continue;
            GetOrCreateProgress(id, FindMount(id)).ApplyState("Scanning");
        }
        OnPropertyChanged(nameof(HasProgress));
    }

    /// <summary>主动移除一张进度卡（clean 成功后调用）。</summary>
    public void RemoveProgressCard(string volumeId)
    {
        if (!_progressMap.Remove(volumeId, out var card)) return;
        ProgressRows.Remove(card);
        OnPropertyChanged(nameof(HasProgress));
    }

    private string FindMount(string volumeId)
        => Rows.FirstOrDefault(r => string.Equals(r.VolumeId, volumeId, StringComparison.OrdinalIgnoreCase))?.Mount
           ?? volumeId;

    /// <summary>有没有可展示的索引进度/状态卡片。</summary>
    public bool HasProgress => ProgressRows.Count > 0;

    private ProgressRow GetOrCreateProgress(string volumeId, string mount)
    {
        if (_progressMap.TryGetValue(volumeId, out var row)) return row;
        row = new ProgressRow { VolumeId = volumeId, Mount = mount };
        _progressMap[volumeId] = row;
        ProgressRows.Add(row);
        return row;
    }

    private void UpdateSelectionSummary()
        => SelectionSummary = $"已选 {Rows.Count(r => r.Selected)} 个范围";

    [RelayCommand]
    private async Task StartSelectedAsync()
    {
        var volumes = Rows.Where(r => r.Selected).Select(r => r.VolumeId).ToList();
        if (volumes.Count == 0)
        {
            _shell.SetNotice("没有选中任何磁盘或目录");
            return;
        }
        await StartVolumesAsync(volumes);
    }

    [RelayCommand]
    private async Task StartAllFixedAsync()
    {
        var scope = _shell.Scope;
        if (scope is null) return;
        var volumes = scope.Volumes.Where(v => !v.Network && !v.Removable).Select(v => v.VolumeId).ToList();
        foreach (var row in Rows) row.Selected = volumes.Contains(row.VolumeId, StringComparer.OrdinalIgnoreCase);
        UpdateSelectionSummary();
        await StartVolumesAsync(volumes);
    }

    private async Task StartVolumesAsync(List<string> volumes)
    {
        Busy = true;
        try
        {
            var ack = await _shell.RequestAsync<IndexStartAck>(Msg.IndexStart, new IndexStart { Volumes = volumes });
            var started = ack?.Started?.Count ?? 0;
            if (started == 0)
            {
                _shell.SetNotice("索引服务没有接受任何范围，请确认磁盘/目录可访问后重试。");
                return;
            }

            // 立即为已接受的卷建卡：后端要等并行槽位/打开索引目录后才有 progress，
            // 这段空窗不建卡的话，用户（尤其勾了网络盘时）会以为点了没反应
            MarkIndexingStarted(ack!.Started);

            // 明确回报实际加入的卷数：网络盘离线、目录不可访问时这里会少，避免「点了没反应」
            var rejected = volumes.Count - started;
            _shell.SetNotice(rejected > 0
                ? $"已开始索引 {started} 个范围（{rejected} 个未被接受，可能已离线或不可访问）"
                : $"已开始索引 {started} 个范围，可在“索引”页查看进度。");
            _shell.ScheduleStatsRefresh(1200);
        }
        catch (Exception ex)
        {
            _shell.SetNotice($"开始索引失败：{ex.Message}");
        }
        finally
        {
            Busy = false;
        }
    }

    [RelayCommand]
    private async Task RescanAsync()
    {
        Busy = true;
        try
        {
            await _shell.RefreshScopeAsync();
            // 行刚重建（状态是从旧行继承的快照）：立刻用最新 stats 回填，
            // 避免「正在索引/已完成」的卷停留在过期状态
            await _shell.RefreshStatsAsync();
        }
        finally { Busy = false; }
    }

    [RelayCommand]
    private async Task AddDirectoryAsync()
    {
        var path = (ScopeDirEdit ?? "").Trim().Trim('"');
        if (path.Length == 0) return;

        try
        {
            if (!Directory.Exists(path))
            {
                _shell.SetNotice(File.Exists(path) ? $"不是目录：{path}" : $"目录不可访问：{path}");
                return;
            }
        }
        catch (Exception ex)
        {
            _shell.SetNotice($"目录不可访问：{path}（{ex.Message}）");
            return;
        }

        Busy = true;
        try
        {
            _shell.SetNotice($"正在添加目录：{path}");
            var ack = await _shell.RequestAsync<Dictionary<string, string>>(Msg.ScopeRegister, new { path });
            var id = ack is not null && ack.TryGetValue("volume_id", out var v) ? v : null;
            if (string.IsNullOrEmpty(id) && ack is not null && ack.TryGetValue("volumeId", out var v2)) id = v2;

            // 刷新范围列表（保留了已有勾选），再按挂载点兜底匹配新登记的范围
            await _shell.RefreshScopeAsync();
            if (string.IsNullOrEmpty(id))
            {
                var full = SafeFullPath(path);
                id = Rows.FirstOrDefault(r =>
                        string.Equals(SafeFullPath(r.Mount), full, StringComparison.OrdinalIgnoreCase))
                    ?.VolumeId;
            }
            if (string.IsNullOrEmpty(id))
            {
                // 注册失败不再静默丢掉：保留输入框内容，提示用户
                _shell.SetNotice($"添加目录失败：{path}（索引服务未返回范围标识）");
                return;
            }

            foreach (var row in Rows)
            {
                if (string.Equals(row.VolumeId, id, StringComparison.OrdinalIgnoreCase)) row.Selected = true;
            }
            UpdateSelectionSummary();
            ScopeDirEdit = "";
            // 立即把新目录加入索引任务
            await StartVolumesAsync(new List<string> { id });
        }
        catch (Exception ex)
        {
            _shell.SetNotice($"添加目录失败：{ex.Message}");
        }
        finally
        {
            Busy = false;
        }
    }

    private static string SafeFullPath(string p)
    {
        try { return Path.GetFullPath(p).TrimEnd('\\', '/'); }
        catch { return p.TrimEnd('\\', '/'); }
    }

    [RelayCommand]
    private async Task VolumeActionAsync(string? arg)
    {
        var parts = (arg ?? "").Split('|', 2);
        if (parts.Length != 2) return;
        var action = parts[0];
        var volumeId = parts[1];

        var label = action switch
        {
            "pause" => "暂停",
            "resume" => "继续",
            "rebuild" => "重建",
            "clean" => "清理",
            "unregister" => "移除",
            _ => action,
        };

        // 移除自定义目录 = 从范围删掉 + 删索引数据，不可逆：先确认
        if (action == "unregister")
        {
            var mount = Rows
                .FirstOrDefault(r => string.Equals(r.VolumeId, volumeId, StringComparison.OrdinalIgnoreCase))
                ?.Mount ?? volumeId;
            var tcs = new TaskCompletionSource();
            ConfirmRequested?.Invoke(
                "移除自定义目录",
                $"将从索引范围移除 {mount}，并删除它的全部索引数据（目录里的文件不受影响）。此操作不可撤销，确定继续？",
                async () =>
                {
                    await DoVolumeActionAsync(action, volumeId, label);
                    tcs.TrySetResult();
                });
            await tcs.Task;
            return;
        }

        await DoVolumeActionAsync(action, volumeId, label);
    }

    private async Task DoVolumeActionAsync(string action, string volumeId, string label)
    {
        // 点击后立刻给反馈：后端动作是异步的，等 200ms 的进度帧会让用户以为没点上
        _shell.SetNotice($"正在{label}…");
        // 删除大索引耗时较长：卡片立刻切「清理中」走马灯，明确告知正在动
        ProgressRow? cleanCard = null;
        if (action is "clean" or "unregister") _progressMap.TryGetValue(volumeId, out cleanCard);
        cleanCard?.BeginCleaning();
        try
        {
            var ack = await _shell.RequestStrictAsync<VolumeActionAck>(
                Msg.VolumeAction,
                new VolumeAction { Action = action, VolumeId = volumeId });

            // 后端回传每个卷的最终状态：立即更新范围表与卡片
            if (ack?.States is { Count: > 0 } states)
            {
                foreach (var (id, state) in states) UpdateRowState(id, state);
                if (action is "clean" or "unregister")
                {
                    // 索引数据已删除：主动撤卡，否则要等 10 秒宽限才消失
                    foreach (var id in states.Keys) RemoveProgressCard(id);
                }
            }

            _shell.SetNotice(action switch
            {
                "pause" => "已暂停该范围的索引。",
                "resume" => "已继续该范围的索引。",
                "rebuild" => "已开始重建该范围的索引。",
                "clean" => "已清理并删除该范围的索引数据。",
                "unregister" => "已移除该目录并删除其索引数据。",
                _ => $"已{label}。",
            });
            if (action == "unregister")
            {
                // 该行已不在后端范围里：刷新范围列表让这一行消失
                await _shell.RefreshScopeAsync();
                _shell.ScheduleStatsRefresh(800);
            }
            else
            {
                _shell.ScheduleStatsRefresh(action == "clean" ? 800 : 400);
            }
        }
        catch (Exception ex)
        {
            cleanCard?.EndCleaning();
            _shell.SetNotice($"{label}失败：{ex.Message}");
        }
    }

    // ---- 批量操作（全部卷一键控制）--------------------------------------

    /// <summary>
    /// 请求界面弹出确认框：参数为 (标题, 说明, 用户确认后执行的操作)。
    /// 由 IndexView 弹出覆盖层；重建/删除索引这类不可逆操作必须先确认。
    /// </summary>
    public event Action<string, string, Func<Task>>? ConfirmRequested;

    [RelayCommand]
    private Task BulkPauseAsync() => RunBulkAsync("pause", "暂停", requiresConfirm: false);

    [RelayCommand]
    private Task BulkResumeAsync() => RunBulkAsync("resume", "继续", requiresConfirm: false);

    [RelayCommand]
    private Task BulkRebuildAsync() => RunBulkAsync("rebuild", "重建", requiresConfirm: true);

    [RelayCommand]
    private Task BulkCleanAsync() => RunBulkAsync("clean", "清理并删除索引", requiresConfirm: true);

    /// <summary>按动作筛选目标卷：暂停只针对在跑的，继续只针对已暂停的。</summary>
    private List<string> BulkTargets(string action) => action switch
    {
        "pause" => Rows.Where(r => IsActiveState(r.RowState)).Select(r => r.VolumeId).ToList(),
        "resume" => Rows
            .Where(r => string.Equals(r.RowState, "Paused", StringComparison.OrdinalIgnoreCase))
            .Select(r => r.VolumeId).ToList(),
        _ => Rows.Where(r => HasIndexData(r.RowState)).Select(r => r.VolumeId).ToList(),
    };

    private static bool IsActiveState(string s)
        => s is "Scanning" or "Indexing" or "Rebuilding" or "Watching";

    /// <summary>已有索引数据（或正在进行）的卷才允许重建/删除；「未索引」跳过。</summary>
    private static bool HasIndexData(string s)
        => s is "Scanning" or "Indexing" or "Rebuilding" or "Watching" or "Paused" or "Stopped" or "Degraded";

    private Task RunBulkAsync(string action, string label, bool requiresConfirm)
    {
        var targets = BulkTargets(action);
        if (targets.Count == 0)
        {
            _shell.SetNotice(action switch
            {
                "pause" => "没有正在索引的范围可暂停。",
                "resume" => "没有已暂停的范围可继续。",
                _ => "没有已建立索引的范围。",
            });
            return Task.CompletedTask;
        }

        if (!requiresConfirm)
            return RunBulkCoreAsync(action, targets, label);

        // 重建会清空现有索引数据、删除会直接销毁索引数据，都必须用户确认
        var message = action == "rebuild"
            ? $"将对 {targets.Count} 个范围重建索引：现有索引数据会先清空再全量重扫，期间搜索结果不完整，耗时可能较长。\n\n确定继续？"
            : $"将删除 {targets.Count} 个范围的全部索引数据（磁盘文件本身不受影响），删除后需重新完整索引才能按内容搜索。\n\n此操作不可撤销，确定继续？";
        var tcs = new TaskCompletionSource();
        ConfirmRequested?.Invoke($"批量{label}", message, async () =>
        {
            await RunBulkCoreAsync(action, targets, label);
            tcs.TrySetResult();
        });
        return tcs.Task;
    }

    private async Task RunBulkCoreAsync(string action, List<string> targets, string label)
    {
        if (Busy) return;
        Busy = true;
        var ok = 0;
        var failed = 0;
        try
        {
            for (var i = 0; i < targets.Count; i++)
            {
                // 逐卷反馈进度，全量重建/删除耗时较长时用户知道在动
                _shell.SetNotice($"正在{label} {i + 1}/{targets.Count}…");
                if (action == "clean" && _progressMap.TryGetValue(targets[i], out var bulkCard))
                    bulkCard.BeginCleaning();
                try
                {
                    var ack = await _shell.RequestStrictAsync<VolumeActionAck>(
                        Msg.VolumeAction,
                        new VolumeAction { Action = action, VolumeId = targets[i] });
                    if (ack?.States is { Count: > 0 } states)
                    {
                        foreach (var (id, state) in states) UpdateRowState(id, state);
                        if (action == "clean")
                            foreach (var id in states.Keys) RemoveProgressCard(id);
                    }
                    ok++;
                }
                catch (Exception ex)
                {
                    if (action == "clean" && _progressMap.TryGetValue(targets[i], out var failCard))
                        failCard.EndCleaning();
                    failed++;
                    Helpers.Log.Write($"bulk {action} {targets[i]} failed: {ex.Message}");
                }
            }

            _shell.SetNotice(failed == 0
                ? $"已{label} {ok} 个范围。"
                : $"已{label} {ok} 个范围，{failed} 个失败（详情见日志）。");
            _shell.ScheduleStatsRefresh(action == "clean" ? 800 : 400);
        }
        finally
        {
            Busy = false;
        }
    }

    /// <summary>把后端返回的最终状态同步到范围表与进度卡片。</summary>
    private void UpdateRowState(string volumeId, string state)
    {
        foreach (var row in Rows)
        {
            if (!string.Equals(row.VolumeId, volumeId, StringComparison.OrdinalIgnoreCase)) continue;
            row.RowState = state;
            row.StateText = Format.StateText(state);
            row.StateBrushKey = Format.StateBrushKey(state);
            row.StateBrush = Format.ResolveBrush(row.StateBrushKey);
        }
        if (_progressMap.TryGetValue(volumeId, out var card)) card.ApplyState(state);
    }

    [RelayCommand]
    private async Task RetryAsync()
    {
        ScopeError = "";
        HasScopeError = false;
        _shell.ClearBootError();
        // client 已死或从未连上时先重启 indexd，否则下面的刷新永远报「索引进程未运行」
        if (!await _shell.EnsureConnectedAsync()) return;
        await _shell.RefreshStatsAsync();
        await _shell.RefreshScopeAsync();
    }

    public void NotifySelectionChanged() => UpdateSelectionSummary();
}
