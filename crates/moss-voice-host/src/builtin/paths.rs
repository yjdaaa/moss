//! 本地辅助程序与可写安装目录定位、Laya 环境变量解析、本机地址探测。

use super::*;

/// 辅助程序与 MOSS 放在同一目录。
pub(super) fn helper_path() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|dir| dir.join(HELPER_FILE))
}

/// 若 MOSS 运行在用户自定义独立目录（排除 Cargo `target/` 与 macOS `.app` 签名包目录且当前用户可写），返回该安装目录。
pub(super) fn writable_install_dir() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        return None;
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let norm = exe_dir
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    if norm.ends_with("\\target\\release")
        || norm.ends_with("\\target\\debug")
        || norm.contains("\\target\\")
        || norm.contains(".app\\contents\\macos")
    {
        return None;
    }
    let models_dir = exe_dir.join("models");
    let probe = models_dir.join(".moss-write-probe");
    if std::fs::create_dir_all(&models_dir).is_ok() && std::fs::write(&probe, b"ok").is_ok() {
        let _ = std::fs::remove_file(&probe);
        return Some(exe_dir);
    }
    None
}

pub(super) fn start_error(message: impl Into<String>) -> CenterError {
    CenterError::new("builtin_start_failed", 409, message)
}

pub(super) fn parse_abs_env_path(raw: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    let raw = raw?;
    if raw.is_empty() {
        return None;
    }
    let path = match raw.to_str() {
        Some(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return None;
            }
            std::path::PathBuf::from(trimmed)
        }
        None => std::path::PathBuf::from(raw),
    };
    path.is_absolute().then_some(path)
}

pub(super) fn parse_non_empty_env_path(
    raw: Option<&std::ffi::OsStr>,
) -> Option<std::path::PathBuf> {
    let raw = raw?;
    if raw.is_empty() {
        return None;
    }
    match raw.to_str() {
        Some(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| std::path::PathBuf::from(trimmed))
        }
        None => Some(std::path::PathBuf::from(raw)),
    }
}

pub(super) fn moss_laya_root_env() -> Option<std::path::PathBuf> {
    parse_abs_env_path(std::env::var_os("MOSS_LAYA_ROOT").as_deref())
}

pub(super) fn moss_laya_python_env() -> Option<std::path::PathBuf> {
    parse_non_empty_env_path(std::env::var_os("MOSS_LAYA_PYTHON").as_deref())
}

/// 本机真实局域网地址（跳过 Clash TUN 198.18.0.0/15、Tailscale 与 VMware 等虚拟网卡）。
pub(super) fn local_ip() -> Option<String> {
    moss_voice_server::primary_lan_ip().map(|ip| ip.to_string())
}
