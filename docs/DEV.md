# SnapFind 开发说明文档

| 项目 | 内容 |
| --- | --- |
| 产品名 | SnapFind |
| 中文宣传语 | SnapFind：开源、轻量、本地优先的全文搜索工具。 |
| 英文宣传语 | SnapFind — Open-source, lightweight, local-first full-text search. |
| 文档版本 | v0.1（基线评审稿） |
| 日期 | 2026-11-02 |
| 目标平台 | Windows x64、Windows arm64、Linux x64、Linux arm64（不做 macOS，见 §1.3、§13.2） |
| 技术栈基线 | Rust + Tauri v2（UI 进程 `snapfind`）+ 独立索引进程 `snapfind-indexd` + React/TypeScript 前端 |
| 许可证基线 | Apache-2.0（见 §16.6 选型理由） |

> 需求未明确处以「**假设：**」显式标注。实施以本文档为准；决策变更须同步修订并递增版本号。

> **实现现状提示**：桌面 UI 已由本文档描述的 Rust + Tauri / eframe 方案迁移到 **.NET 8 + Avalonia**（`SnapFind.Desktop/`）。原 `crates/snapfind-desktop`（eframe/egui GUI）与 `crates/snapfind-tray`（独立托盘进程）两个 crate 已从 workspace 移除并删除源码；文中涉及 `snapfind-gui`、egui/winit、`snapfind-tray.exe` 的段落属于**历史设计记录**，与当前代码不一致处以 `README.md` 与 `SnapFind.Desktop/` 为准。

---

## 1. 项目概述

### 1.1 定位

SnapFind：开源、轻量、本地优先的全文搜索工具。
SnapFind — Open-source, lightweight, local-first full-text search.

SnapFind 是一款开源、轻量、隐私优先的本地文件全文搜索桌面软件。默认完全本地运行：不上传文件内容、不强制登录、不扫码、不云校验、不付费解锁预览；源码开源、可审计、可自行编译扩展。

### 1.2 重点解决的问题域

| 问题域 | 痛点 | SnapFind 的承诺 |
| --- | --- | --- |
| 预览受阻 | 预览被扫码/登录/付费墙阻断 | 预览零门槛：不扫码、不登录、不付费、无云端校验 |
| 索引时间长 | 首次全盘索引耗时长、不可中断 | 默认全盘但可中断、可恢复（检查点断点续建），多卷并行 + 增量 |
| 索引内存占用大 | 索引过程内存飙升 | 流式/批处理，全局内存上限可配（默认 512MB），超限跳过并记录 |
| 索引占磁盘大 | 索引体积膨胀 | 压缩倒排、只保留必要 token/偏移、分片冷热分层、体积统计与清理工具 |

### 1.3 目标与非目标

**目标（In Scope）**

1. 文件名 / 路径 / 内容三域全文搜索：关键词、短语、布尔（AND/OR/NOT）、通配符；正则与模糊为可选开关（默认关闭）。
2. 覆盖纯文本、代码、日志、Markdown、HTML、RTF、PDF、DOCX、XLSX、PPTX、EPUB；OCR 为可选插件，**默认不启用**。
3. 预览零门槛 + 结果高亮 + 命中片段预览 + 打开文件 / 打开所在文件夹 / 复制路径。
4. 过滤器（类型/大小/修改时间/路径）、排序（相关度/名称/大小/修改时间）、虚拟滚动、搜索历史、快捷键。
5. 独立索引进程 + 本机 IPC（Windows 命名管道 / Linux UDS），不监听 TCP。
6. 多卷独立状态机与独立控制，后台低优先级索引 + IO 节流。
7. UI 主线程零阻塞（可测指标见 §10.2）。
8. 关闭主界面 = 关闭索引进程（默认且唯一默认行为）。

**非目标（Out of Scope，不得变相引入）**

| 非目标 | 说明 |
| --- | --- |
| macOS 构建/打包/签名/发布 | 不做；CI 中不得出现任何 macOS 任务（§13.2） |
| 云同步 / 云端索引 / 远程检索 | 不做 |
| 账号体系 / 登录 / 扫码 / 设备绑定 | 不做 |
| 付费墙 / 预览解锁 / 云端校验 | 不做 |
| 默认启用 OCR | OCR 仅作可选插件，默认关闭 |
| 默认启用遥测 | 必须默认关闭且需用户显式开启 |
| 文件内容外发 | 任何情况下均不上传文件内容 |

### 1.4 隐私承诺（对外可引用）

1. **不出网**：默认构建不含网络请求代码路径；索引、查询、预览、配置、日志全部本地保存。
2. **不上传**：文件内容、文件名、路径均不离开本机。
3. **不强制**：不强制登录、扫码、云校验、付费解锁。
4. **可审计**：源码开源，提供构建文档，可自行编译验证（§13.6）。
5. **可删除**：卸载或「清理索引」后，索引目录、日志、配置可被完整删除，无残留驻留进程。
6. **可自查**：提供 `--privacy-audit` 子命令，扫描二进制与配置中的网络相关设置并输出报告。

---

## 2. 术语与缩写

| 术语/缩写 | 全称或含义 |
| --- | --- |
| SnapFind | 本产品名 |
| `snapfind` | UI 进程可执行文件（Tauri 主进程 + WebView） |
| `snapfind-indexd` | 独立索引/查询服务进程（daemon 名，非系统常驻服务） |
| IPC | 进程间通信（本机命名管道 / Unix Domain Socket） |
| UDS | Unix Domain Socket |
| 卷 / Volume | 一个可访问的逻辑卷，是索引调度与生命周期的最小单位 |
| volumeId | 卷稳定标识，`vol-<hex16>`，索引目录分片以此命名 |
| 分片 / Segment | 卷内一组已冻结的不可变数据文件；写入后只读 |
| 热层 / hot | 近期增量段，达阈值后合并进 segments |
| 冷层 / segments | 已合并、已压缩、只读的段集合 |
| meta.db | 全局元数据库（卷表、schema 版本、buildId、统计、事件） |
| docs.db | 单卷文档存储（路径、大小、mtime、类型、标志位） |
| state.json | 单卷状态机持久状态（含 checkpoint 游标） |
| WAL | Write-Ahead Log，崩溃安全写入机制 |
| FTS5 | SQLite 全文检索扩展模块（候选后端之一） |
| Tantivy | Rust 全文检索库（候选后端之一） |
| Roaring | 压缩位图，用于 docId 集合与过滤器下推 |
| BM25 | 相关度评分函数 |
| 信封 / Envelope | §4.2 定义的统一消息外框 `{v,id,type,ts,payload,replyTo?,seq?,final?}` |
| requestId | 一次查询/操作的唯一标识，用于版本比对与旧结果丢弃 |
| seq | 流式响应分片序号（从 0 递增） |
| final | 流式响应结束标记 |
| 抽取 / Extract | 将文件内容转换为纯文本的过程 |
| 检查点 / Checkpoint | 索引进度的持久记录，用于中断后断点续建 |
| 节流档位 | IO 节流等级：low / medium / high（默认 low） |
| FS 事件 | 文件系统变更事件（ReadDirectoryChangesW / inotify） |
| 事件溢出 | FS 事件队列溢出，需降级为目录重扫 |
| 骨架屏 | 预览加载中的占位 UI |
| 虚拟滚动 | 只渲染可视区域列表项的技术 |
| MSI / NSIS | Windows 安装包格式 |
| deb / rpm / AppImage | Linux 安装包格式 |
| Job Object | Windows 进程组对象，用于 IO 限速与子进程生命周期绑定 |
| cgroup v2 | Linux 控制组 v2，用于 IO/CPU 限额（可选） |
| p50 / p95 / p99 | 百分位延迟 |

---

## 3. 总体架构

### 3.1 进程模型图（ASCII）

```
┌────────────────────────────────────────────────────────────────────────────────┐
│                   操作系统（Windows x64/arm64 · Linux x64/arm64）                │
│                                                                                │
│  ┌─────────────────────────────────┐      ┌─────────────────────────────────┐  │
│  │ 进程 A: snapfind（UI 进程）       │      │ 进程 B: snapfind-indexd          │  │
│  │ Tauri v2 主进程 (Rust)          │      │ 无窗口后台进程 (Rust)             │  │
│  │                                 │      │                                 │  │
│  │ ┌─────────────────────────────┐ │      │ ┌─────────────────────────────┐ │  │
│  │ │ WebView 渲染进程             │ │      │ │ IPC Server（单连接）         │ │  │
│  │ │ React 18 + TS + 虚拟滚动     │ │      │ │ NamedPipe / UDS             │ │  │
│  │ │  搜索框/结果/预览/设置/面板   │ │      │ │ 令牌鉴权 + 版本协商           │ │  │
│  │ └───────────┬─────────────────┘ │      │ └────────────┬────────────────┘ │  │
│  │             │ invoke (async)    │      │              │                  │  │
│  │ ┌───────────▼─────────────────┐ │      │ ┌────────────▼────────────────┐ │  │
│  │ │ Tauri Commands (async)       │ │      │ │ 控制平面                     │ │  │
│  │ │ 无同步 IO / 无同步 IPC 等待   │ │      │ │  卷状态机 ×N / 调度 / 节流    │ │  │
│  │ └───────────┬─────────────────┘ │      │ │  内存预算 / checkpoint        │ │  │
│  │             │                   │      │ └────────────┬────────────────┘ │  │
│  │ ┌───────────▼─────────────────┐ │ IPC  │ ┌────────────▼────────────────┐ │  │
│  │ │ IPC Client (tokio, async)   │◄─┼─────┼─►│ 查询引擎（异步）             │ │  │
│  │ │ 单长连接 + 心跳 + 流式聚合    │ │ 本机 │ │  解析/下推/评分/分页/取消     │ │  │
│  │ └─────────────────────────────┘ │ 传输 │ └────────────┬────────────────┘ │  │
│  │                                 │      │              │                  │  │
│  │ ┌─────────────────────────────┐ │      │ ┌────────────▼────────────────┐ │  │
│  │ │ 预览读取器（直接读源文件）    │ │      │ │ 写入管线                     │ │  │
│  │ │  分块 256KB，不经 IPC        │ │      │ │  Scan→Extract→Tokenize       │ │  │
│  │ └─────────────────────────────┘ │      │ │  →hot→merge→checkpoint       │ │  │
│  │                                 │      │ └────────────┬────────────────┘ │  │
│  │ ┌─────────────────────────────┐ │      │              │                  │  │
│  │ │ 本地配置/历史 (JSON/SQLite)  │ │      │ ┌────────────▼────────────────┐ │  │
│  │ └─────────────────────────────┘ │      │ │ 存储抽象 IndexBackend         │ │  │
│  └─────────────────────────────────┘      │ │  SQLite FTS5（默认）          │ │  │
│                                           │ │  Tantivy（可选）             │ │  │
│  ┌─────────────────────────────────┐      │ └────────────┬────────────────┘ │  │
│  │ OS 集成                          │      │              │                  │  │
│  │  Job Object / nice+ionice       │      │ ┌────────────▼────────────────┐ │  │
│  │  FS Watch (RDCW / inotify)      │      │ │ 索引目录                     │ │  │
│  │  文件管理器打开 / 剪贴板         │      │ │  meta.db · lock · tmp        │ │  │
│  └─────────────────────────────────┘      │ │  vol-<id>/{segments,hot,     │ │  │
│                                           │ │   docs.db,state.json,tmp}    │ │  │
│                                           │ └─────────────────────────────┘ │  │
│                                           └─────────────────────────────────┘  │
│  ⚠ 进程 B 由进程 A 独占启动与独占连接；A 退出 → B 退出（§4.8）。不监听 TCP。     │
└────────────────────────────────────────────────────────────────────────────────┘
```

### 3.2 组件职责表

| 组件 | 进程 | 职责 | 不得承担 |
| --- | --- | --- | --- |
| `ui-shell` | A | 窗口/WebView 托管、路由、主题、国际化 | 任何同步 IO |
| `ui-search-view` | A | 搜索框（防抖 ≤150ms）、结果虚拟列表、过滤器面板 | 大数组同步计算 |
| `ui-preview-view` | A | 分块读取、骨架屏、高亮渲染（HTML 转义） | 大文件一次性读入 |
| `ui-settings-view` | A | 索引范围、排除规则、节流档位、内存上限、托盘开关（含文字说明） | 直接写索引文件 |
| `ui-index-panel` | A | 卷状态面板（§18.2）、暂停/恢复/停止/重建/清理 | 同步等待 IPC |
| `ipc-client` | A | 单长连接、令牌握手、心跳、请求响应关联、流式聚合、requestId 版本比对 | 阻塞调用线程 |
| `preview-reader` | A | 分块读源文件（不经 IPC） | 修改文件 |
| `ipc-server` | B | 单连接、鉴权、版本协商、分发、心跳 | 监听 TCP |
| `control-plane` | B | 卷发现、状态机、调度、节流、内存预算、checkpoint 编排 | UI 渲染 |
| `scan-worker` | B | 目录遍历、排除规则匹配、FS 事件消费 | 写段文件 |
| `extract-dispatcher` | B | 按类型分派抽取器，超时/超限保护，PDF/Office 惰性加载 | 解析网络资源 |
| `write-pipeline` | B | 分词、批写 hot、段合并、WAL/原子替换、checkpoint | 直接响应 UI |
| `query-engine` | B | 语法解析、下推、评分、排序、分页、流式、取消/超时 | 阻塞 IPC 事件循环 |
| `storage-backend` | B | `IndexBackend` trait 实现 | 关心 IPC |
| `fs-watch` | B | RDCW / inotify 封装、溢出降级重扫 | 跨卷状态耦合 |
| `metrics` | B | 体积/内存/延迟统计，供基准脚本采集 | 出网 |

### 3.3 四类数据流

**（1）索引数据流**

```
卷发现 → 范围确认（UI 一次性页）→ scan-worker 流式遍历目录树
 → 排除规则过滤 → 变更判定（mtime+size，可选内容哈希）
 → extract-dispatcher（流式/分块读取 + 超时/超限保护）
 → tokenizer（流式产出 token + 偏移，丢弃停用词，长度上限）
 → write-pipeline 批处理（默认 512 文件/批 或 8MB 文本/批，先到先触发）
 → hot 段写入（WAL + 原子替换）→ docs.db 更新 → 统计更新
 → 达阈值触发 merge → segments（压缩倒排 + Roaring + 偏移压缩）
 → checkpoint（游标 + 计数）→ state.json 原子写
```
批次边界检查：取消标志、内存上限、暂停标志、节流令牌。

**（2）查询数据流**

```
UI 输入（防抖 ≤150ms）→ 生成 requestId → query.execute（含过滤器/排序/游标）
 → 语法解析（错误 → ERR_QUERY_SYNTAX）
 → 过滤器下推（类型/大小/时间/路径 → docId 位图，先于评分）
 → 后端检索（多段并发 + 段内并行，超时预算控制）
 → 评分（BM25 + 字段权重 + 名称/路径加成）→ 排序 → 取窗口
 → 首批（≤200ms）返回 final:false → 分批追加 → final:true
 → UI 按 requestId 版本比对：非当前 requestId 的分片立即丢弃
```

**（3）预览数据流**

```
UI 点击结果 → preview.meta（仅传 path + 命中偏移，不经 IPC 传大文本）
 → UI 侧 preview-reader 分块（256KB/块）异步读源文件
 → 抽取器按需惰性加载（PDF/Office）→ 文本分块送渲染层
 → HTML 转义 + 高亮（仅片段窗口内）→ 骨架屏消失
 → 打开 / 打开所在文件夹 / 复制路径 走 OS 集成（不上传、不校验、不登录）
```

**（4）控制数据流**

```
设置变更 → config.apply（增量字段）→ indexd 热更新（节流/并行度/内存上限/排除规则）
卷操作 → volume.action（start/pause/resume/stop/rebuild/clean/setExcludes）→ 状态迁移 → event.volumeState
生命周期 → hello → heartbeat(5s) → shutdown → 落盘检查点 → shutdown.ack/bye → 退出
监控 → fs-watch 事件 → 增量队列 → 去抖 1s + 按目录合并 → 增量索引批次
```

### 3.4 后端可替换抽象（`IndexBackend` trait）

```rust
// 后端抽象：IPC 协议与后端完全解耦，协议中不得出现后端专有字段
pub trait IndexBackend: Send + Sync + 'static {
    const NAME: &'static str;                      // "sqlite-fts5" | "tantivy"
    fn open(cfg: BackendCfg) -> Result<Self> where Self: Sized;
    fn schema_version(&self) -> u32;
    fn migrate(&mut self, from: u32) -> Result<()>;

    // 写路径
    fn begin_batch(&mut self, vol: VolumeId) -> Result<BatchId>;
    fn upsert_doc(&mut self, b: BatchId, d: DocRecord) -> Result<()>;
    fn delete_doc(&mut self, b: BatchId, doc_id: DocId) -> Result<()>;
    fn commit_batch(&mut self, b: BatchId) -> Result<CommitReceipt>;   // 段号 + 校验和
    fn merge(&mut self, vol: VolumeId, policy: MergePolicy) -> Result<MergeReport>;
    fn checkpoint(&mut self, vol: VolumeId) -> Result<Checkpoint>;

    // 读路径
    fn search(&self, q: NormalizedQuery, opt: SearchOpt, budget: Budget)
        -> impl Stream<Item = Result<HitBatch>>;
    fn stats(&self, vol: Option<VolumeId>) -> Result<IndexStats>;
    fn verify(&self, vol: VolumeId) -> Result<VerifyReport>;           // 校验和 / 损坏检测
    fn compact_or_rebuild(&mut self, vol: VolumeId) -> Result<()>;
    fn close(&mut self) -> Result<()>;                                 // 崩溃安全关闭
}
```

选型（3 选 1）：

| 选项 | 优点 | 缺点 | 结论 |
| --- | --- | --- | --- |
| A. SQLite FTS5（自管分片 + 外部内容表） | 四目标零外部依赖、arm64 官方支持；WAL 崩溃安全成熟；内存可控；`offsets` 支持高亮；体积可通过外部内容表 + 段压缩控制 | BM25 需调优；段合并需自研 | **推荐（默认后端）** |
| B. Tantivy | Rust 原生；BM25/短语/分面成熟；段合并内建 | 编译时间与二进制体积大；mmap 在 Windows 上内存账目不透明；arm64 需额外验证 | **可选后端**（v1.1，默认关闭） |
| C. 自研倒排 | 格式完全可控 | 正确性成本高、周期长 | 不采用 |

**推荐 A。理由**：跨平台四目标可用性最高（直接决定 CI 能否通过）；WAL 与崩溃安全是可靠性要求的现成答案；增量增删成本低；体积可通过「外部内容表 + 分离 `docs.db` + 段级 zstd」控制。
**风险**：中文分词需额外集成；段合并需自研。**缓解**：分词器做成本地可替换组件（`Tokenizer` trait），中文默认 bigram、可选词典分词插件；合并策略自研但复用后端 `merge` 接口。

---

## 4. 进程与 IPC 协议规范

### 4.1 传输与安全

| 项 | Windows | Linux |
| --- | --- | --- |
| 传输 | 命名管道 `\\.\pipe\snapfind-<userSidHash>-<sessionId>-<pid>` | UDS `$XDG_RUNTIME_DIR/snapfind-<uid>/ipc.sock`；缺省 `/run/user/<uid>/snapfind/ipc.sock`；再降级 `/tmp/snapfind-<uid>/ipc.sock` |
| 监听 | **不监听 TCP** | **不监听 TCP** |
| 访问控制 | SECURITY_ATTRIBUTES DACL 仅允许当前用户 SID + SYSTEM；连接后 `GetNamedPipeClientProcessId` 校验子进程 PID | socket 文件 `0600`，父目录 `0700`；`SO_PEERCRED` 校验对端 uid == 自身 uid |
| 一次性令牌 | UI 生成 32 字节 CSPRNG → hex(64)；经 `CreateProcess` 命令行 `--token <hex>` 传入；握手后进程内立即清零；**每个新连接必须重新握手** | 同左（argv 传入）；不写入环境变量 |
| 令牌泄露面 | 同用户可读 `/proc` 与命令行；同用户即同权限，接受该风险 | 同左 |
| 版本协商 | `hello.supportedVersions` 交集取最高；无交集 → `ERR_PROTOCOL_MISMATCH` | 同左 |
| 禁止新旧混跑 | `hello.buildId` 必须等于 `meta.db.build_id`；不等 → `ERR_BUILD_MISMATCH`，indexd 拒绝启动并退出 | 同左 |
| 连接数 | 1（第二连接拒绝 `ERR_AUTH_FAILED`） | 同左 |
| 父子绑定 | indexd 置于 UI 的 Job Object（UI 崩溃 → OS 终止子进程，兜底） | indexd 设置 `PR_SET_PDEATHSIG=SIGTERM`（兜底） |
| 帧格式 | 4 字节大端长度 + UTF-8 JSON（默认）；可选 MessagePack（`hello.encodings` 协商） | 同左 |
| 单帧上限 | 8 MiB（超出 → `ERR_PAYLOAD_TOO_LARGE`） | 同左 |

**假设：** 单帧上限 8 MiB；预览文本不经 IPC 传输，故该上限足够。

### 4.2 消息信封（Envelope）

```json
{
  "v": 1,
  "id": "01J9Z7Q8W3E4R5T6Y7U8I9O0",
  "type": "query.execute",
  "ts": 1762000000123,
  "payload": { },
  "replyTo": null,
  "seq": null,
  "final": null
}
```

| 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- |
| `v` | int | 是 | 协议版本号，由握手确定；每帧携带便于诊断 |
| `id` | string | 是 | 消息唯一 ID（ULID）；响应与请求 `id` 相同；一次请求的所有流式分片共享同一 `id` |
| `type` | string | 是 | 消息类型，见 §4.3 |
| `ts` | int | 是 | 发送方毫秒时间戳（Unix epoch ms） |
| `payload` | object | 是 | 类型相关负载，见 §4.4 / §4.5 |
| `replyTo` | string\|null | 否 | 响应填对应请求的 `id`；请求为 `null` |
| `seq` | int\|null | 否 | 流式分片序号，从 0 单调递增；非流式为 `null` |
| `final` | bool\|null | 否 | 流式最后一帧为 `true`；非流式为 `null` |

规则：

1. 每个 request **必回** 一个 `response.*`（成功）或 `error`（失败），二者互斥且必有其一。
2. 流式响应以 `final:true` **或** 一条 `response.cancelled` 结束；不得出现既无 `final` 也无 `cancelled` 的悬挂请求。
3. 事件（`event.*`）为单向推送，不需要响应；`event.*` 可用 `seq` 表示顺序但不要求 `final`。

### 4.3 消息类型清单

| type | 方向 | 流式 | 说明 |
| --- | --- | --- | --- |
| `hello` | UI → ID | 否 | 握手：版本协商 + 令牌 + buildId |
| `hello.ack` | ID → UI | 否 | 握手确认，返回协商版本、能力集、后端名 |
| `error` | 双向 | 否 | 统一错误响应（`replyTo` 指回请求） |
| `heartbeat` | 双向 | 否 | 5s 一次，`ping` / `pong` |
| `config.get` / `config.apply` | UI → ID | 否 | 读取 / 增量应用配置 |
| `volume.list` | UI → ID | 否 | 列出已发现卷及状态 |
| `volume.discover` | UI → ID | 否 | 重新探测可访问卷（供范围确认页） |
| `volume.action` | UI → ID | 否 | `start/pause/resume/stop/rebuild/clean/setExcludes` |
| `volume.state` | ID → UI | 否 | 状态迁移结果（也作为事件推送） |
| `index.scope.preview` | UI → ID | 否 | 范围确认页预估（卷/文件系统/容量/量级/排除项） |
| `scope.register` | UI → ID | 是 | `{path}` → `{volumeId}`：把任意目录登记为独立索引范围 |
| `stats.extensions` | UI → ID | 是 | `{extensions:[{ext,count}]}`：搜索页「类型」多选标签数据 |
| `index.start` | UI → ID | 否 | 按确认后的范围启动首次索引 |
| `query.parse` | UI → ID | 否 | 语法预校验 |
| `query.execute` | UI → ID | **是** | 执行查询，流式返回命中 |
| `query.cancel` | UI → ID | 否 | 协作式取消（按 requestId） |
| `query.suggest` | UI → ID | 否 | 历史 / 路径补全（本地） |
| `preview.meta` | UI → ID | 否 | 文档元信息（类型、大小、编码、是否可抽取、命中偏移） |
| `history.list` / `history.clear` | UI → ID | 否 | 搜索历史（本地存储） |
| `stats.get` | UI → ID | 否 | 索引体积 / 内存 / 文档数 / 段数 / 吞吐 |
| `maintenance.verify` | UI → ID | **是** | 分片校验和验证，流式进度 |
| `maintenance.compact` | UI → ID | **是** | 段合并 / 压缩 |
| `maintenance.clean` | UI → ID | **是** | 清理（按卷 / 全量） |
| `event.progress` | ID → UI | 事件 | 索引进度（节流 ≤50ms/次） |
| `event.volumeState` | ID → UI | 事件 | 卷状态迁移 |
| `event.watchOverflow` | ID → UI | 事件 | FS 事件溢出，降级重扫 |
| `event.memoryPressure` | ID → UI | 事件 | 内存上限触发 |
| `event.error` | ID → UI | 事件 | 非致命错误聚合（权限不足 / 抽取失败等） |
| `event.indexDegraded` | ID → UI | 事件 | 索引目录降级（不可写 → 备用目录） |
| `shutdown` | UI → ID | 否 | 请求关闭（§4.8） |
| `shutdown.ack` | ID → UI | 否 | 已落盘检查点，可退出 |
| `bye` | 双向 | 否 | 连接优雅关闭 |

### 4.4 `query.execute` payload 字段表

**请求（UI → ID）**

```json
{
  "v": 1,
  "id": "01J9Z7Q8W3E4R5T6Y7U8I9O0",
  "type": "query.execute",
  "ts": 1762000000123,
  "replyTo": null, "seq": null, "final": null,
  "payload": {
    "requestId": "01J9Z7Q8W3E4R5T6Y7U8I9O0",
    "query": "foo AND (bar OR \"baz qux\") NOT *.log",
    "mode": { "regex": false, "fuzzy": false, "fuzzyDistance": 1, "caseSensitive": false, "wholeWord": false },
    "fields": { "name": true, "path": true, "content": true },
    "filters": {
      "types": ["pdf", "docx"],
      "excludeTypes": [],
      "size": { "min": 0, "max": 104857600 },
      "mtime": { "from": "2025-01-01T00:00:00Z", "to": null },
      "paths": ["C:\\Users\\a\\docs", "D:\\work"],
      "excludePaths": ["**/node_modules/**", "**/.git/**"],
      "volumes": ["vol-3f2a1b9c0d8e7f60", "vol-9c1d2e3f4a5b6c7d"]
    },
    "sort": { "key": "relevance", "order": "desc" },
    "paging": { "offset": 0, "limit": 50 },
    "highlight": { "enable": true, "snippetLen": 160, "maxSnippets": 3, "pre": "<mark>", "post": "</mark>" },
    "timeoutMs": 2000,
    "firstBatchHintMs": 150
  }
}
```

| 字段 | 类型 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- | --- |
| `requestId` | string | 是 | — | UI 生成；用于版本比对，旧 requestId 分片立即丢弃 |
| `query` | string | 是 | — | 原始查询串（语法见 §7.1） |
| `mode.regex` | bool | 是 | false | 正则开关（默认关闭） |
| `mode.fuzzy` | bool | 是 | false | 模糊开关（默认关闭） |
| `mode.fuzzyDistance` | int | 否 | 1 | 编辑距离上限（1–2） |
| `mode.caseSensitive` | bool | 否 | false | 大小写敏感 |
| `mode.wholeWord` | bool | 否 | false | 全词匹配 |
| `fields.name/path/content` | bool | 是 | true | 检索域开关 |
| `filters.types` | string[] | 否 | [] | 类型白名单（空=不限） |
| `filters.excludeTypes` | string[] | 否 | [] | 类型黑名单 |
| `filters.size` | `{min,max}` | 否 | null | 字节；`null` 不限 |
| `filters.mtime` | `{from,to}` | 否 | null | ISO-8601 UTC |
| `filters.paths` | string[] | 否 | [] | 路径前缀白名单（平台原生分隔符） |
| `filters.excludePaths` | string[] | 否 | [] | glob 排除（优先级高于 `paths`） |
| `filters.volumes` | string[] | 否 | [] | 卷 ID 白名单；空=全部已启用卷 |
| `sort.key` | enum | 是 | `relevance` | `relevance\|name\|size\|mtime` |
| `sort.order` | enum | 是 | `desc` | `asc\|desc` |
| `paging.offset/limit` | int | 是 | 0/50 | 首窗口；后续用游标 |
| `highlight.enable` | bool | 是 | true | 是否生成片段 |
| `highlight.snippetLen` | int | 否 | 160 | 单片段字符数 |
| `highlight.maxSnippets` | int | 否 | 3 | 每文档片段数 |
| `highlight.pre/post` | string | 否 | `<mark>` / `</mark>` | 高亮包裹串（渲染前 HTML 转义） |
| `timeoutMs` | int | 否 | 2000 | 查询总超时预算 |
| `firstBatchHintMs` | int | 否 | 150 | 首批返回目标（对应 ≤200ms 硬性要求） |

**响应（ID → UI，流式，`type:"response.query.execute"`）**

```json
{
  "v": 1, "id": "01J9Z7Q8W3E4R5T6Y7U8I9O0", "type": "response.query.execute",
  "ts": 1762000000180, "replyTo": "01J9Z7Q8W3E4R5T6Y7U8I9O0", "seq": 0, "final": false,
  "payload": {
    "requestId": "01J9Z7Q8W3E4R5T6Y7U8I9O0",
    "phase": "partial",
    "totalEstimate": 1234,
    "scannedSegments": 7, "totalSegments": 24,
    "tookMs": 42,
    "nextCursor": "eyJvIjoxMjAsInMiOiJ2b2wtM2YyYSJ9",
    "warnings": [],
    "hits": [
      {
        "docId": 918273,
        "volumeId": "vol-3f2a1b9c0d8e7f60",
        "path": "C:\\Users\\a\\docs\\report.pdf",
        "name": "report.pdf",
        "ext": "pdf",
        "size": 2097152,
        "mtime": "2025-08-11T09:22:31Z",
        "score": 12.75,
        "matchedFields": ["name", "content"],
        "snippets": [{ "field": "content", "text": "…季度 <mark>foo</mark> 报表…", "offsets": [[120, 123]] }],
        "flags": { "encrypted": false, "truncated": false }
      }
    ]
  }
}
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `requestId` | string | 必须与请求一致；UI 侧不一致即丢弃 |
| `phase` | enum | `partial`（还有后续）/ `final`（最后一批）/ `cancelled` |
| `totalEstimate` | int | 估计命中总数（`partial` 阶段可为下界；`final` 阶段为准） |
| `scannedSegments/totalSegments` | int | 段进度，用于「已搜 x/y 段」微文案 |
| `tookMs` | int | 服务端本批耗时 |
| `nextCursor` | string\|null | 下一批游标；`null` 表示无更多 |
| `warnings` | string[] | `timeout_partial`、`scan_limit_reached`、`volume_unavailable:<id>` |
| `hits[].docId` | int | 卷内文档 ID |
| `hits[].volumeId` | string | 卷 ID |
| `hits[].path` | string | 平台原生路径 |
| `hits[].name/ext/size/mtime` | — | 展示与排序字段 |
| `hits[].score` | float | BM25 + 字段权重加成后的相关度 |
| `hits[].matchedFields` | string[] | `name` / `path` / `content` |
| `hits[].snippets[]` | object | `field`、`text`（已含高亮包裹且已 HTML 转义）、`offsets` |
| `hits[].flags.encrypted/truncated` | bool | 加密无法抽取 / 文本被截断 |

**取消确认**

```json
{ "v":1, "id":"01J9Z7QB1C2D3E4F5G6H7J8K9", "type":"response.query.cancel",
  "replyTo":"01J9Z7QA1B2C3D4E5F6G7H8J9", "ts":1762000000200,
  "payload": { "requestId":"01J9Z7Q8W3E4R5T6Y7U8I9O0", "cancelledAt":"batch", "deliveredSeq": 3 } }
```
随后对原 `query.execute` 补发一帧 `phase:"cancelled"`、`final:true` 的分片。

### 4.5 `event.progress` payload 字段表

```json
{
  "v": 1, "id": "01J9Z7Q9A1B2C3D4E5F6G7H8", "type": "event.progress",
  "ts": 1762000000500, "seq": 812, "final": null, "replyTo": null,
  "payload": {
    "volumeId": "vol-3f2a1b9c0d8e7f60",
    "mount": "C:\\",
    "state": "Indexing",
    "phase": "scan",
    "filesTotal": 1843200,
    "filesDone": 421337,
    "bytesDone": 913742336,
    "dirsDone": 38122,
    "filesFailed": 17,
    "failedSamples": [
      { "path": "C:\\Users\\<USER>\\AppData\\x", "code": "ERR_PERMISSION_DENIED" }
    ],
    "throughput": { "filesPerSec": 1832, "bytesPerSec": 41943040 },
    "etaSec": 776,
    "checkpoint": { "id": 41, "at": "2026-11-02T10:00:00Z", "cursorPath": "C:\\Users\\<USER>\\docs", "resumable": true },
    "memory": { "rssBytes": 268435456, "budgetBytes": 536870912, "pressure": false },
    "io": { "throttle": "low", "actualBytesPerSec": 8388608 }
  }
}
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `volumeId` | string | 卷 ID |
| `mount` | string | 挂载点 / 盘符 |
| `state` | enum | 状态机状态（§5.2） |
| `phase` | enum | `discover\|scan\|extract\|write\|merge\|watch\|idle` |
| `filesTotal` | int | 待处理文件数（扫描中为估计值，可能增长） |
| `filesDone` | int | 已处理文件数 |
| `bytesDone` | int | 已读取字节数 |
| `dirsDone` | int | 已遍历目录数 |
| `filesFailed` | int | 失败计数（权限不足 / 抽取失败 / 超限） |
| `failedSamples` | array | 最多 5 条样本；**路径须隐私脱敏**（§9.3） |
| `throughput.filesPerSec/bytesPerSec` | int | 滑动窗口吞吐 |
| `etaSec` | int\|null | 估计剩余秒；未知为 `null` |
| `checkpoint.id/at/cursorPath/resumable` | — | 最近检查点信息 |
| `memory.rssBytes/budgetBytes/pressure` | — | 内存实况与预算 |
| `io.throttle/actualBytesPerSec` | — | 节流档位与实际速率 |

节流规则：单卷进度事件 **≤ 每 50ms 一条**；多卷按卷分别节流后合并到同一条 `event.progress`（`payload.batches[]`）时仍保证每卷 50ms 间隔；UI 侧再以 `requestAnimationFrame` 合帧渲染。

### 4.6 取消与流式

| 规则 | 说明 |
| --- | --- |
| 协作式取消 | indexd 在**批次边界 / 单文件边界 / 检查点边界**检查取消集合；不在指令中间强杀线程 |
| 取消请求 | `query.cancel { requestId }` → `response.query.cancel`；原请求补发 `phase:"cancelled"` 终帧 |
| 超时 | `timeoutMs` 到点即收尾；已产出 ≥1 批 → `final:true` + `warnings:["timeout_partial"]`；0 批 → `error ERR_QUERY_TIMEOUT` |
| 背压 | UI 未消费分片 ≤ 4；超过则服务端暂停产出（避免 UI 内存暴涨） |
| 顺序 | `seq` 单调；UI 幂等丢弃乱序与重复 |
| 断连 | 连接断开 → 服务端撤销该连接所有在途请求并记录 |

### 4.7 心跳与断连

| 项 | 规则 |
| --- | --- |
| 周期 | 双向 5s（`heartbeat{payload:{kind:"ping"}}` → `pong`） |
| 失败判定 | 连续 3 次无 `pong`（≈15s）视为对端失联 |
| UI 失联（indexd 侧） | indexd 暂停所有卷任务 → 落盘检查点 → 关闭索引文件 → `exit(0)`；**不驻留后台** |
| indexd 失联（UI 侧） | UI 标记「索引服务未连接」并提供「重启索引服务」；自动重启最多 2 次、间隔 2s，失败后提示用户 |
| 系统关机/注销 | 走 §4.8 同一流程（Windows `WM_ENDSESSION` / `CTRL_SHUTDOWN_EVENT`；Linux `SIGTERM`/`SIGINT`） |
| 托盘 | 仅用户显式开启时关闭窗口不退出进程；**默认关闭**，设置中须有文字说明（§10.5） |

### 4.8 关闭流程时序

```
T0        UI：用户点击关闭 / 收到系统关机或注销信号
T0+0ms    UI：停止发起新请求；展示「正在保存索引…」（≤200ms 内仍可交互）
T0+0ms    UI → ID: shutdown { reason:"user|shutdown|logout", timeoutMs:5000 }
T0+~5ms   ID：各卷状态机 → Pausing（不再接受新批次，当前批次快速收尾）
T0+~50ms  ID：停 fs-watch；增量队列持久化（未丢失，下次启动重扫兜底）
T0+~200ms ID：每卷 checkpoint()（WAL + 原子替换 state.json）；关闭段文件句柄
T0+~300ms ID：storage.close()（SQLite WAL checkpoint + 关闭）
T0+~350ms ID → UI: shutdown.ack { savedCheckpoint:true, volumes:[{id, checkpointId, filesDone}] }
T0+~360ms ID → UI: bye；ID exit(0)
T0+~370ms UI：关闭窗口，进程退出

—— 超时路径 ——
T0+5000ms 未收到 shutdown.ack → UI 强制终止索引进程，
          提示「索引已保存至最近检查点」，随后自身退出（宽限 200ms）

—— 崩溃路径 ——
UI 崩溃/被强杀 → Job Object（Win）/ PDEATHSIG（Linux）毫秒级终止子进程；
若因任何原因未生效 → indexd 心跳连续 3 次无 pong（≈15s）→ 落盘检查点 → 自退
```

| 参数 | 默认 | 可配范围 | 说明 |
| --- | --- | --- | --- |
| `shutdown.timeoutMs` | 5000 | 3000–10000 | 超时后强制终止并提示 |
| `heartbeat.intervalMs` | 5000 | 3000–10000 | 心跳周期 |
| `heartbeat.missThreshold` | 3 | 2–5 | 连续未响应次数 |

**硬约束**：关闭主界面 = 关闭索引进程，是**默认且唯一默认行为**；托盘常驻只能由用户显式开启。任何实现不得引入「最小化到托盘继续索引」的默认路径。

### 4.9 错误码表

| 错误码 | 语义 | 触发条件 | UI 处理 |
| --- | --- | --- | --- |
| `ERR_AUTH_FAILED` | 鉴权失败 | 令牌不匹配 / 未握手 / 第二连接 | 提示并重启索引服务；不重试连接 |
| `ERR_PROTOCOL_MISMATCH` | 协议不匹配 | 版本交集为空；信封字段不合法 | 提示版本不一致，终止连接 |
| `ERR_BUILD_MISMATCH` | 版本混跑 | `buildId` 与 `meta.db.build_id` 不一致 | 提示索引由其他版本创建，提供清理/重建 |
| `ERR_VOLUME_UNAVAILABLE` | 卷不可用 | 卷不可访问 / 被拔出 / 未挂载 | 该卷 Unavailable，其他卷不受影响 |
| `ERR_PERMISSION_DENIED` | 权限不足 | 文件或目录无读取权限 | 跳过并计数，聚合到 `event.error` |
| `ERR_INDEX_DIR_NOT_WRITABLE` | 索引目录不可写 | 写探针失败 | 按 §5.5 降级；降级也失败则停止该卷并报错 |
| `ERR_INDEX_CORRUPTED` | 索引损坏 | 分片校验和不匹配 / 段头损坏 | 隔离该段 → 卷 Degraded → 提示重建 |
| `ERR_EXTRACT_UNSUPPORTED` | 抽取不支持 | 类型不在支持矩阵 | 跳过，仅索引文件名/路径 |
| `ERR_EXTRACT_ENCRYPTED` | 加密 | PDF/Office 加密且无口令 | 跳过并记录 `flags.encrypted` |
| `ERR_EXTRACT_TOO_LARGE` | 超大 | 超过单文件抽取上限 | 截断或跳过（按配置），记录 `flags.truncated` |
| `ERR_EXTRACT_TIMEOUT` | 抽取超时 | 超过 `extract.timeoutMs` | 跳过并记录 |
| `ERR_QUERY_SYNTAX` | 查询语法错误 | 括号/运算符/正则非法 | 输入处红框 + 错误位置提示 |
| `ERR_QUERY_TIMEOUT` | 查询超时 | 超过 `timeoutMs` | 展示部分结果 + 「已超时，已返回部分结果」 |
| `ERR_MEMORY_LIMIT` | 内存上限触发 | 超过 `memoryBudgetBytes` | 暂停批次 → 释放 → 继续；持续触发则跳过大文件并记录 |
| `ERR_PAYLOAD_TOO_LARGE` | 负载过大 | 单帧 > 8 MiB | 关闭连接并重建 |
| `ERR_CANCELLED` | 已取消 | 请求被取消 | UI 丢弃 |
| `ERR_INTERNAL` | 内部错误 | 未分类错误 | 记录脱敏日志 + 提示 |
| `ERR_BUSY` | 忙 | 重建/清理中冲突操作 | 提示稍后重试 |
| `ERR_WATCH_OVERFLOW` | 监控溢出 | FS 事件队列溢出 | 自动降级为目录重扫（事件通知，非终止错误） |

错误响应示例：

```json
{ "v":1, "id":"01J9Z7Q000000000000000000", "type":"error", "replyTo":"01J9Z7P0000000000000000000",
  "ts":1762000000000,
  "payload": { "code":"ERR_QUERY_SYNTAX", "message":"unmatched '(' at position 12",
               "detail": { "position": 12 }, "retryable": false, "volumeId": null } }
```

---

## 5. 索引子系统设计

### 5.1 卷识别

| 项 | Windows | Linux |
| --- | --- | --- |
| 枚举 | `GetLogicalDrives` + `GetVolumeInformationW`（盘符、卷 GUID、文件系统、序列号）；挂载点用 `FindFirstVolumeMountPoint` | 解析 `/proc/self/mounts` + `statvfs`（取 `f_fsid`、源设备、挂载点） |
| `volumeId` 生成 | `sha256(volumeGuidPath + fsType)[0:16]` → `vol-<hex16>` | `sha256(fsid + stableMountKey + fsType)[0:16]` |
| 稳定性 | 卷 GUID 路径 `\\?\Volume{...}` 跨重启稳定；盘符可能变化 | `fsid` 稳定；挂载点可变（主键用 fsid，挂载点仅展示） |
| 文件系统过滤 | 索引 `NTFS/ReFS/exFAT/FAT32`；跳过空 CD-ROM；网络驱动器默认不索引（可显式勾选） | 跳过 `proc/sys/devtmpfs/overlay/cgroup2` 等虚拟文件系统；NFS/SMB 默认不索引（可显式勾选） |
| 可移动介质 | 默认不索引（U 盘/SD 卡），可显式勾选；拔出 → Unavailable | 同左 |
| 容量 | `GetDiskFreeSpaceEx` → total/used/free | `statvfs` → total/used/free |
| 权限 | 无需管理员；受限目录按 `ERR_PERMISSION_DENIED` 跳过 | 无需 root；受限目录同左 |

**假设：** 网络驱动器与可移动介质默认**不**纳入首次全盘范围，需用户在范围确认页显式勾选（隐私与性能折中）。

### 5.2 卷任务状态机

状态集合：`Discovered`、`Scanning`、`Indexing`、`Paused`、`Stopped`、`Rebuilding`、`Cleaning`、`Watching`（就绪并监控中）、`Degraded`（部分损坏/部分失败仍可服务）、`Unavailable`。

**迁移表（源 → 事件 → 目标 → 副作用）**

| 源状态 | 事件 | 目标状态 | 副作用 |
| --- | --- | --- | --- |
| （无） | `volume.discovered` | `Discovered` | 写 `meta.db.volumes`；初始化 state.json；不入调度队列 |
| `Discovered` | `index.start(scope)` | `Scanning` | 创建游标 `cursor=""`；启动 scan-worker；写 state.json |
| `Discovered` | `volume.action=skip` | `Stopped` | `enabled=false`；不创建卷目录内容 |
| `Discovered` | `volume.unavailable` | `Unavailable` | 不占调度槽位；UI 置灰 |
| `Scanning` | `scan.progress` | `Scanning` | 更新 filesTotal/filesDone；节流发 `event.progress` |
| `Scanning` | `scan.completed` | `Indexing` | 枚举完成，继续消化抽取/写入队列（扫描与索引流水化） |
| `Scanning` | `pause` | `Paused` | 保存游标 checkpoint；释放内存；发 `event.volumeState` |
| `Scanning` | `stop` | `Stopped` | checkpoint；关闭段文件；`enabled=false` |
| `Scanning` | `io.error(volumeGone)` | `Unavailable` | checkpoint；释放槽位；保留索引数据 |
| `Indexing` | `batch.committed` | `Indexing` | 更新 docs.db 与统计；周期 checkpoint（默认 30s 或 5000 文件） |
| `Indexing` | `queue.empty` | `Watching` | 启动/恢复 fs-watch；`resumable=true`；发 `event.volumeState` |
| `Indexing` | `pause` | `Paused` | checkpoint；暂停 watch；释放槽位 |
| `Indexing` | `stop` | `Stopped` | checkpoint；关闭；停 watch |
| `Indexing` | `memory.limit` | `Indexing` | 暂停批次 → 释放缓冲 → 跳过大文件并记录 → 发 `event.memoryPressure` |
| `Indexing` | `checksum.failed(seg)` | `Degraded` | 隔离该段至 `tmp/quarantine-<seg>/`；其余段继续服务；提示重建 |
| `Paused` | `resume` | `Indexing` | 从 checkpoint 续建；重新校验卷可用性 |
| `Paused` | `stop` | `Stopped` | checkpoint；释放资源 |
| `Paused` | `rebuild` | `Rebuilding` | 见下 |
| `Watching` | `fs.event` | `Watching` | 增量批次；去重去抖 1s |
| `Watching` | `watch.overflow` | `Watching` | 受影响子树重扫；发 `event.watchOverflow` |
| `Watching` | `excludes.changed` | `Rebuilding` | 差异重建（无法精确推导影响面则全量重建） |
| `Watching` | `pause` | `Paused` | 停 watch；保存 |
| `Watching` | `stop` | `Stopped` | 停 watch；保存 |
| `Watching` | `volume.unavailable` | `Unavailable` | 停 watch；保留数据；UI 显示「卷不可用」 |
| `Watching` | `verify.failed` | `Degraded` | 隔离损坏段；查询保持可用 |
| `Degraded` | `rebuild` | `Rebuilding` | 清空该卷段与 docs.db → 全量重建 |
| `Degraded` | `clean` | `Cleaning` | 删除段、docs.db、state.json → 回到 `Discovered` |
| `Rebuilding` | `rebuild.progress` | `Rebuilding` | 流式进度；旧数据在切换前仍可查（读旧写新） |
| `Rebuilding` | `rebuild.completed` | `Watching` | 原子替换段目录；更新 meta.db；旧段移入 tmp 后删除 |
| `Rebuilding` | `rebuild.failed` | `Degraded` | 保留旧数据；提示用户 |
| `Rebuilding` | `stop` | `Stopped` | 保留旧数据（可回滚）；记录中断点 |
| `Cleaning` | `clean.completed` | `Discovered` | 删除 `vol-<id>/` 下全部内容；清空该卷统计 |
| `Unavailable` | `volume.available` | `Paused` | 校验卷 GUID/fsid 一致 → 恢复；不一致视为新卷（新 volumeId） |
| 任意 | `shutdown` | `Stopped`（内存态） | checkpoint；关闭句柄；进程退出 |
| 任意 | `clean` | `Cleaning` | 先停任务与 watch，再删除 |

实现要求：

1. 每卷一个 `VolumeTask`，持有独立 `state.json`（原子写：`state.json.tmp` → `fsync` → `rename`）。
2. 迁移须幂等且可重放：重启按 `state.json` 恢复；`Rebuilding` 中断重启 → `Degraded`（保留旧数据）。
3. 单卷异常（`Unavailable`/`Degraded`）**不得**影响其他卷与查询服务：查询时该卷返回空并带 `warnings`。
4. 所有迁移写入 `meta.db.events`（每卷滚动保留最近 1000 条）。

### 5.3 调度策略与多盘并行

| 项 | 规则 |
| --- | --- |
| 默认并行卷数 | `min(卷数, 2)` |
| 低配自动降级 | 判定：逻辑 CPU ≤ 2 或物理内存 ≤ 4GB 或（电池供电且电量 < 30%）→ 并行度降为 1；可配置覆盖 |
| 可配范围 | 1–4（`index.parallelVolumes`）；超出可用 CPU 时提示 |
| 卷内并行 | 抽取线程 `min(CPU/2, 4)`；**写线程恒为 1**（避免段写竞争） |
| 调度粒度 | 「目录批次」（默认 256 目录 或 30ms 工作时间片，先到先触发） |
| 优先级 | 用户可见卷 > 已就绪卷的增量 > 首次全量 > 重建/合并/压缩（后台最低） |
| 抢占 | 用户查询/预览使 CPU 预算临时上调，索引线程让出；不抢占已提交批次 |
| 公平性 | 多卷轮转（round-robin）分配时间片，避免小卷饿死 |
| 槽位 | 槽位 = 并行卷数；卷暂停/停止立即释放槽位 |
| 合并调度 | 仅在卷处于 `Watching` 且空闲（无用户输入 60s）+ 节流令牌充足时执行 |

### 5.4 默认全盘范围与平台默认排除名单

**默认行为**：首次启动进入一次性「索引范围确认页」，逐卷展示：卷名/挂载点、文件系统、容量（总/已用）、预计文件量级、预计耗时量级、索引目录位置、默认排除项列表；用户可**取消勾选卷**、增删排除项、修改索引目录；确认后才开始索引。取消/关闭则进入「未建立索引」空状态，并提供再次确认入口。

**预估计方法（假设）**：先枚举前 20 万目录采样，按 `文件数/目录数` 与 `平均文件字节` 外推；量级以「约 N 万 / 约 N 百万」「约 X–Y 分钟（低档节流）」表述，不承诺精确值。

**Windows 默认排除**

| 类别 | 示例 |
| --- | --- |
| 系统目录 | `C:\Windows`（默认排除，可勾选）、`C:\$Recycle.Bin`、`C:\System Volume Information`、`C:\Recovery`、`**\$WINDOWS.~BT` |
| 用户缓存 | `~\AppData\Local\Temp`、`~\AppData\Local\Microsoft\Windows\INetCache`、`~\AppData\Local\CrashDumps`、`~\AppData\Roaming\Microsoft\Windows\Recent` |
| 开发/依赖 | `**\node_modules`、`**\.git\objects`、`**\target\debug`、`**\target\release`、`**\build`、`**\dist`、`**\.venv`、`**\venv`、`**\__pycache__` |
| 包管理缓存 | `~\.cargo\registry`、`~\.nuget\packages`、`~\.m2`、`~\.gradle\caches`、`~\.npm\_cacache` |
| 大文件/影子 | `**\*.vhdx`、`**\*.vhd`、`**\*.iso`、`**\pagefile.sys`、`**\hiberfil.sys`、`**\swapfile.sys` |
| 自身 | `<索引目录>/**`（必须排除，防自索引） |
| 重解析点 | 默认不跟随（防环），可在设置开启 |

**设置页的索引排除（当前实现）**：`index.excludePaths` 是「路径片段」列表（大小写不敏感，路径中任意位置包含即整棵子树剪枝），默认含 `$Recycle.Bin`、`RECYCLER`、`System Volume Information`，因此**任意盘符**的回收站都会跳过，而不只 `C:`；`index.excludeTypes` 是扩展名列表（不含点），命中即不建立索引。二者在「设置 → 索引排除」中编辑，保存在便携 `config.json`，索引进程在每次扫描 / 增量批次开始时重读，无需重启即生效，并与 `index.excludeRules`（范围确认页的 glob/前缀规则）叠加生效。

**Linux 默认排除**

| 类别 | 示例 |
| --- | --- |
| 虚拟文件系统 | `/proc`、`/sys`、`/dev`、`/run`、`/tmp`、`/var/run`、`/var/lock` |
| 系统/包数据 | `/usr/share/doc`、`/usr/lib/debug`、`/var/cache`、`/var/lib/apt`、`/var/lib/dnf`、`/var/log/journal` |
| 用户缓存 | `~/.cache`、`~/.local/share/Trash`、`~/.npm/_cacache`、`~/.cargo/registry`、`~/.m2`、`~/.gradle/caches` |
| 开发/依赖 | `**/node_modules`、`**/.git/objects`、`**/target/debug`、`**/target/release`、`**/build`、`**/dist`、`**/.venv`、`**/venv`、`**/__pycache__` |
| 容器/镜像 | `/var/lib/docker/overlay2`、`/var/lib/containers`、`**/*.qcow2`、`**/*.iso` |
| 自身 | `<索引目录>/**`（必须排除） |
| 挂载点 | 其他文件系统挂载点默认不穿越（`statvfs` fsid 变化即停止深入），可在设置开启 |

> 排除语法：glob（`**`、`*`、`?`）+ 路径前缀；大小写：Windows 不敏感、Linux 敏感（`filters.caseSensitivePathExcludes` 按平台默认）。

### 5.5 索引目录与降级策略

默认索引目录：`<程序目录>/index`（可执行文件所在目录下的 `index/`）。

| 平台 | 首选 | 降级目标（按序） | 说明 |
| --- | --- | --- | --- |
| Windows | `<程序目录>\index` | 1. `%LOCALAPPDATA%\SnapFind\index` 2. `%TEMP%\SnapFind\index`（最后兜底，明确提示易失） | 程序目录不可写常见于 `C:\Program Files\` |
| Linux | `<程序目录>/index` | 1. `$XDG_DATA_HOME/snapfind/index`（缺省 `~/.local/share/snapfind/index`） 2. `$XDG_CACHE_HOME/snapfind/index`（兜底，明确提示可能被清理） | 程序目录不可写常见于 `/opt`、`/usr/bin` |

降级判定与提示：

1. 启动时对候选目录做**真实写探针**：创建 `tmp/.writetest` → 写 4KB → `fsync` → 读回校验 → 删除。失败即判不可写。
2. 原因分类并在 UI 明示：`ACCESS_DENIED`（权限不足）、`READ_ONLY_FS`（只读文件系统）、`DISK_FULL`（空间不足）、`PATH_TOO_LONG`/`ENAMETOOLONG`、`NOT_FOUND`、`IO_ERROR`。
3. 降级后发 `event.indexDegraded{from,to,reason}`；UI 在索引状态面板顶部常驻红色提示条，设置页显示当前生效目录。
4. 用户可手动指定目录；指定失败则回退自动选择并提示。
5. 空间预检：可用空间 < 预计索引体积 × 1.5 时警告；空间耗尽 → 该卷 `Paused` 并提示（不静默写坏）。

### 5.6 索引目录结构

```
<index-root>/
├── meta.db                 # 全局元数据（SQLite, WAL）
├── meta.db-wal
├── meta.db-shm
├── lock                    # 进程独占锁（pid/buildId/协议版本/启动时间）
├── tmp/                    # 临时与隔离区（原子替换中间产物、隔离段）
│   ├── writetest/
│   └── quarantine-<segId>/
└── vol-<volumeId>/
    ├── state.json          # 卷状态机持久状态 + checkpoint
    ├── docs.db             # 卷文档存储（路径/大小/mtime/类型/标志/编码）
    ├── hot/                # 热层：近期增量小段（可查，轻压缩）
    │   ├── seg-000123.seg
    │   └── seg-000123.ix
    ├── segments/           # 冷层：已合并压缩的只读段
    │   ├── seg-000100.seg
    │   ├── seg-000100.ix
    │   └── seg-000100.crc32
    └── tmp/                # 本卷临时产物
```

| 文件 | 内容 | 关键点 |
| --- | --- | --- |
| `meta.db` | `schema_version`、`build_id`、`protocol_version`、`volumes(id,mount,fs,enabled,exclude_rules,state,created_at,last_seen)`、`stats`、`events`、`search_history` | WAL；`build_id` 用于禁止新旧混跑 |
| `lock` | `pid`、`buildId`、`protocolVersion`、`startedAt`、`indexRoot` | Linux `flock`；Windows `CreateFile` + `LockFileEx` 独占；含崩溃残留检测（pid 存活判定） |
| `vol-<id>/state.json` | `state`、`phase`、`cursor`、`filesDone/Total`、`checkpoint{id,at,cursorPath}`、`failedCounts`、`watchState`、`lastError`、`doneSubtrees`（已完成的一级子树，断点续跑用，扫描完成即清空） | 原子替换写；体积小，每次 checkpoint 全量重写 |
| `vol-<id>/docs.db` | `docs(doc_id PK, path UNIQUE, parent_id, name, ext, size, mtime, ctime, encoding, flags, doc_len, content_hash NULL)` + `dirs(dir_id PK, parent_id, name)` | 路径表与倒排解耦，支持重命名/移动的增量更新 |
| `vol-<id>/hot/*.seg` | 小段倒排 + 位置/偏移（轻压缩或未压缩，写入快） | 达阈值合并 |
| `vol-<id>/segments/*.seg` | 冷段倒排（zstd 压缩 postings + Roaring docId + delta-varint 偏移） | 只读，带 `.crc32` |
| `vol-<id>/segments/*.ix` | 段索引头（词表前缀索引、段文档范围、文档数、校验和、schema 版本） | 便于快速跳过与校验 |

**假设：** 冷段目标大小 64 MiB（压缩后）；热段达 8 个或累计 32 MiB 触发合并。

### 5.7 分片格式与压缩

| 结构 | 编码 | 说明 |
| --- | --- | --- |
| 词表 | FST 或 有序词块（每 256 词一块 + 前缀索引） | 支持前缀/通配符候选扩展 |
| postings（docId 列表） | Roaring Bitmap + delta-varint（小段） | 快速 AND/OR/NOT |
| 位置/偏移 | delta + varint + 分块 + 块级 zstd | 短语与高亮必需；可关闭（省 30–40%，短语降级为 AND） |
| 正排/摘要 | 存 `docs.db`，不在段内重复 | 命中后按需读取，降低体积 |
| 字段掩码 | 每 posting 1 字节（name/path/content 位） | 支持「仅文件名」快速过滤 |
| 压缩 | 冷段 zstd level 3 / 热段 level 1；可选 LZ4 | 冷段读多写少，用更高压缩比 |
| 校验和 | 段级 CRC32C + 块级 CRC32C | `maintenance.verify` 可定位到块 |
| 段头 | magic `SFSEG1` + schema_version + build_id + 段范围 + 文档数 + 校验和 | 版本不兼容直接拒绝加载 |

体积控制承诺：

* 只保留必要 token：丢弃停用词、长度 < 2 的中文单字可配置、超长 token 截断 64 字节。
* 只保留必要偏移：短语与高亮必需；`index.storePositions=false` 时可关闭。
* 提供体积统计与清理工具：`snapfind-indexd --stats`、`--compact`、`--clean --volume <id>`，并在 UI「存储占用」面板按卷/按段类型/按倒排 vs 正排拆分展示。

### 5.8 检查点与断点续建

| 项 | 规则 |
| --- | --- |
| checkpoint 内容 | `checkpointId`、`phase`、`cursorPath`、`cursorId`（dirs 表 id，更稳）、`filesDone`、`bytesDone`、`queueSnapshot`（最多 2000 条，超出退化为目录游标重扫）、`hotSegmentIds`、`failedCounts` |
| 触发时机 | 每 5000 文件 / 每 30s / 每提交 10 批次 / 暂停 / 停止 / shutdown / 内存压力前 |
| 写入方式 | `state.json.tmp` → `fsync` → `rename`；`docs.db` 依赖 SQLite WAL（checkpoint 时 `PRAGMA synchronous=FULL`） |
| 恢复 | 按 `cursorId` 定位目录 → 该目录及之后重新枚举；已处理目录不重复（mtime 变化由增量逻辑处理） |
| 首次索引中断 | 用户可随时暂停/停止/关闭；下次启动自动从最近 checkpoint 续建（UI 显示「从 X 继续」） |
| 幂等 | 重复处理同一文件安全（`upsert_doc`：先删旧 posting 再插） |
| 崩溃安全 | 段写入走 `tmp/` → `fsync` → `rename` 入段目录；启动清理 `tmp/` 残留 |

### 5.9 增量索引与文件变更监控

| 项 | Windows | Linux |
| --- | --- | --- |
| 机制 | `ReadDirectoryChangesW`（异步、递归 `TRUE`） | `inotify`（`IN_CREATE/IN_DELETE/IN_MODIFY/IN_MOVED_FROM/IN_MOVED_TO/IN_CLOSE_WRITE`）；`fanotify` 可选（需权限，不默认） |
| 递归 | 内建递归；监视根为卷根 + 用户自定义路径 | 不递归；逐目录 `inotify_add_watch` |
| 规模限制 | 每卷 1 个句柄，缓冲区 64KB | `fs.inotify.max_user_watches`（默认 8192）、`max_user_instances`（128） |
| 溢出降级 | `ERROR_NOTIFY_ENUM_DIR` → 标记溢出 → 受影响子树重扫 | `IN_Q_OVERFLOW` → 脏目录集合重扫；超限则卷级重扫 |
| 大规模策略 | 卷根单句柄，成本低 | 热目录（用户主目录）全 watch；冷目录定时轮询（默认 10 分钟） |
| 合并去重 | 入队 → 1s 去抖 → 按目录合并 → 同路径保留最后事件 | 同左 |
| 变更判定 | 主判据 `mtime` + `size`；不一致即重索引。可选内容哈希（Blake3，默认关闭） | 同左 |
| 移动/重命名 | 无 cookie；退化为删+增（USN Journal 可选，需管理员，默认关） | `IN_MOVED_FROM/TO` cookie 配对 → 更新路径，不重抽取 |
| 删除 | docs.db 标记删除 → 段内 docId 入删除位图 → 合并时物理清除 | 同左 |
| 启动兜底 | 对「上次运行期间脏但未完成」的目录快速重扫（不整盘重扫） | 同左 |
| 卷不可用 | 停监控；恢复后做一次差异重扫 | 同左 |

**假设：** 内容哈希默认关闭；`index.hashPolicy` 取 `none`（默认）/ `partial-1mb` / `full`。

### 5.10 损坏检测与重建

| 层 | 检测手段 | 处置 |
| --- | --- | --- |
| 段文件 | 加载校验段头 magic/schema/CRC32C；`maintenance.verify` 逐块 CRC | 移入 `tmp/quarantine-<seg>/` → 卷 `Degraded` → 其余段继续服务 → UI 提示「部分索引损坏，建议重建该卷」 |
| docs.db | `PRAGMA integrity_check` / `quick_check` | 重建 docs.db（从段 + 文件系统重枚举路径，成本低） |
| state.json | JSON 解析失败 / schema 版本不符 | 丢弃状态 → 卷回 `Discovered` 并做一致性重扫（不丢段） |
| meta.db | `integrity_check`；失败从 `meta.db.bak`（schema 变更时生成）恢复；仍失败则从各卷 state.json + 段头重建 | 最小化损失 |
| 写入中崩溃 | WAL + 原子替换；启动清理 `tmp/` + WAL 恢复 | 无损坏 |
| 隔离语义 | 隔离 ≠ 删除：`quarantine` 保留至用户确认或磁盘压力清理 | 可审计 |
| 重建 | `volume.action=rebuild`：清理该卷全部段与 docs.db → 全量重建（读旧写新，完成后原子切换） | 不影响其他卷与查询 |

### 5.11 内存与 IO 控制

**内存**

| 项 | 规则 |
| --- | --- |
| 全局预算 | 默认 512MB（`index.memoryBudgetBytes`，128MB–4GB）；覆盖抽取缓冲 + 分词缓冲 + 批次缓冲 + 段写缓冲 + 后端缓存 |
| 计量 | RSS 采样（1s）+ 自记账（已分配缓冲字节），取大者 |
| 分级 | 80% → 批次减半；90% → 暂停新文件、完成当前批次、跳过大文件；100% → 强制释放、写 checkpoint、发 `event.memoryPressure` |
| 大文件分块 | 1MiB/块；单文件抽取上限默认 64MiB（`extract.maxBytesPerFile`），超限默认截断（或跳过），记录 `flags.truncated` |
| 文本截断 | 单文件索引文本上限默认 2MiB（`index.maxTextBytesPerFile`），超出不再分词 |
| 流式/批处理 | 绝不整文件入内存；批次默认 512 文件 或 8MB 文本（先到先触发） |
| 后端缓存 | SQLite `PRAGMA cache_size` 受预算约束；mmap 上限受预算约束（Tantivy） |

**IO 节流档位**

| 档位 | 目标速率 | Windows | Linux |
| --- | --- | --- | --- |
| low（默认） | ≤ 8 MiB/s 且 ≤ 400 IOPS | Job Object `JOB_OBJECT_IO_RATE_CONTROL`（Win10+）；不可用时占空比控制（工作 20ms / 休眠 80ms）+ `IDLE_PRIORITY_CLASS` + `THREAD_MODE_BACKGROUND_BEGIN` | `ionice -c 3` + `nice +19`；可选 cgroup v2 `io.max`（不默认）；退化占空比 |
| medium | ≤ 32 MiB/s | 同左，放宽速率 | `ionice -c 2 -n 7` + `nice +10` |
| high | 不限（仅低优先级） | `BELOW_NORMAL_PRIORITY_CLASS` | `nice +5` |
| 自适应 | 键鼠 3s 内有输入时降一档，空闲 60s 恢复 | UI 经 `config.apply` 通知 | 同左 |

| 其他 | 规则 |
| --- | --- |
| CPU 优先级 | Windows：`IDLE_PRIORITY_CLASS`（low）/ `BELOW_NORMAL_PRIORITY_CLASS`（high）；线程 `THREAD_MODE_BACKGROUND_BEGIN` |
| Linux | `nice 19 / 10 / 5`；可选 `SCHED_IDLE` |
| 电池 | 电池供电默认 low 且并行度 1（可配） |
| 节流令牌 | 令牌桶（速率 + 突发），每卷独立 + 跨卷总桶（避免总 IO 超标） |
| fsync | checkpoint 时 `fsync`；冷段落盘 `fsync`；热段可 `synchronous=NORMAL` 降低抖动 |

---

## 6. 内容抽取

### 6.1 类型矩阵

| 类型 | 扩展名 | 抽取方式 | 惰性加载 | 依赖 | 备注 |
| --- | --- | --- | --- | --- | --- |
| 纯文本 | `.txt` `.text` `.log` `.csv` `.tsv` `.ini` `.cfg` `.conf` `.toml` `.yaml` `.yml` `.json` `.xml` | 字节流读取 + 编码探测（UTF-8/UTF-16/GBK/latin1） | 否 | 无 | 无上限内全量 |
| 代码 | `.rs` `.py` `.js` `.ts` `.tsx` `.jsx` `.go` `.c` `.h` `.cpp` `.hpp` `.java` `.kt` `.cs` `.rb` `.php` `.swift` `.sh` `.ps1` `.sql` | 同纯文本 | 否 | 无 | 分词按标识符规则（`snake_case`/`camelCase` 拆分） |
| 日志 | `.log` 及无扩展名大文本 | 同纯文本（流式，尾部优先） | 否 | 无 | 支持超大文件分块 |
| Markdown | `.md` `.markdown` | 剥离语法标记后索引（保留标题与正文） | 否 | 自研（可选 `pulldown-cmark`） | 标题字段加权 |
| HTML | `.html` `.htm` `.xhtml` | 抽 `<title>`/`<meta>` 与正文，剔除 script/style | 否 | 自研 tokenizer（可选 `scraper`） | 不执行 JS |
| RTF | `.rtf` | 轻量 RTF 解析（控制字、`\ansicpg`、`\uN` 转义、跳过 `\bin`） | 否 | 自研 | 不支持嵌入 OLE 对象内容 |
| PDF | `.pdf` | 文本层抽取（ToUnicode CMap → Unicode） | **是** | 见 §6.4 | 加密/无文本层 → 记录，可触发 OCR 插件（默认关） |
| DOCX | `.docx` `.dotx` | ZIP + `word/document.xml`（`w:t`），含页眉页脚可选 | **是** | `zip` + `quick-xml` | 老式 `.doc` 见下方「旧二进制 Office」 |
| XLSX | `.xlsx` `.xlsm` | ZIP + `xl/sharedStrings.xml` + `xl/worksheets/*.xml` | **是** | `zip` + `quick-xml` | 老式 `.xls` 见下方 |
| PPTX | `.pptx` `.ppsx` | ZIP + `ppt/slides/slide*.xml`（`a:t`） | **是** | `zip` + `quick-xml` | 老式 `.ppt` 见下方 |
| EPUB | `.epub` | ZIP + OPF 清单顺序 + XHTML 文本 | **是** | `zip` + `quick-xml` | DRM 加密 → `ERR_EXTRACT_ENCRYPTED` |
| 其他/未知 | — | 仅索引文件名与路径 | 否 | 无 | 可开启「前 8KB 可打印文本启发式」（默认关） |
| OCR（插件） | 图像 `.png` `.jpg` `.tif` / 扫描 PDF | 可选插件，**默认不启用** | 是 | 见 §6.4 | 独立动态库，缺失即禁用 |
| 旧二进制 Office | `.doc` `.dot` `.xls` `.xlt` `.ppt` `.pps` `.pot` | `ole_text()`：连续可打印 ASCII（≥6 字节）+ 未被覆盖的 UTF-16LE 可打印片段（ASCII/CJK/中文标点/全角）兜底抽取 | 否 | 自研 | 输出无排版纯文本，可预览也可全文检索；二进制垃圾不会进入正文 |

### 6.2 统一接口

```rust
pub struct Limits {
    pub max_bytes_read: usize,   // 单文件最大读取字节（默认 64 MiB）
    pub max_text_bytes: usize,   // 单文件最大抽取文本（默认 2 MiB）
    pub timeout: Duration,       // 单文件抽取超时（默认 5s）
    pub max_depth: u32,          // 容器嵌套深度（zip 内 zip，默认 2）
    pub max_mem: usize,          // 单文件抽取内存软上限（默认 32 MiB）
}

pub struct ExtractedText {
    pub text: String,                          // 已按 max_text_bytes 截断
    pub truncated: bool,
    pub lang_hint: Option<String>,
    pub encoding: String,
    pub meta: BTreeMap<String, String>,        // title/author/sheet 名等
    pub offsets: Option<Vec<(u32, u32)>>,      // 可选：段落/行偏移，用于高亮对齐
}

pub trait Extractor: Send + Sync {
    fn can(&self, ext: &str, sniff: &[u8]) -> bool;   // 扩展名 + magic sniff 双判据
    fn extract(&self, reader: &mut dyn ReadSeek, limits: &Limits) -> Result<ExtractedText, ExtractError>;
}

pub fn extract(reader: &mut dyn ReadSeek, limits: Limits) -> Result<ExtractedText, ExtractError>;
```

契约：

1. 必须**流式**：不得以「读入整个文件」为前提（ZIP 走 seek 读中央目录，仍不整文件入内存）。
2. 超时由外层 `spawn_blocking` + 时间预算强制终止；终止后记 `ERR_EXTRACT_TIMEOUT`，不重试。
3. 超限必须返回截断结果或 `ExtractError::TooLarge`，不得 OOM。
4. 所有解析错误必须捕获，不得 panic（panic → 捕获为 `ERR_INTERNAL`，本次批次禁用该类型）。
5. 加密输入必须返回 `ExtractError::Encrypted`，不得尝试破解口令。

### 6.3 健壮性与降级

| 场景 | 行为 |
| --- | --- |
| 类型不支持 | `ERR_EXTRACT_UNSUPPORTED`；仍索引文件名/路径；计入 `filesFailed`，不视为卷失败 |
| 加密 | `ERR_EXTRACT_ENCRYPTED`；记录 `flags.encrypted`；UI 命中项显示「已加密，无法预览内容」 |
| 超大文件 | 超 `max_bytes_read` → 截断或跳过（`extract.onTooLarge: truncate\|skip`，默认 truncate）；`flags.truncated=true` |
| 损坏文件 | `ERR_EXTRACT_FAILED`（非致命）；跳过并记录 |
| 抽取超时 | `ERR_EXTRACT_TIMEOUT`；跳过；同类型连续 50 次超时 → 临时禁用该类型 10 分钟并提示 |
| 内存超限 | `ERR_MEMORY_LIMIT`；跳过大文件、降批次规模 |
| ZIP 炸弹 / 高压缩比 | 压缩比 > 1000 或解压后 > `max_bytes_read` → 立即中止，记 `ERR_EXTRACT_TOO_LARGE` |
| 长路径 | Windows 使用 `\\?\` 前缀；仍失败 → 记录并跳过 |
| 编码失败 | 探测失败 → latin1 兜底 + `lossy` 转换；不失败 |
| 二进制夹杂文本 | 可打印比例阈值（默认 ≥ 0.8）判定；不满足仅索引名称/路径 |
| 旧 Office 二进制 | 不索引内容；设置页「支持格式」明示，命中项提示「该格式暂不支持内容检索，可转换为 docx/xlsx/pptx」 |
| 无文本层 PDF | 标记 `scanned`；OCR 插件启用时交由插件，否则仅索引名称/路径 |

### 6.4 四目标依赖可用性说明（Windows/Linux × x64/arm64）

> 图例：✅ 可用（预编译或纯 Rust）｜⚠️ 需自编译或需预编译二进制｜❌ 不可用 → 需降级

| 能力 | 候选依赖 | Win x64 | Win arm64 | Linux x64 | Linux arm64 | 结论与降级 |
| --- | --- | --- | --- | --- | --- | --- |
| ZIP 读取 | 纯 Rust `zip` | ✅ | ✅ | ✅ | ✅ | 采用（DOCX/XLSX/PPTX/EPUB 共用） |
| XML 解析 | `quick-xml` | ✅ | ✅ | ✅ | ✅ | 采用 |
| PDF 文本层（主） | 纯 Rust `lopdf` / `pdf-extract` | ✅ | ✅ | ✅ | ✅ | **默认采用**：零外部二进制，四目标一致 |
| PDF 文本层（增强） | `pdfium-render` + Pdfium | ✅ | ⚠️ 需 arm64 预编译 | ✅ | ⚠️ 需 arm64 构建 | **可选插件**：仅预编译可用目标打包；不可用自动回退纯 Rust 路径（复杂 CMap 覆盖率略降） |
| PDF 渲染预览 | 同上（可选） | 见上 | 见上 | 见上 | 见上 | 默认预览=文本+命中片段；渲染为可选插件，缺失仅退化为文本/元信息 |
| HTML | 自研 tokenizer（默认）/ `scraper` | ✅ | ✅ | ✅ | ✅ | 默认自研，零依赖 |
| Markdown | 自研轻量剥离（默认）/ `pulldown-cmark` | ✅ | ✅ | ✅ | ✅ | 默认自研 |
| RTF | 自研解析器 | ✅ | ✅ | ✅ | ✅ | 完全自控 |
| 编码探测 | `chardetng`（纯 Rust） | ✅ | ✅ | ✅ | ✅ | 采用 |
| 中文分词 | bigram（自研，默认）；词典分词插件 | ✅ | ✅ | ✅ | ✅ | 默认 bigram；词典插件可选，默认关 |
| 西文分词 | Unicode 分词 + 大小写折叠 + 可选轻量词干 | ✅ | ✅ | ✅ | ✅ | 词干默认关闭（避免召回噪声） |
| OCR | `tesseract` 动态库（插件，默认关） | ⚠️ 需用户提供 | ⚠️ | ⚠️ 发行版包 | ⚠️ | **可选插件**，进程外 + 动态加载；不可用时面板显示「OCR 插件不可用」 |
| 旧二进制 Office | 无可用轻量方案 | ❌ | ❌ | ❌ | ❌ | 不索引内容；UI 明示并建议转换 |
| 压缩 | `zstd-safe`（vendored 构建） | ✅ | ✅ | ✅ | ✅ | 采用；构建保守指令集，避免 arm64 问题 |
| 位图 | `roaring-rs`（纯 Rust） | ✅ | ✅ | ✅ | ✅ | 采用 |
| FS 事件 | `notify`（RDCW / inotify 封装） | ✅ | ✅ | ✅ | ✅ | 采用；溢出降级自研 |
| 嵌入式 DB | `rusqlite` bundled | ✅ | ✅ | ✅ | ✅ | 采用（arm64 需 CI 验证） |
| 可选后端 | `tantivy` | ✅ | ✅ | ✅ | ✅ | v1.1 可选后端 |
| GUI | Tauri v2（WebView2 / WebKitGTK 4.1） | ✅ | ✅ WebView2 arm64 | ✅ | ✅ webkit2gtk-4.1 | 采用；Linux 需声明运行时依赖 |

构建文档须记录上表的**每次验证结论**（依赖版本、commit、验证日期、验证人）；依赖升级后重新验证并更新矩阵。

---

## 7. 查询引擎

### 7.1 语法表

| 语法 | 写法 | 说明 | 默认 |
| --- | --- | --- | --- |
| 关键词 | `foo` | 分词后在选定域（name/path/content）检索 | 支持 |
| 短语 | `"foo bar"` | 位置连续匹配（需 `storePositions=true`） | 支持 |
| AND | `foo AND bar`、`foo && bar`、空白分隔（隐式 AND） | 交集 | 支持 |
| OR | `foo OR bar`、`foo \|\| bar` | 并集 | 支持 |
| NOT | `NOT foo`、`-foo`、`!foo` | 差集；一元 NOT 需至少一个正项 | 支持 |
| 分组 | `(a OR b) AND c` | 括号优先；不匹配报 `ERR_QUERY_SYNTAX`（带 position） | 支持 |
| 通配符 | `foo*`、`f?o` | `*` 多字符、`?` 单字符；**前导通配符**默认禁用（`query.allowLeadingWildcard=false`） | 支持 |
| 字段限定 | `name:foo`、`path:src`、`content:foo` | 域限定 | 支持 |
| 类型限定 | `type:pdf`、`ext:pdf` | 走过滤器下推 | 支持 |
| 路径限定 | `path:"C:\Users\a\docs"` | 前缀匹配 | 支持 |
| 排除词 | `-node_modules` | 等价 NOT | 支持 |
| 正则 | `/foo.*bar/i` + `mode.regex=true` | **默认关闭**；仅对候选集限量扫描（默认 5000 文档上限） | 可选 |
| 模糊 | `foo~1` + `mode.fuzzy=true` | **默认关闭**；编辑距离 1–2；限量扫描 | 可选 |
| 转义 | `\"` `\\` `\(` | 保留字符转义 | 支持 |
| 大小写 | `mode.caseSensitive` | 西文折叠；中文无影响 | 默认不敏感 |
| 整词 | `mode.wholeWord=true` | 边界匹配 | 默认关闭 |

解析输出 `NormalizedQuery`（AST），优化后执行：

```
AST 优化顺序：
  NOT 下推 → 高选择性词项优先 → 前缀/通配符扩展为词表候选（上限 1024）
  → 纯负项查询改写为「全集 MINUS 负项」→ 过滤器下推为位图 → 段级并行执行
```

### 7.2 过滤器下推

| 过滤器 | 下推形式 | 数据结构 | 说明 |
| --- | --- | --- | --- |
| 类型 `ext/type` | 段级 ext 位图 | Roaring | 先于评分求交，避免无效打分 |
| 大小 `size` | 段级 size 分桶位图（0–64K/64K–1M/1M–64M/64M+）+ 桶内精确复核 | Roaring + docs.db | 减少回表 |
| 修改时间 `mtime` | 段级按天分桶位图 + 桶内精确复核 | Roaring | 同上 |
| 路径前缀 | `dirs` 表前缀 → docId 集合 | Roaring | 路径树预计算 `parent_id` |
| 排除路径 glob | 预编译 → 前缀/正则候选 → 位图取反 | Roaring | 复杂 glob 走正则限量扫描 |
| 卷选择 | 直接裁剪参与检索的卷与段 | — | 最高效 |

下推顺序：卷 → 段（ext/size/mtime 桶）→ 路径位图 → 词项检索 → 精确复核（size/mtime 边界）→ 评分。

### 7.3 排序 / 分页 / 流式

| 排序键 | 实现 | 流式可行 |
| --- | --- | --- |
| 相关度 | BM25（FTS5 `bm25()` / Tantivy）+ 字段权重（name ×2.0、path ×1.2、content ×1.0）+ 新鲜度轻加成（可关） | 是（段内 top-K 堆 + 全局归并） |
| 名称 | 按 `name` 序（默认字节序 + 大小写折叠；可选 ICU collation） | 是（段内有序归并） |
| 大小 | `size` 数值序 | 是 |
| 修改时间 | `mtime` 序 | 是 |

| 项 | 规则 |
| --- | --- |
| 首批 | 目标 ≤150ms 产出第一批（硬性 ≤200ms）：段内 top-K（K = offset+limit）→ 归并 → 立即返回前 limit |
| 分批 | 后续每批 ≤ 50 条；`partial` 阶段 `totalEstimate` 可更新 |
| 分页 | 游标优先（`nextCursor` 编码 offset + 段进度 + 排序键水位），避免深翻页重复扫描；同时支持 `offset/limit` |
| 虚拟滚动 | UI 按滚动位置增量请求（预取 ±2 屏），服务端以游标响应 |
| 去重 | 跨段同一 docId 归并；删除位图过滤 |
| 上限 | 单次查询最多 10000 条（可配），超出提示「请增加过滤条件」 |

### 7.4 取消与超时

| 项 | 规则 |
| --- | --- |
| 取消时机 | 段边界、批边界、每 1000 候选文档检查一次；协作式 |
| 取消后 | 已产出批次保留（是否显示由 UI 的 requestId 决定）；补发 `phase:"cancelled"` 终帧 |
| 超时预算 | 解析 5% / 检索 80% / 合并排序 10% / 序列化 5%；任一阶段超时立即收尾 |
| 超时返回 | ≥1 批 → `final:true` + `warnings:["timeout_partial"]`；0 批 → `ERR_QUERY_TIMEOUT` |
| 正则/模糊限量 | 候选扫描上限 5000 文档或预算耗尽，超出标 `warnings:["scan_limit_reached"]` |
| 慢查询日志 | 本地记录（脱敏）：耗时、段数、候选数、语法类型，用于基准与调优 |

### 7.5 高亮与片段

| 项 | 规则 |
| --- | --- |
| 片段来源 | 优先用索引偏移定位；`storePositions=false` 时回退为对抽取文本的局部重扫（限量 1MiB） |
| 片段长度 | `snippetLen=160`（可配 40–500）；`maxSnippets=3` |
| 上下文 | 命中前后各取约一半，按 UTF-8 字符边界裁剪，不切断代理对/组合字符 |
| 高亮 | `pre/post` 包裹；**先 HTML 转义文本，再插入标记**；标记串由 UI 白名单控制，防注入 |
| 多命中合并 | 距离 < 32 字符的命中合并为一个片段 |
| 文件名/路径高亮 | 同机制，走对应字段偏移 |
| 性能 | 仅对返回窗口内文档生成片段（≤50 条/批），不做全量高亮 |

### 7.6 分词

| 语言 | 策略 |
| --- | --- |
| 西文 | Unicode 分词（UAX#29 简化）+ 小写折叠 + 去变音符号 + 可选轻量词干（默认关）+ 代码标识符切分 |
| 中文/日文 | 默认 bigram 滑窗；可选词典分词插件（默认不启用，避免体积膨胀） |
| 数字 | 保留数字与单位；版本号按 `.` 切分并保留整体 |
| 停用词 | 内置表（可配）；中文停用字可配 |
| token 限制 | 长度 1–64 字节；丢弃纯符号 token |
| 一致性 | 索引与查询必须使用同一分词器（`meta.db.tokenizer_id` 记录，变更触发重建提示） |

---

## 8. 预览与文件操作

### 8.1 分块读取

| 项 | 规则 |
| --- | --- |
| 通道 | 预览文本**不经 IPC**：UI 进程用 `preview-reader` 直接读源文件（避免 8MiB 帧限与双份内存）；IPC 仅取 `preview.meta` |
| 分块 | 纯文本 256 KiB/块按需续读；抽取类（PDF/Office）先取 `preview.meta`，再按页/段落分块 |
| 首屏 | 优先读命中偏移附近窗口（±64 KiB），保证「点开即有内容」；其余异步补 |
| 超大文件 | > 64 MiB 仅提供窗口预览（前后各 1 MiB + 命中窗口），提示「文件过大，已提供窗口预览」 |
| 骨架屏 | 加载中显示骨架屏 + 取消按钮；首块到达即替换 |
| 取消 | 切走/关闭预览立即取消（协作式，块边界检查） |
| 编码 | 复用索引期编码探测结果（`docs.db.encoding`），避免二次探测 |
| 内存 | 预览累计渲染上限 1 MiB 文本；滚动时滚动窗口外文本释放 |

> **当前桌面端实现（`snapfind-desktop`）差异**：预览正文由 UI 进程一次性直读全文（普通文档不截断，> 32 MiB 才截断并提示），不经 IPC 分块；显示行按预览面板宽度（360–680px）软换行，一整行内容自动折行、无需横向滚动，虚拟滚动只绘制视口内行。右侧用 18px 宽标尺替代默认细滚动条（隐藏原生滚动条）：灰滑块表示视口，蓝色为搜索关键词命中行、橙色为 Ctrl+F 命中，单击标记吸附到该行、拖动按比例滚动。结果列表已去掉「得分」列、增加 `#` 序号列，大小/修改时间为固定列宽并各自裁切；表头排序与类型多选均只对本轮已加载结果做本地重排/筛选，不重新发起搜索（`filters.types` 恒为空，`paging.limit` 提高到 1000 以覆盖更多类型）。`timeout_partial` 警告在界面上转为中文说明。

### 8.2 安全渲染

| 威胁 | 处置 |
| --- | --- |
| HTML/脚本注入 | 预览以**纯文本**渲染（React 默认转义）；高亮仅插入白名单标签 `<mark>`；禁止 `dangerouslySetInnerHTML` |
| 路径注入 | 打开前校验绝对路径且存在；不做 shell 拼接，参数数组调用；Windows `ShellExecuteW`（`lpFile`/`lpDirectory`），Linux `xdg-open`（或 `org.freedesktop.FileManager1`） |
| 畸形内容 | 渲染前截断（默认 1 MiB），非法字符替换为 U+FFFD |
| 剪贴板 | 仅复制路径字符串；提供「复制为 POSIX 路径」选项（Windows） |
| 文件权限 | `File::open` 只读；从不写目标文件 |
| 符号链接 | 默认不跟随跨卷符号链接（可配） |
| 危险操作 | v1 不提供从结果列表删除/移动文件，仅打开与打开所在文件夹 |
| 内容嗅探 | 预览不做 MIME 执行；扩展名与 magic 不一致时按抽取器 `sniff` 结果决定渲染方式，不执行内容 |

### 8.3 零门槛约束（硬性）

| 约束 | 实现保证 |
| --- | --- |
| 不扫码 | 代码无二维码生成/识别路径；预览不发任何网络请求 |
| 不登录 | 无账号/会话/令牌刷新逻辑；无用户标识 |
| 不付费 | 无 license 校验、无付费解锁开关；所有功能对源码构建者一致可用 |
| 无云端校验 | 预览不校验在线状态；默认构建不含 HTTP 客户端（§13.5 依赖准入） |
| 可审计 | `--list-capabilities` 输出启用的可选能力与插件状态 |

---

## 9. 配置与本地存储

### 9.1 各平台路径表

| 用途 | Windows | Linux |
| --- | --- | --- |
| 索引根（默认） | `<程序目录>\index` | `<程序目录>/index` |
| 索引根（降级 1） | `%LOCALAPPDATA%\SnapFind\index` | `$XDG_DATA_HOME/snapfind/index`（缺省 `~/.local/share/snapfind/index`） |
| 索引根（降级 2） | `%TEMP%\SnapFind\index`（提示易失） | `$XDG_CACHE_HOME/snapfind/index`（缺省 `~/.cache/snapfind/index`，提示易失） |
| 配置文件 | `%APPDATA%\SnapFind\config.json` | `$XDG_CONFIG_HOME/snapfind/config.json`（缺省 `~/.config/snapfind/config.json`） |
| 搜索历史 | `%APPDATA%\SnapFind\history.db` | `$XDG_DATA_HOME/snapfind/history.db` |
| 日志目录 | `%LOCALAPPDATA%\SnapFind\logs` | `$XDG_STATE_HOME/snapfind/logs`（缺省 `~/.local/state/snapfind/logs`） |
| 插件目录 | `%LOCALAPPDATA%\SnapFind\plugins` | `$XDG_DATA_HOME/snapfind/plugins` |
| IPC 端点 | `\\.\pipe\snapfind-<sidHash>-<session>-<pid>` | `$XDG_RUNTIME_DIR/snapfind-<uid>/ipc.sock` |
| 全局元数据 | `<索引根>\meta.db` | `<索引根>/meta.db` |
| 桌面入口 | 开始菜单快捷方式 | `~/.local/share/applications/snapfind.desktop` |
| 卸载 | MSI / NSIS 卸载器 | `apt remove` / `rpm -e`；AppImage 手动删除 |

配置文件为 JSON（UTF-8，无 BOM），含 `$schema` 与 `version`；写入走「临时文件 + 原子替换」；读取失败 → 备份为 `config.json.bak` 并回退默认值。配置中不含任何凭据。

### 9.2 配置分组（完整表见 §18.1）

`index.*`（root / parallelVolumes / memoryBudgetBytes / throttle / storePositions / maxTextBytesPerFile / hashPolicy）、`extract.*`（maxBytesPerFile / timeoutMs / onTooLarge）、`query.*`（timeoutMs / firstBatchHintMs / regex / fuzzy / allowLeadingWildcard）、`ui.*`（debounceMs / theme / language / overscan / trayEnabled）、`lifecycle.*`（shutdownTimeoutMs / heartbeatIntervalMs / heartbeatMissThreshold）、`privacy.*`（telemetry=false / historyEnabled / logLevel）。

### 9.3 日志与隐私脱敏

| 项 | 规则 |
| --- | --- |
| 级别 | 默认 `info`；可配 `error/warn/info/debug/trace`；默认不记录查询明文（`log.queryDebug` 可选，仅本地） |
| 内容 | 时间戳、级别、线程、模块、消息、错误码、卷 ID、**脱敏路径** |
| 脱敏 | 用户名目录替换（`C:\Users\<name>` → `C:\Users\<USER>`；`/home/<name>` → `~`）；默认仅保留首段 + 末 2 段；完整路径需 `log.fullPaths=true` |
| 轮转 | 8 MiB × 5；按大小轮转；不压缩（便于审计）；可一键清空 |
| 不出网 | 日志仅本地，无上报组件 |
| 崩溃转储 | 默认**不**生成 minidump/core；开启诊断模式后本地生成并提示 |
| 事件日志 | `meta.db.events` 滚动保留状态迁移与错误聚合，同样脱敏 |

---

## 10. UI 与交互

### 10.1 主要界面

| 界面 | 元素 | 说明 |
| --- | --- | --- |
| 首次索引范围确认页 | 卷卡片（名称/挂载点/文件系统/容量条/预计文件量级/预计耗时量级，可勾选）、索引目录位置与「更改」、默认排除项列表与增删、「开始索引」/「稍后」 | 一次性；确认后才索引；默认勾选本地固定卷 |
| 主搜索页 | 搜索框 + 语法提示、过滤器栏（类型/大小/时间/路径）、排序选择、结果虚拟列表（名称/路径/大小/修改时间/片段）、状态栏（命中数/耗时/卷覆盖） | 启动默认聚焦搜索框 |
| 预览面板 | 文件头（名称/路径/大小/修改时间/类型）、高亮文本、骨架屏、工具条（打开 / 打开所在文件夹 / 复制路径 / 复制为 POSIX 路径） | 零门槛，无登录/付费/扫码入口 |
| 索引状态面板 | 每卷一行：挂载点、状态徽章、进度条、吞吐、ETA、已索引/总数、失败数、检查点时间、操作按钮（暂停/恢复/停止/重建/清理/排除规则） | 字段见 §18.2 |
| 设置页 | 常规 / 索引 / 搜索 / 预览 / 插件（OCR 默认关） / 隐私（遥测默认关） / 关于与许可 | 危险操作二次确认 |
| 空状态 | 「尚未建立索引」+ 引导进入范围确认页 | 不自动开始索引 |

### 10.2 界面不卡顿（硬性、可测）

| # | 指标 | 阈值 | 测量方式 |
| --- | --- | --- | --- |
| 1 | 按键回显延迟 | ≤ 1 帧（16.7ms @60Hz，按刷新率折算） | 注入 keydown → 字符上屏的 `performance` 差值；p95 |
| 2 | 搜索输入防抖 | ≤ 150ms（默认 120ms） | 代码常量 + 单测 |
| 3 | 首批结果展示 | ≤ 200ms（从最后一次输入到首屏有内容） | 端到端埋点 p95 |
| 4 | 主线程长任务 | 索引满载时 0 条 > 50ms | `PerformanceObserver('longtask')` |
| 5 | 主线程同步 IO | 0 处 | 代码审计 + lint 规则（禁 `readFileSync` 等） |
| 6 | 主线程同步 IPC 等待 | 0 处 | 审计：全部 async/await + 可取消 |
| 7 | 切页/改排序响应 | ≤ 200ms 首屏更新 | 埋点 p95 |
| 8 | 进度事件节流 | ≤ 50ms/次/卷（1s 内 ≤ 20 条/卷） | 服务端单测 + 前端计数器 |
| 9 | 滚动帧率 | ≥ 55 FPS（10000 条结果滚动） | FPS 采样脚本 |
| 10 | 大数组同步计算 | 0 处（排序/过滤/聚合在 indexd 或 Worker） | 审计 + 行渲染按可视窗口 |
| 11 | 旧结果丢弃 | 旧 requestId 分片到达后 ≤ 1 帧内丢弃且不渲染 | 单测 + 注入延迟响应的集成测试 |
| 12 | 预览首块 | ≤ 300ms 出骨架屏；≤ 800ms 出首块内容（本地典型文件） | 埋点 p95 |
| 13 | 设置修改生效 | ≤ 500ms 内 UI 继续可交互 | 埋点 |
| 14 | UI 进程空闲内存 | < 150MB（RSS，空闲 5min） | `bench-ui` |

实现约束：

* 所有 IO/IPC 在 Rust 侧 tokio 异步任务或前端 `async`；禁止阻塞 UI 线程。
* 结果列表虚拟滚动（窗口 + overscan 8），列表项 memo 化。
* 进度/统计更新走 `requestAnimationFrame` 合帧，单帧最多一次 setState。
* 预览在独立组件 + 分块状态机；> 1 MiB 文本分片渲染（`requestIdleCallback` 续渲）。
* 高亮/diff 等大文本计算放 Web Worker 或 Rust 侧。
* 切换查询立即 `query.cancel` 并作废旧 requestId。

### 10.3 默认快捷键表

| 快捷键 | 功能 | 平台差异 |
| --- | --- | --- |
| `Ctrl+F` | 聚焦搜索框 | 同 |
| `Ctrl+L` | 聚焦并全选搜索框 | 同 |
| `Esc` | 清空搜索 / 关闭预览（按层级） | 同 |
| `Enter` | 立即执行搜索（跳过防抖） | 同 |
| `↑` / `↓` | 结果列表上下移动 | 同 |
| `PageUp` / `PageDown` | 翻页 | 同 |
| `Ctrl+Enter` | 打开选中文件 | 同 |
| `Ctrl+Shift+C` | 复制选中项完整路径 | 同 |
| `Ctrl+Shift+O` | 打开所在文件夹 | 同 |
| `Ctrl+P` | 切换预览面板 | 同 |
| `Ctrl+K` | 打开过滤器面板 | 同 |
| `Ctrl+,` | 打开设置 | 同 |
| `Ctrl+Shift+I` | 打开索引状态面板 | 同 |
| `Alt+1..4` | 切换排序（相关度/名称/大小/修改时间） | 同 |
| `Ctrl+H` | 搜索历史 | 同 |
| `F5` | 重新执行当前查询 | 同 |
| `Ctrl+Q` | 退出（触发 §4.8 关闭流程） | Windows 亦支持 `Alt+F4` |
| `F1` | 快捷键帮助 | 同 |

快捷键可在设置中重绑定；冲突时提示。

### 10.4 结果列表与分页

* 虚拟滚动：`@tanstack/react-virtual`（或 `react-window`），行高固定（紧凑/舒适可切），overscan 8。
* 预取：滚动到窗口 70% 时按游标请求下一批。
* 选中态与键盘导航按 `docId` 维持，不因虚拟滚动丢失。
* 空结果、加载中、错误三态明确区分；错误态提供「重试」。

### 10.5 托盘与启动行为（默认说明）

* 默认：关闭主界面 → 退出应用 + 关闭索引进程。
* 托盘常驻**只能**由用户在「设置 → 常规 → 关闭主窗口时最小化到托盘」显式开启，该项附文字说明：

  > 开启后 SnapFind 将在关闭窗口后继续在后台运行并继续索引，索引进程不会退出。你可以随时在托盘图标中选择「退出」以完全关闭（索引将保存至最近检查点）。

* 托盘开启时，托盘菜单必须包含「退出」（完全退出并关闭索引进程）。
* 开机自启默认关闭；Windows 走注册表 `Run` 项或任务计划（不提权）；Linux 走 `~/.config/autostart/snapfind.desktop`。

> **托盘最终架构（独立进程，当前实现）**：
> - **图标由 `crates/snapfind-tray`（`snapfind-tray.exe`）承载**，GUI 启动时以 `--watch-pid <pid> --gui <path>` 拉起；宿主每秒检查 GUI 存活，GUI 退出即 `NIM_DELETE` 清理并退出（不留僵尸图标）。
> - 交互：**双击** → 以 `--tray-show` 重启 GUI（单实例分支 `SW_SHOW + SW_MAXIMIZE + SetForegroundWindow`，保持最大化）；**右键** → 立即弹菜单「显示（默认加粗）/ 设置 / 退出」；**设置** → `--tray-settings` + 置位 `SnapFind.OpenSettings` 事件；**退出** → `--tray-quit` + 置位 `SnapFind.RequestExit` 事件，GUI 每帧 `control::poll()` 消费后走常规关闭链路真实退出；**单击无动作**；**关闭按钮** → GUI 隐藏窗口并 `notify_hidden()` 让宿主弹气泡。
> - 可靠性要点：① 宿主窗口是**屏幕外可见的 1×1 工具窗口**（`WS_POPUP`+`WS_EX_TOOLWINDOW`，坐标 -32000）——`SW_HIDE` 的隐藏窗口无法成为前台窗口，`SetForegroundWindow` 会被拒绝，`TrackPopupMenu` 弹出即失焦消失，这正是「右键菜单时有时无」的根因；② 右键**立即**弹菜单（去掉延迟定时器），先 `SetForegroundWindow`（失败则模拟 Alt 抬键重试 ≤3 次），结束 `PostMessage(WM_NULL)`（KB135788，避免第一下点击被吞）；③ 窗口类必须带 `CS_DBLCLKS`，否则系统不合成 `WM_LBUTTONDBLCLK`；④ `TaskbarCreated` 广播重挂图标；⑤ GUI 与宿主共写 `%TEMP%\snapfind-tray.log`（`SNAPFIND_TRAY_LOG=1` 或 `%TEMP%\snapfind-tray-log-enable` 开启，默认静默）。
> - **全局热键 `Ctrl+Alt+F`**：GUI 内独立线程 `RegisterHotKey`，`WM_HOTKEY` 由系统直接投递到注册线程（不经 explorer），是托盘链路失效时的保底唤起；恢复用进程内枚举 `app::find_main_hwnd()`（按标题 `FindWindow` 不稳；枚举时最小化窗口要跳过尺寸过滤，其矩形是 -32000）。
>
> 启动最大化实现注意：**不要用 winit 的 `with_maximized(true)`，也不要设置 `with_inner_size`**——前者只设最大化标志、尺寸仍停在 `inner_size`（实测 `IsZoomed=true` 但窗口 1180×760），后者在窗口显示后才被应用，会把已最大化的窗口拉回原尺寸。当前做法是在 `SnapFindApp::new`（首帧之前）用 Win32 `ShowWindow(SW_SHOW)` + `SW_MAXIMIZE` 显示并最大化，并在启动后 8 秒内逐帧校正（`is_main_maximized_native` 为真即停止）。枚举主窗口时必须排除同进程的辅助窗口：托盘 `SnapFindTrayClass`、`NVOpenGLPbuffer`/`NVOGLDC`（egui glow 后端，带标题栏且尺寸达 1440×753）、`Winit Thread Event Target`（16×16）、`PseudoConsoleWindow`、`IME`；判定条件为「有标题 + `WS_CAPTION` + 尺寸 ≥300px」。排查时设 `SNAPFIND_TRAY_LOG=1` 可把托盘/窗口日志写到 `%TEMP%\snapfind-tray.log`（默认静默）。
>
> 托盘气泡语义：**只在窗口真正驻留托盘时弹**（手动隐藏由 `hide_window` 经 `notify_hidden()` 发 `WM_APP+2` 给宿主进程弹）；窗口正常显示时不弹。
>
> **为什么托盘最终移出 GUI 进程（排障结论）**：曾用 v4 v0 两代协议、多轮重写（`NIN_SELECT` 时间戳判双击 → v0 + 单击定时器判定 → 去掉单击判定的 v3），并把托盘线程挪到 `eframe::run_native` 之前启动；但本机环境下 **explorer 的真实点击从未投递到 GUI 进程内的托盘窗口**（`NIM_ADD` 成功、图标显示正常、`PostMessage` 注入正常，唯独真实点击零到达）。对照实验（曾放在 `examples/tray_min.rs`，已随排障结束删除）证明：**同样的注册代码作为独立小进程可正常收到单击/双击/右键，且与固定/溢出、提权状态无关**。因此改为独立宿主进程承载图标——不再依赖 GUI 进程内的任何环境因素。
>
> **排障中修掉的两个真 bug**：① 命名事件名必须 NUL 结尾（`wide()` 缺 NUL → 跨进程读到的名字不一致：设置事件侥幸能开、退出事件 `OpenEventW` 报 `ERROR_FILE_NOT_FOUND`，表现为「设置能用、退出没反应」）；② 进程内枚举主窗口时，**最小化窗口的矩形是 (-32000,-32000,160,28)**，尺寸过滤会把它误杀（热键/托盘恢复最小化窗口依赖 `IsIconic` 跳过尺寸检查）。
>
> 验证（注入 + 参数直调）：双击隐藏窗口 → `visible=True zoomed=True` ✓；右键 → 菜单 `fg=true` 稳定可见并返回 cmd ✓；`--tray-settings` → 界面切设置页 ✓；`--tray-quit` → GUI 真实退出、宿主清理图标退出 ✓；热键注册 `ok=true` ✓。
>
> **单实例聚焦（已修）**：`focus_existing_instance` 曾用 `SW_RESTORE` 唤起已有窗口——它会**取消最大化状态**，即用户报告的「启动最大化失效」（日志佐证：二次启动 `single-instance: focus existing window` 后窗口变小）。已改为 `SW_SHOW` + `SW_MAXIMIZE`。
>
> **~~提权降权机制~~（已移除）**：曾怀疑「explorer/应用提权组合导致托盘点击被 UIPI 静默拦截」，实现过 `de_elevate_once()`（IsUserAnAdmin 检测 → `explorer.exe <exe>` 代理重启）。**已整体删除**：(1) 用户实测其他同样提权运行的软件托盘功能正常，UIPI 理论不成立；(2) 中继链路副作用严重——debug 构建黑框反复闪烁、瞬态 explorer 进程增多、连续双击时链路叠加导致「启动即消失」。同时 debug 构建改为 `#![windows_subsystem = "windows"]`（双击不再弹黑框）。提权不再视为缺陷、不做任何干预。
>
> **僵尸图标（「点了没反应」的头号根因）**：Windows 不会立即移除已死进程的托盘图标（鼠标划过才清除）。两个僵尸来源都堵住了：
> ① **正常退出也漏清理**——托盘线程阻塞在 `GetMessageW`，进程退出时被直接杀死，`NIM_DELETE` 从未执行。修复：`Tray` 记录线程 ID 与 `JoinHandle`，`Drop` 时 `PostThreadMessageW(WM_QUIT)` + `join`，让线程走完 `NIM_DELETE` 清理再退出（实测退出后 UIA 枚举图标数 -1，无残留）。
> ② **外部强杀**（任务管理器 / `Stop-Process -Force`）不可拦截。补救：`snapfind-gui.exe --exit` 向命名事件 `SnapFind.RequestExit.Event` 发信号，托盘线程 400ms 轮询（`SetTimer(3)`）→ `TrayEvent::Quit` 优雅退出。**排查/清理/杀进程一律用 `--exit`，不要用强杀。**
> 已有僵尸的清理：重启 explorer，或鼠标逐个划过通知区/溢出面板里的图标（死图标会消失）。多显示器下每个任务栏都有图标副本，UIA 枚举数是副本总和，勿误判为多实例。
>
> **退出被「关闭到托盘」吞掉的 bug**：托盘菜单「退出」/`--exit` 发 `ViewportCommand::Close` 后，`close_requested` 会被 close_to_tray 逻辑拦截变成「隐藏」。修复：`quit_requested: bool` 标记真实退出，close 处理器中 `!self.quit_requested &&` 才走隐藏分支。
>
> **前台锁（核心坑）**：点击托盘图标时接收输入的是 explorer，本进程没有前台转移权，托盘线程直接 `SetForegroundWindow` 会被拒绝——窗口其实显示了，却排在别的窗口后面，`IsWindowVisible` 检测不出来（它不管 z-order），看起来就是「点了没反应」。`restore_window` 的做法：先模拟 Alt 按下/抬起把前台转移权交给本进程，再 `ShowWindow` + `BringWindowToTop` + `SetForegroundWindow`（日志会记录 `set_foreground=` 返回值，1=成功）。
>
> **右键菜单延迟弹出**：右键溢出面板里的图标时 Windows 会先收起面板，焦点切换会瞬间吞掉刚弹出的菜单。做法：`WM_CONTEXTMENU`/`WM_RBUTTONUP` 只 `SetTimer(hwnd,2,50)`，50ms 后在 `WM_TIMER(wParam=2)` 里调 `show_menu()`（菜单逻辑提取为独立嵌套函数，弹前/关后均有日志）。
>
> **气泡语义（用户要求）**：正常启动不打扰；仅 `--minimized` 自启时弹「已在后台运行」；手动隐藏到托盘时由 `notify_hidden()` 弹「已隐藏到托盘」。
>
> **日志带时间戳**：`SNAPFIND_TRAY_LOG=1` 时每行前缀 UTC `HH:MM:SS.mmm`，用于分析点击间隔与双击判定。
>
> **Win11 图标位置提示**：新安装的托盘图标默认进「隐藏的图标」溢出区（任务栏右下 `^`）。用户可在 设置 → 个性化 → 任务栏 → 其他系统托盘图标 把 SnapFind 设为「开」固定到通知区。启动气泡显示期间图标会临时出现在通知区。
>
> **单实例守护**（`main.rs focus_existing_instance`）：Windows 命名互斥体 `SnapFind.SingleInstance.Mutex`；已有实例时按标题 `FindWindowW` 找到主窗口 `SW_RESTORE`/`SW_MAXIMIZE` + 前台化后 `exit(0)`。作用：重复启动不再叠加托盘图标（用户点中旧实例/僵尸图标没反应是「双击右键无效」的主要来源；进程被强杀残留的僵尸图标鼠标划过即消失）。
>
> 托盘恢复窗口保持最大化：`restore_window` 先读 `IsZoomed`（隐藏不取消最大化标志），再按原状态选 `SW_MAXIMIZE`/`SW_RESTORE`——因为移除了 `with_inner_size`，`SW_RESTORE` 会回退到最小尺寸 916×600。

---

## 11. 非功能需求与指标

### 11.1 性能

| 指标 | 目标 | 验收方式 |
| --- | --- | --- |
| 常见查询延迟 | < 200ms（p95；百万级文件参考目标） | `bench-query`；报告须标注平台与架构（如 `linux-x64`、`win-arm64`） |
| 首批结果 | ≤ 200ms 展示并分批追加 | 端到端埋点 + `bench-query` |
| 索引吞吐 | 首次全盘低档节流下 ≥ 800 文件/s（SSD、本地固定卷） | `bench-index` |
| 索引体积 | ≤ 源文本量 × 12%（默认配置；`storePositions=false` 可降至 ~7%） | `bench-size`：固定语料测量并输出比率 |
| 索引内存峰值 | ≤ `memoryBudgetBytes`（默认 512MB），p99 | `bench-memory`：RSS 采样峰值 |
| UI 空闲内存 | < 150MB | `bench-ui` |
| UI 响应性 | §10.2 全部指标通过 | `bench-ui` + 长任务埋点 |
| 启动到可搜索 | ≤ 3s（索引已存在） | 冒烟测试计时 |
| 增量生效延迟 | ≤ 10s（保存后可搜到，含 1s 去抖 + 批次） | 集成测试 |

基准报告要求：标注 OS 版本、CPU、内存、磁盘类型、语料规模、SnapFind 版本与 commit、配置快照；脚本产出 CSV + Markdown 报告（§13.6）。

#### 当前引擎已实施的性能优化（`snapfind-engine`）

* **批量写事务**：`VolumeStore::upsert_doc` 不再每条 SQL 各自 autocommit，连续写入共用一个事务，每 `BATCH_COMMIT_DOCS`（512）篇提交一次；检查点（`flush_state`）、查询（`run_query`）、watch 批次收尾、`compact`/`rebuild` 前均先 `commit_batch`，崩溃最多丢失最后一批（断点续跑可覆盖）。
* **总数估算封顶**：每卷每查询的 `COUNT(*) MATCH` 改为 `LIMIT 20000` 的封顶子查询，宽查询不再为「约 N 个结果」遍历全部命中。
* **正文单遍小写化**：相关度与片段生成共用同一次 `to_lowercase`（此前每个命中全文小写两次）。
* **纯文本读取上限收紧**：txt/log/csv/md/html/rtf 等纯文本族读取上限从 `max_bytes`（64MB）收紧为约 `max_text_bytes×2 + 64KB`（默认 ~4MB），超大日志不再整读进内存。
* **编码探测采样**：chardetng 只喂前 64KB（此前全量 feed）。
* **OLE2 兜底抽取 O(n) 化**：`ole_text` 第二遍的覆盖判断改为有序区间 + 前向指针（此前每字节线性扫描全部区间，近似平方复杂度）。
* **CJK 分词零分配**：`token_string` 改为单遍流式写入，与 `tokens_of` 输出严格一致（有测试锁定），消除中文文档每 bigram 一次的堆分配。
* **扫描不再持锁做慢活（修复「搜索一直转圈」）**：节流睡眠、内容抽取、分词移到锁外——扫描改为两段式 `scan::prepare_file`（锁外，秒级）+ `apply_prepared`（锁内毫秒级），`doc_stat` 变更检测在锁内快速完成。此前扫描线程每个文件都持全局 `Mutex<Store>` 做秒级抽取，查询拿不到锁；实测「气象」短语查询从 13–23s 降到 1.5–2s（含 indexd 启动 ~1.5s）。
* **精排/片段只处理正文前 256KB**：命中词几乎都在前部，宽查询避免对最大 2MB 正文重复全文扫描。
* **GUI 侧兜底**：查询在桥接 worker 中放独立任务（取消请求与新查询不再排在慢查询后）；30s 查询看门狗，索引进程异常时不再永远停在「搜索中」。
* **watch 批次去重**：`apply_changes` 由 `Vec::contains`（O(n²)）改为 `HashSet`。

#### 已识别、尚未实施的主要优化方向

1. 单卷内流水线并行（遍历 → 多 worker 抽取 → 单 writer 批量写库，当前单卷完全串行）；写路径拆分卷级锁。
2. 查询仍逐卷串行并持有全局 `Mutex<Store>`（扫描已改为短持锁，争用大幅缓解）；可进一步改为卷级锁 + 跨卷并行查询。
3. 真流式首屏：当前先跑完全部卷 + 精排再分批 emit，`first_batch_hint_ms` 未生效。
4. 抽取超时兜底：`Limits.timeout_ms` 已声明未实现，损坏 PDF 可长时间卡住单卷扫描。
5. 节流档自适应（首次全量/系统空闲时自动提速；默认 low 档 80 文件/s 是吞吐首要上限）。
6. 路径过滤 `LIKE 'prefix%'` 走全表扫描（需 `path_lower` 列 + 索引）；深分页 `per_volume = offset+limit` 随页深线性膨胀。

### 11.2 可靠性

| 指标 | 目标 | 验收方式 |
| --- | --- | --- |
| 崩溃安全 | 随机时机强杀 100 次，重启后索引可加载且最近检查点可用 | 故障注入 `fi-kill` |
| 损坏可检测 | 人为翻转段字节 → 加载或 `verify` 检出并隔离 | `fi-corrupt` |
| 单卷隔离 | 一卷损坏/不可用时其他卷查询正常 | `fi-volume-fail` |
| 权限不足 | 不计入失败卷，仅计数并展示样本 | `fi-permission` |
| 不驻留 | UI 崩溃后 ≤15s（心跳 3×5s）索引进程自退；Job Object/PDEATHSIG 生效时毫秒级 | `fi-ui-crash` |
| 断点续建 | 中断 100 次后与全量结果 docId 集合相等（无遗漏） | `fi-interrupt` |
| 事件溢出 | 溢出后重扫结果与全量重扫一致 | `fi-watch-overflow` |
| 数据一致性 | 索引文档集合与文件系统实际集合（应用排除后）差异率 < 0.1% | `--verify --deep` |

### 11.3 一致性

| 项 | 级别 | 说明 |
| --- | --- | --- |
| 索引 vs 文件系统 | 最终一致（增量窗口 ≤10s，重扫兜底） | 允许短暂滞后，不允许永久丢失 |
| 删除 | 最终一致；合并时物理清除 | 查询期用删除位图过滤 |
| 移动/重命名 | 尽力更新路径（Linux cookie 配对；Windows 退化为删+增） | 不重抽取内容 |
| 并发 | 单写者（每卷写线程=1）+ 多读者（WAL/MVCC），读不阻塞写 | — |
| 跨卷 | 卷间完全独立，无全局锁 | — |
| 查询结果 | 同一 requestId 内单调（只追加不撤回） | 避免列表抖动 |

### 11.4 隐私

| 项 | 验收方式 |
| --- | --- |
| 无网络依赖 | 断网环境完整通过冒烟测试；构建期依赖白名单检查；二进制字符串扫描无上传域名 |
| 无遥测 | `privacy.telemetry=false` 默认；`--privacy-audit` 输出「遥测：已关闭」 |
| 数据不出机 | 代码审计：默认构建无 HTTP client（feature `network` 默认关） |
| 可完整删除 | 卸载/一键清理可删除索引、配置、历史、日志；无残留进程 |
| 默认不 OCR | 设置页 OCR 默认关闭，并标注「启用后将调用本地 OCR 引擎处理图片与扫描件」 |
| 日志脱敏 | 抽查 100 条日志样本，用户名与完整路径均已替换 |

---

## 12. 跨平台适配要点（Windows / Linux 对照）

### 12.1 路径

| 项 | Windows（x64/arm64） | Linux（x64/arm64） |
| --- | --- | --- |
| 分隔符 | `\`（同时接受 `/` 输入） | `/` |
| 大小写 | 不敏感（比较时折叠，展示保留原样） | 敏感 |
| 长路径 | 需 `\\?\` 前缀（建议系统开启 `LongPathsEnabled`）；处理 `ERROR_PATH_NOT_FOUND` | 无 260 限制；`PATH_MAX` 4096 |
| 卷标识 | 盘符 `C:\` + 卷 GUID `\\?\Volume{...}` | 挂载点 + `fsid` |
| 网络路径 | UNC `\\server\share`，默认不索引 | NFS/SMB 挂载点，默认不索引 |
| 链接 | 重解析点默认不跟随；`FILE_FLAG_OPEN_REPARSE_POINT` 识别 | symlink 默认不跟随；`O_NOFOLLOW` |
| 保留名 | `CON/PRN/AUX/NUL/COM1-9/LPT1-9` | 无 |
| 编码 | 内部 UTF-16；展示 UTF-8 | 允许非 UTF-8 字节文件名；`OsStr` + `lossy` 处理 |
| 归一化 | `\\?\` 前缀、去尾分隔符、盘符大写 | 折叠重复 `/`，逻辑解析 `.`/`..`（不跟随 symlink） |
| 回收站 | `$Recycle.Bin` 排除 | `~/.local/share/Trash` 排除 |

### 12.2 权限

| 项 | Windows | Linux |
| --- | --- | --- |
| 提权 | **不需要**管理员；不请求 UAC；受限目录跳过 | **不需要** root；受限目录跳过；以 root 运行时警告索引归属 |
| 判定 | `ERROR_ACCESS_DENIED` → `ERR_PERMISSION_DENIED` | `EACCES`/`EPERM` → `ERR_PERMISSION_DENIED` |
| 独占锁 | `CreateFile` + `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK)` | `flock(LOCK_EX\|LOCK_NB)` |
| IPC 对端校验 | `GetNamedPipeClientProcessId` + 令牌 SID | `SO_PEERCRED` uid |
| 文件被占用 | 打开失败 → 记录并稍后重试一次（不阻塞） | 通常无强制占用；仍可能 `EACCES` |
| 提权提示 | 永不触发 | 永不触发 |

### 12.3 文件监控

| 项 | Windows | Linux |
| --- | --- | --- |
| API | `ReadDirectoryChangesW`（`OVERLAPPED` 异步 + `FILE_FLAG_BACKUP_SEMANTICS`） | `inotify`（逐目录 `inotify_add_watch`） |
| 递归 | 内建（单句柄监视子树） | 需遍历加 watch |
| 限制 | 单句柄缓冲 64KB；`ERROR_NOTIFY_ENUM_DIR` | `max_user_watches`（默认 8192）、`max_user_instances`（128）、`IN_Q_OVERFLOW` |
| 溢出降级 | 标记受影响根 → 子树重扫 | 脏目录集合重扫；超限则卷级重扫 |
| 大规模策略 | 卷根单句柄 | 热目录全 watch；冷目录定时轮询（默认 10min） |
| 移动识别 | 无 cookie（USN Journal 可选，需管理员，默认关） | `IN_MOVED_FROM/TO` cookie 配对 |
| 网络卷 | 不可靠 → 定时轮询 | 不可靠 → 定时轮询或排除 |
| 资源回收 | 目录删除后关闭句柄；定期核对 | `IN_IGNORED` 自动清理 |
| 休眠恢复 | 恢复后重扫 | 恢复后重扫 |

### 12.4 桌面集成

| 项 | Windows | Linux |
| --- | --- | --- |
| 快捷方式 | 开始菜单项（可选桌面） | `~/.local/share/applications/snapfind.desktop`（`Categories=Utility;`） |
| 打开文件夹 | `explorer /select,"<path>"` 或 `ShellExecuteW`（参数数组） | `org.freedesktop.FileManager1.ShowItems` → `gio open` → `xdg-open` |
| 打开文件 | `ShellExecuteW(NULL,"open",path,NULL,dir,SW_SHOW)` | `xdg-open <file>`；失败提示「未找到默认应用」 |
| 剪贴板 | `SetClipboardData(CF_UNICODETEXT)` | `arboard` / `wl-copy`（Wayland）/ `xclip`（X11） |
| URI scheme | 不注册（v1） | 不注册（v1） |
| 通知 | Toast（可选，仅本地） | `org.freedesktop.Notifications`（可选） |
| 主题 | 跟随系统（`AppsUseLightTheme`） | 跟随 XDG 门户 `color-scheme` |
| DPI/缩放 | Per-Monitor V2 | GTK/Qt scale factor + `Xft.dpi` |
| 文件关联 | 不劫持任何扩展名 | 不劫持 |
| 右键菜单集成 | 不做（v1） | 不做（v1） |

### 12.5 安装与卸载

| 项 | Windows x64 / arm64 | Linux x64 / arm64 |
| --- | --- | --- |
| 构建物 | MSI（WiX）+ NSIS；按架构分别出包 `SnapFind-win-x64.msi` / `SnapFind-win-arm64.msi` | deb（`amd64`/`arm64`）、rpm（`x86_64`/`aarch64`）、AppImage（`x86_64`/`aarch64`），可选 `tar.gz` |
| 安装位置 | MSI：`C:\Program Files\SnapFind`（UAC）；NSIS 用户级：`%LOCALAPPDATA%\Programs\SnapFind`（免 UAC） | deb/rpm：`/usr/bin/snapfind` + `/usr/share/snapfind`；AppImage 用户自选 |
| 运行时依赖 | WebView2 Runtime（Evergreen，缺则由安装器引导，**需联网**；离线场景提供 bootstrapper 说明）；arm64 需 arm64 WebView2 | WebKitGTK 4.1（deb/rpm 声明依赖）；AppImage 要求系统存在 webkit2gtk |
| 索引目录 | §9.1（默认程序目录，通常降级到 `%LOCALAPPDATA%`） | §9.1（`/opt` 场景常降级到 XDG） |
| 卸载清理 | 卸载器移除程序与快捷方式；**询问**是否删除索引与配置（默认保留并提示路径） | `apt remove` / `rpm -e` 移除程序；用户数据保留；提供 `--purge-data` 说明 |
| 静默安装 | `msiexec /i SnapFind-win-x64.msi /qn` | `apt install -y ./snapfind_*.deb` |
| 更新 | 不内置自动更新；可选「检查更新」默认关闭且不联网 | 同左（交由包管理器） |
| 签名 | 不做代码签名（开源分发）；发布 SHA256 + 构建说明 | 同左，另提供可选 `minisign` |
| 卸载后残留 | 无后台进程、无服务、无计划任务 | 无 systemd 服务（v1 不注册服务） |

### 12.6 进程管理

| 项 | Windows | Linux |
| --- | --- | --- |
| 启动子进程 | `CreateProcessW`（命令行传 token），加入 Job Object | `Command::spawn`，子进程内立即 `prctl(PR_SET_PDEATHSIG, SIGTERM)` |
| 崩溃兜底 | Job Object `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` | `PDEATHSIG` |
| 心跳兜底 | 3×5s 无 pong → 自退 | 同左 |
| 优先级 | `IDLE_PRIORITY_CLASS`；线程 `THREAD_MODE_BACKGROUND_BEGIN` | `nice 19` / `SCHED_IDLE` |
| IO 限速 | Job Object IO Rate Control（Win10+），不支持时占空比 | `ionice -c 3`；可选 cgroup v2 |
| 关机/注销 | `WM_ENDSESSION`、`CTRL_SHUTDOWN_EVENT`、`CTRL_LOGOFF_EVENT` | `SIGTERM`/`SIGINT`（会话管理器发送） |
| 单实例 | 命名互斥量 `SnapFind-UI-<user>`；第二实例激活已有窗口 | `flock` 于 `$XDG_RUNTIME_DIR/snapfind-ui.lock` |
| 多用户 | 管道名含 sessionId，会话隔离 | socket 路径含 uid，用户隔离 |
| 后台驻留 | 默认无；托盘需显式开启 | 同左 |

### 12.7 默认排除（对照汇总）

| 类别 | Windows | Linux |
| --- | --- | --- |
| 系统/虚拟 | `C:\Windows`、`$Recycle.Bin`、`System Volume Information`、`Recovery` | `/proc`、`/sys`、`/dev`、`/run`、`/tmp` |
| 缓存 | `AppData\Local\Temp`、`INetCache`、`CrashDumps` | `~/.cache`、`/var/cache`、Trash |
| 开发依赖 | `node_modules`、`.git\objects`、`target\{debug,release}`、`build`、`dist`、`.venv`、`__pycache__` | 同左（大小写敏感） |
| 包管理 | `.cargo\registry`、`.nuget`、`.m2`、`.gradle`、`.npm\_cacache` | `.cargo/registry`、`.m2`、`.gradle`、`.npm/_cacache`、`/var/lib/{apt,dnf}` |
| 大文件/镜像 | `*.vhdx`、`*.vhd`、`*.iso`、`pagefile.sys` | `*.qcow2`、`*.iso`、`/var/lib/docker/overlay2` |
| 自身 | `<索引目录>/**` | `<索引目录>/**` |
| 网络/可移动 | 默认排除（可勾选） | 默认排除（可勾选） |

---

## 13. 构建、CI 与测试

### 13.1 本地构建

| 目标 | 命令（示例） |
| --- | --- |
| Windows x64 | `cargo build --release --target x86_64-pc-windows-msvc` 然后 `cargo tauri build --target x86_64-pc-windows-msvc` |
| Windows arm64 | `cargo build --release --target aarch64-pc-windows-msvc` 然后 `cargo tauri build --target aarch64-pc-windows-msvc` |
| Linux x64 | `cargo build --release --target x86_64-unknown-linux-gnu` 然后 `cargo tauri build` |
| Linux arm64 | arm64 runner 原生构建，或交叉 `cargo zigbuild --target aarch64-unknown-linux-gnu`（需 sysroot/依赖交叉包） |
| 质量门 | `cargo fmt --check && cargo clippy -- -D warnings && cargo deny check` |

构建要求：

* `rust-toolchain.toml` 固定 Rust 版本（如 `1.82`）；`Cargo.lock` 入库。
* SQLite 用 `rusqlite` bundled（消除系统版本差异）；需 C 编译器（Windows MSVC / Linux cc；arm64 交叉用 `cargo-zigbuild`）。
* `default = ["backend-sqlite"]`；`network` feature **默认关闭**；`ocr`、`pdfium`、`tantivy` 为可选 feature，默认关闭。
* 依赖准入：新增依赖须登记到 §6.4 矩阵并写明四目标可用性；具备网络能力的依赖必须归入 `network` feature 并在文档列出。

### 13.2 CI 矩阵（明确无 macOS）

```yaml
name: ci
on: [push, pull_request]
concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true
env:
  CARGO_TERM_COLOR: always
  RUSTFLAGS: "-D warnings"

jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { components: "rustfmt,clippy" }
      - run: cargo fmt --all -- --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - uses: EmbarkStudios/cargo-deny-action@v2

  build:
    needs: lint
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: windows-latest
            target: x86_64-pc-windows-msvc
            arch: x64
            pkg: msi,nsis
          - os: windows-11-arm
            target: aarch64-pc-windows-msvc
            arch: arm64
            pkg: msi,nsis
          - os: ubuntu-latest
            target: x86_64-unknown-linux-gnu
            arch: x64
            pkg: deb,rpm,appimage
          - os: ubuntu-24.04-arm
            target: aarch64-unknown-linux-gnu
            arch: arm64
            pkg: deb,rpm,appimage
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: "${{ matrix.target }}" }
      - name: Install Linux deps
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev \
            libayatana-appindicator3-dev librsvg2-dev patchelf rpm
      - uses: swatinem/rust-cache@v2
        with: { key: "${{ matrix.target }}" }
      - run: cargo build --release --frozen
      - run: cargo test --release --workspace --no-fail-fast
      - run: cargo tauri build --target ${{ matrix.target }}
      - uses: actions/upload-artifact@v4
        with:
          name: snapfind-${{ runner.os }}-${{ matrix.arch }}
          path: target/${{ matrix.target }}/release/bundle/**

  smoke:
    needs: build
    strategy:
      fail-fast: false
      matrix:
        os: [windows-latest, ubuntu-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: actions/download-artifact@v4
        with: { path: dist }
      - run: ./scripts/smoke.${{ runner.os == 'Windows' && 'ps1' || 'sh' }} dist
```

约束：

* 矩阵**仅**包含 `windows-latest`、`windows-11-arm`、`ubuntu-latest`、`ubuntu-24.04-arm`；**不得出现任何 macOS runner 或 macOS 打包步骤**。
* `windows-11-arm` / `ubuntu-24.04-arm` 若在目标 CI 平台不可用，改用同名 self-hosted arm64 runner（标签 `self-hosted, arm64, windows` / `self-hosted, arm64, linux`），矩阵结构不变。
* arm64 job 失败不得阻断 x64 发布以外的质量门（`fail-fast: false`），但发布前必须四目标全绿。

### 13.3 冒烟测试步骤（每个目标构建物均执行）

1. 冷启动（无索引）：应显示空状态 → 进入范围确认页 → 展示卷列表与默认排除项。
2. 选择一个小语料目录（测试用 fixture，≤ 5000 文件）开始索引：进度事件出现且频率 ≤ 20 条/s。
3. 索引进行中搜索：结果分批出现，UI 可滚动、可切页、可改排序，无卡顿。
4. 索引就绪：搜索已知关键词命中；命中片段高亮正确（HTML 已转义）。
5. 预览：打开命中项 → 骨架屏 → 内容出现；执行「打开所在文件夹」「复制路径」。
6. 过滤器：按类型/大小/时间/路径各过滤一次，结果符合预期。
7. 增量：外部修改一个已索引文件 → ≤10s 后可搜到新内容；删除文件 → 结果消失。
8. 暂停/恢复/停止/重建/清理：每项操作后状态面板与查询行为符合 §5.2。
9. 关闭主界面：观察索引进程退出（任务管理器 / `ps`），无残留；索引目录出现 `.idx` 检查点。
10. 断点续建：索引中途关闭 → 重启 → 从检查点继续，最终 docId 集合与全量一致。
11. 隐私：断网执行 1–10，全部通过；`--privacy-audit` 报告「无网络能力」。
12. 卸载：卸载后无残留进程/服务；按提示选择性删除数据。

### 13.4 测试分层

| 层 | 范围 | 工具 | 覆盖目标 |
| --- | --- | --- | --- |
| L1 单元 | 分词器、语法解析、过滤器下推、状态机迁移、段编解码、路径归一化 | `cargo test`（`proptest` 可选） | 行覆盖 ≥ 80% 核心 crate |
| L2 组件 | 抽取器（各类型 fixture）、IPC 编解码、checkpoint 原子性 | `cargo test` + fixture 仓 | 每个抽取类型 ≥ 3 个正例 + 3 个异常例 |
| L3 集成 | indexd 端到端（启动→索引→查询→关闭）、IPC 全链路、卷隔离 | `cargo test --test e2e` + 临时目录 | 关键路径 100% |
| L4 UI | 防抖、虚拟滚动、requestId 丢弃、骨架屏、快捷键 | Playwright（WebDriver via Tauri） | §10.2 全部指标 |
| L5 端到端 | 四目标构建物冒烟（§13.3） | 脚本 + CI `smoke` job | 每目标每发布通过 |
| L6 性能 | 四类基准（§13.6） | 基准脚本 | 相对基线回归 ≤ 10% 报警 |
| L7 故障注入 | §13.5 清单 | 注入开关 + 脚本 | 每项有断言 |

### 13.5 故障注入清单

| 编号 | 注入点 | 手段 | 断言 |
| --- | --- | --- | --- |
| `fi-kill` | 索引写入中随机强杀 | `kill -9` / `TerminateProcess` | 重启后可加载；最近 checkpoint 可用；无 `tmp/` 残留 |
| `fi-corrupt` | 段文件随机字节翻转 | 脚本改字节 + 更新/不更新 crc | 加载或 `verify` 检出；段被隔离；卷 Degraded；其他段可查 |
| `fi-volume-fail` | 单卷 IO 错误 / 拔盘 | 临时卸载（Linux）/ 弹出（Win）或 mock IO 错误 | 该卷 Unavailable；其他卷查询正常 |
| `fi-permission` | 无权限目录 | 创建 0000 目录（Linux）/ ACL 拒绝（Win） | 跳过并计数；不失败卷 |
| `fi-ui-crash` | UI 崩溃 | 强杀 UI 进程 | indexd ≤15s 自退；无后台驻留 |
| `fi-indexd-crash` | 索引进程崩溃 | 强杀 indexd | UI 提示并可重启服务；不无限重试 |
| `fi-memory` | 内存上限 | 小语料 + 强制 `memoryBudgetBytes=64MB` | 触发事件、跳过大文件、不 OOM、不崩溃 |
| `fi-timeout` | 抽取超时 | 构造慢文件（大 PDF/嵌套 ZIP） | 超时跳过并记录；不阻塞批次 |
| `fi-query-timeout` | 查询超时 | `timeoutMs=1` | 返回部分结果 + warning；不悬挂 |
| `fi-watch-overflow` | 事件溢出 | 批量创建 10 万文件 | 降级重扫；结果与全量一致 |
| `fi-disk-full` | 磁盘写满 | 小容量 tmpfs（Linux）/ 配额（Win） | 卷 Paused + 提示；不写坏 |
| `fi-bad-config` | 配置损坏 | 写入非法 JSON | 备份为 `config.json.bak`，回退默认，可启动 |
| `fi-protocol-mismatch` | 协议不一致 | 手工发错误 `v` | `ERR_PROTOCOL_MISMATCH`，连接关闭 |
| `fi-build-mismatch` | 新旧混跑 | 改 `meta.db.build_id` | `ERR_BUILD_MISMATCH`，indexd 拒绝启动 |
| `fi-auth` | 错误令牌 | 传错 token | `ERR_AUTH_FAILED`，无数据泄露 |
| `fi-interrupt` | 频繁中断 | 循环启动/关闭 100 次 | 最终 docId 集合与全量一致 |

### 13.6 基准脚本与产出

| 脚本 | 采集指标 | 产出 |
| --- | --- | --- |
| `scripts/bench-index.{ps1,sh}` | 吞吐（文件/s、MB/s）、CPU、RSS 峰值、耗时、失败数 | `bench/out/index-<platform>-<arch>-<date>.csv` + `.md` |
| `scripts/bench-size.{ps1,sh}` | 源文本量、索引体积、按段类型拆分、压缩比 | `bench/out/size-*.csv` + `.md` |
| `scripts/bench-query.{ps1,sh}` | 20 条固定查询的 p50/p95/p99、首批延迟、命中数 | `bench/out/query-*.csv` + `.md` |
| `scripts/bench-memory.{ps1,sh}` | indexd RSS 时序峰值、UI RSS 空闲/峰值、触发 memoryPressure 次数 | `bench/out/memory-*.csv` + `.md` |
| `scripts/bench-ui.{ps1,sh}` | 按键回显、防抖、首批展示、长任务、FPS、预览首块 | `bench/out/ui-*.csv` + `.md` |

四类核心基准（索引体积、内存峰值、搜索延迟、UI 响应性）必须在每个发布候选的四目标上各跑一次，报告随发布附出；报告头部强制字段：`platform`、`arch`、`os_version`、`cpu`、`ram`、`disk`、`corpus`、`version`、`commit`、`config_hash`。

---

## 14. 里程碑与任务分解

### 14.1 M1–M5

| 里程碑 | 交付内容 | 出口条件（Exit Criteria） |
| --- | --- | --- |
| **M1 骨架与协议** | 双进程骨架（`snapfind` + `snapfind-indexd`）；命名管道/UDS 传输；令牌握手 + 版本协商 + `buildId` 校验；信封与 `hello/heartbeat/shutdown/bye`；单实例锁；基础 CI 四目标能编过 | 四个目标可启动双进程并握手成功；错误令牌/版本不符/新旧混跑三类用例全部返回对应错误码；关闭主界面后索引进程 100% 退出（无残留，含 UI 强杀场景）；CI 矩阵四目标绿 |
| **M2 索引主干** | 卷发现与范围确认页；索引目录与降级探针；目录结构与 `meta.db`；扫描 + 抽取（纯文本/代码/日志/MD/HTML/RTF）+ 分词 + 段写入 + 合并；checkpoint 与断点续建；单卷状态机全迁移 | 单卷全量索引可完成；中断 100 次后 docId 集合与全量一致；`fi-kill`/`fi-disk-full`/`fi-permission` 通过；索引体积 ≤ 源文本 12%；内存峰值 ≤ 512MB |
| **M3 查询与预览** | 语法解析（含布尔/通配符/字段限定）；过滤器下推；排序/分页/流式；高亮与片段；预览分块 + 骨架屏；打开/打开文件夹/复制路径；搜索历史与快捷键 | 20 条固定查询 p95 < 200ms（基准语料）；首批 ≤200ms；语法错误返回 `ERR_QUERY_SYNTAX` 带位置；预览零门槛可验收（无任何登录/付费/扫码入口）；旧 requestId 分片不渲染 |
| **M4 多卷、增量与 Office/PDF** | 多卷并行调度（默认 min(卷数,2)、低配降 1、可配 1–4）；卷独立控制（暂停/恢复/停止/重建/清理/改排除）；fs-watch 增量与溢出降级；PDF/DOCX/XLSX/PPTX/EPUB 抽取（惰性加载）；节流档位；内存分级控制 | 多卷并行时单卷失败不影响其他卷与查询；增量生效 ≤10s；`fi-watch-overflow` 通过；11 类格式抽取各自正例/异常例通过；节流三档速率达标 |
| **M5 硬化与发布** | 损坏检测/隔离/重建；完整错误码覆盖；UI 性能达标（§10.2 全部）；四类基准脚本 + 报告；四目标安装包（MSI/NSIS、deb/rpm/AppImage）；文档（构建/索引格式/隐私/贡献/许可证）；冒烟测试全绿 | 四目标安装包可安装可卸载无残留；§15 验收清单 100% 通过；§10.2 指标全部达标；基准报告随发布附出（含平台与架构）；§18.3 发布文档清单逐项打勾；CI 无 macOS 任务 |

### 14.2 WBS 摘要

| 编号 | 工作包 | 主要产出 | 关联里程碑 |
| --- | --- | --- | --- |
| WP1 | 进程与生命周期 | 双进程启动器、Job Object/PDEATHSIG、单实例、关闭时序 | M1 |
| WP2 | IPC 协议 | 信封、编解码、握手鉴权、心跳、流式与取消、错误码 | M1 |
| WP3 | 卷管理 | 卷发现、volumeId、范围确认页数据、状态机与迁移引擎 | M1/M2 |
| WP4 | 索引目录 | 目录布局、写探针与降级、`meta.db` schema、lock | M2 |
| WP5 | 扫描与排除 | 目录遍历器、glob 匹配、平台默认排除名单 | M2 |
| WP6 | 抽取层 | `Extractor` trait、11 类实现、Limits/超时/降级、插件（OCR/PDF 渲染） | M2/M4 |
| WP7 | 分词与写入 | Tokenizer、批处理、hot 段、段合并、zstd 压缩 | M2 |
| WP8 | 检查点 | checkpoint 结构、原子写、恢复、断点续建 | M2 |
| WP9 | 查询引擎 | 解析器、AST 优化、过滤器下推、评分、排序、游标 | M3 |
| WP10 | 高亮与片段 | 偏移定位、片段生成、转义 | M3 |
| WP11 | 预览与文件操作 | 分块读取、骨架屏、OS 集成（打开/复制） | M3 |
| WP12 | 增量与监控 | fs-watch 封装、去抖合并、溢出降级、启动兜底重扫 | M4 |
| WP13 | 资源控制 | 内存预算与分级、IO 节流三档、并行调度 | M4 |
| WP14 | 可靠性与维护 | 校验和、verify/compact/clean、隔离与重建 | M5 |
| WP15 | UI | 六个界面、虚拟滚动、异步渲染、快捷键、设置页 | M2–M5 |
| WP16 | 构建与 CI | 四目标 CI 矩阵、打包（MSI/NSIS/deb/rpm/AppImage）、可复现构建说明 | M1/M5 |
| WP17 | 测试与基准 | L1–L7 分层、故障注入、四类基准脚本与报告 | M2–M5 |
| WP18 | 文档与合规 | 构建文档、索引格式说明、隐私说明、贡献指南、许可证、依赖矩阵 | M5 |

---

## 15. 验收标准（逐条可判定）

> 判定方式：A=自动化测试；M=人工按步骤核对；B=基准报告；L=日志/产物检查。每条仅「通过 / 不通过」，不接受「基本满足」。

### 15.1 功能

| # | 验收项 | 判定 | 方式 |
| --- | --- | --- | --- |
| F1 | 搜索覆盖文件名、路径、文件内容三域，且可分别关闭 | 三域各构造正例，命中字段 `matchedFields` 正确 | A |
| F2 | 支持关键词、短语、布尔（AND/OR/NOT）、通配符 | 每类 ≥5 用例通过 | A |
| F3 | 正则与模糊为可选开关且**默认关闭** | 默认配置下 `/re/` 与 `foo~1` 不生效且给出提示 | A |
| F4 | 11 类格式（txt/代码/日志/MD/HTML/RTF/PDF/DOCX/XLSX/PPTX/EPUB）可检索内容 | 每类 ≥1 正例命中内容 | A |
| F5 | OCR 默认不启用 | 设置页显示关闭；图像文件不产生内容命中 | A+M |
| F6 | 预览不扫码/不登录/不付费/无云校验 | 全应用无二维码、无登录页、无付费入口；断网可用 | M |
| F7 | 支持打开文件、打开所在文件夹、复制路径 | 三操作均成功（Win/Linux 各验一次） | M |
| F8 | 结果高亮 + 命中片段预览 | 片段含 `<mark>` 且原文已转义（含 `<script>` 样本不执行） | A |
| F9 | 过滤器：类型/大小/修改时间/路径 | 四类过滤器各 ≥3 用例 | A |
| F10 | 排序：相关度/名称/大小/修改时间 | 四种排序结果序正确 | A |
| F11 | 分页或虚拟滚动；搜索历史；快捷键 | 10000 条结果滚动 ≥55FPS；历史可回填；快捷键表全部可用 | A+M |

### 15.2 索引

| # | 验收项 | 判定 | 方式 |
| --- | --- | --- | --- |
| I1 | 默认索引整机全盘（所有可访问卷） | 范围确认页列出全部本地固定卷且默认勾选 | M |
| I2 | 首次索引前有一次性范围确认页，展示卷/文件系统/容量/预计量级/索引目录/默认排除项，可取消勾选卷 | 页面字段齐全；取消勾选的卷不被索引 | M |
| I3 | 默认索引目录 `<程序目录>/index`；不可写时按平台降级并明确提示原因 | 构造只读程序目录，观察降级与提示文案含原因码 | A+M |
| I4 | 索引目录结构含 `meta.db`、`lock`、`vol-<volumeId>/{segments,hot,docs.db,state.json}`、`tmp` | 目录树快照比对 | A |
| I5 | 索引为独立进程，IPC 走命名管道/UDS，**不监听 TCP** | `netstat -ano`/`ss -ltnp` 无 SnapFind 监听端口；连接类型为 pipe/UDS | A |
| I6 | 一次性令牌鉴权；协议版本协商；禁止新旧二进制混跑 | 三类负例分别返回 `ERR_AUTH_FAILED`/`ERR_PROTOCOL_MISMATCH`/`ERR_BUILD_MISMATCH` | A |
| I7 | 多盘同时索引，每卷独立状态机与独立控制（暂停/恢复/停止/重建/清理/改排除） | 两卷并行，对 A 卷 pause 不影响 B 卷与查询 | A |
| I8 | 并行卷数默认 min(卷数,2)，低配自动降 1，可配 1–4 | 配置读取与低配模拟（cgroup/启动参数）均正确 | A |
| I9 | 单卷失败或卡住不影响其他卷与查询服务 | `fi-volume-fail` 通过 | A |
| I10 | 增量索引 + 文件变更监控；未变更文件不重复索引（mtime+size 为主） | 修改 1 个文件仅 1 个文档被重写（日志断言） | A |
| I11 | 首次索引可中断、可恢复（检查点断点续建） | `fi-interrupt` 100 次后集合一致 | A |
| I12 | 后台低优先级索引；IO 节流三档（低/中/高，默认低） | 进程优先级为 IDLE/nice19；三档实测速率落在区间 | A+B |
| I13 | 轻量索引：压缩倒排、必要 token/偏移、分片、冷热分层、可重建 | 段文件为压缩结构；`storePositions=false` 体积下降 | A+B |
| I14 | 提供索引体积统计与清理工具 | `--stats`/`--compact`/`--clean` 可执行；UI 面板显示一致 | A+M |
| I15 | 低内存：流式/批处理；全局上限可配（默认 512MB）；大文件分块；超限跳过并记录；抽取文本截断 | `bench-memory` 峰值 ≤ 预算；超限事件出现 | A+B |
| I16 | 可靠性：分片校验和；损坏可检测/隔离/重建；权限不足跳过并记录；崩溃安全写入（WAL/原子替换） | `fi-corrupt`/`fi-permission`/`fi-kill` 通过 | A |

### 15.3 界面与生命周期

| # | 验收项 | 判定 | 方式 |
| --- | --- | --- | --- |
| U1 | UI 主线程零阻塞（无同步 IO/同步 IPC/大数组同步计算） | 静态审计 + lint 规则命中数 = 0 | A |
| U2 | 索引高负载下搜索/预览/切页/改设置保持可交互 | 满载索引时 §10.2 指标 1/3/7/13 达标 | A+B |
| U3 | 按键回显 ≤ 1 帧 | p95 ≤ 16.7ms | B |
| U4 | 搜索输入防抖 ≤ 150ms | 常量与实测均 ≤150ms | A+B |
| U5 | 索引就绪时首批结果 ≤ 200ms 展示并分批追加 | p95 ≤ 200ms 且存在多批 | B |
| U6 | 长列表虚拟滚动 | 10000 条 DOM 节点 ≤ 可视窗口×3 | A |
| U7 | 进度事件节流 ≤ 50ms/次 | 事件时间戳差值 ≥50ms | A |
| U8 | 切换查询后旧结果按 requestId 立即丢弃 | 注入延迟响应，旧分片不渲染 | A |
| U9 | 大文件预览分块 + 骨架屏 | 骨架屏 ≤300ms 出现；分块读取（日志记录块数 >1） | A |
| U10 | 关闭主界面 = 关闭索引进程（默认且唯一默认） | 关闭后 5s 内索引进程消失；托盘未开启时无残留 | A |
| U11 | 超时默认 5s（可配 3–10s）后强制终止并提示「索引已保存至最近检查点」 | 注入慢 checkpoint，观察提示文案与强杀 | A |
| U12 | 系统关机/注销走同一流程 | Windows `WM_ENDSESSION` / Linux `SIGTERM` 模拟，均落盘检查点 | A |
| U13 | UI 崩溃/被强杀时索引进程心跳超时（双向 5s，连续 3 次无 pong）后落盘检查点并自退，不驻留 | `fi-ui-crash` 通过；进程表无残留 | A |
| U14 | 托盘常驻只能由用户显式开启，设置中有文字说明，且非默认 | 默认配置 `ui.trayEnabled=false`；说明文案存在 | A+M |

### 15.4 平台与合规

| # | 验收项 | 判定 | 方式 |
| --- | --- | --- | --- |
| P1 | 四目标（Win x64/arm64、Linux x64/arm64）均提供可安装/可运行构建物 | 四个安装包可安装并启动（Win: MSI/NSIS；Linux: deb/rpm/AppImage） | M |
| P2 | 不构建/打包/签名/发布 macOS；CI 无 macOS 任务 | CI 配置文件全文检索无 `macos`；产物列表无 macOS | A |
| P3 | Windows 与 Linux 在路径、权限、监控、桌面集成、安装卸载分别适配，差异写入文档 | §12 对照表逐项可核对（路径/权限/监控/桌面集成/安装卸载/进程管理/默认排除 七类齐全） | L |
| P4 | 第三方依赖四目标可用性逐一确认，不可用给降级方案，矩阵结论入构建文档 | §6.4 矩阵每行有结论；构建文档含验证记录 | L |
| P5 | 默认无遥测、无网络依赖；索引与配置全本地；提供许可证、构建文档、索引格式说明、隐私说明、贡献指南 | 断网冒烟通过；`--privacy-audit` 通过；五份文档存在且内容完整 | A+L |
| P6 | 性能目标：常见查询 < 200ms（百万级参考，可复现基准报告标注平台与架构）；UI 空闲内存 < 150MB；四类基准脚本齐备 | 报告齐备且指标达标 | B |
| P7 | 产品名统一为 SnapFind；宣传语与定位未被改写 | 全文检索产品名一致；宣传语逐字一致 | L |
| P8 | 不出现任何具体对标产品名称 | 全文与发布物料检索无此类名称 | L |

### 15.5 验收阻断项（任一不通过即不可发布）

1. U10（关闭即退出）、U14（托盘非默认）、I5（无 TCP 监听）、P2（无 macOS）任一不通过。
2. 默认配置启用 OCR、遥测或任何网络请求。
3. §10.2 中 1/2/3/5/6 任一项不达标。

---

## 16. 交付物清单

### 16.1 构建物（四目标）

| 序号 | 交付物 | 目标 | 说明 |
| --- | --- | --- | --- |
| D1 | `SnapFind-win-x64.msi` | Windows x64 | WiX 打包 |
| D2 | `SnapFind-win-x64-setup.exe` | Windows x64 | NSIS 用户级安装 |
| D3 | `SnapFind-win-arm64.msi` | Windows arm64 | WiX 打包 |
| D4 | `SnapFind-win-arm64-setup.exe` | Windows arm64 | NSIS |
| D5 | `snapfind_<ver>_amd64.deb` | Linux x64 | 声明 webkit2gtk 依赖 |
| D6 | `snapfind-<ver>-1.x86_64.rpm` | Linux x64 | — |
| D7 | `SnapFind-<ver>-x86_64.AppImage` | Linux x64 | — |
| D8 | `snapfind_<ver>_arm64.deb` | Linux arm64 | — |
| D9 | `snapfind-<ver>-1.aarch64.rpm` | Linux arm64 | — |
| D10 | `SnapFind-<ver>-aarch64.AppImage` | Linux arm64 | — |
| D11 | `SHA256SUMS` + `SHA256SUMS.minisign` | 全部 | 校验与可选签名 |

> 明确不产出：任何 macOS 构建物（`.dmg`/`.app`/`.pkg`）、任何移动端/网页版产物。

### 16.2 文档

| 序号 | 交付物 | 要点 |
| --- | --- | --- |
| D12 | `README.md` | 宣传语、定位、截图、快速开始、平台支持矩阵 |
| D13 | `docs/DEV.md`（本文档） | 架构、协议、索引、验收 |
| D14 | `docs/BUILD.md` | 四目标构建步骤、依赖矩阵验证记录、可复现构建说明、CI 说明 |
| D15 | `docs/INDEX_FORMAT.md` | 索引目录结构、段格式、编码、压缩、校验和、版本迁移 |
| D16 | `docs/PRIVACY.md` | 隐私承诺、数据清单、网络能力声明、删除方式 |
| D17 | `docs/CONTRIBUTING.md` | 分支策略、提交规范、测试要求、依赖准入流程、代码风格 |
| D18 | `LICENSE`（Apache-2.0） + `NOTICE` | 见 §16.6 |
| D19 | `docs/THIRD_PARTY.md` | 依赖清单 + 许可证 + 四目标可用性矩阵 |
| D20 | `docs/BENCHMARKS.md` | 四类基准方法与最近一次报告（含平台与架构） |
| D21 | `docs/PLATFORM.md` | 平台差异摘要（引 §12） |
| D22 | `CHANGELOG.md` | 版本变更 |

### 16.3 代码与工程

| 序号 | 交付物 |
| --- | --- |
| D23 | 源码仓库（`crates/` 拆分：`snapfind-ipc`、`snapfind-index`、`snapfind-extract`、`snapfind-query`、`snapfind-platform`、`snapfind-ui`、`snapfind-indexd`） |
| D24 | `ui/`（React + TS + Vite） |
| D25 | CI 配置（§13.2，四目标，无 macOS） |
| D26 | 测试套件（L1–L7）+ fixture 仓 |
| D27 | 基准脚本 `scripts/bench-*.{ps1,sh}` + 冒烟脚本 `scripts/smoke.{ps1,sh}` |
| D28 | 故障注入脚本 `scripts/fi/*` |

### 16.4 运维/支持物料

| 序号 | 交付物 |
| --- | --- |
| D29 | 安装/卸载说明（含静默安装、WebView2 离线场景） |
| D30 | 常见问题（索引慢、卷不可用、降级目录、权限不足） |
| D31 | 索引格式迁移与「清理/重建」操作指引 |

### 16.5 质量证据

| 序号 | 交付物 |
| --- | --- |
| D32 | 四目标冒烟测试记录（截图/日志） |
| D33 | 四类基准报告（CSV + Markdown） |
| D34 | 故障注入执行报告（16 项全绿） |
| D35 | 验收清单（§15）逐条签署表 |

### 16.6 许可证选型

| 选项 | 优点 | 缺点 | 结论 |
| --- | --- | --- | --- |
| **Apache-2.0** | 明确专利授权与终止条款；允许商用与再分发；对贡献者有明确专利让渡；与 Rust 生态主流依赖兼容；含商标条款（保护产品名） | 需保留 NOTICE；与 GPL-2.0-only 存在兼容摩擦（本项目无此类依赖） | **推荐** |
| MIT | 极简、最易被采用 | 无专利授权条款，企业采用时法律审查成本高；无商标保护 | 备选 |
| GPL-3.0 | 强 copyleft，保证衍生开源 | 传染性会阻碍企业内部集成与二次分发；与部分抽取库/插件动态加载产生许可争议 | 不采用 |

**推荐 Apache-2.0。理由**：①「开源、可审计」的定位需要清晰的专利与商标保护，避免第三方以同名分发误导用户；② 便于企业用户合规采用，符合轻量工具的传播诉求；③ 与 Rust 生态（MIT/Apache-2.0 双许可为主）兼容成本最低。
**风险**：与 GPL 系依赖不兼容 → **对策**：依赖准入流程禁止 GPL 系进入默认构建（`cargo deny` 白名单），必要时以独立进程/可选插件方式隔离。

---

## 17. 风险与对策表

| # | 风险 | 影响 | 对策 | 触发信号 |
| --- | --- | --- | --- | --- |
| R1 | 首次全盘索引耗时过长（机械盘/大容量） | 用户放弃 | 默认低节流 + 断点续建 + 范围确认页可只勾部分卷 + ETA 与吞吐可见 + 先索引用户主目录（可配优先级） | `etaSec` > 数小时；用户中途取消率 > 30% |
| R2 | 索引体积超预期 | 占盘、卸载阻力 | zstd 冷段 + 可选关闭位置 + 停用词/长度阈值 + 体积面板 + 清理工具；发布前三平台比率基准 | `bench-size` 比率 > 12% |
| R3 | 内存峰值超预算 | OOM、系统卡顿 | 分级降批次 + 大文件跳过 + 预算可配 + `bench-memory` 门禁 | `event.memoryPressure` 频次高；RSS > 预算 |
| R4 | 中文分词质量不足（bigram） | 召回噪声、相关度差 | 默认 bigram；提供词典分词插件；字段权重调优；查询侧短语优先 | 相关性抽检（20 查询）满意度 < 80% |
| R5 | Windows arm64 生态（WebView2/CI runner/pdfium） | 该目标无法交付 | 依赖矩阵前置验证；arm64 预编译缺失时功能降级（PDF 渲染/OCR 可选化）；CI 用 self-hosted arm64 兜底 | arm64 job 连续失败；依赖无 arm64 产物 |
| R6 | Linux inotify watch 上限 | 大目录增量失效 | 热/冷分层策略 + 溢出降级重扫 + 启动时提示调大 `max_user_watches` | `IN_Q_OVERFLOW` 频发 |
| R7 | 关闭时 checkpoint 超 5s | 用户感知卡顿/被强杀 | checkpoint 增量小、周期性预写、关闭时只做「收尾 + 原子替换」；超时文案明确 | `shutdown.ack` 超时率 > 1% |
| R8 | FS 事件丢失/顺序错乱 | 索引与磁盘不一致 | 1s 去抖合并 + 事件溢出重扫 + 启动兜底重扫 + `--verify --deep` 一致性巡检 | 一致性差异率 > 0.1% |
| R9 | 抽取器 panic/死循环 | 批次卡死 | `spawn_blocking` + 超时强杀 + panic 捕获 + 同类型连续失败熔断 | 单批次耗时异常；同类型超时计数上升 |
| R10 | 正则/模糊滥用导致慢查询 | UI 卡顿 | 默认关闭；候选集限量 5000；预算耗尽即截断并 warning | `warnings:["scan_limit_reached"]` 频发 |
| R11 | 索引目录降级到易失目录 | 重启后索引丢失 | 明确红色提示 + 建议手动指定目录 + 检测 `%TEMP%`/`XDG_CACHE` 时额外提示 | 降级事件出现 |
| R12 | 第三方依赖引入网络能力（供应链） | 破坏隐私承诺 | `cargo deny` + 依赖准入 + `network` feature 隔离 + 二进制字符串扫描 | 依赖审计发现 HTTP/TLS 引用 |
| R13 | 段格式变更导致旧索引不可用 | 升级后用户需重建 | schema 版本 + 迁移脚本 + 无法迁移时明确提示重建；`buildId` 校验避免混跑 | schema_version 变更 |
| R14 | UI 主线程被第三方组件阻塞 | 违反硬性指标 | 虚拟滚动 + Web Worker + 长任务门禁 + 逐 PR 性能回归 | longtask > 50ms 计数 > 0 |
| R15 | 权限不足导致大量目录被跳过（企业环境） | 结果不完整 | 计数 + 样本展示 + 明确文案「部分目录因权限跳过」；不请求提权 | `filesFailed` 占比 > 5% |
| R16 | 单实例/多用户会话冲突 | 双开抢占索引 | 命名互斥量 + 管道名含 sessionId/uid + 第二实例仅激活窗口 | 双进程同时持锁告警 |
| R17 | 段合并占用大量 IO 影响前台 | 卡顿 | 合并仅在空闲 + 节流令牌充足时进行；合并可中断 | 合并期用户输入延迟上升 |

---

## 18. 附录

### 18.1 默认配置项表

| 键 | 类型 | 默认值 | 范围/枚举 | 说明 |
| --- | --- | --- | --- | --- |
| `version` | int | 1 | — | 配置 schema 版本 |
| `index.root` | string | `""`（空=按 §5.5 自动） | — | 自定义索引目录 |
| `index.parallelVolumes` | int | 0（0=auto → min(卷数,2)） | 1–4 | 并行卷数 |
| `index.lowSpecAutoDegrade` | bool | true | — | 低配自动降为 1 |
| `index.memoryBudgetBytes` | int | 536870912 | 134217728–4294967296 | 全局内存预算 |
| `index.throttle` | enum | `low` | `low\|medium\|high` | IO 节流档位 |
| `index.adaptiveThrottle` | bool | true | — | 用户活跃时自动降档 |
| `index.batchFiles` | int | 512 | 64–4096 | 每批文件数 |
| `index.batchTextBytes` | int | 8388608 | 1MiB–64MiB | 每批文本字节 |
| `index.maxTextBytesPerFile` | int | 2097152 | 64KiB–16MiB | 单文件索引文本上限 |
| `index.storePositions` | bool | true | — | 存位置偏移（关=省空间，短语降级） |
| `index.hashPolicy` | enum | `none` | `none\|partial-1mb\|full` | 内容哈希策略 |
| `index.checkpointEveryFiles` | int | 5000 | 500–50000 | checkpoint 文件间隔 |
| `index.checkpointEveryMs` | int | 30000 | 5000–300000 | checkpoint 时间间隔 |
| `index.mergeHotSegments` | int | 8 | 2–64 | 热段数达阈值合并 |
| `index.segmentTargetBytes` | int | 67108864 | 16MiB–256MiB | 冷段目标大小 |
| `index.excludeRules` | string[] | 平台默认（§5.4） | glob/前缀 | 追加排除规则 |
| `index.excludePaths` | string[] | `$Recycle.Bin` / `RECYCLER` / `System Volume Information` | 路径片段 | 路径中包含即跳过（任意盘符，大小写不敏感） |
| `index.excludeTypes` | string[] | `[]` | 扩展名 | 命中即不建立索引 |
| `index.followSymlinks` | bool | false | — | 是否跟随符号链接/重解析点 |
| `index.dailyScan` | bool | false | — | 每日定时增量索引（到点对已完成卷重扫一遍，未变更文件只 stat） |
| `index.dailyScanAt` | string | `03:00` | `HH:MM` | 定时触发时刻（本地时间） |
| `index.resumeOnStart` | bool | true | — | 启动时自动继续未完成的索引（按一级子树断点续跑） |
| `index.includeNetworkShares` | bool | false | — | 是否索引网络驱动器 |
| `index.includeRemovable` | bool | false | — | 是否索引可移动介质 |
| `extract.maxBytesPerFile` | int | 67108864 | 1MiB–1GiB | 单文件读取上限 |
| `extract.timeoutMs` | int | 5000 | 500–60000 | 单文件抽取超时 |
| `extract.onTooLarge` | enum | `truncate` | `truncate\|skip` | 超限行为 |
| `extract.maxDepth` | int | 2 | 1–5 | 容器嵌套深度 |
| `extract.ocrEnabled` | bool | false | — | OCR 插件开关（默认关） |
| `query.timeoutMs` | int | 2000 | 200–30000 | 查询超时预算 |
| `query.firstBatchHintMs` | int | 150 | 50–500 | 首批目标 |
| `query.regexEnabled` | bool | false | — | 正则开关默认值 |
| `query.fuzzyEnabled` | bool | false | — | 模糊开关默认值 |
| `query.allowLeadingWildcard` | bool | false | — | 前导通配符 |
| `query.maxResults` | int | 10000 | 100–100000 | 单次最大结果数 |
| `query.regexScanLimit` | int | 5000 | 100–50000 | 正则/模糊候选上限 |
| `ui.debounceMs` | int | 120 | 30–150 | 输入防抖 |
| `ui.virtualOverscan` | int | 8 | 2–32 | 虚拟滚动 overscan |
| `ui.rowDensity` | enum | `comfortable` | `compact\|comfortable` | 行高 |
| `ui.theme` | enum | `system` | `system\|light\|dark` | 主题 |
| `ui.language` | string | `system` | — | 语言 |
| `ui.trayEnabled` | bool | **false** | — | 托盘常驻（需显式开启并展示说明） |
| `ui.autostart` | bool | false | — | 开机自启 |
| `ui.historyEnabled` | bool | true | — | 搜索历史 |
| `ui.historyMaxEntries` | int | 200 | 0–2000 | 历史上限 |
| `lifecycle.shutdownTimeoutMs` | int | 5000 | 3000–10000 | 关闭超时 |
| `lifecycle.heartbeatIntervalMs` | int | 5000 | 3000–10000 | 心跳周期 |
| `lifecycle.heartbeatMissThreshold` | int | 3 | 2–5 | 失联判定次数 |
| `lifecycle.restartIndexdMaxRetries` | int | 2 | 0–5 | 索引服务重启上限 |
| `privacy.telemetry` | bool | **false**（不可通过 UI 外的配置开启上报；见 §11.4） | — | 遥测 |
| `log.level` | enum | `info` | `error\|warn\|info\|debug\|trace` | 日志级别 |
| `log.fullPaths` | bool | false | — | 日志记录完整路径（含隐私风险提示） |
| `log.queryDebug` | bool | false | — | 记录查询明文（仅本地，提示风险） |

### 18.2 索引状态面板字段

| 字段 | 类型 | 展示 | 来源 |
| --- | --- | --- | --- |
| 挂载点 / 卷名 | string | `C:\` · `NTFS` · `本地固定` | `volume.list` |
| 卷 ID | string | 详情展开显示（可复制） | `volume.list` |
| 状态徽章 | enum | `未索引/扫描中/索引中/已暂停/已停止/重建中/清理中/监控中/降级/不可用` | `event.volumeState` |
| 进度 | float | 进度条 + 百分比（不确定态用条纹） | `event.progress` |
| 文件数（已完成/总数） | int | `421,337 / 1,843,200`（扫描中总数标「≈」） | `event.progress` |
| 目录数（已完成） | int | `38,122` | `event.progress` |
| 已读字节 | int | `913.7 MB` | `event.progress` |
| 吞吐 | `{filesPerSec,bytesPerSec}` | `1,832 文件/s · 40.0 MB/s` | `event.progress` |
| ETA | int/null | `约 13 分钟` / `—` | `event.progress` |
| 失败数 | int | `17`（点击展开最近 5 条样本，路径脱敏） | `event.progress.failedSamples` |
| 最近检查点 | `{id,at,cursorPath}` | `#41 · 10:00:00 · …/docs` | `event.progress.checkpoint` |
| 内存（进程 / 预算） | int | `256 MB / 512 MB` + 压力标记 | `event.progress.memory` |
| IO 节流 | enum | `低（8.0 MB/s）` | `event.progress.io` |
| 索引体积（该卷） | int | `1.8 GB`（倒排/正排/正排DB 拆分） | `stats.get` |
| 段数（冷/热） | `{cold,hot}` | `24 / 3` | `stats.get` |
| 文档数 | int | `1,798,442` | `stats.get` |
| 监控状态 | enum | `监控中 / 已暂停 / 溢出重扫中 / 不可用` | `event.volumeState` + `event.watchOverflow` |
| 索引目录（生效） | string | `%LOCALAPPDATA%\SnapFind\index`（降级时红色标记 + 原因） | `config.get` + `event.indexDegraded` |
| 操作按钮 | — | 暂停/恢复/停止/重建/清理/排除规则 | `volume.action` |

面板刷新约束：进度类字段随 `event.progress`（≤50ms/次，UI 合帧）；统计类字段每 2s 拉取 `stats.get`（异步，不阻塞）。

### 18.3 发布前文档检查清单

| # | 检查项 | 通过标准 |
| --- | --- | --- |
| 1 | 产品名统一 | 全文仅出现 `SnapFind`；无旧名/别名 |
| 2 | 宣传语一致 | 中英文宣传语与 §1.1 逐字一致 |
| 3 | 定位表述 | 含「开源、轻量、隐私优先的本地文件全文搜索桌面软件」与「默认完全本地运行」 |
| 4 | 无对标产品名 | 全文检索无任何具体对标产品名称 |
| 5 | 无 macOS | 全文无 macOS 构建/打包/签名/发布内容；CI 配置无 macOS |
| 6 | 关闭即退出 | 文档明确「关闭主界面 = 关闭索引进程，默认且唯一默认行为」 |
| 7 | 托盘非默认 | 明确托盘常驻需显式开启且附文字说明 |
| 8 | 零门槛 | 明确不扫码/不登录/不付费/无云校验 |
| 9 | OCR 默认关 | 明确 OCR 为可选插件且默认不启用 |
| 10 | 无遥测 | 明确默认无遥测、无网络依赖 |
| 11 | 协议字段表 | `query.execute`、`event.progress` 字段表与 JSON 示例齐备 |
| 12 | 错误码 | §4.9 覆盖至少 10 类必需错误码（鉴权/协议/卷不可用/权限/目录不可写/索引损坏/抽取不支持/加密/超大/语法/超时/内存） |
| 13 | 状态迁移表 | 每卷状态机有「源→事件→目标→副作用」四列表 |
| 14 | 平台差异 | Windows/Linux 分列且覆盖 x64/arm64 |
| 15 | 四目标构建物 | 每目标至少一种可安装/可运行构建物 |
| 16 | 依赖矩阵 | 每个抽取/关键依赖有四目标结论与降级方案 |
| 17 | 索引格式说明 | 目录结构、段格式、校验和、版本迁移齐备 |
| 18 | 隐私说明 | 含数据清单、网络能力声明、删除方式 |
| 19 | 许可证 | Apache-2.0 + `NOTICE`，含选型理由 |
| 20 | 基准报告 | 四类报告含 `platform`/`arch` 标注 |
| 21 | 验收清单 | §15 全部条目有判定结果与签署 |
| 22 | 故障注入报告 | §13.5 全部 16 项有执行记录 |
| 23 | CI 矩阵 | 四目标 job 齐备，无 macOS |
| 24 | 卸载残留 | 记录四目标卸载验证结果（无残留进程/服务） |
| 25 | 索引降级说明 | 记录降级触发方式与提示文案截图 |

---

## 自检结果

1. **（a）是否出现 macOS 构建/发布相关内容**：**无**。全文仅在「非目标 / 平台范围 / CI 约束 / 交付物排除项」中以「不做 macOS、不产出 macOS 构建物、CI 不得出现 macOS 任务」的否定形式声明，未包含任何 macOS 构建、打包、签名、发布步骤或 CI 任务。
2. **（b）是否破坏「关闭主界面即关闭索引进程」或「界面不卡顿」**：**未破坏**。§4.8 将「关闭主界面 = 关闭索引进程」定义为默认且唯一默认行为（含超时强制终止、崩溃心跳自退、不驻留后台），托盘常驻仅为用户显式开启的非默认项；§10.2 以 14 项可测指标约束 UI 主线程零阻塞（无同步 IO、无同步 IPC 等待、无大数组同步计算），并在 §15.5 设为发布阻断项。
3. **（c）是否出现任何具体对标产品名称**：**无**。全文未出现任何具体对标产品名称，问题域统一以「现状痛点」描述。

