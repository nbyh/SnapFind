using Avalonia.Media;
using SnapFind.Desktop.Helpers;
using SnapFind.Desktop.Ipc;

namespace SnapFind.Desktop.ViewModels;

/// <summary>结果行视图模型：把 IPC 的 <see cref="Hit"/> 转成可直接绑定的显示字段。</summary>
public sealed class HitItem
{
    public HitItem(Hit hit, int arrivalSeq, ResultLayout layout)
    {
        Hit = hit;
        ArrivalSeq = arrivalSeq;
        Layout = layout;
        ExtColor = new SolidColorBrush(Format.ExtColor(hit.Ext));
        var tinted = Format.ExtColor(hit.Ext);
        ExtTint = new SolidColorBrush(Color.FromArgb(46, tinted.R, tinted.G, tinted.B));
    }

    public Hit Hit { get; }
    public int ArrivalSeq { get; }

    /// <summary>与表头共享的列宽（供行模板绑定）。</summary>
    public ResultLayout Layout { get; }

    public string Path => Hit.Path;
    public string Name => Hit.Name;
    public string Ext => Hit.Ext;
    public string ExtBadge => Format.ExtBadge(Hit.Ext);
    public string SizeText => Format.Human(Hit.Size);
    public string MtimeText => Format.TimeShort(Hit.Mtime);
    public IBrush ExtColor { get; }
    public IBrush ExtTint { get; }

    public bool NameOnly => !Format.ContentReadable(Hit.Ext);
    public bool Encrypted => Hit.Flags.Encrypted;

    /// <summary>命中字段角标，例如「内容 文件名」。</summary>
    public string FieldBadges => string.Join("  ",
        Hit.MatchedFields.Select(Format.FieldLabel).Distinct());

    public bool HasBadges => FieldBadges.Length > 0;

    public int Seq { get; set; }

    public IReadOnlyList<Snippet> Snippets => Hit.Snippets;
}
