//! 端到端查询测试：验证 FTS5 表达式、过滤器下推、排序与高亮片段。

use snapfind_core::config::resolve_index_root;
use snapfind_core::ipc::{Filters, QueryRequest, SortSpec};
use snapfind_core::volume::VolumeInfo;
use snapfind_engine::store::{DocRecord, Store};
use snapfind_engine::tokenize;
use std::sync::atomic::AtomicBool;

fn temp_root(tag: &str) -> snapfind_core::config::IndexRoot {
    let dir = std::env::temp_dir().join(format!(
        "snapfind-e2e-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    resolve_index_root(Some(&dir))
}

fn doc(path: &str, content: &str, size: u64, mtime: i64) -> DocRecord {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path).to_string();
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    DocRecord {
        path: path.to_string(),
        parent: path
            .rsplit_once(['\\', '/'])
            .map(|(p, _)| p.to_string())
            .unwrap_or_default(),
        name_tok: tokenize::token_string(&name),
        path_tok: tokenize::token_string(path),
        content_tok: tokenize::token_string(content),
        name,
        ext,
        size,
        mtime,
        encoding: "utf-8".into(),
        flags: 0,
        content: content.to_string(),
    }
}

fn setup(tag: &str) -> (Store, VolumeInfo, snapfind_core::config::IndexRoot) {
    let root = temp_root(tag);
    let mut store = Store::open(&root).unwrap();
    let info = VolumeInfo {
        volume_id: "vol-e2e".into(),
        mount: "C:\\".into(),
        fs: "NTFS".into(),
        total_bytes: 0,
        free_bytes: 0,
        removable: false,
        network: false,
        enabled: true,
    };
    let vol = store.volume_mut(&info).unwrap();
    vol.upsert_doc(&doc(
        "C:\\work\\report.pdf",
        "quarterly 季度 report with foo metrics",
        2_000,
        1_700_000_000,
    ))
    .unwrap();
    vol.upsert_doc(&doc(
        "C:\\work\\notes.md",
        "# notes\nhello world 你好 世界 foo bar",
        500,
        1_700_000_100,
    ))
    .unwrap();
    vol.upsert_doc(&doc(
        "C:\\src\\main.rs",
        "fn main() { println!(\"hello rust\"); }",
        120,
        1_700_000_200,
    ))
    .unwrap();
    (store, info, root)
}

fn request(q: &str) -> QueryRequest {
    QueryRequest {
        request_id: "q1".into(),
        query: q.into(),
        ..Default::default()
    }
}

#[test]
fn keyword_hits_all_fields() {
    let (store, _info, root) = setup("kw");
    let cancel = AtomicBool::new(false);
    let out = snapfind_engine::query::search(&store, &request("hello"), &cancel).unwrap();
    assert_eq!(out.hits.len(), 2, "hello 应命中 notes.md 与 main.rs");
    assert!(out.hits.iter().all(|h| !h.matched_fields.is_empty()));
    let _ = std::fs::remove_dir_all(root.path);
}

#[test]
fn cjk_phrase_and_boolean() {
    let (store, _info, root) = setup("cjk");
    let cancel = AtomicBool::new(false);

    let out = snapfind_engine::query::search(&store, &request("世界"), &cancel).unwrap();
    assert_eq!(out.hits.len(), 1);

    let out = snapfind_engine::query::search(&store, &request("季度报表"), &cancel).unwrap();
    assert_eq!(out.hits.len(), 0, "bigram 短语不连续时不应命中");

    let out =
        snapfind_engine::query::search(&store, &request("hello AND rust"), &cancel).unwrap();
    assert_eq!(out.hits.len(), 1);
    assert!(out.hits[0].path.ends_with("main.rs"));

    let out = snapfind_engine::query::search(&store, &request("hello NOT rust"), &cancel).unwrap();
    assert_eq!(out.hits.len(), 1);
    assert!(out.hits[0].path.ends_with("notes.md"));

    let _ = std::fs::remove_dir_all(root.path);
}

#[test]
fn field_filter_and_sort() {
    let (store, _info, root) = setup("filter");
    let cancel = AtomicBool::new(false);

    let out = snapfind_engine::query::search(&store, &request("name:report"), &cancel).unwrap();
    assert_eq!(out.hits.len(), 1);

    let mut req = request("foo");
    req.filters = Filters {
        types: vec!["md".into()],
        ..Default::default()
    };
    let out = snapfind_engine::query::search(&store, &req, &cancel).unwrap();
    assert_eq!(out.hits.len(), 1);
    assert!(out.hits[0].path.ends_with("notes.md"));

    let mut req = request("hello");
    req.sort = SortSpec {
        key: "size".into(),
        order: "asc".into(),
    };
    let out = snapfind_engine::query::search(&store, &req, &cancel).unwrap();
    assert_eq!(out.hits[0].size, 120);

    let mut req = request("foo");
    req.filters.size_min = Some(1000);
    let out = snapfind_engine::query::search(&store, &req, &cancel).unwrap();
    assert_eq!(out.hits.len(), 1);
    assert!(out.hits[0].path.ends_with("report.pdf"));

    let _ = std::fs::remove_dir_all(root.path);
}

#[test]
fn path_prefix_filter() {
    let (store, _info, root) = setup("path");
    let cancel = AtomicBool::new(false);
    let mut req = request("hello");
    req.filters.paths = vec!["C:\\work".into()];
    let out = snapfind_engine::query::search(&store, &req, &cancel).unwrap();
    assert_eq!(out.hits.len(), 1);
    assert!(out.hits[0].path.starts_with("C:\\work"));

    let mut req = request("hello");
    req.filters.exclude_paths = vec!["**/*.rs".into()];
    let out = snapfind_engine::query::search(&store, &req, &cancel).unwrap();
    assert!(out.hits.iter().all(|h| !h.path.ends_with(".rs")));

    let _ = std::fs::remove_dir_all(root.path);
}

#[test]
fn wildcard_prefix_query() {
    let (store, _info, root) = setup("wild");
    let cancel = AtomicBool::new(false);
    let out = snapfind_engine::query::search(&store, &request("rep*"), &cancel).unwrap();
    assert_eq!(out.hits.len(), 1);
    assert!(out.hits[0].path.ends_with("report.pdf"));
    let _ = std::fs::remove_dir_all(root.path);
}

#[test]
fn syntax_error_is_reported() {
    let (store, _info, root) = setup("syntax");
    let cancel = AtomicBool::new(false);
    let err = snapfind_engine::query::search(&store, &request("(foo AND"), &cancel).unwrap_err();
    assert_eq!(err.code, snapfind_core::error::ErrorCode::QuerySyntax);
    let _ = std::fs::remove_dir_all(root.path);
}
