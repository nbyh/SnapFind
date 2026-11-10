//! IO 节流与内存分级降级（文档 §8.1、§8.2）。
//!
//! * **节流**：按 `index.throttle` 配置的「文件/秒」与「字节/秒」双配额做 1 秒窗口令牌桶，
//!   额度用尽即睡眠，索引吞吐可预期，不抢占前台 IO；
//! * **内存分级**：周期性采样进程 RSS，按预算比例降级内容抽取：
//!   - L0 正常；
//!   - L1（RSS ≥ 75% 预算）正文上限压到 256 KiB；
//!   - L2（RSS ≥ 预算）只索引文件名/路径（元数据），并置 `flags` 标记「未抽取正文」，
//!     RSS 回落且完成一轮扫描后自动恢复 L0。

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// L1 档正文硬上限。
pub const LEVEL1_CONTENT_CAP: usize = 256 * 1024;
/// 文档未抽取正文（仅元数据）标记位，写入 `docs.flags`。
pub const FLAG_METADATA_ONLY: i64 = 4;

struct Window {
    start: Instant,
    files: u64,
    bytes: u64,
}

/// 共享节流/降级控制器：一个索引进程一份，所有卷线程共用。
pub struct Governor {
    files_per_sec: u64,
    bytes_per_sec: u64,
    budget: u64,
    window: Mutex<Window>,
    level: AtomicU8,
    last_rate: AtomicU64,
}

impl Governor {
    pub fn new(throttle: &str, budget: u64) -> Self {
        // 额度取值参考文档 §8.1：低占用为默认档
        let (files_per_sec, bytes_per_sec) = match throttle {
            "off" | "none" | "disabled" => (0, 0),
            "high" => (600, 96 * 1024 * 1024),
            "medium" => (240, 24 * 1024 * 1024),
            _ => (80, 6 * 1024 * 1024), // low（默认）
        };
        Self {
            files_per_sec,
            bytes_per_sec,
            budget,
            window: Mutex::new(Window {
                start: Instant::now(),
                files: 0,
                bytes: 0,
            }),
            level: AtomicU8::new(0),
            last_rate: AtomicU64::new(0),
        }
    }

    /// 索引每个文件前调用：超出配额则睡眠（分片睡眠，保证暂停/关闭可及时响应）。
    pub fn before_file(&self, size: u64) {
        if self.files_per_sec == 0 && self.bytes_per_sec == 0 {
            return;
        }
        loop {
            let wait = {
                let mut w = self.window.lock().unwrap();
                let elapsed = w.start.elapsed();
                if elapsed >= Duration::from_secs(1) {
                    let rate = w.bytes;
                    w.start = Instant::now();
                    w.files = 0;
                    w.bytes = 0;
                    self.last_rate.store(rate, Ordering::Relaxed);
                    Duration::ZERO
                } else if (self.files_per_sec > 0 && w.files >= self.files_per_sec)
                    || (self.bytes_per_sec > 0 && w.bytes >= self.bytes_per_sec)
                {
                    Duration::from_secs(1) - elapsed
                } else {
                    w.files += 1;
                    w.bytes += size;
                    Duration::ZERO
                }
            };
            if wait.is_zero() {
                return;
            }
            std::thread::sleep(wait.min(Duration::from_millis(50)));
        }
    }

    /// 最近一个窗口内的实测读取速率（字节/秒），用于进度上报。
    pub fn observed_bytes_per_sec(&self) -> u64 {
        self.last_rate.load(Ordering::Relaxed)
    }

    pub fn throttle_name(&self) -> String {
        match (self.files_per_sec, self.bytes_per_sec) {
            (0, 0) => "off".to_string(),
            (80, _) => "low".to_string(),
            (240, _) => "medium".to_string(),
            _ => "high".to_string(),
        }
    }

    /// 采样 RSS 并更新降级档位。
    pub fn refresh_level(&self) -> u8 {
        let rss = crate::service::rss_bytes();
        let level = if self.budget == 0 {
            0
        } else if rss >= self.budget {
            2
        } else if rss.saturating_mul(4) >= self.budget.saturating_mul(3) {
            1
        } else {
            0
        };
        self.level.store(level, Ordering::Relaxed);
        level
    }

    pub fn level(&self) -> u8 {
        self.level.load(Ordering::Relaxed)
    }

    /// 手工设置档位（测试与未来「手动降级」入口使用）。
    pub fn set_level(&self, level: u8) {
        self.level.store(level.min(2), Ordering::Relaxed);
    }

    /// 当前档位下允许的正文上限；`None` 表示本轮只索引元数据。
    pub fn content_cap(&self, configured: usize) -> Option<usize> {
        match self.level() {
            0 => Some(configured),
            1 => Some(configured.min(LEVEL1_CONTENT_CAP)),
            _ => None,
        }
    }

    pub fn level_name(&self) -> &'static str {
        match self.level() {
            0 => "normal",
            1 => "content-capped",
            _ => "metadata-only",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_throttle_paces_files() {
        let gov = Governor::new("low", 512 * 1024 * 1024);
        let start = Instant::now();
        // 超过 low 档 80 文件/秒的配额，必然产生等待
        for _ in 0..100 {
            gov.before_file(16);
        }
        assert!(start.elapsed() >= Duration::from_millis(150));
    }

    #[test]
    fn off_throttle_never_waits() {
        let gov = Governor::new("off", 0);
        let start = Instant::now();
        for _ in 0..500 {
            gov.before_file(4096);
        }
        assert!(start.elapsed() < Duration::from_millis(100));
        assert_eq!(gov.level(), 0);
        assert_eq!(gov.content_cap(1024), Some(1024));
    }

    #[test]
    fn budget_zero_keeps_normal_level() {
        let gov = Governor::new("off", 0);
        assert_eq!(gov.refresh_level(), 0);
        assert_eq!(gov.level_name(), "normal");
    }
}
