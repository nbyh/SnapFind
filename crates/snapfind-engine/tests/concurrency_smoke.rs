//! 并发压测（计划 §8 前置条件）：扫描进行中同时跑 `stats()` 与 `run_query()`。
//!
//! 目的不是测吞吐，而是**证明当前实现不会死锁、不会 panic、查询始终能返回**：
//! 后续 R2-1「拆锁」（把 store 全局锁拆成每卷锁/读连接池）必须以这条测试为回归基线，
//! 否则改动一旦引入死锁，表现是「搜索永久转圈」，很难在生产环境定位。
//!
//! 现在 run_query 会持有全局 store 锁并 `commit_all()`，与扫描线程争锁是常态，
//! 所以这里刻意让查询与扫描重叠，且断言总耗时在预算内。

use snapfind_core::config::AppConfig;
use snapfind_core::ipc::{Paging, QueryRequest};
use snapfind_engine::service::{EventSink, Service};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
    let dir = base.join(format!(
        "snapfind-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

#[test]
fn search_and_stats_do_not_block_scan() {
    // ---- 造数据：多目录、多文件，让扫描持续一小段时间 ----
    let base = temp_dir("concurrency");
    let docs = base.join("docs");
    const DIRS: usize = 8;
    const FILES_PER_DIR: usize = 60;
    for d in 0..DIRS {
        let sub = docs.join(format!("sub{d}"));
        std::fs::create_dir_all(&sub).expect("建子目录");
        for f in 0..FILES_PER_DIR {
            let body = if f % 2 == 0 {
                format!("alpha beta gamma needle-{d}-{f} delta epsilon")
            } else {
                format!("unrelated text filler number {d} {f}")
            };
            std::fs::write(sub.join(format!("file-{d}-{f}.txt")), body).expect("写测试文件");
        }
    }

    // ---- 索引进程：索引目录放 %TEMP%（扫描内容才受默认排除影响） ----
    let index_root = base.join("index");
    let mut cfg = AppConfig::default();
    cfg.index.root = index_root.to_string_lossy().to_string();
    cfg.index.resume_on_start = false;
    cfg.index.parallel_volumes = 1;
    let sink: EventSink = Arc::new(|_| {});
    let svc = Service::open(cfg, sink).expect("打开服务");

    let volume_id = svc
        .register_scope_dir(docs.to_str().expect("路径必须是 UTF-8"))
        .expect("登记目录");

    let started = Instant::now();
    svc.start_index(vec![volume_id.clone()], vec![])
        .expect("启动索引");

    // ---- 并发：一边扫描，一边高频统计 + 查询 ----
    let queries = Arc::new(AtomicUsize::new(0));
    let hits_seen = Arc::new(AtomicUsize::new(0));
    let query_errors = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    std::thread::scope(|scope| {
        // 查询线程：扫描期间持续查询，模拟用户边索引边搜
        {
            let svc = svc.clone();
            let (queries, hits_seen, query_errors, stop) = (
                queries.clone(),
                hits_seen.clone(),
                query_errors.clone(),
                stop.clone(),
            );
            scope.spawn(move || {
                let mut i = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    let req = QueryRequest {
                        request_id: format!("q-{i}"),
                        query: "needle".into(),
                        paging: Paging {
                            offset: 0,
                            limit: 20,
                        },
                        ..Default::default()
                    };
                    let cancel = AtomicBool::new(false);
                    let mut got = 0usize;
                    match svc.run_query(&req, &cancel, &mut |chunk| got += chunk.hits.len()) {
                        Ok(()) => {
                            queries.fetch_add(1, Ordering::Relaxed);
                            hits_seen.fetch_add(got, Ordering::Relaxed);
                        }
                        Err(_) => {
                            query_errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    i += 1;
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
        }

        // 统计线程：桌面端 100ms 轮询 stats，与查询/扫描完全并发
        {
            let svc = svc.clone();
            let stop = stop.clone();
            scope.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let s = svc.stats();
                    assert!(!s.backend.is_empty(), "stats 返回了空 backend");
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
        }

        // 主线程：等索引完成（有超时，死锁就会在这里暴露）
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut finished = false;
        while Instant::now() < deadline {
            let s = svc.stats();
            if s.total_docs as usize >= DIRS * FILES_PER_DIR
                && s.volumes
                    .iter()
                    .all(|v| v.state != "Scanning" && v.state != "Indexing")
            {
                finished = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        stop.store(true, Ordering::Relaxed);

        assert!(
            finished,
            "60 秒内未完成索引：total_docs={}，疑似扫描/查询互锁",
            svc.stats().total_docs
        );
    });

    let elapsed = started.elapsed();
    let q = queries.load(Ordering::Relaxed);
    assert!(q > 0, "扫描期间一次查询都没跑完（疑似被扫描长时间阻塞）");
    assert_eq!(
        query_errors.load(Ordering::Relaxed),
        0,
        "扫描期间的查询报了错"
    );
    assert!(
        hits_seen.load(Ordering::Relaxed) > 0,
        "扫描期间查询从未命中已入库文档（{q} 次查询）"
    );
    assert!(
        elapsed < Duration::from_secs(120),
        "并发压测总耗时过长：{elapsed:?}"
    );

    // 扫描完成后仍必须能正常查询
    let req = QueryRequest {
        request_id: "q-final".into(),
        query: "needle".into(),
        paging: Paging {
            offset: 0,
            limit: 100,
        },
        ..Default::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut total = 0usize;
    svc.run_query(&req, &cancel, &mut |chunk| total += chunk.hits.len())
        .expect("索引完成后的查询必须成功");
    assert!(total > 0, "索引完成后仍搜不到 needle");

    let _ = std::fs::remove_dir_all(&base);
}
