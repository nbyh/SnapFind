//! 平台适配：目录、写探针、优先级、独占锁、桌面集成、默认排除（文档 §12）。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 可执行文件所在目录（索引目录首选位置的基准）。
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn home_dir() -> PathBuf {
    #[cfg(windows)]
    {
        env_path("USERPROFILE")
            .or_else(|| env_path("HOMEDRIVE").map(|d| d.join(env_path("HOMEPATH").unwrap_or_default())))
            .unwrap_or_else(|| PathBuf::from("."))
    }
    #[cfg(unix)]
    {
        env_path("HOME").unwrap_or_else(|| PathBuf::from("."))
    }
}

/// 配置文件目录。
pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        env_path("APPDATA")
            .unwrap_or_else(|| home_dir().join("AppData/Roaming"))
            .join("SnapFind")
    }
    #[cfg(unix)]
    {
        env_path("XDG_CONFIG_HOME")
            .unwrap_or_else(|| home_dir().join(".config"))
            .join("snapfind")
    }
}

/// 用户数据目录（索引降级目标）。
pub fn data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        env_path("LOCALAPPDATA")
            .unwrap_or_else(|| home_dir().join("AppData/Local"))
            .join("SnapFind")
    }
    #[cfg(unix)]
    {
        env_path("XDG_DATA_HOME")
            .unwrap_or_else(|| home_dir().join(".local/share"))
            .join("snapfind")
    }
}

/// 缓存目录（最后兜底，可能被系统清理）。
pub fn cache_dir() -> PathBuf {
    #[cfg(windows)]
    {
        env_path("TEMP")
            .unwrap_or_else(|| data_dir().join("tmp"))
            .join("SnapFind")
    }
    #[cfg(unix)]
    {
        env_path("XDG_CACHE_HOME")
            .unwrap_or_else(|| home_dir().join(".cache"))
            .join("snapfind")
    }
}

/// 状态目录（日志）。
pub fn state_dir() -> PathBuf {
    #[cfg(windows)]
    {
        data_dir().join("logs")
    }
    #[cfg(unix)]
    {
        env_path("XDG_STATE_HOME")
            .unwrap_or_else(|| home_dir().join(".local/state"))
            .join("snapfind")
            .join("logs")
    }
}

/// 运行时目录（IPC 端点）。
pub fn runtime_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let _ = home_dir();
        data_dir().join("run")
    }
    #[cfg(unix)]
    {
        let uid = unsafe { libc::getuid() };
        env_path("XDG_RUNTIME_DIR")
            .unwrap_or_else(|| PathBuf::from(format!("/tmp/snapfind-{uid}")))
    }
}

/// IPC 端点标识：Windows 为命名管道名，Linux 为 UDS 绝对路径。
pub fn ipc_endpoint(pin: &str) -> String {
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\snapfind-{pin}")
    }
    #[cfg(unix)]
    {
        let dir = runtime_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = set_dir_mode_700(&dir);
        dir.join(format!("{pin}.sock")).to_string_lossy().to_string()
    }
}

#[cfg(unix)]
fn set_dir_mode_700(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(dir)?.permissions();
    perm.set_mode(0o700);
    std::fs::set_permissions(dir, perm)
}

/// 真实写探针：创建目录 → 写 4KB → fsync → 读回 → 删除。
/// 返回 `Err(原因码)` 表示不可写，原因码与文档 §5.5 一致。
pub fn probe_writable(dir: &Path) -> Result<(), String> {
    if let Err(e) = std::fs::create_dir_all(dir) {
        return Err(classify_io(&e));
    }
    // 探针文件名带 PID + 自增序号：多进程（GUI 与 indexd）/ 并发测试同时探测同一目录时不互相干扰
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let probe = dir.join(format!(".writetest-{pid}-{n}", pid = std::process::id()));
    let payload = vec![0x5au8; 4096];
    let write = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&probe)?;
        f.write_all(&payload)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write {
        let _ = std::fs::remove_file(&probe);
        return Err(classify_io(&e));
    }
    let ok = std::fs::read(&probe).map(|b| b == payload).unwrap_or(false);
    let _ = std::fs::remove_file(&probe);
    if ok {
        Ok(())
    } else {
        Err("IO_ERROR".into())
    }
}

fn classify_io(e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        PermissionDenied => "ACCESS_DENIED".into(),
        NotFound => "NOT_FOUND".into(),
        _ => {
            let msg = e.to_string();
            if msg.contains("os error 28") || msg.contains("space") {
                "DISK_FULL".into()
            } else if msg.to_lowercase().contains("read-only") || msg.contains("os error 30") {
                "READ_ONLY_FS".into()
            } else if msg.contains("os error 36") || msg.contains("name too long") {
                "ENAMETOOLONG".into()
            } else {
                "IO_ERROR".into()
            }
        }
    }
}

/// 后台低优先级：Windows `IDLE_PRIORITY_CLASS`，Linux `nice 19`。
pub fn set_low_priority() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, SetPriorityClass, IDLE_PRIORITY_CLASS,
        };
        SetPriorityClass(GetCurrentProcess(), IDLE_PRIORITY_CLASS);
    }
    #[cfg(unix)]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
    }
}

/// 独占锁（索引进程单实例）。返回的文件句柄需保持存活。
pub fn try_lock(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    file.try_lock()?;
    let mut f2 = &file;
    let _ = f2.write_all(
        format!(
            "pid={} build={}\n",
            std::process::id(),
            crate::BUILD_ID
        )
        .as_bytes(),
    );
    let _ = f2.flush();
    Ok(file)
}

/// 用系统默认程序打开文件。
pub fn open_file(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .spawn()?;
        Ok(())
    }
    #[cfg(unix)]
    {
        std::process::Command::new("xdg-open").arg(path).spawn()?;
        Ok(())
    }
}

/// 在文件管理器中定位文件。
pub fn reveal_path(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let arg = format!("/select,{}", path.display());
        std::process::Command::new("explorer").arg(arg).spawn()?;
        Ok(())
    }
    #[cfg(unix)]
    {
        let dir = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open").arg(dir).spawn()?;
        Ok(())
    }
}

/// 复制文本到剪贴板（仅本地）。
pub fn copy_text(text: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "clip"])
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(text.as_bytes())?;
        }
        let _ = child.wait();
        Ok(())
    }
    #[cfg(unix)]
    {
        for (bin, args) in [
            ("wl-copy", vec![]),
            ("xclip", vec!["-selection", "clipboard"]),
        ] {
            if let Ok(mut child) = std::process::Command::new(bin)
                .args(args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                if let Some(stdin) = child.stdin.as_mut() {
                    if stdin.write_all(text.as_bytes()).is_ok() {
                        let _ = child.wait();
                        return Ok(());
                    }
                }
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no clipboard tool found (install wl-clipboard or xclip)",
        ))
    }
}

/// 平台默认排除名单（文档 §5.4）。
#[cfg(test)]
mod tests {
    use super::local_hhmm;

    /// 本地时间三个分量都在合理区间（用于每日定时增量索引的触发判断）。
    #[test]
    fn local_time_is_sane() {
        let (h, m, day) = local_hhmm().expect("本地时间应可读取");
        assert!(h < 24, "hour={h}");
        assert!(m < 60, "minute={m}");
        assert!(day >= 19700101, "day={day}");
    }
}

/// 本地时间 `(时, 分, 日序号 yyyymmdd)`，用于「每日定时增量索引」。
pub fn local_hhmm() -> Option<(u32, u32, u32)> {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        let mut st: SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut st);
        Some((
            st.wHour as u32,
            st.wMinute as u32,
            st.wYear as u32 * 10000 + st.wMonth as u32 * 100 + st.wDay as u32,
        ))
    }
    #[cfg(unix)]
    unsafe {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        Some((
            tm.tm_hour as u32,
            tm.tm_min as u32,
            (tm.tm_year as u32 + 1900) * 10000 + (tm.tm_mon as u32 + 1) * 100 + tm.tm_mday as u32,
        ))
    }
    #[cfg(not(any(windows, unix)))]
    {
        None
    }
}

pub fn default_excludes() -> Vec<String> {
    #[cfg(windows)]
    {
        vec![
            r"C:\Windows\**".into(),
            r"C:\$Recycle.Bin\**".into(),
            r"C:\System Volume Information\**".into(),
            r"C:\Recovery\**".into(),
            r"**\AppData\Local\Temp\**".into(),
            r"**\AppData\Local\CrashDumps\**".into(),
            r"**\node_modules\**".into(),
            r"**\.git\objects\**".into(),
            r"**\target\debug\**".into(),
            r"**\target\release\**".into(),
            r"**\.venv\**".into(),
            r"**\__pycache__\**".into(),
            r"**\.cargo\registry\**".into(),
            r"**\.nuget\packages\**".into(),
            r"**\.gradle\caches\**".into(),
            r"**\*.vhd".into(),
            r"**\*.vhdx".into(),
            r"**\*.iso".into(),
            r"**\pagefile.sys".into(),
            r"**\hiberfil.sys".into(),
            r"**\swapfile.sys".into(),
        ]
    }
    #[cfg(unix)]
    {
        vec![
            "/proc/**".into(),
            "/sys/**".into(),
            "/dev/**".into(),
            "/run/**".into(),
            "/tmp/**".into(),
            "/var/cache/**".into(),
            "/var/lib/docker/**".into(),
            "/usr/lib/debug/**".into(),
            "**/node_modules/**".into(),
            "**/.git/objects/**".into(),
            "**/target/debug/**".into(),
            "**/target/release/**".into(),
            "**/.venv/**".into(),
            "**/__pycache__/**".into(),
            "**/.cargo/registry/**".into(),
            "**/.cache/**".into(),
            "**/*.qcow2".into(),
            "**/*.iso".into(),
        ]
    }
}
