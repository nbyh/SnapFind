//! 内存分级降级的实测：超预算时只索引元数据，正文不再抽取（文档 §8.2）。

use snapfind_core::config::{resolve_index_root, AppConfig};
use snapfind_core::volume::VolumeInfo;
use snapfind_engine::governor::{Governor, FLAG_METADATA_ONLY, LEVEL1_CONTENT_CAP};
use snapfind_engine::{scan, service};

fn temp_root(tag: &str) -> snapfind_core::config::IndexRoot {
    let dir = std::env::temp_dir().join(format!(
        "snapfind-degrade-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    resolve_index_root(Some(&dir))
}

fn write_file(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn over_budget_indexes_metadata_only() {
    let root = temp_root("l2");
    let corpus = std::env::temp_dir().join(format!("snapfind-degrade-corpus-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&corpus);
    let path = write_file(&corpus, "metasearchable.txt", "zebraquoll-body-should-be-skipped");

    let mut store = snapfind_engine::store::Store::open(&root).unwrap();
    let info = VolumeInfo {
        volume_id: "vol-degrade".into(),
        mount: corpus.to_string_lossy().to_string(),
        fs: "NTFS".into(),
        total_bytes: 0,
        free_bytes: 0,
        removable: false,
        network: false,
        enabled: true,
    };
    // 预算设为 1 字节 → 必然触发 L2
    let gov = Governor::new("off", 1);
    assert_eq!(gov.refresh_level(), 2);
    assert_eq!(gov.content_cap(1024 * 1024), None);

    let cfg = AppConfig::default();
    let outcome = {
        let vol = store.volume_mut(&info).unwrap();
        scan::index_file(vol, &path, 40, 1, &cfg, &gov)
    };
    assert_eq!(outcome, scan::FileOutcome::Indexed);

    let vol = store.volume(&info.volume_id).unwrap();
    let flags: i64 = vol
        .conn
        .query_row("SELECT flags FROM docs LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert!(flags & FLAG_METADATA_ONLY != 0, "应标记仅元数据");
    // 文件名仍可搜，正文不可搜
    let by_name: i64 = vol
        .conn
        .query_row("SELECT COUNT(*) FROM fts WHERE name_tok MATCH '\"metasearchable\"'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(by_name, 1);
    let by_content: i64 = vol
        .conn
        .query_row(
            "SELECT COUNT(*) FROM fts WHERE content_tok MATCH '\"zebraquoll\"'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(by_content, 0);

    let _ = std::fs::remove_dir_all(root.path);
    let _ = std::fs::remove_dir_all(&corpus);
}

#[test]
fn level1_caps_content_length() {
    let rss = service::rss_bytes();
    if rss < 8 {
        return; // 采样不可用（非 Windows/Linux）时跳过
    }
    let gov = Governor::new("off", rss.saturating_mul(4) / 3);
    gov.set_level(1);
    assert_eq!(gov.level(), 1);
    assert_eq!(gov.content_cap(4 * 1024 * 1024), Some(LEVEL1_CONTENT_CAP));
    assert_eq!(gov.content_cap(64 * 1024), Some(64 * 1024));
    assert_eq!(gov.level_name(), "content-capped");
}

#[test]
fn under_budget_keeps_full_content() {
    let gov = Governor::new("off", 64 * 1024 * 1024 * 1024);
    assert_eq!(gov.refresh_level(), 0);
    let vol = temp_root("l0");
    let _ = std::fs::remove_dir_all(vol.path);
}
