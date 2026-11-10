//! 目录遍历、排除规则与单文件索引（文档 §5.4、§5.9）。

use crate::extract::{self, ExtractError, Limits};
use crate::governor::{Governor, FLAG_METADATA_ONLY};
use crate::store::{DocRecord, VolumeStore};
use crate::tokenize;
use globset::{Glob, GlobSet, GlobSetBuilder};
use snapfind_core::config::AppConfig;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOutcome {
    Indexed,
    Unchanged,
    Unsupported,
    Failed,
}

pub struct ExcludeSet {
    set: Option<GlobSet>,
    /// 路径片段（小写）：路径中任意位置包含即排除。
    fragments: Vec<String>,
    /// 扩展名（小写、不含点）：命中即排除。
    exts: Vec<String>,
}

impl ExcludeSet {
    pub fn new(patterns: &[String]) -> Self {
        let mut builder = GlobSetBuilder::new();
        let mut any = false;
        for raw in patterns {
            let normalized = raw.replace('\\', "/");
            let mut gb = globset::GlobBuilder::new(&normalized);
            gb.literal_separator(false);
            #[cfg(windows)]
            gb.case_insensitive(true);
            if let Ok(g) = gb.build() {
                builder.add(g);
                any = true;
            }
            // 目录形式（无通配符）追加 `/**`
            if !normalized.contains('*') && !normalized.contains('?') {
                if let Ok(g) = Glob::new(&format!("{}/**", normalized.trim_end_matches('/'))) {
                    builder.add(g);
                    any = true;
                }
            }
        }
        Self {
            set: if any { builder.build().ok() } else { None },
            fragments: Vec::new(),
            exts: Vec::new(),
        }
    }

    /// 追加「路径中包含即排除」的片段（目录名 / 路径片段，大小写不敏感）。
    pub fn with_path_fragments(mut self, fragments: &[String]) -> Self {
        self.fragments = fragments
            .iter()
            .map(|s| s.trim().replace('\\', "/").to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        self
    }

    /// 追加「按扩展名排除」的类型（不含点，大小写不敏感）。
    pub fn with_extensions(mut self, exts: &[String]) -> Self {
        self.exts = exts
            .iter()
            .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        self
    }

    pub fn is_excluded(&self, path: &str) -> bool {
        let normalized = path.replace('\\', "/");
        let lower = normalized.to_ascii_lowercase();
        // 1) 路径片段：任意位置包含即整棵子树跳过（覆盖任意盘符的回收站等）
        if self.fragments.iter().any(|f| lower.contains(f)) {
            return true;
        }
        // 2) 扩展名
        if !self.exts.is_empty() {
            let file = normalized.rsplit('/').next().unwrap_or("");
            if let Some((_, ext)) = file.rsplit_once('.') {
                let ext = ext.to_ascii_lowercase();
                if self.exts.iter().any(|e| e == &ext) {
                    return true;
                }
            }
        }
        // 3) 原有 glob 规则
        match &self.set {
            Some(set) => set.is_match(&normalized),
            None => false,
        }
    }
}

/// 迭代式深度优先遍历（避免递归爆栈），目录被排除时整棵子树剪枝。
pub fn walk(
    root: &Path,
    excludes: &ExcludeSet,
    follow_symlinks: bool,
    mut on_file: impl FnMut(&Path, u64, i64),
    mut on_dir: impl FnMut(&Path),
) {
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if excludes.is_excluded(&dir.to_string_lossy()) {
            continue;
        }
        on_dir(&dir);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue, // 权限不足：跳过（由调用方计数）
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_symlink() && !follow_symlinks {
                continue;
            }
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            if excludes.is_excluded(&path.to_string_lossy()) {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            on_file(&path, meta.len(), mtime);
        }
    }
}

fn parent_string(path: &Path) -> String {
    path.parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn name_string(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 索引单个文件：未变更（mtime+size 相同）则跳过；抽取失败仍写入文件名/路径。
///
/// 组合入口（旧语义）：变更检测 → 准备 → 写库。适合 watch 单文件变更与测试；
/// 全量扫描请用 `doc_stat` 变更检测 + `prepare_file` + `apply_prepared` 两段式，
/// 让节流睡眠与内容抽取在锁外进行（见 `prepare_file` 的说明）。
pub fn index_file(
    vol: &VolumeStore,
    path: &Path,
    size: u64,
    mtime: i64,
    cfg: &AppConfig,
    gov: &Governor,
) -> FileOutcome {
    index_file_paced(vol, path, size, mtime, cfg, gov, true)
}

/// `pace_throttle=false` 时跳过 IO 节流：watch 增量的小批次使用（用户新建的文件
/// 应秒级入库，而不是在共享令牌桶里等恢复扫描把配额让出来）。
pub fn index_file_paced(
    vol: &VolumeStore,
    path: &Path,
    size: u64,
    mtime: i64,
    cfg: &AppConfig,
    gov: &Governor,
    pace_throttle: bool,
) -> FileOutcome {
    let path_str = path.to_string_lossy().to_string();
    if let Ok(Some(stat)) = vol.doc_stat(&path_str) {
        if stat.size == size && stat.mtime == mtime {
            return FileOutcome::Unchanged;
        }
    }
    match prepare_file_paced(path, size, mtime, cfg, gov, pace_throttle) {
        Some((doc, outcome)) => apply_prepared(vol, &doc, outcome),
        None => FileOutcome::Failed,
    }
}

/// 把准备好的文档写入库。持锁时间仅为单条 upsert（毫秒级）。
pub fn apply_prepared(vol: &VolumeStore, doc: &DocRecord, outcome: FileOutcome) -> FileOutcome {
    match vol.upsert_doc(doc) {
        Ok(_) => outcome,
        Err(_) => FileOutcome::Failed,
    }
}

/// 扫描准备阶段（**不持有 store 锁**）：IO 节流、内容抽取、分词。
///
/// 抽取与节流睡眠可能耗时数百毫秒甚至更久（受内存压力降级影响）；
/// 调用方必须在锁内先做毫秒级的 `doc_stat` 变更检测，然后**解锁**执行本函数，
/// 最后再短暂持锁 `apply_prepared`。否则扫描线程持锁做慢活，搜索查询会被
/// 长期阻塞——这正是「索引进行中搜索一直转圈」的根因。
///
/// 返回 `None` 表示文件已不存在。
pub fn prepare_file(
    path: &Path,
    size: u64,
    mtime: i64,
    cfg: &AppConfig,
    gov: &Governor,
) -> Option<(DocRecord, FileOutcome)> {
    prepare_file_paced(path, size, mtime, cfg, gov, true)
}

/// `pace_throttle=false`：跳过 IO 节流（watch 增量小批次专用，见 `index_file_paced`）。
pub fn prepare_file_paced(
    path: &Path,
    size: u64,
    mtime: i64,
    cfg: &AppConfig,
    gov: &Governor,
    pace_throttle: bool,
) -> Option<(DocRecord, FileOutcome)> {
    let path_str = path.to_string_lossy().to_string();
    if !path.is_file() {
        return None;
    }
    // IO 节流：超出配额时在此睡眠，保证索引不抢前台磁盘
    if pace_throttle {
        gov.before_file(size);
    }

    let ext = extract::ext_of(path);
    let name = name_string(path);

    let (content, encoding, flags, outcome) = match gov.content_cap(cfg.index.max_text_bytes_per_file)
    {
        None => {
            // L2 降级：只索引文件名/路径，正文留待 RSS 回落后重建
            (String::new(), String::new(), FLAG_METADATA_ONLY, FileOutcome::Indexed)
        }
        Some(cap) => {
            let limits = Limits {
                max_bytes: cfg.extract.max_bytes_per_file,
                max_text_bytes: cap,
                timeout_ms: cfg.extract.timeout_ms,
            };
            // 第三方抽取库会 panic：pdf-extract 遇到非 Identity-H 的 CID 字体
            // 直接 assert 失败。panic 会杀死整条扫描线程，让整个卷无声停在
            // 「建立索引 x/x」上不再前进，所以必须兜住——按抽取失败处理，
            // 该文件只索引文件名/路径，其余文件继续。
            let extracted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                extract::extract(path, &limits)
            }))
            .unwrap_or_else(|_| Err(ExtractError::Failed("抽取器内部崩溃".into())));
            match extracted {
                Ok(et) => {
                    let mut flags = 0i64;
                    if et.truncated {
                        flags |= 1;
                    }
                    (et.text, et.encoding, flags, FileOutcome::Indexed)
                }
                Err(ExtractError::Unsupported) => {
                    (String::new(), String::new(), 0, FileOutcome::Unsupported)
                }
                Err(ExtractError::Encrypted) => {
                    (String::new(), String::new(), 2, FileOutcome::Failed)
                }
                Err(ExtractError::TooLarge) => (String::new(), String::new(), 1, FileOutcome::Failed),
                Err(ExtractError::Failed(_)) => {
                    (String::new(), String::new(), 0, FileOutcome::Failed)
                }
            }
        }
    };

    let doc = DocRecord {
        path: path_str.clone(),
        parent: parent_string(path),
        name_tok: tokenize::token_string(&name),
        path_tok: tokenize::token_string(&path_str),
        content_tok: tokenize::token_string(&content),
        name,
        ext,
        size,
        mtime,
        encoding,
        flags,
        content,
    };
    Some((doc, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_match_both_separators() {
        let set = ExcludeSet::new(&["**/node_modules/**".into()]);
        assert!(set.is_excluded("C:\\proj\\node_modules\\a\\b.js"));
        assert!(set.is_excluded("/home/u/proj/node_modules/a.js"));
        assert!(!set.is_excluded("/home/u/proj/src/a.js"));
    }

    #[test]
    fn excludes_directory_prefix() {
        let set = ExcludeSet::new(&["C:\\Windows".into()]);
        assert!(set.is_excluded("C:\\Windows\\System32\\a.dll"));
    }

    #[test]
    fn excludes_recycle_bin_on_any_drive_by_fragment() {
        let set = ExcludeSet::new(&[]).with_path_fragments(&["$Recycle.Bin".into()]);
        assert!(set.is_excluded("D:\\$Recycle.Bin\\S-1-5-21\\$Iabc.txt"));
        assert!(set.is_excluded("/mnt/d/$Recycle.Bin/x/y.docx"));
        assert!(!set.is_excluded("D:\\docs\\report.docx"));
    }

    #[test]
    fn excludes_by_extension_case_insensitively() {
        let set = ExcludeSet::new(&[]).with_extensions(&[".TMP".into(), "bak".into()]);
        assert!(set.is_excluded("C:\\work\\a.TMP"));
        assert!(set.is_excluded("/home/u/a.bak"));
        assert!(!set.is_excluded("/home/u/a.txt"));
    }
}
