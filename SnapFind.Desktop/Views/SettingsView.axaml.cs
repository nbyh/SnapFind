using Avalonia.Controls;
using Avalonia.Interactivity;
using SnapFind.Desktop.ViewModels;

namespace SnapFind.Desktop.Views;

public partial class SettingsView : UserControl
{
    public SettingsView() => InitializeComponent();

    private void OnDailyScanAtLostFocus(object? sender, RoutedEventArgs e)
        => (DataContext as SettingsViewModel)?.SaveDailyScanAtCommand.Execute(null);

    private void OnMemoryBudgetLostFocus(object? sender, RoutedEventArgs e)
        => (DataContext as SettingsViewModel)?.SaveMemoryBudgetCommand.Execute(null);

    private void OnQueryBudgetLostFocus(object? sender, RoutedEventArgs e)
        => (DataContext as SettingsViewModel)?.SaveQueryBudgetCommand.Execute(null);
}
