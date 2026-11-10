using Avalonia;
using Avalonia.Controls;
using Avalonia.Controls.Documents;
using Avalonia.Media;

namespace SnapFind.Desktop.Controls;

/// <summary>
/// 支持关键词高亮的文本块：把命中的词片段渲染成带底色/前景色的 <see cref="Run"/>。
///
/// 注意：**不要复用 <see cref="TextBlock.Text"/>** —— 它与 <see cref="TextBlock.Inlines"/>
/// 互相覆盖（设置 Inlines 会回写 Text，Text 变化又会清空 Inlines），
/// 实际表现是正文行只剩行号、文字全空。所以这里用独立的 <see cref="SourceText"/>。
/// </summary>
public class MarkTextBlock : TextBlock
{
    public static readonly StyledProperty<string?> SourceTextProperty =
        AvaloniaProperty.Register<MarkTextBlock, string?>(nameof(SourceText));

    public static readonly StyledProperty<IReadOnlyList<string>?> TermsProperty =
        AvaloniaProperty.Register<MarkTextBlock, IReadOnlyList<string>?>(nameof(Terms));

    public static readonly StyledProperty<IBrush?> MarkBackgroundProperty =
        AvaloniaProperty.Register<MarkTextBlock, IBrush?>(nameof(MarkBackground));

    public static readonly StyledProperty<IBrush?> MarkForegroundProperty =
        AvaloniaProperty.Register<MarkTextBlock, IBrush?>(nameof(MarkForeground));

    public static readonly StyledProperty<bool> MarkEnabledProperty =
        AvaloniaProperty.Register<MarkTextBlock, bool>(nameof(MarkEnabled), true);

    static MarkTextBlock()
    {
        AffectsRender<MarkTextBlock>(
            SourceTextProperty, TermsProperty, MarkBackgroundProperty,
            MarkForegroundProperty, MarkEnabledProperty);
        SourceTextProperty.Changed.AddClassHandler<MarkTextBlock>((c, _) => c.Rebuild());
        TermsProperty.Changed.AddClassHandler<MarkTextBlock>((c, _) => c.Rebuild());
        MarkEnabledProperty.Changed.AddClassHandler<MarkTextBlock>((c, _) => c.Rebuild());
    }

    public MarkTextBlock()
    {
        // 兜底：控件自身也可能被直接赋值 Text，同步回 SourceText
        TextProperty.Changed.AddClassHandler<MarkTextBlock>((c, e) =>
        {
            if (c._rebuilding) return;
            if (e.NewValue is string s) c.SourceText = s;
        });
    }

    private bool _rebuilding;

    public string? SourceText
    {
        get => GetValue(SourceTextProperty);
        set => SetValue(SourceTextProperty, value);
    }

    public IReadOnlyList<string>? Terms
    {
        get => GetValue(TermsProperty);
        set => SetValue(TermsProperty, value);
    }

    public IBrush? MarkBackground
    {
        get => GetValue(MarkBackgroundProperty);
        set => SetValue(MarkBackgroundProperty, value);
    }

    public IBrush? MarkForeground
    {
        get => GetValue(MarkForegroundProperty);
        set => SetValue(MarkForegroundProperty, value);
    }

    public bool MarkEnabled
    {
        get => GetValue(MarkEnabledProperty);
        set => SetValue(MarkEnabledProperty, value);
    }

    private void Rebuild()
    {
        _rebuilding = true;
        try
        {
            var text = SourceText;
            Inlines?.Clear();
            if (string.IsNullOrEmpty(text)) return;

            var terms = Terms;
            if (!MarkEnabled || terms is null || terms.Count == 0)
            {
                Inlines?.Add(new Run(text));
                return;
            }

            // 收集所有命中区间后合并，避免重叠区间产生嵌套 Run
            var spans = new List<(int Start, int End)>();
            foreach (var term in terms)
            {
                if (string.IsNullOrEmpty(term)) continue;
                var i = 0;
                while (i < text.Length)
                {
                    var idx = text.IndexOf(term, i, StringComparison.OrdinalIgnoreCase);
                    if (idx < 0) break;
                    spans.Add((idx, idx + term.Length));
                    i = idx + Math.Max(1, term.Length);
                }
            }
            if (spans.Count == 0)
            {
                Inlines?.Add(new Run(text));
                return;
            }

            spans.Sort((a, b) => a.Start.CompareTo(b.Start));
            var pos = 0;
            foreach (var (start, end) in spans)
            {
                if (start < pos) continue;
                if (start > pos) Inlines?.Add(new Run(text[pos..start]));
                var run = new Run(text[start..end]);
                if (MarkBackground is not null) run.Background = MarkBackground;
                if (MarkForeground is not null) run.Foreground = MarkForeground;
                Inlines?.Add(run);
                pos = end;
            }
            if (pos < text.Length) Inlines?.Add(new Run(text[pos..]));
        }
        finally
        {
            _rebuilding = false;
        }
    }
}
