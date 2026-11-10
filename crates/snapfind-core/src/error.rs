//! 错误码：与《SnapFind 开发说明文档》§4.9 一一对应。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    AuthFailed,
    ProtocolMismatch,
    BuildMismatch,
    VolumeUnavailable,
    PermissionDenied,
    IndexDirNotWritable,
    IndexCorrupted,
    ExtractUnsupported,
    ExtractEncrypted,
    ExtractTooLarge,
    ExtractTimeout,
    QuerySyntax,
    QueryTimeout,
    MemoryLimit,
    PayloadTooLarge,
    Cancelled,
    Internal,
    Busy,
    WatchOverflow,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthFailed => "ERR_AUTH_FAILED",
            Self::ProtocolMismatch => "ERR_PROTOCOL_MISMATCH",
            Self::BuildMismatch => "ERR_BUILD_MISMATCH",
            Self::VolumeUnavailable => "ERR_VOLUME_UNAVAILABLE",
            Self::PermissionDenied => "ERR_PERMISSION_DENIED",
            Self::IndexDirNotWritable => "ERR_INDEX_DIR_NOT_WRITABLE",
            Self::IndexCorrupted => "ERR_INDEX_CORRUPTED",
            Self::ExtractUnsupported => "ERR_EXTRACT_UNSUPPORTED",
            Self::ExtractEncrypted => "ERR_EXTRACT_ENCRYPTED",
            Self::ExtractTooLarge => "ERR_EXTRACT_TOO_LARGE",
            Self::ExtractTimeout => "ERR_EXTRACT_TIMEOUT",
            Self::QuerySyntax => "ERR_QUERY_SYNTAX",
            Self::QueryTimeout => "ERR_QUERY_TIMEOUT",
            Self::MemoryLimit => "ERR_MEMORY_LIMIT",
            Self::PayloadTooLarge => "ERR_PAYLOAD_TOO_LARGE",
            Self::Cancelled => "ERR_CANCELLED",
            Self::Internal => "ERR_INTERNAL",
            Self::Busy => "ERR_BUSY",
            Self::WatchOverflow => "ERR_WATCH_OVERFLOW",
        }
    }

    pub fn retryable(self) -> bool {
        matches!(self, Self::Busy | Self::MemoryLimit | Self::QueryTimeout)
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 带错误码的应用错误。
#[derive(Debug)]
pub struct SnapError {
    pub code: ErrorCode,
    pub message: String,
    pub detail: Option<serde_json::Value>,
}

impl SnapError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

impl std::fmt::Display for SnapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SnapError {}

pub type SnapResult<T> = Result<T, SnapError>;

impl From<rusqlite::Error> for SnapError {
    fn from(value: rusqlite::Error) -> Self {
        SnapError::new(ErrorCode::Internal, format!("sqlite: {value}"))
    }
}

impl From<std::io::Error> for SnapError {
    fn from(value: std::io::Error) -> Self {
        let code = match value.kind() {
            std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            std::io::ErrorKind::NotFound => ErrorCode::VolumeUnavailable,
            _ => ErrorCode::Internal,
        };
        SnapError::new(code, format!("io: {value}"))
    }
}
