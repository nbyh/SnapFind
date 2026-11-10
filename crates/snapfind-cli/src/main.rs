//! `snapfind`：UI 进程（v0.1 以命令行界面呈现）。
//!
//! 职责与文档中的桌面端一致：
//! * 启动并独占连接 `snapfind-indexd`（一次性令牌 + 协议版本协商）；
//! * 所有请求异步发送，流式结果按 requestId 分批渲染；
//! * 退出时发送 `shutdown`，等待 `shutdown.ack`，超时则强制终止索引进程；
//! * 预览由本进程直接分块读取源文件（不经 IPC），零扫码/零登录/零云校验；
//! * 二进制名固定为 `snapfind`，桌面端为 `SnapFind.Desktop`（.NET/Avalonia）。

use clap::{Parser, Subcommand};
use snapfind_core::ipc::{self, msg, Envelope, QueryRequest};
use snapfind_core::platform;
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "snapfind",
    version,
    about = "SnapFind CLI：本地全文搜索的命令行管理工具（图形界面请运行同目录的 SnapFind）。",
    long_about = "SnapFind CLI：本地全文搜索的命令行管理工具。\n\n注意：带图形界面的桌面程序是同目录下首字母大写的 ./SnapFind；\n本命令只是 CLI（无界面），误启动它只会看到这份命令列表。"
)]
struct Cli {
    /// 覆盖索引目录（默认 <程序目录>/index，不可写时自动降级）
    #[arg(long, global = true)]
    index_dir: Option<String>,
    /// 命令结束后保留索引进程（默认关闭：关闭主界面 = 关闭索引进程）
    #[arg(long, global = true)]
    keep: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// 列出可访问的卷与文件系统信息
    Volumes,
    /// 显示首次索引范围确认页（卷/文件系统/容量/预计量级/索引目录/默认排除项）
    Scope,
    /// 建立/更新索引
    Index {
        /// 只索引指定卷（可多次），为空表示全部已勾选卷
        #[arg(long = "volume")]
        volumes: Vec<String>,
        /// 只索引指定目录（可多次）；目录会登记为一个独立索引范围
        #[arg(long = "path")]
        paths: Vec<String>,
        /// 追加排除规则（glob）
        #[arg(long = "exclude")]
        excludes: Vec<String>,
        /// 等待索引完成后退出（默认也是等待，Ctrl+C 可中断）
        #[arg(long)]
        no_wait: bool,
        /// 全量校验：删除索引中已不存在的文件记录
        #[arg(long)]
        resync: bool,
    },
    /// 全文搜索
    Search {
        /// 查询串，支持 AND/OR/NOT、短语、通配符、name:/path:/content:/ext:
        query: String,
        #[arg(long = "ext")]
        exts: Vec<String>,
        #[arg(long, default_value = "relevance")]
        sort: String,
        #[arg(long, default_value = "desc")]
        order: String,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long = "path")]
        paths: Vec<String>,
        #[arg(long)]
        min_size: Option<u64>,
        #[arg(long)]
        max_size: Option<u64>,
        /// 输出 JSON（便于脚本消费）
        #[arg(long)]
        json: bool,
    },
    /// 预览文件内容（本地分块读取，不扫码/不登录/不付费）
    Preview {
        /// 文件路径，或 `volumeId:docId`
        target: String,
        #[arg(long, default_value_t = 8192)]
        bytes: usize,
    },
    /// 用系统默认程序打开文件
    Open { path: String },
    /// 在文件管理器中定位文件
    Reveal { path: String },
    /// 复制路径到剪贴板
    Copy { path: String },
    /// 索引统计（体积/内存/文档数/状态）
    Status,
    /// 卷控制：pause/resume/stop/rebuild/clean/compact/skip/unregister
    Volume {
        action: String,
        #[arg(long)]
        id: Option<String>,
    },
    /// 整理分段：回收更新留下的垃圾块（可选 --id 指定卷）
    Compact {
        #[arg(long = "volume")]
        id: Option<String>,
    },
    /// 关闭索引进程（保存至最近检查点）
    Shutdown,
}

fn main() {
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let code = rt.block_on(run(cli));
    std::process::exit(code);
}

// UI 进程侧 IPC：与 GUI 共用 snapfind-ipc-client，保证协议语义一致。
type Session = snapfind_ipc_client::Client;

async fn run(cli: Cli) -> i32 {
    let path = cli.index_dir.clone();
    match &cli.cmd {
        Cmd::Open { path } => {
            return match platform::open_file(std::path::Path::new(path)) {
                Ok(_) => 0,
                Err(e) => {
                    eprintln!("打开失败: {e}");
                    1
                }
            }
        }
        Cmd::Reveal { path } => {
            return match platform::reveal_path(std::path::Path::new(path)) {
                Ok(_) => 0,
                Err(e) => {
                    eprintln!("定位失败: {e}");
                    1
                }
            }
        }
        Cmd::Copy { path } => {
            return match platform::copy_text(path) {
                Ok(_) => {
                    println!("已复制: {path}");
                    0
                }
                Err(e) => {
                    eprintln!("复制失败: {e}");
                    1
                }
            };
        }
        _ => {}
    }

    let session = match Session::connect(path.as_deref()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("无法连接索引服务: {e}");
            return 1;
        }
    };

    let code = dispatch(&cli, &session).await;
    if !cli.keep {
        session.shutdown(5000).await;
    }
    code
}

async fn dispatch(cli: &Cli, session: &Session) -> i32 {
    match &cli.cmd {
        Cmd::Volumes => {
            let env = Envelope::request(msg::VOLUME_LIST, serde_json::json!({}));
            match session.call(env, &mut |_| {}).await {
                Ok(resp) => {
                    if resp.msg_type == msg::ERROR {
                        print_error(&resp);
                        return 1;
                    }
                    let vols: Vec<snapfind_core::volume::VolumeInfo> =
                        resp.decode().unwrap_or_default();
                    println!(
                        "{:<20} {:<8} {:>10} {:>10}  {}",
                        "卷", "文件系统", "总容量", "可用", "索引"
                    );
                    for v in vols {
                        println!(
                            "{:<20} {:<8} {:>10} {:>10}  {}",
                            v.mount,
                            v.fs,
                            human(v.total_bytes),
                            human(v.free_bytes),
                            if v.enabled { "已勾选" } else { "未勾选" }
                        );
                    }
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        Cmd::Scope => {
            let env = Envelope::request(msg::SCOPE_PREVIEW, serde_json::json!({}));
            match session.call(env, &mut |_| {}).await {
                Ok(resp) => {
                    if resp.msg_type == msg::ERROR {
                        print_error(&resp);
                        return 1;
                    }
                    let preview: ipc::ScopePreview = resp.decode().unwrap_or_else(|_| ipc::ScopePreview {
                        index_root: String::new(),
                        degraded_reason: None,
                        volumes: vec![],
                        excludes: vec![],
                    });
                    println!("索引目录: {}", preview.index_root);
                    if let Some(reason) = &preview.degraded_reason {
                        println!("⚠ 索引目录已降级（原因：{reason}）");
                    }
                    println!("\n卷：");
                    for v in &preview.volumes {
                        println!(
                            "  [{}] {:<12} {:<8} 总 {:>9} 可用 {:>9} 预计约 {} 个文件{}",
                            if v.enabled { "x" } else { " " },
                            v.mount,
                            v.fs,
                            human(v.total_bytes),
                            human(v.free_bytes),
                            v.estimated_files,
                            if v.network {
                                "（网络卷，默认不索引）"
                            } else if v.removable {
                                "（可移动介质，默认不索引）"
                            } else {
                                ""
                            }
                        );
                    }
                    println!("\n默认排除项（{} 条）：", preview.excludes.len());
                    for e in preview.excludes.iter().take(20) {
                        println!("  - {e}");
                    }
                    if preview.excludes.len() > 20 {
                        println!("  … 其余 {} 条", preview.excludes.len() - 20);
                    }
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        Cmd::Index {
            volumes,
            paths,
            excludes,
            no_wait,
            resync,
        } => {
            let mut target_ids: Vec<String> = volumes.clone();
            for p in paths {
                let env = Envelope::request(msg::SCOPE_REGISTER, serde_json::json!({ "path": p }));
                match session.call(env, &mut |_| {}).await {
                    Ok(resp) if resp.msg_type != msg::ERROR => {
                        if let Some(id) = resp.payload.get("volumeId").and_then(|v| v.as_str()) {
                            target_ids.push(id.to_string());
                        }
                    }
                    Ok(resp) => {
                        print_error(&resp);
                        return 1;
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        return 1;
                    }
                }
            }
            let env = Envelope::request(
                msg::INDEX_START,
                serde_json::json!({ "volumes": target_ids, "excludes": excludes }),
            );
            let mut last_line = String::new();
            let mut events = session.events();
            let resp = session.call(env, &mut |_| {}).await;
            println!();
            match resp {
                Ok(resp) if resp.msg_type != msg::ERROR => {
                    if let Ok(ack) = resp.decode::<ipc::IndexStartAck>() {
                        println!(
                            "已启动索引：{} 个卷，并行度 {}",
                            ack.started.len(),
                            ack.parallel_volumes
                        );
                    }
                    if *no_wait {
                        return 0;
                    }
                    // 进度来自事件广播；卷进入 Watching 即视为就绪
                    loop {
                        tokio::select! {
                            _ = tokio::signal::ctrl_c() => {
                                println!("\n收到中断，正在保存检查点（稍后可续建）…");
                                session.shutdown(5000).await;
                                return 130;
                            }
                            r = events.recv() => {
                                let frame = match r { Ok(f) => f, Err(_) => break };
                                if frame.msg_type != msg::EVENT_PROGRESS {
                                    continue;
                                }
                                if let Ok(p) = frame.decode::<ipc::Progress>() {
                                    let line = progress_line(&p);
                                    if line != last_line {
                                        print!("\r{:<100}", line);
                                        let _ = std::io::stdout().flush();
                                        last_line = line;
                                    }
                                    if p.state == "Watching" && p.files_done > 0 {
                                        println!("\n{} 索引就绪：{} 个文件", p.mount, p.files_done);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    if *resync {
                        println!("注意：v0.1 的全量校验请在索引完成后使用 `snapfind volume rebuild --id <卷>` 重建。");
                    }
                    0
                }
                Ok(resp) => {
                    print_error(&resp);
                    1
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        Cmd::Search {
            query,
            exts,
            sort,
            order,
            limit,
            offset,
            paths,
            min_size,
            max_size,
            json,
        } => {
            let request_id = snapfind_core::ids::next_id("q");
            let req = QueryRequest {
                request_id: request_id.clone(),
                query: query.clone(),
                filters: ipc::Filters {
                    types: exts.clone(),
                    size_min: *min_size,
                    size_max: *max_size,
                    paths: paths.clone(),
                    ..Default::default()
                },
                sort: ipc::SortSpec {
                    key: sort.clone(),
                    order: order.clone(),
                },
                paging: ipc::Paging {
                    offset: *offset,
                    limit: *limit,
                },
                ..Default::default()
            };
            let env = Envelope::request(msg::QUERY_EXECUTE, &req);
            let mut printed = 0usize;
            let mut total = 0usize;
            let mut took = 0u64;
            let mut warnings: Vec<String> = Vec::new();
            let start = std::time::Instant::now();
            // 流式：每帧一批命中，按 requestId 丢弃过期结果
            let outcome = session
                .call(env, &mut |frame: &Envelope| {
                    if frame.msg_type == msg::ERROR {
                        print_error(frame);
                        return;
                    }
                    let chunk: ipc::QueryChunk = match frame.decode() {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    if chunk.request_id != request_id {
                        return;
                    }
                    total = chunk.total_estimate;
                    took = chunk.took_ms;
                    for w in &chunk.warnings {
                        if !warnings.contains(w) {
                            warnings.push(w.clone());
                        }
                    }
                    if *json {
                        for hit in &chunk.hits {
                            println!("{}", serde_json::to_string(hit).unwrap_or_default());
                        }
                    } else {
                        for hit in &chunk.hits {
                            printed += 1;
                            print_hit(printed, hit);
                        }
                        let _ = std::io::stdout().flush();
                    }
                })
                .await;
            if let Err(e) = outcome {
                eprintln!("{e}");
                return 1;
            }
            if !*json {
                let elapsed = start.elapsed().as_millis();
                println!(
                    "\n命中约 {} 条，显示 {} 条（服务端 {}ms / 端到端 {}ms）",
                    total, printed, took, elapsed
                );
                if !warnings.is_empty() {
                    println!("提示：{}", warnings.join("；"));
                }
            }
            if printed == 0 && total == 0 {
                println!("没有找到匹配结果。试试更短的关键词，或先用 `snapfind index` 建立索引。");
            }
            0
        }
        Cmd::Preview { target, bytes } => {
            let path = if let Some((vol, doc)) = target.split_once(':') {
                let env = Envelope::request(
                    msg::PREVIEW_META,
                    serde_json::json!({ "volumeId": vol, "docId": doc.parse::<i64>().unwrap_or(-1) }),
                );
                match session.call(env, &mut |_| {}).await {
                    Ok(resp) if resp.msg_type != msg::ERROR => match resp.decode::<ipc::PreviewMeta>() {
                        Ok(meta) => PathBuf::from(meta.path),
                        Err(e) => {
                            eprintln!("元信息解析失败: {e}");
                            return 1;
                        }
                    },
                    _ => {
                        eprintln!("文档不存在: {target}");
                        return 1;
                    }
                }
            } else {
                PathBuf::from(target)
            };
            match preview_file(&path, *bytes) {
                Ok(text) => {
                    println!("== {} ==", path.display());
                    println!("{text}");
                    0
                }
                Err(e) => {
                    eprintln!("预览失败: {e}");
                    1
                }
            }
        }
        Cmd::Status => {
            let env = Envelope::request(msg::STATS_GET, serde_json::json!({}));
            match session.call(env, &mut |_| {}).await {
                Ok(resp) if resp.msg_type != msg::ERROR => {
                    let st: ipc::Stats = match resp.decode() {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("{e}");
                            return 1;
                        }
                    };
                    println!("索引目录 : {}", st.index_root);
                    println!("后端     : {}", st.backend);
                    println!("文档总数 : {}", st.total_docs);
                    println!("索引体积 : {}", human(st.total_index_bytes));
                    println!(
                        "内存     : {} / 预算 {}",
                        human(st.rss_bytes),
                        human(st.memory_budget_bytes)
                    );
                    println!("\n卷状态：");
                    for v in st.volumes {
                        println!(
                            "  {} {:<12} {:<10} 文档 {:>8} 体积 {:>9}",
                            short(&v.volume_id),
                            v.mount,
                            v.state,
                            v.docs,
                            human(v.index_bytes)
                        );
                    }
                    0
                }
                Ok(resp) => {
                    print_error(&resp);
                    1
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        Cmd::Volume { action, id } => {
            let env = Envelope::request(
                msg::VOLUME_ACTION,
                serde_json::json!({ "action": action, "volume_id": id }),
            );
            match session.call(env, &mut |_| {}).await {
                Ok(resp) if resp.msg_type != msg::ERROR => {
                    println!("已执行: {action}");
                    0
                }
                Ok(resp) => {
                    print_error(&resp);
                    1
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        Cmd::Compact { id } => {
            let env = Envelope::request(
                msg::VOLUME_ACTION,
                serde_json::json!({ "action": "compact", "volume_id": id }),
            );
            match session.call(env, &mut |_| {}).await {
                Ok(resp) if resp.msg_type != msg::ERROR => {
                    println!("已整理分段（回收更新留下的垃圾块）");
                    0
                }
                Ok(resp) => {
                    print_error(&resp);
                    1
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        Cmd::Shutdown => {
            session.shutdown(5000).await;
            println!("索引进程已退出");
            0
        }
        Cmd::Open { .. } | Cmd::Reveal { .. } | Cmd::Copy { .. } => 0,
    }
}

fn print_error(env: &Envelope) {
    let payload: ipc::ErrorPayload = env.decode().unwrap_or(ipc::ErrorPayload {
        code: "ERR_INTERNAL".into(),
        message: format!("{}", env.payload),
        detail: None,
        retryable: false,
        volume_id: None,
    });
    eprintln!("错误 {}: {}", payload.code, payload.message);
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

fn short(id: &str) -> String {
    if id.len() <= 9 {
        id.to_string()
    } else {
        format!("{}…", &id[..9])
    }
}

fn strip_markup(s: &str) -> String {
    s.replace("<mark>", "\x1b[43m\x1b[30m")
        .replace("</mark>", "\x1b[0m")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

fn progress_line(p: &ipc::Progress) -> String {
    format!(
        "{} {} {}/{} 文件 {:.1} MB {}",
        p.mount,
        p.state,
        p.files_done,
        p.files_total,
        p.bytes_done as f64 / 1_048_576.0,
        p.eta_sec
            .map(|s| format!("ETA {s}s"))
            .unwrap_or_default()
    )
}

fn print_hit(idx: usize, hit: &ipc::Hit) {
    let when = hit.mtime;
    println!(
        "{:>3}. [{:.2}] {}  ({} , {} )",
        idx,
        hit.score,
        hit.path,
        human(hit.size),
        when
    );
    if hit.flags.encrypted {
        println!("     （已加密，无法预览内容）");
    }
    for s in hit.snippets.iter().take(2) {
        println!("     [{}] {}", s.field, strip_markup(&s.text));
    }
}

fn preview_file(path: &std::path::Path, max: usize) -> anyhow::Result<String> {
    if !path.exists() {
        anyhow::bail!("文件不存在");
    }
    let limits = snapfind_engine::extract::Limits {
        max_bytes: max.max(1024),
        max_text_bytes: max.max(1024),
        timeout_ms: 5_000,
    };
    match snapfind_engine::extract::extract(path, &limits) {
        Ok(t) => Ok(t.text),
        Err(snapfind_engine::extract::ExtractError::Unsupported) => {
            // 未知类型：按纯文本兜底读取
            let data = std::fs::read(path)?;
            let take = data.len().min(max);
            Ok(String::from_utf8_lossy(&data[..take]).to_string())
        }
        Err(e) => anyhow::bail!("{e}"),
    }
}

#[allow(dead_code)]
fn unused_write(_: &mut dyn Write) {}
