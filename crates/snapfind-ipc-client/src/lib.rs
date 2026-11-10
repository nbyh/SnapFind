//! UI 进程侧的 IPC 客户端：负责拉起 `snapfind-indexd`、握手、请求-响应、
//! 事件广播与优雅关闭。CLI 与 GUI 共用这一份实现，避免两套语义漂移。
//!
//! * 传输：Windows 命名管道 / Linux UDS，不监听 TCP；
//! * 令牌：随机生成后经命令行传入，握手后本地清零；
//! * 生命周期：Client 持有 indexd 的 stdin，前端进程退出即触发 EOF 自退。

use snapfind_core::ipc::{self, msg, Envelope, Hello};
use snapfind_core::platform;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::Mutex as AsyncMutex;

trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}
type BoxStream = Box<dyn AsyncStream>;

/// 单条请求的响应收集器：流式响应可多帧，直到 `final=true` 或单帧响应为止。
struct Inner {
    pending: Mutex<HashMap<String, UnboundedSender<Envelope>>>,
    events: broadcast::Sender<Envelope>,
}

pub struct Client {
    inner: Arc<Inner>,
    out: UnboundedSender<Envelope>,
    seq: Mutex<u64>,
    /// 保持 indexd 的 stdin 打开；本进程退出时 EOF 即为它的退出信号
    _stdin: Option<std::process::ChildStdin>,
    child: AsyncMutex<Option<std::process::Child>>,
}

impl Client {
    /// 启动索引进程并完成握手（令牌 + 协议版本 + build_id 校验）。
    pub async fn connect(index_dir: Option<&str>) -> anyhow::Result<Self> {
        let pin = format!("{}-{}", std::process::id(), rand::random::<u32>() % 100_000);
        let mut token: String = (0..32)
            .map(|_| format!("{:02x}", rand::random::<u8>()))
            .collect();
        let endpoint = platform::ipc_endpoint(&pin);

        let indexd = indexd_path()?;
        let mut cmd = std::process::Command::new(&indexd);
        cmd.arg("--pipe").arg(&pin).arg("--token").arg(&token);
        if let Some(dir) = index_dir {
            cmd.arg("--index-dir").arg(dir);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit());
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("启动 {} 失败: {e}", indexd.display()))?;
        let stdin = child.stdin.take();

        let mut stream: Option<BoxStream> = None;
        for _ in 0..200 {
            if let Some(s) = try_connect(&endpoint).await {
                stream = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let stream = match stream {
            Some(s) => s,
            None => {
                let _ = child.kill();
                anyhow::bail!("索引进程启动超时（端点 {endpoint}）");
            }
        };

        let (mut rd, wr) = tokio::io::split(stream);
        let inner = Arc::new(Inner {
            pending: Mutex::new(HashMap::new()),
            events: broadcast::channel(512).0,
        });

        // 读泵：按 replyTo 路由到请求，其余（事件/通知）进广播
        {
            let inner = inner.clone();
            tokio::spawn(async move {
                loop {
                    let frame = match ipc::read_frame(&mut rd).await {
                        Ok(Some(f)) => f,
                        _ => break,
                    };
                    let routed = match frame.reply_to.as_ref() {
                        Some(rid) => {
                            let tx = inner.pending.lock().unwrap().get(rid).cloned();
                            match tx {
                                Some(tx) => {
                                    let _ = tx.send(frame.clone());
                                    true
                                }
                                None => false,
                            }
                        }
                        None => false,
                    };
                    if !routed {
                        let _ = inner.events.send(frame);
                    }
                }
            });
        }

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Envelope>();
        tokio::spawn(async move {
            let mut wr = wr;
            while let Some(env) = out_rx.recv().await {
                if ipc::write_frame(&mut wr, &env).await.is_err() {
                    break;
                }
            }
        });

        let hello = Envelope::request(
            msg::HELLO,
            Hello {
                token: token.clone(),
                build_id: snapfind_core::BUILD_ID.to_string(),
                supported_versions: vec![snapfind_core::PROTOCOL_VERSION],
                encodings: vec!["json".into()],
                role: "ui".into(),
            },
        );
        token.clear();

        let (ack_tx, mut ack_rx) = mpsc::unbounded_channel::<Envelope>();
        let hello_id = hello.id.clone();
        inner.pending.lock().unwrap().insert(hello_id, ack_tx);
        out_tx.send(hello)?;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match tokio::time::timeout_at(deadline, ack_rx.recv()).await {
                Ok(Some(f)) if f.msg_type == msg::HELLO_ACK => {
                    inner.pending.lock().unwrap().clear();
                    break;
                }
                Ok(Some(f)) if f.msg_type == msg::ERROR => {
                    anyhow::bail!("握手失败: {}", f.payload)
                }
                Ok(Some(_)) => continue,
                _ => anyhow::bail!("握手超时"),
            }
        }

        Ok(Self {
            inner,
            out: out_tx,
            seq: Mutex::new(0),
            _stdin: stdin,
            child: AsyncMutex::new(Some(child)),
        })
    }

    fn next_seq(&self) -> u64 {
        let mut s = self.seq.lock().unwrap();
        *s += 1;
        *s
    }

    pub async fn send(&self, mut env: Envelope) -> anyhow::Result<String> {
        if env.seq.is_none() {
            env.seq = Some(self.next_seq());
        }
        let id = env.id.clone();
        self.out.send(env).map_err(|_| anyhow::anyhow!("IPC 已关闭"))?;
        Ok(id)
    }

    /// 单帧响应请求。
    pub async fn request(
        &self,
        msg_type: &str,
        payload: serde_json::Value,
    ) -> anyhow::Result<Envelope> {
        self.call(Envelope::request(msg_type, payload), &mut |_| {})
            .await
    }

    /// 请求并收集响应；`on_frame` 会收到每一帧（流式查询的每一批），
    /// 返回值为最后一帧（final 或单帧响应）。
    pub async fn call<F: FnMut(&Envelope)>(
        &self,
        env: Envelope,
        on_frame: &mut F,
    ) -> anyhow::Result<Envelope> {
        let id = env.id.clone();
        let (tx, mut rx) = mpsc::unbounded_channel::<Envelope>();
        self.inner.pending.lock().unwrap().insert(id.clone(), tx);
        self.send(env).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
        let result = loop {
            let frame = match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(f)) => f,
                Ok(None) => break Err(anyhow::anyhow!("索引进程已断开")),
                Err(_) => break Err(anyhow::anyhow!("请求超时")),
            };
            let is_final = frame.is_final.unwrap_or(true);
            on_frame(&frame);
            if is_final {
                break Ok(frame);
            }
        };
        self.inner.pending.lock().unwrap().remove(&id);
        result
    }

    /// 事件订阅（进度、卷状态、降级、监控溢出）。
    pub fn events(&self) -> broadcast::Receiver<Envelope> {
        self.inner.events.subscribe()
    }

    /// 心跳：保持连接活跃，同时让 indexd 判定前端存活。
    pub async fn heartbeat(&self) -> anyhow::Result<()> {
        self.send(Envelope::request(
            msg::HEARTBEAT,
            serde_json::json!({ "kind": "ping" }),
        ))
        .await?;
        Ok(())
    }

    /// 请求取消（requestId 版本比对，旧结果前端直接丢弃）。
    pub async fn cancel(&self, request_id: &str) -> anyhow::Result<()> {
        self.send(Envelope::request(
            msg::QUERY_CANCEL,
            serde_json::json!({ "requestId": request_id }),
        ))
        .await?;
        Ok(())
    }

    /// 优雅关闭：shutdown → 等 ack → 超时强杀（索引已保存至最近检查点）。
    pub async fn shutdown(&self, timeout_ms: u64) {
        let req = Envelope::request(
            msg::SHUTDOWN,
            serde_json::json!({ "reason": "user", "timeoutMs": timeout_ms }),
        );
        let _ = self.call(req, &mut |_| {}).await;
        if let Some(child) = self.child.lock().await.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => {}
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
    }
}

pub fn indexd_path() -> anyhow::Result<PathBuf> {
    // 显式覆盖：集成测试用 `CARGO_BIN_EXE_snapfind-indexd` 指过来，
    // 生产端排查时也可临时指向别的构建产物（与 C# 侧 LocateIndexd 同名变量）
    if let Ok(p) = std::env::var("SNAPFIND_INDEXD") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
    }
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("无法定位可执行文件目录"))?;
    let name = if cfg!(windows) {
        "snapfind-indexd.exe"
    } else {
        "snapfind-indexd"
    };
    let candidate = dir.join(name);
    if candidate.exists() {
        Ok(candidate)
    } else {
        Ok(PathBuf::from(name))
    }
}

#[cfg(windows)]
async fn try_connect(endpoint: &str) -> Option<BoxStream> {
    use tokio::net::windows::named_pipe::ClientOptions;
    match ClientOptions::new().open(endpoint) {
        Ok(c) => Some(Box::new(c)),
        Err(_) => None,
    }
}

#[cfg(unix)]
async fn try_connect(endpoint: &str) -> Option<BoxStream> {
    match tokio::net::UnixStream::connect(endpoint).await {
        Ok(c) => Some(Box::new(c)),
        Err(_) => None,
    }
}
