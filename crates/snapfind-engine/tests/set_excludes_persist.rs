//! 回归测试：`setExcludes` 只改内存，绝不回写 config.json。
//!
//! 历史缺陷：索引进程在处理 `setExcludes` 时把**启动时的整份配置快照**写回磁盘，
//! 覆盖桌面端刚保存的主题 / 托盘 / 并发 / 索引目录等设置，重启后才暴露。
//! 本测试模拟「桌面端已落盘、indexd 内存仍是旧值」的时刻，断言磁盘文件一字未变。

use snapfind_core::config::AppConfig;
use snapfind_core::ipc::VolumeAction;
use snapfind_engine::service::{EventSink, Service};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn set_excludes_does_not_rewrite_config_json() {
    // 1) 磁盘上是「用户刚改过」的配置
    let mut disk = AppConfig::default();
    disk.ui.theme = "dark".into();
    disk.index.max_text_bytes_per_file = 1_234_567;
    disk.save().expect("写测试配置");
    let cfg_path = AppConfig::path();

    // 2) indexd 用同一份配置启动（内存快照 = 读到的内容）
    let dir = std::env::temp_dir().join(format!("snapfind-excl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时索引目录");
    let mut cfg = AppConfig::load();
    cfg.index.root = dir.to_string_lossy().to_string();
    cfg.index.resume_on_start = false;

    let sink: EventSink = Arc::new(|_| {});
    let svc = Service::open(cfg, sink).expect("打开服务");

    // 3) 桌面端保存排除规则：先写盘，再通知索引进程同步内存
    disk.index.exclude_rules = vec!["**/node_modules/**".into()];
    disk.save().expect("桌面端保存配置");
    let after_desktop = std::fs::read_to_string(&cfg_path).expect("读配置");

    svc.volume_action(&VolumeAction {
        action: "setExcludes".into(),
        volume_id: Some("vol-test".into()),
        excludes: Some(vec!["**/node_modules/**".into()]),
        volumes: None,
    })
    .expect("setExcludes 应成功");

    // 4) 索引进程不得改动磁盘：否则会把桌面端刚写的设置回滚成旧快照
    let after_indexd = std::fs::read_to_string(&cfg_path).expect("读配置");
    assert_eq!(
        after_desktop, after_indexd,
        "setExcludes 不得改写 config.json（会把主题/托盘/并发等回滚成启动时的旧值）"
    );
    assert!(
        after_indexd.contains("1234567"),
        "桌面端写入的 extract 参数必须保留"
    );

    let _ = svc.shutdown(Duration::from_millis(300));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&cfg_path);
}
