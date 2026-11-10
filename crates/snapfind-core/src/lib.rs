//! SnapFind 核心层：配置、路径、卷发现、IPC 信封与错误码。
//!
//! 该 crate 不包含任何索引实现与 UI 逻辑，供 `snapfind-indexd`（索引进程）
//! 与 `snapfind`（UI/CLI 进程）共用，保证两端协议类型一致。

pub mod autostart;
pub mod config;
pub mod error;
pub mod ipc;
pub mod platform;
pub mod volume;

pub mod ids {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// 生成单调递增的消息 ID（ULID 风格：时间戳 + 进程内序号 + 计数器）。
    pub fn next_id(prefix: &str) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!(
            "{}{:011x}{:04x}{:04x}",
            prefix,
            now.as_millis() as u64,
            (std::process::id() & 0xffff) as u16,
            (n & 0xffff) as u16
        )
    }
}

pub const PROTOCOL_VERSION: u32 = 1;
pub const BUILD_ID: &str = concat!(env!("CARGO_PKG_VERSION"), "-", env!("CARGO_PKG_NAME"));
