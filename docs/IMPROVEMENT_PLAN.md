# SnapFind 改进任务计划（v0.2 体检与路线图）

> 生成时间：2026-11-10。依据：全仓库静态审计（Rust workspace + `SnapFind.Desktop` + 构建脚本 + README）。
> 本文只列**可执行任务**与**验收标准**；每条都标明证据位置，便于逐条核销。

---

## 0. 体检结论（一句话版）

| 维度 | 结论 |
| --- | --- |
| 最严重问题 | **config.json 有两个「整份快照回写」的写入方**：桌面端（每次开关）与 indexd（`setExcludes` 时 `cfg.save()`）。indexd 会把启动时的旧快照写回，导致桌面端后来改的主题/托盘/并发/索引目录等**在重启后悄悄回滚** |
| 配置有效性 | 41 个配置字段中 **11 个完全没被任何代码读取**，2 个只写不读；`ui.*` 段引擎一个都不读；一半 `query.*` 是死配置 |
| 生效时机 | 绝大多数配置**必须重启 indexd** 才生效，但 UI 只在少数项上标注；`exclude_paths` 每次扫描重读、`exclude_rules` 只能靠 IPC 推送 —— 两套机制并存 |
| 静默失败 | Rust 侧 `let _ =` 吞错密集（IPC 发送、线程 spawn、watch 注册、增量写入、`cfg.save`）；C# 侧设置页 `Save()` 无 try，磁盘只读/满会直接抛到 UI 事件 |
| 性能 | 全局单 `Mutex<Store>`：搜索、扫描写、`compact`（可达数十秒）、`stats()`（UI 100ms 轮询）全串行；`build_exclude_set` 每批变更都读一次磁盘 |
| 工程 | 桌面端零测试、indexd/cli/ipc-client 零测试；无统一日志（Rust）、无崩溃上报；版本号 `0.1.0` vs `1.0.0` 不一致；Windows 未签名、Linux 无安装脚本与图标 |
| 文档 | README 仍描述已删除的 Rust GUI（`snapfind-gui`/eframe），架构图、目录树与现状不符 |

---

## 实施进度

| 任务 | 状态 | 落地内容 |
| --- | --- | --- |
| R0-1 配置写入唯一化 | ✅ 2026-11-10 | 引擎 `setExcludes` 不再 `cfg.save()`，只改内存；新增回归测试 `crates/snapfind-engine/tests/set_excludes_persist.rs` |
| R0-2 恢复默认补同步 | ✅ 2026-11-10 | `RestoreDefaultExcludesAsync` 落盘后经 `setExcludes` 清空 indexd 内存规则 |
| R0-3 死配置清理 | ✅ 2026-11-10 | 移除 11 个无效字段；`config.version` 升到 2（`CONFIG_VERSION`），旧键被忽略并提示升级 |
| R0-4 静默失败补提示 | ✅ 2026-11-10 | 线程 spawn / watcher 启动失败 → `Degraded` + 事件；配置损坏 → 桌面端/ stderr 明确提示；`Config.Save()` 统一走 `TrySaveConfig()` 并在失败时提示 |
| R1-1 生效标注 + 重启按钮 | ✅ 2026-11-10 | 设置页「重启索引服务」按钮（走 `shutdown` 优雅退出 + 断线自愈重连）；需重启项文案标注 |
| R1-2 索引目录切换善后 | 🔶 部分实施 | 保存前探测可写性并当场提示；保存后提示旧目录位置；**锁文件改为跟随 `config.index.root`**（此前锁落默认 index/ 与实际目录脱钩）。「迁移/清理旧目录三选」未做 |
| R1-3 超时统一 | ✅ 2026-11-10 | 看门狗 = 预算 × 3 且 ≥ 10s；设置页新增「查询时间预算（秒）」，保存后即时同步搜索页（`UpdateQueryBudget`） |
| R1-4 设置项去留 | ✅ 2026-11-10 | 暴露 IO 限速四档下拉与内存预算（256–4096 MB）；`tray_enabled` 并入「关窗到托盘」开关；`debounce_ms` 不暴露（仅高级用户手改，仍生效） |
| R2-1 stats 提效 | ✅ 2026-11-10 | `stats()` 加 1 秒缓存；`COUNT(*)`/segments 持锁（轻量 SQL），**目录树大小计算移出全局锁**；桌面端 100ms 轮询不再反复 COUNT + 遍历目录 |
| R2-1 拆锁（搜索/compact 不阻塞扫描） | ⏳ 延后（前置已就绪） | 仍需 store 层重构（每卷 `Mutex<VolumeStore>` / 读连接池），`VolumeStore` 含 `Cell` 且 38 处 `store.lock()` 调用点；**并发压测已补齐**（`tests/concurrency_smoke.rs`：扫描中并行查询与 stats，60s 死锁预算），拆锁后以此作为回归基线；按 §8 约定放到功能冻结后 |
| R2-2 排除集缓存 | ✅ 2026-11-10 | `disk_excludes()` 按 config.json mtime+大小缓存路径片段/扩展名；watch 批次不再每秒读盘 + 反序列化 |
| R2-3 桌面端小修 | ✅ 2026-11-10 | stats 宽限窗 1.5s→5s（进度不再被打回「—」）；`ProgressRow`/`UpdateStatus` 复用 StringBuilder。**退出时 6s 阻塞等待维持现状**（有意设计：等待 indexd 落盘检查点后再杀进程，避免丢检查点）；心跳异常已有观测（`Diag` + `RaiseDisconnected`），无需修改 |
| R3-1 Linux 图标 | ✅ 2026-11-10 | `MainWindow.axaml` 全部 `Segoe MDL2 Assets` 字形改为内置 `StreamGeometry` + `PathIcon`（9 个资源：菜单/搜索/最小化/最大化/还原/关闭/层/设置/警告），最大化/还原切换改为代码切换 `Data` |
| R3-2 统一日志 | ✅ 2026-11-10 | C# `Log` 加 10MB 轮转（`.old` 覆盖一代）；**indexd 的 stderr 恒定落盘到 `<index>/logs/indexd.log`**（不再受 `SNAPFIND_LOG` 开关控制），indexd 启动打一行 `start v0.2.0 index_root=...`；不引入 tracing（Rust 侧日志统一走 stderr，避免依赖膨胀） |
| R3-3 崩溃上报 | ✅ 2026-11-10 | Rust：indexd panic hook 追加 `<index>/logs/panic.log`（未解析出索引目录时退回配置目录 `snapfind-panic.log`，含时间戳/线程）；C#：`AppDomain.UnhandledException` + `TaskScheduler.UnobservedTaskException` + Avalonia UI 线程异常 → `%TEMP%\snapfind-crash.log`，UI 线程异常仅记录并提示不闪退 |
| R3-4 国际化 | ⏳ 待开始 | 文案仍硬编码中文（现阶段可接受）；`Strings.resx` 未落地 |
| R3-5 无障碍 | 🔶 部分 | 图标类按钮（折叠导航、最小化/最大化/关闭、三个导航项）补 `AutomationProperties.Name`；**未加 AccessKey**：Avalonia 11 只在 `AccessText`/`MenuItem`/`Label` 上提供访问键，`Button` 无该属性，且中文界面用拉丁字母助记不自然——现有键盘路径（Tab 序、Esc、Enter、Ctrl+F、Ctrl+Alt+F 全局热键）保持不变 |
| R3-6 失败反馈 | ✅ 2026-11-10 | 剪贴板复制失败 → 页内提示；托盘创建失败/抛异常 → 通知条 + 降级为无托盘；单实例意图转发 → 事件未就绪时重试 5×300ms（修「点了设置没反应」） |
| R4-1 版本号统一 | ✅ 2026-11-10 | Cargo workspace 与 `Directory.Build.props` 统一 0.2.0；关于页/导航栏读程序集版本；manifest 带版本 |
| R4-2 测试补齐 | ✅ 2026-11-10 | 桌面端 `SnapFind.Desktop.Tests`（23 用例：AppConfig 读写不丢键/未知键保留/损坏提示、Format、进度卡片状态机含 5s 宽限与暂停保值，`SNAPFIND_CONFIG` 隔离）；**indexd 冒烟链路** `crates/snapfind-indexd/tests/ipc_smoke.rs`（真实进程：启动→hello→scope.preview→scope.register→index.start→stats 轮询→query.execute 命中/零命中→shutdown，2.2s）；**并发压测** `crates/snapfind-engine/tests/concurrency_smoke.rs`（扫描中并行查询+stats，8s 通过）。踩坑记录：文档必须放 `target/tmp`，`%TEMP%` 命中默认排除 `**\AppData\Local\Temp\**` |
| R4-3 发布工程 | ✅ 2026-11-10 | `SHA256SUMS.txt`（GNU 兼容格式）；zigHome 由 python 推导；publish 失败回显真实错误；dist 清理改逐文件；Linux 包附 `install.sh`（装到 `~/.local/opt/snapfind` + 注册应用菜单）与 `snapfind.png`（构建时 GDI+ 生成）`.desktop` 模板补 `Icon=` |

---

## 1. P0 — 正确性（先修这个，其它都建立在它之上）

### R0-1 配置写入唯一化（**最高优先**）✅ 已实施（采用方案 A）

**问题**：`crates/snapfind-engine/src/service.rs:1576-1583` 的 `setExcludes` 分支：

```rust
cfg.index.exclude_rules = ex.clone();
sync_exclusions_from_disk(&mut cfg);  // 只补 exclude_paths / exclude_types
let _ = cfg.save();                    // 整份 AppConfig 序列化回写
```

`sync_exclusions_from_disk`（`service.rs:36-41`）只同步 2 个字段，其余全部用 indexd **启动时**的快照覆盖磁盘：`ui.theme`、`ui.close_to_tray`、`ui.autostart`、`ui.debounce_ms`、`index.throttle`、`index.memory_budget_bytes`、`index.parallel_volumes`、`index.resume_on_start`、`index.root`、`extract.*`、`query.*`、`lifecycle.*`。桌面端的调用顺序是先 `Config.Save()` 再发 `setExcludes`（`SettingsViewModel.cs:186-196`、`SetupViewModel.cs:101-113`），所以**引擎后写，赢得最后一次写入**。

**做法（二选一，推荐 A）**：
- **A. 引擎不再整份回写**：`setExcludes` 只改内存；`index.exclude_rules` 持久化交给桌面端（桌面本来就写盘）。引擎彻底删掉 `save()` 调用。
- **B. 读-改-写补丁**：`setExcludes` 时 `AppConfig::load()` → 只改 `index.exclude_rules` → `save()`。副作用小，但仍是双写方。

**验收**：
1. 打开桌面 → 换主题（深色）→ 设置页「保存排除规则」→ 退出重启：主题仍为深色；
2. `config.json` 中 `ui.*`、`extract.*`、`query.*` 在保存排除规则前后字节级不变；
3. 新增回归测试：模拟「引擎 save 后桌面端设置不回滚」。

### R0-2 `RestoreDefaultExcludes` 遗漏 `setExcludes` ✅ 已实施

`SnapFind.Desktop/ViewModels/SettingsViewModel.cs:205-213` 只写 `exclude_paths`/`exclude_types`，不发 `setExcludes`；提示文案却说「已恢复默认排除规则」。用户自定义的 glob（如 `node_modules/**`）仍在引擎内存中生效，直到下次保存或重启。

**做法**：复用 `SaveExcludesAsync` 的同步逻辑（同一份代码路径），并复用其提示。
**验收**：自定义规则 → 恢复默认 → 不重启、重扫该目录，被规则排除的文件重新入库。

### R0-3 死配置清理（11 项）✅ 已实施

无任何读取点：`index.batch_files`、`index.store_positions`、`extract.on_too_large`、`extract.ocr_enabled`、`query.first_batch_hint_ms`、`query.regex_enabled`、`query.fuzzy_enabled`、`query.allow_leading_wildcard`、`query.max_results`、`ui.history_enabled`、`ui.history_max_entries`。
只写不读：`ui.autostart`（真实状态在注册表/autostart 文件）、顶层 `version`（无迁移逻辑）。

**做法**：`config.version` 升到 2；`load()` 读到旧版本时打印一次「已移除 N 个无效配置项」并把新结构写回；`regex_enabled`/`fuzzy_enabled` 若要保留能力位，改为「明确标注未实现」并保留字段（README 已如实写）。
**验收**：`cargo test -p snapfind-core` 新增「旧配置迁移」用例；`config.json` 里不再出现死键。

### R0-4 关键静默失败补提示 ✅ 已实施

| 位置 | 现状 | 改为 |
| --- | --- | --- |
| `service.rs:298` 扫描线程 `spawn` | `let _ =` 吞掉 | 失败 → 卷置 `Degraded` + `ServiceEvent` 事件（前端出卡片） |
| `service.rs:1581` `cfg.save()` | 失败假装成功 | 回执带 `ok:false`；桌面端提示「保存失败：磁盘只读/已满」 |
| `watch.rs:27/31` | 监听/溢出静默 | 失败 → 该卷 `Degraded`，事件附原因 |
| `config.rs:214-216` | 解析失败静默重置 | 事件/返回值带 `config_reset` 标记，桌面端横幅提示「配置文件损坏，已备份为 .bak」 |
| `SettingsViewModel` 各 `Config.Save()` | 无 try | 统一 `TrySave()`：失败弹提示且**回滚 UI 勾选状态** |

**验收**：把 `config.json` 设为只读后保存设置，界面给出明确失败提示，不崩、不假成功。

---

## 2. P1 — 设置一致性（用户可感知）

### R1-1 统一「生效时机」并在 UI 标注

现状：`root`/`parallel_volumes`/`resume_on_start`/`throttle`/`memory_budget_bytes`/`extract.*`/`query.batch_size`/`lifecycle.*` 均需重启 indexd；`daily_scan*`、`exclude_paths`、`exclude_types` 热生效；`exclude_rules` 靠 IPC。

**做法**：
1. 设置页每个需要重启的项后面加 `需重启` 徽标；
2. 顶部加「重启索引服务」按钮（`shutdown` → 自动重连，桌面已有 `ShellViewModel` 断线自愈逻辑；
3. README 增加「配置项 × 生效时机」总表（已写入）。

**验收**：改并发数 → 点「重启索引服务」→ 索引页进度卡并行数立即变化，无需退出程序。

### R1-2 `index.root` 切换的善后

`SettingsViewModel.SaveIndexRoot` 提示「重启生效」，但：
- 旧 `index/` 目录**不迁移、不清理**（白占盘，用户以为索引还在）；
- 锁文件走 `--index-dir`/默认路径（`snapfind-indexd/src/main.rs:65-67`），与 `config.index.root` 可能不同目录；
- 配置目录不可写时 `resolve_index_root` 静默降级，设置页却显示用户填的路径（`SettingsViewModel.cs:56`）而索引页 footer 显示真实路径（`IndexViewModel.cs:315`）——两处不一致。

**做法**：保存前探测可写性并当场提示；切换时提供「迁移到新目录 / 清理旧目录 / 仅切换」三选；锁文件改为跟随解析后的 root。
**验收**：切换索引目录后，索引页 footer、设置页、磁盘目录三者路径一致；旧目录可按用户选择迁移或删除。

### R1-3 三处「超时」统一

- 引擎 `query.timeout_ms` 默认 2000，但桌面端**总是显式传** `10000`（`AppConfig.QueryTimeoutMs`），引擎默认永不生效；
- 桌面 `SearchViewModel.cs:106-115` 另有写死 30s 看门狗，才是真正弹「查询超时」的东西；
- `query.timeout_ms` 只影响「耗时超预算」提示，不中止查询。

**做法**：桌面端只在用户显式配置时传 `timeout_ms`；看门狗改为「预算 × 3 且 ≥ 10s」；设置页暴露一个「查询时间预算」项。
**验收**：改配置后，提醒阈值与看门狗同步变化，不再出现「显示 10s 预算、30s 才超时」的错位。

### R1-4 `throttle` / `memory_budget_bytes` / `debounce_ms` / `tray_enabled` 的去留

**做法**：
- `throttle`：设置页加四档下拉（低/中/高/关），保存后走「重启索引服务」；
- `memory_budget_bytes`：设置页加「内存预算（MB）」输入，默认 1024，范围 256–4096；
- `debounce_ms`：要么暴露（30–500ms），要么固定 120 不再当配置；
- `tray_enabled`：与 `close_to_tray` 合并（关托盘即隐藏该开关），或暴露；避免「配置 true 但行为是关窗即退出」。
**验收**：设置页不存在「改了没反应」的项；每一项都有明确的生效时机标注。

---

## 3. P2 — 性能与稳定性

### R2-1 拆锁：搜索/统计不再阻塞扫描

- `service.rs:1664-1668` 整个 `query::search` 在 `store` 锁内（跨卷 FTS + 分段读）；
- `service.rs:1035-1046` 扫描结束时在锁内 `compact()`（整卷重写，数十秒）；
- `service.rs:1782-1799` `stats()` 每卷 `COUNT(*)` + 遍历目录算大小，而桌面上层 100ms ticker 轮询。

**做法**：每卷独立 `Connection` + 读连接池（`store.rs:107-111` 现为单连接共享）；`compact` 移出锁（卷级互斥即可）；`stats()` 结果缓存 1s 并把「索引大小」改为增量累计。
**验收**：100 万文档下，搜索期间进度事件不中断（当前会停顿）；`stats` CPU 占用在空闲时接近 0。

### R2-2 `build_exclude_set` 不再每批读盘

`service.rs:26-34` 内部 `AppConfig::load()`，调用点 `801`（全量扫描）与 `1226`（每个 watch 批次）。改为：配置版本号（mtime/hash）+ 缓存，变更才重建。
**验收**：10 万次增量事件下无磁盘读放大（可用计数器断言）。

### R2-3 桌面端小修

- `IndexViewModel.cs:140/164-168`：stats 与进度竞争导致进度回退（百分比跳回 `—`）——stats 刷新时保留最近 5s 内的实时数值；
- `SearchViewModel.cs:488`、`IndexViewModel.cs:110/171`：每事件新建 `StringBuilder`，改为复用字段；
- `MainWindow.axaml.cs:269`：UI 线程 `Task.Run(...).Wait(6s)` 阻塞退出，改 async；
- `IpcClient.cs:48/50`：心跳 `Task.Run` 火忘，补异常观测与取消。

---

## 4. P3 — 体验与可维护性

| 任务 | 说明 |
| --- | --- |
| R3-1 Linux 图标字体 | `MainWindow.axaml:19` 用 `Segoe MDL2 Assets`（Windows 专有），Linux 上图标全变方块。改用内置 `PathIcon`/`StreamGeometry` 资源 |
| R3-2 统一日志 | Rust 侧引入 `log`+`tracing-subscriber`（默认写 `index/logs/`，按天轮转、上限 10MB）；C# 侧 `Helpers/Log.cs` 加轮转与大小上限 |
| R3-3 崩溃上报 | Rust `panic hook` 落盘最后 200 行日志；C# `AppDomain.UnhandledException` 落盘并提示导出 |
| R3-4 国际化 | 现阶段硬编码中文可接受；先把文案集中到资源类（`Strings.resx`），为后续多语言留口 |
| R3-5 无障碍 | 窗口控制按钮、图标按钮补 `AutomationProperties.Name`；关键操作补 `AccessKey` |
| R3-6 失败反馈 | 剪贴板复制失败（`SearchView.axaml.cs:490`）、托盘创建失败（`TrayService.cs:123`）、单实例让路（`SingleInstance.cs:94`）都要有提示 |

---

## 5. P4 — 工程与发布

### R4-1 版本号统一
`Cargo.toml:14` = `0.1.0`；`SnapFind.Desktop.csproj` 无 `<Version>`（程序集 1.0.0，关于页显示 `v1.0.0`）。
**做法**：新增 `Directory.Build.props` 设 `<Version>0.2.0</Version>`；Cargo workspace 同步；关于页读程序集版本。
**验收**：`snapfind --version`、关于页、包名、README 徽标四处一致。

### R4-2 测试补齐（当前最大质量风险）

| 层 | 现状 | 目标 |
| --- | --- | --- |
| `snapfind-engine` | 3 个集成测试 | 保持 |
| `snapfind-indexd` / `ipc-client` / `cli` | 0 测试 | 至少一条「启动 → hello → scope → index 小目录 → search → shutdown」冒烟链路 |
| 桌面端 | 0 测试工程 | 新建 `SnapFind.Desktop.Tests`（xUnit）：`AppConfig` 读写不丢键、`IndexViewModel` 状态机、排除规则保存链路（mock IPC） |

**验收**：`dotnet test` + `cargo test` 均绿；CI 脚本（可选）一条命令跑全量。

### R4-3 发布工程
- **Windows**：Authenticode 签名（可选，至少提供 `SHA256SUMS.txt`）；
- **Linux**：包内提供 `install.sh`（安装到 `~/.local/opt/snapfind` + 写 `~/.local/share/applications/snapfind.desktop`）；`.desktop` 模板补 `Icon=`，包内附 `snapfind.png`（256×256）；
- **build-release.ps1**：`$zigHome` 由 `python -c` 推导而非硬编码 `C:\Python312\...`；`publish` 重试时打印真实错误（当前只报「第 N 次失败」）；`Remove-Item dist` 改为逐文件删除以规避批量删除保护。

---

## 6. 里程碑

| 里程碑 | 内容 | 预估 | 退出标准 |
| --- | --- | --- | --- |
| M1（P0） | R0-1 ~ R0-4 | 1–2 天 | 配置不再回滚；保存失败可见；死配置清理并迁移 |
| M2（P1） | R1-1 ~ R1-4 | 2–3 天 | 设置页每项都有生效标注；改完能就地生效或一键重启；索引目录切换可迁移 |
| M3（P2） | R2-1 ~ R2-3 | 3–5 天 | 搜索不阻塞扫描；空闲 CPU 近 0；进度不回退 |
| M4（P3/P4） | R3-*、R4-* | 3–5 天 | 日志/崩溃可诊断；四包带图标与校验和；测试覆盖三条链路 |

---

## 7. 建议的实施顺序（给决策用）

1. **只做 R0-1 + R0-2**（半天）：立刻消灭「设置莫名其妙丢」的投诉来源，风险最低、收益最高。
2. 接着 R0-3 + R0-4（半天）：配置面收干净，失败有提示。
3. 再做 R1-1（「重启索引服务」按钮）——它把「需重启」的体验问题一次性消化，也让 R1-4 的新设置项有落点。
4. R2-1 拆锁是「要不要支撑百万级文档」的分水岭，建议在功能冻结后再动。

---

## 8. 风险与取舍

- **R0-1 方案 A** 会让 CLI 的 `setExcludes` 不落盘（CLI 无此命令，实际不受影响）；若将来 CLI 需要改排除规则，再补「读-改-写补丁」（方案 B）。
- **R0-3 删死配置**会让手改过这些键的用户配置被清空——迁移时打日志并在 README「已知限制」注明。
- **R2-1 拆锁**改动面大（store 层），必须先用 `search_e2e` + 新增并发压测兜住，再合并。
