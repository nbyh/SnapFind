using System.Collections.ObjectModel;
using System.Net;
using Avalonia.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using SnapFind.Desktop.Helpers;
using SnapFind.Desktop.Ipc;

namespace SnapFind.Desktop.ViewModels;

/// <summary>预览正文的一个显示行（软换行后的片段，不是原文行）。</summary>
public sealed partial class PreviewLine : ObservableObject
{
    public int No { get; init; }
    public string Text { get; init; } = "";

    /// <summary>查询词（所有行共享同一实例，避免逐行复制）。</summary>
    public IReadOnlyList<string> Terms { get; init; } = Array.Empty<string>();

    [ObservableProperty] private bool _queryHit;
    [ObservableProperty] private bool _findHit;
    [ObservableProperty] private bool _findCurrent;
}

/// <summary>摘要片段：字段名 + 已去掉标记标签的文本 + 用于高亮的词。</summary>
public sealed class SnippetBlock
{
    public string FieldLabel { get; init; } = "";
    public string Text { get; init; } = "";
    public IReadOnlyList<string> Terms { get; init; } = Array.Empty<string>();
}

/// <summary>
/// 右侧预览面板。
///
/// 正文由 indexd 的 <c>preview.read</c> 返回（复用 Rust 的抽取器，支持 docx/xlsx/pdf/epub）。
/// 软换行与 Rust 版 <c>PreviewData</c> 完全同构：按显示宽度单位切分
/// （ASCII 记 1、非 ASCII 记 2），尽量在空格处断开，**不丢失任何字符**。
/// </summary>
public sealed partial class PreviewViewModel : ObservableObject
{
    private const int MaxLines = 50_000;
    /// <summary>与 Rust 版一致：默认每行显示宽度单位。</summary>
    public const int DefaultWrapUnits = 48;

    private readonly Func<IpcClient?> _client;
    private int _seq;
    private string _bodyText = "";
    private int _wrapUnits = DefaultWrapUnits;
    private List<int> _findHits = new();
    private List<int> _lineOffsets = new(); // 每个显示行在原文中的起始偏移，用于 rewrap 定位

    public PreviewViewModel(Func<IpcClient?> client) => _client = client;

    /// <summary>当前查询词（用于正文行内高亮与命中行标记）。</summary>
    public IReadOnlyList<string> QueryTerms { get; set; } = Array.Empty<string>();

    [ObservableProperty] private HitItem? _hit;
    [ObservableProperty] private bool _loading;
    [ObservableProperty] private bool _nameOnly;
    [ObservableProperty] private bool _truncated;
    [ObservableProperty] private bool _isEmpty = true;
    [ObservableProperty] private string _loadError = "";
    [ObservableProperty] private string _metaLine = "";

    [ObservableProperty] private IReadOnlyList<PreviewLine> _lines = Array.Empty<PreviewLine>();
    [ObservableProperty] private string _lineCountText = "";

    [ObservableProperty] private string _findText = "";
    [ObservableProperty] private bool _findOpen;
    [ObservableProperty] private string _findCounter = "";
    [ObservableProperty] private int _findCurrentIndex = -1;
    /// <summary>区分大小写（Ctrl+F 查找选项）。</summary>
    [ObservableProperty] private bool _findMatchCase;
    /// <summary>全字匹配（Ctrl+F 查找选项）。</summary>
    [ObservableProperty] private bool _findWholeWord;

    public ObservableCollection<SnippetBlock> Snippets { get; } = new();

    /// <summary>请求滚动到指定显示行（0 基）。</summary>
    public event Action<int>? ScrollToLineRequested;
    /// <summary>显示行重建完成（软换行变化 / 新文档），供标尺刷新。</summary>
    public event Action? LinesRebuilt;

    public bool HasHit => Hit is not null;
    public bool ShowSnippets => Snippets.Count > 0;
    public bool HasLoadError => !string.IsNullOrEmpty(LoadError);

    public int WrapUnits => _wrapUnits;

    /// <summary>当前显示行总数（标尺按它换算比例）。</summary>
    [ObservableProperty] private int _totalLines;

    /// <summary>查询关键词命中的显示行（0 基），供标尺绘制蓝色标记。</summary>
    [ObservableProperty] private IReadOnlyList<int> _queryHitLines = Array.Empty<int>();
    /// <summary>Ctrl+F 命中的显示行（0 基，上限 2000 与 Rust 版一致），供标尺绘制橙色标记。</summary>
    [ObservableProperty] private IReadOnlyList<int> _findHitLines = Array.Empty<int>();
    /// <summary>当前定位到的 Ctrl+F 命中行（0 基），-1 表示无。</summary>
    [ObservableProperty] private int _findCurrentLine = -1;

    partial void OnLoadErrorChanged(string value) => OnPropertyChanged(nameof(HasLoadError));

    /// <summary>清空（切换筛选后没有可预览项时）。</summary>
    public void Clear()
    {
        _seq++;
        _bodyText = "";
        Hit = null;
        Lines = Array.Empty<PreviewLine>();
        TotalLines = 0;
        Snippets.Clear();
        NameOnly = false;
        Truncated = false;
        Loading = false;
        IsEmpty = true;
        LoadError = "";
        MetaLine = "";
        LineCountText = "";
        FindText = "";
        FindOpen = false;
        FindCounter = "";
        _findHits.Clear();
        QueryHitLines = Array.Empty<int>();
        FindHitLines = Array.Empty<int>();
        FindCurrentLine = -1;
        OnPropertyChanged(nameof(HasHit));
        OnPropertyChanged(nameof(ShowSnippets));
        LinesRebuilt?.Invoke();
    }

    public void Load(HitItem? item)
    {
        if (item is null)
        {
            Clear();
            return;
        }

        Hit = item;
        IsEmpty = false;
        OnPropertyChanged(nameof(HasHit));

        MetaLine = $"{Format.Human(item.Hit.Size)} · {Format.Time(item.Hit.Mtime)} · " +
                   (string.IsNullOrEmpty(item.Hit.Ext) ? "无扩展名" : item.Hit.Ext);

        // 摘要：去掉 <mark> 标记并反转义实体，再用查询词重新高亮
        Snippets.Clear();
        foreach (var s in item.Snippets)
        {
            Snippets.Add(new SnippetBlock
            {
                FieldLabel = Format.FieldLabel(s.Field),
                Text = StripMarkup(s.Text),
                Terms = QueryTerms,
            });
        }
        OnPropertyChanged(nameof(ShowSnippets));

        if (item.Hit.Flags.Encrypted)
        {
            NameOnly = false;
            _bodyText = "";
            Lines = Array.Empty<PreviewLine>();
            TotalLines = 0;
            LineCountText = "";
            LoadError = "该文档已加密，无法预览内容。";
            LinesRebuilt?.Invoke();
            return;
        }

        if (item.NameOnly)
        {
            NameOnly = true;
            _bodyText = "";
            Lines = Array.Empty<PreviewLine>();
            TotalLines = 0;
            LineCountText = "";
            LoadError = "";
            LinesRebuilt?.Invoke();
            return;
        }

        NameOnly = false;
        _ = LoadBodyAsync(item.Path);
    }

    private async Task LoadBodyAsync(string path)
    {
        var my = ++_seq;
        Loading = true;
        LoadError = "";
        _bodyText = "";
        Lines = Array.Empty<PreviewLine>();
        TotalLines = 0;
        LineCountText = "";
        QueryHitLines = Array.Empty<int>();
        FindHitLines = Array.Empty<int>();
        FindCurrentLine = -1;
        LinesRebuilt?.Invoke();

        try
        {
            var client = _client();
            if (client is null)
            {
                await Dispatcher.UIThread.InvokeAsync(() =>
                {
                    if (my != _seq) return;
                    LoadError = "索引进程未运行，无法读取正文。";
                    Loading = false;
                });
                return;
            }

            var resp = await client.RequestAsync<PreviewRead>(
                Msg.PreviewRead,
                new PreviewReadRequest { Path = path, MaxBytes = 32L * 1024 * 1024 });

            if (my != _seq) return;

            await Dispatcher.UIThread.InvokeAsync(() =>
            {
                if (my != _seq) return;
                if (resp is null || resp.Error is { Length: > 0 })
                {
                    LoadError = resp?.Error ?? "无法读取正文。";
                    Loading = false;
                    return;
                }
                if (resp.NameOnly)
                {
                    NameOnly = true;
                    Loading = false;
                    return;
                }
                Truncated = resp.Truncated;
                _bodyText = resp.Text ?? "";
                Loading = false;
                Rebuild(jumpToHit: true);
            });
        }
        catch (Exception ex)
        {
            if (my != _seq) return;
            await Dispatcher.UIThread.InvokeAsync(() =>
            {
                LoadError = $"读取正文失败：{ex.Message}";
                Loading = false;
            });
        }
    }

    /// <summary>
    /// 面板宽度变化时调整每行显示宽度。返回 true 表示已重建显示行
    /// （调用方需要按新行数恢复滚动位置）。
    /// </summary>
    public bool SetWrapUnits(int units)
    {
        units = Math.Clamp(units, 16, 200);
        if (units == _wrapUnits) return false;
        _wrapUnits = units;
        if (_bodyText.Length == 0) return false;
        // rewrap 只改断行位置，不强行跳转（调用方按比例恢复滚动）
        Rebuild(jumpToHit: false);
        return true;
    }

    // ---- 软换行 ----------------------------------------------------------

    private void Rebuild(bool jumpToHit = true)
    {
        var terms = QueryTerms;
        var result = new List<PreviewLine>(Math.Min(_bodyText.Length / 40 + 8, MaxLines));
        var queryHits = new List<int>();

        var parts = _bodyText.Split('\n');
        var count = parts.Length;
        // 末尾换行不额外产生一个空行（与 Rust 版一致）
        if (_bodyText.EndsWith('\n')) count--;

        var lineNo = 0;
        var truncated = false;

        for (var i = 0; i < count; i++)
        {
            if (lineNo >= MaxLines)
            {
                truncated = true;
                break;
            }

            var raw = parts[i].TrimEnd('\r');
            var start = 0;
            var units = 0;
            var lastSpace = -1;

            for (var k = 0; k < raw.Length; k++)
            {
                if (lineNo >= MaxLines)
                {
                    truncated = true;
                    break;
                }
                var w = char.IsAscii(raw[k]) ? 1 : 2;
                if (units + w > _wrapUnits && k > start)
                {
                    var cut = lastSpace >= start + _wrapUnits / 3 ? lastSpace : k;
                    AddLine(result, queryHits, raw[start..cut], terms, lineNo++);
                    start = cut;
                    units = 0;
                    for (var j = start; j < k; j++) units += char.IsAscii(raw[j]) ? 1 : 2;
                    lastSpace = -1;
                }
                units += w;
                if (raw[k] == ' ') lastSpace = k + 1;
            }

            if (lineNo >= MaxLines)
            {
                truncated = true;
                break;
            }
            AddLine(result, queryHits, raw[start..], terms, lineNo++);
        }

        Lines = result;
        TotalLines = result.Count;
        QueryHitLines = queryHits;
        LineCountText = truncated ? $"{lineNo:N0} 行（已截断）" : $"{lineNo:N0} 行";

        // 打开预览即定位到首个查询命中行（rewrap 时不跳，交给调用方按比例恢复）
        if (jumpToHit)
        {
            if (queryHits.Count > 0) ScrollToLineRequested?.Invoke(queryHits[0]);
            else ScrollToLineRequested?.Invoke(0);
        }

        // 行重排后 Ctrl+F 命中要重算
        if (FindOpen && !string.IsNullOrWhiteSpace(FindText)) Recount();
        else RecomputeFindHits();

        LinesRebuilt?.Invoke();
    }

    private static void AddLine(
        List<PreviewLine> result, List<int> queryHits, string text,
        IReadOnlyList<string> terms, int index)
    {
        var hit = ContainsAny(text, terms);
        if (hit) queryHits.Add(index);
        result.Add(new PreviewLine
        {
            No = index + 1,
            Text = text,
            Terms = terms,
            QueryHit = hit,
        });
    }

    private static bool ContainsAny(string line, IReadOnlyList<string> terms)
    {
        foreach (var t in terms)
        {
            if (t.Length > 0 && line.Contains(t, StringComparison.OrdinalIgnoreCase)) return true;
        }
        return false;
    }

    // ---- 正文内查找 ------------------------------------------------------

    partial void OnFindTextChanged(string value) => Recount();
    partial void OnFindMatchCaseChanged(bool value) => Recount();
    partial void OnFindWholeWordChanged(bool value) => Recount();

    private StringComparison FindComparison
        => FindMatchCase ? StringComparison.Ordinal : StringComparison.OrdinalIgnoreCase;

    /// <summary>「词字符」只认 ASCII 字母/数字/下划线；CJK 视作分隔符，
    /// 否则中文里的全字匹配会退化成「整行都算一个词」。</summary>
    private static bool IsWordChar(char c)
        => c < 128 && (char.IsLetterOrDigit(c) || c == '_');

    /// <summary>按当前查找选项（大小写 / 全字）判断一行是否命中。</summary>
    private bool LineMatches(string line, string term)
    {
        if (term.Length == 0) return false;
        var cmp = FindComparison;
        if (!FindWholeWord) return line.Contains(term, cmp);

        var from = 0;
        while (from <= line.Length - term.Length)
        {
            var idx = line.IndexOf(term, from, cmp);
            if (idx < 0) return false;
            var before = idx - 1;
            var after = idx + term.Length;
            var leftOk = before < 0 || !IsWordChar(line[before]);
            var rightOk = after >= line.Length || !IsWordChar(line[after]);
            if (leftOk && rightOk) return true;
            from = idx + 1;
        }
        return false;
    }

    public void OpenFind()
    {
        FindOpen = true;
        Recount();
    }

    public void CloseFind()
    {
        FindOpen = false;
        FindText = "";
        FindCounter = "";
        _findHits.Clear();
        FindHitLines = Array.Empty<int>();
        FindCurrentLine = -1;
        foreach (var l in Lines)
        {
            l.FindHit = false;
            l.FindCurrent = false;
        }
        LinesRebuilt?.Invoke();
    }

    /// <summary>只更新橙色标记（不改当前命中），用于 rewrap 后同步标尺。</summary>
    private void RecomputeFindHits()
    {
        var term = FindText?.Trim() ?? "";
        if (term.Length == 0)
        {
            FindHitLines = Array.Empty<int>();
            FindCurrentLine = -1;
            return;
        }
        var hits = new List<int>();
        var lines = Lines;
        for (var i = 0; i < lines.Count; i++)
        {
            if (LineMatches(lines[i].Text, term)) hits.Add(i);
        }
        FindHitLines = hits.Count > 2000 ? hits.GetRange(0, 2000) : hits;
        FindCurrentLine = -1;
        FindCounter = hits.Count == 0 ? "无匹配" : $"{hits.Count} 处";
    }

    private void Recount()
    {
        var term = FindText?.Trim() ?? "";
        _findHits = new List<int>();
        if (term.Length == 0)
        {
            FindCounter = "";
            FindHitLines = Array.Empty<int>();
            FindCurrentLine = -1;
            foreach (var l in Lines)
            {
                l.FindHit = false;
                l.FindCurrent = false;
            }
            LinesRebuilt?.Invoke();
            return;
        }

        var lines = Lines;
        for (var i = 0; i < lines.Count; i++)
        {
            var hit = LineMatches(lines[i].Text, term);
            lines[i].FindHit = hit;
            lines[i].FindCurrent = false;
            if (hit) _findHits.Add(i);
        }

        // 标尺只画前 2000 个橙色标记，避免超大文件绘制过密
        FindHitLines = _findHits.Count > 2000 ? _findHits.GetRange(0, 2000) : _findHits;
        FindCurrentLine = -1;

        if (_findHits.Count == 0)
        {
            FindCounter = "无匹配";
            LinesRebuilt?.Invoke();
            return;
        }
        Step(1);
    }

    /// <summary>在查找命中之间循环跳转。</summary>
    public void Step(int delta)
    {
        if (_findHits.Count == 0)
        {
            FindCounter = string.IsNullOrWhiteSpace(FindText) ? "" : "无匹配";
            return;
        }

        var pos = _findHits.IndexOf(FindCurrentIndex);
        pos = pos < 0 ? 0 : (pos + delta + _findHits.Count) % _findHits.Count;
        FindCurrentIndex = _findHits[pos];
        FindCurrentLine = FindCurrentIndex;

        foreach (var l in Lines) l.FindCurrent = false;
        if (FindCurrentIndex >= 0 && FindCurrentIndex < Lines.Count)
        {
            Lines[FindCurrentIndex].FindCurrent = true;
        }
        FindCounter = $"{pos + 1} / {_findHits.Count}";
        ScrollToLineRequested?.Invoke(FindCurrentIndex);
        LinesRebuilt?.Invoke();
    }

    // ---- 摘要文本处理 ----------------------------------------------------

    /// <summary>去掉 <c>&lt;mark&gt;</c> 标签并反转义 HTML 实体。</summary>
    private static string StripMarkup(string text)
    {
        if (string.IsNullOrEmpty(text)) return "";
        var plain = text
            .Replace("<mark>", "", StringComparison.OrdinalIgnoreCase)
            .Replace("</mark>", "", StringComparison.OrdinalIgnoreCase);
        return WebUtility.HtmlDecode(plain);
    }
}
