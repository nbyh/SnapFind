//! 服务层：卷状态机、调度、进度事件、查询流式输出、关闭编排。
//!
//! 对应文档 §5.2（状态机）与 §4.8（关闭时序）。所有阻塞工作跑在独立线程，
//! IPC 层只做收发，保证索引高负载时查询与事件不被拖住。

use crate::governor::{FLAG_METADATA_ONLY, Governor};
use crate::store::{Store, VolumeState};
use crate::watch::{self, WatchHandle};
use crate::{query, scan};
use snapfind_core::config::{resolve_index_root, AppConfig, IndexRoot};
use snapfind_core::error::{ErrorCode, SnapError, SnapResult};
use rusqlite::OptionalExtension;
use snapfind_core::ipc::{
    CheckpointInfo, IoInfo, MemoryInfo, PreviewMeta, Progress, QueryChunk, QueryRequest,
    ScopePreview, ScopeVolume, Stats, Throughput, VolumeAction, VolumeStateEvent, VolumeStats,
};
use snapfind_core::volume::{self, VolumeInfo};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 构造排除集：glob 规则取运行时配置（含 `index.start` / `setExcludes` 写入的规则），
/// 路径片段与扩展名从磁盘读（带缓存），保证桌面端设置页改完后，下一次扫描/增量立即生效。
fn build_exclude_set(cfg: &AppConfig, extra_globs: &[String]) -> scan::ExcludeSet {
    let mut patterns = snapfind_core::platform::default_excludes();
    patterns.extend(cfg.index.exclude_rules.iter().cloned());
    patterns.extend(extra_globs.iter().cloned());
    let (paths, types) = disk_excludes();
    scan::ExcludeSet::new(&patterns)
        .with_path_fragments(&paths)
        .with_extensions(&types)
}

/// 磁盘排除配置（路径片段 + 扩展名）的缓存。
///
/// `build_exclude_set` 在每次扫描开始与**每个 watch 批次**都会调用；以前每次都
/// `AppConfig::load()` 整份读盘 + 反序列化，10 万次增量事件就是 10 万次文件读
/// （R2-2）。这里按 config.json 的 mtime + 大小做缓存，文件没变直接复用。
static DISK_EXCLUDES_CACHE: Mutex<Option<(Option<std::time::SystemTime>, u64, Vec<String>, Vec<String>)>> =
    Mutex::new(None);

fn disk_excludes() -> (Vec<String>, Vec<String>) {
    let path = AppConfig::path();
    let (mtime, len) = match std::fs::metadata(&path) {
        Ok(m) => (m.modified().ok(), m.len()),
        Err(_) => (None, 0),
    };
    if let Ok(cache) = DISK_EXCLUDES_CACHE.lock() {
        if let Some((cm, cl, paths, types)) = cache.as_ref() {
            if *cm == mtime && *cl == len {
                return (paths.clone(), types.clone());
            }
        }
    }
    let cfg = AppConfig::load();
    if let Ok(mut cache) = DISK_EXCLUDES_CACHE.lock() {
        *cache = Some((
            mtime,
            len,
            cfg.index.exclude_paths.clone(),
            cfg.index.exclude_types.clone(),
        ));
    }
    (cfg.index.exclude_paths, cfg.index.exclude_types)
}

#[derive(Debug, Clone)]
pub enum ServiceEvent {
    Progress(Progress),
    VolumeState(VolumeStateEvent),
    WatchOverflow { volume_id: String },
    Degraded { reason: String },
}

pub type EventSink = Arc<dyn Fn(ServiceEvent) + Send + Sync>;

#[derive(Debug, Clone, Default)]
struct VolumeControl {
    pause: bool,
    stop: bool,
    /// 代际编号：每次重新建立控制条目（start / resume / rebuild / clean）自增。
    /// 旧的扫描线程与 watcher 发现代际不匹配即退出，且不再写状态，
    /// 避免「重建后旧线程继续扫、把状态改回 Indexing」这类竞态。
    epoch: u64,
}

/// 控制条目的代际序号（进程内单调递增）。
static CONTROL_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_epoch() -> u64 {
    CONTROL_EPOCH.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

/// 扫描线程的实时计数。
///
/// 进度事件此前直接读 store 里的 VolumeState，而那只是检查点快照
/// （每 5000 文件 / 30 秒才写一次）——于是卡片长期显示「0 文件/秒」、
/// 计数长时间不动。这里由扫描线程每处理一个文件就刷新，ticker 优先读它。
#[derive(Default)]
pub struct LiveCounters {
    pub files_done: AtomicU64,
    pub files_total: AtomicU64,
    pub bytes_done: AtomicU64,
    pub dirs_done: AtomicU64,
    pub files_failed: AtomicU64,
    /// 子树进度：根目录一级子树的总数/完成数（百分比的真实依据）
    pub subs_done: AtomicU64,
    pub subs_total: AtomicU64,
}

/// 把扫描线程的本地计数同步到 `LiveCounters`（供进度事件实时读取）。
fn publish_live(
    live: &LiveCounters,
    files_done: &Cell<u64>,
    files_total: &Cell<u64>,
    bytes_done: &Cell<u64>,
    dirs_done: &Cell<u64>,
    files_failed: &Cell<u64>,
) {
    live.files_done.store(files_done.get(), Ordering::Relaxed);
    live.files_total.store(files_total.get(), Ordering::Relaxed);
    live.bytes_done.store(bytes_done.get(), Ordering::Relaxed);
    live.dirs_done.store(dirs_done.get(), Ordering::Relaxed);
    live.files_failed.store(files_failed.get(), Ordering::Relaxed);
}

/// 扫描线程退出守卫：无论正常结束还是提前 return，都清理「扫描中」标记与并行槽位。
struct VolumeRunGuard {
    svc: Arc<Service>,
    id: String,
}

impl Drop for VolumeRunGuard {
    fn drop(&mut self) {
        self.svc.live.lock().unwrap().remove(&self.id);
        self.svc.scanning.lock().unwrap().remove(&self.id);
        self.svc.running.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Service {
    pub root: IndexRoot,
    pub store: Mutex<Store>,
    pub cfg: Mutex<AppConfig>,
    pub volumes: Mutex<Vec<VolumeInfo>>,
    pub sink: EventSink,
    /// IO 节流 + 内存分级降级控制器（全进程共享）。
    pub governor: Governor,
    controls: Mutex<HashMap<String, VolumeControl>>,
    active: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// 正在运行的目录监听（避免重复启动 watcher）。
    watching: Mutex<std::collections::HashSet<String>>,
    /// 正在扫描的卷（用于区分「暂停扫描线程」与「暂停已完成的 watcher」）。
    scanning: Mutex<std::collections::HashSet<String>>,
    /// 正在运行卷的实时计数（按 volume_id），供进度事件读取。
    live: Mutex<HashMap<String, Arc<LiveCounters>>>,
    /// stats 快照缓存（1 秒）：桌面端 100ms 轮询一次，而每次 stats 都要对每卷
    /// `COUNT(*)` 并遍历索引目录算大小——不缓存的话空闲时也常驻一把全局锁 + 磁盘 IO。
    stats_cache: Mutex<Option<(Instant, Stats)>>,
    shutdown: AtomicBool,
    running: AtomicUsize,
}

/// 解析 `HH:MM`（本地时间）。
fn parse_hhmm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.trim().split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    if h < 24 && m < 60 {
        Some((h, m))
    } else {
        None
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/statm") {
            let parts: Vec<&str> = s.split_whitespace().collect();
            if parts.len() > 1 {
                if let Ok(pages) = parts[1].parse::<u64>() {
                    return pages * 4096;
                }
            }
        }
        0
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::ProcessStatus::{
            K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        unsafe {
            let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            let ok = K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb);
            if ok != 0 {
                return counters.WorkingSetSize as u64;
            }
        }
        0
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        0
    }
}

impl Service {
    pub fn open(cfg: AppConfig, sink: EventSink) -> SnapResult<Arc<Self>> {
        let configured = if cfg.index.root.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(&cfg.index.root))
        };
        let root = resolve_index_root(configured.as_deref());
        let degraded_reason = root.reason.clone();
        let mut store = Store::open(&root)?;
        let mut discovered = volume::discover_volumes();
        // 合并已登记过的范围（含用户手工指定的目录）
        let mut corrupt: Vec<String> = Vec::new();
        for stored in store.load_volumes()? {
            // 旧版本/损坏记录（没有挂载点或文件系统）直接丢弃，避免索引页出现空行
            if stored.mount.trim().is_empty() || stored.fs.trim().is_empty() {
                corrupt.push(stored.volume_id.clone());
                continue;
            }
            // 同一挂载点只保留一条（盘符重挂/命名规则变化会产生不同 volume_id）
            let dup = discovered.iter().any(|v| {
                v.volume_id == stored.volume_id || v.mount.eq_ignore_ascii_case(&stored.mount)
            });
            if !dup {
                discovered.push(stored);
            }
        }
        for id in &corrupt {
            store.volumes.remove(id);
            let _ = store.set_volume_state_in_meta(id, "Discovered");
        }
        for info in &discovered {
            store.register_volume(info)?;
        }
        // 已存在索引数据的卷在启动时打开，保证搜索立即可用
        for info in &discovered {
            if root.volume_dir(&info.volume_id).exists() {
                let _ = store.volume_mut(info);
            }
        }

        let governor = Governor::new(&cfg.index.throttle, cfg.index.memory_budget_bytes);
        let resume_on_start = cfg.index.resume_on_start;
        let svc = Arc::new(Self {
            root,
            store: Mutex::new(store),
            cfg: Mutex::new(cfg),
            volumes: Mutex::new(discovered),
            sink: sink.clone(),
            governor,
            controls: Mutex::new(HashMap::new()),
            active: Mutex::new(HashMap::new()),
            watching: Mutex::new(std::collections::HashSet::new()),
            scanning: Mutex::new(std::collections::HashSet::new()),
            live: Mutex::new(HashMap::new()),
            stats_cache: Mutex::new(None),
            shutdown: AtomicBool::new(false),
            running: AtomicUsize::new(0),
        });

        if let Some(reason) = degraded_reason {
            sink(ServiceEvent::Degraded { reason });
        }
        svc.spawn_progress_ticker();
        svc.clone().spawn_daily_scheduler();
        // 上次退出/断电时没扫完的卷，启动后自动接着扫（已索引文件只做 stat 比对）
        if resume_on_start {
            svc.resume_unfinished();
        }
        // 已完成索引（Watching）的卷：重启后立即恢复变更监听。
        // Windows 上递归 watch 只是单个 ReadDirectoryChangesW 句柄，开销可忽略；
        // 不恢复的话，重启到每日定时增量之间新文件完全不会被索引。
        svc.restore_watchers();
        Ok(svc)
    }

    /// 启动时自动继续未完成的索引。
    ///
    /// 除 Scanning/Indexing 外还必须包含 Rebuilding 与 Paused：
    /// 否则进程在「重建中」被关闭后，该卷会永久停在 0/0 的 Rebuilding 状态，
    /// 重启后既不会被续跑、也不会再有进度（网络盘尤其容易撞上）。
    pub fn resume_unfinished(self: &Arc<Self>) {
        let ids: Vec<String> = {
            let store = self.store.lock().unwrap();
            store
                .volumes
                .iter()
                .filter(|(_, v)| {
                    // Paused 是用户主动暂停的：重启后保持暂停（卡片有完整计数，可手动继续），
                    // 其余未完成状态都自动续跑
                    matches!(
                        v.state.state.as_str(),
                        "Scanning" | "Indexing" | "Rebuilding"
                    )
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        // 并行度按「本次要恢复的卷数」计算：此前传 1，导致多卷续跑被串行化，
        // 后面的卷要等前面的扫完才动，表现为「重启后某些卷一直 0」
        let parallel = self.cfg.lock().unwrap().parallel_volumes(ids.len());
        for id in ids {
            if let Some(info) = self.find_volume(&id) {
                self.controls
                    .lock()
                    .unwrap()
                    .insert(id.clone(), self.new_control());
                self.spawn_volume_thread(info, parallel);
            }
        }
    }

    /// 启动时为所有 Watching 卷恢复文件变更监听（程序重启会丢掉内存里的 watcher）。
    fn restore_watchers(self: &Arc<Self>) {
        let infos: Vec<VolumeInfo> = {
            let store = self.store.lock().unwrap();
            store
                .volumes
                .values()
                .filter(|v| v.state.state == "Watching")
                .map(|v| v.info.clone())
                .collect()
        };
        for info in infos {
            if self.find_volume(&info.volume_id).is_some() {
                self.start_watch(&info);
            }
        }
    }

    /// 每日定时增量索引：每 30s 检查一次，到点后对「已完成且正在监听」的卷重扫一遍。
    ///
    /// 重扫本身是增量的：`index_file` 对 mtime+size 未变的文件直接跳过，不会重复抽取。
    fn spawn_daily_scheduler(self: Arc<Self>) {
        let _ = std::thread::Builder::new()
            .name("idxd-daily".into())
            .spawn(move || {
                let mut last_day = 0u32;
                loop {
                    std::thread::sleep(Duration::from_secs(30));
                    if self.is_shutting_down() {
                        break;
                    }
                    // 桌面端改过 config.json 时热加载索引计划与排除规则（无需重启索引进程）
                    let disk = snapfind_core::config::AppConfig::load();
                    let (enabled, at) = {
                        let mut cfg = self.cfg.lock().unwrap();
                        if cfg.index.daily_scan != disk.index.daily_scan
                            || cfg.index.daily_scan_at != disk.index.daily_scan_at
                        {
                            cfg.index.daily_scan = disk.index.daily_scan;
                            cfg.index.daily_scan_at = disk.index.daily_scan_at.clone();
                        }
                        cfg.index.exclude_paths = disk.index.exclude_paths.clone();
                        cfg.index.exclude_types = disk.index.exclude_types.clone();
                        (cfg.index.daily_scan, cfg.index.daily_scan_at.clone())
                    };
                    if !enabled {
                        continue;
                    }
                    let Some((h, m, day)) = snapfind_core::platform::local_hhmm() else {
                        continue;
                    };
                    let Some((th, tm)) = parse_hhmm(&at) else {
                        continue;
                    };
                    if day == last_day || (h, m) < (th, tm) {
                        continue;
                    }
                    last_day = day;
                    let ids: Vec<String> = {
                        let store = self.store.lock().unwrap();
                        store
                            .volumes
                            .iter()
                            .filter(|(_, v)| v.state.state == "Watching")
                            .map(|(id, _)| id.clone())
                            .collect()
                    };
                    let parallel = self.cfg.lock().unwrap().parallel_volumes(ids.len());
                    for id in ids {
                        if let Some(info) = self.find_volume(&id) {
                            self.controls
                                .lock()
                                .unwrap()
                                .insert(id.clone(), self.new_control());
                            self.spawn_volume_thread(info, parallel);
                        }
                    }
                }
            });
    }

    pub fn list_volumes(&self) -> Vec<VolumeInfo> {
        self.volumes.lock().unwrap().clone()
    }

    /// 重新枚举系统卷并与已登记范围合并（保留用户手工添加的目录）。
    ///
    /// `list_volumes` 只是进程启动时的缓存：启动后才映射的网络盘、新插入的移动盘
    /// 都不会出现，「重新扫描磁盘」也就点不动。范围预览统一走这里刷新。
    pub fn refresh_volumes(&self) -> Vec<VolumeInfo> {
        let mut merged = volume::discover_volumes();
        let mut vols = self.volumes.lock().unwrap();
        for v in vols.iter() {
            // 已登记的光盘/光学介质记录不再保留
            if snapfind_core::volume::is_optical_fs(&v.fs) {
                continue;
            }
            // 手工目录（fs == "dir"）与当前枚举不到的已登记卷都保留，
            // 掉盘的卷仍在列表里（状态显示为不可访问），不会被静默丢弃
            let seen = merged
                .iter()
                .any(|d| d.volume_id == v.volume_id || d.mount.eq_ignore_ascii_case(&v.mount));
            if !seen {
                merged.push(v.clone());
            }
        }
        merged.sort_by(|a, b| a.mount.cmp(&b.mount));
        merged.dedup_by(|a, b| a.volume_id == b.volume_id);
        *vols = merged.clone();
        merged
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }

    fn clone_ref(self: &Arc<Self>) -> Arc<Self> {
        Arc::clone(self)
    }

    fn emit_state(&self, volume_id: &str, mount: &str, state: &str, reason: Option<String>) {
        let files_done = {
            let store = self.store.lock().unwrap();
            store
                .volume(volume_id)
                .map(|v| v.state.files_done)
                .unwrap_or(0)
        };
        (self.sink)(ServiceEvent::VolumeState(VolumeStateEvent {
            volume_id: volume_id.to_string(),
            mount: mount.to_string(),
            state: state.to_string(),
            reason,
            files_done,
        }));
    }

    /// 起一个卷扫描线程。
    ///
    /// 以前统一用 `let _ = ...spawn()`：线程创建失败（句柄/内存耗尽、系统限制）被
    /// 静默吞掉，卷就永久停在「未开始」或「扫描中」且没有任何提示。这里改为
    /// 明确写 Degraded 并发事件，让卡片显示可行动的原因。
    fn spawn_volume_thread(self: &Arc<Self>, info: VolumeInfo, parallel: usize) -> bool {
        let me = self.clone_ref();
        let volume_id = info.volume_id.clone();
        let mount = info.mount.clone();
        match std::thread::Builder::new()
            .name(format!("idxd-{}", short_id(&volume_id)))
            .spawn(move || me.run_volume(info, parallel))
        {
            Ok(_) => true,
            Err(e) => {
                let msg = format!(
                    "无法启动索引线程：{e}（系统资源不足，可稍后重试或重启 SnapFind）"
                );
                {
                    let store = self.store.lock().unwrap();
                    let _ = store.set_volume_state_in_meta(&volume_id, "Degraded");
                    store.log_event(Some(&volume_id), "spawn_failed", &msg);
                }
                self.emit_state(&volume_id, &mount, "Degraded", Some(msg));
                false
            }
        }
    }

    /// 把用户指定的目录登记为一个索引范围（便于只索引某个目录，而非整盘）。
    pub fn register_scope_dir(&self, dir: &str) -> SnapResult<String> {
        let path = std::path::PathBuf::from(dir);
        if !path.is_dir() {
            return Err(SnapError::new(
                ErrorCode::VolumeUnavailable,
                format!("目录不存在或不可访问: {dir}"),
            ));
        }
        let canon = std::fs::canonicalize(&path).unwrap_or(path);
        let mut mount = canon.to_string_lossy().to_string();
        // canonicalize 对 UNC 路径返回 \\?\UNC\server\share\...；
        // 直接去掉 \\?\ 会得到 UNC\server\share\...（非法路径），必须还原成 \\server\share\...
        if let Some(rest) = mount.strip_prefix(r"\\?\UNC\") {
            mount = format!(r"\\{rest}");
        } else if let Some(stripped) = mount.strip_prefix(r"\\?\") {
            mount = stripped.to_string();
        }
        if let Some(v) = self
            .volumes
            .lock()
            .unwrap()
            .iter()
            .find(|v| v.mount.eq_ignore_ascii_case(&mount))
        {
            return Ok(v.volume_id.clone());
        }
        let fs = "dir".to_string();
        let id = snapfind_core::volume::volume_id_for(&mount, &fs);
        // UNC 路径按网络卷登记，避免后续做容量/文件量采样时卡在 SMB 超时
        let network = mount.starts_with(r"\\");
        let info = VolumeInfo {
            volume_id: id.clone(),
            mount,
            fs,
            total_bytes: 0,
            free_bytes: 0,
            removable: false,
            network,
            enabled: true,
        };
        {
            let mut store = self.store.lock().unwrap();
            store.register_volume(&info)?;
        }
        self.volumes.lock().unwrap().push(info);
        Ok(id)
    }

    /// 范围确认页数据（文档 §5.4）。
    pub fn scope_preview(&self) -> ScopePreview {
        let cfg = self.cfg.lock().unwrap().clone();
        let mut excludes = snapfind_core::platform::default_excludes();
        excludes.extend(cfg.index.exclude_rules.clone());
        excludes.push(format!(
            "{}/**",
            self.root.path.to_string_lossy().replace('\\', "/")
        ));

        let volumes = self.refresh_volumes();
        let scope = volumes
            .iter()
            .map(|v| ScopeVolume {
                volume_id: v.volume_id.clone(),
                mount: v.mount.clone(),
                fs: v.fs.clone(),
                total_bytes: v.total_bytes,
                free_bytes: v.free_bytes,
                removable: v.removable,
                network: v.network,
                enabled: v.enabled,
                // 网络盘 / 手工目录不做采样（前者可能卡在 SMB 超时，后者本就不大）
                estimated_files: if v.network || v.total_bytes == 0 {
                    0
                } else {
                    estimate_files(&v.mount, v.total_bytes.saturating_sub(v.free_bytes))
                },
            })
            .collect();

        ScopePreview {
            index_root: self.root.path.to_string_lossy().to_string(),
            degraded_reason: self.root.reason.clone(),
            volumes: scope,
            excludes,
        }
    }

    pub fn start_index(
        self: &Arc<Self>,
        ids: Vec<String>,
        extra_excludes: Vec<String>,
    ) -> SnapResult<Vec<String>> {
        // 用户刚映射/刚插上的盘可能还不在缓存里：先按需刷新一次再匹配，
        // 否则「索引所选」会对这些卷报「没有可索引的卷」
        if !ids.is_empty() {
            let missing = {
                let vols = self.volumes.lock().unwrap();
                ids.iter()
                    .any(|id| !vols.iter().any(|v| &v.volume_id == id))
            };
            if missing {
                self.refresh_volumes();
            }
        }

        let cfg = self.cfg.lock().unwrap().clone();
        let mut targets: Vec<VolumeInfo> = Vec::new();
        {
            let mut vols = self.volumes.lock().unwrap();
            for v in vols.iter_mut() {
                if ids.is_empty() {
                    if v.enabled {
                        targets.push(v.clone());
                    }
                } else if ids.contains(&v.volume_id) {
                    v.enabled = true;
                    targets.push(v.clone());
                }
            }
        }
        if targets.is_empty() {
            return Err(SnapError::new(
                ErrorCode::VolumeUnavailable,
                "没有可索引的卷（未勾选或不可访问）",
            ));
        }

        let parallel = cfg.parallel_volumes(targets.len());
        {
            let mut ex = cfg.index.exclude_rules.clone();
            ex.extend(extra_excludes);
            if let Ok(mut cfg_mut) = self.cfg.lock() {
                cfg_mut.index.exclude_rules = ex;
            }
        }

        // 启动后才出现的卷（新映射的网络盘、新插入的移动盘、手工目录）可能还没登记到
        // store；未登记的卷在 run_volume 里会直接返回，表现为「任务已开始但毫无进度」。
        let mut ready: Vec<VolumeInfo> = Vec::new();
        {
            let mut store = self.store.lock().unwrap();
            for info in targets {
                if store.register_volume(&info).is_ok() {
                    ready.push(info);
                }
            }
        }
        if ready.is_empty() {
            return Err(SnapError::new(
                ErrorCode::VolumeUnavailable,
                "索引范围无法登记（索引目录不可写？）",
            ));
        }
        let targets = ready;

        for info in &targets {
            self.controls
                .lock()
                .unwrap()
                .insert(info.volume_id.clone(), VolumeControl::default());
        }

        // 立即宣告状态：排队等待并行槽位、打开索引目录期间的卷不会有任何 progress，
        // 桌面端靠这条事件先把卡片建出来，否则「点了索引却看不到任务」
        for info in &targets {
            self.emit_state(&info.volume_id, &info.mount, "Scanning", None);
        }

        let started: Vec<String> = targets.iter().map(|v| v.volume_id.clone()).collect();
        for info in targets {
            self.spawn_volume_thread(info, parallel);
        }
        Ok(started)
    }

    fn acquire_slot(&self, limit: usize) -> bool {
        loop {
            if self.is_shutting_down() {
                return false;
            }
            let cur = self.running.load(Ordering::SeqCst);
            if cur < limit.max(1) {
                if self
                    .running
                    .compare_exchange(cur, cur + 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    return true;
                }
            } else {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    fn control(&self, volume_id: &str) -> VolumeControl {
        self.controls
            .lock()
            .unwrap()
            .get(volume_id)
            .cloned()
            .unwrap_or_default()
    }

    /// 新建一代控制条目（代际自增），用于 start / resume / rebuild / clean。
    fn new_control(&self) -> VolumeControl {
        VolumeControl {
            pause: false,
            stop: false,
            epoch: next_epoch(),
        }
    }

    /// 等待某卷的扫描线程与 watcher 全部退出（清理/重建前必须做，
    /// 否则 SQLite 句柄仍打开时删不掉目录）。
    fn wait_worker_exit(&self, volume_id: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let scanning = self.scanning.lock().unwrap().contains(volume_id);
            let watching = self.watching.lock().unwrap().contains(volume_id);
            if !scanning && !watching {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn run_volume(self: Arc<Self>, info: VolumeInfo, parallel: usize) {
        // 兜底：卷扫描线程里的任何 panic 都会让该卷无声停在「建立索引」，
        // 卡片永久不动。这里捕获后明确写成 Degraded，让用户看到可行动的原因。
        let id = info.volume_id.clone();
        let mount = info.mount.clone();
        let me = self.clone_ref();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            me.run_volume_inner(info, parallel)
        }));
        if result.is_err() {
            if let Some(info) = self.find_volume(&id) {
                let mut store = self.store.lock().unwrap();
                if let Ok(vol) = store.volume_mut(&info) {
                    // store 里的状态必须一起改：否则 ticker 仍按 Scanning 发进度，
                    // 卡片看起来还在「建立索引」，实际线程早已退出
                    vol.state.state = "Degraded".into();
                    vol.state.phase = "idle".into();
                    let _ = vol.save_state();
                }
                let _ = store.set_volume_state_in_meta(&id, "Degraded");
                store.log_event(Some(&id), "panic", "scan thread panicked");
            }
            self.emit_state(
                &id,
                &mount,
                "Degraded",
                Some("索引线程异常终止（该卷已停止），点「重建」可重试".into()),
            );
        }
    }

    fn run_volume_inner(self: Arc<Self>, info: VolumeInfo, parallel: usize) {
        if !self.acquire_slot(parallel) {
            return;
        }
        // 代际：本次扫描属于哪一代控制条目；被 rebuild/clean/resume 接管后不再写状态
        let my_epoch = self.control(&info.volume_id).epoch;
        self.scanning.lock().unwrap().insert(info.volume_id.clone());
        // 任何退出路径（含提前 return）都清理「扫描中」标记与并行槽位
        let _guard = VolumeRunGuard {
            svc: Arc::clone(&self),
            id: info.volume_id.clone(),
        };
        // 断点续跑：上次停在 Scanning/Indexing 且记录了「已完成的一级子树」→ 接着扫
        let (resume, done_list, base_done, base_total) = {
            let store = self.store.lock().unwrap();
            match store.volume(&info.volume_id) {
                Some(v) => {
                    // Paused 也属于「未完成」：继续时要接着断点跑，
                    // 否则中断点被清空、进度归零，等于整卷重扫
                    let unfinished =
                        matches!(v.state.state.as_str(), "Scanning" | "Indexing" | "Paused");
                    (
                        unfinished,
                        v.state.done_subtrees.clone(),
                        v.state.files_done,
                        v.state.files_total,
                    )
                }
                None => (false, Vec::new(), 0, 0),
            }
        };
        {
            let mut store = self.store.lock().unwrap();
            match store.volume_mut(&info) {
                Ok(vol) => {
                    vol.state.state = "Scanning".into();
                    vol.state.phase = "scan".into();
                    if !resume {
                        vol.state.files_done = 0;
                        vol.state.files_total = 0;
                        vol.state.dirs_done = 0;
                        vol.state.files_failed = 0;
                        vol.state.done_subtrees.clear();
                    }
                    let _ = vol.save_state();
                }
                Err(e) => {
                    // 打不开索引目录（磁盘满 / 杀软占用 / 权限问题）：明确降级并告知，
                    // 而不是静默消失——否则该卷永远不会出现在进度与统计里
                    let _ = store.set_volume_state_in_meta(&info.volume_id, "Degraded");
                    let msg = format!("索引目录打开失败：{e}");
                    drop(store);
                    self.emit_state(&info.volume_id, &info.mount, "Degraded", Some(msg));
                    return;
                }
            }
            let _ = store.set_volume_state_in_meta(&info.volume_id, "Scanning");
        }
        self.emit_state(&info.volume_id, &info.mount, "Scanning", None);

        // 网络盘/映射盘：先确认根目录真的能枚举。映射断开时 walk 内部对
        // read_dir 错误是「静默跳过」的，结果是任务显示在跑、文件数永远 0，
        // 用户分不清是空盘、掉线还是程序坏了。这里给出确定性的失败原因。
        if info.network {
            if let Err(e) = std::fs::read_dir(std::path::PathBuf::from(&info.mount)) {
                let msg = format!(
                    "无法访问网络盘 {}：{e}（映射可能已断开，重新连接后点「重建」再试）",
                    info.mount
                );
                {
                    let store = self.store.lock().unwrap();
                    let _ = store.set_volume_state_in_meta(&info.volume_id, "Degraded");
                }
                self.emit_state(&info.volume_id, &info.mount, "Degraded", Some(msg));
                return;
            }
        }

        let live = Arc::new(LiveCounters::default());
        self.live
            .lock()
            .unwrap()
            .insert(info.volume_id.clone(), Arc::clone(&live));
        // 让 ticker 在第一批文件处理完之前也显示正确的断点基数
        live.files_done.store(base_done, Ordering::Relaxed);
        live.files_total
            .store(if resume { base_total.max(base_done) } else { 0 }, Ordering::Relaxed);

        let cfg = self.cfg.lock().unwrap().clone();
        let index_root_glob = format!(
            "{}/**",
            self.root.path.to_string_lossy().replace('\\', "/")
        );
        let exclude_set = build_exclude_set(&cfg, &[index_root_glob]);

        let root = std::path::PathBuf::from(&info.mount);
        let files_done = Cell::new(base_done);
        let files_total = Cell::new(if resume {
            base_total.max(base_done)
        } else {
            0
        });
        let bytes_done = Cell::new(0u64);
        let dirs_done = Cell::new(0u64);
        let files_failed = Cell::new(0u64);
        let since_save = Cell::new(0u64);
        let last_save = Cell::new(Instant::now());
        let paused = Cell::new(false);
        let stopped = Cell::new(false);
        // 已完成的一级子树（断点续跑），用 RefCell 让多个闭包共享
        let done_subtrees = std::cell::RefCell::new(done_list);

        {
            let flush = |force: bool| {
                if !force
                    && since_save.get() < cfg.index.checkpoint_every_files
                    && (last_save.get().elapsed().as_millis() as u64)
                        < cfg.index.checkpoint_every_ms
                {
                    return;
                }
                since_save.set(0);
                last_save.set(Instant::now());
                self.flush_state(
                    &info,
                    files_done.get(),
                    files_total.get(),
                    bytes_done.get(),
                    dirs_done.get(),
                    files_failed.get(),
                    &done_subtrees.borrow(),
                    live.subs_done.load(Ordering::Relaxed),
                    live.subs_total.load(Ordering::Relaxed),
                );
            };

            let mut on_file = |path: &std::path::Path, size: u64, mtime: i64| {
                if stopped.get() {
                    return;
                }
                let ctl = self.control(&info.volume_id);
                if ctl.epoch != my_epoch {
                    // 已被 rebuild/clean/resume 接管：立刻退出，不写任何状态
                    if let Ok(store) = self.store.lock() {
                        store.log_event(
                            Some(&info.volume_id),
                            "abort-epoch",
                            &format!("my={my_epoch} cur={} at {}", ctl.epoch, path.display()),
                        );
                    }
                    stopped.set(true);
                    return;
                }
                if ctl.stop || self.is_shutting_down() {
                    if let Ok(store) = self.store.lock() {
                        store.log_event(
                            Some(&info.volume_id),
                            "abort-stop",
                            &format!(
                                "stop={} shutdown={} at {}",
                                ctl.stop,
                                self.is_shutting_down(),
                                path.display()
                            ),
                        );
                    }
                    stopped.set(true);
                    return;
                }
                if ctl.pause {
                    paused.set(true);
                    return;
                }
                // 用户点了「继续」：复位暂停标记，线程原地恢复处理
                // （否则 paused 一旦置真就永远空转，必须重启线程才能继续）
                paused.set(false);
                files_total.set(files_total.get() + 1);
                // 两段式：锁内只做毫秒级变更检测与写库，节流/抽取/分词在锁外，
                // 保证搜索查询不被扫描线程长期阻塞
                let path_str = path.to_string_lossy().to_string();
                let gov_level = self.governor.level();
                let unchanged = {
                    let mut store = self.store.lock().unwrap();
                    match store.volume_mut(&info) {
                        Ok(vol) => match vol.doc_stat(&path_str) {
                            Ok(Some(stat)) => {
                                // 尺寸/mtime 未变，但正文可能是内存压力期（L2）被跳过的：
                                // 恢复 L0 后把这类文件重新抽取补齐正文
                                //（重抽成功后 flags 清零，之后的扫描继续走快路径）
                                stat.size == size
                                    && stat.mtime == mtime
                                    && (stat.flags & FLAG_METADATA_ONLY == 0 || gov_level > 0)
                            }
                            _ => false,
                        },
                        Err(_) => false,
                    }
                };
                let outcome = if unchanged {
                    scan::FileOutcome::Unchanged
                } else {
                    match scan::prepare_file(path, size, mtime, &cfg, &self.governor) {
                        Some((doc, outcome)) => {
                            let mut store = self.store.lock().unwrap();
                            match store.volume_mut(&info) {
                                Ok(vol) => scan::apply_prepared(vol, &doc, outcome),
                                Err(_) => scan::FileOutcome::Failed,
                            }
                        }
                        None => scan::FileOutcome::Failed,
                    }
                };
                match outcome {
                    scan::FileOutcome::Indexed | scan::FileOutcome::Unsupported => {
                        files_done.set(files_done.get() + 1);
                        bytes_done.set(bytes_done.get() + size);
                    }
                    scan::FileOutcome::Unchanged => {
                        files_done.set(files_done.get() + 1);
                    }
                    scan::FileOutcome::Failed => {
                        files_failed.set(files_failed.get() + 1);
                        // 「已处理」口径：失败文件同样消耗了处理时间，也计入 done，
                        // 否则 files_done 永远追不平 files_total，进度条到不了 100%
                        files_done.set(files_done.get() + 1);
                    }
                }
                since_save.set(since_save.get() + 1);
                // 实时计数：ticker 每 200ms 读它算速度/进度，不必等检查点落盘
                publish_live(&live, &files_done, &files_total, &bytes_done, &dirs_done, &files_failed);
                flush(false);
            };
            let mut on_dir = |_dir: &std::path::Path| {
                dirs_done.set(dirs_done.get() + 1);
                publish_live(&live, &files_done, &files_total, &bytes_done, &dirs_done, &files_failed);
                if dirs_done.get() % 64 == 0 {
                    flush(false);
                }
            };

            // 按一级子树推进：每完成一棵子树写一次 state.json，
            // 重启后整棵跳过已完成的子树，只补没做完的部分。
            let mut children: Vec<std::path::PathBuf> = match std::fs::read_dir(&root) {
                Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
                Err(_) => Vec::new(),
            };
            children.sort();
            // 子树进度：一级子树总数在开扫前就已知，用它算出的百分比是真实值
            live.subs_total.store(children.len() as u64, Ordering::Relaxed);
            live.subs_done
                .store(done_subtrees.borrow().len() as u64, Ordering::Relaxed);
            if children.is_empty() {
                scan::walk(
                    &root,
                    &exclude_set,
                    cfg.index.follow_symlinks,
                    &mut on_file,
                    &mut on_dir,
                );
            }
            for child in children {
                if stopped.get() || self.is_shutting_down() {
                    break;
                }
                // 暂停时原地等待，绝不退出线程：退出会让「继续」只能重启线程，
                // 且收尾代码会把状态写回 Paused，盖掉 resume 刚发的 Scanning——
                // 卡片出现「继续 → 暂停 → 继续」的抖动，甚至任务实际已死。
                while paused.get() && !stopped.get() && !self.is_shutting_down() {
                    std::thread::sleep(Duration::from_millis(100));
                    let ctl = self.control(&info.volume_id);
                    if ctl.stop {
                        stopped.set(true);
                    }
                    if !ctl.pause {
                        paused.set(false);
                    }
                }
                if stopped.get() || self.is_shutting_down() {
                    break;
                }
                let key = child.to_string_lossy().to_string();
                if done_subtrees.borrow().iter().any(|d| d == &key) {
                    continue;
                }
                if exclude_set.is_excluded(&key) {
                    continue;
                }
                let meta = match std::fs::symlink_metadata(&child) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if meta.is_symlink() && !cfg.index.follow_symlinks {
                    continue;
                }
                if meta.is_dir() {
                    scan::walk(
                        &child,
                        &exclude_set,
                        cfg.index.follow_symlinks,
                        &mut on_file,
                        &mut on_dir,
                    );
                } else if meta.is_file() {
                    let mtime = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    on_file(&child, meta.len(), mtime);
                } else {
                    continue;
                }
                // 断点条目设上限，避免超大目录把 state.json 撑大；
                // 超出后仍会从头走一遍，但未变更文件只做 stat 比对，不会重复抽取。
                if done_subtrees.borrow().len() < 10_000 {
                    done_subtrees.borrow_mut().push(key);
                }
                live.subs_done
                    .store(done_subtrees.borrow().len() as u64, Ordering::Relaxed);
                // 断点按阈值批量落盘（checkpoint_every_files / _ms），不要每个子树都写一次
                since_save.set(since_save.get() + 1);
                flush(false);
            }
            flush(true);
            // 扫描结束：垃圾字节（更新留下的旧块）占比高时整理分段，回收磁盘
            {
                let mut store = self.store.lock().unwrap();
                let freed = {
                    let vol = match store.volume_mut(&info) {
                        Ok(v) => v,
                        Err(_) => return,
                    };
                    if vol.compact_needed() {
                        vol.compact().unwrap_or(0)
                    } else {
                        0
                    }
                };
                if freed > 0 {
                    store.log_event(
                        Some(&info.volume_id),
                        "compact",
                        &format!("reclaimed_bytes={freed}"),
                    );
                }
            }
        }

        // 代际已变（rebuild / clean / resume 接管了该卷）：本次扫描作废，
        // 不得覆盖新任务的状态，直接退出（守卫会清理扫描标记与槽位）
        if self.control(&info.volume_id).epoch != my_epoch {
            return;
        }

        // 进程退出导致的中断保留 "Indexing"：下次启动据此自动续跑；
        // 用户手动停止/暂停才落成 Stopped / Paused。
        let next_state = if self.is_shutting_down() {
            "Indexing"
        } else if stopped.get() {
            "Stopped"
        } else if paused.get() {
            "Paused"
        } else {
            "Watching"
        };

        {
            let mut store = self.store.lock().unwrap();
            if let Ok(vol) = store.volume_mut(&info) {
                vol.state.state = next_state.to_string();
                vol.state.phase = if next_state == "Watching" {
                    "watch".to_string()
                } else {
                    "idle".to_string()
                };
                if next_state == "Watching" {
                    // 扫描完成：清空断点子树，下次（定时增量/手工）从头走一遍 stat
                    vol.state.done_subtrees.clear();
                    // 进入监控 = 全量索引完成：子树进度置满，UI 显示 100%
                    // （live 计数器随扫描线程退出销毁，必须落进持久状态）
                    vol.state.subs_done = vol.state.subs_total;
                }
                let next_cp_id = vol.state.checkpoint.as_ref().map(|c| c.id + 1).unwrap_or(1);
                vol.state.checkpoint = Some(CheckpointInfo {
                    id: next_cp_id,
                    at: now_secs(),
                    cursor_path: info.mount.clone(),
                    resumable: true,
                });
                let _ = vol.save_state();
                vol.checkpoint().ok();
            }
            let _ = store.set_volume_state_in_meta(&info.volume_id, next_state);
            store.log_event(
                Some(&info.volume_id),
                next_state,
                &format!(
                    "files_done={} failed={}",
                    files_done.get(),
                    files_failed.get()
                ),
            );
        }
        self.emit_state(&info.volume_id, &info.mount, next_state, None);

        if next_state == "Watching" {
            self.start_watch(&info);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn flush_state(
        &self,
        info: &VolumeInfo,
        files_done: u64,
        files_total: u64,
        bytes_done: u64,
        dirs_done: u64,
        files_failed: u64,
        done_subtrees: &[String],
        subs_done: u64,
        subs_total: u64,
    ) {
        let mut store = self.store.lock().unwrap();
        if let Ok(vol) = store.volume_mut(info) {
            vol.state.files_done = files_done;
            vol.state.files_total = files_total;
            vol.state.bytes_done = bytes_done;
            vol.state.dirs_done = dirs_done;
            vol.state.files_failed = files_failed;
            vol.state.subs_done = subs_done;
            vol.state.subs_total = subs_total;
            vol.state.done_subtrees = done_subtrees.to_vec();
            vol.state.phase = "scan".into();
            vol.state.state = "Indexing".into();
            // 检查点必须与数据库一致：先提交批量写事务再落 state.json
            let _ = vol.commit_batch();
            let _ = vol.save_state();
        }
    }



    fn start_watch(self: &Arc<Self>, info: &VolumeInfo) {
        // 同一卷只允许一个 watcher（定时增量重扫后会再次走到这里）
        if !self
            .watching
            .lock()
            .unwrap()
            .insert(info.volume_id.clone())
        {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel::<Vec<std::path::PathBuf>>();
        let handle = match watch::start(&[std::path::PathBuf::from(&info.mount)], tx) {
            Ok(h) => h,
            Err(e) => {
                self.watching.lock().unwrap().remove(&info.volume_id);
                // 监听起不来时，该卷表面是 Watching，实际不会再收到任何变更。
                // 以前这里静默 return，用户只会发现「新文件搜不到」。
                let msg = format!(
                    "文件变更监听启动失败：{e}（该范围不会自动增量更新，可点「重建」重试）"
                );
                {
                    let store = self.store.lock().unwrap();
                    let _ = store.set_volume_state_in_meta(&info.volume_id, "Degraded");
                    store.log_event(Some(&info.volume_id), "watch_failed", &msg);
                }
                self.emit_state(&info.volume_id, &info.mount, "Degraded", Some(msg));
                return;
            }
        };
        let me = self.clone_ref();
        let vol_info = info.clone();
        let _ = std::thread::Builder::new()
            .name(format!("watch-{}", short_id(&info.volume_id)))
            .spawn(move || {
                let _handle: WatchHandle = handle;
                // 暂停期间积压的变更（恢复后补应用）：避免「暂停后仍在写索引」，
                // 也避免恢复时丢掉这段时间的文件变化
                let mut deferred: Vec<std::path::PathBuf> = Vec::new();
                loop {
                    if me.is_shutting_down() || me.control(&vol_info.volume_id).stop {
                        break;
                    }
                    let paused = me.control(&vol_info.volume_id).pause;
                    if !paused && !deferred.is_empty() {
                        let batch = std::mem::take(&mut deferred);
                        me.apply_changes(&vol_info, batch);
                    }
                    match rx.recv_timeout(Duration::from_millis(1000)) {
                        Ok(mut batch) => {
                            while let Ok(more) = rx.try_recv() {
                                batch.extend(more);
                            }
                            if batch.is_empty() {
                                (me.sink)(ServiceEvent::WatchOverflow {
                                    volume_id: vol_info.volume_id.clone(),
                                });
                                continue;
                            }
                            if me.control(&vol_info.volume_id).pause {
                                // 上限保护：超量则丢弃并提示，恢复后由增量重扫兜底
                                if deferred.len() + batch.len() <= 20_000 {
                                    deferred.extend(batch);
                                } else {
                                    deferred.clear();
                                    (me.sink)(ServiceEvent::WatchOverflow {
                                        volume_id: vol_info.volume_id.clone(),
                                    });
                                }
                                continue;
                            }
                            me.apply_changes(&vol_info, batch);
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                me.watching.lock().unwrap().remove(&vol_info.volume_id);
            });
    }

    fn apply_changes(&self, info: &VolumeInfo, batch: Vec<std::path::PathBuf>) {
        if self.is_shutting_down() {
            return;
        }
        let cfg = self.cfg.lock().unwrap().clone();
        let exclude_set = build_exclude_set(&cfg, &[]);
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(batch.len());
        // 小批次豁免 IO 节流：用户新建的文件应秒级入库；大批次
        // （如整目录复制进来）仍走共享令牌桶，避免冲击前台 IO
        let pace = batch.len() <= 32;
        for path in batch {
            let p = path.to_string_lossy().to_string();
            if !seen.insert(p.clone()) {
                continue;
            }
            if exclude_set.is_excluded(&p) {
                continue;
            }
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() => {
                    let mtime = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    let size = meta.len();
                    // 变更检测在锁内、节流/抽取/分词在锁外、写库在锁内（与全量扫描一致）
                    let gov_level = self.governor.level();
                    let unchanged = {
                        let mut store = self.store.lock().unwrap();
                        match store.volume_mut(info) {
                            Ok(vol) => match vol.doc_stat(&p) {
                                // 同上：L2 降级期只索引了文件名的文档，
                                // 内存回落后要重新抽取正文
                                Ok(Some(stat)) => {
                                    stat.size == size
                                        && stat.mtime == mtime
                                        && (stat.flags & FLAG_METADATA_ONLY == 0 || gov_level > 0)
                                }
                                _ => false,
                            },
                            Err(_) => false,
                        }
                    };
                    if unchanged {
                        continue;
                    }
                    if let Some((doc, outcome)) =
                        scan::prepare_file_paced(&path, size, mtime, &cfg, &self.governor, pace)
                    {
                        let mut store = self.store.lock().unwrap();
                        if let Ok(vol) = store.volume_mut(info) {
                            let _ = scan::apply_prepared(vol, &doc, outcome);
                        }
                    }
                }
                _ => {
                    // 文件被删除或变更为目录：移除索引记录
                    let mut store = self.store.lock().unwrap();
                    if let Ok(vol) = store.volume_mut(info) {
                        let _ = vol.delete_path(&p);
                    }
                }
            }
        }
        // 批次写完提交批量写事务
        let store = self.store.lock().unwrap();
        if let Some(v) = store.volume(&info.volume_id) {
            let _ = v.commit_batch();
        }
    }

    /// 执行单卷 / 全部卷动作，返回每个卷的最终状态（id, state）供前端即时展示。
    pub fn volume_action(
        self: &Arc<Self>,
        action: &VolumeAction,
    ) -> SnapResult<Vec<(String, String)>> {
        let ids: Vec<String> = match &action.volume_id {
            Some(id) => vec![id.clone()],
            None => self
                .list_volumes()
                .iter()
                .map(|v| v.volume_id.clone())
                .collect(),
        };
        let mut results: Vec<(String, String)> = Vec::new();
        for id in ids {
            let state = match action.action.as_str() {
                "pause" => {
                    {
                        let mut controls = self.controls.lock().unwrap();
                        let c = controls.entry(id.clone()).or_insert_with(|| VolumeControl {
                            epoch: next_epoch(),
                            ..Default::default()
                        });
                        c.pause = true;
                        c.stop = false;
                    }
                    self.mark_state(&id, "Paused");
                    "Paused"
                }
                "resume" | "start" => {
                    let scanning = self.scanning.lock().unwrap().contains(&id);
                    let watching = self.watching.lock().unwrap().contains(&id);
                    if scanning || watching {
                        // 线程还活着：清掉暂停标志即可，由线程自己恢复，不必重扫
                        if let Some(c) = self.controls.lock().unwrap().get_mut(&id) {
                            c.pause = false;
                            c.stop = false;
                        }
                        if watching && !scanning {
                            self.mark_state(&id, "Watching");
                            "Watching"
                        } else {
                            self.mark_state(&id, "Scanning");
                            "Scanning"
                        }
                    } else if let Some(info) = self.find_volume(&id) {
                        // 线程已退出：新一代控制条目 + 重启扫描。
                        // 状态为 Paused 时保留断点，继续而不是清零重扫。
                        let ctl = self.new_control();
                        self.controls.lock().unwrap().insert(id.clone(), ctl);
                        // 自动并行模式下不许排队：正在扫 N 个卷就允许 N+1 并发，
                        // 否则新任务要等别的卷扫完才动，卡片表现为「跑马灯无进展」；
                        // 手动设置了并行上限时仍尊重该上限
                        let parallel = self
                            .cfg
                            .lock()
                            .unwrap()
                            .parallel_volumes(self.running.load(Ordering::SeqCst) + 1);
                        self.spawn_volume_thread(info, parallel);
                        "Scanning"
                    } else {
                        return Err(SnapError::new(
                            ErrorCode::VolumeUnavailable,
                            format!("找不到索引范围 {id}（磁盘可能已离线）"),
                        ));
                    }
                }
                "stop" => {
                    {
                        let mut controls = self.controls.lock().unwrap();
                        let c = controls.entry(id.clone()).or_insert_with(|| VolumeControl {
                            epoch: next_epoch(),
                            ..Default::default()
                        });
                        c.stop = true;
                        c.pause = false;
                    }
                    self.mark_state(&id, "Stopped");
                    "Stopped"
                }
                "rebuild" => {
                    // 1) 先让旧扫描线程 / watcher 退出：旧线程不停会与重建并发写同一卷
                    {
                        let mut controls = self.controls.lock().unwrap();
                        if let Some(c) = controls.get_mut(&id) {
                            c.stop = true;
                            c.pause = false;
                        } else {
                            controls.insert(
                                id.clone(),
                                VolumeControl {
                                    epoch: next_epoch(),
                                    stop: true,
                                    pause: false,
                                },
                            );
                        }
                    }
                    self.wait_worker_exit(&id, Duration::from_secs(3));

                    let Some(info) = self.find_volume(&id) else {
                        return Err(SnapError::new(
                            ErrorCode::VolumeUnavailable,
                            format!("找不到索引范围 {id}（磁盘可能已离线）"),
                        ));
                    };
                    {
                        let mut store = self.store.lock().unwrap();
                        if let Ok(vol) = store.volume_mut(&info) {
                            // 清空元数据、倒排与分段文件，随后重新全量扫描
                            let _ = vol.rebuild();
                            vol.state.done_subtrees.clear();
                            vol.state.files_done = 0;
                            vol.state.files_total = 0;
                            vol.state.dirs_done = 0;
                            vol.state.files_failed = 0;
                            vol.state.state = "Rebuilding".into();
                            vol.state.phase = "idle".into();
                            let _ = vol.save_state();
                        }
                        let _ = store.set_volume_state_in_meta(&id, "Rebuilding");
                    }
                    self.emit_state(&id, &info.mount, "Rebuilding", None);

                    // 2) 新代际 + 立即重扫（旧线程读到代际变化会自行退出且不写状态）
                    self.controls
                        .lock()
                        .unwrap()
                        .insert(id.clone(), self.new_control());
                    // 同 resume：自动模式下「重建」不应排队等槽位（否则跑马灯无进展）
                    let parallel = self
                        .cfg
                        .lock()
                        .unwrap()
                        .parallel_volumes(self.running.load(Ordering::SeqCst) + 1);
                    self.spawn_volume_thread(info, parallel);
                    "Rebuilding"
                }
                "clean" => {
                    // 1) 停掉扫描线程与 watcher，并等它们退出
                    {
                        let mut controls = self.controls.lock().unwrap();
                        if let Some(c) = controls.get_mut(&id) {
                            c.stop = true;
                            c.pause = false;
                        } else {
                            controls.insert(
                                id.clone(),
                                VolumeControl {
                                    epoch: next_epoch(),
                                    stop: true,
                                    pause: false,
                                },
                            );
                        }
                    }
                    self.wait_worker_exit(&id, Duration::from_secs(3));

                    // 2) 先关闭 SQLite 句柄再删目录：Windows 上句柄未关会删除失败
                    {
                        let mut store = self.store.lock().unwrap();
                        store.volumes.remove(&id);
                        let _ = store.set_volume_state_in_meta(&id, "Discovered");
                    }
                    let dir = self.root.volume_dir(&id);
                    if dir.exists() {
                        if let Err(e) = std::fs::remove_dir_all(&dir) {
                            return Err(SnapError::new(
                                ErrorCode::Internal,
                                format!("删除索引目录失败（可能仍被占用）：{e}"),
                            ));
                        }
                    }

                    // 3) 清控制条目；状态只写 meta 并发事件。
                    //    这里不能走 mark_state：它会 volume_mut 把刚删掉的索引目录重新创建出来。
                    self.controls.lock().unwrap().remove(&id);
                    let mount = self
                        .find_volume(&id)
                        .map(|v| v.mount)
                        .unwrap_or_else(|| id.clone());
                    {
                        let store = self.store.lock().unwrap();
                        let _ = store.set_volume_state_in_meta(&id, "Discovered");
                        store.log_event(Some(&id), "clean", "index_removed");
                    }
                    self.emit_state(&id, &mount, "Discovered", None);
                    "Discovered"
                }
                "unregister" => {
                    // 移除手工添加的目录（fs=="dir"）：从索引范围里删掉并清掉其索引数据。
                    // 磁盘/网络卷是扫描自动发现的，不支持移除（它们不是用户加的）。
                    let info = self.find_volume(&id).ok_or_else(|| {
                        SnapError::new(
                            ErrorCode::VolumeUnavailable,
                            format!("找不到索引范围 {id}"),
                        )
                    })?;
                    if info.fs != "dir" {
                        return Err(SnapError::new(
                            ErrorCode::Internal,
                            "本地磁盘/网络卷由扫描自动发现，不支持移除；如需清掉索引数据请使用「清理并删除索引」",
                        ));
                    }
                    let mount = info.mount.clone();

                    // 1) 停掉扫描线程与 watcher，并等它们退出（与 clean 相同）
                    {
                        let mut controls = self.controls.lock().unwrap();
                        if let Some(c) = controls.get_mut(&id) {
                            c.stop = true;
                            c.pause = false;
                        } else {
                            controls.insert(
                                id.clone(),
                                VolumeControl {
                                    epoch: next_epoch(),
                                    stop: true,
                                    pause: false,
                                },
                            );
                        }
                    }
                    self.wait_worker_exit(&id, Duration::from_secs(3));

                    // 2) 先关 SQLite 句柄再删目录：Windows 上句柄未关会删除失败
                    {
                        let mut store = self.store.lock().unwrap();
                        store.volumes.remove(&id);
                        let _ = store.delete_volume_in_meta(&id);
                    }
                    let dir = self.root.volume_dir(&id);
                    if dir.exists() {
                        if let Err(e) = std::fs::remove_dir_all(&dir) {
                            return Err(SnapError::new(
                                ErrorCode::Internal,
                                format!("删除索引目录失败（可能仍被占用）：{e}"),
                            ));
                        }
                    }

                    // 3) 从已登记卷列表移除（否则 refresh_volumes 会把它当
                    //    「枚举不到的已登记卷」再加回来），清控制条目
                    self.volumes
                        .lock()
                        .unwrap()
                        .retain(|v| v.volume_id != id);
                    self.controls.lock().unwrap().remove(&id);
                    {
                        let store = self.store.lock().unwrap();
                        store.log_event(Some(&id), "unregister", "scope_dir_removed");
                    }
                    // 事件也发 Discovered：前端刷新范围列表后该行直接消失
                    self.emit_state(&id, &mount, "Discovered", None);
                    "Discovered"
                }
                "compact" => {
                    // 整理分段：回收更新留下的垃圾块（不影响搜索可用性）
                    if let Some(info) = self.find_volume(&id) {
                        let freed = {
                            let mut store = self.store.lock().unwrap();
                            match store.volume_mut(&info) {
                                Ok(vol) => vol.compact().unwrap_or(0),
                                Err(_) => 0,
                            }
                        };
                        if freed > 0 {
                            self.store
                                .lock()
                                .unwrap()
                                .log_event(Some(&id), "compact", &format!("reclaimed_bytes={freed}"));
                        }
                    }
                    "Indexing"
                }
                "setExcludes" => {
                    // 只更新本进程内存，**不落盘**：config.json 的唯一写入方是桌面端
                    // （它先把规则写进 config.json，再发本动作同步内存）。
                    // 这里若 cfg.save()，会把索引进程启动时的旧快照整份写回，
                    // 覆盖桌面端刚改的主题/托盘/并发/索引目录等——重启后才暴露，
                    // 表现为「设置莫名其妙变回旧值」。
                    if let Some(ex) = &action.excludes {
                        self.cfg.lock().unwrap().index.exclude_rules = ex.clone();
                    }
                    "OK"
                }
                "skip" => {
                    self.mark_state(&id, "Stopped");
                    "Stopped"
                }
                other => {
                    return Err(SnapError::new(
                        ErrorCode::Internal,
                        format!("未知操作 {other}"),
                    ))
                }
            };
            results.push((id, state.to_string()));
        }
        Ok(results)
    }

    fn find_volume(&self, id: &str) -> Option<VolumeInfo> {
        self.volumes
            .lock()
            .unwrap()
            .iter()
            .find(|v| v.volume_id == id)
            .cloned()
    }

    fn mark_state(&self, id: &str, state: &str) {
        let mount = self
            .find_volume(id)
            .map(|v| v.mount)
            .unwrap_or_else(|| id.to_string());
        {
            let info = self.find_volume(id);
            let mut store = self.store.lock().unwrap();
            if let Some(info) = info {
                if let Ok(vol) = store.volume_mut(&info) {
                    vol.state.state = state.to_string();
                    let _ = vol.save_state();
                }
            }
            let _ = store.set_volume_state_in_meta(id, state);
        }
        self.emit_state(id, &mount, state, None);
    }

    // -----------------------------------------------------------------
    // 查询
    // -----------------------------------------------------------------

    pub fn begin_query(&self, request_id: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.active
            .lock()
            .unwrap()
            .insert(request_id.to_string(), flag.clone());
        flag
    }

    pub fn end_query(&self, request_id: &str) {
        self.active.lock().unwrap().remove(request_id);
    }

    pub fn cancel_query(&self, request_id: &str) -> bool {
        let map = self.active.lock().unwrap();
        if let Some(flag) = map.get(request_id) {
            flag.store(true, Ordering::SeqCst);
            return true;
        }
        false
    }

    /// 执行查询并按批回调（流式）。批次边界检查取消标志。
    pub fn run_query(
        &self,
        req: &QueryRequest,
        cancel: &AtomicBool,
        emit: &mut dyn FnMut(QueryChunk),
    ) -> SnapResult<()> {
        let started = Instant::now();
        let outcome = {
            let mut store = self.store.lock().unwrap();
            // 查询前提交批量写事务，保证用户能搜到已扫描的内容
            store.commit_all();
            query::search(&store, req, cancel)?
        };
        let batch = self.cfg.lock().unwrap().query.batch_size.max(1);
        let total = outcome.hits.len();
        let mut sent = 0usize;
        let mut warnings = outcome.warnings.clone();

        if total == 0 {
            emit(QueryChunk {
                request_id: req.request_id.clone(),
                phase: "final".into(),
                total_estimate: outcome.total,
                scanned_segments: 1,
                total_segments: 1,
                took_ms: started.elapsed().as_millis() as u64,
                next_cursor: None,
                hits: Vec::new(),
                warnings,
            });
            return Ok(());
        }

        while sent < total {
            if cancel.load(Ordering::SeqCst) {
                emit(QueryChunk {
                    request_id: req.request_id.clone(),
                    phase: "cancelled".into(),
                    total_estimate: outcome.total,
                    scanned_segments: 1,
                    total_segments: 1,
                    took_ms: started.elapsed().as_millis() as u64,
                    next_cursor: None,
                    hits: Vec::new(),
                    warnings,
                });
                return Ok(());
            }
            let end = (sent + batch).min(total);
            let is_last = end == total;
            let chunk = outcome.hits[sent..end].to_vec();
            emit(QueryChunk {
                request_id: req.request_id.clone(),
                phase: if is_last { "final" } else { "partial" }.into(),
                total_estimate: outcome.total,
                scanned_segments: 1,
                total_segments: 1,
                took_ms: outcome.took_ms,
                next_cursor: if is_last {
                    None
                } else {
                    Some(format!("offset:{}", req.paging.offset + end))
                },
                hits: chunk,
                warnings: std::mem::take(&mut warnings),
            });
            sent = end;
        }
        Ok(())
    }

    /// 文档元信息（预览前调用；正文由 UI 进程直接分块读取，不经 IPC）。
    pub fn preview_meta(&self, volume_id: &str, doc_id: i64) -> Option<PreviewMeta> {
        let store = self.store.lock().unwrap();
        let vol = store.volume(volume_id)?;
        let row = vol
            .conn
            .query_row(
                "SELECT path,name,ext,size,mtime,encoding,flags FROM docs WHERE doc_id=?1",
                [doc_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()
            .ok()
            .flatten()?;
        let (path, name, ext, size, mtime, encoding, flags) = row;
        Some(PreviewMeta {
            doc_id,
            path,
            name,
            extractable: crate::extract::is_supported(&ext),
            ext,
            size: size as u64,
            mtime,
            encoding,
            truncated: flags & 1 != 0,
        })
    }

    /// 跨卷合并的扩展名分布（按文档数降序），供 UI 生成「类型」多选标签。
    pub fn ext_stats(&self, limit: usize) -> Vec<(String, u64)> {
        let store = self.store.lock().unwrap();
        let mut merged: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for (_, vol) in store.volumes.iter() {
            for (ext, count) in vol.ext_counts(limit).unwrap_or_default() {
                *merged.entry(ext).or_insert(0) += count;
            }
        }
        let mut list: Vec<(String, u64)> = merged.into_iter().collect();
        list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        list.truncate(limit);
        list
    }

    pub fn stats(&self) -> Stats {
        // 1 秒缓存：桌面端 100ms 轮询，没必要每次都 COUNT(*) + 遍历目录
        if let Ok(cache) = self.stats_cache.lock() {
            if let Some((at, stats)) = cache.as_ref() {
                if at.elapsed() < Duration::from_secs(1) {
                    return stats.clone();
                }
            }
        }

        let budget = self.cfg.lock().unwrap().index.memory_budget_bytes;
        // 第一步（持锁、快）：COUNT(*) 与 segments 数都是轻量 SQL/内存读；
        // 第二步（无锁、慢）：`volume_index_bytes` 要遍历索引目录树算大小，
        // 放到锁外做，避免统计 IO 阻塞扫描写入与搜索。
        let partial: Vec<_> = {
            let store = self.store.lock().unwrap();
            store
                .volumes
                .iter()
                .map(|(id, vol)| {
                    (
                        id.clone(),
                        vol.info.mount.clone(),
                        vol.state.state.clone(),
                        vol.count_docs().unwrap_or(0),
                        vol.segments_count(),
                    )
                })
                .collect()
        };
        let mut total_bytes = 0u64;
        let mut volumes = Vec::with_capacity(partial.len());
        for (id, mount, state, docs, segments) in partial {
            let bytes = crate::store::volume_index_bytes(&self.root, &id);
            total_bytes += bytes;
            volumes.push(VolumeStats {
                volume_id: id,
                mount,
                state,
                docs,
                index_bytes: bytes,
                segments,
            });
        }
        let stats = Stats {
            index_root: self.root.path.to_string_lossy().to_string(),
            backend: crate::store::BACKEND_NAME.to_string(),
            total_docs: volumes.iter().map(|v| v.docs).sum(),
            total_index_bytes: total_bytes,
            rss_bytes: rss_bytes(),
            memory_budget_bytes: budget,
            volumes,
        };
        if let Ok(mut cache) = self.stats_cache.lock() {
            *cache = Some((Instant::now(), stats.clone()));
        }
        stats
    }

    // -----------------------------------------------------------------
    // 进度事件（每 200ms 一次）
    // -----------------------------------------------------------------

    fn spawn_progress_ticker(self: &Arc<Self>) {
        let me = self.clone_ref();
        let _ = std::thread::Builder::new()
            .name("progress-ticker".into())
            .spawn(move || {
                let mut last: HashMap<String, (u64, Instant)> = HashMap::new();
                let mut last_level = 0u8;
                loop {
                    if me.is_shutting_down() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                    let budget = me.cfg.lock().unwrap().index.memory_budget_bytes;
                    // 内存分级：采样 RSS 并更新档位，档位变化时通知前端
                    let level = me.governor.refresh_level();
                    if level != last_level {
                        last_level = level;
                        let reason = match level {
                            0 => "内存压力解除，恢复完整正文抽取".to_string(),
                            1 => format!(
                                "内存接近预算，正文上限降至 {} KiB",
                                crate::governor::LEVEL1_CONTENT_CAP / 1024
                            ),
                            _ => "内存达到预算，暂降级为仅索引文件名/路径".to_string(),
                        };
                        (me.sink)(ServiceEvent::Degraded { reason });
                    }
                    let throttle = me.governor.throttle_name();
                    let rate = me.governor.observed_bytes_per_sec();
                    let snapshot: Vec<(String, String, VolumeState, Option<Arc<LiveCounters>>)> = {
                        let store = me.store.lock().unwrap();
                        let live = me.live.lock().unwrap();
                        store
                            .volumes
                            .values()
                            .map(|v| {
                                (
                                    v.info.volume_id.clone(),
                                    v.info.mount.clone(),
                                    v.state.clone(),
                                    live.get(&v.info.volume_id).cloned(),
                                )
                            })
                            .collect()
                    };
                    for (id, mount, state, live) in snapshot {
                        if state.state == "Discovered" || state.state == "Stopped" {
                            continue;
                        }
                        // 有实时计数就用它（扫描线程每处理一个文件刷新一次），
                        // 否则退回检查点快照（已完成/暂停的卷只有快照）
                        let (files_done, files_total, bytes_done, dirs_done, files_failed) =
                            match &live {
                                Some(l) => (
                                    l.files_done.load(Ordering::Relaxed),
                                    l.files_total.load(Ordering::Relaxed),
                                    l.bytes_done.load(Ordering::Relaxed),
                                    l.dirs_done.load(Ordering::Relaxed),
                                    l.files_failed.load(Ordering::Relaxed),
                                ),
                                None => (
                                    state.files_done,
                                    state.files_total,
                                    state.bytes_done,
                                    state.dirs_done,
                                    state.files_failed,
                                ),
                            };
                        // 子树进度：运行中取实时值；线程退出后（Watching/Paused）
                        // 用持久化在 state.json 里的值（完成时两者相等 = 100%）
                        let (subs_done, subs_total) = match &live {
                            Some(l) => (
                                l.subs_done.load(Ordering::Relaxed),
                                l.subs_total.load(Ordering::Relaxed),
                            ),
                            None => (state.subs_done, state.subs_total),
                        };
                        let now = Instant::now();
                        let fps = match last.get(&id) {
                            Some((done, at)) => {
                                let dt = now.duration_since(*at).as_secs_f64();
                                if dt > 0.2 {
                                    ((files_done.saturating_sub(*done)) as f64 / dt) as u64
                                } else {
                                    0
                                }
                            }
                            None => 0,
                        };
                        last.insert(id.clone(), (files_done, now));
                        // ETA 只在「已发现 > 已处理」时有意义：滚动累加的 total 追平后
                        // 剩余数恒为 0，算出来的 0 秒会误导「马上完成」
                        let eta = if fps > 0 && files_total > files_done {
                            Some((files_total - files_done) / fps)
                        } else {
                            None
                        };
                        (me.sink)(ServiceEvent::Progress(Progress {
                            volume_id: id.clone(),
                            mount,
                            state: state.state.clone(),
                            phase: state.phase.clone(),
                            files_total,
                            files_done,
                            bytes_done,
                            dirs_done,
                            files_failed,
                            subs_done,
                            subs_total,
                            failed_samples: Vec::new(),
                            throughput: Throughput {
                                files_per_sec: fps,
                                bytes_per_sec: rate,
                            },
                            eta_sec: eta,
                            checkpoint: state.checkpoint.clone(),
                            memory: MemoryInfo {
                                rss_bytes: rss_bytes(),
                                budget_bytes: budget,
                                pressure: level > 0,
                            },
                            io: IoInfo {
                                throttle: throttle.clone(),
                                actual_bytes_per_sec: rate,
                            },
                        }));
                    }
                }
            });
    }

    // -----------------------------------------------------------------
    // 关闭：落盘检查点 → 关闭索引文件（文档 §4.8）
    // -----------------------------------------------------------------

    pub fn shutdown(&self, timeout: Duration) -> Vec<VolumeStateEvent> {
        self.shutdown.store(true, Ordering::SeqCst);
        {
            let mut controls = self.controls.lock().unwrap();
            for c in controls.values_mut() {
                c.stop = true;
            }
        }
        let deadline = Instant::now() + timeout;
        while self.running.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }

        let mut out = Vec::new();
        let mut ids = Vec::new();
        {
            let mut store = self.store.lock().unwrap();
            for (id, vol) in store.volumes.iter_mut() {
                // 未完成的任务必须原样保留，否则下次启动无从续跑（Stopped 是终态，不会恢复）：
                // * Scanning/Indexing：正常续扫；
                // * Rebuilding：重建被中途打断，索引数据已清空，必须重启从零扫；
                // * Paused：用户主动暂停，保留暂停态和断点，启动后不自动续跑。
                let (keep, meta_state) = match vol.state.state.as_str() {
                    "Scanning" | "Indexing" => (vol.state.state.clone(), "Indexing"),
                    "Rebuilding" => (vol.state.state.clone(), "Rebuilding"),
                    "Paused" => (vol.state.state.clone(), "Paused"),
                    // 已完成索引（增量监控中）的卷必须保持 Watching：写成 Stopped
                    // （终态）的话，下次启动 restore_watchers 不会恢复监听，
                    // 表现为「重启后显示已停止、新文件不再被索引」
                    "Watching" => ("Watching".to_string(), "Watching"),
                    _ => ("Stopped".to_string(), "Stopped"),
                };
                vol.state.state = keep;
                let next_cp_id = vol.state.checkpoint.as_ref().map(|c| c.id + 1).unwrap_or(1);
                vol.state.checkpoint = Some(CheckpointInfo {
                    id: next_cp_id,
                    at: now_secs(),
                    cursor_path: vol.info.mount.clone(),
                    resumable: true,
                });
                let _ = vol.save_state();
                vol.close();
                out.push(VolumeStateEvent {
                    volume_id: id.clone(),
                    mount: vol.info.mount.clone(),
                    state: "Stopped".into(),
                    reason: Some("shutdown".into()),
                    files_done: vol.state.files_done,
                });
                ids.push((id.clone(), meta_state.to_string()));
            }
            for (id, state) in &ids {
                let _ = store.set_volume_state_in_meta(id, state);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::parse_hhmm;

    /// 定时时间解析：只接受合法 `HH:MM`。
    #[test]
    fn hhmm_parsing() {
        assert_eq!(parse_hhmm("03:00"), Some((3, 0)));
        assert_eq!(parse_hhmm(" 23:59 "), Some((23, 59)));
        assert_eq!(parse_hhmm("00:00"), Some((0, 0)));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("12:60"), None);
        assert_eq!(parse_hhmm("1200"), None);
        assert_eq!(parse_hhmm(""), None);
    }
}

fn short_id(id: &str) -> String {
    id.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect()
}

fn estimate_files(mount: &str, used_bytes: u64) -> u64 {
    let root = std::path::PathBuf::from(mount);
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![root];
    // 只是个展示用估算：预算压到 50ms，避免拖慢启动/范围探测
    let deadline = Instant::now() + Duration::from_millis(50);
    while let Some(dir) = stack.pop() {
        if Instant::now() > deadline || files > 3_000 {
            break;
        }
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                if let Ok(ft) = e.file_type() {
                    if ft.is_dir() {
                        stack.push(e.path());
                    } else if ft.is_file() {
                        files += 1;
                        if let Ok(m) = e.metadata() {
                            bytes += m.len();
                        }
                    }
                }
            }
        }
    }
    if files == 0 || bytes == 0 || used_bytes == 0 {
        return files;
    }
    let ratio = (used_bytes as f64 / bytes as f64).clamp(1.0, 1000.0);
    (files as f64 * ratio) as u64
}
