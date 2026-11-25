//! 回归测试：卷库损坏自愈。
//!
//! 历史缺陷：docs.db 的 FTS5 影子表被写坏（强杀 / 断电 / 杀毒软件干扰）后，
//! 打开卷时 `CREATE VIRTUAL TABLE IF NOT EXISTS fts` 抛
//! 「vtable constructor failed: fts」，且 user_version 仍是 2，
//! schema 迁移路径不会触发——用户只能手动删索引目录。
//!
//! 修复：打开卷库时加廉价探针（读 fts 首个叶子页），探针/建表报损坏类错误
//! 即整库重建（与 schema 版本迁移同路径），随后的扫描自动重新填充。
//!
//! 本测试构造真实的 FTS5 影子表损坏（把 fts_data 覆写成零块），
//! 断言：重新打开不报错 → 查询可用 → 重扫后文档回归。

use snapfind_core::config::resolve_index_root;
use snapfind_core::ipc::QueryRequest;
use snapfind_core::volume::VolumeInfo;
use snapfind_engine::store::{DocRecord, Store};
use snapfind_engine::tokenize;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

fn temp_root(tag: &str) -> snapfind_core::config::IndexRoot {
    let dir = std::env::temp_dir().join(format!(
        "snapfind-heal-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    resolve_index_root(Some(&dir))
}

fn info() -> VolumeInfo {
    VolumeInfo {
        volume_id: "vol-heal".into(),
        mount: "C:\\".into(),
        fs: "NTFS".into(),
        total_bytes: 0,
        free_bytes: 0,
        removable: false,
        network: false,
        enabled: true,
    }
}

fn doc(path: &str, content: &str) -> DocRecord {
    let name = path.rsplit(['\\', '/']).next().unwrap().to_string();
    DocRecord {
        path: path.to_string(),
        parent: path.rsplit_once('\\').map(|(p, _)| p.to_string()).unwrap_or_default(),
        name_tok: tokenize::token_string(&name),
        path_tok: tokenize::token_string(path),
        content_tok: tokenize::token_string(content),
        name,
        ext: "txt".into(),
        size: content.len() as u64,
        mtime: 1_700_000_000,
        encoding: "utf-8".into(),
        flags: 0,
        content: content.to_string(),
    }
}

fn request(q: &str) -> QueryRequest {
    QueryRequest {
        request_id: "q-heal".into(),
        query: q.into(),
        ..Default::default()
    }
}

fn run_query(store: &Store, q: &str) -> Result<usize, String> {
    let out = snapfind_engine::query::search(store, &request(q), &AtomicBool::new(false));
    out.map(|o| o.hits.len()).map_err(|e| e.to_string())
}

/// 构造真实的 FTS5 影子表损坏：整行删除 fts_data（等价断电丢页）。
/// 注：不能用 zeroblob 覆写——fts5 把全零块当空闲页容忍，不触发报错。
/// 损坏后 constructor 阶段即报 SQLITE_CORRUPT_VTAB（"vtable constructor
/// failed: fts" 或 "database disk image is malformed"）。
fn corrupt_fts(docs_db: &PathBuf) {
    let conn = rusqlite::Connection::open(docs_db).expect("打开 docs.db");
    conn.execute_batch("DELETE FROM fts_data;")
        .expect("破坏 fts_data");
}

#[test]
fn corrupt_volume_db_self_heals() {
    let root = temp_root("heal");
    let docs_db = root.volume_dir("vol-heal").join("docs.db");

    // 1) 正常索引一个文档
    let mut store = Store::open(&root).unwrap();
    {
        let vol = store.volume_mut(&info()).unwrap();
        vol.upsert_doc(&doc("C:\\work\\hello.txt", "heal zanzibar marker")).unwrap();
        vol.commit_batch().unwrap();
    }
    assert_eq!(run_query(&mut store, "zanzibar").unwrap(), 1, "修复前应能正常命中");
    drop(store);

    // 2) 破坏影子表 → 重新打开。修复前这里直接报
    //    "vtable constructor failed: fts"，查询永久失败
    corrupt_fts(&docs_db);
    let mut store = Store::open(&root).unwrap();
    {
        let vol = store
            .volume_mut(&info())
            .expect("打开损坏的卷库应自愈成功而不是报错");
        // 3) 重建后库是空的：查询可用（不报错），旧命中消失
    }
    assert_eq!(
        run_query(&mut store, "zanzibar").unwrap(),
        0,
        "自愈重建后旧倒排应被清空"
    );

    // 4) 重扫（模拟）：文档重新入库 → 检索恢复
    {
        let vol = store.volume_mut(&info()).unwrap();
        vol.upsert_doc(&doc("C:\\work\\hello.txt", "heal zanzibar marker")).unwrap();
        vol.commit_batch().unwrap();
    }
    assert_eq!(run_query(&mut store, "zanzibar").unwrap(), 1, "重扫后应恢复命中");

    drop(store);
    let _ = std::fs::remove_dir_all(&root.path);
}
