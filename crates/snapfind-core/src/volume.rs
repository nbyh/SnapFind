//! 卷发现与稳定标识（文档 §5.1）。
//!
//! `volume_id` 用于索引目录分片命名：Windows 以卷 GUID 为准，Linux 以 `fsid` 为准，
//! 并对 `(稳定键 + 文件系统类型)` 取 SHA-256 前 16 位十六进制。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeInfo {
    pub volume_id: String,
    /// 挂载点（Windows 盘符根如 `C:\`；Linux 如 `/`、`/home`）。
    pub mount: String,
    pub fs: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub removable: bool,
    pub network: bool,
    /// 是否纳入索引范围（范围确认页勾选结果，默认本地固定卷为 true）。
    pub enabled: bool,
}

/// 是否为光学介质文件系统（CD/DVD/蓝光）。
/// 光驱（尤其无介质时）容量探测会长时间阻塞，且索引内容没有意义，统一排除。
pub fn is_optical_fs(fs: &str) -> bool {
    matches!(
        fs.to_ascii_lowercase().as_str(),
        "iso9660" | "udf" | "cd9660" | "cdfs" | "cdrom"
    )
}

pub fn volume_id_for(key: &str, fs: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hasher.update(b"|");
    hasher.update(fs.as_bytes());
    let digest = hasher.finalize();
    format!("vol-{}", hex16(&digest[..8]))
}

fn hex16(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(16);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 枚举当前可访问卷。默认只把「本地固定卷」标记为纳入索引。
pub fn discover_volumes() -> Vec<VolumeInfo> {
    let mut list = platform_volumes();
    list.sort_by(|a, b| a.mount.cmp(&b.mount));
    list.dedup_by(|a, b| a.volume_id == b.volume_id);
    list
}

#[cfg(windows)]
fn platform_volumes() -> Vec<VolumeInfo> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW,
        GetVolumeNameForVolumeMountPointW,
    };

    const DRIVE_UNKNOWN: u32 = 0;
    const DRIVE_NO_ROOT_DIR: u32 = 1;
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let mut out = Vec::new();
    for letter in b'A'..=b'Z' {
        let root = format!("{}:\\", letter as char);
        let wroot = wide(&root);
        // 先取盘符类型：`GetDriveTypeW` 只查本地挂载表、不产生网络 I/O。
        // 断开的网络映射盘若走 metadata/容量探测会等 SMB 超时（几十秒），必须绕开。
        // 注意：未连接的映射盘可能返回 DRIVE_UNKNOWN（而非 DRIVE_REMOTE），
        // 此前直接跳过会导致这类盘不出现在列表、无法手动勾选索引。
        let drive_type = unsafe { GetDriveTypeW(wroot.as_ptr()) };
        // 光盘/光驱（含无介质）直接排除：容量探测会卡住，索引它没有意义
        if drive_type == DRIVE_NO_ROOT_DIR || drive_type == DRIVE_CDROM {
            continue;
        }
        let removable = drive_type == DRIVE_REMOVABLE;
        // DRIVE_UNKNOWN 与 DRIVE_REMOTE 一样按「网络/未知卷」登记：只入列不探测，
        // 由用户在索引页手动勾选后纳入索引。
        let network = drive_type == DRIVE_REMOTE || drive_type == DRIVE_UNKNOWN;
        let fixed = drive_type == DRIVE_FIXED;

        if network {
            // 网络盘：只登记（供索引页勾选），不探测文件系统与容量
            out.push(VolumeInfo {
                volume_id: volume_id_for(&root, "net"),
                mount: root,
                fs: "net".to_string(),
                total_bytes: 0,
                free_bytes: 0,
                removable: false,
                network: true,
                enabled: false,
            });
            continue;
        }

        let mut fs_buf = [0u16; 64];
        let mut serial: u32 = 0;
        let mut max_component: u32 = 0;
        let mut flags: u32 = 0;
        let ok = unsafe {
            GetVolumeInformationW(
                wroot.as_ptr(),
                std::ptr::null_mut(),
                0,
                &mut serial,
                &mut max_component,
                &mut flags,
                fs_buf.as_mut_ptr(),
                fs_buf.len() as u32,
            )
        };
        let fs = if ok != 0 {
            let len = fs_buf.iter().position(|&c| c == 0).unwrap_or(0);
            String::from_utf16_lossy(&fs_buf[..len])
        } else {
            "unknown".to_string()
        };

        let mut guid_buf = [0u16; 128];
        let got = unsafe {
            GetVolumeNameForVolumeMountPointW(wroot.as_ptr(), guid_buf.as_mut_ptr(), 128)
        };
        let guid = if got != 0 {
            let len = guid_buf.iter().position(|&c| c == 0).unwrap_or(0);
            String::from_utf16_lossy(&guid_buf[..len])
        } else {
            format!("serial-{serial:08x}")
        };

        let mut free_to_caller: u64 = 0;
        let mut total: u64 = 0;
        let mut free: u64 = 0;
        let capacity_ok = unsafe {
            GetDiskFreeSpaceExW(
                wroot.as_ptr(),
                &mut free_to_caller,
                &mut total,
                &mut free,
            )
        };
        if capacity_ok == 0 {
            total = 0;
            free = 0;
        }

        out.push(VolumeInfo {
            volume_id: volume_id_for(&guid, &fs),
            mount: root,
            fs,
            total_bytes: total,
            free_bytes: free,
            removable,
            network,
            enabled: fixed,
        });
    }
    out
}

#[cfg(unix)]
fn platform_volumes() -> Vec<VolumeInfo> {
    const VIRTUAL_FS: &[&str] = &[
        "proc", "sysfs", "devtmpfs", "devpts", "cgroup", "cgroup2", "overlay", "squashfs",
        "autofs", "debugfs", "securityfs", "pstore", "bpf", "tracefs", "configfs", "fusectl",
        "mqueue", "hugetlbfs", "ramfs", "rpc_pipefs", "nsfs", "efivarfs", "binfmt_misc",
        "fuse.lxcfs", "tmpfs",
    ];
    const NETWORK_FS: &[&str] = &["nfs", "nfs4", "cifs", "smbfs", "smb3", "sshfs", "9p", "ncpfs"];

    let mut out = Vec::new();
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    for line in mounts.lines() {
        let mut parts = line.split_whitespace();
        let dev = parts.next().unwrap_or("");
        let mount = parts.next().unwrap_or("");
        let fs = parts.next().unwrap_or("");
        if mount.is_empty() || fs.is_empty() {
            continue;
        }
        if VIRTUAL_FS.contains(&fs) || fs.starts_with("fuse.") || is_optical_fs(fs) {
            continue;
        }
        let network = NETWORK_FS.contains(&fs);
        // 网络挂载不做 statvfs / fsid 探测：断链的 NFS/CIFS 上这些调用会长时间阻塞
        let (total, free) = if network {
            (0, 0)
        } else {
            statvfs(mount).unwrap_or((0, 0))
        };
        let key = if network {
            format!("mount:{}", mount)
        } else {
            fsid_key(mount)
        };
        // key 用原始转义串保持 volume_id 稳定；展示用挂载点做反转义
        out.push(VolumeInfo {
            volume_id: volume_id_for(&format!("{key}:{dev}"), fs),
            mount: decode_mount_escapes(mount),
            fs: fs.to_string(),
            total_bytes: total,
            free_bytes: free,
            removable: mount.starts_with("/media/") || mount.starts_with("/run/media/"),
            network,
            enabled: !network && mount == "/",
        });
    }
    out
}

/// 还原 `/proc/self/mounts` 中的八进制转义（`\040` 空格、`\011` 制表符、`\134` 反斜杠等）。
#[cfg(unix)]
fn decode_mount_escapes(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let d1 = (bytes[i + 1] as char).to_digit(8);
            let d2 = (bytes[i + 2] as char).to_digit(8);
            let d3 = (bytes[i + 3] as char).to_digit(8);
            if let (Some(a), Some(b), Some(c)) = (d1, d2, d3) {
                out.push((a * 64 + b * 8 + c) as u8);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(unix)]
fn fsid_key(mount: &str) -> String {
    let c = std::ffi::CString::new(mount).unwrap_or_default();
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return format!("mount:{}", mount);
    }
    format!("fsid:{}:{}", st.f_fsid, st.f_bsize)
}

#[cfg(unix)]
fn statvfs(mount: &str) -> Option<(u64, u64)> {
    let c = std::ffi::CString::new(mount).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }
    let total = (st.f_blocks as u64).saturating_mul(st.f_frsize as u64);
    let free = (st.f_bavail as u64).saturating_mul(st.f_frsize as u64);
    Some((total, free))
}
