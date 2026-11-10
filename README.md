# SnapFind

开源、轻量、本地优先的桌面全文搜索工具。索引、配置、历史全部留在本机，**不联网、不上传、无遥测、无云校验**。

- 双进程：`SnapFind`（界面）/ `snapfind`（命令行） + `snapfind-indexd`（索引守护进程），本机 IPC（Windows 命名管道 / Linux UDS），不监听任何 TCP 端口
- 索引后端：SQLite FTS5（contentless 倒排）+ zstd 压缩正文分段，一个盘符/目录一个独立分片，互不影响
- 界面：.NET 8 + Avalonia 原生绘制（无 Web/HTML 前端、无内嵌浏览器），WinUI/Fluent 风格，深浅主题
- 关闭界面即关闭索引进程（落检查点后退出），不驻留后台

> 当前为 **v0.1 可运行内核**。功能清单见[特性](#特性)，配置生效时机见[配置参考](#配置参考)，已知问题见[已知限制与已知问题](#已知限制与已知问题)，改进路线见 [`docs/IMPROVEMENT_PLAN.md`](docs/IMPROVEMENT_PLAN.md)。设计基线见 [`docs/DEV.md`](docs/DEV.md)。

---

## 特性

| 能力 | 状态 | 说明 |
| --- | --- | --- |
| 双进程架构 | ✅ | 前端独占启动并连接 `snapfind-indexd`；前端退出或崩溃（stdin EOF）索引进程随之退出 |
| 本机 IPC | ✅ | Windows 命名管道 / Linux UDS，**不监听 TCP**；4 字节长度 + JSON 帧 |
| 一次性令牌鉴权 | ✅ | 32 字节 CSPRNG，命令行传入，握手后进程内清零 |
| 协议版本 / 构建一致性 | ✅ | `hello` 协商协议版本；`meta.db` 记录 `build_id`，禁止新旧混跑 |
| 流式查询 | ✅ | 按 `seq`/`final` 分批返回，前端按 `requestId` 丢弃过期结果 |
| 查询语法 | ✅ | 关键词、短语、AND/OR/NOT、`-`/`!`、分组、`*` 前缀、`name:/path:/content:/ext:` |
| 过滤器与排序 | ✅ | 类型、大小区间、修改时间、路径限定；相关度 / 名称 / 大小 / 修改时间升降序 |
| 高亮与片段 | ✅ | 命中上下文窗口，先转义再插 `<mark>`，UTF-16 偏移 |
| 中文分词 | ✅ | bigram 滑窗，索引与查询共用同一分词器 |
| 内容抽取 | ✅ | 纯文本/代码/日志/Markdown/HTML/RTF/PDF、OOXML（DOCX/XLSX/PPTX）、EPUB、OLE2 老式 Office；不可读类型（exe/dll/媒体/压缩包）只索引文件名与路径 |
| 索引目录降级 | ✅ | 首选 `<程序目录>/index`，不可写时降级到用户数据目录并给出原因码 |
| 断点续建 | ✅ | `state.json` 原子写；重启按 mtime+size 跳过未变文件，不重复抽取 |
| 增量索引 | ✅ | `notify` 监控 + 1s 去抖；未变更文件只 stat |
| 卷控制 | ✅ | 暂停 / 继续 / 重建 / 清理并删除 / 移除自定义目录（磁盘与网络卷不支持移除） |
| 资源控制 | ✅ | IO 节流（令牌桶，四档）+ 内存分级降级（L0/L1/L2）+ 分段整理（compact） |
| 桌面端 | ✅ | 搜索 / 索引 / 设置三页、预览面板与标尺、系统托盘、全局热键 `Ctrl+Alt+F`、单实例、开机自启 |
| 无网络 / 无遥测 | ✅ | 默认构建无 HTTP 依赖；索引、配置、日志全部本地 |
| 正则 / 模糊匹配 | ⚠️ | 预留开关，v0.1 执行时降级为普通关键词并给出 warning |
| OCR | ❌ | 未实现（默认关闭，与设计文档一致） |

---

## 组件与二进制命名

包内三个程序，**名字互不相同，Linux 下大小写敏感**：

| 文件 | 角色 | 说明 |
| --- | --- | --- |
| `SnapFind.exe`（Windows：`SnapFind.Desktop.exe`） | 桌面界面 | 双击启动；Linux 上必须大写 `./SnapFind` |
| `snapfind(.exe)` | 命令行工具 | 管理索引、检索、预览；**无界面**，误启动只会打印命令列表 |
| `snapfind-indexd(.exe)` | 索引守护进程 | 由前端自动拉起，不要手工常驻 |

> Linux 包内主程序特意命名为 `SnapFind`（不带 `.Desktop` 后缀）：freedesktop 会把 `*.desktop` 当成启动器配置而不是可执行文件，双击无法运行。

---

## 快速开始

### Windows

1. 解压 `SnapFind-win-x64.zip`（或 `win-arm64`）；
2. 运行 `SnapFind.Desktop.exe`；
3. 首次启动进入范围确认页：勾选要索引的盘符 → 确认索引目录 → 选填排除规则 → 「开始索引」。

### Linux

```bash
tar xzf SnapFind-linux-x64.tar.gz
cd linux-x64
chmod +x SnapFind snapfind snapfind-indexd

./SnapFind        # 图形界面（必须在桌面会话中运行）
./snapfind --help # 命令行
```

图形界面依赖系统 X11 库（多数桌面发行版已自带）：

```bash
# Debian / Ubuntu
sudo apt install libx11-6 libxrandr2 libxi6 libxcursor1 libice6 libsm6 libfontconfig1
# Fedora / RHEL
sudo dnf install libX11 libXrandr libXi libXcursor libICE libSM fontconfig
```

放进应用菜单：包内附 `snapfind.desktop.template`，把 `Exec=` / `Path=` 改成实际解压路径后复制到 `~/.local/share/applications/snapfind.desktop`（图标安装脚本见路线图 `docs/IMPROVEMENT_PLAN.md` R4-3）。

> 纯终端 / SSH 环境没有图形会话，无法启动界面；请用 `snapfind` 命令行。

---

## 使用

### 界面

| 区域 | 说明 |
| --- | --- |
| 搜索页 | 大输入框（回车即搜）+ 类型多选标签（数字为本次命中数，仅在已加载结果内筛选）+ 可展开的路径筛选；结果表首列序号可恢复相关度排序，表头点击即排序（本地重排，不重新搜索） |
| 预览面板 | 默认 460px，可拉伸 360–680px；命中片段高亮 + 全文预览（本机直读，>32MB 才截断）；按宽度自动软换行；右侧 18px 滚动条以灰色滑块/蓝色关键词标记/橙色 Ctrl+F 标记定位；支持 Ctrl+F 查找、打开、定位、复制路径 |
| 索引页 | 范围表列出全部盘符与已添加目录；可勾选「索引所选」、「索引全部固定盘」、「重新扫描磁盘」、添加任意目录；每范围一张进度卡（进度、吞吐、ETA、暂停/继续/重建/清理并删除；自定义目录另有「移除」）；卡片上方统一显示 `IO 限速 <档位>（全部卷共享）· 合计读写 x/秒` |
| 设置页 | 主题、索引计划（每日定时增量 + 触发时刻 + 启动续跑）、索引目录、同时索引的卷数、索引排除（路径片段 / 文件类型 / glob 规则）、关闭到托盘、开机自启、整理分段、关于 |
| 系统托盘 | 双击恢复窗口；右键「显示 / 设置 / 退出」；单击无动作；`Ctrl+Alt+F` 全局唤起；重复启动会唤起已有窗口（`--exit` 可优雅退出） |

### 命令行

> Windows 下为 `snapfind.exe`，Linux 下为 `./snapfind`（小写）。每个命令结束后默认关闭索引进程，`--keep` 可跳过显式 shutdown。

```bash
snapfind scope                                  # 查看范围（卷/文件系统/容量/预计量级/索引目录/排除项）
snapfind index                                  # 索引已勾选的范围
snapfind index --path D:\docs --path D:\src     # 只索引指定目录（登记为独立范围）

snapfind search "季度报表"
snapfind search "hello AND rust"
snapfind search 'name:report ext:pdf'
snapfind search "foo" --ext md --sort mtime --order desc --limit 20

snapfind preview D:\docs\a.md
snapfind preview vol-9dbfd0a1b2c3d4e5:123       # 命中项 docId
snapfind open D:\docs\a.md
snapfind reveal D:\docs\a.md
snapfind copy D:\docs\a.md

snapfind status
snapfind volume pause   --id vol-xxxx
snapfind volume resume  --id vol-xxxx
snapfind volume rebuild --id vol-xxxx
snapfind volume clean   --id vol-xxxx           # 清理并删除该范围索引
snapfind volume unregister --id vol-xxxx        # 移除自定义目录（仅 fs=dir）

snapfind compact                                # 回收更新留下的垃圾分段
snapfind shutdown
```

全局参数：

| 参数 | 说明 |
| --- | --- |
| `--index-dir <path>` | 覆盖索引目录（默认 `<程序目录>/index`，不可写时自动降级） |
| `--keep` | 跳过显式 `shutdown` 握手（索引进程仍随前端进程退出） |

---

## 查询语法

| 写法 | 含义 |
| --- | --- |
| `foo` | 关键词（默认搜文件名 + 路径 + 内容） |
| `"foo bar"` | 短语 |
| `a AND b` / `a && b` / 空格 | 交集 |
| `a OR b` / `a \|\| b` | 并集 |
| `NOT a` / `-a` / `!a` | 差集（需至少一个正项） |
| `(a OR b) AND c` | 分组 |
| `foo*` | 前缀通配（不支持前导通配） |
| `name:x` / `path:x` / `content:x` | 字段限定 |
| `ext:pdf` / `type:pdf,docx` | 类型过滤 |

---

## 配置参考

配置文件为**便携式**：`<程序目录>/config.json`；程序目录不可写（如安装到 Program Files）时回退 `%APPDATA%\SnapFind\config.json` / `$XDG_CONFIG_HOME/snapfind/config.json`。桌面端与索引进程读同一份。

**索引进程只在启动时读一次配置**；下表的「生效时机」是实际行为（与界面是否标注无关）：

| 字段 | 默认 | 生效时机 | 设置页 | 说明 |
| --- | --- | --- | --- | --- |
| `index.root` | 空 = 自动 | **需重启** | ✅（提示重启） | 索引目录；空时按 `<程序目录>/index` → 用户数据目录降级 |
| `index.parallel_volumes` | `0` | **需重启** | ✅ | 同时索引的卷数；`0` = 全部卷同时开始，上限 8 |
| `index.resume_on_start` | `true` | **需重启** | ✅ | 启动时续跑未完成的索引 |
| `index.daily_scan` / `daily_scan_at` | `false` / `03:00` | 热生效（30s 轮询） | ✅ | 每日定点增量扫描；时刻为本地时间 `HH:MM` |
| `index.throttle` | `low` | **需重启索引服务** | ✅ | IO 节流档位：`off` / `low`(80 文件·6MB/s) / `medium`(240·24MB) / `high`(600·96MB)；索引进程级、所有卷共享。设置页「IO 限速档位」（低/中/高/关闭） |
| `index.memory_budget_bytes` | 1 GiB | **需重启索引服务** | ✅ | RSS 预算：≥75% 正文截到 256 KiB，≥100% 只索引文件名/路径。设置页「内存预算（MB）」（256–4096） |
| `index.max_text_bytes_per_file` | 2 MiB | **需重启** | ❌ | 单文件正文上限（L1 降级时进一步压到 256 KiB） |
| `index.exclude_paths` | 回收站等 3 项 | 热生效（每次扫描重读） | ✅ | 路径片段，命中即跳过整棵子树，大小写不敏感 |
| `index.exclude_types` | 空 | 热生效 | ✅ | 扩展名（不含点） |
| `index.exclude_rules` | 空 | 热生效（UI 保存时经 IPC 同步） | ✅（与首次引导共用） | glob 规则，如 `node_modules/**`；内置默认项与索引目录规则扫描时自动叠加，不写入此字段 |
| `index.follow_symlinks` | `false` | 需重启 | ❌ | 是否跟随符号链接 |
| `index.checkpoint_every_files` / `_every_ms` | 5000 / 30000 | 运行时 | ❌ | 检查点落盘阈值（或关系） |
| `extract.max_bytes_per_file` | 64 MiB | 需重启 | ❌ | 抽取读取上限 |
| `extract.timeout_ms` | 5000 | 需重启 | ❌ | 预留，当前抽取无超时中断 |
| `query.batch_size` | 50 | 需重启 | ❌ | 流式响应分批大小 |
| `query.timeout_ms` | 10000 | 即时（桌面端） | ✅ | 查询时间预算：只用于「耗时超预算」提醒，不中止检索。设置页「查询时间预算（秒）」；桌面端超时看门狗 = 预算 × 3 且 ≥ 10 秒，保存后即时同步 |
| `ui.theme` | `system` | 即时（桌面端） | ✅ | `system` / `light` / `dark` |
| `ui.close_to_tray` | `true` | 即时（桌面端） | ✅ | 关窗仅隐藏到托盘（需托盘可用） |
| `ui.tray_enabled` | 跟随 `close_to_tray` | 需重启桌面端（图标） | ✅ | 已并入「关闭窗口时最小化到托盘」开关：勾选时创建托盘图标，取消则关窗即退出；图标在启动时创建，改后需重启应用 |
| `ui.debounce_ms` | 120 | 需重启桌面端 | ❌ | 搜索输入去抖，下限 30ms |
| `ui.autostart` | `false` | 仅镜像 | ✅（仅 Windows） | 真实状态在注册表 `HKCU\...\Run`（值名 `SnapFind`，带 `--minimized`）；Linux 暂未实现自启（开关隐藏），程序不读此字段 |
| `lifecycle.*` | 5s / 5s / 3 | 启动时 | ❌ | 关闭预算、心跳间隔与失联阈值 |

**配置结构 v2（2026-11）**：已移除 11 个「从未被任何代码读取」的字段——`index.batch_files`、`index.store_positions`、`extract.on_too_large`、`extract.ocr_enabled`、`query.first_batch_hint_ms`、`query.regex_enabled`、`query.fuzzy_enabled`、`query.allow_leading_wildcard`、`query.max_results`、`ui.history_enabled`、`ui.history_max_entries`。旧 `config.json` 里的这些键会被安全忽略，下次保存时自动写成 `version: 2`。

**写入方唯一**：`config.json` 只由桌面端/CLI 前端写入；索引进程只读（`setExcludes` 仅同步内存）。因此手改配置需在程序退出后进行，运行期间手改会被桌面端的下一次保存覆盖。

### 索引体积与资源控制（实测）

语料：2000 个文本文件，正文合计 2,899,993 B（Windows，debug 构建，`throttle=off`）。

| 指标 | 实测 |
| --- | --- |
| 段内正文（zstd 压缩后） | 1,020,078 B（约为原始正文的 **35.2%**） |
| `docs.db`（元数据 + 倒排） | 1,564,672 B |
| 每文档平均占用 | ≈ 1.3 KB（段内 510 B + 元数据/倒排 782 B） |
| 外推 100 万文档 | ≈ 1.3 GB（同等内容分布下） |

正文不进 SQLite（无 `docs.content` / `*_tok` 列），FTS 为 **contentless**（`content=''`、`contentless_delete=1`），同一份文本不再存 3–4 份。旧 schema 打开时自动重建为 v2 并触发重扫。

---

## 数据目录与隐私

```
<索引目录>/
├── meta.db          # 卷登记表、schema/build_id/tokenizer_id、事件
├── lock             # 进程独占锁（单实例）
├── tmp/
└── vol-<volumeId>/  # 每个盘符/目录一个分片
    ├── state.json   # 状态机 + 检查点（原子替换写）
    ├── docs.db      # 文档表 + FTS5 倒排（只存倒排）
    ├── hot/         # 增量段（预留）
    └── segments/    # zstd 压缩正文：seg-00001.seg …（单段 96MB 轮转）
```

- **一 一对应**：卷 ID 由卷 GUID 派生，盘符改字母仍指向同一分片；「重建」只清该卷 `segments/` 与三张表，「清理并删除」只删该卷目录。
- 全部数据在本机；无网络请求、无遥测、无账号。

---

## 架构速览

```
SnapFind（Avalonia）/ snapfind（CLI）        snapfind-indexd
┌────────────────────────────┐   IPC 单连接  ┌──────────────────────────────┐
│ 界面 / 命令解析             │◄────────────►│ ipc-server（鉴权/版本协商）    │
│ ipc-client（异步流式）      │ 命名管道/UDS │ control-plane（卷状态机/调度） │
│ 预览直读（不经索引进程）    │              │ write-pipeline / query-engine │
└────────────────────────────┘              │ IndexBackend = SQLite FTS5    │
                                            └──────────────────────────────┘
```

- 前端与索引进程共用 `snapfind-ipc-client`（拉起、握手、心跳、流式、优雅关闭）。
- 索引为「每个卷一个线程 + 一个独立分片」；同时开工的卷数由 `index.parallel_volumes` 限制，吞吐最终由全局 IO 节流与内存预算兜底。

---

## 构建

```bash
# Rust 引擎 / 索引进程 / CLI
cargo build
cargo test

# 桌面端（需 .NET 8 SDK）
dotnet build SnapFind.Desktop/SnapFind.Desktop.csproj
dotnet run   --project SnapFind.Desktop

# 四平台发布包（Windows x64/ARM64、Linux x64/ARM64，无 macOS）
./build-release.ps1              # 产物在 dist/
./build-release.ps1 -Only win-x64
```

跨平台产物说明：Linux 主程序在发布时以 `-p:AssemblyName=SnapFind` 生成（apphost 按文件名找同名 dll，不能事后改名），因此包内文件为 `SnapFind` + `SnapFind.dll`。

测试现状（诚实标注）：`snapfind-engine` 有 3 个集成测试；`snapfind-indexd`、`snapfind-ipc-client`、`snapfind-cli` **尚无测试**；桌面端**无测试工程**。补齐计划见路线图 R4-2。

---

## 已知限制与已知问题

| 项 | 现状 |
| --- | --- |
| 多数配置需重启 | `root`、`parallel_volumes`、`throttle`、`memory_budget_bytes`、`extract.*`、`query.batch_size`、`lifecycle.*` 只在索引进程启动时读取；设置页已提供「重启索引服务」按钮（保存检查点后重启索引进程，无需关闭应用） |
| 索引目录切换 | 保存前探测可写性，旧目录**不迁移、不自动清理**（保存后会提示旧目录位置）；切换后需重建索引 |
| 查询超时 | `query.timeout_ms` 只影响「耗时超预算」提醒，不中止检索；桌面端看门狗 = 预算 × 3 且 ≥ 10 秒，预算在设置页可改并即时生效 |
| 正则 / 模糊 | 未实现，降级为关键词并提示 |
| OCR | 未实现 |
| 发布包 | Windows 未签名（Authenticode 需证书，暂缓）；已提供 `SHA256SUMS.txt` 校验和、Linux `install.sh` 与 256×256 图标（R4-3） |

---

## 故障排查

**双击没反应 / 打开的是文本编辑器（Linux）**
主程序文件名必须以大写 `SnapFind` 结尾且**没有** `.Desktop` 后缀；确认可执行权限 `chmod +x SnapFind`。

**`./SnapFind` 只弹出命令列表**
你运行的是小写 `./snapfind`（CLI）。图形界面是 `./SnapFind`。

**界面起不来，终端报 `Unable to open display` / 缺 `libX11`**
说明不在图形会话（SSH/纯终端）或缺 X11 库，按上文安装依赖后在桌面会话中运行。

**索引页显示「已降级」**
`<程序目录>/index` 不可写（常见于安装到 Program Files），已自动落到用户数据目录；索引页底部会显示真实路径。

**搜索不到刚改过的文件**
增量监控有 1s 去抖；若该范围被暂停/停止，需「继续」或「重建」。被排除规则命中的路径不会入库。

**索引目录或设置改了没反应**
对照[配置参考](#配置参考)的「生效时机」；需要重启的项请退出程序（托盘「退出」）后重开。

**进度卡一直「扫描中」且百分比不动**
`files_total` 在扫描期是滚动累加值，百分比依赖根目录一级子树进度；拿不到子树信息时使用不确定进度条，属正常表现。

---

## 路线图

按优先级推进的任务清单、验收标准与工作量估算见 [`docs/IMPROVEMENT_PLAN.md`](docs/IMPROVEMENT_PLAN.md)：

1. **P0** 配置写入唯一化、`RestoreDefaultExcludes` 补同步、死配置清理与迁移、关键静默失败补提示；
2. **P1** 设置页补齐生效时机标注与「重启索引服务」、索引目录切换善后、三处超时统一、把节流/内存预算/去抖暴露到界面；
3. **P2** 拆 `store` 全局锁、排除集缓存、进度与 stats 竞争修复；
4. **P3/P4** 日志与崩溃上报、Linux 图标、国际化底座、版本号统一、测试补齐、发布工程（签名 / SHA256 / Linux 安装脚本）。

> 2026-11-10：P0～P4 除「拆锁」（前置并发压测已补齐，等 store 重构）、「国际化底座」、部分无障碍（Avalonia 无 Button 级 AccessKey）与「Authenticode 签名」外均已落地；版本统一为 **0.2.0**；测试：桌面端 xUnit 23 例（`dotnet test`）+ indexd IPC 冒烟链路 + 扫描/查询并发压测（`cargo test`）。日志统一落 `<index>/logs/`：`indexd.log`（indexd stderr，10MB 轮转）、`panic.log`（崩溃）。

---

## 许可

Apache-2.0（理由见 `docs/DEV.md` §16.6）。
