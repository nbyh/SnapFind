//! 开机自启：仅写入本机用户级启动项（不联网、不写系统目录）。
//!
//! * Windows：`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下的 `SnapFind` 值，
//!   命令行带 `--minimized`，启动后直接驻留托盘；
//! * Linux：`$XDG_CONFIG_HOME/autostart/snapfind.desktop`；
//! * 其他平台：不支持，界面会隐藏相关开关。

use std::path::PathBuf;

pub fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_default()
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
    };

    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0";
    const VALUE: &str = "SnapFind\0";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    pub fn supported() -> bool {
        true
    }

    pub fn is_enabled() -> bool {
        unsafe {
            let sub = wide(RUN_KEY);
            let name = wide(VALUE);
            let mut key: HKEY = std::mem::zeroed();
            if RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_READ, &mut key) != 0 {
                return false;
            }
            let mut ty: u32 = 0;
            let mut size: u32 = 0;
            let ok = RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut ty,
                std::ptr::null_mut(),
                &mut size,
            );
            RegCloseKey(key);
            ok == 0 && size > 0
        }
    }

    pub fn set_enabled(enable: bool) -> Result<(), String> {
        if !enable {
            return unsafe {
                let sub = wide(RUN_KEY);
                let name = wide(VALUE);
                let mut key: HKEY = std::mem::zeroed();
                if RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_WRITE, &mut key) != 0 {
                    return Err("无法打开启动项注册表键".into());
                }
                let rc = RegDeleteValueW(key, name.as_ptr());
                RegCloseKey(key);
                if rc == 0 || rc == 2 {
                    Ok(())
                } else {
                    Err(format!("删除启动项失败（错误码 {rc}）"))
                }
            };
        }
        unsafe {
            let sub = wide(RUN_KEY);
            let name = wide(VALUE);
            let cmd = format!("\"{}\" --minimized\0", exe_path().display());
            let data = wide(&cmd);
            let bytes = std::slice::from_raw_parts(
                data.as_ptr() as *const u8,
                data.len() * std::mem::size_of::<u16>(),
            );
            let mut key: HKEY = std::mem::zeroed();
            let rc = RegCreateKeyW(HKEY_CURRENT_USER, sub.as_ptr(), &mut key);
            if rc != 0 {
                return Err(format!("无法打开启动项注册表键（错误码 {rc}）"));
            }
            let rc = RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                bytes.as_ptr(),
                bytes.len() as u32,
            );
            RegCloseKey(key);
            if rc == 0 {
                Ok(())
            } else {
                Err(format!("写入启动项失败（错误码 {rc}）"))
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    fn desktop_path() -> PathBuf {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".config"))
                    .unwrap_or_else(|_| PathBuf::from(".config"))
            });
        base.join("autostart").join("snapfind.desktop")
    }

    pub fn supported() -> bool {
        true
    }

    pub fn is_enabled() -> bool {
        desktop_path().exists()
    }

    pub fn set_enabled(enable: bool) -> Result<(), String> {
        let path = desktop_path();
        if !enable {
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let content = format!(
            "[Desktop Entry]\nType=Application\nName=SnapFind\nExec=\"{}\" --minimized\nHidden=false\nNoDisplay=false\nX-GNOME-Autostart-enabled=true\n",
            exe_path().display()
        );
        std::fs::write(&path, content).map_err(|e| e.to_string())
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    pub fn supported() -> bool {
        false
    }
    pub fn is_enabled() -> bool {
        false
    }
    pub fn set_enabled(_enable: bool) -> Result<(), String> {
        Err("当前平台不支持开机自启".into())
    }
}

pub fn supported() -> bool {
    imp::supported()
}

pub fn is_enabled() -> bool {
    imp::is_enabled()
}

pub fn set_enabled(enable: bool) -> Result<(), String> {
    imp::set_enabled(enable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_autostart_entry() {
        if !supported() {
            return; // 平台不支持时跳过
        }
        let before = is_enabled();
        set_enabled(true).expect("写入启动项");
        assert!(is_enabled(), "开启后应能读到启动项");
        set_enabled(false).expect("删除启动项");
        assert!(!is_enabled(), "关闭后启动项应消失");
        if before {
            let _ = set_enabled(true);
        }
    }
}
