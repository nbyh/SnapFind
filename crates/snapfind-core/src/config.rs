//! 配置与本地目录解析（对应文档 §9.1、§18.1）。
//!
//! 所有配置项均本地保存，不含任何凭据，不含任何网络设置。

use crate::platform;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 当前配置结构版本。v2 移除了 11 个「从未被读取」的字段
/// （`index.batch_files`、`index.store_positions`、`extract.on_too_large`、
/// `extract.ocr_enabled`、`query.first_batch_hint_ms`、`query.regex_enabled`、
/// `query.fuzzy_enabled`、`query.allow_leading_wildcard`、`query.max_results`、
/// `ui.history_enabled`、`ui.history_max_entries`）。
/// 旧文件里的这些键会被 serde 直接忽略，不影响启动。
pub const CONFIG_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub version: u32,
    pub index: IndexCfg,
    pub extract: ExtractCfg,
    pub query: QueryCfg,
    pub ui: UiCfg,
    pub lifecycle: LifecycleCfg,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexCfg {
    /// 空表示按 §5.5 自动选择（程序目录 → 平台用户目录）。
    pub root: String,
    /// 同时索引的卷数。`0` = auto（默认：每个卷一个独立线程，全部卷**同时开始**）；
    /// 手动范围 1–8（用于限流，例如机械硬盘想减少随机 IO 争用）。
    pub parallel_volumes: u32,
    /// 每日定时增量索引：到点后对已启用的范围做一次扫描（未变更文件只 stat，不重复抽取）。
    pub daily_scan: bool,
    /// 定时触发时刻，**本地时间** `HH:MM`。
    pub daily_scan_at: String,
    /// 应用/索引进程启动时自动继续未完成的索引（断点续跑）。
    pub resume_on_start: bool,
    pub memory_budget_bytes: u64,
    pub throttle: String,
    pub max_text_bytes_per_file: usize,
    pub checkpoint_every_files: u64,
    pub checkpoint_every_ms: u64,
    pub exclude_rules: Vec<String>,
    /// 排除路径：目录名 / 路径片段，每行一条。路径中「包含」即跳过（大小写不敏感）。
    /// 默认覆盖任意盘符的回收站与系统卷信息目录。
    pub exclude_paths: Vec<String>,
    /// 排除文件类型：扩展名（不含点），命中即不建立索引。
    pub exclude_types: Vec<String>,
    pub follow_symlinks: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtractCfg {
    pub max_bytes_per_file: usize,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct QueryCfg {
    pub timeout_ms: u64,
    pub batch_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiCfg {
    pub debounce_ms: u64,
    /// 主题：`system`（跟随系统，默认）/ `light` / `dark`。
    pub theme: String,
    pub tray_enabled: bool,
    /// 关闭主窗口时最小化到托盘（需平台支持托盘）。
    pub close_to_tray: bool,
    /// 开机自启（用户级启动项，带 `--minimized`）。
    pub autostart: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LifecycleCfg {
    pub shutdown_timeout_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub heartbeat_miss_threshold: u32,
}

/// 内置排除路径片段：任意盘符、路径中包含即跳过。
pub fn default_exclude_paths() -> Vec<String> {
    vec![
        "$Recycle.Bin".into(),
        "RECYCLER".into(),
        "System Volume Information".into(),
    ]
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            index: IndexCfg::default(),
            extract: ExtractCfg::default(),
            query: QueryCfg::default(),
            ui: UiCfg::default(),
            lifecycle: LifecycleCfg::default(),
        }
    }
}

impl Default for IndexCfg {
    fn default() -> Self {
        Self {
            root: String::new(),
            parallel_volumes: 0,
            daily_scan: false,
            daily_scan_at: "03:00".into(),
            resume_on_start: true,
            // 默认 1 GiB：预算是按整个 indexd 进程的 RSS 采样（含 sqlite 页缓存、
            // mmap、临时缓冲），512MB 在多卷并行扫描时几乎必然触发降级，
            // 导致大量文件只索引文件名/路径（正文搜不到）
            memory_budget_bytes: 1024 * 1024 * 1024,
            throttle: "low".into(),
            max_text_bytes_per_file: 2 * 1024 * 1024,
            checkpoint_every_files: 5000,
            checkpoint_every_ms: 30_000,
            exclude_rules: Vec::new(),
            exclude_paths: default_exclude_paths(),
            exclude_types: Vec::new(),
            follow_symlinks: false,
        }
    }
}

impl Default for ExtractCfg {
    fn default() -> Self {
        Self {
            max_bytes_per_file: 64 * 1024 * 1024,
            timeout_ms: 5_000,
        }
    }
}

impl Default for QueryCfg {
    fn default() -> Self {
        Self {
            timeout_ms: 2_000,
            batch_size: 50,
        }
    }
}

impl Default for UiCfg {
    fn default() -> Self {
        Self {
            debounce_ms: 120,
            theme: "system".into(),
            tray_enabled: true,
            close_to_tray: true,
            autostart: false,
        }
    }
}

impl Default for LifecycleCfg {
    fn default() -> Self {
        Self {
            shutdown_timeout_ms: 5_000,
            heartbeat_interval_ms: 5_000,
            heartbeat_miss_threshold: 3,
        }
    }
}

impl AppConfig {
    /// 配置文件路径：**便携优先**——存放在程序所在目录（`<exe 目录>/config.json`，
    /// 与 `index/` 索引目录同级）；程序目录不可写（如安装进 Program Files）时
    /// 回退用户目录（Windows `%APPDATA%\SnapFind`；Linux `$XDG_CONFIG_HOME/snapfind`）。
    pub fn path() -> PathBuf {
        let exe = platform::exe_dir();
        if platform::probe_writable(&exe).is_ok() {
            return exe.join("config.json");
        }
        let dir = platform::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        dir.join("config.json")
    }

    /// 读取配置，并返回「被重置/被升级」的原因，供前端提示用户。
    ///
    /// 以前解析失败会**静默**把文件改名成 `.bak` 再回默认值——用户只会发现
    /// 所有设置突然消失，且不知道发生了什么。现在把原因交回调用方。
    pub fn load_with_warning() -> (Self, Option<String>) {
        let path = Self::path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), None)
            }
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!("读取配置文件失败：{e}（本次使用默认设置）")),
                )
            }
        };
        match serde_json::from_str::<Self>(&raw) {
            Ok(cfg) => {
                let warning = (cfg.version < CONFIG_VERSION).then(|| {
                    format!(
                        "配置结构已从 v{} 升级到 v{}（移除无效配置项），下次保存后生效",
                        cfg.version, CONFIG_VERSION
                    )
                });
                (cfg, warning)
            }
            Err(e) => {
                let bak = path.with_extension("json.bak");
                let _ = std::fs::rename(&path, &bak);
                (
                    Self::default(),
                    Some(format!(
                        "配置文件损坏（{e}），已备份为 {} 并恢复默认设置",
                        bak.display()
                    )),
                )
            }
        }
    }

    /// 读取配置；文件缺失或损坏时回退默认值（损坏文件备份为 `.bak`）。
    pub fn load() -> Self {
        Self::load_with_warning().0
    }

    /// 原子写入（临时文件 + fsync + rename）。
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let data = serde_json::to_vec_pretty(self).unwrap_or_default();
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&data)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)
    }

    /// 同时运行的卷数。`0`（默认）= 全部目标卷同时开始：
    /// 每个卷本来就跑在独立线程里、写自己的 `vol-<id>/` 目录，
    /// 因此这里不再人为限制 2 个，卷越多越能"一起开工"。
    /// 实际吞吐仍受全局内存预算与 IO 节流档位约束。
    pub fn parallel_volumes(&self, volume_count: usize) -> usize {
        let n = if self.index.parallel_volumes == 0 {
            volume_count
        } else {
            self.index.parallel_volumes as usize
        };
        n.clamp(1, 8).min(volume_count.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 便携优先：程序目录可写时，配置与索引都应落在 exe 同目录。
    #[test]
    fn config_path_prefers_exe_dir() {
        let path = AppConfig::path();
        assert!(path.ends_with("config.json"));
        assert_eq!(
            path.parent(),
            Some(platform::exe_dir().as_path()),
            "程序目录可写时，config.json 必须保存在程序所在目录（便携模式）"
        );
    }

    /// auto 并行度 = 全部卷同时开始（不再人头上限 2），手动值受 1–8 约束。
    #[test]
    fn parallel_volumes_auto_covers_all_volumes() {
        let mut cfg = AppConfig::default();
        assert_eq!(cfg.parallel_volumes(1), 1);
        assert_eq!(cfg.parallel_volumes(3), 3);
        assert_eq!(cfg.parallel_volumes(6), 6);
        assert_eq!(cfg.parallel_volumes(16), 8, "硬上限 8");
        cfg.index.parallel_volumes = 2;
        assert_eq!(cfg.parallel_volumes(6), 2);
        assert_eq!(cfg.parallel_volumes(1), 1, "不得超过实际卷数");
        cfg.index.parallel_volumes = 99;
        assert_eq!(cfg.parallel_volumes(10), 8);
    }

    /// 保存后能原样读回，且文件落在便携路径上。
    #[test]
    fn config_save_and_reload_roundtrip() {
        let mut cfg = AppConfig::default();
        cfg.ui.close_to_tray = false;
        cfg.index.throttle = "medium".into();
        cfg.save().expect("保存配置");
        let path = AppConfig::path();
        assert!(path.exists(), "保存后配置应存在于便携路径");
        let re = AppConfig::load();
        assert!(!re.ui.close_to_tray);
        assert_eq!(re.index.throttle, "medium");
        let _ = std::fs::remove_file(path);
    }

    /// 旧 config.json 缺少新增的排除字段时，自动落到内置默认值（含回收站）。
    #[test]
    fn missing_exclusion_fields_fall_back_to_defaults() {
        let old = serde_json::json!({ "version": 1, "index": { "root": "" } });
        let cfg: AppConfig = serde_json::from_value(old).expect("旧配置应可反序列化");
        assert!(cfg
            .index
            .exclude_paths
            .iter()
            .any(|p| p.eq_ignore_ascii_case("$Recycle.Bin")));
        assert!(cfg.index.exclude_types.is_empty());
    }

    #[test]
    fn default_exclude_paths_cover_recycle_bin_and_system_volume() {
        let paths = default_exclude_paths();
        assert!(paths.iter().any(|p| p.eq_ignore_ascii_case("$Recycle.Bin")));
        assert!(paths
            .iter()
            .any(|p| p.eq_ignore_ascii_case("System Volume Information")));
    }

    /// v2 移除了 11 个从未被读取的字段：旧文件里的这些键必须被忽略，而不是解析失败。
    #[test]
    fn removed_v1_fields_are_ignored() {
        let old = serde_json::json!({
            "version": 1,
            "index": { "batch_files": 512, "store_positions": true, "root": "" },
            "extract": { "on_too_large": "truncate", "ocr_enabled": true },
            "query": {
                "max_results": 10000,
                "regex_enabled": true,
                "fuzzy_enabled": true,
                "allow_leading_wildcard": true,
                "first_batch_hint_ms": 150
            },
            "ui": { "history_enabled": true, "history_max_entries": 200 }
        });
        let cfg: AppConfig = serde_json::from_value(old).expect("含已移除字段的旧配置应可解析");
        assert_eq!(cfg.version, 1, "旧版本号保留，由 load_with_warning 提示升级");
        assert_eq!(cfg.index.checkpoint_every_files, 5000);
        assert!(cfg.ui.close_to_tray);

        let json = serde_json::to_value(AppConfig::default()).expect("序列化默认配置");
        assert_eq!(
            json["version"].as_u64().unwrap() as u32,
            CONFIG_VERSION,
            "默认配置必须写当前版本号"
        );
        let index = json["index"].as_object().unwrap();
        assert!(!index.contains_key("batch_files"));
        assert!(!index.contains_key("store_positions"));
        assert!(json["extract"].as_object().unwrap().get("ocr_enabled").is_none());
        assert!(json["query"].as_object().unwrap().get("max_results").is_none());
    }
}

/// 索引根目录解析结果。
#[derive(Debug, Clone)]
pub struct IndexRoot {
    /// 实际生效目录。
    pub path: PathBuf,
    /// 首选目录（<程序目录>/index）。
    pub preferred: PathBuf,
    /// 是否发生降级。
    pub degraded: bool,
    /// 降级原因（不可写原因码）。
    pub reason: Option<String>,
}

impl IndexRoot {
    pub fn meta_db(&self) -> PathBuf {
        self.path.join("meta.db")
    }
    pub fn lock_file(&self) -> PathBuf {
        self.path.join("lock")
    }
    pub fn tmp_dir(&self) -> PathBuf {
        self.path.join("tmp")
    }
    pub fn volume_dir(&self, volume_id: &str) -> PathBuf {
        self.path.join(volume_id)
    }
}

/// 按 §5.5 解析索引目录：首选 `<程序目录>/index`，不可写时降级并给出原因码。
pub fn resolve_index_root(configured: Option<&Path>) -> IndexRoot {
    if let Some(dir) = configured {
        if platform::probe_writable(dir).is_ok() {
            return IndexRoot {
                path: dir.to_path_buf(),
                preferred: dir.to_path_buf(),
                degraded: false,
                reason: None,
            };
        }
    }

    let preferred = platform::exe_dir().join("index");
    if platform::probe_writable(&preferred).is_ok() {
        return IndexRoot {
            path: preferred.clone(),
            preferred,
            degraded: false,
            reason: None,
        };
    }

    let reason = platform::probe_writable(&preferred).err();
    let fallback = platform::data_dir().join("index");
    let _ = std::fs::create_dir_all(&fallback);
    let second_reason = platform::probe_writable(&fallback).err();
    if second_reason.is_none() {
        return IndexRoot {
            path: fallback,
            preferred,
            degraded: true,
            reason,
        };
    }

    let cache = platform::cache_dir().join("index");
    let _ = std::fs::create_dir_all(&cache);
    IndexRoot {
        path: cache,
        preferred,
        degraded: true,
        reason: second_reason.or(reason),
    }
}
