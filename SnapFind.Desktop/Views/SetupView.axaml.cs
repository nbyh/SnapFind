using Avalonia.Controls;
using Avalonia.Interactivity;
using SnapFind.Desktop.ViewModels;

namespace SnapFind.Desktop.Views;

public partial class SetupView : UserControl
{
    public SetupView() => InitializeComponent();

    private void OnVolumeChecked(object? sender, RoutedEventArgs e)
        => (DataContext as SetupViewModel)?.UpdateSummary();
}
