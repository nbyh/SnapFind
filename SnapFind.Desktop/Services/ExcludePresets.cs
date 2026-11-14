using CommunityToolkit.Mvvm.ComponentModel;

namespace SnapFind.Desktop.Services;

/// <summary>引导页/设置页共用的「常用排除项」勾选框模型：勾选 = 把该组规则写入排除规则。</summary>
public sealed partial class ExcludePresetOption : ObservableObject
{
    public string Label { get; init; } = "";
    public string[] Rules { get; init; } = [];

    [ObservableProperty] private bool _isChecked;
}

/// <summary>
/// 常用排除预设：按当前平台给出一组 glob 规则，供首次引导与设置页共用。
///
/// 注意：Rust 内置默认（platform::default_excludes）已自动覆盖大部分常用项
/// （Linux：/proc、/sys、node_modules、.cache、*.iso 等；Windows：C:\Windows、
/// C:\$Recycle.Bin、AppData\Temp、node_modules、*.vhd 等），且不占用户配置。
/// 这里只列**内置没有的增量项**，避免引导页出现一排勾了也没效果的重复规则。
/// 规则文本与勾选状态的双向同步也在这里。
/// </summary>
public static class ExcludePresets
{
    /// <summary>
    /// Rust 内置默认排除（platform::default_excludes）的镜像，按平台区分。
    /// 用于启动时从用户规则里去掉与内置重复的项（内置扫描时自动叠加，
    /// 写进配置纯属冗余）。镜像若滞后于 Rust 侧，最多留下无害的旧条目。
    /// </summary>
    public static readonly HashSet<string> BuiltInDefaults =
        new(OperatingSystem.IsWindows()
            ? new[]
            {
                @"C:\Windows\**", @"C:\$Recycle.Bin\**", @"C:\System Volume Information\**",
                @"C:\Recovery\**", @"**\AppData\Local\Temp\**", @"**\AppData\Local\CrashDumps\**",
                @"**\node_modules\**", @"**\.git\objects\**", @"**\target\debug\**",
                @"**\target\release\**", @"**\.venv\**", @"**\__pycache__\**",
                @"**\.cargo\registry\**", @"**\.nuget\packages\**", @"**\.gradle\caches\**",
                @"**\*.vhd", @"**\*.vhdx", @"**\*.iso", @"**\pagefile.sys", @"**\hiberfil.sys",
                @"**\swapfile.sys",
            }
            : new[]
            {
                "/proc/**", "/sys/**", "/dev/**", "/run/**", "/tmp/**", "/var/cache/**",
                "/var/lib/docker/**", "/usr/lib/debug/**", "**/node_modules/**",
                "**/.git/objects/**", "**/target/debug/**", "**/target/release/**",
                "**/.venv/**", "**/__pycache__/**", "**/.cargo/registry/**", "**/.cache/**",
                "**/*.qcow2", "**/*.iso",
            }, StringComparer.Ordinal);

    /// <summary>创建当前平台的预设组（顺序：系统目录、开发缓存、磁盘镜像）。</summary>
    public static List<ExcludePresetOption> Create()
    {
        if (OperatingSystem.IsWindows())
        {
            return
            [
                // 内置默认只覆盖 C:\ 的回收站/卷信息；这里补任意盘符形态
                new()
                {
                    Label = "系统目录（所有盘符）",
                    Rules =
                    [
                        @"**\$Recycle.Bin\**", @"**\System Volume Information\**",
                        @"**\RECYCLER\**",
                    ],
                },
                new()
                {
                    Label = "开发缓存",
                    Rules =
                    [
                        @"**\.svn\**", @"**\.hg\**", @"**\.pytest_cache\**",
                        @"**\.mypy_cache\**", @"**\.m2\repository\**", @"**\obj\**",
                    ],
                },
                new()
                {
                    Label = "磁盘镜像",
                    Rules = [@"**\*.vmdk"],
                },
            ];
        }

        return
        [
            new()
            {
                Label = "系统目录",
                Rules =
                [
                    "/var/tmp/**", "/var/log/**", "/var/lib/flatpak/**", "/var/lib/snapd/**",
                ],
            },
            new()
            {
                Label = "开发缓存",
                Rules =
                [
                    "**/.svn/**", "**/.hg/**", "**/.pytest_cache/**", "**/.mypy_cache/**",
                    "**/.gradle/caches/**", "**/.m2/repository/**", "**/.npm/**", "**/obj/**",
                ],
            },
            new()
            {
                Label = "磁盘镜像",
                Rules = ["**/*.vmdk"],
            },
        ];
    }

    /// <summary>把规则文本解析成去重后的行列表。</summary>
    public static List<string> ParseRules(string? text)
        => (text ?? "").Split('\n', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
            .Distinct(StringComparer.Ordinal)
            .ToList();

    /// <summary>按规则文本回显勾选状态（某组规则全部命中才勾选）。</summary>
    public static void SyncChecked(IEnumerable<ExcludePresetOption> presets, string? text)
    {
        var lines = ParseRules(text);
        foreach (var p in presets)
            p.IsChecked = p.Rules.All(lines.Contains);
    }

    /// <summary>勾选/取消一组预设：勾选 = 追加缺失规则；取消 = 从文本中移除。返回新文本。</summary>
    public static string Toggle(ExcludePresetOption option, string text)
    {
        var lines = ParseRules(text);
        foreach (var rule in option.Rules)
        {
            if (option.IsChecked)
            {
                if (!lines.Contains(rule)) lines.Add(rule);
            }
            else
            {
                lines.RemoveAll(r => r == rule);
            }
        }
        return string.Join("\n", lines);
    }
}
