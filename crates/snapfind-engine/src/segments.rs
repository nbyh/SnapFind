//! 自研分段内容存储（文档 §6.3 `segments/`）。
//!
//! 设计要点：
//! * 正文不再存进 SQLite（`docs.content` 与 `*_tok` 列全部去掉），FTS 用 **contentless** 表，
//!   SQLite 只保留元数据与倒排，避免同一份文本被存 3–4 次；
//! * 正文按文档为块、zstd 压缩后追加写入 `segments/seg-<n>.seg`，单段上限 96 MB，
//!   超出后自动轮转到下一段，便于增量写入与后续整理；
//! * 块定位信息（段号/偏移/压缩长度/原始长度）落在 `content_blocks` 表，读取时按需解压，
//!   只给当前结果窗口的文档解压，不整卷加载；
//! * 更新是追加写：旧块成为垃圾字节，由 `Store::compact` 在扫描结束或用户点“整理”时回收。

use std::collections::{BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 单段文件上限：超过后轮转（文档 §6.3 建议 64–128 MB）。
pub const SEG_MAX_BYTES: u64 = 96 * 1024 * 1024;
/// zstd 压缩级别：3 在速度/体积间取平衡（百万级文件仍在后台低优先级下可接受）。
pub const ZSTD_LEVEL: i32 = 3;
/// 帧头长度：raw_len(u32) + comp_len(u32)。
const FRAME_HEADER: u64 = 8;

/// 一个内容块的物理位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRef {
    /// 段号；`u32::MAX` 表示空内容（不落盘）。
    pub seg: u32,
    pub off: u64,
    /// 帧总长（含 8 字节头）。
    pub len: u32,
    pub raw_len: u32,
}

impl BlockRef {
    pub const NONE: BlockRef = BlockRef {
        seg: u32::MAX,
        off: 0,
        len: 0,
        raw_len: 0,
    };

    pub fn is_none(&self) -> bool {
        self.seg == u32::MAX
    }
}

struct Writer {
    seg: u32,
    file: File,
    size: u64,
}

/// 卷级分段存储；读写均可从 `&self` 发起（内部加锁），便于与 `&VolumeStore` 共用。
///
/// 段长度在内存中按段累计（Windows 上写入句柄打开期间，目录项长度不刷新，
/// 直接 stat 会得到 0），仅在打开时用目录列表校准一次。
pub struct SegmentStore {
    dir: PathBuf,
    writer: Mutex<Writer>,
    readers: Mutex<HashMap<u32, File>>,
    sizes: Mutex<HashMap<u32, u64>>,
    dead_bytes: Mutex<u64>,
}

impl SegmentStore {
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let seg = Self::max_seg_id(dir)? + 1;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(seg_path(dir, seg))?;
        let mut sizes = HashMap::new();
        for (id, len) in Self::list(dir)? {
            sizes.insert(id, len);
        }
        let size = sizes.get(&seg).copied().unwrap_or(0);
        sizes.insert(seg, size);
        Ok(Self {
            dir: dir.to_path_buf(),
            writer: Mutex::new(Writer { seg, file, size }),
            readers: Mutex::new(HashMap::new()),
            sizes: Mutex::new(sizes),
            dead_bytes: Mutex::new(0),
        })
    }

    fn list(dir: &Path) -> std::io::Result<Vec<(u32, u64)>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)?.flatten() {
            if let Some(id) = parse_seg_name(&entry.file_name().to_string_lossy()) {
                let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                out.push((id, len));
            }
        }
        Ok(out)
    }

    fn max_seg_id(dir: &Path) -> std::io::Result<u32> {
        let mut max = 0u32;
        for entry in std::fs::read_dir(dir)?.flatten() {
            if let Some(id) = parse_seg_name(&entry.file_name().to_string_lossy()) {
                max = max.max(id);
            }
        }
        Ok(max)
    }

    /// 追加一块正文，返回其物理位置。
    pub fn append(&self, data: &[u8]) -> std::io::Result<BlockRef> {
        if data.is_empty() {
            return Ok(BlockRef::NONE);
        }
        let compressed = zstd::bulk::compress(data, ZSTD_LEVEL)?;
        let mut w = self.writer.lock().unwrap();
        if w.size > SEG_MAX_BYTES {
            let next = w.seg + 1;
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(seg_path(&self.dir, next))?;
            *w = Writer {
                seg: next,
                file,
                size: 0,
            };
        }
        let off = w.size;
        let mut frame = Vec::with_capacity(FRAME_HEADER as usize + compressed.len());
        frame.extend_from_slice(&(data.len() as u32).to_le_bytes());
        frame.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        frame.extend_from_slice(&compressed);
        w.file.write_all(&frame)?;
        w.size += frame.len() as u64;
        self.sizes.lock().unwrap().insert(w.seg, w.size);
        Ok(BlockRef {
            seg: w.seg,
            off,
            len: frame.len() as u32,
            raw_len: data.len() as u32,
        })
    }

    /// 按需解压读取一块正文（读取句柄按段缓存）。
    pub fn read(&self, block: &BlockRef) -> std::io::Result<Vec<u8>> {
        if block.is_none() {
            return Ok(Vec::new());
        }
        let mut cache = self.readers.lock().unwrap();
        if !cache.contains_key(&block.seg) {
            let f = File::open(seg_path(&self.dir, block.seg))?;
            cache.insert(block.seg, f);
        }
        let f = cache.get_mut(&block.seg).expect("segment handle");
        let mut head = [0u8; FRAME_HEADER as usize];
        f.seek(SeekFrom::Start(block.off))?;
        f.read_exact(&mut head)?;
        let raw_len = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let comp_len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
        let mut buf = vec![0u8; comp_len];
        f.read_exact(&mut buf)?;
        zstd::bulk::decompress(&buf, raw_len)
    }

    /// 强制切换到新段：整理（compaction）前调用，保证旧段可整体删除。
    pub fn rotate(&self) -> std::io::Result<u32> {
        let mut w = self.writer.lock().unwrap();
        let next = Self::max_seg_id(&self.dir)?.max(w.seg) + 1;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(seg_path(&self.dir, next))?;
        let id = next;
        self.sizes.lock().unwrap().insert(id, 0);
        *w = Writer {
            seg: next,
            file,
            size: 0,
        };
        Ok(id)
    }

    pub fn segment_ids(&self) -> std::io::Result<Vec<u32>> {
        let mut ids = BTreeSet::new();
        for entry in std::fs::read_dir(&self.dir)?.flatten() {
            if let Some(id) = parse_seg_name(&entry.file_name().to_string_lossy()) {
                ids.insert(id);
            }
        }
        Ok(ids.into_iter().collect())
    }

    /// 删除指定段（先丢弃读取句柄，避免 Windows 上的占用）。
    pub fn remove_segments(&self, ids: &[u32]) -> std::io::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        {
            let mut cache = self.readers.lock().unwrap();
            for id in ids {
                cache.remove(id);
            }
        }
        for id in ids {
            let _ = std::fs::remove_file(seg_path(&self.dir, *id));
        }
        {
            let mut sizes = self.sizes.lock().unwrap();
            for id in ids {
                sizes.remove(id);
            }
        }
        Ok(())
    }

    /// 各段逻辑长度之和（不依赖打开中文件的目录项长度）。
    pub fn total_bytes(&self) -> u64 {
        self.sizes.lock().unwrap().values().sum()
    }

    /// 记录因文档更新而废弃的字节（用于判断是否值得整理）。
    pub fn mark_dead(&self, bytes: u64) {
        let mut d = self.dead_bytes.lock().unwrap();
        *d += bytes;
    }

    pub fn dead_bytes(&self) -> u64 {
        *self.dead_bytes.lock().unwrap()
    }

    pub fn reset_dead(&self) {
        *self.dead_bytes.lock().unwrap() = 0;
    }
}

fn seg_path(dir: &Path, id: u32) -> PathBuf {
    dir.join(format!("seg-{id:05}.seg"))
}

fn parse_seg_name(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("seg-")?.strip_suffix(".seg")?;
    rest.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_read_roundtrip() {
        let dir = std::env::temp_dir().join(format!("snapfind-seg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = SegmentStore::open(&dir).unwrap();
        let text = "hello 世界 ".repeat(200);
        let b = store.append(text.as_bytes()).unwrap();
        assert!(!b.is_none());
        let back = store.read(&b).unwrap();
        assert_eq!(String::from_utf8(back).unwrap(), text);
        // 压缩后应小于原文
        assert!(store.total_bytes() < text.len() as u64);
        let empty = store.append(b"").unwrap();
        assert!(empty.is_none());
        assert!(store.read(&empty).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_removes_old_segments() {
        let dir = std::env::temp_dir().join(format!("snapfind-seg-rot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = SegmentStore::open(&dir).unwrap();
        let a = store.append(b"aaaa").unwrap();
        let new_id = store.rotate().unwrap();
        let b = store.append(b"bbbb").unwrap();
        assert_ne!(a.seg, b.seg);
        assert!(new_id > a.seg);
        store.remove_segments(&[a.seg]).unwrap();
        assert!(store.read(&b).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
