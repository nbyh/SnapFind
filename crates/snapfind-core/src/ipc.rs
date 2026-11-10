//! IPC 协议：统一信封 + 消息类型 + 帧编解码（文档 §4.2–§4.6）。
//!
//! 传输：Windows 命名管道 / Linux UDS，**不监听 TCP**。
//! 帧格式：4 字节大端长度 + UTF-8 JSON。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// 消息类型常量。
pub mod msg {
    pub const HELLO: &str = "hello";
    pub const HELLO_ACK: &str = "hello.ack";
    pub const ERROR: &str = "error";
    pub const HEARTBEAT: &str = "heartbeat";
    pub const CONFIG_GET: &str = "config.get";
    pub const VOLUME_LIST: &str = "volume.list";
    pub const VOLUME_ACTION: &str = "volume.action";
    pub const VOLUME_STATE: &str = "volume.state";
    pub const SCOPE_PREVIEW: &str = "index.scope.preview";
    pub const SCOPE_REGISTER: &str = "scope.register";
    pub const INDEX_START: &str = "index.start";
    pub const QUERY_PARSE: &str = "query.parse";
    pub const QUERY_EXECUTE: &str = "query.execute";
    pub const QUERY_CANCEL: &str = "query.cancel";
    pub const PREVIEW_META: &str = "preview.meta";
    /// 由 indexd 复用引擎的抽取器返回预览正文。
    ///
    /// 供不具备 Office/PDF 解析能力的前端（如 .NET/Avalonia 版 UI）使用：
    /// 请求 `{ path, max_bytes }`，响应 `{ path, text, truncated, name_only, encoding, error? }`。
    pub const PREVIEW_READ: &str = "preview.read";
    pub const HISTORY_LIST: &str = "history.list";
    pub const STATS_GET: &str = "stats.get";
    /// 扩展名分布（用于搜索页「类型」多选标签）。
    pub const STATS_EXTENSIONS: &str = "stats.extensions";
    pub const EVENT_PROGRESS: &str = "event.progress";
    pub const EVENT_VOLUME_STATE: &str = "event.volumeState";
    pub const EVENT_WATCH_OVERFLOW: &str = "event.watchOverflow";
    pub const EVENT_MEMORY_PRESSURE: &str = "event.memoryPressure";
    pub const EVENT_INDEX_DEGRADED: &str = "event.indexDegraded";
    pub const SHUTDOWN: &str = "shutdown";
    pub const SHUTDOWN_ACK: &str = "shutdown.ack";
    pub const BYE: &str = "bye";
}

/// 统一信封（字段名与文档严格一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(rename = "v")]
    pub version: u32,
    pub id: String,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub ts: i64,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, rename = "final", skip_serializing_if = "Option::is_none")]
    pub is_final: Option<bool>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Envelope {
    pub fn new(msg_type: &str, payload: Value) -> Self {
        Self {
            version: crate::PROTOCOL_VERSION,
            id: crate::ids::next_id(""),
            msg_type: msg_type.to_string(),
            ts: now_ms(),
            payload,
            reply_to: None,
            seq: None,
            is_final: None,
        }
    }

    pub fn request(msg_type: &str, payload: impl Serialize) -> Self {
        Self::new(msg_type, serde_json::to_value(payload).unwrap_or(Value::Null))
    }

    pub fn reply(&self, msg_type: &str, payload: impl Serialize) -> Self {
        let mut env = Self::new(msg_type, serde_json::to_value(payload).unwrap_or(Value::Null));
        env.reply_to = Some(self.id.clone());
        env.id = self.id.clone();
        env
    }

    pub fn error(&self, code: &str, message: impl Into<String>) -> Self {
        let mut env = Self::new(
            msg::ERROR,
            serde_json::to_value(ErrorPayload {
                code: code.to_string(),
                message: message.into(),
                detail: None,
                retryable: false,
                volume_id: None,
            })
            .unwrap_or(Value::Null),
        );
        env.reply_to = Some(self.id.clone());
        env.id = self.id.clone();
        env
    }

    pub fn event(msg_type: &str, payload: impl Serialize, seq: u64) -> Self {
        let mut env = Self::new(msg_type, serde_json::to_value(payload).unwrap_or(Value::Null));
        env.seq = Some(seq);
        env
    }

    pub fn decode<T: for<'de> Deserialize<'de>>(&self) -> Result<T, String> {
        serde_json::from_value(self.payload.clone()).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub token: String,
    pub build_id: String,
    pub supported_versions: Vec<u32>,
    #[serde(default)]
    pub encodings: Vec<String>,
    #[serde(default)]
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAck {
    pub version: u32,
    pub backend: String,
    pub build_id: String,
    pub index_root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
    pub pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    /// `ping` 或 `pong`
    pub kind: String,
    #[serde(default)]
    pub seq: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Mode {
    pub regex: bool,
    pub fuzzy: bool,
    pub fuzzy_distance: u8,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Fields {
    pub name: bool,
    pub path: bool,
    pub content: bool,
}

impl Default for Fields {
    fn default() -> Self {
        Self {
            name: true,
            path: true,
            content: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Filters {
    pub types: Vec<String>,
    pub exclude_types: Vec<String>,
    pub size_min: Option<u64>,
    pub size_max: Option<u64>,
    pub mtime_from: Option<i64>,
    pub mtime_to: Option<i64>,
    pub paths: Vec<String>,
    pub exclude_paths: Vec<String>,
    pub volumes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SortSpec {
    /// relevance | name | size | mtime
    pub key: String,
    /// asc | desc
    pub order: String,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            key: "relevance".into(),
            order: "desc".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Paging {
    pub offset: usize,
    pub limit: usize,
}

impl Default for Paging {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: 50,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HighlightCfg {
    pub enable: bool,
    pub snippet_len: usize,
    pub max_snippets: usize,
    pub pre: String,
    pub post: String,
}

impl Default for HighlightCfg {
    fn default() -> Self {
        Self {
            enable: true,
            snippet_len: 160,
            max_snippets: 3,
            pre: "<mark>".into(),
            post: "</mark>".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct QueryRequest {
    pub request_id: String,
    pub query: String,
    pub mode: Mode,
    pub fields: Fields,
    pub filters: Filters,
    pub sort: SortSpec,
    pub paging: Paging,
    pub highlight: HighlightCfg,
    pub timeout_ms: Option<u64>,
    pub first_batch_hint_ms: Option<u64>,
}

impl Default for QueryRequest {
    fn default() -> Self {
        Self {
            request_id: String::new(),
            query: String::new(),
            mode: Mode::default(),
            fields: Fields::default(),
            filters: Filters::default(),
            sort: SortSpec::default(),
            paging: Paging::default(),
            highlight: HighlightCfg::default(),
            timeout_ms: None,
            first_batch_hint_ms: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HitFlags {
    pub encrypted: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    pub field: String,
    pub text: String,
    /// UTF-16 单元区间（与前端 JS 字符串索引一致）。
    pub offsets: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
    pub doc_id: i64,
    pub volume_id: String,
    pub path: String,
    pub name: String,
    pub ext: String,
    pub size: u64,
    pub mtime: i64,
    pub score: f64,
    pub matched_fields: Vec<String>,
    pub snippets: Vec<Snippet>,
    #[serde(default)]
    pub flags: HitFlags,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryChunk {
    pub request_id: String,
    /// partial | final | cancelled
    pub phase: String,
    pub total_estimate: usize,
    pub scanned_segments: u32,
    pub total_segments: u32,
    pub took_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub hits: Vec<Hit>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelRequest {
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedSample {
    pub path: String,
    pub code: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Throughput {
    pub files_per_sec: u64,
    pub bytes_per_sec: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointInfo {
    pub id: u64,
    pub at: i64,
    pub cursor_path: String,
    pub resumable: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryInfo {
    pub rss_bytes: u64,
    pub budget_bytes: u64,
    pub pressure: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IoInfo {
    pub throttle: String,
    pub actual_bytes_per_sec: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub volume_id: String,
    pub mount: String,
    pub state: String,
    pub phase: String,
    pub files_total: u64,
    pub files_done: u64,
    pub bytes_done: u64,
    pub dirs_done: u64,
    pub files_failed: u64,
    /// 已完成的一级子树数（进度百分比的真实分母来源，见 subs_total）。
    #[serde(default)]
    pub subs_done: u64,
    /// 根目录一级子树总数：扫描开始时即已知，是唯一真实可算的总量。
    /// files_total 只是「已发现」的滚动累加值，恒等于 files_done，算不出百分比。
    #[serde(default)]
    pub subs_total: u64,
    #[serde(default)]
    pub failed_samples: Vec<FailedSample>,
    pub throughput: Throughput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointInfo>,
    pub memory: MemoryInfo,
    pub io: IoInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeStateEvent {
    pub volume_id: String,
    pub mount: String,
    /// 目标状态（Discovered/Scanning/Indexing/Paused/Stopped/Rebuilding/Cleaning/Watching/Degraded/Unavailable）
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub files_done: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeAction {
    /// start | pause | resume | stop | rebuild | clean | setExcludes | skip
    pub action: String,
    #[serde(default)]
    pub volume_id: Option<String>,
    #[serde(default)]
    pub excludes: Option<Vec<String>>,
    #[serde(default)]
    pub volumes: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopePreview {
    pub index_root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
    pub volumes: Vec<ScopeVolume>,
    pub excludes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeVolume {
    pub volume_id: String,
    pub mount: String,
    pub fs: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub removable: bool,
    pub network: bool,
    pub enabled: bool,
    pub estimated_files: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStart {
    #[serde(default)]
    pub volumes: Vec<String>,
    #[serde(default)]
    pub excludes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStartAck {
    pub started: Vec<String>,
    pub parallel_volumes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeStats {
    pub volume_id: String,
    pub mount: String,
    pub state: String,
    pub docs: u64,
    pub index_bytes: u64,
    pub segments: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub index_root: String,
    pub backend: String,
    pub total_docs: u64,
    pub total_index_bytes: u64,
    pub rss_bytes: u64,
    pub memory_budget_bytes: u64,
    pub volumes: Vec<VolumeStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownReq {
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownAck {
    pub saved_checkpoint: bool,
    pub volumes: Vec<VolumeStateEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewMeta {
    pub doc_id: i64,
    pub path: String,
    pub name: String,
    pub ext: String,
    pub size: u64,
    pub mtime: i64,
    pub encoding: String,
    pub extractable: bool,
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// 帧编解码
// ---------------------------------------------------------------------------

pub async fn write_frame<W>(w: &mut W, env: &Envelope) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(env)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let len = (body.len() as u32).to_be_bytes();
    w.write_all(&len).await?;
    w.write_all(&body).await?;
    w.flush().await
}

/// 读取一帧；返回 `Ok(None)` 表示对端关闭连接。
pub async fn read_frame<R>(r: &mut R) -> std::io::Result<Option<Envelope>>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid frame length",
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    let env: Envelope = serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(env))
}
