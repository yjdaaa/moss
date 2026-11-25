//! `audio.cpp` 原生语音克隆推理引擎（Qwen3-TTS 1.7B Base，Apache-2.0）的跨平台预编译包定位、
//! 按需下载安装、异常孤儿进程清理，以及本地零样本声音克隆参考录音（WAV + 参考台词）管理。
//!
//! - **免 Python / 免 PyTorch**：拉取 `0xShug0/audio.cpp` 官方预编译包（Apache-2.0），经
//!   `audiocpp_server` 在本机回环 `127.0.0.1` 提供 SSE 流式合成；
//! - **跨平台硬件加速**：
//!   - macOS Apple Silicon（M1–M6）与 x64：Apple Metal 硬件加速；
//!   - Windows x64：检测到 `vulkan-1.dll` 时启用 Vulkan 显卡加速（兼容 NVIDIA / AMD / Intel 显卡），
//!     否则回退 CPU 便携构建；用户手动在 `<models>/audiocpp` 放置 CUDA 构建时优先启用 CUDA；
//!   - Linux x64：检测到 `libvulkan.so.1` 时启用 Vulkan 显卡加速，否则回退 CPU 便携构建；
//! - **本地参考音色目录**：参考录音与台词保存在 `<runtime_root>/local-tts-clone/`，每次上传或改台词
//!   递增代次号 `ref_<gen>.wav`，使常驻推理进程的 `voice_prompt_cache` 即时切换，无需重载 2.7 GB 权重。

use std::fs;
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::local_models::fetch_verified_file;
use crate::onejev::{
    RuntimeProgress, extract_tar_gz, extract_zip_with_symlinks, linux_vulkan_loader_present,
    lock_mutex_with_wait, windows_vulkan_loader_present,
};

pub const AUDIO_CPP_RELEASE_TAG: &str = "v0.9.6";
const AUDIO_GITHUB_RELEASE_BASE: &str =
    "https://github.com/0xShug0/audio.cpp/releases/download/v0.9.6";
const AUDIO_GHFAST_MIRROR_BASE: &str =
    "https://ghfast.top/https://github.com/0xShug0/audio.cpp/releases/download/v0.9.6";

pub const MAX_CLONE_REF_WAV_BYTES: usize = 7 * 1024 * 1024;
pub const MAX_CLONE_REF_BASE64_LEN: usize = 10 * 1024 * 1024;
pub const MIN_CLONE_REF_SECS: f64 = 3.0;
pub const MAX_CLONE_REF_SECS: f64 = 30.0;
pub const MIN_CLONE_REF_SAMPLE_RATE: u32 = 16_000;
pub const MAX_CLONE_TRANSCRIPT_CHARS: usize = 500;

static AUDIOCPP_INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioCppArchiveKind {
    TarGz,
    Zip,
}

#[derive(Debug, Clone, Copy)]
struct AudioCppReleaseAsset {
    platform_dir: &'static str,
    file_name: &'static str,
    sha256: &'static str,
    size: u64,
    archive: AudioCppArchiveKind,
    backend: &'static str,
    expected_accel: &'static str,
}

pub fn audiocpp_binary_name() -> &'static str {
    if cfg!(windows) {
        "audiocpp_server.exe"
    } else {
        "audiocpp_server"
    }
}

fn current_platform_asset() -> Option<AudioCppReleaseAsset> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(AudioCppReleaseAsset {
            platform_dir: "macos-arm64-metal",
            file_name: "audio-v0.9.6-bin-macos-arm64-metal.tar.gz",
            sha256: "11848bf10d1142f18ec852051ff8dcd90c12b65b95e6f902b487bae565e3dfe0",
            size: 40_423_741,
            archive: AudioCppArchiveKind::TarGz,
            backend: "metal",
            expected_accel: "Apple Metal 统一内存硬件加速",
        })
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some(AudioCppReleaseAsset {
            platform_dir: "macos-x64-metal",
            file_name: "audio-v0.9.6-bin-macos-x64-metal.tar.gz",
            sha256: "a3e082a554a0e2752aabb610dcd30971a4bd1145d5db1e6e41b9f03e26f58c82",
            size: 43_083_262,
            archive: AudioCppArchiveKind::TarGz,
            backend: "metal",
            expected_accel: "macOS Metal / CPU 原生推理",
        })
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        if windows_vulkan_loader_present() {
            Some(AudioCppReleaseAsset {
                platform_dir: "windows-x64-vulkan",
                file_name: "audio-v0.9.6-bin-windows-x64-vulkan-portable.zip",
                sha256: "8cfbd5a3a3c655dbb752972e548424d761cbd598ad7ea0903b77dbba0f202c3e",
                size: 87_016_182,
                archive: AudioCppArchiveKind::Zip,
                backend: "vulkan",
                expected_accel: "Windows Vulkan 显卡加速（兼容 NVIDIA / AMD / Intel 显卡）",
            })
        } else {
            Some(AudioCppReleaseAsset {
                platform_dir: "windows-x64-cpu",
                file_name: "audio-v0.9.6-bin-windows-x64-cpu-portable.zip",
                sha256: "5d2e7fe24dbec0f8267db00ab2c6ccdd24799d916cb7beef3168eb66a53a4077",
                size: 35_202_438,
                archive: AudioCppArchiveKind::Zip,
                backend: "cpu",
                expected_accel: "Windows x64 CPU 推理",
            })
        }
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        if linux_vulkan_loader_present() {
            Some(AudioCppReleaseAsset {
                platform_dir: "ubuntu-x64-vulkan",
                file_name: "audio-v0.9.6-bin-ubuntu-x64-vulkan-portable.tar.gz",
                sha256: "9effac5d30a24b86a5de9a9ec37a5b2db77695590cb96ae5e6a580f5588be361",
                size: 93_520_711,
                archive: AudioCppArchiveKind::TarGz,
                backend: "vulkan",
                expected_accel: "Linux Vulkan 显卡加速",
            })
        } else {
            Some(AudioCppReleaseAsset {
                platform_dir: "ubuntu-x64-cpu",
                file_name: "audio-v0.9.6-bin-ubuntu-x64-cpu-portable.tar.gz",
                sha256: "7e447eb6b6c5dffbde9c972c11ff6f4b70442d2301ada2e10e1d9c5ec9c8ac8c",
                size: 75_883_690,
                archive: AudioCppArchiveKind::TarGz,
                backend: "cpu",
                expected_accel: "Linux x64 CPU 推理",
            })
        }
    } else {
        None
    }
}

fn managed_runtime_dir(models_root: &Path, asset: &AudioCppReleaseAsset) -> PathBuf {
    models_root
        .join(".runtime")
        .join(format!("audiocpp-{AUDIO_CPP_RELEASE_TAG}"))
        .join(asset.platform_dir)
}

fn has_cuda_companion(bin: &Path) -> bool {
    bin.parent().is_some_and(|dir| {
        dir.join("ggml-cuda.dll").is_file()
            || dir.join("libggml-cuda.so").is_file()
            || dir.join("libggml-cuda.dylib").is_file()
    })
}

fn has_vulkan_companion(bin: &Path) -> bool {
    bin.parent().is_some_and(|dir| {
        dir.join("ggml-vulkan.dll").is_file()
            || dir.join("libggml-vulkan.so").is_file()
            || dir.join("libggml-vulkan.dylib").is_file()
    })
}

fn has_required_model_spec(bin: &Path) -> bool {
    bin.parent()
        .is_some_and(|dir| dir.join("model_specs").join("qwen3_tts.json").is_file())
}

pub fn has_platform_audiocpp_asset() -> bool {
    current_platform_asset().is_some()
}

pub fn resolve_audiocpp_binary(models_root: &Path) -> Option<PathBuf> {
    let bin = audiocpp_binary_name();
    if let Some(env_bin) = std::env::var_os("MOSS_AUDIOCPP_BIN").map(PathBuf::from)
        && env_bin.is_file()
    {
        return Some(env_bin);
    }
    let mut local_dirs = vec![models_root.join("audiocpp")];
    if let Some(parent) = models_root.parent() {
        local_dirs.push(parent.join("audiocpp"));
    }
    // 用户在 `<models>/audiocpp` 放置 CUDA 构建时优先启用 CUDA
    for dir in &local_dirs {
        let cand = dir.join(bin);
        if cand.is_file() && has_required_model_spec(&cand) && has_cuda_companion(&cand) {
            return Some(cand);
        }
    }
    if let Some(asset) = current_platform_asset() {
        let managed = managed_runtime_dir(models_root, &asset).join(bin);
        if managed.is_file() && has_required_model_spec(&managed) {
            return Some(managed);
        }
    }
    for dir in &local_dirs {
        let cand = dir.join(bin);
        if cand.is_file() && has_required_model_spec(&cand) {
            return Some(cand);
        }
    }
    if current_platform_asset().is_none()
        && let Some(path_env) = std::env::var_os("PATH")
    {
        for entry in std::env::split_paths(&path_env) {
            let cand = entry.join(bin);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// 按运行库文件与平台选择 `audiocpp_server --backend` 参数（支持 `MOSS_AUDIOCPP_BACKEND` 显式覆盖）。
pub fn recommended_audiocpp_backend(bin: &Path) -> &'static str {
    if let Ok(raw) = std::env::var("MOSS_AUDIOCPP_BACKEND") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "cpu" => return "cpu",
            "metal" => return "metal",
            "vulkan" => return "vulkan",
            "cuda" => return "cuda",
            _ => {}
        }
    }
    if has_cuda_companion(bin) {
        "cuda"
    } else if cfg!(target_os = "macos") {
        "metal"
    } else if has_vulkan_companion(bin)
        || (current_platform_asset().is_some_and(|a| a.backend == "vulkan"))
    {
        "vulkan"
    } else {
        "cpu"
    }
}

/// 探测本机 `audio.cpp` 运行时环境（供前端状态展示，不暴露任何本地文件系统路径）。
pub fn detect_audiocpp_env(models_root: &Path) -> Value {
    if let Some(bin) = resolve_audiocpp_binary(models_root) {
        let backend = recommended_audiocpp_backend(&bin);
        let accel = match backend {
            "cuda" => "NVIDIA CUDA 显卡加速".to_owned(),
            "metal" => "Apple Metal 硬件加速".to_owned(),
            "vulkan" => "Vulkan 显卡加速".to_owned(),
            _ => current_platform_asset()
                .map(|a| a.expected_accel.to_owned())
                .unwrap_or_else(|| "本机 audiocpp_server 推理引擎".to_owned()),
        };
        return json!({
            "ready": true,
            "installed": true,
            "binary_found": true,
            "backend": backend,
            "accelerator_label": accel,
            "summary": format!("audio.cpp 原生克隆引擎已就绪（{accel} · 免装 Python）"),
        });
    }
    if let Some(asset) = current_platform_asset() {
        let mb = (asset.size as f64) / (1024.0 * 1024.0);
        return json!({
            "ready": true,
            "installed": false,
            "binary_found": false,
            "backend": asset.backend,
            "accelerator_label": asset.expected_accel,
            "summary": format!(
                "支持开箱即用：将启用{}（下载克隆模型时自动准备约 {:.0} MB audio.cpp 推理组件，免装 Python）",
                asset.expected_accel, mb
            ),
        });
    }
    json!({
        "ready": false,
        "installed": false,
        "binary_found": false,
        "backend": "cpu",
        "accelerator_label": "需手动提供 audiocpp_server",
        "summary": "当前系统架构暂无内置预编译 audio.cpp，请编译 audio.cpp 并设置 MOSS_AUDIOCPP_BIN 指向 audiocpp_server。",
    })
}

/// 删除 MOSS 自动下载的 `audio.cpp` 运行时（`<models>/.runtime/audiocpp-*`）；用户自放的 `<models>/audiocpp` 不动。
pub fn remove_managed_runtime(models_root: &Path) -> Result<bool, String> {
    let runtime = models_root.join(".runtime");
    let Ok(entries) = fs::read_dir(&runtime) else {
        return Ok(false);
    };
    let _guard = AUDIOCPP_INSTALL_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut removed = false;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("audiocpp-")
            || !entry.file_type().is_ok_and(|ft| ft.is_dir())
        {
            continue;
        }
        fs::remove_dir_all(entry.path()).map_err(|error| {
            format!("删除 Qwen3-TTS 声音克隆推理引擎失败（可能仍在运行）：{error}")
        })?;
        removed = true;
    }
    let _ = fs::remove_dir(&runtime);
    Ok(removed)
}

/// 按需下载、SHA-256 校验并解压 `audio.cpp` 推理引擎。
pub fn ensure_audiocpp_runtime(
    models_root: &Path,
    mirror_hint: Option<&str>,
    cancel: &AtomicBool,
    progress: &dyn Fn(RuntimeProgress),
) -> Result<PathBuf, String> {
    if let Some(existing) = resolve_audiocpp_binary(models_root) {
        return Ok(existing);
    }
    let _guard = lock_mutex_with_wait(&AUDIOCPP_INSTALL_LOCK, cancel, progress)?;
    if let Some(existing) = resolve_audiocpp_binary(models_root) {
        return Ok(existing);
    }
    let asset = current_platform_asset().ok_or_else(|| {
        "当前操作系统/架构未内置预编译 audio.cpp，请编译 audio.cpp 并设置 MOSS_AUDIOCPP_BIN"
            .to_owned()
    })?;
    let target_dir = managed_runtime_dir(models_root, &asset);
    let target_bin = target_dir.join(audiocpp_binary_name());
    if target_bin.is_file() && has_required_model_spec(&target_bin) {
        return Ok(target_bin);
    }

    let staging = models_root.join(".download");
    fs::create_dir_all(&staging).map_err(|e| format!("创建运行时下载目录失败：{e}"))?;
    let part = staging.join(format!("audiocpp-runtime-{}.part", asset.file_name));
    let prefer_china_mirror = mirror_hint.is_some_and(|m| {
        m.contains("hf-mirror")
            || m.contains("ghfast")
            || m.contains("mirror")
            || m.contains("modelscope")
    });
    let bases = if prefer_china_mirror {
        [AUDIO_GHFAST_MIRROR_BASE, AUDIO_GITHUB_RELEASE_BASE]
    } else {
        [AUDIO_GITHUB_RELEASE_BASE, AUDIO_GHFAST_MIRROR_BASE]
    };

    let mut last_err = None;
    for (idx, base) in bases.iter().enumerate() {
        if idx > 0 && cancel.load(Ordering::SeqCst) {
            break;
        }
        let url = format!("{base}/{}", asset.file_name);
        let total = asset.size;
        progress(RuntimeProgress::Downloading { done: 0, total });
        match fetch_verified_file(&url, asset.sha256, asset.size, &part, cancel, &|done| {
            progress(RuntimeProgress::Downloading { done, total })
        }) {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(err) => {
                last_err = Some(err);
            }
        }
    }
    if let Some(err) = last_err {
        let _ = fs::remove_file(&part);
        return Err(format!(
            "下载 Qwen3-TTS 声音克隆推理引擎（audio.cpp）失败：{err}"
        ));
    }

    let extract_root = models_root.join(format!(".extract-audiocpp-{}", asset.platform_dir));
    let _ = fs::remove_dir_all(&extract_root);
    fs::create_dir_all(&extract_root).map_err(|e| format!("创建解压目录失败：{e}"))?;

    let unpack_res = (|| {
        match asset.archive {
            AudioCppArchiveKind::TarGz => extract_tar_gz(&part, &extract_root, cancel)?,
            AudioCppArchiveKind::Zip => {
                extract_zip_with_symlinks(&part, &extract_root, cancel, true)?
            }
        }
        let extracted_bin = extract_root.join(audiocpp_binary_name());
        if !extracted_bin.is_file() {
            return Err(format!("audio.cpp 压缩包缺少 {}", audiocpp_binary_name()));
        }
        if !has_required_model_spec(&extracted_bin) {
            return Err("audio.cpp 压缩包缺少 model_specs/qwen3_tts.json".to_owned());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&extracted_bin, fs::Permissions::from_mode(0o755));
        }
        if let Some(parent) = target_dir.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建运行时目录失败：{e}"))?;
        }
        let _ = fs::remove_dir_all(&target_dir);
        fs::rename(&extract_root, &target_dir).map_err(|e| format!("安装推理引擎失败：{e}"))?;
        Ok(())
    })();

    let _ = fs::remove_file(&part);
    let _ = fs::remove_dir_all(&extract_root);
    unpack_res?;
    Ok(target_bin)
}

#[cfg(any(test, target_os = "macos", target_os = "linux"))]
fn orphan_audiocpp_pids(ps_output: &str, roots: &[String]) -> Vec<u32> {
    if roots.is_empty() {
        return Vec::new();
    }
    let my_pid = std::process::id();
    ps_output
        .lines()
        .filter_map(|line| {
            let (pid_str, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (ppid_str, command) = rest.trim_start().split_once(char::is_whitespace)?;
            let pid: u32 = pid_str.parse().ok()?;
            let ppid: u32 = ppid_str.parse().ok()?;
            if pid == my_pid || ppid != 1 {
                return None;
            }
            let cmd = command.trim_start();
            if !cmd.contains("audiocpp_server") {
                return None;
            }
            roots
                .iter()
                .any(|r| !r.is_empty() && cmd.contains(r.as_str()))
                .then_some(pid)
        })
        .collect()
}

/// 清理上次异常退出遗留在 `<clone_root>` 下的临时服务端配置目录 `.audiocpp-srv-*` 与试听临时文件。
pub fn sweep_leftover_audiocpp_configs(clone_root: &Path) -> usize {
    let mut removed = 0;
    if let Ok(entries) = fs::read_dir(clone_root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with(".audiocpp-srv-") {
                let path = entry.path();
                let res = if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_dir()) {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                };
                if res.is_ok() {
                    removed += 1;
                }
            }
        }
    }
    if let Ok(entries) = fs::read_dir(clone_voices_dir(clone_root)) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with("preview_")
                && s.ends_with(".wav")
                && fs::remove_file(entry.path()).is_ok()
            {
                removed += 1;
            }
        }
    }
    removed
}

/// 清理上次 MOSS 异常退出遗留的孤儿 `audiocpp_server` 进程（仅限父进程已变为 1 且命令行指向 MOSS 目录的实例）。
pub fn reap_orphaned_audiocpp_servers(roots: &[PathBuf]) -> usize {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let Ok(out) = Command::new("ps")
            .args(["-axo", "pid=,ppid=,command="])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        else {
            return 0;
        };
        let root_strs: Vec<String> = roots
            .iter()
            .map(|r| r.to_string_lossy().into_owned())
            .collect();
        let pids = orphan_audiocpp_pids(&String::from_utf8_lossy(&out.stdout), &root_strs);
        let mut killed = 0;
        for pid in pids {
            let ok = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if ok {
                killed += 1;
            }
        }
        killed
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = roots;
        0
    }
}

// ---------------- 本地零样本克隆参考音色存储 ----------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CloneRefMeta {
    pub generation: u64,
    pub file_name: String,
    pub sample_rate: u32,
    pub duration_secs: f64,
    pub transcript: String,
}

#[derive(Debug, Clone)]
pub struct ValidatedCloneWav {
    /// 归一化后的标准 44 字节头 16-bit LE 单声道 PCM WAV（剥离非音频元数据块，确保 `audiocpp_server` 100% 兼容）
    pub bytes: Vec<u8>,
    pub mono_samples: Vec<i16>,
    pub file_name: String,
    pub sample_rate: u32,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloneManageError {
    InvalidInput(String),
    Io(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneVoiceBinding {
    pub voice_dir: PathBuf,
    pub voice_name: String,
    pub transcript: String,
}

pub fn clone_voices_dir(clone_root: &Path) -> PathBuf {
    clone_root.join("voices")
}

fn clone_meta_path(clone_root: &Path) -> PathBuf {
    clone_root.join("meta.json")
}

fn clone_generation_seq_path(clone_root: &Path) -> PathBuf {
    clone_root.join("generation.seq")
}

fn sanitize_file_name(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("reference.wav")
        .trim();
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        .take(80)
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "reference.wav".to_owned()
    } else if trimmed.to_ascii_lowercase().ends_with(".wav") {
        trimmed.to_owned()
    } else {
        format!("{trimmed}.wav")
    }
}

pub fn sanitize_clone_transcript(raw: &str) -> Result<String, String> {
    let normalized: String = raw
        .chars()
        .map(|c| {
            if matches!(c, '\r' | '\n' | '\t') {
                ' '
            } else {
                c
            }
        })
        .collect();
    if normalized.chars().any(char::is_control) {
        return Err("参考录音台词不能包含控制字符".into());
    }
    let collapsed = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > MAX_CLONE_TRANSCRIPT_CHARS {
        return Err(format!(
            "参考录音台词最多 {MAX_CLONE_TRANSCRIPT_CHARS} 个字"
        ));
    }
    Ok(collapsed)
}

#[derive(Debug, Clone, Copy)]
struct ParsedWavHeader {
    tag: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
    frame_bytes: usize,
    data_offset: usize,
    data_len: usize,
}

fn parse_wav_header(bytes: &[u8]) -> Result<ParsedWavHeader, String> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("参考录音不是有效的 WAV 文件（请上传标准 WAV 无损音频）".into());
    }
    let mut at = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16, usize)> = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size_u32 =
            u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]);
        let size = size_u32 as usize;
        let body = at + 8;
        if id == b"fmt " {
            let fmt_end = body.saturating_add(size).min(bytes.len());
            if fmt_end < body + 16 {
                return Err("参考 WAV 音频格式头（fmt）不完整".into());
            }
            let raw_tag = u16::from_le_bytes([bytes[body], bytes[body + 1]]);
            let channels = u16::from_le_bytes([bytes[body + 2], bytes[body + 3]]);
            let rate = u32::from_le_bytes([
                bytes[body + 4],
                bytes[body + 5],
                bytes[body + 6],
                bytes[body + 7],
            ]);
            let block_align = u16::from_le_bytes([bytes[body + 12], bytes[body + 13]]) as usize;
            let bits = u16::from_le_bytes([bytes[body + 14], bytes[body + 15]]);
            let tag = if raw_tag == 0xFFFE {
                if fmt_end < body + 40 {
                    return Err("参考 WAV 扩展格式头（WAVE_FORMAT_EXTENSIBLE）不完整".into());
                }
                const KSDATAFORMAT_SUBTYPE_TAIL: [u8; 14] = [
                    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B,
                    0x71,
                ];
                if bytes[body + 26..body + 40] != KSDATAFORMAT_SUBTYPE_TAIL {
                    return Err(
                        "参考 WAV 使用了不受支持的扩展子编码，请导出为 16/24/32 位标准 PCM WAV"
                            .into(),
                    );
                }
                u16::from_le_bytes([bytes[body + 24], bytes[body + 25]])
            } else {
                raw_tag
            };
            if channels == 0 || channels > 8 || rate == 0 {
                return Err("参考 WAV 声道数或采样率无效".into());
            }
            if !matches!((tag, bits), (1, 8 | 16 | 24 | 32) | (3, 32 | 64)) {
                return Err(format!(
                    "参考 WAV 编码不受支持（格式 {tag}，{bits} 位），请导出为 16/24/32 位 PCM WAV"
                ));
            }
            let min_frame_bytes = (channels as usize) * (bits as usize / 8);
            let frame_bytes = if block_align >= min_frame_bytes && min_frame_bytes > 0 {
                block_align
            } else {
                min_frame_bytes
            };
            if frame_bytes == 0 {
                return Err("参考 WAV 采样帧字节数无效".into());
            }
            fmt = Some((tag, channels, rate, bits, frame_bytes));
        } else if id == b"data" {
            let (tag, channels, sample_rate, bits_per_sample, frame_bytes) =
                fmt.ok_or_else(|| "参考 WAV 缺少音频格式头（fmt）".to_owned())?;
            let avail = bytes.len().saturating_sub(body);
            let data_len = if size_u32 == u32::MAX {
                avail
            } else {
                size.min(avail)
            };
            return Ok(ParsedWavHeader {
                tag,
                channels,
                sample_rate,
                bits_per_sample,
                frame_bytes,
                data_offset: body,
                data_len,
            });
        }
        at = body.saturating_add(size).saturating_add(size & 1);
    }
    Err("参考 WAV 缺少音频数据块（data）".into())
}

fn encode_pcm16_mono_wav(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_bytes = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_bytes as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36u32.saturating_add(data_bytes)).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate.saturating_mul(2)).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for &sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// 校验上传的本地参考 WAV（Base64 或 `data:audio/...;base64,...`）：≤ 7 MB、采样率 ≥ 16 kHz、时长 3 ~ 30 秒，
/// 并统一解码归一化为标准 16-bit LE 单声道 PCM WAV。
pub fn parse_clone_wav_upload(
    audio_base64_or_data_url: &str,
    raw_file_name: &str,
) -> Result<ValidatedCloneWav, String> {
    let trimmed = audio_base64_or_data_url.trim();
    if trimmed.is_empty() {
        return Err("未收到参考录音文件".into());
    }
    if trimmed.len() > MAX_CLONE_REF_BASE64_LEN {
        return Err("参考录音文件超过 7 MB 上限，请截取 5 ~ 20 秒片段后再上传".into());
    }
    let clean_b64: String = trimmed
        .split_once(',')
        .map(|(_, body)| body)
        .unwrap_or(trimmed)
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    if clean_b64.is_empty() {
        return Err("参考录音文件内容为空".into());
    }
    let raw_bytes = base64::engine::general_purpose::STANDARD
        .decode(&clean_b64)
        .map_err(|_| "参考录音 Base64 编码无效".to_owned())?;
    if raw_bytes.len() > MAX_CLONE_REF_WAV_BYTES {
        return Err("参考录音文件超过 7 MB 上限，请截取 5 ~ 20 秒片段后再上传".into());
    }
    let hdr = parse_wav_header(&raw_bytes)?;
    if hdr.sample_rate < MIN_CLONE_REF_SAMPLE_RATE || hdr.sample_rate > 192_000 {
        return Err(format!(
            "参考录音采样率需不低于 16 kHz（当前 {} Hz）",
            hdr.sample_rate
        ));
    }
    let frames = hdr.data_len / hdr.frame_bytes;
    let duration_secs = frames as f64 / hdr.sample_rate as f64;
    if !(MIN_CLONE_REF_SECS..=MAX_CLONE_REF_SECS).contains(&duration_secs) {
        return Err(format!(
            "参考录音时长需在 3 ~ 30 秒之间（当前约 {duration_secs:.1} 秒；建议截取 5 ~ 15 秒无背景音乐的清晰干声）"
        ));
    }
    let (mono_samples, sample_rate) = decode_clone_wav_to_mono_i16(&raw_bytes)?;
    let canonical_bytes = encode_pcm16_mono_wav(&mono_samples, sample_rate);
    Ok(ValidatedCloneWav {
        bytes: canonical_bytes,
        mono_samples,
        file_name: sanitize_file_name(raw_file_name),
        sample_rate,
        duration_secs,
    })
}

/// 将 8/16/24/32 位 PCM 或 32/64 位浮点 WAV 解码为单声道 `i16` PCM（供上传参考音色时自动识别参考台词）。
pub fn decode_clone_wav_to_mono_i16(bytes: &[u8]) -> Result<(Vec<i16>, u32), String> {
    let hdr = parse_wav_header(bytes)?;
    let frames = hdr.data_len / hdr.frame_bytes;
    let channels = hdr.channels as usize;
    let bps = (hdr.bits_per_sample / 8) as usize;
    let data = &bytes[hdr.data_offset..hdr.data_offset + frames * hdr.frame_bytes];
    let mut out = Vec::with_capacity(frames);
    for frame in data.chunks_exact(hdr.frame_bytes) {
        let mut acc = 0.0f64;
        for ch in 0..channels {
            let off = ch * bps;
            let sample = match (hdr.tag, hdr.bits_per_sample) {
                (1, 8) => ((frame[off] as f64) - 128.0) / 128.0,
                (1, 16) => i16::from_le_bytes([frame[off], frame[off + 1]]) as f64 / 32768.0,
                (1, 24) => {
                    let raw = (frame[off] as i32)
                        | ((frame[off + 1] as i32) << 8)
                        | (((frame[off + 2] as i8) as i32) << 16);
                    raw as f64 / 8_388_608.0
                }
                (1, 32) => {
                    i32::from_le_bytes([frame[off], frame[off + 1], frame[off + 2], frame[off + 3]])
                        as f64
                        / 2_147_483_648.0
                }
                (3, 32) => {
                    let v = f32::from_le_bytes([
                        frame[off],
                        frame[off + 1],
                        frame[off + 2],
                        frame[off + 3],
                    ]) as f64;
                    if v.is_finite() { v } else { 0.0 }
                }
                (3, 64) => {
                    let v = f64::from_le_bytes([
                        frame[off],
                        frame[off + 1],
                        frame[off + 2],
                        frame[off + 3],
                        frame[off + 4],
                        frame[off + 5],
                        frame[off + 6],
                        frame[off + 7],
                    ]);
                    if v.is_finite() { v } else { 0.0 }
                }
                _ => 0.0,
            };
            acc += sample;
        }
        let mono = (acc / channels as f64).clamp(-1.0, 1.0);
        out.push((mono * 32767.0).round() as i16);
    }
    Ok((out, hdr.sample_rate))
}

fn write_atomic(target: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| "参考音色目录路径无效".to_owned())?;
    fs::create_dir_all(parent).map_err(|e| format!("创建参考音色目录失败：{e}"))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let file_name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "data".into());
    let tmp = parent.join(format!(".{file_name}.tmp.{}_{nanos}", std::process::id()));
    fs::write(&tmp, bytes).map_err(|e| format!("写入参考音色临时文件失败：{e}"))?;
    if let Err(e) = fs::rename(&tmp, target) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("保存参考音色文件失败：{e}"));
    }
    Ok(())
}

pub fn read_clone_meta(clone_root: &Path) -> Option<CloneRefMeta> {
    let text = fs::read_to_string(clone_meta_path(clone_root)).ok()?;
    let meta: CloneRefMeta = serde_json::from_str(&text).ok()?;
    if meta.generation == 0 {
        return None;
    }
    let wav_path = clone_voices_dir(clone_root).join(format!("ref_{}.wav", meta.generation));
    wav_path.is_file().then_some(meta)
}

fn read_last_generation(clone_root: &Path) -> u64 {
    let seq_gen = fs::read_to_string(clone_generation_seq_path(clone_root))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let meta_gen = read_clone_meta(clone_root)
        .map(|m| m.generation)
        .unwrap_or(0);
    seq_gen.max(meta_gen)
}

pub fn is_clone_reference_configured(clone_root: &Path) -> bool {
    read_clone_meta(clone_root).is_some()
}

fn remove_file_if_present(path: &Path) -> Result<bool, String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("删除参考音色文件失败：{e}")),
    }
}

/// 清理比 `keep_min_gen` 更早的历史 `ref_*.wav`（保留上一代以防常驻 `audiocpp_server` 恰在读取中），并顺手清扫超过 5 分钟的陈旧试听临时文件。
fn prune_old_voice_files(voices_dir: &Path, keep_min_gen: u64) {
    let Ok(entries) = fs::read_dir(voices_dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if let Some(num) = s
            .strip_prefix("ref_")
            .and_then(|r| r.strip_suffix(".wav"))
            .and_then(|n| n.parse::<u64>().ok())
        {
            if num < keep_min_gen {
                let _ = fs::remove_file(entry.path());
            }
        } else if s.starts_with("preview_")
            && s.ends_with(".wav")
            && entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > std::time::Duration::from_secs(300))
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// 原子保存新的参考 WAV 与参考台词，单调递增代次号（跨移除持久化 `generation.seq`，杜绝撞上常驻进程缓存）。
pub fn store_clone_reference(
    clone_root: &Path,
    wav: &ValidatedCloneWav,
    raw_transcript: &str,
) -> Result<CloneRefMeta, String> {
    let transcript = sanitize_clone_transcript(raw_transcript)?;
    let prev_active_gen = read_clone_meta(clone_root)
        .map(|m| m.generation)
        .unwrap_or(0);
    let last_gen = read_last_generation(clone_root);
    let next_gen = last_gen.saturating_add(1).max(1);
    let voices_dir = clone_voices_dir(clone_root);
    fs::create_dir_all(&voices_dir).map_err(|e| format!("创建参考音色目录失败：{e}"))?;
    let wav_target = voices_dir.join(format!("ref_{next_gen}.wav"));
    write_atomic(&wav_target, &wav.bytes)?;
    write_atomic(
        &clone_generation_seq_path(clone_root),
        next_gen.to_string().as_bytes(),
    )?;
    let meta = CloneRefMeta {
        generation: next_gen,
        file_name: wav.file_name.clone(),
        sample_rate: wav.sample_rate,
        duration_secs: (wav.duration_secs * 10.0).round() / 10.0,
        transcript,
    };
    let meta_json =
        serde_json::to_vec_pretty(&meta).map_err(|e| format!("序列化参考音色信息失败：{e}"))?;
    write_atomic(&clone_meta_path(clone_root), &meta_json)?;
    prune_old_voice_files(&voices_dir, prev_active_gen.max(1));
    Ok(meta)
}

/// 仅修改已保存参考录音的参考台词（递增代次号以刷新 `audiocpp_server` 的 `voice_prompt_cache`）。
pub fn update_clone_transcript(
    clone_root: &Path,
    raw_transcript: &str,
) -> Result<CloneRefMeta, CloneManageError> {
    let transcript =
        sanitize_clone_transcript(raw_transcript).map_err(CloneManageError::InvalidInput)?;
    let current = read_clone_meta(clone_root).ok_or_else(|| {
        CloneManageError::InvalidInput("尚未上传参考录音，请先上传 WAV 录音文件".to_owned())
    })?;
    if current.transcript == transcript {
        return Ok(current);
    }
    let voices_dir = clone_voices_dir(clone_root);
    let cur_wav_path = voices_dir.join(format!("ref_{}.wav", current.generation));
    let wav_bytes = fs::read(&cur_wav_path)
        .map_err(|e| CloneManageError::Io(format!("读取当前参考录音失败：{e}")))?;
    let next_gen = read_last_generation(clone_root)
        .max(current.generation)
        .saturating_add(1);
    let next_wav_path = voices_dir.join(format!("ref_{next_gen}.wav"));
    write_atomic(&next_wav_path, &wav_bytes).map_err(CloneManageError::Io)?;
    write_atomic(
        &clone_generation_seq_path(clone_root),
        next_gen.to_string().as_bytes(),
    )
    .map_err(CloneManageError::Io)?;
    let next = CloneRefMeta {
        generation: next_gen,
        transcript,
        ..current
    };
    let meta_json = serde_json::to_vec_pretty(&next)
        .map_err(|e| CloneManageError::Io(format!("序列化参考音色信息失败：{e}")))?;
    write_atomic(&clone_meta_path(clone_root), &meta_json).map_err(CloneManageError::Io)?;
    prune_old_voice_files(&voices_dir, current.generation);
    Ok(next)
}

/// 删除已保存的本地克隆参考录音与台词（保留 `generation.seq` 单调计数，且如实上报底层磁盘/权限错误）。
pub fn clear_clone_reference(clone_root: &Path) -> Result<bool, String> {
    let mut removed_any = remove_file_if_present(&clone_meta_path(clone_root))?;
    let voices_dir = clone_voices_dir(clone_root);
    match fs::read_dir(&voices_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|e| format!("读取参考音色目录失败：{e}"))?;
                let name = entry.file_name();
                let s = name.to_string_lossy();
                if (s.starts_with("ref_") || s.starts_with("preview_"))
                    && s.ends_with(".wav")
                    && remove_file_if_present(&entry.path())?
                {
                    removed_any = true;
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("读取参考音色目录失败：{e}")),
    }
    Ok(removed_any)
}

/// 面向前端与诊断包的脱敏状态视图（不含任何本地路径）。
pub fn clone_reference_status_json(clone_root: &Path) -> Value {
    match read_clone_meta(clone_root) {
        Some(meta) => {
            let mode = if meta.transcript.trim().is_empty() {
                "x_vector_only"
            } else {
                "icl"
            };
            json!({
                "configured": true,
                "generation": meta.generation,
                "file_name": meta.file_name,
                "sample_rate": meta.sample_rate,
                "duration_secs": meta.duration_secs,
                "transcript": meta.transcript,
                "mode": mode,
            })
        }
        None => json!({
            "configured": false,
            "generation": 0,
            "file_name": "",
            "sample_rate": 0,
            "duration_secs": 0.0,
            "transcript": "",
            "mode": "none",
        }),
    }
}

/// 解析当前推理请求使用的 `voice_dir`、`voice` 名称与参考台词（可选传入试听时的草稿台词覆盖）。
pub fn resolve_clone_voice_binding(
    clone_root: &Path,
    transcript_override: Option<&str>,
) -> Result<CloneVoiceBinding, String> {
    let meta = read_clone_meta(clone_root).ok_or_else(|| {
        "尚未上传本地声音克隆参考录音：请在「本地模型 → 5. 语音合成」上传一段 3 ~ 30 秒清晰 WAV 录音"
            .to_owned()
    })?;
    let transcript = match transcript_override {
        Some(draft) => sanitize_clone_transcript(draft)?,
        None => meta.transcript,
    };
    Ok(CloneVoiceBinding {
        voice_dir: clone_voices_dir(clone_root),
        voice_name: format!("ref_{}", meta.generation),
        transcript,
    })
}

/// 为「按当前参考音色试一句」准备推理绑定：若用户改了输入框里的草稿台词但尚未点保存，
/// 则派生独立临时 `preview_<pid>_<nanos>.wav`，避免与常驻 `audiocpp_server` 已缓存的 `ref_<gen>` 槽位串台。
pub fn prepare_clone_preview_binding(
    clone_root: &Path,
    draft_transcript: Option<&str>,
) -> Result<(CloneVoiceBinding, Option<PathBuf>), String> {
    let meta = read_clone_meta(clone_root).ok_or_else(|| {
        "尚未上传本地声音克隆参考录音：请在「本地模型 → 5. 语音合成」上传一段 3 ~ 30 秒清晰 WAV 录音"
            .to_owned()
    })?;
    let voices_dir = clone_voices_dir(clone_root);
    let transcript = match draft_transcript {
        Some(draft) => sanitize_clone_transcript(draft)?,
        None => meta.transcript.clone(),
    };
    if transcript == meta.transcript {
        return Ok((
            CloneVoiceBinding {
                voice_dir: voices_dir,
                voice_name: format!("ref_{}", meta.generation),
                transcript,
            },
            None,
        ));
    }
    let src_wav = voices_dir.join(format!("ref_{}.wav", meta.generation));
    let wav_bytes = fs::read(&src_wav).map_err(|e| format!("读取当前参考录音失败：{e}"))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let preview_name = format!("preview_{}_{nanos}", std::process::id());
    let preview_path = voices_dir.join(format!("{preview_name}.wav"));
    write_atomic(&preview_path, &wav_bytes)?;
    Ok((
        CloneVoiceBinding {
            voice_dir: voices_dir,
            voice_name: preview_name,
            transcript,
        },
        Some(preview_path),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_pcm16_wav(sample_rate: u32, secs: u32) -> Vec<u8> {
        let samples = sample_rate * secs;
        let data_bytes = samples * 2;
        let mut out = Vec::with_capacity(44 + data_bytes as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_bytes.to_le_bytes());
        for i in 0..samples {
            let val = if i % 2 == 0 { 4096i16 } else { -4096i16 };
            out.extend_from_slice(&val.to_le_bytes());
        }
        out
    }

    fn build_pcm24_stereo_wav(sample_rate: u32, secs: u32) -> Vec<u8> {
        let frames = sample_rate * secs;
        let data_bytes = frames * 6;
        let mut out = Vec::with_capacity(44 + data_bytes as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&(sample_rate * 6).to_le_bytes());
        out.extend_from_slice(&6u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_bytes.to_le_bytes());
        for _ in 0..frames {
            // Left = +0.25 (2_097_152 = 0x200000), Right = +0.25
            out.extend_from_slice(&[0x00, 0x00, 0x20, 0x00, 0x00, 0x20]);
        }
        out
    }

    #[test]
    fn clone_reference_roundtrip_validates_wav_bumps_generation_and_redacts_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let clone_root = tmp.path().join("local-tts-clone");

        assert!(!is_clone_reference_configured(&clone_root));
        assert_eq!(
            clone_reference_status_json(&clone_root)["configured"],
            false
        );

        // 过短（1 秒）与过低采样率（8 kHz）均拒绝
        let short_b64 =
            base64::engine::general_purpose::STANDARD.encode(build_pcm16_wav(24_000, 1));
        assert!(parse_clone_wav_upload(&short_b64, "short.wav").is_err());
        let low_sr_b64 =
            base64::engine::general_purpose::STANDARD.encode(build_pcm16_wav(8_000, 5));
        assert!(parse_clone_wav_upload(&low_sr_b64, "low.wav").is_err());

        // 24-bit 双声道 WAV 正常通过并可解码为单声道 i16
        let wav24 = build_pcm24_stereo_wav(24_000, 5);
        let b64 = format!(
            "data:audio/wav;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&wav24)
        );
        let parsed = parse_clone_wav_upload(&b64, "../secret/moss550w.wav").unwrap();
        assert_eq!(parsed.file_name, "moss550w.wav");
        assert_eq!(parsed.sample_rate, 24_000);
        assert_eq!(
            parsed.bytes.len(),
            44 + 24_000 * 5 * 2,
            "上传的 24-bit 双声道 WAV 须归一化为标准 44 字节头 16-bit 单声道 PCM WAV"
        );
        let (mono, sr) = decode_clone_wav_to_mono_i16(&parsed.bytes).unwrap();
        assert_eq!(sr, 24_000);
        assert_eq!(mono.len(), 24_000 * 5);
        assert!((mono[0] - 8192).abs() <= 2);

        let meta1 =
            store_clone_reference(&clone_root, &parsed, "让人类永远保持理智，确实是一种奢求。")
                .unwrap();
        assert_eq!(meta1.generation, 1);
        let status1 = clone_reference_status_json(&clone_root);
        assert_eq!(status1["configured"], true);
        assert_eq!(status1["generation"], 1);
        assert_eq!(status1["file_name"], "moss550w.wav");
        assert_eq!(status1["mode"], "icl");
        assert!(
            !status1.to_string().contains("local-tts-clone"),
            "状态视图不得暴露本地路径"
        );

        let meta2 = update_clone_transcript(&clone_root, "").unwrap();
        assert_eq!(meta2.generation, 2);
        assert_eq!(
            clone_reference_status_json(&clone_root)["mode"],
            "x_vector_only"
        );
        let binding = resolve_clone_voice_binding(&clone_root, None).unwrap();
        assert_eq!(binding.voice_name, "ref_2");
        assert_eq!(binding.transcript, "");

        // 第三次更新后，第 1 代旧 WAV 自动清理，保留第 2 代与第 3 代
        let meta3 = update_clone_transcript(&clone_root, "新的参考台词").unwrap();
        assert_eq!(meta3.generation, 3);
        assert!(!clone_voices_dir(&clone_root).join("ref_1.wav").exists());
        assert!(clone_voices_dir(&clone_root).join("ref_2.wav").is_file());
        assert!(clone_voices_dir(&clone_root).join("ref_3.wav").is_file());

        // 未保存的试听草稿台词生成独立临时文件，不覆盖线上 ref_3
        let (preview_bind, temp_file) =
            prepare_clone_preview_binding(&clone_root, Some("试听草稿原话")).unwrap();
        assert!(preview_bind.voice_name.starts_with("preview_"));
        assert!(temp_file.as_ref().unwrap().is_file());
        fs::remove_file(temp_file.unwrap()).unwrap();

        assert!(clear_clone_reference(&clone_root).unwrap());
        assert!(!is_clone_reference_configured(&clone_root));

        // 移除后重新上传：generation 继续单调递增为 4，绝不回绕撞上旧 ref_1 缓存
        let meta4 = store_clone_reference(&clone_root, &parsed, "重传后的台词").unwrap();
        assert_eq!(meta4.generation, 4);
        assert!(clear_clone_reference(&clone_root).unwrap());
    }

    #[test]
    fn remove_managed_runtime_only_removes_audiocpp_dirs_and_detect_env_hides_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let audio_dir = tmp
            .path()
            .join(".runtime/audiocpp-v0.9.6/macos-arm64-metal");
        let speech_dir = tmp.path().join(".runtime/speech-v0.10.1/macos-arm64-metal");
        fs::create_dir_all(&audio_dir).unwrap();
        fs::create_dir_all(&speech_dir).unwrap();
        fs::write(audio_dir.join("audiocpp_server"), b"stub").unwrap();
        fs::write(speech_dir.join("speech"), b"stub").unwrap();

        assert!(remove_managed_runtime(tmp.path()).unwrap());
        assert!(!audio_dir.exists());
        assert!(speech_dir.is_dir());
        assert!(!remove_managed_runtime(tmp.path()).unwrap());

        let local_audiocpp = tmp.path().join("audiocpp");
        fs::create_dir_all(local_audiocpp.join("model_specs")).unwrap();
        fs::write(local_audiocpp.join(audiocpp_binary_name()), b"stub").unwrap();
        fs::write(local_audiocpp.join("model_specs/qwen3_tts.json"), b"{}").unwrap();
        let env = detect_audiocpp_env(tmp.path());
        assert_eq!(env["ready"], true);
        assert_eq!(env["installed"], true);
        assert!(env.get("binary_path").is_none());
        assert!(
            !env.to_string()
                .contains(&tmp.path().to_string_lossy().to_string())
        );
    }

    #[test]
    fn tar_gz_with_dot_root_extracts_cleanly_and_orphan_matcher_checks_ppid_and_root() {
        let tmp = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let append = |builder: &mut tar::Builder<flate2::write::GzEncoder<Vec<u8>>>,
                      name: &str,
                      entry_type: tar::EntryType,
                      body: &[u8]| {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(if entry_type == tar::EntryType::Directory {
                0o755
            } else {
                0o644
            });
            header.set_entry_type(entry_type);
            header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
            header.set_cksum();
            builder.append(&header, body).unwrap();
        };
        append(&mut builder, ".", tar::EntryType::Directory, b"");
        append(
            &mut builder,
            "./audiocpp_server",
            tar::EntryType::Regular,
            b"server-bin",
        );
        append(
            &mut builder,
            "./model_specs/qwen3_tts.json",
            tar::EntryType::Regular,
            b"{}",
        );
        let tar_gz = builder.into_inner().unwrap().finish().unwrap();
        let archive_path = tmp.path().join("audio.tar.gz");
        fs::write(&archive_path, tar_gz).unwrap();
        let out_dir = tmp.path().join("out");
        fs::create_dir_all(&out_dir).unwrap();
        extract_tar_gz(&archive_path, &out_dir, &cancel).unwrap();
        assert_eq!(
            fs::read(out_dir.join("audiocpp_server")).unwrap(),
            b"server-bin"
        );
        assert!(out_dir.join("model_specs/qwen3_tts.json").is_file());

        let ps = "\
  401     1 /app/my  models/.runtime/audiocpp-v0.9.6/macos-arm64-metal/audiocpp_server --config /app/server.json\n\
  402   300 /app/my  models/.runtime/audiocpp-v0.9.6/macos-arm64-metal/audiocpp_server --config /app/server.json\n\
  403     1 /usr/local/bin/audiocpp_server --config /other/server.json\n";
        assert_eq!(
            orphan_audiocpp_pids(ps, &["/app/my  models".to_owned()]),
            vec![401]
        );
    }
}
