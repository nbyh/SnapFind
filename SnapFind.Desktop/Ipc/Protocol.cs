using System.Text.Json;
using System.Text.Json.Serialization;

namespace SnapFind.Desktop.Ipc;

/// <summary>
/// 统一信封。字段名与 Rust 侧 <c>snapfind_core::ipc::Envelope</c> 严格一致
/// （<c>v / id / type / ts / payload / reply_to / seq / final</c>）。
/// </summary>
public sealed class Envelope
{
    [JsonPropertyName("v")] public uint V { get; set; } = 1;
    [JsonPropertyName("id")] public string Id { get; set; } = "";
    [JsonPropertyName("type")] public string Type { get; set; } = "";
    [JsonPropertyName("ts")] public long Ts { get; set; }
    [JsonPropertyName("payload")] public JsonElement Payload { get; set; }
    [JsonPropertyName("reply_to")] public string? ReplyTo { get; set; }
    [JsonPropertyName("seq")] public ulong? Seq { get; set; }
    [JsonPropertyName("final")] public bool? Final { get; set; }

    public bool IsFinal => Final ?? true;

    public T? Decode<T>() => Payload.ValueKind == JsonValueKind.Undefined
        ? default
        : Payload.Deserialize<T>(IpcJson.Options);
}

public static class Msg
{
    public const string Hello = "hello";
    public const string HelloAck = "hello.ack";
    public const string Error = "error";
    public const string Heartbeat = "heartbeat";
    public const string VolumeList = "volume.list";
    public const string VolumeAction = "volume.action";
    public const string VolumeState = "volume.state";
    public const string ScopePreview = "index.scope.preview";
    public const string ScopeRegister = "scope.register";
    public const string IndexStart = "index.start";
    public const string QueryParse = "query.parse";
    public const string QueryExecute = "query.execute";
    public const string QueryCancel = "query.cancel";
    public const string QueryResponse = "response.query.execute";
    public const string PreviewMeta = "preview.meta";
    public const string HistoryList = "history.list";
    public const string StatsGet = "stats.get";
    public const string StatsExtensions = "stats.extensions";
    public const string EventProgress = "event.progress";
    public const string EventVolumeState = "event.volumeState";
    public const string EventWatchOverflow = "event.watchOverflow";
    public const string EventMemoryPressure = "event.memoryPressure";
    public const string EventIndexDegraded = "event.indexDegraded";
    public const string Shutdown = "shutdown";
    public const string ShutdownAck = "shutdown.ack";
    public const string Bye = "bye";

    /// <summary>新增：由 indexd 复用 Rust 的抽取器返回预览正文（.NET 端不具备 docx/pdf 解析能力）。</summary>
    public const string PreviewRead = "preview.read";
}

public static class IpcJson
{
    public static readonly JsonSerializerOptions Options = new()
    {
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        PropertyNameCaseInsensitive = false,
    };

    public static readonly JsonSerializerOptions Indented = new(Options) { WriteIndented = true };
}

// ---- 握手 ----------------------------------------------------------------

public sealed class Hello
{
    [JsonPropertyName("token")] public string Token { get; set; } = "";
    [JsonPropertyName("build_id")] public string BuildId { get; set; } = "";
    [JsonPropertyName("supported_versions")] public List<uint> SupportedVersions { get; set; } = new();
    [JsonPropertyName("encodings")] public List<string> Encodings { get; set; } = new() { "json" };
    [JsonPropertyName("role")] public string Role { get; set; } = "ui";
}

public sealed class HelloAck
{
    [JsonPropertyName("version")] public uint Version { get; set; }
    [JsonPropertyName("backend")] public string Backend { get; set; } = "";
    [JsonPropertyName("build_id")] public string BuildId { get; set; } = "";
    [JsonPropertyName("index_root")] public string IndexRoot { get; set; } = "";
    [JsonPropertyName("degraded_reason")] public string? DegradedReason { get; set; }
    [JsonPropertyName("pid")] public uint Pid { get; set; }
}

public sealed class ErrorPayload
{
    [JsonPropertyName("code")] public string Code { get; set; } = "ERR";
    [JsonPropertyName("message")] public string Message { get; set; } = "";
    [JsonPropertyName("retryable")] public bool Retryable { get; set; }
}

// ---- 查询 ----------------------------------------------------------------

public sealed class QueryMode
{
    [JsonPropertyName("regex")] public bool Regex { get; set; }
    [JsonPropertyName("fuzzy")] public bool Fuzzy { get; set; }
    [JsonPropertyName("fuzzy_distance")] public int FuzzyDistance { get; set; } = 1;
    [JsonPropertyName("case_sensitive")] public bool CaseSensitive { get; set; }
    [JsonPropertyName("whole_word")] public bool WholeWord { get; set; }
}

public sealed class QueryFields
{
    [JsonPropertyName("name")] public bool Name { get; set; } = true;
    [JsonPropertyName("path")] public bool Path { get; set; } = true;
    [JsonPropertyName("content")] public bool Content { get; set; } = true;
}

public sealed class QueryFilters
{
    [JsonPropertyName("types")] public List<string> Types { get; set; } = new();
    [JsonPropertyName("exclude_types")] public List<string> ExcludeTypes { get; set; } = new();
    [JsonPropertyName("size_min")] public long? SizeMin { get; set; }
    [JsonPropertyName("size_max")] public long? SizeMax { get; set; }
    [JsonPropertyName("mtime_from")] public long? MtimeFrom { get; set; }
    [JsonPropertyName("mtime_to")] public long? MtimeTo { get; set; }
    [JsonPropertyName("paths")] public List<string> Paths { get; set; } = new();
    [JsonPropertyName("exclude_paths")] public List<string> ExcludePaths { get; set; } = new();
    [JsonPropertyName("volumes")] public List<string> Volumes { get; set; } = new();
}

public sealed class SortSpec
{
    [JsonPropertyName("key")] public string Key { get; set; } = "relevance";
    [JsonPropertyName("order")] public string Order { get; set; } = "desc";
}

public sealed class Paging
{
    [JsonPropertyName("offset")] public int Offset { get; set; }
    [JsonPropertyName("limit")] public int Limit { get; set; } = 1000;
}

public sealed class HighlightCfg
{
    [JsonPropertyName("enable")] public bool Enable { get; set; } = true;
    [JsonPropertyName("snippet_len")] public int SnippetLen { get; set; } = 160;
    [JsonPropertyName("max_snippets")] public int MaxSnippets { get; set; } = 3;
    [JsonPropertyName("pre")] public string Pre { get; set; } = "<mark>";
    [JsonPropertyName("post")] public string Post { get; set; } = "</mark>";
}

public sealed class QueryRequest
{
    [JsonPropertyName("request_id")] public string RequestId { get; set; } = "";
    [JsonPropertyName("query")] public string Query { get; set; } = "";
    [JsonPropertyName("mode")] public QueryMode Mode { get; set; } = new();
    [JsonPropertyName("fields")] public QueryFields Fields { get; set; } = new();
    [JsonPropertyName("filters")] public QueryFilters Filters { get; set; } = new();
    [JsonPropertyName("sort")] public SortSpec Sort { get; set; } = new();
    [JsonPropertyName("paging")] public Paging Paging { get; set; } = new();
    [JsonPropertyName("highlight")] public HighlightCfg Highlight { get; set; } = new();
    [JsonPropertyName("timeout_ms")] public long? TimeoutMs { get; set; } = 2000;
    [JsonPropertyName("first_batch_hint_ms")] public long? FirstBatchHintMs { get; set; }
}

public sealed class HitFlags
{
    [JsonPropertyName("encrypted")] public bool Encrypted { get; set; }
    [JsonPropertyName("truncated")] public bool Truncated { get; set; }
}

public sealed class Snippet
{
    [JsonPropertyName("field")] public string Field { get; set; } = "";
    [JsonPropertyName("text")] public string Text { get; set; } = "";
    /// <summary>UTF-16 单元区间，与 C# 字符串索引一致，可直接用于高亮切分。</summary>
    [JsonPropertyName("offsets")] public List<int[]> Offsets { get; set; } = new();
}

public sealed class Hit
{
    [JsonPropertyName("doc_id")] public long DocId { get; set; }
    [JsonPropertyName("volume_id")] public string VolumeId { get; set; } = "";
    [JsonPropertyName("path")] public string Path { get; set; } = "";
    [JsonPropertyName("name")] public string Name { get; set; } = "";
    [JsonPropertyName("ext")] public string Ext { get; set; } = "";
    [JsonPropertyName("size")] public ulong Size { get; set; }
    [JsonPropertyName("mtime")] public long Mtime { get; set; }
    [JsonPropertyName("score")] public double Score { get; set; }
    [JsonPropertyName("matched_fields")] public List<string> MatchedFields { get; set; } = new();
    [JsonPropertyName("snippets")] public List<Snippet> Snippets { get; set; } = new();
    [JsonPropertyName("flags")] public HitFlags Flags { get; set; } = new();
}

public sealed class QueryChunk
{
    [JsonPropertyName("request_id")] public string RequestId { get; set; } = "";
    [JsonPropertyName("phase")] public string Phase { get; set; } = "partial";
    [JsonPropertyName("total_estimate")] public int TotalEstimate { get; set; }
    [JsonPropertyName("scanned_segments")] public uint ScannedSegments { get; set; }
    [JsonPropertyName("total_segments")] public uint TotalSegments { get; set; }
    [JsonPropertyName("took_ms")] public ulong TookMs { get; set; }
    [JsonPropertyName("next_cursor")] public string? NextCursor { get; set; }
    [JsonPropertyName("hits")] public List<Hit> Hits { get; set; } = new();
    [JsonPropertyName("warnings")] public List<string> Warnings { get; set; } = new();
}

// ---- 索引 / 卷 ----------------------------------------------------------

public sealed class Throughput
{
    [JsonPropertyName("files_per_sec")] public ulong FilesPerSec { get; set; }
    [JsonPropertyName("bytes_per_sec")] public ulong BytesPerSec { get; set; }
}

public sealed class MemoryInfo
{
    [JsonPropertyName("rss_bytes")] public ulong RssBytes { get; set; }
    [JsonPropertyName("budget_bytes")] public ulong BudgetBytes { get; set; }
    [JsonPropertyName("pressure")] public bool Pressure { get; set; }
}

public sealed class IoInfo
{
    [JsonPropertyName("throttle")] public string Throttle { get; set; } = "low";
    [JsonPropertyName("actual_bytes_per_sec")] public ulong ActualBytesPerSec { get; set; }
}

public sealed class FailedSample
{
    [JsonPropertyName("path")] public string Path { get; set; } = "";
    [JsonPropertyName("code")] public string Code { get; set; } = "";
}

public sealed class CheckpointInfo
{
    [JsonPropertyName("id")] public ulong Id { get; set; }
    [JsonPropertyName("at")] public long At { get; set; }
    [JsonPropertyName("cursor_path")] public string CursorPath { get; set; } = "";
    [JsonPropertyName("resumable")] public bool Resumable { get; set; }
}

public sealed class Progress
{
    [JsonPropertyName("volume_id")] public string VolumeId { get; set; } = "";
    [JsonPropertyName("mount")] public string Mount { get; set; } = "";
    [JsonPropertyName("state")] public string State { get; set; } = "";
    [JsonPropertyName("phase")] public string Phase { get; set; } = "";
    [JsonPropertyName("files_total")] public ulong FilesTotal { get; set; }
    [JsonPropertyName("files_done")] public ulong FilesDone { get; set; }
    [JsonPropertyName("bytes_done")] public ulong BytesDone { get; set; }
    [JsonPropertyName("dirs_done")] public ulong DirsDone { get; set; }
    [JsonPropertyName("files_failed")] public ulong FilesFailed { get; set; }
    /// <summary>已完成的一级子树数（与 subs_total 一起构成真实百分比）。</summary>
    [JsonPropertyName("subs_done")] public ulong SubsDone { get; set; }
    /// <summary>根目录一级子树总数：扫描开始时已知，唯一真实可算的总量。</summary>
    [JsonPropertyName("subs_total")] public ulong SubsTotal { get; set; }
    [JsonPropertyName("failed_samples")] public List<FailedSample> FailedSamples { get; set; } = new();
    [JsonPropertyName("throughput")] public Throughput Throughput { get; set; } = new();
    [JsonPropertyName("eta_sec")] public ulong? EtaSec { get; set; }
    [JsonPropertyName("checkpoint")] public CheckpointInfo? Checkpoint { get; set; }
    [JsonPropertyName("memory")] public MemoryInfo Memory { get; set; } = new();
    [JsonPropertyName("io")] public IoInfo Io { get; set; } = new();
}

public sealed class VolumeStateEvent
{
    [JsonPropertyName("volume_id")] public string VolumeId { get; set; } = "";
    [JsonPropertyName("mount")] public string Mount { get; set; } = "";
    [JsonPropertyName("state")] public string State { get; set; } = "";
    [JsonPropertyName("reason")] public string? Reason { get; set; }
    [JsonPropertyName("files_done")] public ulong FilesDone { get; set; }
}

public sealed class VolumeAction
{
    [JsonPropertyName("action")] public string Action { get; set; } = "";
    [JsonPropertyName("volume_id")] public string? VolumeId { get; set; }
    [JsonPropertyName("excludes")] public List<string>? Excludes { get; set; }
    [JsonPropertyName("volumes")] public List<string>? Volumes { get; set; }
}

/// <summary>volume.action 的响应：每个卷动作后的最终状态。</summary>
public sealed class VolumeActionAck
{
    [JsonPropertyName("ok")] public bool Ok { get; set; }
    [JsonPropertyName("action")] public string Action { get; set; } = "";
    [JsonPropertyName("states")] public Dictionary<string, string> States { get; set; } = new();
}

public sealed class ScopeVolume
{
    [JsonPropertyName("volume_id")] public string VolumeId { get; set; } = "";
    [JsonPropertyName("mount")] public string Mount { get; set; } = "";
    [JsonPropertyName("fs")] public string Fs { get; set; } = "";
    [JsonPropertyName("total_bytes")] public ulong TotalBytes { get; set; }
    [JsonPropertyName("free_bytes")] public ulong FreeBytes { get; set; }
    [JsonPropertyName("removable")] public bool Removable { get; set; }
    [JsonPropertyName("network")] public bool Network { get; set; }
    [JsonPropertyName("enabled")] public bool Enabled { get; set; }
    [JsonPropertyName("estimated_files")] public ulong EstimatedFiles { get; set; }
}

public sealed class ScopePreview
{
    [JsonPropertyName("index_root")] public string IndexRoot { get; set; } = "";
    [JsonPropertyName("degraded_reason")] public string? DegradedReason { get; set; }
    [JsonPropertyName("volumes")] public List<ScopeVolume> Volumes { get; set; } = new();
    [JsonPropertyName("excludes")] public List<string> Excludes { get; set; } = new();
}

public sealed class IndexStart
{
    [JsonPropertyName("volumes")] public List<string> Volumes { get; set; } = new();
    [JsonPropertyName("excludes")] public List<string> Excludes { get; set; } = new();
}

public sealed class IndexStartAck
{
    [JsonPropertyName("started")] public List<string> Started { get; set; } = new();
    [JsonPropertyName("parallel_volumes")] public int ParallelVolumes { get; set; }
}

public sealed class VolumeStats
{
    [JsonPropertyName("volume_id")] public string VolumeId { get; set; } = "";
    [JsonPropertyName("mount")] public string Mount { get; set; } = "";
    [JsonPropertyName("state")] public string State { get; set; } = "";
    [JsonPropertyName("docs")] public ulong Docs { get; set; }
    [JsonPropertyName("index_bytes")] public ulong IndexBytes { get; set; }
    [JsonPropertyName("segments")] public uint Segments { get; set; }
}

public sealed class Stats
{
    [JsonPropertyName("index_root")] public string IndexRoot { get; set; } = "";
    [JsonPropertyName("backend")] public string Backend { get; set; } = "";
    [JsonPropertyName("total_docs")] public ulong TotalDocs { get; set; }
    [JsonPropertyName("total_index_bytes")] public ulong TotalIndexBytes { get; set; }
    [JsonPropertyName("rss_bytes")] public ulong RssBytes { get; set; }
    [JsonPropertyName("memory_budget_bytes")] public ulong MemoryBudgetBytes { get; set; }
    [JsonPropertyName("volumes")] public List<VolumeStats> Volumes { get; set; } = new();
}

public sealed class ExtStat
{
    [JsonPropertyName("ext")] public string Ext { get; set; } = "";
    [JsonPropertyName("count")] public ulong Count { get; set; }
}

public sealed class ExtStatList
{
    [JsonPropertyName("extensions")] public List<ExtStat> Extensions { get; set; } = new();
}

// ---- 预览（新增消息）----------------------------------------------------

public sealed class PreviewReadRequest
{
    [JsonPropertyName("path")] public string Path { get; set; } = "";
    [JsonPropertyName("max_bytes")] public long MaxBytes { get; set; } = 32L * 1024 * 1024;
}

public sealed class PreviewRead
{
    [JsonPropertyName("path")] public string Path { get; set; } = "";
    [JsonPropertyName("text")] public string Text { get; set; } = "";
    [JsonPropertyName("truncated")] public bool Truncated { get; set; }
    [JsonPropertyName("name_only")] public bool NameOnly { get; set; }
    [JsonPropertyName("encoding")] public string Encoding { get; set; } = "";
    [JsonPropertyName("error")] public string? Error { get; set; }
}
