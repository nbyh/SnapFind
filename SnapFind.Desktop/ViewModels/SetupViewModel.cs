using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using SnapFind.Desktop.Helpers;
using SnapFind.Desktop.Ipc;
using SnapFind.Desktop.Services;

namespace SnapFind.Desktop.ViewModels;

public sealed partial class SetupVolumeRow : ObservableObject
{
    [ObservableProperty] private bool _selected;

    public string VolumeId { get; init; } = "";
    public string Mount { get; init; } = "";
    public string MountSuffix { get; init; } = "";
    public string Fs { get; init; } = "";
    public string TotalText { get; init; } = "";
    public string FreeText { get; init; } = "";
    public string EstimateText { get; init; } = "";
    public bool Selectable { get; init; } = true;
    public ulong EstimatedFiles { get; init; }
}

/// <summary>首次向导：选择范围、确认索引目录、排除规则、开始索引。</summary>
public sealed partial class SetupViewModel : ObservableObject
{
    private readonly ShellViewModel _shell;
    private bool _excludePresetsInitialized;
    private bool _syncingPresets;

    public SetupViewModel(ShellViewModel shell) => _shell = shell;

    public ObservableCollection<SetupVolumeRow> Rows { get; } = new();

    /// <summary>常用排除预设（按当前操作系统给出；勾选状态与下方规则文本双向同步）。</summary>
    public ObservableCollection<ExcludePresetOption> PresetOptions { get; } = new();

    [ObservableProperty] private string _excludesText = "";
    [ObservableProperty] private bool _busy;
    [ObservableProperty] private string _summary = "已选 0 个卷 · 预计约 0 个文件";
    [ObservableProperty] private string _degradedReason = "";
    [ObservableProperty] private bool _hasDegraded;
    [ObservableProperty] private string _indexRoot = "";

    partial void OnExcludesTextChanged(string value)
    {
        // 用户手改文本后重算勾选状态；_syncingPresets 防止回写循环
        if (_syncingPresets) return;
        ExcludePresets.SyncChecked(PresetOptions, value);
    }

    public void ApplyScope(ScopePreview? scope)
    {
        Rows.Clear();
        if (scope is null)
        {
            IndexRoot = "";
            HasDegraded = false;
            return;
        }

        IndexRoot = scope.IndexRoot;
        DegradedReason = scope.DegradedReason ?? "";
        HasDegraded = !string.IsNullOrEmpty(DegradedReason);
        // 只展示/编辑「用户自定义规则」，与设置页的排除规则同一数据源
        // （config.json 的 index.exclude_rules）。scope.Excludes 还混着内置默认项
        // 与索引目录规则——那些是扫描时自动叠加的，不存配置，放进来会让用户
        // 保存出一堆「看起来是我配的」默认项，且和设置页对不上。
        var rules = _shell.Config.ExcludeRules;
        InitExcludePresets(rules);
        ExcludesText = string.Join("\n", rules);

        foreach (var v in scope.Volumes)
        {
            Rows.Add(new SetupVolumeRow
            {
                VolumeId = v.VolumeId,
                Mount = v.Mount,
                MountSuffix = v.Network ? "（网络卷，默认不索引）" : v.Removable ? "（可移动介质）" : "",
                Fs = v.Fs,
                TotalText = Format.Human(v.TotalBytes),
                FreeText = Format.Human(v.FreeBytes),
                EstimateText = $"≈ {v.EstimatedFiles}",
                Selectable = !v.Network,
                EstimatedFiles = v.EstimatedFiles,
                // 默认勾选本地固定盘（与 Rust 版一致）
                Selected = v.Enabled && !v.Network && !v.Removable,
            });
        }
        UpdateSummary();
    }

    public void UpdateSummary()
    {
        var picked = Rows.Where(r => r.Selected).ToList();
        var files = picked.Aggregate(0UL, (acc, r) => acc + r.EstimatedFiles);
        Summary = $"已选 {picked.Count} 个卷 · 预计约 {files} 个文件";
    }

    [RelayCommand]
    private async Task StartAsync()
    {
        var volumes = Rows.Where(r => r.Selected).Select(r => r.VolumeId).ToList();
        if (volumes.Count == 0)
        {
            _shell.SetNotice("请至少选择一个卷或目录");
            return;
        }

        // 引导页编辑的规则持久化到 config.json（与设置页同一字段），并同步进
        // indexd 内存——之前只随 INDEX_START 传运行时参数，重启即丢、设置页也看不到
        var excludes = (ExcludesText ?? "")
            .Split('\n', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
            .ToList();
        _shell.Config.ExcludeRules = excludes;
        // 保存失败就别往下走：否则索引照建、规则却没落盘，用户以为规则已生效
        if (!_shell.TrySaveConfig()) return;

        Busy = true;
        _shell.EnterMain();
        _shell.ShowIndexView();

        try
        {
            try
            {
                await _shell.RequestAsync<VolumeActionAck>(Msg.VolumeAction,
                    new VolumeAction { Action = "setExcludes", Excludes = excludes });
            }
            catch
            {
                // 索引进程暂时没响应也没关系：磁盘已保存，扫描构建排除集时
                // 路径/类型从磁盘重读，规则重启后生效
            }

            await _shell.RequestAsync<IndexStartAck>(Msg.IndexStart,
                new IndexStart { Volumes = volumes });
            _shell.SetNotice("已开始建立索引，可在“索引”页查看进度。");
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
    private void Skip() => _shell.EnterMain();

    // ---- 常用排除预设 ------------------------------------------------------

    /// <summary>
    /// 初始化常用排除预设勾选框。首次使用（用户还没有任何自定义规则）时，
    /// 预填当前平台的常用排除项并全部勾选，让「无关文件不进索引」开箱即得；
    /// 已有规则的用户则按规则文本回显勾选状态。只初始化一次。
    /// </summary>
    private void InitExcludePresets(IReadOnlyList<string> rules)
    {
        if (_excludePresetsInitialized) return;
        _excludePresetsInitialized = true;

        foreach (var p in ExcludePresets.Create()) PresetOptions.Add(p);

        var lines = ExcludePresets.ParseRules(string.Join("\n", rules));
        if (lines.Count == 0)
        {
            // 首次使用：预填全部预设并勾选
            var all = PresetOptions.SelectMany(p => p.Rules).ToList();
            foreach (var p in PresetOptions) p.IsChecked = true;
            _syncingPresets = true;
            ExcludesText = string.Join("\n", all);
            _syncingPresets = false;
        }
        else
        {
            ExcludePresets.SyncChecked(PresetOptions, ExcludesText);
        }
    }

    /// <summary>勾选/取消一组预设：勾选 = 追加缺失规则；取消 = 从规则文本中移除。</summary>
    [RelayCommand]
    private void TogglePreset(ExcludePresetOption option)
    {
        _syncingPresets = true;
        ExcludesText = ExcludePresets.Toggle(option, ExcludesText);
        _syncingPresets = false;
    }
}
