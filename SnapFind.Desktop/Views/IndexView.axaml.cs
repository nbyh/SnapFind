using Avalonia;
using Avalonia.Controls;
using Avalonia.Interactivity;
using Avalonia.Platform.Storage;
using SnapFind.Desktop.ViewModels;

namespace SnapFind.Desktop.Views;

public partial class IndexView : UserControl
{
    /// <summary>当前待确认的批量操作（用户点「确定执行」后运行）。</summary>
    private Func<Task>? _pendingConfirm;

    public IndexView() => InitializeComponent();

    private IndexViewModel? Vm => DataContext as IndexViewModel;

    protected override void OnDataContextChanged(EventArgs e)
    {
        base.OnDataContextChanged(e);
        if (Vm is not null) Vm.ConfirmRequested += ShowBulkConfirm;
    }

    private void ShowBulkConfirm(string title, string message, Func<Task> onConfirm)
    {
        ConfirmTitle.Text = title;
        ConfirmMessage.Text = message;
        _pendingConfirm = onConfirm;
        BulkConfirm.IsVisible = true;
        ConfirmCancelBtn.Focus();
    }

    private void OnBulkConfirmCancel(object? sender, RoutedEventArgs e)
    {
        BulkConfirm.IsVisible = false;
        _pendingConfirm = null;
    }

    private async void OnBulkConfirmOk(object? sender, RoutedEventArgs e)
    {
        BulkConfirm.IsVisible = false;
        var action = _pendingConfirm;
        _pendingConfirm = null;
        if (action is not null) await action();
    }

    private void OnVolumeChecked(object? sender, RoutedEventArgs e) => Vm?.NotifySelectionChanged();

    private async void OnBrowseDirectory(object? sender, RoutedEventArgs e)
    {
        if (TopLevel.GetTopLevel(this)?.StorageProvider is not { CanPickFolder: true } provider) return;

        IReadOnlyList<IStorageFolder> picked;
        try
        {
            picked = await provider.OpenFolderPickerAsync(new FolderPickerOpenOptions
            {
                Title = "选择要索引的目录",
                AllowMultiple = false,
            });
        }
        catch
        {
            return; // 用户取消或平台不支持
        }

        if (picked.Count == 0 || Vm is null) return;
        var local = picked[0].TryGetLocalPath();
        Vm.ScopeDirEdit = string.IsNullOrEmpty(local) ? picked[0].Path.LocalPath : local;
    }

    private void OnVolumeAction(object? sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string action, DataContext: ProgressRow row })
        {
            Vm?.VolumeActionCommand.Execute($"{action}|{row.VolumeId}");
        }
    }

    private void OnVolumeRemove(object? sender, RoutedEventArgs e)
    {
        if (sender is Button { DataContext: VolumeRow row })
        {
            Vm?.VolumeActionCommand.Execute($"unregister|{row.VolumeId}");
        }
    }
}
