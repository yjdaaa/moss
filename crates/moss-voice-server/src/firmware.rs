//! NV17 固件 OTA（2026-10-15 用户决定：逐台确认推送）。
//!
//! 用户在私密窗口导入自己编译的 ESP32 应用固件（`.bin`），选一台已允许的设备确认推送后，
//! 设备下次请求 OTA 时会拿到 `firmware{version, url}`；标准 xiaozhi 固件发现版本更新时自行下载并刷写。
//! 下载地址带一次推送专用的随机令牌、只对这台设备有效、到期失效。下载完成本身不代表刷写成功。
//!
//! 要不要下发、算不算已升级，优先按内容比（2026-11-13 n87）：固件 `.bin` 的应用描述里带 ELF 哈希，
//! 设备请求 OTA 时也上报自己的 ELF 哈希。两边都有时——相同即“设备上已经是这一份”；不同且版本号更新则照常下发；
//! 不同但版本号相同或更旧（同版本号重新编译、降级）则带 `force: 1` 下发，且每次用户确认只强制一次。
//! 任一边没有 ELF 哈希（旧固件）时退回只比版本号。

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
    /// 这台设备当前是否有待下载的推送。只对已允许的设备调用。
    fn offer(&self, device_id: &str, current: &DeviceFirmware<'_>) -> Option<FirmwarePush>;
    /// 按令牌取固件内容（宿主负责核对 SHA-256、到期与设备）；同时记下“已开始下载”。
    fn fetch(&self, token: &str, peer: &str) -> Option<Vec<u8>>;
    /// 设备请求 OTA 时上报的当前固件；与推送的是同一份时记为已升级。
    fn reported(&self, device_id: &str, current: &DeviceFirmware<'_>);
}

/// 设备请求 OTA 时上报的当前固件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceFirmware<'a> {
    pub version: &'a str,
    /// ELF 哈希（64 位小写十六进制）；旧固件不上报
    pub elf_sha256: Option<&'a str>,
}

/// 要下发给设备的一次推送。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwarePush {
    pub version: String,
    pub token: String,
    /// 回复里带 `force: 1`（固件不比版本号直接安装）
    pub force: bool,
}

/// 一次推送对这台设备该怎么办。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushDecision {
    /// 下发；`force` 为真时带 `force: 1`
    Send { force: bool },
    /// 设备上跑的已经是这一份（按 ELF 哈希，或两边都没有哈希时按版本号）
    AlreadyRunning,
    /// 不下发（不比设备新，或无法比较）
    Skip,
}

/// 推送的固件（版本、ELF 哈希）对上报了 `current` 的设备该怎么办。
/// `never_downloaded` 为假（这次推送已经开始下载过）时不再强制：防止刷写失败或哈希对不上时反复重刷。
pub fn decide_push(
    push_version: &str,
    push_elf: Option<&str>,
    current: &DeviceFirmware<'_>,
    never_downloaded: bool,
) -> PushDecision {
    let newer = is_newer(push_version, current.version);
    match (push_elf, current.elf_sha256) {
        (Some(push), Some(device)) if push.eq_ignore_ascii_case(device) => {
            PushDecision::AlreadyRunning
        }
        (Some(_), Some(_)) if newer => PushDecision::Send { force: false },
        (Some(_), Some(_))
            if never_downloaded
                && version_parts(push_version).is_some()
                && version_parts(current.version).is_some() =>
        {
            PushDecision::Send { force: true }
        }
        (Some(_), Some(_)) => PushDecision::Skip,
        _ if newer => PushDecision::Send { force: false },
        _ if !never_downloaded
            && version_parts(push_version).is_some()
            && version_parts(push_version) == version_parts(current.version) =>
        {
            PushDecision::AlreadyRunning
        }
        _ => PushDecision::Skip,
    }
}

/// 从 ESP-IDF 应用镜像头读出的信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppImage {
    pub version: String,
    pub project: String,
    /// 应用描述里的 ELF 哈希（构建时写入；没写或全零时为空）
    pub elf_sha256: Option<String>,
}

/// 读固件 `.bin` 开头的应用描述里的 ELF 哈希（`esp_app_desc_t.app_elf_sha256`，文件偏移 176..208）。
/// 只需要文件的前 208 字节。
pub fn image_elf_sha256(head: &[u8]) -> Option<String> {
    let bytes = head.get(176..208)?;
    if head.first() != Some(&0xE9) || bytes.iter().all(|b| *b == 0) {
        return None;
    }
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn c_string(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let text = std::str::from_utf8(&bytes[..end]).ok()?.trim().to_owned();
    (!text.is_empty() && !text.chars().any(char::is_control)).then_some(text)
}

/// 校验是 ESP32 应用固件并读出版本与项目名：镜像头魔数 `0xE9`，`esp_app_desc_t`
/// 位于偏移 32（魔数 `0xABCD5432`，版本 48..80，项目名 80..112，ELF 哈希 176..208）。不是应用固件（如整片烧录包、
/// 引导程序、分区表）一律拒绝。
pub fn parse_app_image(bytes: &[u8]) -> Result<AppImage, &'static str> {
    if bytes.len() < FIRMWARE_MIN_BYTES {
        return Err("文件太小，不是完整的固件");
    }
    if bytes.len() > FIRMWARE_MAX_BYTES {
        return Err("固件超过 16 MB");
    }
    if bytes[0] != 0xE9 {
        return Err(
            "不是 ESP32 应用固件（镜像头不对）；请选择编译输出里的应用 .bin，不要选整片烧录包",
        );
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
    Ok(AppImage {
        version,
        project,
        elf_sha256: image_elf_sha256(bytes),
    })
}

/// 数字版本（最多 5 段），用于和设备当前版本比较。
pub fn version_parts(version: &str) -> Option<Vec<u32>> {
    let version = version.trim().trim_start_matches(['v', 'V']);
    if version.is_empty() || version.len() > 32 {
        return None;
    }
    let parts = version
        .split('.')
        .map(|part| part.parse::<u32>().ok())
        .collect::<Option<Vec<_>>>()?;
    (1..=5).contains(&parts.len()).then_some(parts)
}

/// 推送前预检：xiaozhi 固件只在服务器版本更新时才升级；设备当前版本不是数字版本时不推送。
pub fn is_newer(candidate: &str, current: &str) -> bool {
    version_parts(candidate).is_some()
        && version_parts(current).is_some()
        && moss_xiaozhi_proto::is_newer_version(
            candidate.trim_start_matches(['v', 'V']),
            current.trim_start_matches(['v', 'V']),
        )
}

/// 固件文件的 SHA-256（小写十六进制）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 下载令牌：32 位十六进制。
pub fn token_ok(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/xiaozhi/firmware/{file}", get(download))
        .with_state(shared)
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
    let bytes = tokio::task::spawn_blocking(move || host.fetch(&lookup, &peer_ip))
        .await
        .ok()
        .flatten();
    match bytes {
        Some(bytes) => {
            shared.state.event(crate::EventRecord::info(
                crate::DiagCategory::Firmware,
                format!("固件下载：{} 开始下载 {} 字节", peer.ip(), bytes.len()),
            ));
            ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response()
        }
        None => {
            shared.state.event(
                crate::EventRecord::warn(
                    crate::DiagCategory::Firmware,
                    format!(
                        "固件下载被拒绝（{}）：令牌无效、已过期或推送已取消",
                        peer.ip()
                    ),
                )
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
        assert_eq!(
            parse_app_image(&image("1.8.2", "xiaozhi")).unwrap(),
            AppImage {
                version: "1.8.2".into(),
                project: "xiaozhi".into(),
                elf_sha256: None,
            }
        );
        let mut hashed = image("1.8.2", "xiaozhi");
        hashed[176..208].copy_from_slice(&[0xAB; 32]);
        assert_eq!(
            parse_app_image(&hashed).unwrap().elf_sha256,
            Some("ab".repeat(32))
        );
        assert_eq!(image_elf_sha256(&hashed[..208]), Some("ab".repeat(32)));
        assert_eq!(image_elf_sha256(&hashed[..100]), None, "头不够长读不出");
        let mut merged = image("1.8.2", "xiaozhi");
        merged[0] = 0xFF;
        assert!(
            parse_app_image(&merged).is_err(),
            "整片烧录包 / 非应用镜像要拒绝"
        );
        let mut no_desc = image("1.8.2", "xiaozhi");
        no_desc[32] = 0;
        assert!(parse_app_image(&no_desc).is_err());
        assert!(
            parse_app_image(&image("dev-build", "x")).is_err(),
            "非数字版本设备无法比较"
        );
        assert!(parse_app_image(&[0xE9; 100]).is_err(), "太小");
    }

    #[test]
    fn push_decision_prefers_content_over_version_and_forces_only_once() {
        use PushDecision::*;
        let (a, b) = ("a".repeat(64), "b".repeat(64));
        let device = |version: &'static str, elf: Option<&'static str>| DeviceFirmware {
            version,
            elf_sha256: elf,
        };
        let (a_ref, b_ref): (&'static str, &'static str) = (a.clone().leak(), b.clone().leak());
        // 两边都有 ELF 哈希：相同就是同一份，哪怕版本号写得不一样
        assert_eq!(
            decide_push("1.9.0", Some(&a), &device("1.8.0", Some(a_ref)), true),
            AlreadyRunning
        );
        // 内容不同、版本号更新：照常下发
        assert_eq!(
            decide_push("1.9.0", Some(&a), &device("1.8.0", Some(b_ref)), true),
            Send { force: false }
        );
        // 内容不同、版本号相同（重新编译）或更旧（降级）：强制下发，但这次推送只强制一次
        for current in ["1.9.0", "2.0.0"] {
            assert_eq!(
                decide_push("1.9.0", Some(&a), &device(current, Some(b_ref)), true),
                Send { force: true },
                "{current}"
            );
            assert_eq!(
                decide_push("1.9.0", Some(&a), &device(current, Some(b_ref)), false),
                Skip,
                "已经下载过一次就不再强制：{current}"
            );
        }
        // 任一边没有 ELF 哈希（旧固件）：退回只比版本号
        assert_eq!(
            decide_push("1.9.0", Some(&a), &device("1.8.0", None), true),
            Send { force: false }
        );
        assert_eq!(
            decide_push("1.9.0", Some(&a), &device("1.9.0", None), true),
            Skip,
            "旧固件未下载过时不能仅凭版本号相同算已升级"
        );
        assert_eq!(
            decide_push("1.9.0", Some(&a), &device("1.9.0", None), false),
            AlreadyRunning
        );
        assert_eq!(
            decide_push("1.9.0", None, &device("2.0.0", Some(b_ref)), true),
            Skip
        );
        assert_eq!(
            decide_push("1.9.0", Some(&a), &device("unknown", None), true),
            Skip
        );
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
