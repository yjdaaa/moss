//! NV17 固件 OTA（2026-10-15 用户决定：逐台确认推送）。
//!
//! 用户在私密窗口导入自己编译的 ESP32 应用固件（`.bin`），选一台已允许的设备确认推送后，
//! 设备下次请求 OTA 时会拿到 `firmware{version, url}`；标准 xiaozhi 固件发现版本更新时自行下载并刷写。
//! 下载地址带一次推送专用的随机令牌、只对这台设备有效、到期失效；只有设备之后上报的版本等于推送版本，
//! 才记为“已升级”。下载完成本身不代表刷写成功。

use std::sync::Arc;

use axum::Router;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::net::SocketAddr;

use crate::Shared;

/// 单个固件文件上限（ESP32 OTA 分区远小于此）。
pub const FIRMWARE_MAX_BYTES: usize = 16 * 1024 * 1024;
const FIRMWARE_MIN_BYTES: usize = 1024;

/// 宿主（桌面端）提供的推送记录与固件文件。
pub trait FirmwareHost: Send + Sync {
    /// 这台设备当前是否有待下载的推送：返回 (固件版本, 下载令牌)。只对已允许的设备调用。
    fn offer(&self, device_id: &str, current_version: &str) -> Option<(String, String)>;
    /// 按令牌取固件内容（宿主负责核对 SHA-256、到期与设备）；同时记下“已开始下载”。
    fn fetch(&self, token: &str, peer: &str) -> Option<Vec<u8>>;
    /// 设备请求 OTA 时上报的当前版本；等于推送版本时记为已升级。
    fn reported(&self, device_id: &str, version: &str);
}

/// 从 ESP-IDF 应用镜像头读出的信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppImage {
    pub version: String,
    pub project: String,
}

fn c_string(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let text = std::str::from_utf8(&bytes[..end]).ok()?.trim().to_owned();
    (!text.is_empty() && !text.chars().any(char::is_control)).then_some(text)
}

/// 校验是 ESP32 应用固件并读出版本与项目名：镜像头魔数 `0xE9`，`esp_app_desc_t`
/// 位于偏移 32（魔数 `0xABCD5432`，版本 48..80，项目名 80..112）。不是应用固件（如整片烧录包、
/// 引导程序、分区表）一律拒绝。
pub fn parse_app_image(bytes: &[u8]) -> Result<AppImage, &'static str> {
    if bytes.len() < FIRMWARE_MIN_BYTES {
        return Err("文件太小，不是完整的固件");
    }
    if bytes.len() > FIRMWARE_MAX_BYTES {
        return Err("固件超过 16 MB");
    }
    if bytes[0] != 0xE9 {
        return Err("不是 ESP32 应用固件（镜像头不对）；请选择编译输出里的应用 .bin，不要选整片烧录包");
    }
    let magic = u32::from_le_bytes(bytes[32..36].try_into().expect("four bytes"));
    if magic != 0xABCD_5432 {
        return Err("固件里没有应用描述信息（不是应用固件）");
    }
    let version = c_string(&bytes[48..80]).ok_or("固件版本号无效")?;
    let project = c_string(&bytes[80..112]).unwrap_or_default();
    if version_parts(&version).is_none() {
        return Err("固件版本号须为 1.2.3 这样的数字版本，设备才能判断是否更新");
    }
    Ok(AppImage { version, project })
}

/// 数字版本（最多 5 段），用于和设备当前版本比较。
pub fn version_parts(version: &str) -> Option<Vec<u32>> {
    let version = version.trim().trim_start_matches(['v', 'V']);
    if version.is_empty() || version.len() > 32 {
        return None;
    }
    let parts = version.split('.').map(|part| part.parse::<u32>().ok()).collect::<Option<Vec<_>>>()?;
    (1..=5).contains(&parts.len()).then_some(parts)
}

/// 推送前预检：xiaozhi 固件只在服务器版本更新时才升级；设备当前版本不是数字版本时不推送。
pub fn is_newer(candidate: &str, current: &str) -> bool {
    version_parts(candidate).is_some() && version_parts(current).is_some()
        && moss_xiaozhi_proto::is_newer_version(candidate.trim_start_matches(['v', 'V']), current.trim_start_matches(['v', 'V']))
}

/// 固件文件的 SHA-256（小写十六进制）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// 下载令牌：32 位十六进制。
pub fn token_ok(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn router(shared: Arc<Shared>) -> Router {
    Router::new().route("/xiaozhi/firmware/{file}", get(download)).with_state(shared)
}

async fn download(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(file): Path<String>,
) -> Response {
    let token = file.strip_suffix(".bin").unwrap_or(&file).to_owned();
    let Some(host) = shared.config.firmware.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !token_ok(&token) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let peer_ip = peer.ip().to_string();
    let lookup = token.clone();
    let bytes = tokio::task::spawn_blocking(move || host.fetch(&lookup, &peer_ip)).await.ok().flatten();
    match bytes {
        Some(bytes) => {
            shared.state.event(crate::EventRecord::info(crate::DiagCategory::Firmware, format!("固件下载：{} 开始下载 {} 字节", peer.ip(), bytes.len())));
            ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response()
        }
        None => {
            shared.state.event(
                crate::EventRecord::warn(crate::DiagCategory::Firmware, format!("固件下载被拒绝（{}）：令牌无效、已过期或推送已取消", peer.ip()))
                    .hint("在“设备与工具 → 固件升级”里重新确认推送这台设备"),
            );
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(version: &str, project: &str) -> Vec<u8> {
        let mut bytes = vec![0u8; 4096];
        bytes[0] = 0xE9;
        bytes[32..36].copy_from_slice(&0xABCD_5432u32.to_le_bytes());
        bytes[48..48 + version.len()].copy_from_slice(version.as_bytes());
        bytes[80..80 + project.len()].copy_from_slice(project.as_bytes());
        bytes
    }

    #[test]
    fn app_image_header_is_required_and_version_is_read() {
        assert_eq!(parse_app_image(&image("1.8.2", "xiaozhi")).unwrap(), AppImage { version: "1.8.2".into(), project: "xiaozhi".into() });
        let mut merged = image("1.8.2", "xiaozhi");
        merged[0] = 0xFF;
        assert!(parse_app_image(&merged).is_err(), "整片烧录包 / 非应用镜像要拒绝");
        let mut no_desc = image("1.8.2", "xiaozhi");
        no_desc[32] = 0;
        assert!(parse_app_image(&no_desc).is_err());
        assert!(parse_app_image(&image("dev-build", "x")).is_err(), "非数字版本设备无法比较");
        assert!(parse_app_image(&[0xE9; 100]).is_err(), "太小");
    }

    #[test]
    fn version_comparison_matches_device_upgrade_rule() {
        assert!(is_newer("1.8.3", "1.8.2"));
        assert!(is_newer("2.0", "1.9.9"));
        assert!(!is_newer("1.8.2", "1.8.2"));
        assert!(!is_newer("1.8.2.0", "1.8.2"));
        assert!(!is_newer("1.8.1", "1.8.2"), "设备不会降级");
        assert!(!is_newer("1.8.3", "unknown"), "无法比较时不推送");
        assert!(is_newer("v1.10.0", "1.9.0"));
        assert!(token_ok(&"a".repeat(32)) && !token_ok("../etc") && !token_ok(&"g".repeat(32)));
    }
}
