//! `snapfind-indexd`：独立索引/查询服务进程。
//!
//! * 传输：Windows 命名管道 / Linux UDS，**不监听 TCP**；
//! * 鉴权：一次性令牌（由 UI 进程经命令行传入），握手后校验，失败即断开；
//! * 生命周期：父进程 stdin EOF、心跳超时或收到 `shutdown` 都会落盘检查点并退出。

use clap::Parser;
use snapfind_core::config::AppConfig;
use snapfind_core::error::ErrorCode;
use snapfind_core::ipc::{
    self, msg, Envelope, Hello, HelloAck, IndexStart, IndexStartAck, ShutdownAck, ShutdownReq,
    VolumeAction,
};
use snapfind_core::platform;
use snapfind_engine::service::{Service, ServiceEvent};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

#[derive(Parser, Debug)]
#[command(
    name = "snapfind-indexd",
    version,
    about = "SnapFind 索引守护进程（由 snapfind 启动）"
)]
struct Args {
    /// 命名管道名（Windows）或 socket 文件名（Linux）
    #[arg(long)]
    pipe: String,
    /// 一次性鉴权令牌（hex）
    #[arg(long)]
    token: String,
    /// 索引目录（覆盖默认解析）
    #[arg(long)]
    index_dir: Option<String>,
}

fn main() {
    let args = Args::parse();
    install_panic_hook();
    // 后台低优先级（文档 §5.11）：索引进程全程 IDLE / nice 19
    platform::set_low_priority();

    // 配置被重置/升级时把原因打到 stderr（桌面端也会自行提示），
    // 避免「设置突然全没了」却没有任何线索。
    let (mut cfg, cfg_warning) = AppConfig::load_with_warning();
    if let Some(w) = &cfg_warning {
        eprintln!("SnapFind(indexd): {w}");
    }
    if let Some(dir) = &args.index_dir {
        cfg.index.root = dir.clone();
    }
    // panic 日志跟着索引目录走（<index>/logs/），与桌面端排空 stderr 得到的
    // indexd.log 同处一处，排障时只需要看一个目录（R3-2 / R3-3）。
    // 注意必须用**解析后**的目录：config.index.root 为空表示「自动」，
    // 直接拿空串会落到进程当前目录下的 logs/。
    let resolved_root = snapfind_core::config::resolve_index_root(
        (!cfg.index.root.is_empty()).then(|| std::path::Path::new(cfg.index.root.as_str())),
    );
    set_panic_log_dir(&resolved_root.path);

    // 启动标记：桌面端把 stderr 落到 <index>/logs/indexd.log，
    // 有这一行才能确认「索引进程确实起过」（R3-2）
    eprintln!(
        "SnapFind(indexd): start v{} index_root={} pipe={}",
        env!("CARGO_PKG_VERSION"),
        resolved_root.path.display(),
        args.pipe
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let code = rt.block_on(run(args, cfg));
    std::process::exit(code);
}

/// panic 日志目录：默认落在配置目录（panic 可能早于索引目录解析），
/// 解析出 `index.root` 后由 [`set_panic_log_dir`] 改指 `<index>/logs/`。
static PANIC_LOG_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

fn set_panic_log_dir(index_root: &std::path::Path) {
    let dir = index_root.join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let _ = PANIC_LOG_DIR.set(dir);
}

/// panic 兜底（计划 R3-3）：panic 信息追加到 `<index>/logs/panic.log`
/// （未解析出索引目录时退回配置目录 `snapfind-panic.log`）。
///
/// indexd 的 stderr 平时被桌面端转发进日志，但 panic 可能发生在 IPC 建立前/断开后
/// （以及 CLI 用户直接跑 indexd 时），那时什么线索都没有——这里独立落盘一份，
/// 不受日志开关影响。文件按 panic 次数增长，每次写入带时间戳与线程名。
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        eprintln!("snapfind-indexd panic: {info}");
        let line = format!(
            "[{}] [{}] {}{}\n---\n",
            chrono_like_now(),
            std::thread::current().name().unwrap_or("<unnamed>"),
            info,
            info.payload()
                .downcast_ref::<&str>()
                .map(|s| format!(" ({s})"))
                .unwrap_or_default(),
        );
        let path = match PANIC_LOG_DIR.get() {
            Some(dir) => dir.join("panic.log"),
            None => AppConfig::path().with_file_name("snapfind-panic.log"),
        };
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = std::io::Write::write_all(&mut f, line.as_bytes());
        }
    }));
}

/// 简单本地时间戳（YYYY-MM-DD HH:MM:SS UTC），避免为崩溃日志引入 chrono 依赖。
fn chrono_like_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let tod = secs % 86_400;
    // 由 Unix 天数推算 Y-M-D（民用算法，无需库）
    let (y, m, d) = {
        let z = days as i64 + 719_468;
        let era = z / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        ((y + if m <= 2 { 1 } else { 0 }) as u32, m as u32, d as u32)
    };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

async fn run(args: Args, cfg: AppConfig) -> i32 {
    // 单实例：索引目录独占锁。
    //
    // UI 重连会立刻拉起新进程，而旧实例往往还在收尾（检查点落盘最多 3s+WAL 刷新），
    // 此时抢锁得到 "operation would block"。原来直接退出(code=2)会让重连必吃一次失败，
    // 重试窗口不够时最终报「索引服务不可用」。这里原地等待旧实例退出，最多 15s。
    // 锁文件必须与实际索引根目录同源：优先 --index-dir，其次 config.index.root，
    // 否则用户在设置页改了索引目录后，锁仍落在默认 index/ 里——
    // 重启会因「旧锁未释放」误判，而真正的新目录却没人加锁
    let configured_root: Option<&std::path::Path> = args
        .index_dir
        .as_deref()
        .map(std::path::Path::new)
        .or(if cfg.index.root.is_empty() {
            None
        } else {
            Some(std::path::Path::new(&cfg.index.root))
        });
    let initial_root = snapfind_core::config::resolve_index_root(configured_root);
    let _lock = {
        let lock_path = initial_root.lock_file();
        let mut waited = Duration::ZERO;
        let mut announced = false;
        loop {
            match platform::try_lock(&lock_path) {
                Ok(f) => break f,
                Err(e) if waited < Duration::from_secs(15) => {
                    if !announced {
                        announced = true;
                        eprintln!("snapfind-indexd: 索引锁被占用（{e}），等待旧实例退出…");
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    waited += Duration::from_millis(250);
                }
                Err(e) => {
                    eprintln!("snapfind-indexd: 已有实例持有索引锁（{e}），等待超时");
                    return 2;
                }
            }
        }
    };

    let (out_tx, out_rx) = mpsc::unbounded_channel::<Envelope>();
    let seq = Arc::new(Mutex::new(0u64));
    let sink_tx = out_tx.clone();
    let sink_seq = seq.clone();
    let sink: snapfind_engine::service::EventSink = Arc::new(move |ev: ServiceEvent| {
        let mut s = sink_seq.lock().unwrap();
        *s += 1;
        let seq_no = *s;
        let env = match ev {
            ServiceEvent::Progress(p) => Envelope::event(msg::EVENT_PROGRESS, p, seq_no),
            ServiceEvent::VolumeState(v) => Envelope::event(msg::EVENT_VOLUME_STATE, v, seq_no),
            ServiceEvent::WatchOverflow { volume_id } => Envelope::event(
                msg::EVENT_WATCH_OVERFLOW,
                serde_json::json!({ "volumeId": volume_id }),
                seq_no,
            ),
            ServiceEvent::Degraded { reason } => Envelope::event(
                msg::EVENT_INDEX_DEGRADED,
                serde_json::json!({ "reason": reason }),
                seq_no,
            ),
        };
        let _ = sink_tx.send(env);
    });

    let service = match Service::open(cfg, sink) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("snapfind-indexd: 初始化失败 {e}");
            return 3;
        }
    };

    // 父进程存活监测：stdin EOF 表示 UI 进程已退出（崩溃/被强杀亦生效）
    let parent_gone = Arc::new(AtomicBool::new(false));
    {
        let flag = parent_gone.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut stdin = tokio::io::stdin();
            let mut buf = [0u8; 64];
            loop {
                match stdin.read(&mut buf).await {
                    Ok(0) => {
                        flag.store(true, Ordering::SeqCst);
                        break;
                    }
                    Ok(_) => continue,
                    Err(_) => {
                        flag.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            }
        });
    }

    let endpoint = platform::ipc_endpoint(&args.pipe);
    if let Err(e) = serve(endpoint, args.token, service.clone(), out_rx, parent_gone).await {
        eprintln!("snapfind-indexd: IPC 结束 {e}");
    }
    // 兜底：无论何种原因退出，都落盘检查点
    let _ = service.shutdown(Duration::from_millis(3000));
    0
}

#[cfg(windows)]
async fn serve(
    endpoint: String,
    token: String,
    svc: Arc<Service>,
    out_rx: UnboundedReceiver<Envelope>,
    parent_gone: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&endpoint)?;
    server.connect().await?;
    run_connection(server, token, svc, out_rx, parent_gone).await
}

#[cfg(unix)]
async fn serve(
    endpoint: String,
    token: String,
    svc: Arc<Service>,
    out_rx: UnboundedReceiver<Envelope>,
    parent_gone: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    use tokio::net::UnixListener;
    let _ = std::fs::remove_file(&endpoint);
    let listener = UnixListener::bind(&endpoint)?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600));
    }
    let (stream, _) = listener.accept().await?;
    run_connection(stream, token, svc, out_rx, parent_gone).await
}

struct Conn {
    svc: Arc<Service>,
    token: String,
    out: UnboundedSender<Envelope>,
    authed: AtomicBool,
    shutting: Arc<AtomicBool>,
    last_inbound: Arc<Mutex<Instant>>,
}

async fn run_connection<S>(
    stream: S,
    token: String,
    svc: Arc<Service>,
    out_rx: UnboundedReceiver<Envelope>,
    parent_gone: Arc<AtomicBool>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut rd, mut wr) = tokio::io::split(stream);
    let (tx, mut rx) = mpsc::unbounded_channel::<Envelope>();

    // 写任务：所有出站帧（服务事件 + 响应）走同一通道，保证顺序
    let writer = tokio::spawn(async move {
        while let Some(env) = rx.recv().await {
            if ipc::write_frame(&mut wr, &env).await.is_err() {
                break;
            }
        }
    });

    // 服务事件 → 连接通道
    {
        let out2 = tx.clone();
        let mut out_rx = out_rx;
        tokio::spawn(async move {
            while let Some(env) = out_rx.recv().await {
                if out2.send(env).is_err() {
                    break;
                }
            }
        });
    }

    let conn = Arc::new(Conn {
        svc: svc.clone(),
        token,
        out: tx.clone(),
        authed: AtomicBool::new(false),
        shutting: Arc::new(AtomicBool::new(false)),
        last_inbound: Arc::new(Mutex::new(Instant::now())),
    });

    // 心跳监测：连续 N 个周期无入站帧即视为 UI 失联（默认 3×5s）
    {
        let c = conn.clone();
        let gone = parent_gone;
        tokio::spawn(async move {
            let cfg = c.svc.cfg.lock().unwrap().lifecycle.clone();
            let interval = Duration::from_millis(cfg.heartbeat_interval_ms.max(1000));
            let threshold = cfg.heartbeat_miss_threshold.max(1) as u32;
            loop {
                tokio::time::sleep(interval).await;
                if gone.load(Ordering::SeqCst) {
                    c.initiate_shutdown("parent_gone").await;
                    return;
                }
                let idle = c.last_inbound.lock().unwrap().elapsed();
                if idle > interval * threshold {
                    c.initiate_shutdown("heartbeat_timeout").await;
                    return;
                }
            }
        });
    }

    loop {
        match ipc::read_frame(&mut rd).await {
            Ok(Some(env)) => {
                *conn.last_inbound.lock().unwrap() = Instant::now();
                if conn.handle(env).await {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }

    let _ = tx.send(Envelope::new(
        msg::BYE,
        serde_json::json!({ "reason": "connection_closed" }),
    ));
    drop(conn);
    let _ = tokio::time::timeout(Duration::from_millis(500), writer).await;
    Ok(())
}

impl Conn {
    async fn initiate_shutdown(&self, reason: &str) {
        if self.shutting.swap(true, Ordering::SeqCst) {
            return;
        }
        let svc = self.svc.clone();
        let timeout =
            Duration::from_millis(svc.cfg.lock().unwrap().lifecycle.shutdown_timeout_ms.max(1000));
        let vols = tokio::task::spawn_blocking(move || svc.shutdown(timeout))
            .await
            .unwrap_or_default();
        let _ = self.out.send(Envelope::new(
            msg::SHUTDOWN_ACK,
            serde_json::to_value(ShutdownAck {
                saved_checkpoint: true,
                volumes: vols,
            })
            .unwrap_or(serde_json::Value::Null),
        ));
        let _ = self.out.send(Envelope::new(
            msg::BYE,
            serde_json::json!({ "reason": reason }),
        ));
        tokio::time::sleep(Duration::from_millis(150)).await;
        std::process::exit(0);
    }

    fn reject(&self, req: &Envelope, code: ErrorCode, message: &str) {
        let _ = self.out.send(req.error(code.as_str(), message));
    }

    /// 返回 true 表示应结束连接。
    async fn handle(&self, env: Envelope) -> bool {
        if env.msg_type == msg::HELLO {
            match env.decode::<Hello>() {
                Ok(hello) => {
                    if hello.token != self.token {
                        self.reject(&env, ErrorCode::AuthFailed, "令牌不匹配");
                        return true;
                    }
                    if !hello
                        .supported_versions
                        .contains(&snapfind_core::PROTOCOL_VERSION)
                    {
                        self.reject(&env, ErrorCode::ProtocolMismatch, "协议版本无交集");
                        return true;
                    }
                    // 前端未提供 build_id 时跳过一致性校验：非 Rust 前端（.NET/Avalonia）
                    // 无法获得编译期常量，空白 build_id 视为「不参与版本绑定」。
                    // 提供则仍然严格比对，继续拦截 Rust 侧新旧二进制混跑。
                    if !hello.build_id.is_empty() && hello.build_id != snapfind_core::BUILD_ID {
                        self.reject(
                            &env,
                            ErrorCode::BuildMismatch,
                            "索引服务与前端版本不一致（禁止新旧二进制混跑）",
                        );
                        return true;
                    }
                    let ack = HelloAck {
                        version: snapfind_core::PROTOCOL_VERSION,
                        backend: snapfind_engine::store::BACKEND_NAME.to_string(),
                        build_id: snapfind_core::BUILD_ID.to_string(),
                        index_root: self.svc.root.path.to_string_lossy().to_string(),
                        degraded_reason: self.svc.root.reason.clone(),
                        pid: std::process::id(),
                    };
                    let _ = self.out.send(env.reply(msg::HELLO_ACK, ack));
                    self.authed.store(true, Ordering::SeqCst);
                    return false;
                }
                Err(e) => {
                    self.reject(
                        &env,
                        ErrorCode::ProtocolMismatch,
                        &format!("hello 解析失败: {e}"),
                    );
                    return true;
                }
            }
        }

        if !self.authed.load(Ordering::SeqCst) {
            self.reject(&env, ErrorCode::AuthFailed, "未握手");
            return true;
        }

        match env.msg_type.as_str() {
            msg::HEARTBEAT => {
                let kind = env
                    .decode::<ipc::Heartbeat>()
                    .map(|h| h.kind)
                    .unwrap_or_else(|_| "ping".into());
                if kind == "ping" {
                    let _ = self.out.send(env.reply(
                        msg::HEARTBEAT,
                        ipc::Heartbeat {
                            kind: "pong".into(),
                            seq: 0,
                        },
                    ));
                }
            }
            msg::VOLUME_LIST => {
                let vols = self.svc.list_volumes();
                let _ = self.out.send(env.reply(msg::VOLUME_LIST, vols));
            }
            msg::SCOPE_PREVIEW => {
                let preview = self.svc.scope_preview();
                let _ = self.out.send(env.reply(msg::SCOPE_PREVIEW, preview));
            }
            msg::SCOPE_REGISTER => {
                let dir = env
                    .payload
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                match self.svc.register_scope_dir(&dir) {
                    Ok(id) => {
                        let _ = self.out.send(
                            env.reply(msg::SCOPE_REGISTER, serde_json::json!({ "volumeId": id })),
                        );
                    }
                    Err(e) => {
                        let _ = self.out.send(env.error(e.code.as_str(), e.message));
                    }
                }
            }
            msg::INDEX_START => {
                let req: IndexStart = env.decode().unwrap_or(IndexStart {
                    volumes: vec![],
                    excludes: vec![],
                });
                match self.svc.start_index(req.volumes, req.excludes) {
                    Ok(started) => {
                        let parallel = self
                            .svc
                            .cfg
                            .lock()
                            .unwrap()
                            .parallel_volumes(started.len());
                        let _ = self.out.send(env.reply(
                            msg::INDEX_START,
                            IndexStartAck {
                                started,
                                parallel_volumes: parallel,
                            },
                        ));
                    }
                    Err(e) => {
                        let _ = self.out.send(env.error(e.code.as_str(), e.message));
                    }
                }
            }
            msg::VOLUME_ACTION => {
                let action: VolumeAction = env.decode().unwrap_or(VolumeAction {
                    action: "skip".into(),
                    volume_id: None,
                    excludes: None,
                    volumes: None,
                });
                match self.svc.volume_action(&action) {
                    Ok(states) => {
                        // 回传每个卷的最终状态，前端据此立即更新卡片（不必等下一帧 stats）
                        let states: std::collections::HashMap<String, String> =
                            states.into_iter().collect();
                        let _ = self.out.send(env.reply(
                            msg::VOLUME_ACTION,
                            serde_json::json!({
                                "ok": true,
                                "action": action.action,
                                "states": states
                            }),
                        ));
                    }
                    Err(e) => {
                        let _ = self.out.send(env.error(e.code.as_str(), e.message));
                    }
                }
            }
            msg::QUERY_PARSE => {
                let q = env
                    .payload
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                match snapfind_engine::query::parse(&q) {
                    Ok(p) => {
                        let _ = self.out.send(env.reply(
                            msg::QUERY_PARSE,
                            serde_json::json!({ "ok": true, "types": p.types, "warnings": p.warnings }),
                        ));
                    }
                    Err(e) => {
                        let _ = self.out.send(env.error(e.code.as_str(), e.message));
                    }
                }
            }
            msg::QUERY_CANCEL => {
                let req: ipc::CancelRequest = env.decode().unwrap_or(ipc::CancelRequest {
                    request_id: String::new(),
                });
                let ok = self.svc.cancel_query(&req.request_id);
                let _ = self.out.send(env.reply(
                    msg::QUERY_CANCEL,
                    serde_json::json!({ "requestId": req.request_id, "cancelled": ok }),
                ));
            }
            msg::QUERY_EXECUTE => {
                let mut req: ipc::QueryRequest = match env.decode() {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = self.out.send(
                            env.error(ErrorCode::QuerySyntax.as_str(), format!("请求解析失败: {e}")),
                        );
                        return false;
                    }
                };
                let cfg = self.svc.cfg.lock().unwrap().clone();
                if req.request_id.is_empty() {
                    req.request_id = env.id.clone();
                }
                if req.timeout_ms.is_none() {
                    req.timeout_ms = Some(cfg.query.timeout_ms);
                }
                let cancel = self.svc.begin_query(&req.request_id);
                let svc = self.svc.clone();
                let out = self.out.clone();
                let req_id = req.request_id.clone();
                let reply_to = env.id.clone();
                tokio::task::spawn_blocking(move || {
                    let mut seq: u64 = 0;
                    let out2 = out.clone();
                    let mut emit = |chunk: ipc::QueryChunk| {
                        let is_final = chunk.phase != "partial";
                        let mut e = Envelope::new(
                            "response.query.execute",
                            serde_json::to_value(&chunk).unwrap_or(serde_json::Value::Null),
                        );
                        e.id = reply_to.clone();
                        e.reply_to = Some(reply_to.clone());
                        e.seq = Some(seq);
                        e.is_final = Some(is_final);
                        seq += 1;
                        let _ = out2.send(e);
                    };
                    if let Err(e) = svc.run_query(&req, &cancel, &mut emit) {
                        let mut err = Envelope::new(
                            msg::ERROR,
                            serde_json::to_value(ipc::ErrorPayload {
                                code: e.code.as_str().to_string(),
                                message: e.message.clone(),
                                detail: e.detail.clone(),
                                retryable: e.code.retryable(),
                                volume_id: None,
                            })
                            .unwrap_or(serde_json::Value::Null),
                        );
                        err.id = reply_to.clone();
                        err.reply_to = Some(reply_to);
                        err.is_final = Some(true);
                        let _ = out.send(err);
                    }
                    svc.end_query(&req_id);
                });
            }
            msg::STATS_GET => {
                let stats = self.svc.stats();
                let _ = self.out.send(env.reply(msg::STATS_GET, stats));
            }
            msg::STATS_EXTENSIONS => {
                let list: Vec<serde_json::Value> = self
                    .svc
                    .ext_stats(40)
                    .into_iter()
                    .map(|(ext, count)| serde_json::json!({ "ext": ext, "count": count }))
                    .collect();
                let _ = self.out.send(env.reply(
                    msg::STATS_EXTENSIONS,
                    serde_json::json!({ "extensions": list }),
                ));
            }
            msg::PREVIEW_META => {
                let volume_id = env
                    .payload
                    .get("volumeId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let doc_id = env
                    .payload
                    .get("docId")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(-1);
                match self.svc.preview_meta(&volume_id, doc_id) {
                    Some(meta) => {
                        let _ = self.out.send(env.reply(msg::PREVIEW_META, meta));
                    }
                    None => {
                        let _ = self.out.send(
                            env.error(ErrorCode::VolumeUnavailable.as_str(), "文档不存在"),
                        );
                    }
                }
            }
            msg::PREVIEW_READ => {
                let path = env
                    .payload
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let max_bytes = env
                    .payload
                    .get("max_bytes")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(32 * 1024 * 1024)
                    .clamp(4096, 256 * 1024 * 1024) as usize;
                let reply_id = env.id.clone();
                let out = self.out.clone();
                // 解析 PDF/Office 可能耗时数百毫秒到数秒，放阻塞线程池执行，
                // 否则会占住异步消息循环，期间心跳与取消都排不上队。
                tokio::task::spawn_blocking(move || {
                    use snapfind_engine::extract::{self, ExtractError, Limits};
                    let p = std::path::Path::new(&path);
                    let ext = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();

                    let (text, truncated, name_only, encoding, error) =
                        if !extract::is_supported(&ext) {
                            (String::new(), false, true, String::new(), None)
                        } else {
                            let limits = Limits {
                                max_bytes,
                                max_text_bytes: max_bytes,
                                timeout_ms: 20_000,
                            };
                            match extract::extract(p, &limits) {
                                Ok(t) => (t.text, t.truncated, false, t.encoding, None),
                                Err(ExtractError::Encrypted) => (
                                    String::new(),
                                    false,
                                    false,
                                    String::new(),
                                    Some("该文档已加密，无法预览内容。".to_string()),
                                ),
                                Err(e) => (
                                    String::new(),
                                    false,
                                    false,
                                    String::new(),
                                    Some(e.to_string()),
                                ),
                            }
                        };

                    let payload = serde_json::json!({
                        "path": path,
                        "text": text,
                        "truncated": truncated,
                        "name_only": name_only,
                        "encoding": encoding,
                        "error": error,
                    });
                    let mut reply = Envelope::new(msg::PREVIEW_READ, payload);
                    reply.id = reply_id.clone();
                    reply.reply_to = Some(reply_id);
                    let _ = out.send(reply);
                });
            }
            msg::CONFIG_GET => {
                let cfg = self.svc.cfg.lock().unwrap().clone();
                let _ = self.out.send(
                    env.reply(msg::CONFIG_GET, serde_json::to_value(cfg).unwrap_or_default()),
                );
            }
            msg::SHUTDOWN => {
                let req: ShutdownReq = env.decode().unwrap_or(ShutdownReq {
                    reason: "user".into(),
                    timeout_ms: None,
                });
                let svc = self.svc.clone();
                let timeout = Duration::from_millis(
                    req.timeout_ms
                        .unwrap_or_else(|| svc.cfg.lock().unwrap().lifecycle.shutdown_timeout_ms)
                        .max(1000),
                );
                let vols = tokio::task::spawn_blocking(move || svc.shutdown(timeout))
                    .await
                    .unwrap_or_default();
                let _ = self.out.send(env.reply(
                    msg::SHUTDOWN_ACK,
                    ShutdownAck {
                        saved_checkpoint: true,
                        volumes: vols,
                    },
                ));
                let _ = self.out.send(Envelope::new(
                    msg::BYE,
                    serde_json::json!({ "reason": req.reason }),
                ));
                tokio::time::sleep(Duration::from_millis(120)).await;
                std::process::exit(0);
            }
            msg::BYE => return true,
            other => {
                let _ = self.out.send(
                    env.error(ErrorCode::Internal.as_str(), &format!("未知消息类型 {other}")),
                );
            }
        }
        false
    }
}
