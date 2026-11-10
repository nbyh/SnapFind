//! 分段压缩与整理（compaction）的端到端验证：
//! 正文不进 SQLite，更新是追加写，整理后旧段被回收且内容仍可读。

use snapfind_core::config::{resolve_index_root, AppConfig};
use snapfind_core::volume::VolumeInfo;
use snapfind_engine::segments::SegmentStore;
use snapfind_engine::store::{DocRecord, Store};
use snapfind_engine::tokenize;

fn temp_root(tag: &str) -> snapfind_core::config::IndexRoot {
    let dir = std::env::temp_dir().join(format!(
        "snapfind-segtest-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    resolve_index_root(Some(&dir))
}

fn volume(tag: &str) -> VolumeInfo {
    VolumeInfo {
        volume_id: format!("vol-{tag}"),
        mount: "C:\\".into(),
        fs: "NTFS".into(),
        total_bytes: 0,
        free_bytes: 0,
        removable: false,
        network: false,
        enabled: true,
    }
}

fn record(path: &str, content: &str) -> DocRecord {
    DocRecord {
        path: path.into(),
        parent: "C:\\docs".into(),
        name: "a.txt".into(),
        ext: "txt".into(),
        size: content.len() as u64,
        mtime: 1,
        encoding: "utf-8".into(),
        flags: 0,
        content: content.into(),
        name_tok: tokenize::token_string("a.txt"),
        path_tok: tokenize::token_string(path),
        content_tok: tokenize::token_string(content),
    }
}

#[test]
fn content_lives_in_segments_and_compaction_keeps_it_readable() {
    let root = temp_root("compact");
    let mut store = Store::open(&root).unwrap();
    let info = volume("compact");
    let long = "lorem ipsum dolor sit amet ".repeat(200);
    {
        let vol = store.volume_mut(&info).unwrap();
        let id = vol.upsert_doc(&record("C:\\docs\\a.txt", &long)).unwrap();
        // 正文来自分段，且 SQLite 里不再保存 content 列
        let (path, _name, content, _flags) = vol.doc_content(id).unwrap().unwrap();
        assert_eq!(path, "C:\\docs\\a.txt");
        assert_eq!(content, long);
    }
    // 多次更新 → 产生垃圾块
    {
        let vol = store.volume_mut(&info).unwrap();
        for i in 0..8 {
            vol.upsert_doc(&record("C:\\docs\\a.txt", &format!("{long} rev{i}")))
                .unwrap();
        }
        let before = vol.segments.total_bytes();
        assert!(before > 0, "段目录应有内容");
        assert!(vol.segments.dead_bytes() > 0);
        let freed = vol.compact().unwrap();
        assert!(
            freed > 0,
            "整理应回收垃圾块: freed={freed} before={before} after={}",
            vol.segments.total_bytes()
        );
        assert!(vol.segments.total_bytes() < before);
        assert_eq!(vol.segments_count(), 1);
        // 整理后内容仍可读
        let hits = fts_hit(vol, "\"rev7\"");
        assert_eq!(hits, 1);
        let id = doc_id(vol);
        let (_p, _n, content, _f) = vol.doc_content(id).unwrap().unwrap();
        assert!(content.ends_with("rev7"));
    }
    let _ = std::fs::remove_dir_all(root.path);
}

fn doc_id(vol: &snapfind_engine::store::VolumeStore) -> i64 {
    vol.conn
        .query_row("SELECT doc_id FROM docs LIMIT 1", [], |r| r.get(0))
        .unwrap()
}

fn fts_hit(vol: &snapfind_engine::store::VolumeStore, expr: &str) -> i64 {
    vol.conn
        .query_row("SELECT COUNT(*) FROM fts WHERE fts MATCH ?1", [expr], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn deleted_docs_leave_no_orphan_postings() {
    let root = temp_root("delete");
    let mut store = Store::open(&root).unwrap();
    let info = volume("delete");
    let vol = store.volume_mut(&info).unwrap();
    vol.upsert_doc(&record("C:\\docs\\x.txt", "uniqueword-alpha"))
        .unwrap();
    vol.upsert_doc(&record("C:\\docs\\y.txt", "uniqueword-beta"))
        .unwrap();
    assert_eq!(fts_hit(vol, "uniqueword"), 2);
    assert!(vol.delete_path("C:\\docs\\x.txt").unwrap());
    assert_eq!(fts_hit(vol, "uniqueword"), 1);
    assert_eq!(fts_hit(vol, "\"uniqueword-alpha\""), 0);
    let _ = std::fs::remove_dir_all(root.path);
}

#[test]
fn segment_roundtrip_compresses() {
    let dir = std::env::temp_dir().join(format!("snapfind-segrt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let seg = SegmentStore::open(&dir).unwrap();
    let text = "the quick brown fox jumps over the lazy dog ".repeat(500);
    let b = seg.append(text.as_bytes()).unwrap();
    assert!(seg.total_bytes() < text.len() as u64 / 2, "压缩率应明显优于 50%");
    assert_eq!(String::from_utf8(seg.read(&b).unwrap()).unwrap(), text);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = AppConfig::default();
}
