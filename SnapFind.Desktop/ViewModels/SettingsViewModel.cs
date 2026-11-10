using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using SnapFind.Desktop.Ipc;
using SnapFind.Desktop.Services;

namespace SnapFind.Desktop.ViewModels;

/// <summary>设置页。改动写入与 Rust 共用的 config.json。</summary>
public sealed partial class SettingsViewModel : ObservableObject
{
    private readonly ShellViewModel _shell;
    private bool _loading;

    public SettingsViewModel(ShellViewModel shell)
    {
        _shell = shell;
    }

    public bool TraySupported => PlatformService.IsWindows;
    public bool AutostartSupported => AutostartService.Supported;
    public string ConfigPath => _shell.Config.Path;
    public string VersionText => $"SnapFind v{PlatformService.AppVersion} · 本地全文搜索 · 不联网、不上传、无云端校验";

    public ObservableCollection<int> ParallelOptions { get; } = new() { 0, 1, 2, 4, 8 };
    /// <summary>IO 限速档位（index.throttle）的中文选项，与 Format.ThrottleText 的档位一一对应。</summary>
    public ObservableCollection<string> ThrottleOptions { get; } = new() { "低", "中", "高", "关闭" };

    [ObservableProperty] private string _theme = "system";

    [ObservableProperty] private bool _dailyScan;
    [ObservableProperty] private string _dailyScanAt = "03:00";
    [ObservableProperty] private string _dailyScanAtError = "";

    [ObservableProperty] private bool _resumeOnStart = true;
    [ObservableProperty] private string _indexRootEdit = "";
    [ObservableProperty] private int _parallelVolumes;

    [ObservableProperty] private string _ioThrottleText = "低";
    [ObservableProperty] private string _memoryBudgetText = "1024";

    [ObservableProperty] private string _queryBudgetText = "10";
    [ObservableProperty] private string _queryBudgetError = "";

    [ObservableProperty] private string _excludePathsText = "";
    [ObservableProperty] private string _excludeTypesText = "";
    [ObservableProperty] private string _excludeRulesText = "";

    [ObservableProperty] private bool _closeToTray = true;
    [ObservableProperty] private bool _autostart;

    [ObservableProperty] private string _aboutLine = "";

    public void Load()
    {
        _loading = true;
        try
        {
            var cfg = _shell.Config;
            Theme = cfg.Theme;
            DailyScan = cfg.DailyScan;
            DailyScanAt = cfg.DailyScanAt;
            ResumeOnStart = cfg.ResumeOnStart;
            IndexRootEdit = cfg.IndexRoot;
            ParallelVolumes = cfg.ParallelVolumes;
            IoThrottleText = TextFromThrottle(cfg.IoThrottle);
            MemoryBudgetText = cfg.MemoryBudgetMb.ToString();
            QueryBudgetText = (cfg.QueryTimeoutMs / 1000).ToString();
            ExcludePathsText = string.Join("\n", cfg.ExcludePaths);
            ExcludeTypesText = string.Join(", ", cfg.ExcludeTypes);
            ExcludeRulesText = string.Join("\n", cfg.ExcludeRules);
            CloseToTray = cfg.CloseToTray;
            Autostart = AutostartService.IsEnabled();

            AboutLine = TraySupported
                ? "关闭窗口仅隐藏到托盘；从托盘「退出」才会关闭索引进程（保存至最近检查点）。"
                : "关闭本窗口即关闭索引进程：先保存至最近检查点再退出，不会驻留后台。";
        }
        finally
        {
            _loading = false;
        }
    }

    public bool IsSystemTheme
    {
        get => Theme == "system";
        set { if (value) Theme = "system"; }
    }

    public bool IsLightTheme
    {
        get => Theme == "light";
        set { if (value) Theme = "light"; }
    }

    public bool IsDarkTheme
    {
        get => Theme == "dark";
        set { if (value) Theme = "dark"; }
    }

    partial void OnThemeChanged(string value)
    {
        OnPropertyChanged(nameof(IsSystemTheme));
        OnPropertyChanged(nameof(IsLightTheme));
        OnPropertyChanged(nameof(IsDarkTheme));
        if (_loading) return;
        _shell.Config.Theme = value;
        _shell.TrySaveConfig();
        _shell.ApplyTheme(value);
    }

    partial void OnDailyScanChanged(bool value)
    {
        if (_loading) return;
        _shell.Config.DailyScan = value;
        _shell.TrySaveConfig();
    }

    partial void OnResumeOnStartChanged(bool value)
    {
        if (_loading) return;
        _shell.Config.ResumeOnStart = value;
        _shell.TrySaveConfig();
    }

    partial void OnParallelVolumesChanged(int value)
    {
        if (_loading) return;
        _shell.Config.ParallelVolumes = value;
        if (!_shell.TrySaveConfig()) return;
        _shell.SetNotice("同时索引的卷数已保存，重启索引服务后生效（维护 → 重启索引服务）。");
    }

    partial void OnIoThrottleTextChanged(string value)
    {
        if (_loading) return;
        _shell.Config.IoThrottle = ThrottleFromText(value);
        if (_shell.TrySaveConfig())
        {
            _shell.SetNotice("IO 限速档位已保存，重启索引服务后生效（维护 → 重启索引服务）。");
        }
    }

    partial void OnCloseToTrayChanged(bool value)
    {
        if (_loading) return;
        _shell.Config.CloseToTray = value;
        // 与 tray_enabled 合并：避免「配置里 tray_enabled=true、关窗却直接退出」的
        // 歧义——托盘图标只在「关窗到托盘」开启时创建。图标在启动时创建，
        // 因此此项改动需重启应用才会出现/消失。
        _shell.Config.TrayEnabled = value;
        _shell.TrySaveConfig();
    }

    partial void OnAutostartChanged(bool value)
    {
        if (_loading) return;
        if (AutostartService.Set(value))
        {
            _shell.Config.Autostart = value;
            _shell.TrySaveConfig();
        }
        else
        {
            _shell.SetNotice("写入启动项失败，已保持原状态。");
        }
    }

    [RelayCommand]
    private void SaveDailyScanAt()
    {
        var v = (DailyScanAt ?? "").Trim();
        if (TimeOnly.TryParse(v, out var t))
        {
            DailyScanAtError = "";
            DailyScanAt = t.ToString("HH:mm");
            _shell.Config.DailyScanAt = DailyScanAt;
            _shell.TrySaveConfig();
        }
        else
        {
            DailyScanAtError = $"时间格式应为 HH:MM（当前：{v}）";
        }
    }

    [RelayCommand]
    private void SaveIndexRoot()
    {
        var root = (IndexRootEdit ?? "").Trim();
        if (root.Length > 0)
        {
            // 保存前先探测可写性：以前写进配置后才发现不可写，索引启动时静默降级，
            // 设置页显示用户填的路径、索引页 footer 显示真实路径，两处不一致
            try
            {
                Directory.CreateDirectory(root);
                var probe = Path.Combine(root, ".write-probe");
                File.WriteAllText(probe, "");
                File.Delete(probe);
            }
            catch (Exception ex)
            {
                _shell.SetNotice($"索引目录不可写（{ex.Message}），未保存。");
                return;
            }
        }
        var oldRoot = _shell.Config.IndexRoot;
        _shell.Config.IndexRoot = root;
        if (!_shell.TrySaveConfig()) return;
        _shell.SetNotice("索引目录已保存，重启索引服务后生效。");
        // 善后：旧目录的索引数据不会自动迁移，明确告诉用户，避免白占盘还以为索引在
        if (!string.IsNullOrEmpty(oldRoot)
            && !string.Equals(oldRoot, root, StringComparison.OrdinalIgnoreCase)
            && Directory.Exists(oldRoot))
        {
            _shell.SetNotice(
                $"索引目录已保存，重启索引服务后生效。注意：旧目录 {oldRoot} 的索引数据不会自动迁移；确认新目录工作正常后可手动删除它以释放空间。");
        }
    }

    [RelayCommand]
    private void SaveMemoryBudget()
    {
        if (!int.TryParse((MemoryBudgetText ?? "").Trim(), out var mb))
        {
            _shell.SetNotice("内存预算应为数字（MB，256–4096）。");
            return;
        }
        mb = Math.Clamp(mb, 256, 4096);
        MemoryBudgetText = mb.ToString();
        _shell.Config.MemoryBudgetMb = mb;
        if (_shell.TrySaveConfig())
        {
            _shell.SetNotice("内存预算已保存，重启索引服务后生效。");
        }
    }

    [RelayCommand]
    private void SaveQueryBudget()
    {
        if (!int.TryParse((QueryBudgetText ?? "").Trim(), out var sec) || sec < 1 || sec > 60)
        {
            QueryBudgetError = "应为 1–60 的整数（秒）";
            return;
        }
        QueryBudgetError = "";
        _shell.Config.QueryTimeoutMs = sec * 1000;
        if (!_shell.TrySaveConfig()) return;
        // 搜索页的提醒阈值与超时看门狗按新值即时生效，无需重启
        _shell.Search.UpdateQueryBudget(sec * 1000);
        _shell.SetNotice("查询时间预算已保存并即时生效。");
    }

    /// <summary>
    /// 优雅重启索引进程：indexd 收到 shutdown 后落盘检查点再退出，
    /// 桌面端断线自愈会自动拉起新进程（新进程读取 config.json），
    /// 所有「需重启」的设置随之生效，无需退出整个应用。
    /// </summary>
    [RelayCommand]
    private async Task RestartIndexServiceAsync()
    {
        try
        {
            await _shell.RequestAsync<object>(Msg.Shutdown,
                new { reason = "settings_restart", timeout_ms = 5000 });
            _shell.SetNotice("索引服务正在重启，「需重启」的设置即将生效…");
        }
        catch
        {
            // 请求失败多半因为管道已断（正是想要的结果）——交给断线自愈重启
            _shell.SetNotice("已请求重启索引服务，正在重连…");
            _ = _shell.ReconnectAsync();
        }
    }

    private static string TextFromThrottle(string? t) => (t ?? "").ToLowerInvariant() switch
    {
        "medium" => "中",
        "high" => "高",
        "off" => "关闭",
        _ => "低",
    };

    private static string ThrottleFromText(string? text) => text switch
    {
        "中" => "medium",
        "高" => "high",
        "关闭" => "off",
        _ => "low",
    };

    [RelayCommand]
    private async Task SaveExcludesAsync()
    {
        var paths = (ExcludePathsText ?? "")
            .Split('\n', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
            .ToList();
        var types = (ExcludeTypesText ?? "")
            .Split(new[] { ',', ';', '\n', '，', '；' }, StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
            .Select(t => t.TrimStart('.'))
            .Where(t => t.Length > 0)
            .ToList();
        var rules = (ExcludeRulesText ?? "")
            .Split('\n', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
            .ToList();

        _shell.Config.ExcludePaths = paths;
        _shell.Config.ExcludeTypes = types;
        _shell.Config.ExcludeRules = rules;
        if (!_shell.TrySaveConfig()) return;

        // 同步进 indexd 的内存配置：exclude_rules 只在进程启动时加载一次，
        // 不通知的话磁盘已改、内存仍旧，直到重启才生效。
        try
        {
            await _shell.RequestAsync<VolumeActionAck>(Msg.VolumeAction,
                new VolumeAction { Action = "setExcludes", Excludes = rules });
            _shell.SetNotice("排除规则已保存，下一次扫描或增量索引即生效。");
        }
        catch
        {
            _shell.SetNotice("排除规则已保存；索引进程未响应，重启 SnapFind 后完全生效。");
        }
    }

    [RelayCommand]
    private async Task RestoreDefaultExcludesAsync()
    {
        _shell.Config.ExcludePaths = new List<string> { "$Recycle.Bin", "RECYCLER", "System Volume Information" };
        _shell.Config.ExcludeTypes = new List<string>();
        _shell.Config.ExcludeRules = new List<string>();
        if (!_shell.TrySaveConfig()) return;
        Load();

        // 自定义 glob 规则只存在于索引进程内存里：只清磁盘不同步的话，
        // 用户会看到输入框已清空、但那些规则仍在排除文件，直到重启才失效。
        try
        {
            await _shell.RequestAsync<VolumeActionAck>(Msg.VolumeAction,
                new VolumeAction { Action = "setExcludes", Excludes = new List<string>() });
            _shell.SetNotice("已恢复默认排除规则（回收站、系统卷信息、索引目录由内置规则自动排除）。");
        }
        catch
        {
            _shell.SetNotice("已恢复默认排除规则；索引进程未响应，重启 SnapFind 后完全生效。");
        }
    }

    [RelayCommand]
    private void HideToTray() => _shell.HideToTray();

    [RelayCommand]
    private async Task CompactAsync()
    {
        try
        {
            await _shell.RequestAsync<object>(Msg.VolumeAction,
                new VolumeAction { Action = "compact", VolumeId = null });
            _shell.SetNotice("已请求整理分段。");
        }
        catch (Exception ex)
        {
            _shell.SetNotice($"操作失败：{ex.Message}");
        }
    }
}
