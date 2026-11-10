using System.Text.Json;
using System.Text.Json.Nodes;

namespace SnapFind.Desktop.Services;

/// <summary>
/// 与 Rust 侧共享的 <c>config.json</c>。
///
/// 关键约束：**必须保留不认识的字段**。Rust 的结构体全部带 <c>serde(default)</c>，
/// 如果这里整体反序列化成强类型再写回，手改过的 <c>extract.* / query.* / lifecycle.*</c>
/// 会被抹成默认值。因此这里直接操作 <see cref="JsonNode"/>，只改已知节点。
/// </summary>
public sealed class AppConfig
{
    /// <summary>配置结构版本，必须与 Rust 侧 <c>config::CONFIG_VERSION</c> 保持一致。</summary>
    private const int CurrentVersion = 2;
    private JsonObject _root;

    public string Path { get; }

    private AppConfig(string path, JsonObject root)
    {
        Path = path;
        _root = root;
    }

    // ---- 位置 ------------------------------------------------------------

    /// <summary>便携优先：可写则用 exe 目录，否则落到用户配置目录。
    /// 环境变量 <c>SNAPFIND_CONFIG</c>（完整文件路径）优先——供测试隔离与调试用。</summary>
    public static string ResolvePath()
    {
        var forced = Environment.GetEnvironmentVariable("SNAPFIND_CONFIG");
        if (!string.IsNullOrEmpty(forced)) return forced;
        var exeDir = AppContext.BaseDirectory;
        var portable = System.IO.Path.Combine(exeDir, "config.json");
        if (CanWrite(exeDir)) return portable;

        var appData = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
        return System.IO.Path.Combine(appData, "SnapFind", "config.json");
    }

    private static bool CanWrite(string dir)
    {
        try
        {
            var probe = System.IO.Path.Combine(dir, ".write-probe");
            using (File.Create(probe, 1, FileOptions.DeleteOnClose)) { }
            return true;
        }
        catch { return false; }
    }

    public static AppConfig Load() => Load(out _);

    /// <summary>
    /// 读取配置。<paramref name="warning"/> 非空表示配置被重置或读取失败——
    /// 调用方应提示用户，否则「设置突然全没了」没有任何线索。
    /// </summary>
    public static AppConfig Load(out string? warning)
    {
        warning = null;
        var path = ResolvePath();
        JsonObject root;
        try
        {
            if (File.Exists(path))
            {
                var text = File.ReadAllText(path);
                root = JsonNode.Parse(text) as JsonObject ?? new JsonObject();
            }
            else
            {
                root = new JsonObject();
            }
        }
        catch (Exception ex)
        {
            // 解析失败：备份后回默认值（与 Rust 侧行为一致），并告知用户
            try { File.Move(path, path + ".bak", overwrite: true); } catch { /* 备份失败也只能继续 */ }
            warning = $"配置文件损坏（{ex.Message}），已备份为 config.json.bak 并恢复默认设置。";
            root = new JsonObject();
        }
        return new AppConfig(path, root);
    }

    // ---- 原子写入 --------------------------------------------------------

    public void Save()
    {
        // 写当前版本号（不再用 ??=）：旧配置只有桌面端保存时才真正升级到 v2
        _root["version"] = CurrentVersion;
        var dir = System.IO.Path.GetDirectoryName(Path);
        if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);

        var tmp = Path + ".tmp";
        var json = _root.ToJsonString(new JsonSerializerOptions { WriteIndented = true });
        using (var fs = new FileStream(tmp, FileMode.Create, FileAccess.Write, FileShare.None))
        using (var w = new StreamWriter(fs))
        {
            w.Write(json);
            w.Flush();
            fs.Flush(flushToDisk: true);
        }
        File.Move(tmp, Path, overwrite: true);
    }

    // ---- 节点访问 --------------------------------------------------------

    private JsonObject Section(string name)
    {
        if (_root[name] is not JsonObject o)
        {
            o = new JsonObject();
            _root[name] = o;
        }
        return o;
    }

    private string GetString(string section, string key, string fallback)
    {
        var s = Section(section);
        return s[key] is JsonValue v && v.TryGetValue<string>(out var str) ? str : fallback;
    }

    private void Set(string section, string key, JsonNode? value) => Section(section)[key] = value;

    private bool GetBool(string section, string key, bool fallback)
    {
        var s = Section(section);
        return s[key] is JsonValue v && v.TryGetValue<bool>(out var b) ? b : fallback;
    }

    private int GetInt(string section, string key, int fallback)
    {
        var s = Section(section);
        return s[key] is JsonValue v && v.TryGetValue<int>(out var i) ? i : fallback;
    }

    private List<string> GetStringList(string section, string key, IEnumerable<string> fallback)
    {
        if (Section(section)[key] is JsonArray arr)
        {
            return arr.OfType<JsonValue>()
                .Select(v => v.TryGetValue<string>(out var s) ? s : null)
                .Where(s => s is not null)
                .Select(s => s!)
                .ToList();
        }
        return fallback.ToList();
    }

    // ---- 界面可改项 ------------------------------------------------------

    /// <summary>system | light | dark</summary>
    public string Theme
    {
        get => GetString("ui", "theme", "system");
        set => Set("ui", "theme", value);
    }

    public bool CloseToTray
    {
        get => GetBool("ui", "close_to_tray", true);
        set => Set("ui", "close_to_tray", value);
    }

    public bool Autostart
    {
        get => GetBool("ui", "autostart", false);
        set => Set("ui", "autostart", value);
    }

    public int DebounceMs
    {
        get => Math.Clamp(GetInt("ui", "debounce_ms", 120), 0, 2000);
        set => Set("ui", "debounce_ms", value);
    }

    public bool TrayEnabled
    {
        get => GetBool("ui", "tray_enabled", true);
        set => Set("ui", "tray_enabled", value);
    }

    public string IndexRoot
    {
        get => GetString("index", "root", "");
        set => Set("index", "root", value);
    }

    public int ParallelVolumes
    {
        get => GetInt("index", "parallel_volumes", 0);
        set => Set("index", "parallel_volumes", value);
    }

    public bool DailyScan
    {
        get => GetBool("index", "daily_scan", false);
        set => Set("index", "daily_scan", value);
    }

    public string DailyScanAt
    {
        get => GetString("index", "daily_scan_at", "03:00");
        set => Set("index", "daily_scan_at", value);
    }

    public bool ResumeOnStart
    {
        get => GetBool("index", "resume_on_start", true);
        set => Set("index", "resume_on_start", value);
    }

    /// <summary>IO 节流档位（index.throttle）：low（默认，最省磁盘）/ medium / high / off。</summary>
    public string IoThrottle
    {
        get => GetString("index", "throttle", "low");
        set => Set("index", "throttle", value);
    }

    public List<string> ExcludePaths
    {
        get => GetStringList("index", "exclude_paths",
            new[] { "$Recycle.Bin", "RECYCLER", "System Volume Information" });
        set => Set("index", "exclude_paths", new JsonArray(value.Select(v => (JsonNode?)v).ToArray()));
    }

    public List<string> ExcludeTypes
    {
        get => GetStringList("index", "exclude_types", Array.Empty<string>());
        set => Set("index", "exclude_types", new JsonArray(value.Select(v => (JsonNode?)v).ToArray()));
    }

    /// <summary>用户自定义 glob 排除规则（index.exclude_rules）。内置默认排除与
    /// 索引目录规则是扫描时自动叠加的，不存这里——引导页与设置页共用本字段。</summary>
    public List<string> ExcludeRules
    {
        get => GetStringList("index", "exclude_rules", Array.Empty<string>());
        set => Set("index", "exclude_rules", new JsonArray(value.Select(v => (JsonNode?)v).ToArray()));
    }

    /// <summary>查询预算（毫秒）。后端不会因此中止搜索，只用于「耗时超预算」提醒；
    /// 默认给足 10 秒，避免宽泛查询/冷缓存时频繁出现 timeout_partial 提醒。设置页以「秒」暴露。</summary>
    public int QueryTimeoutMs
    {
        get => Math.Clamp(GetInt("query", "timeout_ms", 10000), 100, 60000);
        set => Set("query", "timeout_ms", Math.Clamp(value, 100, 60000));
    }

    /// <summary>索引进程内存预算（index.memory_budget_bytes），设置页以 MB 展示（256–4096）。
    /// 超出预算引擎会自动降级为只索引文件名/路径。</summary>
    public int MemoryBudgetMb
    {
        get
        {
            var v = Section("index")["memory_budget_bytes"] is JsonValue jv && jv.TryGetValue<long>(out var l)
                ? l
                : 1024L * 1024 * 1024;
            return (int)Math.Clamp(v / (1024 * 1024), 1, 8192);
        }
        set
        {
            var mb = Math.Clamp(value, 1, 8192);
            Section("index")["memory_budget_bytes"] = (long)mb * 1024 * 1024;
        }
    }
}
