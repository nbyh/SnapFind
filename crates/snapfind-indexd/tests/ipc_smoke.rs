//! 端到端冒烟链路（计划 R4-2）：真实拉起 `snapfind-indexd` 进程，走完整 IPC 流程
//!
//! 启动 → hello 握手 → scope.preview → scope.register → index.start
//!      → 轮询 stats.get 直到文档可见 → query.execute 命中 → shutdown
//!
//! 该测试覆盖桌面端与 CLI 共用的「协议 + 进程生命周期」这一段，
//! 是 R2-1 拆锁等后续重构的回归兜底。

use snapfind_core::ipc::{self, msg, Envelope, QueryRequest};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn temp_root(tag: &str) -> PathBuf {
    // 注意：不能用 std::env::temp_dir()——默认排除规则里有
    // `**\AppData\Local\Temp\**` / `/tmp/**`，放那里的文档会被整目录排除。
    // 统一落在仓库 target/tmp 下（默认排除表只排 target/debug 与 target/release）。
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
    let dir = base.join(format!(
        "snapfind-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 轮询 stats 直到谓词成立，超时返回最后一次快照。
async fn wait_for_stats(
    client: &snapfind_ipc_client::Client,
    timeout: Duration,
    mut done: impl FnMut(&ipc::Stats) -> bool,
) -> ipc::Stats {
    let deadline = Instant::now() + timeout;
    loop {
        let resp = client
            .request(msg::STATS_GET, serde_json::json!({}))
            .await
            .expect("stats.get 请求失败");
        assert_ne!(
            resp.msg_type,
            msg::ERROR,
            "stats.get 返回错误：{}",
            resp.payload
        );
        let stats: ipc::Stats = resp.decode().expect("stats.get 响应无法解析");
        if done(&stats) {
            return stats;
        }
        if Instant::now() > deadline {
            panic!("等待索引完成超时，最后一次 stats：{stats:?}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn path_str(p: &Path) -> &str {
    p.to_str().expect("临时目录路径必须是合法 UTF-8")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smoke_start_hello_scope_index_search_shutdown() {
    // indexd_path() 默认找测试可执行文件的同级目录，这里显式指向 cargo 构建产物
    std::env::set_var("SNAPFIND_INDEXD", env!("CARGO_BIN_EXE_snapfind-indexd"));

    let base = temp_root("ipc-smoke");
    let index_root = base.join("index");
    let docs = base.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(
        docs.join("hello.txt"),
        "snapfind ipc smoke marker zanzibar quarterly report",
    )
    .unwrap();
    std::fs::write(docs.join("other.txt"), "unrelated content about nothing").unwrap();

    // ---- 启动 + hello 握手（Client::connect 内部完成 HELLO/HELLO_ACK） ----
    let client = snapfind_ipc_client::Client::connect(Some(path_str(&index_root)))
        .await
        .expect("indexd 启动或握手失败");

    // ---- scope.preview：索引目录必须落在我们指定的位置 ----
    let resp = client
        .request(msg::SCOPE_PREVIEW, serde_json::json!({}))
        .await
        .unwrap();
    assert_ne!(resp.msg_type, msg::ERROR, "scope.preview 失败：{}", resp.payload);
    let preview: ipc::ScopePreview = resp.decode().unwrap();
    let want = std::fs::canonicalize(&index_root).unwrap();
    let got = std::fs::canonicalize(&preview.index_root).unwrap_or_default();
    assert_eq!(want, got, "indexd 使用了非预期的索引目录");

    // ---- scope.register：把被测目录注册成一个卷 ----
    let resp = client
        .request(
            msg::SCOPE_REGISTER,
            serde_json::json!({ "path": path_str(&docs) }),
        )
        .await
        .unwrap();
    assert_ne!(resp.msg_type, msg::ERROR, "scope.register 失败：{}", resp.payload);
    let volume_id = resp
        .payload
        .get("volumeId")
        .and_then(|v| v.as_str())
        .expect("scope.register 未返回 volumeId")
        .to_string();

    // ---- index.start ----
    let resp = client
        .request(
            msg::INDEX_START,
            serde_json::json!({ "volumes": [volume_id.clone()], "excludes": [] }),
        )
        .await
        .unwrap();
    assert_ne!(resp.msg_type, msg::ERROR, "index.start 失败：{}", resp.payload);
    let ack: ipc::IndexStartAck = resp.decode().unwrap();
    assert!(
        ack.started.contains(&volume_id),
        "index.start 未接受被测卷：{:?}",
        ack.started
    );

    // ---- 等待索引落地：两个文件都进库，且不再处于扫描中 ----
    let stats = wait_for_stats(&client, Duration::from_secs(60), |s| {
        s.total_docs >= 2
            && s.volumes
                .iter()
                .all(|v| v.state != "Scanning" && v.state != "Indexing")
    })
    .await;
    let vol = stats
        .volumes
        .iter()
        .find(|v| v.volume_id == volume_id)
        .expect("stats 里找不到被测卷");
    assert!(vol.docs >= 2, "卷内文档数不足：{}", vol.docs);

    // ---- query.execute：命中 hello.txt ----
    let req = QueryRequest {
        request_id: "q-smoke".into(),
        query: "zanzibar".into(),
        ..Default::default()
    };
    let mut hits: Vec<ipc::Hit> = Vec::new();
    let final_frame = client
        .call(Envelope::request(msg::QUERY_EXECUTE, &req), &mut |frame: &Envelope| {
            if frame.msg_type == msg::ERROR {
                panic!("query.execute 返回错误：{}", frame.payload);
            }
            if let Ok(chunk) = frame.decode::<ipc::QueryChunk>() {
                hits.extend(chunk.hits);
            }
        })
        .await
        .expect("query.execute 失败");
    assert!(final_frame.is_final.unwrap_or(true), "查询最后一帧必须是 final");
    assert!(
        hits.iter().any(|h| h.name == "hello.txt"),
        "未命中 hello.txt，实际命中：{:?}",
        hits.iter().map(|h| &h.name).collect::<Vec<_>>()
    );

    // 无关键词必须零命中（防止「过滤器写成恒真」这类回归）
    let req_none = QueryRequest {
        request_id: "q-smoke-none".into(),
        query: "no-such-token-xyzzy".into(),
        ..Default::default()
    };
    let mut none_hits = 0usize;
    client
        .call(
            Envelope::request(msg::QUERY_EXECUTE, &req_none),
            &mut |frame: &Envelope| {
                if let Ok(chunk) = frame.decode::<ipc::QueryChunk>() {
                    none_hits += chunk.hits.len();
                }
            },
        )
        .await
        .expect("空查询失败");
    assert_eq!(none_hits, 0, "无意义查询不应有命中");

    // ---- shutdown：优雅退出（内部带超时强杀兜底） ----
    client.shutdown(1500).await;

    let _ = std::fs::remove_dir_all(&base);
}
