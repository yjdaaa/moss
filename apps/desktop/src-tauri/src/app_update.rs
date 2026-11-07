//! 应用内更新（2026-11-07 用户决定：设置或启动横幅检查到新版后，可直接在软件里下载并完成更新）。
//!
//! - 版本检查仍由后端 `/api/system/check-update` 读 GitHub Releases；这里只负责“下载并安装”检查结果里的那个标签。
//! - 安装包来自固定仓库 `yjdaaa/moss` 该标签下的 `latest.json`，下载后用内置公钥校验 minisign 签名，
//!   并要求签名里记录的版本与清单一致（防止用旧版的合法签名冒充新版）。
//! - 只给受信任主窗口（桌面端 [`crate::window_is_trusted_main`]，Android 打包页 [`is_trusted_mobile_main`]）；
//!   有加工任务在下发、启动或待确认停机时拒绝，桌面安装前走与“完全退出”相同的收尾顺序（急停 → 停语音服务）再重启。
//! - Windows 按安装形态选择更新包：轻量版 / 含离线语音版；免安装便携版不支持（没有安装目录可覆盖）。
//! - macOS 用通用版 `.app` 原地替换。
//! - Android 下载 `android-aarch64` APK 到应用私有缓存目录，校验带版本绑定的 minisign 签名后，经 `FileProvider` 拉起系统安装界面由用户确认安装。

#[cfg(any(target_os = "android", test))]
use base64::Engine;
#[cfg(any(target_os = "android", test))]
use minisign_verify::{PublicKey, Signature};
use serde_json::Value;
#[cfg(any(target_os = "android", test))]
use std::path::Path;
use std::path::PathBuf;

/// 固定的发布仓库下载前缀；标签只取校验过的版本号字符，不能拼出其他地址。
const RELEASE_DOWNLOAD_PREFIX: &str = "https://github.com/yjdaaa/moss/releases/download/";

/// Android APK 最大下载上限（512 MiB），防止异常响应耗尽存储。
#[cfg(any(target_os = "android", test))]
const MAX_ANDROID_APK_BYTES: u64 = 512 * 1024 * 1024;

/// 只接受 `v1.2.3` / `1.2.3-beta.1` 这类版本标签，统一补上前缀 `v`。
pub(crate) fn normalize_release_tag(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let version = trimmed.strip_prefix(['v', 'V']).unwrap_or(trimmed);
    let (core, suffix) = match version.find(['-', '+']) {
        Some(index) => (&version[..index], &version[index + 1..]),
        None => (version, ""),
    };
    let parts: Vec<&str> = core.split('.').collect();
    let core_ok = parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.len() <= 6 && part.chars().all(|c| c.is_ascii_digit()));
    let suffix_ok = suffix.len() <= 32
        && suffix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && (version.len() == core.len() || !suffix.is_empty());
    (core_ok && suffix_ok).then(|| format!("v{version}"))
}

/// 该标签的更新清单地址。
pub(crate) fn manifest_url(tag: &str) -> String {
    format!("{RELEASE_DOWNLOAD_PREFIX}{tag}/latest.json")
}

/// Windows 安装形态：决定从清单里取哪一个更新包。
#[cfg_attr(not(windows), allow(dead_code))] // 只在 Windows 构建里使用，其他平台仅单测覆盖
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowsFlavor {
    Lite,
    OfflineVoice,
    Portable,
}

/// 按主程序所在目录判断：Tauri NSIS 安装目录里有 `uninstall.exe`；含离线语音版还带 `moss-voice-local.exe`。
#[cfg_attr(not(windows), allow(dead_code))] // 只在 Windows 构建里使用，其他平台仅单测覆盖
pub(crate) fn windows_flavor(has_uninstaller: bool, has_voice_helper: bool) -> WindowsFlavor {
    match (has_uninstaller, has_voice_helper) {
        (false, _) => WindowsFlavor::Portable,
        (true, true) => WindowsFlavor::OfflineVoice,
        (true, false) => WindowsFlavor::Lite,
    }
}

/// 内置的更新签名公钥（`updater-pubkey.txt` 里第一行非注释内容，即 `cargo tauri signer generate` 生成的 `.pub` 文件内容）。
/// 私钥由维护者在本机保存并只放进 GitHub 仓库密钥，不进仓库（2026-11-07 用户决定）。
pub(crate) fn parse_pubkey(raw: &str) -> Option<String> {
    raw.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|line| line.len() > 40 && line.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')))
        .map(str::to_owned)
}

pub(crate) fn embedded_pubkey() -> Option<String> {
    parse_pubkey(include_str!("../updater-pubkey.txt"))
}

/// 清单里的平台键（与 `.github/workflows/release.yml` 生成的 `latest.json` 一致）。
#[cfg_attr(not(windows), allow(dead_code))] // 只在 Windows 构建里使用，其他平台仅单测覆盖
pub(crate) fn windows_target(flavor: WindowsFlavor) -> Option<&'static str> {
    match flavor {
        WindowsFlavor::Lite => Some("windows-x86_64-lite"),
        WindowsFlavor::OfflineVoice => Some("windows-x86_64-offline-voice"),
        WindowsFlavor::Portable => None,
    }
}

/// 移动端打包主页来源判定：只允许 `main` 窗口的本地打包来源。
#[cfg_attr(not(mobile), allow(dead_code))]
pub(crate) fn is_trusted_mobile_main(label: &str, url: &tauri::Url) -> bool {
    if label != "main" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    (matches!(url.scheme(), "http" | "https")
        && url.host_str() == Some("tauri.localhost")
        && url.port().is_none())
        || (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
}

#[cfg_attr(not(any(desktop, target_os = "android", test)), allow(dead_code))]
fn parse_semver_tuple(raw: &str) -> Option<(u64, u64, u64, String)> {
    let clean = raw.trim().trim_start_matches(['v', 'V']);
    let (core, rest) = match clean.split_once('-') {
        Some((c, s)) => (c, s.split('+').next().unwrap_or(s)),
        None => (clean.split('+').next().unwrap_or(clean), ""),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch, rest.trim().to_owned()))
}

#[cfg_attr(not(any(desktop, target_os = "android", test)), allow(dead_code))]
pub(crate) fn versions_equal(a: &str, b: &str) -> bool {
    match (parse_semver_tuple(a), parse_semver_tuple(b)) {
        (Some(ta), Some(tb)) => ta == tb,
        _ => a.trim().trim_start_matches(['v', 'V']) == b.trim().trim_start_matches(['v', 'V']),
    }
}

#[cfg_attr(not(any(desktop, target_os = "android", test)), allow(dead_code))]
pub(crate) fn is_allowed_update_redirect_url(next: &reqwest::Url, allow_loopback: bool) -> bool {
    if !next.username().is_empty() || next.password().is_some() {
        return false;
    }
    if next.scheme() == "https" && next.port().is_none() {
        return matches!(
            next.host_str(),
            Some(
                "github.com"
                    | "objects.githubusercontent.com"
                    | "release-assets.githubusercontent.com"
                    | "github-releases.githubusercontent.com"
            )
        );
    }
    allow_loopback && next.scheme() == "http" && matches!(next.host_str(), Some("127.0.0.1" | "localhost"))
}

/// 更新下载重定向策略：最多 5 跳，仅允许 HTTPS 跳转至 GitHub 官方发布资产域名（测试回环除外），拒绝 HTTP 降级与内网 SSRF。
#[cfg_attr(not(any(desktop, target_os = "android", test)), allow(dead_code))]
pub(crate) fn update_redirect_policy(allow_loopback: bool) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 5 {
            return attempt.stop();
        }
        if is_allowed_update_redirect_url(attempt.url(), allow_loopback) {
            attempt.follow()
        } else {
            attempt.error("拒绝跳转到不受信任的更新地址")
        }
    })
}

#[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
fn cmp_prerelease(candidate: &str, current: &str) -> std::cmp::Ordering {
    match (candidate.is_empty(), current.is_empty()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => {
            let mut c_iter = candidate.split('.');
            let mut cur_iter = current.split('.');
            loop {
                match (c_iter.next(), cur_iter.next()) {
                    (None, None) => return std::cmp::Ordering::Equal,
                    (Some(_), None) => return std::cmp::Ordering::Greater,
                    (None, Some(_)) => return std::cmp::Ordering::Less,
                    (Some(a), Some(b)) => {
                        let ord = match (a.parse::<u64>(), b.parse::<u64>()) {
                            (Ok(na), Ok(nb)) => na.cmp(&nb),
                            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                            (Err(_), Err(_)) => a.cmp(b),
                        };
                        if ord != std::cmp::Ordering::Equal {
                            return ord;
                        }
                    }
                }
            }
        }
    }
}

/// 判断 `candidate` 是否比 `current` 更新。
#[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
pub(crate) fn is_newer_semver(candidate: &str, current: &str) -> bool {
    let Some((c_maj, c_min, c_pat, c_pre)) = parse_semver_tuple(candidate) else {
        return false;
    };
    let Some((cur_maj, cur_min, cur_pat, cur_pre)) = parse_semver_tuple(current) else {
        return false;
    };
    match (c_maj, c_min, c_pat).cmp(&(cur_maj, cur_min, cur_pat)) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => cmp_prerelease(&c_pre, &cur_pre).is_gt(),
    }
}

#[cfg(any(target_os = "android", test))]
fn verify_package_reader<R: std::io::Read>(
    reader: &mut R,
    max_bytes: u64,
    signature_b64: &str,
    pubkey_b64: &str,
    expected_version: &str,
) -> Result<u64, String> {
    let decode_b64 = |input: &str, label: &str| -> Result<String, String> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(input.trim())
            .map_err(|e| format!("{label} base64 解码失败：{e}"))?;
        String::from_utf8(raw).map_err(|e| format!("{label} 编码无效：{e}"))
    };
    let pubkey_str = decode_b64(pubkey_b64, "更新公钥")?;
    let public_key = PublicKey::decode(&pubkey_str).map_err(|e| format!("解析更新公钥失败：{e}"))?;
    let sig_str = decode_b64(signature_b64, "更新包签名")?;
    let signature = Signature::decode(&sig_str).map_err(|e| format!("解析更新包签名失败：{e}"))?;

    // 按 64 KiB 分块流式计算 Blake2b 并校验全局签名（含 trusted comment），不整包读入内存
    let mut verifier = public_key
        .verify_stream(&signature)
        .map_err(|e| format!("初始化更新包签名流式校验失败：{e}"))?;
    let mut buf = [0u8; 64 * 1024];
    let mut total_read: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("读取更新包数据失败：{e}"))?;
        if n == 0 {
            break;
        }
        total_read = total_read
            .checked_add(n as u64)
            .filter(|&len| len <= max_bytes)
            .ok_or_else(|| "更新包体积超出安全上限".to_owned())?;
        verifier.update(&buf[..n]);
    }
    verifier
        .finalize()
        .map_err(|e| format!("更新包签名校验未通过：{e}"))?;

    let signed_version = signature
        .trusted_comment()
        .split('\t')
        .find_map(|field| field.strip_prefix("version:"))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "更新包签名缺少版本绑定（requireSignedVersion）".to_owned())?;

    if !versions_equal(signed_version, expected_version) {
        return Err(format!(
            "更新包签名版本（{signed_version}）与发布清单版本（{expected_version}）不一致"
        ));
    }
    Ok(total_read)
}

/// 校验内存切片的更新包 minisign 签名与 `version:` 绑定（供单测核对）。
#[cfg(test)]
pub(crate) fn verify_package_signature(
    data: &[u8],
    signature_b64: &str,
    pubkey_b64: &str,
    expected_version: &str,
) -> Result<(), String> {
    let mut cursor = std::io::Cursor::new(data);
    verify_package_reader(
        &mut cursor,
        MAX_ANDROID_APK_BYTES,
        signature_b64,
        pubkey_b64,
        expected_version,
    )
    .map(|_| ())
}

/// 流式校验磁盘上的更新包文件，避免将整个 APK 载入内存。
#[cfg(any(target_os = "android", test))]
pub(crate) fn verify_package_file(
    path: &Path,
    signature_b64: &str,
    pubkey_b64: &str,
    expected_version: &str,
) -> Result<u64, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("读取更新包元数据失败：{e}"))?;
    if !meta.file_type().is_file() {
        return Err("更新包不是普通文件".into());
    }
    if meta.len() > MAX_ANDROID_APK_BYTES {
        return Err("更新包体积超出安全上限".into());
    }
    let mut file = std::fs::File::open(path).map_err(|e| format!("打开更新包文件失败：{e}"))?;
    verify_package_reader(
        &mut file,
        MAX_ANDROID_APK_BYTES,
        signature_b64,
        pubkey_b64,
        expected_version,
    )
}

/// 下载进度快照，页面轮询 [`app_update_status`] 读取。
#[derive(Debug, Clone, Default)]
pub(crate) struct Progress {
    /// `idle` / `checking` / `downloading` / `installing` / `restarting` / `installer_launched` / `error`
    pub phase: &'static str,
    pub version: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub error: String,
}

impl Progress {
    fn to_json(&self) -> Value {
        serde_json::json!({
            "phase": if self.phase.is_empty() { "idle" } else { self.phase },
            "version": self.version,
            "downloaded": self.downloaded,
            "total": self.total,
            "error": self.error,
        })
    }

    fn busy(&self) -> bool {
        matches!(self.phase, "checking" | "downloading" | "installing" | "restarting")
    }
}

pub struct AppUpdateState {
    host: Option<std::sync::Arc<laser_host::HostApplication>>,
    #[cfg_attr(desktop, allow(dead_code))]
    cache_dir: Option<PathBuf>,
    progress: std::sync::Arc<std::sync::Mutex<Progress>>,
}

pub fn init(
    host: Option<std::sync::Arc<laser_host::HostApplication>>,
    cache_dir: Option<PathBuf>,
) -> AppUpdateState {
    AppUpdateState {
        host,
        cache_dir,
        progress: Default::default(),
    }
}

fn require_main(window: &tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(desktop)]
    {
        if crate::window_is_trusted_main(window) {
            Ok(())
        } else {
            Err("应用内更新只能在 MOSS 主窗口操作".into())
        }
    }
    #[cfg(mobile)]
    {
        let url = window.url().map_err(|_| "无法校验当前页面来源".to_owned())?;
        if is_trusted_mobile_main(window.label(), &url) {
            Ok(())
        } else {
            Err("应用内更新只能在 MOSS 手机端主界面操作".into())
        }
    }
}

fn current_platform_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(target_os = "android")]
    {
        "android"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "android")))]
    {
        "other"
    }
}

/// 当前安装形态对应的清单平台键；`Ok(None)` 表示用桌面插件默认的平台键（macOS）。
fn update_target() -> Result<Option<&'static str>, String> {
    if embedded_pubkey().is_none() {
        return Err("此版本没有内置更新签名公钥，暂不支持应用内更新，请到 GitHub Releases 手动下载".to_owned());
    }
    #[cfg(target_os = "windows")]
    {
        let exe = std::env::current_exe().map_err(|e| format!("无法定位 MOSS 主程序：{e}"))?;
        let dir = exe.parent().ok_or("无法定位 MOSS 安装目录")?;
        let flavor = windows_flavor(
            dir.join("uninstall.exe").is_file(),
            dir.join("moss-voice-local.exe").is_file(),
        );
        windows_target(flavor)
            .map(Some)
            .ok_or_else(|| "免安装便携版不支持应用内更新，请到 GitHub Releases 下载新的便携版替换".to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        Ok(None)
    }
    #[cfg(target_os = "android")]
    {
        Ok(Some("android-aarch64"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "android")))]
    {
        Err("当前系统没有发布应用内更新包，请到 GitHub Releases 手动下载".to_owned())
    }
}

#[tauri::command]
pub async fn app_update_status(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppUpdateState>,
) -> Result<Value, String> {
    require_main(&window)?;
    let progress = state.progress.lock().map_err(|_| "更新状态不可用")?;
    let mut value = progress.to_json();
    let target = update_target();
    value["supported"] = Value::Bool(target.is_ok());
    value["platform"] = Value::String(current_platform_name().to_owned());
    if let Err(reason) = target {
        value["unsupported_reason"] = Value::String(reason);
    }
    Ok(value)
}

/// Android / 单元测试共用的清单解析、流式落盘与流式签名校验逻辑。
#[cfg(any(target_os = "android", test))]
pub(crate) fn download_and_stage_android_apk(
    manifest_endpoint: &str,
    allowed_download_prefix: &str,
    current_version: &str,
    pubkey: &str,
    cache_root: &Path,
    tag: &str,
    progress: &std::sync::Mutex<Progress>,
) -> Result<PathBuf, String> {
    use std::io::{Read, Write};

    let allow_loopback = cfg!(test)
        && (manifest_endpoint.starts_with("http://127.0.0.1:")
            || manifest_endpoint.starts_with("http://localhost:"));
    let client = reqwest::blocking::Client::builder()
        .redirect(update_redirect_policy(allow_loopback))
        .timeout(std::time::Duration::from_secs(600))
        .connect_timeout(std::time::Duration::from_secs(15))
        .user_agent(format!("MOSS-Android/{current_version}"))
        .build()
        .map_err(|e| format!("初始化更新下载客户端失败：{e}"))?;

    let resp = client
        .get(manifest_endpoint)
        .send()
        .map_err(|e| format!("获取更新清单失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "{tag} 没有可用的应用内更新清单（HTTP {}）。较早版本请前往 GitHub Releases 手动下载 APK",
            resp.status().as_u16()
        ));
    }
    let manifest: Value = resp.json().map_err(|e| format!("解析更新清单失败：{e}"))?;
    let remote_version = manifest
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "更新清单缺少 version 字段".to_owned())?;
    let expected_version = tag.trim_start_matches(['v', 'V']);
    if !versions_equal(remote_version, expected_version) {
        return Err(format!(
            "更新清单版本（{remote_version}）与请求标签（{expected_version}）不一致"
        ));
    }
    if !is_newer_semver(remote_version, current_version) {
        return Err("已经是这个版本或更新的版本，无需更新".into());
    }
    let entry = manifest
        .get("platforms")
        .and_then(|p| p.get("android-aarch64"))
        .ok_or_else(|| format!("{tag} 清单中没有 Android 更新包，请前往 GitHub Releases 手动下载"))?;
    let expected_asset_prefix = format!("{allowed_download_prefix}{tag}/");
    let apk_url = entry
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|u| u.starts_with(&expected_asset_prefix))
        .ok_or_else(|| "更新清单中的 Android 下载地址不受信任".to_owned())?;
    let signature = entry
        .get("signature")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "更新清单缺少 Android 签名".to_owned())?;

    let updates_dir = cache_root.join("updates");
    std::fs::create_dir_all(&updates_dir).map_err(|e| format!("创建更新缓存目录失败：{e}"))?;
    let target_apk = updates_dir.join(format!("MOSS_{tag}_android_aarch64.apk"));

    // 若上次已下载并通过流式签名校验（例如首次授权“允许安装未知应用”后返回重试），直接复用免重下且不整包读入内存
    if target_apk.exists() {
        if let Ok(len) = verify_package_file(&target_apk, signature, pubkey, remote_version) {
            if let Ok(mut p) = progress.lock() {
                p.version = remote_version.to_owned();
                p.downloaded = len;
                p.total = Some(len);
            }
            return Ok(target_apk);
        }
        let _ = std::fs::remove_file(&target_apk);
    }

    if let Ok(entries) = std::fs::read_dir(&updates_dir) {
        for item in entries.flatten() {
            let path = item.path();
            if path != target_apk
                && let Ok(ft) = item.file_type()
                && (ft.is_file() || ft.is_symlink())
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    let mut apk_resp = client
        .get(apk_url)
        .send()
        .map_err(|e| format!("下载 Android 更新包失败：{e}"))?;
    if !apk_resp.status().is_success() {
        return Err(format!("下载 Android 更新包失败（HTTP {}）", apk_resp.status().as_u16()));
    }
    let total = apk_resp.content_length();
    if total.is_some_and(|len| len > MAX_ANDROID_APK_BYTES) {
        return Err("更新包体积超出安全上限".into());
    }
    if let Ok(mut p) = progress.lock() {
        p.phase = "downloading";
        p.version = remote_version.to_owned();
        p.downloaded = 0;
        p.total = total;
    }

    let tmp_apk = updates_dir.join(format!(".MOSS_{tag}_android_aarch64.apk.tmp"));
    let write_and_verify = (|| -> Result<(), String> {
        let mut tmp_file =
            std::fs::File::create(&tmp_apk).map_err(|e| format!("创建更新包临时文件失败：{e}"))?;
        let mut written: u64 = 0;
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let n = apk_resp
                .read(&mut chunk)
                .map_err(|e| format!("读取更新包数据流失败：{e}"))?;
            if n == 0 {
                break;
            }
            written = written
                .checked_add(n as u64)
                .filter(|&len| len <= MAX_ANDROID_APK_BYTES)
                .ok_or_else(|| "更新包体积超出安全上限".to_owned())?;
            tmp_file
                .write_all(&chunk[..n])
                .map_err(|e| format!("写入更新包缓存失败：{e}"))?;
            if let Ok(mut p) = progress.lock() {
                p.downloaded = written;
            }
        }
        tmp_file
            .sync_all()
            .map_err(|e| format!("同步更新包缓存失败：{e}"))?;
        drop(tmp_file);

        verify_package_file(&tmp_apk, signature, pubkey, remote_version)?;
        std::fs::rename(&tmp_apk, &target_apk).map_err(|e| format!("保存更新包失败：{e}"))
    })();
    if let Err(err) = write_and_verify {
        let _ = std::fs::remove_file(&tmp_apk);
        return Err(err);
    }
    Ok(target_apk)
}

#[cfg(target_os = "android")]
fn launch_android_apk_installer(apk_path: &Path) -> Result<(), String> {
    use jni::objects::{JObject, JValue};
    use std::mem::ManuallyDrop;

    let ctx = ndk_context::android_context();
    let vm = ManuallyDrop::new(
        unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }
            .map_err(|e| format!("无法连接 Android 运行时：{e}"))?,
    );
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("无法挂接 Android 线程：{e}"))?;
    let context = ManuallyDrop::new(unsafe { JObject::from_raw(ctx.context().cast()) });
    if context.is_null() {
        return Err("Android 应用上下文不可用".into());
    }
    let class_loader = env
        .call_method(&*context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
        .and_then(|v| v.l())
        .map_err(|e| {
            let _ = env.exception_clear();
            format!("读取应用类加载器失败：{e}")
        })?;
    let class_name = env.new_string("com.moss.laser.ApkInstaller").map_err(|e| {
        let _ = env.exception_clear();
        format!("构造安装器类名失败：{e}")
    })?;
    let loaded_class = env
        .call_method(
            &class_loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&class_name)],
        )
        .and_then(|v| v.l())
        .map_err(|e| {
            let _ = env.exception_clear();
            format!("加载 Android 安装器失败：{e}")
        })?;
    let installer_class: jni::objects::JClass<'_> = loaded_class.into();
    let path_str = apk_path
        .to_str()
        .ok_or_else(|| "更新包路径包含无效字符".to_owned())?;
    let j_path = env.new_string(path_str).map_err(|e| {
        let _ = env.exception_clear();
        format!("构造更新包路径失败：{e}")
    })?;
    let result_obj = env
        .call_static_method(
            installer_class,
            "installApk",
            "(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;",
            &[JValue::Object(&*context), JValue::Object(&j_path)],
        )
        .and_then(|v| v.l())
        .map_err(|e| {
            let _ = env.exception_clear();
            format!("调用系统安装器失败：{e}")
        })?;
    if result_obj.is_null() {
        return Ok(());
    }
    let j_str = jni::objects::JString::from(result_obj);
    let msg: String = env
        .get_string(&j_str)
        .map_err(|e| {
            let _ = env.exception_clear();
            format!("读取安装器返回信息失败：{e}")
        })?
        .into();
    if msg.is_empty() {
        Ok(())
    } else {
        Err(msg)
    }
}

/// 下载并安装 `tag` 对应的新版本（桌面端完成后自动重启；Android 拉起系统安装界面）。立即返回，进度用 [`app_update_status`] 轮询。
#[tauri::command]
pub async fn app_update_install(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppUpdateState>,
    tag: String,
) -> Result<Value, String> {
    require_main(&window)?;
    let tag = normalize_release_tag(&tag).ok_or("版本标签格式无效")?;
    let expected_version = tag.trim_start_matches(['v', 'V']).to_owned();
    let target = update_target()?;
    let pubkey = embedded_pubkey().ok_or("此版本没有内置更新签名公钥")?;
    if state
        .host
        .as_ref()
        .is_some_and(|host| host.api_service.device_work_in_progress())
    {
        return Err("设备正在加工或等待确认停机，请先完成或停止任务再更新".into());
    }
    {
        let mut progress = state.progress.lock().map_err(|_| "更新状态不可用")?;
        if progress.busy() {
            return Err("已经在更新中，请稍候".into());
        }
        *progress = Progress {
            phase: "checking",
            version: expected_version.clone(),
            ..Default::default()
        };
    }
    let progress = state.progress.clone();
    let fail = move |progress: &std::sync::Mutex<Progress>, message: String| {
        if let Ok(mut p) = progress.lock() {
            p.phase = "error";
            p.error = message;
        }
    };

    #[cfg(desktop)]
    {
        use tauri_plugin_updater::UpdaterExt;

        let url = manifest_url(&tag).parse().map_err(|_| "更新地址无效")?;
        let exit_handle = app.clone();
        let mut builder = app
            .updater_builder()
            .endpoints(vec![url])
            .map_err(|e| format!("更新地址无效：{e}"))?
            .pubkey(pubkey)
            .timeout(std::time::Duration::from_secs(600))
            .configure_client(|client| client.redirect(update_redirect_policy(false)))
            // Windows 安装器启动后插件直接结束进程：先走与“完全退出”相同的急停与停服务顺序
            .on_before_exit(move || crate::shutdown_desktop_services(&exit_handle, true));
        if let Some(target) = target {
            builder = builder.target(target);
        }
        let updater = match builder.build() {
            Ok(updater) => updater,
            Err(e) => {
                fail(&progress, format!("初始化更新失败：{e}"));
                return Err(format!("初始化更新失败：{e}"));
            }
        };

        let host = state.host.clone();
        tauri::async_runtime::spawn(async move {
            let update = match updater.check().await {
                Ok(Some(update)) => update,
                Ok(None) => return fail(&progress, "已经是这个版本或更新的版本，无需更新".into()),
                Err(e) => {
                    return fail(
                        &progress,
                        format!(
                            "{tag} 没有可用的应用内更新包（{e}）。较早的版本没有提供更新包，请前往 GitHub Releases 手动下载"
                        ),
                    );
                }
            };
            if !versions_equal(&update.version, &expected_version) {
                return fail(
                    &progress,
                    format!(
                        "更新清单版本（{}）与请求标签（{}）不一致",
                        update.version, expected_version
                    ),
                );
            }
            let expected_asset_prefix = format!("{RELEASE_DOWNLOAD_PREFIX}{tag}/");
            if !update.download_url.as_str().starts_with(&expected_asset_prefix) {
                return fail(&progress, "更新清单中的安装包下载地址不受信任".into());
            }
            if let Ok(mut p) = progress.lock() {
                p.phase = "downloading";
                p.version = update.version.clone();
            }
            let on_chunk = {
                let progress = progress.clone();
                move |chunk: usize, total: Option<u64>| {
                    if let Ok(mut p) = progress.lock() {
                        p.downloaded += chunk as u64;
                        p.total = total;
                    }
                }
            };
            let bytes = match update.download(on_chunk, || {}).await {
                Ok(bytes) => bytes,
                Err(e) => return fail(&progress, format!("下载或校验更新包失败：{e}")),
            };
            // 下载期间可能开始了新的加工任务：安装（会退出 MOSS）前再确认一次
            if host
                .as_ref()
                .is_some_and(|host| host.api_service.device_work_in_progress())
            {
                return fail(
                    &progress,
                    "下载完成，但设备正在加工，已暂停更新。完成任务后再点一次更新即可".into(),
                );
            }
            if let Ok(mut p) = progress.lock() {
                p.phase = "installing";
            }
            if let Err(e) = update.install(bytes) {
                return fail(&progress, format!("安装更新失败：{e}"));
            }
            // macOS：新 .app 已就位，按完全退出的顺序收尾后重启（Windows 不会走到这里）
            if let Ok(mut p) = progress.lock() {
                p.phase = "restarting";
            }
            crate::shutdown_desktop_services(&app, true);
            app.restart();
        });
    }

    #[cfg(target_os = "android")]
    {
        let _ = (target, expected_version);
        let cache_root = match state.cache_dir.clone() {
            Some(dir) => dir,
            None => {
                let msg = "找不到 Android 应用缓存目录，请前往 GitHub Releases 手动下载 APK".to_owned();
                fail(&progress, msg.clone());
                return Err(msg);
            }
        };
        let current_version = app.package_info().version.to_string();
        let host = state.host.clone();
        std::thread::spawn(move || {
            let endpoint = manifest_url(&tag);
            let apk_path = match download_and_stage_android_apk(
                &endpoint,
                RELEASE_DOWNLOAD_PREFIX,
                &current_version,
                &pubkey,
                &cache_root,
                &tag,
                &progress,
            ) {
                Ok(path) => path,
                Err(err) => return fail(&progress, err),
            };
            if host
                .as_ref()
                .is_some_and(|host| host.api_service.device_work_in_progress())
            {
                return fail(
                    &progress,
                    "下载完成，但设备正在加工，已暂停打开安装界面。完成任务后再点一次即可直接安装".into(),
                );
            }
            if let Ok(mut p) = progress.lock() {
                p.phase = "installing";
            }
            match launch_android_apk_installer(&apk_path) {
                Ok(()) => {
                    if let Ok(mut p) = progress.lock() {
                        p.phase = "installer_launched";
                        p.error.clear();
                    }
                }
                Err(msg) => {
                    let clean = msg.strip_prefix("PERMISSION_REQUIRED:").unwrap_or(&msg);
                    fail(&progress, clean.to_owned());
                }
            }
        });
    }

    #[cfg(all(mobile, not(target_os = "android")))]
    {
        let _ = (app, target, pubkey, expected_version);
        fail(&progress, "当前移动系统暂不支持应用内更新".into());
    }

    Ok(state.progress.lock().map(|p| p.to_json()).unwrap_or(Value::Null))
}

/// 前端调用的应用内更新命令（build.rs 登记与 capability 授权测试核对）。
#[cfg(test)]
pub const COMMANDS: [&str; 2] = ["app_update_status", "app_update_install"];

#[cfg(test)]
mod tests {
    use super::*;

    // 测试专用一次性密钥对（由 `cargo tauri signer generate` 生成，仅用于离线单测与本地回环模拟更新）
    const TEST_PUBKEY_PLAIN: &str = "untrusted comment: minisign public key: A49EF4746FA6E6D7\nRWTX5qZvdPSepDGWg2bz8n6ArQnBMwgQK1geVosorcbLzcunD+KJ+cYe";
    const TEST_SIG_PAYLOAD_V200: &str = "untrusted comment: signature from tauri secret key\nRUTX5qZvdPSepPF2zsjvP/L/TQtBLRnWRvaq+4omK1UEugtQp/USrGiI1v762sDy3id3aor7bRC9ETyeot6lMHHrWoZ5hjUM1Qc=\ntrusted comment: timestamp:1791649051\tfile:payload\tversion:2.0.0\nDDq2CLOEl5cQmC2eJRHlnZPVViOZsu1CjpPKQoei4A/XBfkDqGO0B+QMrcrHgEQ5qV3mne75tdFWhJpqUXh9AA==";

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[test]
    fn release_tag_only_accepts_version_characters() {
        assert_eq!(normalize_release_tag("v0.3.8").as_deref(), Some("v0.3.8"));
        assert_eq!(normalize_release_tag("0.4.0").as_deref(), Some("v0.4.0"));
        assert_eq!(normalize_release_tag(" V1.2.3-beta.1 ").as_deref(), Some("v1.2.3-beta.1"));
        for bad in [
            "",
            "v1.2",
            "v1.2.3.4",
            "v1.2.3-",
            "v1.2.3/../../evil",
            "v1.2.3?x=1",
            "latest",
            "v1.2.x",
            "v1.2.3-a b",
        ] {
            assert_eq!(normalize_release_tag(bad), None, "{bad}");
        }
        assert_eq!(
            manifest_url("v0.3.8"),
            "https://github.com/yjdaaa/moss/releases/download/v0.3.8/latest.json"
        );
    }

    #[test]
    fn windows_flavor_picks_matching_package() {
        assert_eq!(windows_flavor(false, false), WindowsFlavor::Portable);
        assert_eq!(windows_flavor(false, true), WindowsFlavor::Portable);
        assert_eq!(windows_flavor(true, false), WindowsFlavor::Lite);
        assert_eq!(windows_flavor(true, true), WindowsFlavor::OfflineVoice);
        assert_eq!(windows_target(WindowsFlavor::Lite), Some("windows-x86_64-lite"));
        assert_eq!(
            windows_target(WindowsFlavor::OfflineVoice),
            Some("windows-x86_64-offline-voice")
        );
        assert_eq!(windows_target(WindowsFlavor::Portable), None, "便携版没有安装目录可覆盖");
    }

    #[test]
    fn mobile_main_origin_requires_packaged_host_and_main_label() {
        assert!(is_trusted_mobile_main("main", &"http://tauri.localhost/".parse().unwrap()));
        assert!(is_trusted_mobile_main("main", &"https://tauri.localhost/index.html".parse().unwrap()));
        assert!(is_trusted_mobile_main("main", &"tauri://localhost/".parse().unwrap()));
        for (label, url) in [
            ("other", "http://tauri.localhost/"),
            ("main", "http://tauri.localhost:8080/"),
            ("main", "http://127.0.0.1:9666/"),
            ("main", "https://evil.example/"),
            ("main", "http://user@tauri.localhost/"),
        ] {
            assert!(!is_trusted_mobile_main(label, &url.parse().unwrap()), "{label} {url}");
        }
    }

    #[test]
    fn progress_reports_busy_phases() {
        let mut progress = Progress::default();
        assert_eq!(progress.to_json()["phase"], "idle");
        assert!(!progress.busy());
        for phase in ["checking", "downloading", "installing", "restarting"] {
            progress.phase = phase;
            assert!(progress.busy(), "{phase}");
        }
        progress.phase = "installer_launched";
        assert!(!progress.busy(), "Android 拉起系统安装器后允许再次点击");
        progress.phase = "error";
        assert!(!progress.busy(), "失败后允许重试");
    }

    #[test]
    fn pubkey_file_takes_first_non_comment_line() {
        let key = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEFCQ0RFRg==";
        assert_eq!(parse_pubkey(&format!("# 说明\n\n  {key}  \n")).as_deref(), Some(key));
        assert_eq!(parse_pubkey("# 只有注释\n"), None, "未填公钥时不启用应用内更新");
        assert_eq!(parse_pubkey("short"), None);
        assert_eq!(parse_pubkey(&format!("{key} trailing")), None, "公钥只含 base64 字符");
        let raw = include_str!("../updater-pubkey.txt");
        assert!(
            !raw.contains("secret key") && !raw.contains("PRIVATE"),
            "updater-pubkey.txt 只能放公钥"
        );
        let embedded = embedded_pubkey().expect("updater-pubkey.txt 应已配置正式更新公钥");
        let decoded = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(embedded)
                .expect("正式公钥应为合法 base64"),
        )
        .expect("正式公钥解码后应为 UTF-8");
        PublicKey::decode(&decoded).expect("正式公钥应为合法 minisign 公钥");
    }

    #[test]
    fn updater_config_requires_signed_version() {
        let config: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let updater = &config["plugins"]["updater"];
        assert_eq!(updater["pubkey"], "", "公钥只放 updater-pubkey.txt，配置里保持空串供插件反序列化");
        assert_eq!(updater["requireSignedVersion"], true, "签名必须带版本，防止旧版冒充新版");
        assert_eq!(updater["windows"]["installMode"], "passive");
        assert!(
            config["bundle"].get("createUpdaterArtifacts").is_none(),
            "本机快更与未配置私钥的构建不生成更新包；只在发布流水线里用覆盖配置开启"
        );
    }

    #[test]
    fn platform_name_and_redirect_allowlist_match_contract() {
        assert!(matches!(
            current_platform_name(),
            "windows" | "macos" | "android" | "other"
        ));
        #[cfg(target_os = "macos")]
        assert_eq!(current_platform_name(), "macos");
        #[cfg(target_os = "windows")]
        assert_eq!(current_platform_name(), "windows");

        for allowed in [
            "https://github.com/yjdaaa/moss/releases/download/v0.4.0/latest.json",
            "https://objects.githubusercontent.com/github-production-release-asset-123/apk?X-Amz-Signature=abc",
            "https://release-assets.githubusercontent.com/github-production-release-asset-123/apk",
            "https://github-releases.githubusercontent.com/123/apk",
        ] {
            assert!(
                is_allowed_update_redirect_url(&allowed.parse().unwrap(), false),
                "应允许 GitHub 官方 HTTPS 域名：{allowed}"
            );
        }
        for rejected in [
            "http://github.com/yjdaaa/moss/releases/download/v0.4.0/latest.json",
            "http://objects.githubusercontent.com/apk",
            "https://evil.example/apk",
            "https://githubusercontent.com.evil.example/apk",
            "https://github.com:8443/yjdaaa/moss",
            "https://user@github.com/yjdaaa/moss",
            "http://127.0.0.1:8080/apk",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            assert!(
                !is_allowed_update_redirect_url(&rejected.parse().unwrap(), false),
                "正式环境必须拒绝非受信任或降级跳转：{rejected}"
            );
        }
        assert!(is_allowed_update_redirect_url(
            &"http://127.0.0.1:18080/apk".parse().unwrap(),
            true
        ));
    }

    #[test]
    fn minisign_version_verification_and_android_loopback_staging_work() {
        let pubkey = b64(TEST_PUBKEY_PLAIN);
        let sig = b64(TEST_SIG_PAYLOAD_V200);
        verify_package_signature(b"test", &sig, &pubkey, "2.0.0").expect("合法签名与匹配版本应通过");
        verify_package_signature(b"test", &sig, &pubkey, "v2.0.0").expect("带 v 前缀同版本应通过");
        assert!(
            verify_package_signature(b"tampered", &sig, &pubkey, "2.0.0").is_err(),
            "篡改字节必须拒绝"
        );
        assert!(
            verify_package_signature(b"test", &sig, &pubkey, "2.0.1").is_err(),
            "清单虚报更高版本号配旧签名必须拒绝"
        );

        let verify_dir = tempfile::tempdir().unwrap();
        let sample_file = verify_dir.path().join("sample.apk");
        std::fs::write(&sample_file, b"test").unwrap();
        assert_eq!(
            verify_package_file(&sample_file, &sig, &pubkey, "2.0.0").expect("流式文件验签应通过"),
            4
        );
        assert!(verify_package_file(&sample_file, &sig, &pubkey, "2.0.1").is_err());

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let prefix = format!("http://{addr}/releases/download/");
        let sig_for_srv = sig.clone();
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests_srv = requests.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..5 {
                let Ok((mut stream, _)) = listener.accept() else { break };
                let mut req_buf = [0u8; 2048];
                let n = std::io::Read::read(&mut stream, &mut req_buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&req_buf[..n]);
                requests_srv.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let (status, extra_hdr, ct, body): (&str, String, &str, Vec<u8>) =
                    if req.contains("GET /releases/download/v2.0.0/latest.json") {
                        let m = serde_json::json!({
                            "version": "2.0.0",
                            "platforms": {
                                "android-aarch64": {
                                    "url": format!("http://{addr}/releases/download/v2.0.0/MOSS_2.0.0_android_aarch64.apk"),
                                    "signature": sig_for_srv,
                                }
                            }
                        });
                        ("200 OK", String::new(), "application/json", serde_json::to_vec(&m).unwrap())
                    } else if req.contains("GET /releases/download/v1.9.0/latest.json") {
                        // 清单声称的版本（2.0.0）与请求的标签（v1.9.0）不一致，必须拒绝
                        let m = serde_json::json!({
                            "version": "2.0.0",
                            "platforms": {
                                "android-aarch64": {
                                    "url": format!("http://{addr}/releases/download/v1.9.0/MOSS_2.0.0_android_aarch64.apk"),
                                    "signature": sig_for_srv,
                                }
                            }
                        });
                        ("200 OK", String::new(), "application/json", serde_json::to_vec(&m).unwrap())
                    } else if req.contains("GET /releases/download/v2.0.1/latest.json") {
                        // 跳转到不受信任外部主机必须被重定向策略拦截
                        (
                            "302 Found",
                            "Location: http://evil.invalid/latest.json\r\n".to_owned(),
                            "text/plain",
                            Vec::new(),
                        )
                    } else {
                        ("200 OK", String::new(), "application/vnd.android.package-archive", b"test".to_vec())
                    };
                let header = format!(
                    "HTTP/1.1 {status}\r\n{extra_hdr}Content-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = std::io::Write::write_all(&mut stream, header.as_bytes());
                let _ = std::io::Write::write_all(&mut stream, &body);
            }
        });

        let tmp = tempfile::tempdir().unwrap();
        let progress = std::sync::Mutex::new(Progress::default());
        let endpoint = format!("{prefix}v2.0.0/latest.json");
        let staged = download_and_stage_android_apk(
            &endpoint,
            &prefix,
            "0.3.7",
            &pubkey,
            tmp.path(),
            "v2.0.0",
            &progress,
        )
        .expect("首次下载并验签应成功");
        assert_eq!(std::fs::read(&staged).unwrap(), b"test");
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2, "清单 + APK 共 2 次请求");

        // 第二次点击（模拟用户在 Android 设置开启安装权限后返回重试）：复用已验签缓存，只请求清单不重下 APK
        let staged_again = download_and_stage_android_apk(
            &endpoint,
            &prefix,
            "0.3.7",
            &pubkey,
            tmp.path(),
            "v2.0.0",
            &progress,
        )
        .expect("缓存命中应直接通过");
        assert_eq!(staged_again, staged);
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 3, "第二次仅核对清单，不重复下载 APK");

        // 标签与清单版本不一致必须拒绝
        let mismatch = download_and_stage_android_apk(
            &format!("{prefix}v1.9.0/latest.json"),
            &prefix,
            "0.3.7",
            &pubkey,
            tmp.path(),
            "v1.9.0",
            &progress,
        )
        .expect_err("清单版本与请求标签不一致必须拒绝");
        assert!(mismatch.contains("不一致"), "{mismatch}");

        // 跳转到非白名单主机必须被重定向策略拦截
        let bad_redirect = download_and_stage_android_apk(
            &format!("{prefix}v2.0.1/latest.json"),
            &prefix,
            "0.3.7",
            &pubkey,
            tmp.path(),
            "v2.0.1",
            &progress,
        )
        .expect_err("跳转到非白名单主机必须拒绝");
        assert!(bad_redirect.contains("获取更新清单失败"), "{bad_redirect}");
        drop(server);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn desktop_updater_loopback_e2e_verifies_signature_and_replaces_macos_app_bundle() {
        use tauri_plugin_updater::UpdaterExt;

        // 只有在 cargo tauri CLI 可用时执行完整端到端签名+解压替换闭环
        let key_dir = tempfile::tempdir().unwrap();
        let key_path = key_dir.path().join("test-updater.key");
        let keygen = std::process::Command::new("cargo")
            .args([
                "tauri",
                "signer",
                "generate",
                "-w",
                key_path.to_str().unwrap(),
                "-p",
                "",
                "-f",
            ])
            .output()
            .expect("调用 cargo tauri signer generate");
        assert!(keygen.status.success(), "生成测试签名密钥失败");
        let pubkey = std::fs::read_to_string(key_dir.path().join("test-updater.key.pub"))
            .unwrap()
            .trim()
            .to_owned();

        // 1. 构造新版本 MOSS.app.tar.gz 归档并用带版本号（9.9.9）的私钥签名
        let build_dir = tempfile::tempdir().unwrap();
        let new_app = build_dir.path().join("MOSS.app/Contents/MacOS");
        std::fs::create_dir_all(&new_app).unwrap();
        let new_bin = new_app.join("moss-desktop");
        std::fs::write(&new_bin, b"#!/bin/sh\necho MOSS-9.9.9\n").unwrap();
        let tar_gz_path = build_dir.path().join("MOSS_9.9.9_macos_universal.app.tar.gz");
        {
            let file = std::fs::File::create(&tar_gz_path).unwrap();
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            tar.append_dir_all("MOSS.app", build_dir.path().join("MOSS.app")).unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        let sign = std::process::Command::new("cargo")
            .args([
                "tauri",
                "signer",
                "sign",
                "-f",
                key_path.to_str().unwrap(),
                "-p",
                "",
                "--app-version",
                "9.9.9",
                tar_gz_path.to_str().unwrap(),
            ])
            .output()
            .expect("调用 cargo tauri signer sign");
        assert!(sign.status.success(), "签名更新包失败");
        let sig = std::fs::read_to_string(format!("{}.sig", tar_gz_path.display()))
            .unwrap()
            .trim()
            .to_owned();
        let archive_bytes = std::fs::read(&tar_gz_path).unwrap();

        // 2. 在临时目录准备旧版“已安装”的 MOSS.app（0.3.0）
        let install_root = tempfile::tempdir().unwrap();
        let installed_bin = install_root.path().join("MOSS.app/Contents/MacOS/moss-desktop");
        std::fs::create_dir_all(installed_bin.parent().unwrap()).unwrap();
        std::fs::write(&installed_bin, b"OLD-0.3.0").unwrap();

        // 3. 起本地临时 HTTP 更新源提供 latest.json 与 .app.tar.gz
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let archive_srv = archive_bytes.clone();
        let sig_srv = sig.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok((mut stream, _)) = listener.accept() else { break };
                let mut buf = [0u8; 2048];
                let n = std::io::Read::read(&mut stream, &mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let (ct, body): (&str, Vec<u8>) = if req.contains("latest.json") {
                    let m = serde_json::json!({
                        "version": "9.9.9",
                        "notes": "本地模拟更新闭环实测",
                        "pub_date": "2026-11-07T06:40:00Z",
                        "platforms": {
                            "darwin-aarch64": {
                                "url": format!("http://{addr}/MOSS_9.9.9_macos_universal.app.tar.gz"),
                                "signature": sig_srv,
                            },
                            "darwin-x86_64": {
                                "url": format!("http://{addr}/MOSS_9.9.9_macos_universal.app.tar.gz"),
                                "signature": sig_srv,
                            }
                        }
                    });
                    ("application/json", serde_json::to_vec(&m).unwrap())
                } else {
                    ("application/gzip", archive_srv.clone())
                };
                let hdr = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = std::io::Write::write_all(&mut stream, hdr.as_bytes());
                let _ = std::io::Write::write_all(&mut stream, &body);
            }
        });

        // 4. 通过 tauri-plugin-updater 执行完整 check → download → 验签 → 原地覆盖 MOSS.app
        let mut ctx = tauri::test::mock_context(tauri::test::noop_assets());
        ctx.config_mut().plugins.0.insert(
            "updater".into(),
            serde_json::json!({
                "pubkey": pubkey,
                "requireSignedVersion": true,
                "dangerousInsecureTransportProtocol": true
            }),
        );
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(ctx)
            .expect("构建模拟 Tauri App");

        let updater = app
            .updater_builder()
            .endpoints(vec![format!("http://{addr}/latest.json").parse().unwrap()])
            .unwrap()
            .executable_path(&installed_bin)
            .build()
            .unwrap();

        let update = updater.check().await.expect("请求本地更新源").expect("应检测到 9.9.9 新版");
        assert_eq!(update.version, "9.9.9");
        let downloaded_total = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let dl_counter = downloaded_total.clone();
        let bytes = update
            .download(
                move |chunk, _| {
                    dl_counter.fetch_add(chunk as u64, std::sync::atomic::Ordering::SeqCst);
                },
                || {},
            )
            .await
            .expect("下载并通过带版本号的 minisign 验签");
        assert_eq!(downloaded_total.load(std::sync::atomic::Ordering::SeqCst), archive_bytes.len() as u64);

        // 篡改一个字节必须被 install 二次验签拦截
        let mut tampered = bytes.clone();
        *tampered.last_mut().unwrap() ^= 0xff;
        assert!(update.install(&tampered).is_err(), "篡改包必须拒绝安装");
        assert_eq!(std::fs::read(&installed_bin).unwrap(), b"OLD-0.3.0", "拒绝安装后原程序完好无损");

        // 合法包原地替换 MOSS.app
        update.install(bytes).expect("合法更新包原地替换 MOSS.app 应成功");
        assert_eq!(
            std::fs::read(&installed_bin).unwrap(),
            b"#!/bin/sh\necho MOSS-9.9.9\n",
            "MOSS.app 内的可执行文件已被新版本原地替换"
        );
        let _ = server.join();
    }
}
