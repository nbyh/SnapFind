using System.Globalization;
using Avalonia.Media;

namespace SnapFind.Desktop.Helpers;

public static class Format
{
    /// <summary>1024 进制，B/KB/MB/GB/TB；1 字节以下单位不带小数。</summary>
    public static string Human(ulong bytes)
    {
        string[] units = { "B", "KB", "MB", "GB", "TB" };
        double v = bytes;
        var i = 0;
        while (v >= 1024 && i < units.Length - 1)
        {
            v /= 1024;
            i++;
        }
        return i == 0
            ? $"{bytes} B"
            : string.Format(CultureInfo.InvariantCulture, "{0:0.0} {1}", v, units[i]);
    }

    public static string Time(long unixSeconds)
    {
        if (unixSeconds <= 0) return "-";
        return DateTimeOffset.FromUnixTimeSeconds(unixSeconds)
            .ToLocalTime()
            .ToString("yyyy-MM-dd HH:mm:ss", CultureInfo.InvariantCulture);
    }

    public static string TimeShort(long unixSeconds)
    {
        if (unixSeconds <= 0) return "-";
        return DateTimeOffset.FromUnixTimeSeconds(unixSeconds)
            .ToLocalTime()
            .ToString("yyyy-MM-dd HH:mm", CultureInfo.InvariantCulture);
    }

    /// <summary>状态 -> 中文。</summary>
    public static string StateText(string state) => state switch
    {
        "Discovered" => "未索引",
        "Scanning" => "扫描中",
        "Indexing" => "建立索引",
        "Paused" => "已暂停",
        "Stopped" => "已停止",
        "Rebuilding" => "重建中",
        "Cleaning" => "清理中",
        "Watching" => "增量监控",
        "Degraded" => "已降级",
        "Unavailable" => "不可用",
        _ => state,
    };

    /// <summary>IO 限速档位 -> 中文（引擎取值：off/low/medium/high，默认 low）。</summary>
    public static string ThrottleText(string level) => (level ?? "").ToLowerInvariant() switch
    {
        "off" or "none" or "disabled" => "关闭",
        "high" => "高",
        "medium" => "中",
        _ => "低",
    };

    /// <summary>状态 -> 语义色资源键。</summary>
    public static string StateBrushKey(string state) => state switch
    {
        "Watching" => "Good",
        "Indexing" or "Scanning" or "Rebuilding" => "Accent",
        "Paused" or "Stopped" => "Warn",
        "Unavailable" or "Degraded" => "Bad",
        _ => "Text2",
    };

    /// <summary>把语义色资源键解析成画刷；资源缺失时退回默认前景色。</summary>
    public static IBrush ResolveBrush(string key)
    {
        var app = Avalonia.Application.Current;
        if (app is not null
            && Avalonia.Controls.ResourceNodeExtensions.TryFindResource(app, key, out var res)
            && res is IBrush brush)
        {
            return brush;
        }
        return Brushes.Gray;
    }

    /// <summary>扩展名 -> 类别色（与 views.rs::ext_color 一致）。</summary>
    public static Color ExtColor(string ext)
    {
        var e = (ext ?? "").TrimStart('.').ToLowerInvariant();
        return e switch
        {
            "doc" or "docx" or "rtf" or "odt" or "wps" => Color.Parse("#1E88E5"),
            "xls" or "xlsx" or "csv" or "ods" => Color.Parse("#43A047"),
            "ppt" or "pptx" or "odp" => Color.Parse("#FB8C00"),
            "pdf" or "xps" or "djvu" => Color.Parse("#E53935"),
            "rs" or "py" or "js" or "ts" or "c" or "h" or "cpp" or "java" or "go" or "cs" or "rb"
                or "php" or "swift" or "kt" => Color.Parse("#2E7D32"),
            "html" or "htm" or "css" or "scss" or "vue" or "svelte" => Color.Parse("#00BCD4"),
            "json" or "yaml" or "yml" or "toml" or "ini" or "xml" => Color.Parse("#00897B"),
            "txt" or "md" or "log" or "nfo" => Color.Parse("#607D8B"),
            "png" or "jpg" or "jpeg" or "gif" or "webp" or "svg" or "psd" or "bmp" or "ico"
                => Color.Parse("#8E24AA"),
            "mp3" or "mp4" or "mkv" or "wav" or "flac" or "avi" or "mov" => Color.Parse("#F4511E"),
            "zip" or "7z" or "rar" or "iso" or "gz" or "tar" or "xz" => Color.Parse("#F9A825"),
            "epub" or "mobi" or "azw" or "azw3" or "chm" => Color.Parse("#5E35B1"),
            "exe" or "dll" or "sys" or "bat" or "cmd" or "ps1" or "apk" or "msi" => Color.Parse("#78909C"),
            "ttf" or "otf" or "woff" or "woff2" => Color.Parse("#546E7A"),
            "db" or "sqlite" or "mdb" or "sql" => Color.Parse("#6D4C41"),
            _ => Color.Parse("#9E9E9E"),
        };
    }

    /// <summary>内容是否可抽取（不可读类型只按文件名/路径匹配）。</summary>
    public static bool ContentReadable(string ext)
    {
        var e = (ext ?? "").TrimStart('.').ToLowerInvariant();
        if (e.Length == 0) return false;
        return e switch
        {
            "exe" or "dll" or "sys" or "zip" or "7z" or "rar" or "iso" or "gz" or "tar" or "xz"
                or "png" or "jpg" or "jpeg" or "gif" or "webp" or "psd" or "bmp" or "ico"
                or "mp3" or "mp4" or "mkv" or "wav" or "flac" or "avi" or "mov"
                or "ttf" or "otf" or "woff" or "woff2" or "db" or "sqlite" or "mdb"
                or "apk" or "msi" or "so" or "a" or "lib" or "obj" or "pdb" => false,
            _ => true,
        };
    }

    public static string FieldLabel(string field) => field switch
    {
        "content" => "内容",
        "name" => "文件名",
        "path" => "路径",
        _ => field,
    };

    /// <summary>
    /// 从查询串切出用于正文高亮的词：按非字母数字切分，保留长度 ≥2，
    /// ASCII 转小写（与 Rust 侧 <c>views::query_terms</c> 一致）。
    /// </summary>
    public static IReadOnlyList<string> Terms(string? query)
    {
        if (string.IsNullOrWhiteSpace(query)) return Array.Empty<string>();
        var list = new List<string>();
        var sb = new System.Text.StringBuilder();

        void Flush()
        {
            if (sb.Length >= 2)
            {
                var term = sb.ToString();
                list.Add(term.All(c => c < 128) ? term.ToLowerInvariant() : term);
            }
            sb.Clear();
        }

        foreach (var c in query)
        {
            if (char.IsLetterOrDigit(c)) sb.Append(c);
            else Flush();
        }
        Flush();
        return list;
    }

    /// <summary>标签文字：扩展名前 4 字符大写，空则圆点。</summary>
    public static string ExtBadge(string ext)
    {
        var e = (ext ?? "").TrimStart('.');
        if (e.Length == 0) return "·";
        return e.Length <= 4 ? e.ToUpperInvariant() : e[..4].ToUpperInvariant();
    }
}
