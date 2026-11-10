//! 存储层：`meta.db`（全局元数据）+ 每卷 `docs.db`（元数据 + 倒排）+ `segments/`（压缩正文）。
//!
//! 体积口径（文档 §6.3、§9.3「百万级文件」）：
//! * `docs` 只存路径/名称/大小/时间等元数据，**不存正文、不存分词副本**；
//! * `fts` 使用 **contentless**（`content=''`，`contentless_delete=1`），只有倒排，无重复文本；
//! * 正文按块 zstd 压缩写入 `segments/seg-N.seg`，定位信息在 `content_blocks`；
//! * 更新是追加写，旧块由 `compact()` 回收，避免 WAL/页碎片随更新次数膨胀。
//!
//! 旧版（schema v1，正文+分词列存在 `docs` 里）在打开时自动重建为 v2 并触发重扫。

use crate::segments::{BlockRef, SegmentStore};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use snapfind_core::config::IndexRoot;
use snapfind_core::error::{ErrorCode, SnapError, SnapResult};
use snapfind_core::ipc::CheckpointInfo;
use snapfind_core::volume::VolumeInfo;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const BACKEND_NAME: &str = "sqlite-fts5+zstd-segments";
pub const TOKENIZER_ID: &str = "snapfind-bigram-v1";
/// 卷内 schema 版本；与 `docs.db` 的 `PRAGMA user_version` 不一致时重建。
pub const SCHEMA_VERSION: i64 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VolumeState {
    pub state: String,
    pub phase: String,
    pub files_total: u64,
    pub files_done: u64,
    pub bytes_done: u64,
    pub dirs_done: u64,
    pub files_failed: u64,
    /// 子树进度：根目录一级子树的完成数/总数（真实百分比；
    /// 扫描完成进入 Watching 时两者相等 = 100%）。
    pub subs_done: u64,
    pub subs_total: u64,
    pub checkpoint: Option<CheckpointInfo>,
    pub last_error: Option<String>,
    /// 已完成的一级子树（绝对路径）。扫描中断后据此继续，扫描完成时清空。
    pub done_subtrees: Vec<String>,
}

impl Default for VolumeState {
    fn default() -> Self {
        Self {
            state: "Discovered".into(),
            phase: "idle".into(),
            files_total: 0,
            files_done: 0,
            bytes_done: 0,
            dirs_done: 0,
            files_failed: 0,
            subs_done: 0,
            subs_total: 0,
            checkpoint: None,
            last_error: None,
            done_subtrees: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DocRecord {
    pub path: String,
    pub parent: String,
    pub name: String,
    pub ext: String,
    pub size: u64,
    pub mtime: i64,
    pub encoding: String,
    pub flags: i64,
    pub content: String,
    pub name_tok: String,
    pub path_tok: String,
    pub content_tok: String,
}

#[derive(Debug, Clone, Copy)]
pub struct DocStat {
    pub doc_id: i64,
    pub size: u64,
    pub mtime: i64,
    /// 原始 flags（用于识别「仅元数据」文档，恢复 L0 后需要补抽正文）
    pub flags: i64,
}

pub struct VolumeStore {
    pub info: VolumeInfo,
    pub conn: Connection,
    pub state: VolumeState,
    pub dir: PathBuf,
    /// 压缩正文分段存储。
    pub segments: SegmentStore,
    /// 是否已完成首次全量扫描（决定增量语义）。
    pub dirty: bool,
    /// 批量写事务状态：连续 `upsert_doc` 共用一个事务，摊薄 autocommit 开销。
    tx_open: std::cell::Cell<bool>,
    tx_count: std::cell::Cell<u32>,
}

/// 批量写事务的提交间隔（文档数）。崩溃时最多丢失最后一批，断点续跑可覆盖。
const BATCH_COMMIT_DOCS: u32 = 512;

pub struct Store {
    pub root: IndexRoot,
    pub meta: Connection,
    pub volumes: HashMap<String, VolumeStore>,
}

impl Store {
    pub fn open(root: &IndexRoot) -> SnapResult<Self> {
        std::fs::create_dir_all(&root.path)?;
        std::fs::create_dir_all(root.tmp_dir())?;
        let meta = Connection::open(root.meta_db())?;
        meta.pragma_update(None, "journal_mode", "WAL")?;
        meta.pragma_update(None, "synchronous", "NORMAL")?;
        meta.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS volumes (
                id TEXT PRIMARY KEY,
                mount TEXT NOT NULL,
                fs TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                state TEXT NOT NULL DEFAULT 'Discovered',
                created_at INTEGER NOT NULL,
                last_seen INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                volume TEXT,
                kind TEXT NOT NULL,
                detail TEXT
            );
            CREATE TABLE IF NOT EXISTS search_history (
                q TEXT PRIMARY KEY,
                ts INTEGER NOT NULL,
                hits INTEGER NOT NULL DEFAULT 0
            );
            "#,
        )?;

        // buildId 校验：禁止新旧二进制混跑（文档 §4.1）
        let stored_build: Option<String> = meta
            .query_row("SELECT v FROM meta WHERE k='build_id'", [], |r| r.get(0))
            .optional()?;
        match stored_build {
            Some(prev) if prev != snapfind_core::BUILD_ID => {
                // 同版本内的小改动允许继续使用；仅在 schema 不兼容时拒绝。
                let schema: String = meta
                    .query_row("SELECT v FROM meta WHERE k='schema_version'", [], |r| r.get(0))
                    .optional()?
                    .unwrap_or_else(|| "1".into());
                // v1 → v2 属于可自动重建的升级，只有未知版本才拒绝
                if schema != "1" && schema != SCHEMA_VERSION.to_string() {
                    return Err(SnapError::new(
                        ErrorCode::BuildMismatch,
                        format!("索引由其他版本创建（{prev}），请清理或重建"),
                    ));
                }
            }
            _ => {}
        }
        for (k, v) in [
            ("build_id", snapfind_core::BUILD_ID),
            ("schema_version", "2"),
            ("tokenizer_id", TOKENIZER_ID),
            ("backend", BACKEND_NAME),
        ] {
            meta.execute(
                "INSERT INTO meta(k,v) VALUES(?1,?2)
                 ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                params![k, v],
            )?;
        }

        Ok(Self {
            root: root.clone(),
            meta,
            volumes: HashMap::new(),
        })
    }

    pub fn log_event(&self, volume: Option<&str>, kind: &str, detail: &str) {
        let _ = self.meta.execute(
            "INSERT INTO events(ts, volume, kind, detail) VALUES(?1,?2,?3,?4)",
            params![snapfind_core::ipc::now_ms(), volume, kind, detail],
        );
    }

    /// 注册/更新卷信息（不打开卷数据库）。
    pub fn register_volume(&mut self, info: &VolumeInfo) -> SnapResult<()> {
        let now = snapfind_core::ipc::now_ms();
        self.meta.execute(
            "INSERT INTO volumes(id,mount,fs,enabled,state,created_at,last_seen)
             VALUES(?1,?2,?3,?4,'Discovered',?5,?5)
             ON CONFLICT(id) DO UPDATE SET mount=excluded.mount, fs=excluded.fs,
                 enabled=excluded.enabled, last_seen=excluded.last_seen",
            params![
                info.volume_id,
                info.mount,
                info.fs,
                if info.enabled { 1 } else { 0 },
                now
            ],
        )?;
        Ok(())
    }

    /// 从 meta.db 读取已登记的卷（含用户手工指定的目录范围）。
    pub fn load_volumes(&self) -> SnapResult<Vec<VolumeInfo>> {
        let mut stmt = self
            .meta
            .prepare("SELECT id, mount, fs, enabled FROM volumes ORDER BY mount")?;
        let rows = stmt.query_map([], |r| {
            Ok(VolumeInfo {
                volume_id: r.get(0)?,
                mount: r.get(1)?,
                fs: r.get(2)?,
                total_bytes: 0,
                free_bytes: 0,
                removable: false,
                network: false,
                enabled: r.get::<_, i64>(3)? != 0,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn volume_state_in_meta(&self, volume_id: &str) -> SnapResult<String> {
        let s: Option<String> = self
            .meta
            .query_row("SELECT state FROM volumes WHERE id=?1", [volume_id], |r| r.get(0))
            .optional()?;
        Ok(s.unwrap_or_else(|| "Discovered".into()))
    }

    pub fn set_volume_state_in_meta(&self, volume_id: &str, state: &str) -> SnapResult<()> {
        self.meta.execute(
            "UPDATE volumes SET state=?2, last_seen=?3 WHERE id=?1",
            params![volume_id, state, snapfind_core::ipc::now_ms()],
        )?;
        Ok(())
    }

    /// 从 meta 中删除卷记录（仅「移除自定义目录」时使用；
    /// 磁盘卷由扫描自动重新登记，删行没有意义）。
    pub fn delete_volume_in_meta(&self, volume_id: &str) -> SnapResult<()> {
        self.meta
            .execute("DELETE FROM volumes WHERE id=?1", [volume_id])?;
        Ok(())
    }

    /// 打开（或复用）卷存储，创建 `vol-<id>/` 目录结构与数据库。
    pub fn volume_mut(&mut self, info: &VolumeInfo) -> SnapResult<&mut VolumeStore> {
        let id = info.volume_id.clone();
        if !self.volumes.contains_key(&id) {
            let dir = self.root.volume_dir(&id);
            std::fs::create_dir_all(dir.join("segments"))?;
            std::fs::create_dir_all(dir.join("hot"))?;
            std::fs::create_dir_all(dir.join("tmp"))?;
            let conn = Connection::open(dir.join("docs.db"))?;
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
            conn.pragma_update(None, "cache_size", -8192)?;
            conn.pragma_update(None, "temp_store", "MEMORY")?;
            // schema 版本检查：v1（正文+分词列在 docs 里）自动清空重建，随后由扫描重新填充
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap_or(0);
            if version != SCHEMA_VERSION {
                let _ = conn.execute_batch(
                    "DROP TABLE IF EXISTS fts; DROP TABLE IF EXISTS docs;
                     DROP TABLE IF EXISTS content_blocks;",
                );
                let _ = std::fs::remove_dir_all(dir.join("segments"));
                std::fs::create_dir_all(dir.join("segments"))?;
            }
            create_schema(&conn)?;
            let segments = SegmentStore::open(&dir.join("segments"))?;
            let state = load_state(&dir).unwrap_or_default();
            self.volumes.insert(
                id.clone(),
                VolumeStore {
                    info: info.clone(),
                    conn,
                    state,
                    dir,
                    segments,
                    dirty: false,
                    tx_open: std::cell::Cell::new(false),
                    tx_count: std::cell::Cell::new(0),
                },
            );
        }
        self.volumes
            .get_mut(&id)
            .ok_or_else(|| SnapError::new(ErrorCode::Internal, "volume not found"))
    }

    pub fn volume(&self, id: &str) -> Option<&VolumeStore> {
        self.volumes.get(id)
    }

    pub fn volume_ids(&self) -> Vec<String> {
        self.volumes.keys().cloned().collect()
    }

    /// 提交所有卷未落库的批量写事务（查询、统计与检查点前调用）。
    pub fn commit_all(&mut self) {
        for v in self.volumes.values() {
            let _ = v.commit_batch();
        }
    }

    pub fn total_docs(&self) -> u64 {
        self.volumes
            .values()
            .map(|v| v.count_docs().unwrap_or(0))
            .sum()
    }
}

impl VolumeStore {
    pub fn save_state(&self) -> std::io::Result<()> {
        let path = self.dir.join("state.json");
        let tmp = self.dir.join("state.json.tmp");
        let data = serde_json::to_vec_pretty(&self.state).unwrap_or_default();
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&data)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)
    }

    pub fn count_docs(&self) -> SnapResult<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM docs", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// 本卷出现过最多的扩展名（`ext` 已小写、不含点）。
    pub fn ext_counts(&self, limit: usize) -> SnapResult<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT ext, COUNT(*) FROM docs WHERE ext <> '' GROUP BY ext ORDER BY 2 DESC, 1 ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn doc_stat(&self, path: &str) -> SnapResult<Option<DocStat>> {
        let row = self
            .conn
            .query_row(
                "SELECT doc_id, size, mtime, flags FROM docs WHERE path=?1",
                [path],
                |r| {
                    Ok(DocStat {
                        doc_id: r.get(0)?,
                        size: r.get::<_, i64>(1)? as u64,
                        mtime: r.get(2)?,
                        flags: r.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// 写入/更新一篇文档：正文压缩进分段，元数据进 `docs`，倒排进 contentless `fts`。
    ///
    /// 连续写入共用一个批量事务（每 `BATCH_COMMIT_DOCS` 篇提交一次），
    /// 避免每条 SQL 各自 autocommit 的事务开销；调用方在检查点/查询前调用 `commit_batch`。
    pub fn upsert_doc(&self, doc: &DocRecord) -> SnapResult<i64> {
        if !self.tx_open.get() {
            self.conn.execute_batch("BEGIN")?;
            self.tx_open.set(true);
        }
        let existing: Option<i64> = self
            .conn
            .query_row("SELECT doc_id FROM docs WHERE path=?1", [&doc.path], |r| {
                r.get(0)
            })
            .optional()?;
        let block = self.segments.append(doc.content.as_bytes())?;
        let id = match existing {
            Some(id) => {
                let old = self.block_of(id)?;
                // contentless FTS 支持按 rowid 直接删除（contentless_delete=1）
                self.conn
                    .execute("DELETE FROM fts WHERE rowid=?1", params![id])?;
                self.conn.execute(
                    "UPDATE docs SET parent=?2,name=?3,ext=?4,size=?5,mtime=?6,encoding=?7,flags=?8
                     WHERE doc_id=?1",
                    params![
                        id,
                        doc.parent,
                        doc.name,
                        doc.ext,
                        doc.size as i64,
                        doc.mtime,
                        doc.encoding,
                        doc.flags
                    ],
                )?;
                self.put_block(id, &block)?;
                if let Some(old) = old {
                    self.segments.mark_dead(old.len as u64);
                }
                id
            }
            None => {
                self.conn.execute(
                    "INSERT INTO docs (path,parent,name,ext,size,mtime,encoding,flags)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        doc.path,
                        doc.parent,
                        doc.name,
                        doc.ext,
                        doc.size as i64,
                        doc.mtime,
                        doc.encoding,
                        doc.flags
                    ],
                )?;
                let id = self.conn.last_insert_rowid();
                self.put_block(id, &block)?;
                id
            }
        };
        self.conn.execute(
            "INSERT INTO fts(rowid, name_tok, path_tok, content_tok) VALUES (?1,?2,?3,?4)",
            params![id, doc.name_tok, doc.path_tok, doc.content_tok],
        )?;
        self.tx_count.set(self.tx_count.get() + 1);
        if self.tx_count.get() >= BATCH_COMMIT_DOCS {
            self.commit_batch()?;
        }
        Ok(id)
    }

    /// 提交当前批量写事务。检查点（state.json）、查询、统计、整理与重建前都应调用，
    /// 保证检查点与数据库一致；崩溃时最多丢失最后一批文档，断点续跑可覆盖。
    pub fn commit_batch(&self) -> SnapResult<()> {
        if self.tx_open.get() {
            self.conn.execute_batch("COMMIT")?;
            self.tx_open.set(false);
            self.tx_count.set(0);
        }
        Ok(())
    }

    fn block_of(&self, doc_id: i64) -> SnapResult<Option<BlockRef>> {
        let row = self
            .conn
            .query_row(
                "SELECT seg, off, len, raw_len FROM content_blocks WHERE doc_id=?1",
                [doc_id],
                |r| {
                    Ok(BlockRef {
                        seg: r.get::<_, i64>(0)? as u32,
                        off: r.get::<_, i64>(1)? as u64,
                        len: r.get::<_, i64>(2)? as u32,
                        raw_len: r.get::<_, i64>(3)? as u32,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    fn put_block(&self, doc_id: i64, b: &BlockRef) -> SnapResult<()> {
        self.conn.execute(
            "INSERT INTO content_blocks(doc_id, seg, off, len, raw_len) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(doc_id) DO UPDATE SET
                seg=excluded.seg, off=excluded.off, len=excluded.len, raw_len=excluded.raw_len",
            params![
                doc_id,
                b.seg as i64,
                b.off as i64,
                b.len as i64,
                b.raw_len as i64
            ],
        )?;
        Ok(())
    }

    fn delete_doc_row(&self, doc_id: i64) -> SnapResult<()> {
        if let Some(old) = self.block_of(doc_id)? {
            self.segments.mark_dead(old.len as u64);
        }
        self.conn
            .execute("DELETE FROM fts WHERE rowid=?1", params![doc_id])?;
        self.conn
            .execute("DELETE FROM content_blocks WHERE doc_id=?1", params![doc_id])?;
        self.conn
            .execute("DELETE FROM docs WHERE doc_id=?1", params![doc_id])?;
        Ok(())
    }

    pub fn delete_path(&self, path: &str) -> SnapResult<bool> {
        let id: Option<i64> = self
            .conn
            .query_row("SELECT doc_id FROM docs WHERE path=?1", [path], |r| r.get(0))
            .optional()?;
        match id {
            Some(id) => {
                self.delete_doc_row(id)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// 读取正文（按需解压单块，不整卷加载）；返回 (path, name, content, flags)。
    pub fn doc_content(&self, doc_id: i64) -> SnapResult<Option<(String, String, String, i64)>> {
        let row = self
            .conn
            .query_row(
                "SELECT path, name, flags FROM docs WHERE doc_id=?1",
                [doc_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((path, name, flags)) = row else {
            return Ok(None);
        };
        let content = match self.block_of(doc_id)? {
            Some(b) if !b.is_none() => self
                .segments
                .read(&b)
                .map(|d| String::from_utf8_lossy(&d).to_string())
                .unwrap_or_default(),
            _ => String::new(),
        };
        Ok(Some((path, name, content, flags)))
    }

    pub fn begin_seen(&self) -> SnapResult<()> {
        self.conn
            .execute_batch("CREATE TEMP TABLE IF NOT EXISTS seen(path TEXT PRIMARY KEY);")?;
        self.conn.execute("DELETE FROM seen", [])?;
        Ok(())
    }

    pub fn mark_seen(&self, path: &str) -> SnapResult<()> {
        self.conn
            .execute("INSERT OR IGNORE INTO seen(path) VALUES(?1)", [path])?;
        Ok(())
    }

    /// 全量校验模式：删除本轮未遍历到的文档（用于 `--resync`）。
    pub fn delete_unseen(&self) -> SnapResult<usize> {
        let ids: Vec<i64> = {
            let mut stmt = self
                .conn
                .prepare("SELECT doc_id FROM docs WHERE path NOT IN (SELECT path FROM seen)")?;
            let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
            rows.flatten().collect()
        };
        for id in &ids {
            self.delete_doc_row(*id)?;
        }
        Ok(ids.len())
    }

    /// 清空卷索引（docs + 倒排 + 分段文件），供「重建」与 schema 升级使用。
    pub fn rebuild(&mut self) -> SnapResult<()> {
        self.commit_batch()?;
        let _ = self.conn.execute_batch(
            "DROP TABLE IF EXISTS fts; DROP TABLE IF EXISTS content_blocks; DROP TABLE IF EXISTS docs;",
        );
        let ids = self.segments.segment_ids().unwrap_or_default();
        let _ = self.segments.remove_segments(&ids);
        self.segments.reset_dead();
        create_schema(&self.conn)?;
        Ok(())
    }

    pub fn segments_count(&self) -> u32 {
        self.segments.segment_ids().map(|v| v.len() as u32).unwrap_or(0)
    }

    /// 分段整理：把活块重写到新段并删除旧段，回收更新产生的垃圾字节。
    /// 返回回收字节数。
    pub fn compact(&self) -> SnapResult<u64> {
        self.commit_batch()?;
        let before = self.segments.total_bytes();
        if before == 0 {
            return Ok(0);
        }
        // 先切换到全新段，保证旧段整体可删
        let new_start = self.segments.rotate()?;
        let rows: Vec<(i64, BlockRef)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT doc_id, seg, off, len, raw_len FROM content_blocks ORDER BY doc_id")?;
            let mapped = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    BlockRef {
                        seg: r.get::<_, i64>(1)? as u32,
                        off: r.get::<_, i64>(2)? as u64,
                        len: r.get::<_, i64>(3)? as u32,
                        raw_len: r.get::<_, i64>(4)? as u32,
                    },
                ))
            })?;
            mapped.flatten().collect()
        };
        let mut updates = Vec::with_capacity(rows.len());
        for (doc_id, old) in rows {
            if old.is_none() {
                continue;
            }
            let data = match self.segments.read(&old) {
                Ok(d) => d,
                Err(_) => continue, // 块损坏：跳过，保留原记录
            };
            let next = self.segments.append(&data)?;
            updates.push((doc_id, next));
        }
        if !updates.is_empty() {
            let tx = self.conn.unchecked_transaction()?;
            for (doc_id, b) in &updates {
                tx.execute(
                    "UPDATE content_blocks SET seg=?2, off=?3, len=?4, raw_len=?5 WHERE doc_id=?1",
                    params![
                        doc_id,
                        b.seg as i64,
                        b.off as i64,
                        b.len as i64,
                        b.raw_len as i64
                    ],
                )?;
            }
            tx.commit()?;
        }
        let old_ids: Vec<u32> = self
            .segments
            .segment_ids()?
            .into_iter()
            .filter(|id| *id < new_start)
            .collect();
        self.segments.remove_segments(&old_ids)?;
        self.segments.reset_dead();
        Ok(before.saturating_sub(self.segments.total_bytes()))
    }

    /// 垃圾字节占比是否值得整理（>50% 或 >8 MB）。
    pub fn compact_needed(&self) -> bool {
        let dead = self.segments.dead_bytes();
        let total = self.segments.total_bytes();
        total > 0 && (dead > 8 * 1024 * 1024 || dead * 2 >= total)
    }

    pub fn checkpoint(&self) -> SnapResult<()> {
        let _ = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        Ok(())
    }

    pub fn disk_bytes(&self) -> u64 {
        dir_size(&self.dir)
    }

    pub fn close(&mut self) {
        let _ = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }
}

/// 建立卷内 schema（v2）：元数据表 + 正文块定位表 + contentless 倒排。
fn create_schema(conn: &Connection) -> SnapResult<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS docs (
            doc_id INTEGER PRIMARY KEY,
            path TEXT NOT NULL UNIQUE,
            parent TEXT NOT NULL,
            name TEXT NOT NULL,
            ext TEXT NOT NULL,
            size INTEGER NOT NULL,
            mtime INTEGER NOT NULL,
            encoding TEXT NOT NULL DEFAULT '',
            flags INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_docs_ext ON docs(ext);
        CREATE INDEX IF NOT EXISTS idx_docs_mtime ON docs(mtime);
        CREATE INDEX IF NOT EXISTS idx_docs_size ON docs(size);
        CREATE TABLE IF NOT EXISTS content_blocks (
            doc_id INTEGER PRIMARY KEY,
            seg INTEGER NOT NULL,
            off INTEGER NOT NULL,
            len INTEGER NOT NULL,
            raw_len INTEGER NOT NULL
        );
        -- contentless：倒排里不再重复保存原文；contentless_delete=1 允许按 rowid 删除
        CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(
            name_tok, path_tok, content_tok,
            content='', contentless_delete=1,
            tokenize='unicode61 remove_diacritics 2'
        );
        "#,
    )?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            match e.metadata() {
                Ok(m) if m.is_file() => total += m.len(),
                Ok(m) if m.is_dir() => total += dir_size(&e.path()),
                _ => {}
            }
        }
    }
    total
}

pub fn load_state(dir: &Path) -> Option<VolumeState> {
    let raw = std::fs::read_to_string(dir.join("state.json")).ok()?;
    serde_json::from_str(&raw).ok()
}

/// 估算某个卷的索引体积（用于统计面板）。
pub fn volume_index_bytes(root: &IndexRoot, volume_id: &str) -> u64 {
    dir_size(&root.volume_dir(volume_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapfind_core::config::resolve_index_root;

    fn temp_root(tag: &str) -> IndexRoot {
        let dir = std::env::temp_dir().join(format!(
            "snapfind-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        resolve_index_root(Some(&dir))
    }

    fn sample_volume() -> VolumeInfo {
        VolumeInfo {
            volume_id: "vol-test".into(),
            mount: "C:\\".into(),
            fs: "NTFS".into(),
            total_bytes: 0,
            free_bytes: 0,
            removable: false,
            network: false,
            enabled: true,
        }
    }

    #[test]
    fn upsert_and_search() {
        let root = temp_root("store");
        let mut store = Store::open(&root).unwrap();
        let info = sample_volume();
        let vol = store.volume_mut(&info).unwrap();
        let doc = DocRecord {
            path: "C:\\docs\\a.txt".into(),
            parent: "C:\\docs".into(),
            name: "a.txt".into(),
            ext: "txt".into(),
            size: 10,
            mtime: 1,
            encoding: "utf-8".into(),
            flags: 0,
            content: "hello 世界".into(),
            name_tok: crate::tokenize::token_string("a.txt"),
            path_tok: crate::tokenize::token_string("C:\\docs\\a.txt"),
            content_tok: crate::tokenize::token_string("hello 世界"),
        };
        let id = vol.upsert_doc(&doc).unwrap();
        assert!(id > 0);
        assert_eq!(vol.count_docs().unwrap(), 1);

        // 更新同一路径不应产生第二条记录
        let mut doc2 = doc.clone();
        doc2.size = 20;
        vol.upsert_doc(&doc2).unwrap();
        assert_eq!(vol.count_docs().unwrap(), 1);

        // FTS 可命中
        let n: i64 = vol
            .conn
            .query_row(
                "SELECT COUNT(*) FROM fts WHERE fts MATCH '\"hello\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);

        // 中文 bigram 可命中
        let n: i64 = vol
            .conn
            .query_row("SELECT COUNT(*) FROM fts WHERE fts MATCH '\"世界\"'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1);

        vol.delete_path("C:\\docs\\a.txt").unwrap();
        assert_eq!(vol.count_docs().unwrap(), 0);
        let _ = std::fs::remove_dir_all(root.path);
    }
}
