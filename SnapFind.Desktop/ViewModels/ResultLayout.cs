using CommunityToolkit.Mvvm.ComponentModel;

namespace SnapFind.Desktop.ViewModels;

/// <summary>
/// 结果表各列的像素宽度（用户拖动列边界调整）。
///
/// 表头与每一行共享**同一个实例**：行模板直接绑定 <c>Layout.SizeWidth</c>，
/// 因此调整一次即可让所有已实现行同步（虚拟化列表只有屏幕内那几十行）。
/// </summary>
public sealed partial class ResultLayout : ObservableObject
{
    /// <summary>「大小」列宽度（含右侧留白）。</summary>
    [ObservableProperty] private double _sizeWidth = 96;

    /// <summary>「修改时间」列宽度。</summary>
    [ObservableProperty] private double _mtimeWidth = 132;

    public const double MinColumn = 58;
    public const double MaxColumn = 360;
}
