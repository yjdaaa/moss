//! 本地 OneJev 1.6（0.8B / 4B / 9B）多模态 System 1 决策运行时与跨平台硬件加速网关。
//!
//! - **免 Python / 免 PyTorch**：由 MOSS 原生 Rust 直接实现 `OneJev`（`qev-labels-v2` / `onejev-slot-v2`）
//!   的提示渲染、共享前缀 KV 预填、槽位对数概率读取与置信度校准，对外提供标准
//!   `POST /v1/systemone` 与 `GET /health`；
//! - **跨平台异构硬件加速**：
//!   - macOS Apple Silicon（M1–M6）：启用 `Apple Metal` 统一内存 GPU 加速（`-ngl all`）；
//!   - macOS Intel：启用 CPU 推理（`-ngl 0`）；
//!   - Windows：优先识别用户放置的 `NVIDIA CUDA` 构建（`ggml-cuda.dll`），否则在具备现代显卡驱动
//!     （`vulkan-1.dll`）的机器上自动使用 `Vulkan` 显卡加速（通吃 NVIDIA / AMD / Intel 独显与核显），
//!     无显卡环境自动回退 AVX2 CPU（`-ngl 0`）；
//!   - Linux：识别 `CUDA` / `Vulkan` 或回退 CPU；
//! - **按需自动就绪**：若本机未安装 `llama-server`，下载 OneJev 模型或首次启动时自动拉取对应平台官方预编译包
//!   并逐字节核对 SHA-256 摘要，解压到 `<models>/.runtime/llama-b12167/`。

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::local_models::{fetch_verified_file, safe_relative};
use crate::supervisor::JobHandle;

pub const LLAMA_CPP_RELEASE_TAG: &str = "b12167";
const LLAMA_GITHUB_RELEASE_BASE: &str =
    "https://github.com/ggml-org/llama.cpp/releases/download/b12167";
const LLAMA_GHFAST_MIRROR_BASE: &str =
    "https://ghfast.top/https://github.com/ggml-org/llama.cpp/releases/download/b12167";

const PROMPT_VERSION: &str = "qev-labels-v2";
const SYSTEM_PROMPT: &str = "Apply the question to the state. Choose exactly one of the listed options. Respond with only its uppercase letter, with no explanation or reasoning.";
const NOUL_DEFAULT_INSTRUCTIONS: &str =
    "Is the statement true, or is the answer to the question yes?";
const NOUL_DEFAULT_TRUE: &str = "the statement is true / the answer is yes";
const NOUL_DEFAULT_FALSE: &str = "the statement is false / the answer is no";
const NONE_OPTION: &str = "none of the options listed here";
const NONE_DESCRIPTION: &str =
    "The correct option is not among the options listed in this question.";
const LETTERS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const NOT_ONE_TOKEN: [&str; 25] = [
    "BQ", "BZ", "CJ", "CQ", "CZ", "DQ", "DZ", "EJ", "EY", "FJ", "FQ", "FV", "FZ", "GJ", "GK", "GQ",
    "GZ", "HJ", "IY", "JF", "JG", "JH", "JL", "JN", "JQ",
];
const MAX_HTTP_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
const MAX_HTTP_HEADER_LINES: usize = 64;
const MAX_ACTIVE_GATEWAY_CONNS: usize = 16;
const DEFAULT_CTX_TOKENS: usize = 8192;
const MAX_REQUEST_TOKENS: usize = 32768;

static RUNTIME_INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeArchiveKind {
    TarGz { top_dir: &'static str },
    Zip,
}

#[derive(Debug, Clone, Copy)]
struct LlamaReleaseAsset {
    platform_dir: &'static str,
    file_name: &'static str,
    sha256: &'static str,
    size: u64,
    archive: RuntimeArchiveKind,
    expected_accel: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceleratorKind {
    Metal,
    Cuda,
    Vulkan,
    Cpu,
    CpuOverride,
}

impl AcceleratorKind {
    pub fn uses_gpu(&self) -> bool {
        matches!(self, Self::Metal | Self::Cuda | Self::Vulkan)
    }

    pub fn short_label(&self) -> &'static str {
        match self {
            Self::Metal => "Apple Metal GPU",
            Self::Cuda => "NVIDIA CUDA GPU",
            Self::Vulkan => "Vulkan GPU",
            Self::Cpu => "CPU",
            Self::CpuOverride => "CPU（环境变量强制）",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DetectedAccelerator {
    pub kind: AcceleratorKind,
    pub device_detail: Option<String>,
}

impl DetectedAccelerator {
    pub fn display_text(&self) -> String {
        match &self.device_detail {
            Some(detail) if !detail.is_empty() => {
                format!("{}（{detail}）", self.kind.short_label())
            }
            _ => self.kind.short_label().to_owned(),
        }
    }
}

fn slot_labels() -> &'static Vec<String> {
    static SLOTS: OnceLock<Vec<String>> = OnceLock::new();
    SLOTS.get_or_init(|| {
        let mut out: Vec<String> = LETTERS.chars().map(|c| c.to_string()).collect();
        for a in LETTERS.chars() {
            for b in LETTERS.chars() {
                if out.len() >= 256 {
                    break;
                }
                let pair = format!("{a}{b}");
                if !NOT_ONE_TOKEN.contains(&pair.as_str()) {
                    out.push(pair);
                }
            }
        }
        out
    })
}

fn llama_binary_name() -> &'static str {
    if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

fn windows_vulkan_loader_present() -> bool {
    #[cfg(windows)]
    {
        let sys_root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        sys_root.join("System32").join("vulkan-1.dll").is_file()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn linux_vulkan_loader_present() -> bool {
    #[cfg(target_os = "linux")]
    {
        [
            "/usr/lib/x86_64-linux-gnu/libvulkan.so.1",
            "/usr/lib64/libvulkan.so.1",
            "/usr/lib/libvulkan.so.1",
            "/usr/lib/aarch64-linux-gnu/libvulkan.so.1",
        ]
        .iter()
        .any(|p| Path::new(p).exists())
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn current_platform_asset() -> Option<LlamaReleaseAsset> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(LlamaReleaseAsset {
            platform_dir: "macos-arm64",
            file_name: "llama-b12167-bin-macos-arm64.tar.gz",
            sha256: "2b7324ea05035e2cd7ba4ca54b695575c65752e2cef15291a4531b8a1fafbe12",
            size: 16_674_382,
            archive: RuntimeArchiveKind::TarGz {
                top_dir: "llama-b12167",
            },
            expected_accel: "Apple Metal 统一内存硬件加速",
        })
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some(LlamaReleaseAsset {
            platform_dir: "macos-x64",
            file_name: "llama-b12167-bin-macos-x64.tar.gz",
            sha256: "db6df599cf86fc5329a5e2f46f3081a5befa3e665936538cf8093e5b16658560",
            size: 12_411_336,
            archive: RuntimeArchiveKind::TarGz {
                top_dir: "llama-b12167",
            },
            expected_accel: "Intel CPU 原生推理",
        })
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        if windows_vulkan_loader_present() {
            Some(LlamaReleaseAsset {
                platform_dir: "win-vulkan-x64",
                file_name: "llama-b12167-bin-win-vulkan-x64.zip",
                sha256: "f99b2fc83d93f4a36778e5966600c8a4a36e8a3a6660521601317c02f974ca9d",
                size: 37_685_589,
                archive: RuntimeArchiveKind::Zip,
                expected_accel: "Windows Vulkan 显卡加速（兼容 NVIDIA / AMD / Intel 显卡）",
            })
        } else {
            Some(LlamaReleaseAsset {
                platform_dir: "win-cpu-x64",
                file_name: "llama-b12167-bin-win-cpu-x64.zip",
                sha256: "430d22ba7693f2135f9d3e5301f7d773cf1b8ba89fdf5ca3a5cdc5d5a7173980",
                size: 24_726_767,
                archive: RuntimeArchiveKind::Zip,
                expected_accel: "Windows AVX2 CPU 推理",
            })
        }
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        if windows_vulkan_loader_present() {
            Some(LlamaReleaseAsset {
                platform_dir: "win-vulkan-arm64",
                file_name: "llama-b12167-bin-win-vulkan-arm64.zip",
                sha256: "ee34828bc140e7e10a785d093c0bef72ab2de41843521c3011833eb96c54dd4a",
                size: 25_740_002,
                archive: RuntimeArchiveKind::Zip,
                expected_accel: "Windows ARM64 Vulkan 硬件加速",
            })
        } else {
            Some(LlamaReleaseAsset {
                platform_dir: "win-cpu-arm64",
                file_name: "llama-b12167-bin-win-cpu-arm64.zip",
                sha256: "bdb7f985e08c121844712d29296a2421dfbcce8d01a41197b6b3543071c20b01",
                size: 13_205_443,
                archive: RuntimeArchiveKind::Zip,
                expected_accel: "Windows ARM64 CPU 推理",
            })
        }
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        if linux_vulkan_loader_present() {
            Some(LlamaReleaseAsset {
                platform_dir: "linux-vulkan-x64",
                file_name: "llama-b12167-bin-ubuntu-vulkan-x64.tar.gz",
                sha256: "0a27da2be4ea383e18eb7558f7e5b05f1a3b5876ec141415e8f6d3c192932931",
                size: 35_856_178,
                archive: RuntimeArchiveKind::TarGz {
                    top_dir: "llama-b12167",
                },
                expected_accel: "Linux Vulkan 显卡加速",
            })
        } else {
            Some(LlamaReleaseAsset {
                platform_dir: "linux-cpu-x64",
                file_name: "llama-b12167-bin-ubuntu-x64.tar.gz",
                sha256: "b9fcdc51ca6e747085070ae01c7c8acaecdfeb4b6c8386166e2d4b067b36186f",
                size: 23_046_778,
                archive: RuntimeArchiveKind::TarGz {
                    top_dir: "llama-b12167",
                },
                expected_accel: "Linux x64 CPU 推理",
            })
        }
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some(LlamaReleaseAsset {
            platform_dir: "linux-arm64",
            file_name: "llama-b12167-bin-ubuntu-arm64.tar.gz",
            sha256: "17fda66c2c358d32dc7d3b15089b4ba01de207e7ac78832fefe54bb9f3390dd5",
            size: 18_727_515,
            archive: RuntimeArchiveKind::TarGz {
                top_dir: "llama-b12167",
            },
            expected_accel: "Linux ARM64 CPU 推理",
        })
    } else {
        None
    }
}

fn managed_runtime_dir(models_root: &Path, asset: &LlamaReleaseAsset) -> PathBuf {
    models_root
        .join(".runtime")
        .join(format!("llama-{LLAMA_CPP_RELEASE_TAG}"))
        .join(asset.platform_dir)
}

fn has_cuda_companion(llama_bin: &Path) -> bool {
    llama_bin.parent().is_some_and(|dir| {
        dir.join("ggml-cuda.dll").is_file()
            || dir.join("libggml-cuda.so").is_file()
            || dir.join("libggml-cuda.dylib").is_file()
    })
}

pub fn has_platform_runtime_asset() -> bool {
    current_platform_asset().is_some()
}

pub fn resolve_llama_server(models_root: &Path) -> Option<PathBuf> {
    let bin = llama_binary_name();
    if let Some(env_bin) = std::env::var_os("MOSS_LLAMA_SERVER").map(PathBuf::from)
        && env_bin.is_file()
    {
        return Some(env_bin);
    }
    let mut local_dirs = vec![models_root.join("llama")];
    if let Some(parent) = models_root.parent() {
        local_dirs.push(parent.join("llama"));
    }
    // Windows / Linux 用户在 `<models>/llama` 或安装目录 `<path>/llama` 放入官方 CUDA 构建时优先启用 CUDA
    for dir in &local_dirs {
        let cand = dir.join(bin);
        if cand.is_file() && has_cuda_companion(&cand) {
            return Some(cand);
        }
    }
    // 优先使用经 SHA-256 校验落盘的托管运行时，避免被 PATH 上旧版或未校验同名文件劫持
    if let Some(asset) = current_platform_asset() {
        let managed = managed_runtime_dir(models_root, &asset).join(bin);
        if managed.is_file() {
            return Some(managed);
        }
    }
    for dir in &local_dirs {
        let cand = dir.join(bin);
        if cand.is_file() {
            return Some(cand);
        }
    }
    // 仅在无内置预编译资产的平台上回退查找系统 PATH
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

fn configure_no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd;
}

fn device_env_forces_cpu() -> bool {
    std::env::var("MOSS_JEV_DEVICE")
        .or_else(|_| std::env::var("MOSS_LAYA_DEVICE"))
        .ok()
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("cpu"))
}

pub fn probe_llama_accelerator(llama_bin: &Path, run_probe: bool) -> DetectedAccelerator {
    probe_llama_accelerator_with_cancel(llama_bin, run_probe, None)
}

pub fn probe_llama_accelerator_with_cancel(
    llama_bin: &Path,
    run_probe: bool,
    cancel: Option<&AtomicBool>,
) -> DetectedAccelerator {
    if device_env_forces_cpu() {
        return DetectedAccelerator {
            kind: AcceleratorKind::CpuOverride,
            device_detail: None,
        };
    }
    let allow_metal = cfg!(all(target_os = "macos", target_arch = "aarch64"));
    let parent = llama_bin.parent();
    let has_cuda_lib = has_cuda_companion(llama_bin);
    let has_metal_lib = allow_metal;
    let has_vulkan_lib = parent.is_some_and(|dir| {
        dir.join("ggml-vulkan.dll").is_file()
            || dir.join("libggml-vulkan.so").is_file()
            || dir.join("libggml-vulkan.dylib").is_file()
    });

    if run_probe && llama_bin.is_file() && !cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
        let mut cmd = Command::new(llama_bin);
        cmd.arg("--list-devices")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_no_window(&mut cmd);
        if let Ok(mut child) = cmd.spawn() {
            let deadline = Instant::now() + Duration::from_secs(4);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let mut out = String::new();
                        if let Some(mut stdout) = child.stdout.take() {
                            let _ = stdout.read_to_string(&mut out);
                        }
                        if status.success()
                            && let Some(detected) =
                                parse_list_devices_output_for_arch(&out, allow_metal)
                        {
                            return detected;
                        }
                        break;
                    }
                    Ok(None) => {
                        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst))
                            || Instant::now() >= deadline
                        {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(30));
                    }
                    Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
    }

    if has_cuda_lib {
        DetectedAccelerator {
            kind: AcceleratorKind::Cuda,
            device_detail: None,
        }
    } else if has_metal_lib {
        DetectedAccelerator {
            kind: AcceleratorKind::Metal,
            device_detail: None,
        }
    } else if has_vulkan_lib {
        DetectedAccelerator {
            kind: AcceleratorKind::Vulkan,
            device_detail: None,
        }
    } else {
        DetectedAccelerator {
            kind: AcceleratorKind::Cpu,
            device_detail: None,
        }
    }
}

fn parse_list_devices_output_for_arch(
    output: &str,
    allow_metal: bool,
) -> Option<DetectedAccelerator> {
    let mut cuda_dev = None;
    let mut metal_dev = None;
    let mut vulkan_dev = None;
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if let Some(rest) = line.strip_prefix("CUDA0:") {
            cuda_dev = Some(clean_device_description(rest));
        } else if allow_metal && let Some(rest) = line.strip_prefix("MTL0:") {
            metal_dev = Some(clean_device_description(rest));
        } else if let Some(rest) = line.strip_prefix("Vulkan0:") {
            vulkan_dev = Some(clean_device_description(rest));
        }
    }
    if let Some(detail) = cuda_dev {
        Some(DetectedAccelerator {
            kind: AcceleratorKind::Cuda,
            device_detail: Some(detail),
        })
    } else if let Some(detail) = metal_dev {
        Some(DetectedAccelerator {
            kind: AcceleratorKind::Metal,
            device_detail: Some(detail),
        })
    } else if let Some(detail) = vulkan_dev {
        Some(DetectedAccelerator {
            kind: AcceleratorKind::Vulkan,
            device_detail: Some(detail),
        })
    } else if output.contains("Available devices:") {
        Some(DetectedAccelerator {
            kind: AcceleratorKind::Cpu,
            device_detail: None,
        })
    } else {
        None
    }
}

#[cfg(test)]
fn parse_list_devices_output(output: &str) -> Option<DetectedAccelerator> {
    parse_list_devices_output_for_arch(output, true)
}

fn clean_device_description(raw: &str) -> String {
    let trimmed = raw.trim();
    match trimmed.find(" (") {
        Some(idx) => trimmed[..idx].trim().to_owned(),
        None => trimmed.to_owned(),
    }
}

pub fn detect_onejev_env(models_root: &Path, probe_binary: bool) -> Value {
    if let Some(bin) = resolve_llama_server(models_root) {
        let accel = probe_llama_accelerator(&bin, probe_binary);
        let accel_text = accel.display_text();
        return json!({
            "ready": true,
            "installed": true,
            "binary_found": true,
            "accelerator": accel.kind,
            "accelerator_label": accel_text,
            "summary": format!("OneJev 原生推理引擎已就绪（{} · 免装 Python）", accel.display_text()),
        });
    }
    if let Some(asset) = current_platform_asset() {
        let mb = (asset.size as f64) / (1024.0 * 1024.0);
        let force_cpu = device_env_forces_cpu();
        let expected_kind = if force_cpu {
            AcceleratorKind::CpuOverride
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            AcceleratorKind::Metal
        } else if windows_vulkan_loader_present() || linux_vulkan_loader_present() {
            AcceleratorKind::Vulkan
        } else {
            AcceleratorKind::Cpu
        };
        let expected_label = if force_cpu {
            expected_kind.short_label()
        } else {
            asset.expected_accel
        };
        return json!({
            "ready": true,
            "installed": false,
            "binary_found": false,
            "accelerator": expected_kind,
            "accelerator_label": expected_label,
            "summary": format!(
                "OneJev 支持开箱即用：将启用{}（首次启动自动准备约 {:.0} MB 官方 llama-server 推理组件，无需安装 Python/PyTorch）",
                expected_label, mb
            ),
        });
    }
    json!({
        "ready": false,
        "installed": false,
        "binary_found": false,
        "accelerator": AcceleratorKind::Cpu,
        "accelerator_label": "需手动提供 llama-server",
        "summary": "当前系统架构暂无内置预编译包，请安装 llama.cpp 并设置 MOSS_LLAMA_SERVER 指向 llama-server。",
    })
}

/// 推理引擎准备进度（下载 OneJev 时显示在模型行里）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProgress {
    /// 另一个 OneJev 下载正在安装同一个推理引擎，排队等待
    Waiting,
    /// 正在下载推理引擎压缩包
    Downloading { done: u64, total: u64 },
}

/// 等待推理引擎安装锁；等待期间可取消（2026-11-07：多档 OneJev 同时下载时后点的不再卡死在 0%）。
fn lock_runtime_install(
    cancel: &AtomicBool,
    progress: &dyn Fn(RuntimeProgress),
) -> Result<std::sync::MutexGuard<'static, ()>, String> {
    let lock = RUNTIME_INSTALL_LOCK.get_or_init(|| Mutex::new(()));
    let mut reported = false;
    loop {
        match lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                if cancel.load(Ordering::SeqCst) {
                    return Err("已取消".into());
                }
                if !reported {
                    progress(RuntimeProgress::Waiting);
                    reported = true;
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        }
    }
}

/// 删除 MOSS 自动下载的推理引擎（`<models>/.runtime/llama-*`）；用户自己放的 `<models>/llama` 不动。
/// 返回是否删掉了东西。最后一档 OneJev 删除时调用（2026-11-07）。
pub fn remove_managed_runtime(models_root: &Path) -> Result<bool, String> {
    let runtime = models_root.join(".runtime");
    let Ok(entries) = fs::read_dir(&runtime) else {
        return Ok(false);
    };
    let _guard = RUNTIME_INSTALL_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut removed = false;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("llama-")
            || !entry.file_type().is_ok_and(|ft| ft.is_dir())
        {
            continue;
        }
        fs::remove_dir_all(entry.path())
            .map_err(|error| format!("删除 OneJev 推理引擎失败（可能仍在运行）：{error}"))?;
        removed = true;
    }
    // 目录空了才删
    let _ = fs::remove_dir(&runtime);
    Ok(removed)
}

pub fn ensure_llama_runtime(
    models_root: &Path,
    mirror_hint: Option<&str>,
    cancel: &AtomicBool,
    progress: &dyn Fn(RuntimeProgress),
) -> Result<PathBuf, String> {
    if let Some(existing) = resolve_llama_server(models_root) {
        return Ok(existing);
    }
    let _guard = lock_runtime_install(cancel, progress)?;
    if let Some(existing) = resolve_llama_server(models_root) {
        return Ok(existing);
    }
    let asset = current_platform_asset()
        .ok_or_else(|| "当前操作系统/架构未内置预编译 llama-server，请手动安装 llama.cpp 并配置 MOSS_LLAMA_SERVER".to_owned())?;
    let target_dir = managed_runtime_dir(models_root, &asset);
    let target_bin = target_dir.join(llama_binary_name());
    if target_bin.is_file() {
        return Ok(target_bin);
    }

    let staging = models_root.join(".download");
    fs::create_dir_all(&staging).map_err(|e| format!("创建运行时下载目录失败：{e}"))?;
    let part = staging.join(format!("llama-runtime-{}.part", asset.file_name));
    let prefer_china_mirror = mirror_hint
        .is_some_and(|m| m.contains("hf-mirror") || m.contains("ghfast") || m.contains("mirror"));
    let bases = if prefer_china_mirror {
        [LLAMA_GHFAST_MIRROR_BASE, LLAMA_GITHUB_RELEASE_BASE]
    } else {
        [LLAMA_GITHUB_RELEASE_BASE, LLAMA_GHFAST_MIRROR_BASE]
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
            "下载 OneJev 原生推理引擎（llama-server）失败：{err}"
        ));
    }

    let extract_root = models_root.join(format!(".extract-llama-{}", asset.platform_dir));
    let _ = fs::remove_dir_all(&extract_root);
    fs::create_dir_all(&extract_root).map_err(|e| format!("创建解压目录失败：{e}"))?;

    let unpack_res = (|| {
        let unpacked_dir = match asset.archive {
            RuntimeArchiveKind::TarGz { top_dir } => {
                extract_tar_gz(&part, &extract_root, cancel)?;
                let sub = extract_root.join(top_dir);
                if sub.is_dir() {
                    sub
                } else {
                    extract_root.clone()
                }
            }
            RuntimeArchiveKind::Zip => {
                extract_zip(&part, &extract_root, cancel)?;
                extract_root.clone()
            }
        };
        let extracted_bin = unpacked_dir.join(llama_binary_name());
        if !extracted_bin.is_file() {
            return Err(format!("推理引擎压缩包缺少 {}", llama_binary_name()));
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
        fs::rename(&unpacked_dir, &target_dir).map_err(|e| format!("安装推理引擎失败：{e}"))?;
        Ok(())
    })();

    let _ = fs::remove_file(&part);
    let _ = fs::remove_dir_all(&extract_root);
    unpack_res?;
    Ok(target_bin)
}

fn safe_intra_dir_symlink_target(target: &Path) -> bool {
    let mut comps = target.components();
    matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none()
}

fn extract_tar_gz(archive_file: &Path, dest: &Path, cancel: &AtomicBool) -> Result<(), String> {
    let file = fs::File::open(archive_file).map_err(|e| format!("打开 tar.gz 失败：{e}"))?;
    let gz = flate2::read::GzDecoder::new(BufReader::new(file));
    let mut archive = tar::Archive::new(gz);
    let entries = archive
        .entries()
        .map_err(|_| "tar.gz 压缩包损坏".to_owned())?;
    for entry in entries {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".into());
        }
        let mut entry = entry.map_err(|_| "tar.gz 条目损坏".to_owned())?;
        let path = entry
            .path()
            .map_err(|_| "tar.gz 路径无效".to_owned())?
            .into_owned();
        if !safe_relative(&path) {
            return Err(format!("压缩包含不安全路径：{}", path.display()));
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_file() || entry_type.is_dir() {
            if !entry
                .unpack_in(dest)
                .map_err(|e| format!("解压 tar.gz 失败：{e}"))?
            {
                return Err(format!("压缩包含越界路径：{}", path.display()));
            }
        } else if entry_type.is_symlink() {
            #[cfg(unix)]
            {
                if let Ok(Some(link_target)) = entry.link_name() {
                    let link_target = link_target.into_owned();
                    if safe_intra_dir_symlink_target(&link_target) {
                        let full_dest = dest.join(&path);
                        if let Some(parent) = full_dest.parent() {
                            fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
                        }
                        let _ = fs::remove_file(&full_dest);
                        std::os::unix::fs::symlink(&link_target, &full_dest)
                            .map_err(|e| format!("创建动态库符号链接失败：{e}"))?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// 轻量纯 Rust ZIP 解压（仅接受相对安全路径、Stored(0) 与 Deflate(8) 普通文件/目录，拒绝符号链接与路径穿越）。
fn extract_zip(archive_file: &Path, dest: &Path, cancel: &AtomicBool) -> Result<(), String> {
    let bytes = fs::read(archive_file).map_err(|e| format!("读取 zip 失败：{e}"))?;
    extract_zip_bytes(&bytes, dest, cancel)
}

fn extract_zip_bytes(bytes: &[u8], dest: &Path, cancel: &AtomicBool) -> Result<(), String> {
    if bytes.len() < 22 {
        return Err("zip 文件过短".into());
    }
    let min_eocd = bytes.len().saturating_sub(65_557);
    let mut eocd_pos = None;
    for pos in (min_eocd..=bytes.len() - 22).rev() {
        if &bytes[pos..pos + 4] == b"PK\x05\x06" {
            let comment_len = u16::from_le_bytes([bytes[pos + 20], bytes[pos + 21]]) as usize;
            if pos + 22 + comment_len == bytes.len() {
                eocd_pos = Some(pos);
                break;
            }
        }
    }
    let eocd = eocd_pos.ok_or_else(|| "无效的 zip 尾部目录".to_owned())?;
    let total_entries = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]) as usize;
    let cd_offset = u32::from_le_bytes([
        bytes[eocd + 16],
        bytes[eocd + 17],
        bytes[eocd + 18],
        bytes[eocd + 19],
    ]) as usize;
    let mut cd = cd_offset;
    for _ in 0..total_entries {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".into());
        }
        if cd + 46 > bytes.len() || &bytes[cd..cd + 4] != b"PK\x01\x02" {
            return Err("zip 中央目录损坏".into());
        }
        let method = u16::from_le_bytes([bytes[cd + 10], bytes[cd + 11]]);
        let comp_size = u32::from_le_bytes([
            bytes[cd + 20],
            bytes[cd + 21],
            bytes[cd + 22],
            bytes[cd + 23],
        ]) as usize;
        let uncomp_size = u32::from_le_bytes([
            bytes[cd + 24],
            bytes[cd + 25],
            bytes[cd + 26],
            bytes[cd + 27],
        ]) as usize;
        let fn_len = u16::from_le_bytes([bytes[cd + 28], bytes[cd + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[cd + 30], bytes[cd + 31]]) as usize;
        let comment_len = u16::from_le_bytes([bytes[cd + 32], bytes[cd + 33]]) as usize;
        let ext_attr = u32::from_le_bytes([
            bytes[cd + 38],
            bytes[cd + 39],
            bytes[cd + 40],
            bytes[cd + 41],
        ]);
        let local_offset = u32::from_le_bytes([
            bytes[cd + 42],
            bytes[cd + 43],
            bytes[cd + 44],
            bytes[cd + 45],
        ]) as usize;
        let header_end = cd + 46 + fn_len + extra_len + comment_len;
        if header_end > bytes.len() {
            return Err("zip 条目越界".into());
        }
        let raw_name = std::str::from_utf8(&bytes[cd + 46..cd + 46 + fn_len])
            .map_err(|_| "zip 文件名编码无效".to_owned())?;
        cd = header_end;

        let normalized = raw_name.replace('\\', "/");
        let is_dir = normalized.ends_with('/');
        let clean = normalized.trim_end_matches('/');
        if clean.is_empty() {
            continue;
        }
        let rel = Path::new(clean);
        if !safe_relative(rel) {
            return Err(format!("zip 包含不安全路径：{raw_name}"));
        }
        // Unix mode bits in upper 16 bits of external attributes: skip symlinks (0o120000)
        let unix_mode = (ext_attr >> 16) & 0o170000;
        if unix_mode == 0o120000 {
            continue;
        }
        let out_path = dest.join(rel);
        if is_dir {
            fs::create_dir_all(&out_path).map_err(|e| format!("创建目录失败：{e}"))?;
            continue;
        }
        if local_offset + 30 > bytes.len()
            || &bytes[local_offset..local_offset + 4] != b"PK\x03\x04"
        {
            return Err("zip 本地文件头损坏".into());
        }
        let l_fn_len =
            u16::from_le_bytes([bytes[local_offset + 26], bytes[local_offset + 27]]) as usize;
        let l_ex_len =
            u16::from_le_bytes([bytes[local_offset + 28], bytes[local_offset + 29]]) as usize;
        let data_start = local_offset + 30 + l_fn_len + l_ex_len;
        let data_end = data_start.saturating_add(comp_size);
        if data_end > bytes.len() {
            return Err("zip 压缩数据越界".into());
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建父目录失败：{e}"))?;
        }
        let slice = &bytes[data_start..data_end];
        let mut out_file =
            fs::File::create(&out_path).map_err(|e| format!("写入解压文件失败：{e}"))?;
        let written = match method {
            0 => {
                out_file
                    .write_all(slice)
                    .map_err(|e| format!("写入文件失败：{e}"))?;
                slice.len()
            }
            8 => {
                let mut decoder = flate2::read::DeflateDecoder::new(Cursor::new(slice));
                std::io::copy(&mut decoder, &mut out_file)
                    .map_err(|e| format!("解压 deflate 数据失败：{e}"))? as usize
            }
            other => return Err(format!("不支持的 zip 压缩算法：{other}")),
        };
        if written != uncomp_size {
            return Err(format!("zip 解压大小不匹配（{written} != {uncomp_size}）"));
        }
    }
    Ok(())
}

// ============================================================================
// OneJev System One 协议渲染与答案计算（对齐 OmniJev/OneJev qev-labels-v2）
// ============================================================================

#[derive(Debug, Clone)]
enum ParsedQuestion {
    Noul {
        instructions: String,
        true_desc: String,
        false_desc: String,
    },
    Choice {
        instructions: String,
        options: Vec<(String, String)>,
    },
    Score {
        instructions: String,
        levels: Vec<(Value, String)>,
    },
}

#[derive(Debug, Clone)]
struct RenderedBranch {
    qid: String,
    kind: &'static str,
    labels: Vec<String>,
    raw_levels: Vec<Value>,
    suffix: String,
    chunk_index: Option<usize>,
}

fn render_entry(val: Option<&Value>) -> String {
    match val {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn render_state(state: &Value) -> String {
    match state {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn parse_question(raw: &Value) -> Result<ParsedQuestion, String> {
    let obj = raw
        .as_object()
        .ok_or_else(|| "question 必须是 JSON 对象".to_owned())?;
    let qtype = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "question 缺少 type 字段".to_owned())?;
    let inst_raw = render_entry(obj.get("instructions"));
    match qtype {
        "noul" => {
            let instructions = if inst_raw.is_empty() {
                NOUL_DEFAULT_INSTRUCTIONS.to_owned()
            } else {
                inst_raw
            };
            let crit = obj.get("criteria").and_then(Value::as_object);
            let t = crit
                .and_then(|c| c.get("true"))
                .map(|v| render_entry(Some(v)))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| NOUL_DEFAULT_TRUE.to_owned());
            let f = crit
                .and_then(|c| c.get("false"))
                .map(|v| render_entry(Some(v)))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| NOUL_DEFAULT_FALSE.to_owned());
            Ok(ParsedQuestion::Noul {
                instructions,
                true_desc: t,
                false_desc: f,
            })
        }
        "choice" => {
            let instructions = if inst_raw.is_empty() {
                "Which option applies to the state?".to_owned()
            } else {
                inst_raw
            };
            let crit = obj
                .get("criteria")
                .and_then(Value::as_object)
                .ok_or_else(|| "choice 问题必须提供对象类型的 criteria".to_owned())?;
            if crit.is_empty() {
                return Err("choice 问题的 criteria 不能为空".into());
            }
            let options = crit
                .iter()
                .map(|(k, v)| (k.clone(), render_entry(Some(v))))
                .collect();
            Ok(ParsedQuestion::Choice {
                instructions,
                options,
            })
        }
        "score" => {
            let instructions = if inst_raw.is_empty() {
                "Which level describes the state?".to_owned()
            } else {
                inst_raw
            };
            let arr = obj
                .get("criteria")
                .and_then(Value::as_array)
                .ok_or_else(|| "score 问题必须提供数组类型的 criteria".to_owned())?;
            if arr.is_empty() {
                return Err("score 问题的 criteria 不能为空".into());
            }
            let levels = arr
                .iter()
                .map(|v| (v.clone(), render_entry(Some(v))))
                .collect();
            Ok(ParsedQuestion::Score {
                instructions,
                levels,
            })
        }
        other => Err(format!("不支持的 question 类型：{other}")),
    }
}

fn format_options_block(names: &[String], descs: &[String]) -> String {
    let slots = slot_labels();
    names
        .iter()
        .zip(descs.iter())
        .zip(slots.iter())
        .map(|((name, desc), slot)| {
            if desc.is_empty() {
                format!("{slot}. {name}")
            } else {
                format!("{slot}. {name}: {desc}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_suffix(
    kind: &'static str,
    instructions: &str,
    names: &[String],
    descs: &[String],
) -> String {
    let slots = slot_labels();
    let letters: Vec<&str> = slots[..names.len()].iter().map(String::as_str).collect();
    let header = if kind == "score" {
        "Rate the state:"
    } else {
        "Question:"
    };
    let answer_line = if letters.len() <= LETTERS.len() {
        format!("Answer with one letter: {}.", letters.join(", "))
    } else {
        format!("Answer with one label: {}.", letters.join(", "))
    };
    format!(
        "{header} {instructions}\n\nOptions:\n{}\n\n{answer_line}",
        format_options_block(names, descs)
    )
}

fn render_branches(
    questions: &Map<String, Value>,
    max_slots: usize,
) -> Result<(Vec<RenderedBranch>, BTreeMap<String, Vec<String>>), String> {
    let mut branches = Vec::new();
    let mut choice_orders = BTreeMap::new();
    for (qid, raw_q) in questions {
        match parse_question(raw_q)? {
            ParsedQuestion::Noul {
                instructions,
                true_desc,
                false_desc,
            } => {
                let names = vec!["yes".to_owned(), "no".to_owned()];
                let descs = vec![true_desc, false_desc];
                let suffix = build_suffix("noul", &instructions, &names, &descs);
                branches.push(RenderedBranch {
                    qid: qid.clone(),
                    kind: "noul",
                    labels: names,
                    raw_levels: Vec::new(),
                    suffix,
                    chunk_index: None,
                });
            }
            ParsedQuestion::Choice {
                instructions,
                options,
            } => {
                let all_keys: Vec<String> = options.iter().map(|(k, _)| k.clone()).collect();
                choice_orders.insert(qid.clone(), all_keys);
                if options.len() <= max_slots {
                    let names: Vec<String> = options.iter().map(|(k, _)| k.clone()).collect();
                    let descs: Vec<String> = options.iter().map(|(_, d)| d.clone()).collect();
                    let suffix = build_suffix("choice", &instructions, &names, &descs);
                    branches.push(RenderedBranch {
                        qid: qid.clone(),
                        kind: "choice",
                        labels: names,
                        raw_levels: Vec::new(),
                        suffix,
                        chunk_index: None,
                    });
                } else {
                    let cap = max_slots.saturating_sub(1).max(1);
                    let n_chunks = options.len().div_ceil(cap);
                    let chunk_size = options.len().div_ceil(n_chunks);
                    for (ci, chunk) in options.chunks(chunk_size).enumerate() {
                        let mut names: Vec<String> = chunk.iter().map(|(k, _)| k.clone()).collect();
                        let mut descs: Vec<String> = chunk.iter().map(|(_, d)| d.clone()).collect();
                        names.push(NONE_OPTION.to_owned());
                        descs.push(NONE_DESCRIPTION.to_owned());
                        let suffix = build_suffix("choice", &instructions, &names, &descs);
                        branches.push(RenderedBranch {
                            qid: qid.clone(),
                            kind: "choice",
                            labels: names,
                            raw_levels: Vec::new(),
                            suffix,
                            chunk_index: Some(ci),
                        });
                    }
                }
            }
            ParsedQuestion::Score {
                instructions,
                levels,
            } => {
                if levels.len() > max_slots {
                    return Err(format!(
                        "score 等级数 {} 超过上限 {max_slots}",
                        levels.len()
                    ));
                }
                let names: Vec<String> = (0..levels.len()).map(|i| format!("level {i}")).collect();
                let descs: Vec<String> = levels.iter().map(|(_, d)| d.clone()).collect();
                let raw_levels: Vec<Value> = levels.into_iter().map(|(v, _)| v).collect();
                let labels: Vec<String> = (0..names.len()).map(|i| i.to_string()).collect();
                let suffix = build_suffix("score", &instructions, &names, &descs);
                branches.push(RenderedBranch {
                    qid: qid.clone(),
                    kind: "score",
                    labels,
                    raw_levels,
                    suffix,
                    chunk_index: None,
                });
            }
        }
    }
    Ok((branches, choice_orders))
}

fn render_chatml_prompt(user_content: &str) -> String {
    format!(
        "<|im_start|>system\n{SYSTEM_PROMPT}<|im_end|>\n<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n",
        user_content.trim()
    )
}

fn render_full_prompt(
    state: &Value,
    suffix: &str,
    media_marker: Option<&str>,
    media_len: usize,
) -> Result<String, String> {
    let state_str = render_state(state);
    let raw_user = format!("<state>\n{state_str}\n</state>\n\n{suffix}");
    if media_len == 0 {
        return Ok(render_chatml_prompt(&raw_user));
    }
    let marker = media_marker.unwrap_or("<__media__>");
    let mut replaced = String::with_capacity(raw_user.len() + media_len * marker.len());
    let mut seen = Vec::new();
    let mut rest = raw_user.as_str();
    while let Some(idx) = rest.find("<image:") {
        replaced.push_str(&rest[..idx]);
        let after = &rest[idx + "<image:".len()..];
        let Some(end) = after.find('>') else {
            return Err("未闭合的 <image:N> 占位符".into());
        };
        let n: usize = after[..end]
            .parse()
            .map_err(|_| format!("无效的图片占位符序号：{}", &after[..end]))?;
        if n < 1 || n > media_len {
            return Err(format!("<image:{n}> 超出 media 列表长度 {media_len}"));
        }
        seen.push(n);
        replaced.push_str(marker);
        rest = &after[end + 1..];
    }
    replaced.push_str(rest);
    let expected: Vec<usize> = (1..=media_len).collect();
    if seen != expected {
        return Err(format!(
            "media 图片须在 state 中按顺序由 <image:1>..<image:{media_len}> 各引用一次"
        ));
    }
    Ok(render_chatml_prompt(&replaced))
}

fn softmax(logits: &[f64]) -> Vec<f64> {
    if logits.is_empty() {
        return Vec::new();
    }
    let max_z = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = logits.iter().map(|&x| (x - max_z).exp()).collect();
    let sum: f64 = exps.iter().sum();
    if sum <= 0.0 || !sum.is_finite() {
        return vec![1.0 / (logits.len() as f64); logits.len()];
    }
    exps.into_iter().map(|w| w / sum).collect()
}

fn normalize_probs(probs: &[f64]) -> Vec<f64> {
    let sum: f64 = probs.iter().sum();
    if sum <= 0.0 || !sum.is_finite() {
        return vec![1.0 / (probs.len() as f64); probs.len()];
    }
    probs.iter().map(|&p| p / sum).collect()
}

fn round6(x: f64) -> f64 {
    (x * 1_000_000.0).round() / 1_000_000.0
}

fn choice_confidence(probs: &[f64]) -> f64 {
    if probs.len() <= 1 {
        return 1.0;
    }
    let p = normalize_probs(probs);
    let uniform = 1.0 / (p.len() as f64);
    let peak = p.iter().copied().fold(0.0_f64, f64::max);
    ((peak - uniform) / (1.0 - uniform)).clamp(0.0, 1.0)
}

fn score_confidence(probs: &[f64]) -> f64 {
    if probs.len() <= 1 {
        return 1.0;
    }
    let p = normalize_probs(probs);
    let k = p.len();
    let mut mode = 0usize;
    for i in 1..k {
        if p[i] > p[mode] {
            mode = i;
        }
    }
    let distance: f64 = p
        .iter()
        .enumerate()
        .map(|(i, &pi)| pi * ((i as f64) - (mode as f64)).abs())
        .sum();
    let center = ((k - 1) as f64) / 2.0;
    let uniform_mad: f64 = (0..k).map(|i| ((i as f64) - center).abs()).sum::<f64>() / (k as f64);
    if uniform_mad <= 0.0 {
        1.0
    } else {
        (1.0 - distance / uniform_mad).clamp(0.0, 1.0)
    }
}

fn make_noul_answer(p_yes: f64) -> Value {
    let p = round6(p_yes.clamp(0.0, 1.0));
    let conf = round6((2.0 * (p - 0.5).abs()).clamp(0.0, 1.0));
    json!({
        "type": "noul",
        "noul": p,
        "confidence": conf,
        "x_jev_confidence": conf,
    })
}

fn make_choice_answer(options: &[String], probs: &[f64]) -> Value {
    let p = normalize_probs(probs);
    let mut best = 0usize;
    for i in 1..p.len() {
        if p[i] > p[best] {
            best = i;
        }
    }
    let mut prob_map = Map::new();
    for (opt, &pi) in options.iter().zip(p.iter()) {
        prob_map.insert(opt.clone(), json!(round6(pi)));
    }
    let conf = round6(choice_confidence(&p));
    json!({
        "type": "choice",
        "choice": options[best],
        "probabilities": prob_map,
        "confidence": conf,
        "x_jev_confidence": conf,
    })
}

fn make_score_answer(levels: &[Value], probs: &[f64]) -> Value {
    let p = normalize_probs(probs);
    let expected: f64 = p.iter().enumerate().map(|(i, &pi)| (i as f64) * pi).sum();
    let mut legend = Map::new();
    let mut prob_map = Map::new();
    for (i, (lvl, &pi)) in levels.iter().zip(p.iter()).enumerate() {
        let key = i.to_string();
        legend.insert(key.clone(), lvl.clone());
        prob_map.insert(key, json!(round6(pi)));
    }
    let conf = round6(score_confidence(&p));
    json!({
        "type": "score",
        "score": round6(expected),
        "legend": legend,
        "probabilities": prob_map,
        "confidence": conf,
        "x_jev_confidence": conf,
    })
}

fn parse_inline_media(
    media_val: Option<&Value>,
    vision_supported: bool,
) -> Result<Vec<String>, String> {
    let Some(val) = media_val else {
        return Ok(Vec::new());
    };
    if val.is_null() {
        return Ok(Vec::new());
    }
    let items = val
        .as_array()
        .ok_or_else(|| "media 必须是数组".to_owned())?;
    if items.is_empty() {
        return Ok(Vec::new());
    }
    if !vision_supported {
        return Err("当前加载的模型未启用视觉投影器（mmproj），不支持输入图片".into());
    }
    let mut images = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        let obj = item
            .as_object()
            .ok_or_else(|| format!("media[{idx}] 必须是对象"))?;
        let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
        if kind != "image" {
            return Err(format!(
                "本地 OneJev 网关仅支持 image 类型 media（收到 {kind:?}）"
            ));
        }
        let raw_data = obj.get("data").and_then(Value::as_str).ok_or_else(|| {
            format!("media[{idx}] 须提供内联 base64 data 字段（禁止外发未知 URL）")
        })?;
        let b64 = match raw_data.split_once(',') {
            Some((head, rest)) if head.starts_with("data:") => rest.trim(),
            _ => raw_data.trim(),
        };
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|_| format!("media[{idx}] 的 base64 图片数据无效"))?;
        if decoded.is_empty() {
            return Err(format!("media[{idx}] 图片数据为空"));
        }
        images.push(base64::engine::general_purpose::STANDARD.encode(decoded));
    }
    Ok(images)
}

// ============================================================================
// OneJevGateway：托管 llama-server 子进程并在 127.0.0.1:18090 暴露 /v1/systemone
// ============================================================================

struct UpstreamClient {
    base_url: String,
    http: reqwest::blocking::Client,
    slot_ids: Vec<i64>,
    vision: bool,
    media_marker: String,
    lock: Mutex<()>,
}

impl UpstreamClient {
    fn connect(base_url: String) -> Result<Self, String> {
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(45))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("创建本地推理客户端失败：{e}"))?;

        let props: Value = http
            .get(format!("{base_url}/props"))
            .send()
            .map_err(|e| format!("读取 llama-server 属性失败：{e}"))?
            .json()
            .unwrap_or_else(|_| json!({}));
        let vision = props
            .pointer("/modalities/vision")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let media_marker = props
            .get("media_marker")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("<__media__>")
            .to_owned();

        let mut client = Self {
            base_url,
            http,
            slot_ids: Vec::new(),
            vision,
            media_marker,
            lock: Mutex::new(()),
        };
        client.slot_ids = client.verify_slots()?;
        Ok(client)
    }

    fn tokenize(&self, content: &str, parse_special: bool) -> Result<Vec<i64>, String> {
        let resp = self
            .http
            .post(format!("{}/tokenize", self.base_url))
            .json(&json!({
                "content": content,
                "add_special": false,
                "parse_special": parse_special,
            }))
            .send()
            .map_err(|e| format!("llama-server 分词请求失败：{e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "llama-server /tokenize 返回 HTTP {}",
                resp.status().as_u16()
            ));
        }
        let body: Value = resp.json().map_err(|e| format!("解析分词结果失败：{e}"))?;
        let arr = body
            .get("tokens")
            .and_then(Value::as_array)
            .ok_or_else(|| "分词响应缺少 tokens 数组".to_owned())?;
        arr.iter()
            .map(|v| v.as_i64().ok_or_else(|| "token id 非整数".to_owned()))
            .collect()
    }

    fn verify_slots(&self) -> Result<Vec<i64>, String> {
        let labels = slot_labels();
        let mut ids = Vec::with_capacity(labels.len());
        for label in &labels[..LETTERS.len()] {
            let toks = self.tokenize(label, false)?;
            if toks.len() != 1 {
                return Err(format!(
                    "基础选项槽位 {label} 不是单 token（长度 {}）",
                    toks.len()
                ));
            }
            ids.push(toks[0]);
        }
        let mut extra = Vec::new();
        let mut all_extra_single = true;
        for label in &labels[LETTERS.len()..] {
            let toks = self.tokenize(label, false)?;
            if toks.len() == 1 {
                extra.push(toks[0]);
            } else {
                all_extra_single = false;
                break;
            }
        }
        if all_extra_single {
            ids.extend(extra);
        }
        let uniq: HashSet<i64> = ids.iter().copied().collect();
        if uniq.len() != ids.len() {
            return Err("选项槽位 token id 发生冲突".into());
        }
        Ok(ids)
    }

    fn post_completion(&self, body: &Value) -> Result<Vec<Value>, String> {
        let resp = self
            .http
            .post(format!("{}/completion", self.base_url))
            .json(body)
            .send()
            .map_err(|e| format!("llama-server 推理失败：{e}"))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = resp.text().unwrap_or_default();
            return Err(format!(
                "llama-server /completion 返回 HTTP {status}: {}",
                text.chars().take(200).collect::<String>()
            ));
        }
        let parsed: Value = resp.json().map_err(|e| format!("解析推理输出失败：{e}"))?;
        let mut items = match parsed {
            Value::Array(arr) => arr,
            other => vec![other],
        };
        items.sort_by_key(|item| item.get("index").and_then(Value::as_u64).unwrap_or(0));
        Ok(items)
    }

    fn evaluate(
        &self,
        model_name: &str,
        state: &Value,
        questions: &Map<String, Value>,
        media: Option<&Value>,
        abandoned: &dyn Fn() -> bool,
    ) -> Result<(u16, Value), String> {
        let t0 = Instant::now();
        let images = match parse_inline_media(media, self.vision) {
            Ok(imgs) => imgs,
            Err(msg) => return Ok((422, json!({ "detail": msg }))),
        };
        let (branches, choice_orders) = match render_branches(questions, self.slot_ids.len()) {
            Ok(pair) => pair,
            Err(msg) => return Ok((422, json!({ "detail": msg }))),
        };
        if branches.is_empty() {
            return Ok((422, json!({ "detail": "questions 不能为空" })));
        }

        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        // 2026-11-07：网关单槽串行推理。排队期间调用方已放弃（预热被新的中间结果取代、超时）的请求直接丢弃，
        // 不再占用推理，避免正式轮次排在一串作废请求之后超时并触发熔断。
        if abandoned() {
            return Ok((
                CLIENT_GONE_STATUS,
                json!({ "detail": "client disconnected" }),
            ));
        }
        let (prompts, prefix_prompt, total_tokens) = if images.is_empty() {
            let mut full_ids = Vec::with_capacity(branches.len());
            for br in &branches {
                let text = match render_full_prompt(state, &br.suffix, None, 0) {
                    Ok(t) => t,
                    Err(msg) => return Ok((422, json!({ "detail": msg }))),
                };
                full_ids.push(self.tokenize(&text, true)?);
            }
            let marker_text = render_full_prompt(state, "QEV_PREFIX_MARK", None, 0)?;
            let cut = marker_text.find("QEV_PREFIX_MARK").unwrap_or(0);
            let mut prefix_ids = self.tokenize(&marker_text[..cut], true)?;
            while !prefix_ids.is_empty()
                && !full_ids.iter().all(|ids| {
                    ids.len() > prefix_ids.len() && ids[..prefix_ids.len()] == prefix_ids[..]
                })
            {
                prefix_ids.pop();
            }
            let suffix_sum: usize = full_ids
                .iter()
                .map(|ids| ids.len() - prefix_ids.len())
                .sum();
            let longest = full_ids.iter().map(Vec::len).max().unwrap_or(0);
            let total = prefix_ids.len() + suffix_sum;
            if longest > DEFAULT_CTX_TOKENS || total > MAX_REQUEST_TOKENS {
                return Ok((
                    422,
                    json!({ "detail": format!("请求长度超限（最长分支 {longest} tokens，总计 {total} tokens）") }),
                ));
            }
            let prompt_vals: Vec<Value> = full_ids.into_iter().map(|ids| json!(ids)).collect();
            (prompt_vals, json!(prefix_ids), total)
        } else {
            let marker_prompt = match render_full_prompt(
                state,
                "QEV_PREFIX_MARK",
                Some(&self.media_marker),
                images.len(),
            ) {
                Ok(t) => t,
                Err(msg) => return Ok((422, json!({ "detail": msg }))),
            };
            let Some((head, tail)) = marker_prompt.split_once("QEV_PREFIX_MARK") else {
                return Err("内部前缀切分标记丢失".into());
            };
            let prefix_obj = json!({
                "prompt_string": head,
                "multimodal_data": images,
            });
            let prompt_vals: Vec<Value> = branches
                .iter()
                .map(|br| {
                    json!({
                        "prompt_string": format!("{head}{}{tail}", br.suffix),
                        "multimodal_data": images,
                    })
                })
                .collect();
            (prompt_vals, prefix_obj, branches.len() * 256)
        };

        if abandoned() {
            return Ok((
                CLIENT_GONE_STATUS,
                json!({ "detail": "client disconnected" }),
            ));
        }
        let mut pin_slot: Option<i64> = None;
        if prompts.len() > 1 {
            let prefill = self.post_completion(&json!({
                "prompt": [prefix_prompt],
                "n_predict": 0,
                "cache_prompt": true,
            }))?;
            pin_slot = prefill
                .first()
                .and_then(|v| v.get("id_slot"))
                .and_then(Value::as_i64);
        }

        let max_slots_needed = branches.iter().map(|b| b.labels.len()).max().unwrap_or(2);
        let n_top = 64usize.max(2 * max_slots_needed);
        let mut batch_req = json!({
            "prompt": prompts,
            "n_predict": 1,
            "cache_prompt": true,
            "temperature": 0.0,
            "n_probs": n_top,
        });
        if let Some(slot) = pin_slot {
            batch_req["id_slot"] = json!(slot);
        }
        let batch_out = self.post_completion(&batch_req)?;
        if batch_out.len() != branches.len() {
            return Err(format!(
                "llama-server 返回分支数不匹配（{} != {}）",
                batch_out.len(),
                branches.len()
            ));
        }

        let mut branch_probs: Vec<Vec<f64>> = Vec::with_capacity(branches.len());
        for (idx, (br, item)) in branches.iter().zip(batch_out.iter()).enumerate() {
            let slots = &self.slot_ids[..br.labels.len()];
            let top_arr = item
                .pointer("/completion_probabilities/0/top_logprobs")
                .and_then(Value::as_array);
            let mut logprob_by_id = BTreeMap::new();
            if let Some(arr) = top_arr {
                for entry in arr {
                    if let (Some(id), Some(lp)) = (
                        entry.get("id").and_then(Value::as_i64),
                        entry.get("logprob").and_then(Value::as_f64),
                    ) {
                        logprob_by_id.insert(id, lp);
                    }
                }
            }
            let logits = if slots.iter().all(|s| logprob_by_id.contains_key(s)) {
                slots.iter().map(|s| logprob_by_id[s]).collect::<Vec<f64>>()
            } else {
                let bias: Vec<Value> = slots.iter().map(|&s| json!([s, 100.0])).collect();
                let mut fb_req = json!({
                    "prompt": [prompts[idx].clone()],
                    "n_predict": 1,
                    "cache_prompt": true,
                    "n_probs": slots.len(),
                    "post_sampling_probs": true,
                    "samplers": ["temperature"],
                    "temperature": 1.0,
                    "logit_bias": bias,
                });
                if let Some(slot) = pin_slot {
                    fb_req["id_slot"] = json!(slot);
                }
                let fb = self.post_completion(&fb_req)?;
                let mut prob_by_id = BTreeMap::new();
                if let Some(arr) = fb
                    .first()
                    .and_then(|v| v.pointer("/completion_probabilities/0/top_probs"))
                    .and_then(Value::as_array)
                {
                    for entry in arr {
                        if let (Some(id), Some(p)) = (
                            entry.get("id").and_then(Value::as_i64),
                            entry.get("prob").and_then(Value::as_f64),
                        ) {
                            prob_by_id.insert(id, p);
                        }
                    }
                }
                slots
                    .iter()
                    .map(|s| prob_by_id.get(s).copied().unwrap_or(0.0).max(1e-30).ln())
                    .collect::<Vec<f64>>()
            };
            branch_probs.push(softmax(&logits));
        }

        let mut answers = Map::new();
        let mut chunk_accum: BTreeMap<String, (BTreeMap<String, (f64, f64)>, Vec<f64>)> =
            BTreeMap::new();

        for (br, probs) in branches.iter().zip(branch_probs.iter()) {
            if br.chunk_index.is_some() {
                let entry = chunk_accum
                    .entry(br.qid.clone())
                    .or_insert_with(|| (BTreeMap::new(), Vec::new()));
                let none_idx = br
                    .labels
                    .iter()
                    .position(|l| l == NONE_OPTION)
                    .unwrap_or(probs.len() - 1);
                let keep = (1.0 - probs[none_idx]).max(1e-9);
                for (lab, &p) in br.labels.iter().zip(probs.iter()) {
                    if lab != NONE_OPTION {
                        entry.0.insert(lab.clone(), (p / keep, keep));
                    }
                }
                entry.1.push(keep);
                continue;
            }
            let ans = match br.kind {
                "noul" => make_noul_answer(probs[0]),
                "choice" => make_choice_answer(&br.labels, probs),
                _ => make_score_answer(&br.raw_levels, probs),
            };
            answers.insert(br.qid.clone(), ans);
        }

        for (qid, (scores, weights)) in chunk_accum {
            let total_w: f64 = weights.iter().sum::<f64>().max(1e-12);
            let names = choice_orders.get(&qid).cloned().unwrap_or_default();
            let probs: Vec<f64> = names
                .iter()
                .map(|name| {
                    scores
                        .get(name)
                        .map(|&(cond, keep)| cond * (keep / total_w))
                        .unwrap_or(0.0)
                })
                .collect();
            answers.insert(qid, make_choice_answer(&names, &probs));
        }

        let elapsed_s = t0.elapsed().as_secs_f64();
        Ok((
            200,
            json!({
                "id": format!("onejev-{}", t0.elapsed().as_nanos()),
                "model": model_name,
                "answers": answers,
                "usage": {
                    "input_tokens": total_tokens,
                    "output_tokens": questions.len(),
                },
                "qev": {
                    "prompt_version": PROMPT_VERSION,
                    "fork_mode": "llama.cpp",
                    "branches": branches.len(),
                    "timing": { "total_s": elapsed_s },
                }
            }),
        ))
    }
}

/// 本地 OneJev 在语音链路里的等待预算（2026-11-07 用户决定）。
///
/// - `hold_ms`：静音判停后“这句话是不是没说完”的判断最多等多久；为 `None` 时不做这项判断。
///   必须短于会话续听窗口（1200ms），否则结果回来时窗口已过，只会白白多等。9B 来不及，固定关闭。
/// - 没有显卡加速时 4B / 9B 基本赶不上语音节奏：不再加长等待（否则每句话都更慢），
///   改为关闭“没说完”判断并在界面提示改用 0.8B。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalJevBudget {
    pub turn_ms: u64,
    pub hold_ms: Option<u64>,
    pub notice: Option<&'static str>,
}

pub const NOTICE_9B_NO_HOLD: &str =
    "9B 推理较慢，来不及在续听窗口内判断“没说完”，已自动关闭这一项（其余筛选照常）";
pub const NOTICE_SLOW_ON_CPU: &str =
    "当前没有显卡加速，4B / 9B 推理较慢，语音筛选经常来不及而被自动跳过；建议改用 0.8B";

/// 按模型档位与是否有显卡加速给出等待预算；Laya 等非 OneJev 模型返回 `None`（沿用默认值）。
pub fn local_jev_budget(model_id: &str, gpu: bool) -> Option<LocalJevBudget> {
    let budget = match (model_id, gpu) {
        ("onejev_1_6_0_8b", true) => LocalJevBudget {
            turn_ms: 1500,
            hold_ms: Some(600),
            notice: None,
        },
        ("onejev_1_6_0_8b", false) => LocalJevBudget {
            turn_ms: 2000,
            hold_ms: Some(800),
            notice: None,
        },
        ("onejev_1_6_4b", true) => LocalJevBudget {
            turn_ms: 2500,
            hold_ms: Some(800),
            notice: None,
        },
        ("onejev_1_6_4b", false) => LocalJevBudget {
            turn_ms: 2500,
            hold_ms: None,
            notice: Some(NOTICE_SLOW_ON_CPU),
        },
        ("onejev_1_6_9b", true) => LocalJevBudget {
            turn_ms: 3500,
            hold_ms: None,
            notice: Some(NOTICE_9B_NO_HOLD),
        },
        ("onejev_1_6_9b", false) => LocalJevBudget {
            turn_ms: 3500,
            hold_ms: None,
            notice: Some(NOTICE_SLOW_ON_CPU),
        },
        _ => return None,
    };
    Some(budget)
}

/// 未启动推理进程前，按推理引擎文件与平台预估是否会走显卡加速（与 `detect_onejev_env` 同一口径）。
pub fn expected_gpu(models_root: &Path) -> bool {
    detect_onejev_env(models_root, false)
        .get("accelerator")
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "metal" | "cuda" | "vulkan"))
}

/// 从 `ps -axo pid=,ppid=,command=` 的输出里挑出 MOSS 自己拉起、父进程已不在（被 init/launchd 收养）
/// 的 OneJev 推理进程：命令行同时含 `llama-server`、`OneJev1.6-` 权重名与 MOSS 的模型目录。
fn orphan_llama_pids(ps_output: &str, model_roots: &[String]) -> Vec<u32> {
    ps_output
        .lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (ppid, command) = rest.trim_start().split_once(char::is_whitespace)?;
            let pid: u32 = pid.parse().ok()?;
            let ppid: u32 = ppid.parse().ok()?;
            let command = command.trim_start();
            let ours = command.contains("llama-server")
                && command.contains("OneJev1.6-")
                && model_roots
                    .iter()
                    .any(|root| !root.is_empty() && command.contains(root.as_str()));
            (ppid == 1 && ours).then_some(pid)
        })
        .collect()
}

/// 清理上次 MOSS 崩溃或被强制结束后遗留的 OneJev 推理进程，返回结束的个数。
/// Windows 由 Job Object 随 MOSS 一起结束，这里只处理 macOS / Linux；只认 MOSS 模型目录里的 OneJev 权重，
/// 绝不碰用户自己运行的其他 `llama-server`。
pub fn reap_orphaned_llama_servers(model_roots: &[PathBuf]) -> usize {
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
        let roots: Vec<String> = model_roots
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect();
        let pids = orphan_llama_pids(&String::from_utf8_lossy(&out.stdout), &roots);
        let mut killed = 0;
        for pid in pids {
            let ok = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if ok {
                killed += 1;
            }
        }
        killed
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = model_roots;
        0
    }
}

pub struct OneJevGateway {
    model_id: String,
    accelerator: DetectedAccelerator,
    listen_addr: SocketAddr,
    stop_flag: Arc<AtomicBool>,
    server_thread: Option<std::thread::JoinHandle<()>>,
    llama_child: Arc<Mutex<Option<Child>>>,
    _job: Option<JobHandle>,
    log: LlamaLog,
}

/// `llama-server` 错误输出的最近几行（只在内存里，按行数与长度封顶），用于启动失败与意外退出时说明原因。
#[derive(Clone, Default)]
pub struct LlamaLog(Arc<Mutex<std::collections::VecDeque<String>>>);

const LLAMA_LOG_LINES: usize = 40;
const LLAMA_LOG_LINE_CHARS: usize = 240;

impl LlamaLog {
    fn push(&self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let Ok(mut lines) = self.0.lock() else { return };
        if lines.len() >= LLAMA_LOG_LINES {
            lines.pop_front();
        }
        lines.push_back(line.chars().take(LLAMA_LOG_LINE_CHARS).collect());
    }

    /// 持续读完子进程 stderr（读到 EOF 即子进程已退出），避免管道写满阻塞推理进程。
    fn drain<R: Read + Send + 'static>(&self, pipe: R) {
        let log = self.clone();
        let _ = std::thread::Builder::new()
            .name("onejev-llama-log".into())
            .spawn(move || {
                let mut reader = BufReader::new(pipe);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => log.push(&String::from_utf8_lossy(&buf)),
                    }
                }
            });
    }

    /// 最近几行里最能说明问题的：优先带 error / failed 字样的行，否则取最后几行。
    pub fn summary(&self, max_lines: usize) -> String {
        let Ok(lines) = self.0.lock() else {
            return String::new();
        };
        let lower_hit = |l: &String| {
            let l = l.to_ascii_lowercase();
            l.contains("error")
                || l.contains("failed")
                || l.contains("unable")
                || l.contains("out of memory")
        };
        let picked: Vec<&String> = {
            let hits: Vec<&String> = lines.iter().filter(|l| lower_hit(l)).collect();
            if hits.is_empty() {
                lines.iter().collect()
            } else {
                hits
            }
        };
        let start = picked.len().saturating_sub(max_lines);
        picked[start..]
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

fn with_log_hint(message: String, log: &LlamaLog) -> String {
    let tail = log.summary(3);
    if tail.is_empty() {
        message
    } else {
        format!("{message}。推理进程输出：{tail}")
    }
}

impl OneJevGateway {
    pub fn start(
        model_id: &str,
        model_dir: &Path,
        models_root: &Path,
        listen_addr: SocketAddr,
        mirror_hint: Option<&str>,
    ) -> Result<Self, String> {
        let cancel = AtomicBool::new(false);
        Self::start_with_cancel(
            model_id,
            model_dir,
            models_root,
            listen_addr,
            mirror_hint,
            &cancel,
        )
    }

    pub fn start_with_cancel(
        model_id: &str,
        model_dir: &Path,
        models_root: &Path,
        listen_addr: SocketAddr,
        mirror_hint: Option<&str>,
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        if cancel.load(Ordering::SeqCst) {
            return Err("本地 OneJev 启动已取消".into());
        }
        let (gguf_name, mmproj_name) = match model_id {
            "onejev_1_6_0_8b" => (
                "OneJev1.6-0.8B.Q8_0.gguf",
                "OneJev1.6-0.8B.mmproj-Q8_0.gguf",
            ),
            "onejev_1_6_4b" => ("OneJev1.6-4B.Q4_K_M.gguf", "OneJev1.6-4B.mmproj-Q8_0.gguf"),
            "onejev_1_6_9b" => ("OneJev1.6-9B.Q4_K_M.gguf", "OneJev1.6-9B.mmproj-Q8_0.gguf"),
            other => return Err(format!("未知的 OneJev 模型编号：{other}")),
        };
        let gguf_path = model_dir.join(gguf_name);
        let mmproj_path = model_dir.join(mmproj_name);
        if !gguf_path.is_file() {
            return Err(format!("缺少模型权重文件：{}", gguf_path.display()));
        }

        // 先做一次瞬时回环端口探测并立即释放：若 18090 已被外部服务占用可立刻报错，
        // 同时不在下载/加载数 GB 权重期间挂着未 accept 的 LISTEN 队列（否则请求会卡满超时并触发熔断，而非 <1ms ECONNREFUSED 放行）
        if listen_addr.port() != 0 {
            drop(
                TcpListener::bind(listen_addr)
                    .map_err(|err| format!("绑定本地 Jev 端口 {listen_addr} 失败：{err}"))?,
            );
        }

        let llama_bin = ensure_llama_runtime(models_root, mirror_hint, cancel, &|_| {})?;
        if cancel.load(Ordering::SeqCst) {
            return Err("本地 OneJev 启动已取消".into());
        }
        let accel = probe_llama_accelerator_with_cancel(&llama_bin, true, Some(cancel));
        if cancel.load(Ordering::SeqCst) {
            return Err("本地 OneJev 启动已取消".into());
        }

        let llama_port = {
            let sock = TcpListener::bind(("127.0.0.1", 0))
                .map_err(|e| format!("分配本地推理端口失败：{e}"))?;
            sock.local_addr()
                .map_err(|e| format!("读取本地推理端口失败：{e}"))?
                .port()
        };

        let mut cmd = Command::new(&llama_bin);
        cmd.args([
            "--host",
            "127.0.0.1",
            "--port",
            &llama_port.to_string(),
            "-c",
            &DEFAULT_CTX_TOKENS.to_string(),
            "-np",
            "1",
            "--image-min-tokens",
            "64",
            "--image-max-tokens",
            "4096",
            "-m",
        ])
        .arg(&gguf_path);

        if mmproj_path.is_file() {
            cmd.arg("--mmproj").arg(&mmproj_path);
        }
        if accel.kind.uses_gpu() {
            cmd.args(["-ngl", "all", "-fa", "auto"]);
        } else {
            cmd.args(["-ngl", "0"]);
            if mmproj_path.is_file() {
                cmd.arg("--no-mmproj-offload");
            }
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        configure_no_window(&mut cmd);

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动 OneJev llama-server 失败：{e}"))?;
        let job = JobHandle::for_child(&child);
        let log = LlamaLog::default();
        if let Some(stderr) = child.stderr.take() {
            log.drain(stderr);
        }

        let upstream_addr = SocketAddr::from(([127, 0, 0, 1], llama_port));
        let load_timeout = if model_id == "onejev_1_6_9b" {
            Duration::from_secs(60)
        } else if model_id == "onejev_1_6_4b" {
            Duration::from_secs(45)
        } else {
            Duration::from_secs(30)
        };
        let deadline = Instant::now() + load_timeout;
        let mut ready = false;
        while Instant::now() < deadline {
            if cancel.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                return Err("本地 OneJev 启动已取消".into());
            }
            if let Ok(Some(status)) = child.try_wait() {
                // 给日志线程一点时间读完最后的错误输出
                std::thread::sleep(Duration::from_millis(100));
                return Err(with_log_hint(
                    format!(
                        "OneJev 底层推理进程提前退出（状态码 {:?}，加速模式 {}）",
                        status.code(),
                        accel.display_text()
                    ),
                    &log,
                ));
            }
            if probe_http_health(upstream_addr, Duration::from_millis(150)) {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(60));
        }
        if !ready {
            let _ = child.kill();
            let _ = child.wait();
            return Err(with_log_hint(
                "OneJev 模型加载超时（首次加载权重到显存/统一内存较慢，请稍后重试）".into(),
                &log,
            ));
        }
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("本地 OneJev 启动已取消".into());
        }

        let upstream = match UpstreamClient::connect(format!("http://127.0.0.1:{llama_port}")) {
            Ok(client) => Arc::new(client),
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(err);
            }
        };

        let listener = match TcpListener::bind(listen_addr) {
            Ok(sock) => sock,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("绑定本地 Jev 端口 {listen_addr} 失败：{err}"));
            }
        };
        let bound_addr = listener.local_addr().unwrap_or(listen_addr);

        let child_arc = Arc::new(Mutex::new(Some(child)));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let mut gateway = Self::spawn_with_upstream(
            model_id.to_owned(),
            accel,
            listener,
            bound_addr,
            upstream,
            child_arc,
            job,
            stop_flag,
        )?;
        gateway.log = log;
        Ok(gateway)
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_with_upstream(
        model_id: String,
        accelerator: DetectedAccelerator,
        listener: TcpListener,
        bound_addr: SocketAddr,
        upstream: Arc<UpstreamClient>,
        llama_child: Arc<Mutex<Option<Child>>>,
        job: Option<JobHandle>,
        stop_flag: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let thread_stop = stop_flag.clone();
        let thread_child = llama_child.clone();
        let cleanup_child = llama_child.clone();
        let thread_model = model_id.clone();
        let thread_accel = accelerator.display_text();
        let handle = match std::thread::Builder::new()
            .name(format!("onejev-gateway-{model_id}"))
            .spawn(move || {
                run_gateway_loop(
                    listener,
                    upstream,
                    thread_child,
                    thread_stop,
                    &thread_model,
                    &thread_accel,
                );
            }) {
            Ok(h) => h,
            Err(e) => {
                if let Ok(mut guard) = cleanup_child.lock()
                    && let Some(mut child) = guard.take()
                {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                return Err(format!("启动 OneJev 网关线程失败：{e}"));
            }
        };

        Ok(Self {
            model_id,
            accelerator,
            listen_addr: bound_addr,
            stop_flag,
            server_thread: Some(handle),
            llama_child,
            _job: job,
            log: LlamaLog::default(),
        })
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn accelerator(&self) -> &DetectedAccelerator {
        &self.accelerator
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    /// 推理进程最近的错误输出摘要（意外退出后用来说明原因；没有输出时为空）。
    pub fn log_summary(&self) -> String {
        self.log.summary(3)
    }

    pub fn is_alive(&self) -> bool {
        if self.stop_flag.load(Ordering::SeqCst) {
            return false;
        }
        let Ok(mut guard) = self.llama_child.lock() else {
            return false;
        };
        match guard.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => true,
        }
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.listen_addr, Duration::from_millis(80));
        if let Some(handle) = self.server_thread.take() {
            let _ = handle.join();
        }
        if let Ok(mut guard) = self.llama_child.lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        self._job = None;
    }
}

impl Drop for OneJevGateway {
    fn drop(&mut self) {
        self.stop();
    }
}

fn probe_http_health(addr: SocketAddr, timeout: Duration) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 512];
    let Ok(n) = stream.read(&mut buf) else {
        return false;
    };
    let text = String::from_utf8_lossy(&buf[..n]);
    text.lines()
        .next()
        .is_some_and(|l| l.starts_with("HTTP/1.1 200") || l.starts_with("HTTP/1.0 200"))
        && text.contains("\"ok\"")
}

pub fn health_payload(model_id: &str, accel_label: &str) -> Value {
    json!({
        "status": "ok",
        "engine": "onejev",
        "model": model_id,
        "fork_mode": "llama.cpp",
        "prompt_version": PROMPT_VERSION,
        "accelerator": accel_label,
    })
}

struct ActiveConnGuard(Arc<AtomicUsize>);

impl Drop for ActiveConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn is_allowed_loopback_host(raw_host: &str) -> bool {
    let trimmed = raw_host.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Some(rest) = trimmed.strip_prefix("[::1]") {
        return rest.is_empty()
            || rest
                .strip_prefix(':')
                .is_some_and(|port| port.parse::<u16>().is_ok());
    }
    let (host_part, port_ok) = match trimmed.split_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().is_ok()),
        None => (trimmed, true),
    };
    port_ok && (host_part == "127.0.0.1" || host_part.eq_ignore_ascii_case("localhost"))
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    max_bytes: usize,
) -> Result<Option<String>, &'static str> {
    let mut buf = Vec::with_capacity(128);
    loop {
        let available = reader.fill_buf().map_err(|_| "header read error")?;
        if available.is_empty() {
            if buf.is_empty() {
                return Ok(None);
            }
            break;
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            if buf.len() + pos + 1 > max_bytes {
                return Err("header line too long");
            }
            buf.extend_from_slice(&available[..=pos]);
            reader.consume(pos + 1);
            break;
        }
        if buf.len() + available.len() > max_bytes {
            return Err("header line too long");
        }
        buf.extend_from_slice(available);
        let len = available.len();
        reader.consume(len);
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| "invalid header encoding")
}

fn run_gateway_loop(
    listener: TcpListener,
    upstream: Arc<UpstreamClient>,
    llama_child: Arc<Mutex<Option<Child>>>,
    stop_flag: Arc<AtomicBool>,
    model_id: &str,
    accel_label: &str,
) {
    let active_conns = Arc::new(AtomicUsize::new(0));
    while !stop_flag.load(Ordering::SeqCst) {
        let Ok((mut stream, _)) = listener.accept() else {
            if stop_flag.load(Ordering::SeqCst) {
                break;
            }
            continue;
        };
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }
        let mut admitted = false;
        let mut cur = active_conns.load(Ordering::Acquire);
        while cur < MAX_ACTIVE_GATEWAY_CONNS {
            match active_conns.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    admitted = true;
                    break;
                }
                Err(next) => cur = next,
            }
        }
        if !admitted {
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            write_json_response(
                &mut stream,
                503,
                &json!({ "status": "busy", "detail": "too many concurrent requests" }),
            );
            continue;
        }
        let guard = ActiveConnGuard(active_conns.clone());
        let upstream = upstream.clone();
        let llama_child = llama_child.clone();
        let model_id = model_id.to_owned();
        let accel_label = accel_label.to_owned();
        std::thread::spawn(move || {
            let _guard = guard;
            handle_gateway_conn(stream, &upstream, &llama_child, &model_id, &accel_label);
        });
    }
}

fn handle_gateway_conn(
    mut stream: TcpStream,
    upstream: &UpstreamClient,
    llama_child: &Mutex<Option<Child>>,
    model_id: &str,
    accel_label: &str,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let Ok(cloned) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(cloned);
    let request_line = match read_bounded_line(&mut reader, 4096) {
        Ok(Some(line)) => line,
        Ok(None) => return,
        Err(detail) => {
            write_json_response(&mut stream, 400, &json!({ "detail": detail }));
            return;
        }
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("/");
    let path = raw_path.split('?').next().unwrap_or(raw_path);

    let mut header_bytes = request_line.len();
    let mut header_lines = 0usize;
    let mut content_length = 0usize;
    let mut host_header: Option<String> = None;
    let mut has_origin = false;
    let mut sec_fetch_site: Option<String> = None;
    let mut has_transfer_encoding = false;

    loop {
        let line = match read_bounded_line(&mut reader, 4096) {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(detail) => {
                write_json_response(&mut stream, 400, &json!({ "detail": detail }));
                return;
            }
        };
        if line == "\r\n" || line == "\n" {
            break;
        }
        header_lines += 1;
        header_bytes += line.len();
        if header_lines > MAX_HTTP_HEADER_LINES || header_bytes > MAX_HTTP_HEADER_BYTES {
            write_json_response(
                &mut stream,
                431,
                &json!({ "detail": "request headers too large" }),
            );
            return;
        }
        if let Some((name, val)) = line.split_once(':') {
            let key = name.trim();
            let value = val.trim();
            if key.eq_ignore_ascii_case("content-length") {
                let Ok(parsed) = value.parse::<usize>() else {
                    write_json_response(
                        &mut stream,
                        400,
                        &json!({ "detail": "invalid content-length" }),
                    );
                    return;
                };
                content_length = parsed;
            } else if key.eq_ignore_ascii_case("host") {
                host_header = Some(value.to_owned());
            } else if key.eq_ignore_ascii_case("origin") && !value.is_empty() {
                has_origin = true;
            } else if key.eq_ignore_ascii_case("sec-fetch-site") {
                sec_fetch_site = Some(value.to_ascii_lowercase());
            } else if key.eq_ignore_ascii_case("transfer-encoding") {
                has_transfer_encoding = true;
            }
        }
    }

    // 回环免鉴权安全防护（对齐 safety.md §2）：在路由与读取请求体前拦截 DNS 重绑定与跨站浏览器请求
    if method.eq_ignore_ascii_case("OPTIONS") {
        write_json_response(
            &mut stream,
            403,
            &json!({ "detail": "preflight forbidden" }),
        );
        return;
    }
    if !host_header.as_deref().is_some_and(is_allowed_loopback_host) {
        write_json_response(&mut stream, 403, &json!({ "detail": "forbidden host" }));
        return;
    }
    if has_origin
        || sec_fetch_site
            .as_deref()
            .is_some_and(|site| !matches!(site, "none" | "same-origin"))
    {
        write_json_response(
            &mut stream,
            403,
            &json!({ "detail": "cross-origin requests forbidden" }),
        );
        return;
    }
    if has_transfer_encoding {
        write_json_response(
            &mut stream,
            400,
            &json!({ "detail": "transfer-encoding unsupported" }),
        );
        return;
    }

    if content_length > MAX_HTTP_BODY_BYTES {
        write_json_response(
            &mut stream,
            413,
            &json!({ "detail": "request body too large" }),
        );
        return;
    }

    let mut body = Vec::with_capacity(content_length.min(64 * 1024));
    if content_length > 0 {
        let mut limited = (&mut reader).take(content_length as u64);
        if limited.read_to_end(&mut body).is_err() || body.len() != content_length {
            write_json_response(
                &mut stream,
                400,
                &json!({ "detail": "incomplete request body" }),
            );
            return;
        }
    }

    let child_alive = llama_child
        .lock()
        .ok()
        .is_some_and(|mut g| match g.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => true,
        });
    if !child_alive {
        write_json_response(
            &mut stream,
            503,
            &json!({ "status": "error", "detail": "llama-server exited" }),
        );
        return;
    }

    match (method, path) {
        ("GET", "/health") => {
            write_json_response(&mut stream, 200, &health_payload(model_id, accel_label));
        }
        ("GET", "/v1/models") => {
            write_json_response(
                &mut stream,
                200,
                &json!({
                    "models": [
                        { "name": model_id, "description": "OneJev 1.6 local multimodal decision model", "release_date": "2026-10-17" },
                        { "name": "jev-latest", "description": format!("alias for {model_id}"), "release_date": "2026-10-17" },
                        { "name": "jev-preview", "description": format!("alias for {model_id}"), "release_date": "2026-10-17" },
                        { "name": "qev-latest", "description": format!("alias for {model_id}"), "release_date": "2026-10-17" },
                    ]
                }),
            );
        }
        ("POST", "/v1/systemone") => {
            let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
                write_json_response(&mut stream, 422, &json!({ "detail": "invalid JSON" }));
                return;
            };
            let requested_model = payload
                .get("model")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("jev-latest");
            let allowed = matches!(requested_model, "jev-latest" | "jev-preview" | "qev-latest")
                || requested_model == model_id
                || crate::settings::canonical_local_jev_model(requested_model) == Some(model_id);
            if !allowed {
                write_json_response(
                    &mut stream,
                    404,
                    &json!({ "detail": format!("model {requested_model:?} is not served by {model_id}") }),
                );
                return;
            }
            let state = payload.get("state").unwrap_or(&Value::Null);
            let Some(questions) = payload.get("questions").and_then(Value::as_object) else {
                write_json_response(
                    &mut stream,
                    422,
                    &json!({ "detail": "questions must be an object" }),
                );
                return;
            };
            let media = payload.get("media");
            let abandoned = || client_disconnected(&stream);
            match upstream.evaluate(model_id, state, questions, media, &abandoned) {
                Ok((CLIENT_GONE_STATUS, _)) => {}
                Ok((status, resp_json)) => write_json_response(&mut stream, status, &resp_json),
                Err(err) => {
                    write_json_response(&mut stream, 502, &json!({ "error": err, "detail": err }))
                }
            }
        }
        _ => {
            write_json_response(&mut stream, 404, &json!({ "detail": "not found" }));
        }
    }
}

/// 内部状态码：调用方已断开，不写响应。
const CLIENT_GONE_STATUS: u16 = 499;

/// 请求已完整读完后，调用方若已关闭连接（放弃等待），套接字上会读到 EOF 或连接重置。
/// 只做非阻塞 peek，不消费数据；无法判断时按“仍在等待”处理。
fn client_disconnected(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return false;
    }
    let mut buf = [0u8; 1];
    let gone = match stream.peek(&mut buf) {
        Ok(0) => true,
        Ok(_) => false,
        Err(err) => !matches!(
            err.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ),
    };
    let _ = stream.set_nonblocking(false);
    gone
}

fn write_json_response(stream: &mut TcpStream, status: u16, body: &Value) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let payload = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&payload);
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_labels_match_onejev_256_single_token_table() {
        let slots = slot_labels();
        assert_eq!(slots.len(), 256);
        assert_eq!(&slots[..3], &["A", "B", "C"]);
        assert_eq!(slots[25], "Z");
        assert_eq!(slots[26], "AA");
        assert!(!slots.iter().any(|s| s == "BQ" || s == "CQ" || s == "JQ"));
    }

    #[test]
    fn prompt_rendering_and_confidence_match_onejev_reference() {
        let state = json!({"utterance": "检查激光雕刻机连接状态"});
        let mut questions = Map::new();
        questions.insert(
            "probe".into(),
            json!({
                "type": "noul",
                "instructions": "Does state.utterance ask to check connection status?"
            }),
        );
        let (branches, _) = render_branches(&questions, 256).unwrap();
        assert_eq!(branches.len(), 1);
        let full = render_full_prompt(&state, &branches[0].suffix, None, 0).unwrap();
        assert!(full.starts_with("<|im_start|>system\nApply the question to the state."));
        assert!(
            full.contains("<state>\n{\n  \"utterance\": \"检查激光雕刻机连接状态\"\n}\n</state>")
        );
        assert!(full.contains("Question: Does state.utterance ask to check connection status?\n\nOptions:\nA. yes: the statement is true / the answer is yes\nB. no: the statement is false / the answer is no\n\nAnswer with one letter: A, B."));
        assert!(full.ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n"));

        let choice = make_choice_answer(&["a".into(), "b".into(), "c".into()], &[0.85, 0.10, 0.05]);
        assert_eq!(choice["choice"], "a");
        assert!((choice["confidence"].as_f64().unwrap() - 0.775).abs() < 1e-6);

        let score = make_score_answer(
            &[json!("bad"), json!("ok"), json!("great")],
            &[0.0, 0.25, 0.75],
        );
        assert!((score["score"].as_f64().unwrap() - 1.75).abs() < 1e-6);
        assert!(score["confidence"].as_f64().unwrap() > 0.6);
    }

    #[test]
    fn parse_list_devices_distinguishes_metal_cuda_and_vulkan() {
        let mac_out = "Available devices:\n  MTL0: Apple M6 (12124 MiB, 12123 MiB free)\n  BLAS: Accelerate (0 MiB, 0 MiB free)\n";
        let det_mac = parse_list_devices_output(mac_out).unwrap();
        assert_eq!(det_mac.kind, AcceleratorKind::Metal);
        assert_eq!(det_mac.device_detail.as_deref(), Some("Apple M6"));
        // Intel Mac（x86_64）即使输出含 MTL0 也固定回退 CPU（-ngl 0）
        let det_intel_mac = parse_list_devices_output_for_arch(mac_out, false).unwrap();
        assert_eq!(det_intel_mac.kind, AcceleratorKind::Cpu);
        assert!(det_intel_mac.device_detail.is_none());

        let win_cuda =
            "Available devices:\n  CUDA0: NVIDIA GeForce RTX 4060 (8188 MiB, 7100 MiB free)\n";
        let det_cuda = parse_list_devices_output(win_cuda).unwrap();
        assert_eq!(det_cuda.kind, AcceleratorKind::Cuda);
        assert_eq!(
            det_cuda.device_detail.as_deref(),
            Some("NVIDIA GeForce RTX 4060")
        );

        let win_vk =
            "Available devices:\n  Vulkan0: AMD Radeon RX 7800 XT (16384 MiB, 15000 MiB free)\n";
        let det_vk = parse_list_devices_output(win_vk).unwrap();
        assert_eq!(det_vk.kind, AcceleratorKind::Vulkan);
        assert_eq!(
            det_vk.device_detail.as_deref(),
            Some("AMD Radeon RX 7800 XT")
        );
    }

    fn make_test_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut cd = Vec::new();
        for &(name, content) in entries {
            let local_offset = buf.len() as u32;
            let mut enc =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
            enc.write_all(content).unwrap();
            let comp = enc.finish().unwrap();
            let name_bytes = name.as_bytes();

            buf.extend_from_slice(b"PK\x03\x04");
            buf.extend_from_slice(&20u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&8u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0u32.to_le_bytes());
            buf.extend_from_slice(&(comp.len() as u32).to_le_bytes());
            buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
            buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(name_bytes);
            buf.extend_from_slice(&comp);

            cd.extend_from_slice(b"PK\x01\x02");
            cd.extend_from_slice(&20u16.to_le_bytes());
            cd.extend_from_slice(&20u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&8u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u32.to_le_bytes());
            cd.extend_from_slice(&(comp.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(content.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u32.to_le_bytes());
            cd.extend_from_slice(&local_offset.to_le_bytes());
            cd.extend_from_slice(name_bytes);
        }
        let cd_offset = buf.len() as u32;
        let cd_size = cd.len() as u32;
        buf.extend_from_slice(&cd);
        buf.extend_from_slice(b"PK\x05\x06");
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        buf.extend_from_slice(&cd_size.to_le_bytes());
        buf.extend_from_slice(&cd_offset.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf
    }

    /// 2026-11-07：多档 OneJev 同时下载时，后来的排队等推理引擎并报告“等待”，等待中可取消。
    #[test]
    fn runtime_install_wait_reports_waiting_and_is_cancellable() {
        let held = RUNTIME_INSTALL_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let cancel = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let waiter = {
            let cancel = cancel.clone();
            let seen = seen.clone();
            std::thread::spawn(move || {
                lock_runtime_install(&cancel, &|progress| seen.lock().unwrap().push(progress))
                    .map(drop)
            })
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while seen.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline, "未报告等待");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(seen.lock().unwrap().as_slice(), &[RuntimeProgress::Waiting]);
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(waiter.join().unwrap(), Err("已取消".to_owned()));
        drop(held);
        assert!(
            lock_runtime_install(&AtomicBool::new(false), &|_| {}).is_ok(),
            "释放后立即可拿到"
        );
    }

    #[test]
    fn zip_extractor_unpacks_deflate_and_blocks_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let ok_zip = make_test_zip(&[
            ("llama-server.exe", b"MZ-test-binary"),
            ("sub/mtmd.dll", b"dll-bytes"),
        ]);
        extract_zip_bytes(&ok_zip, tmp.path(), &cancel).unwrap();
        assert_eq!(
            fs::read(tmp.path().join("llama-server.exe")).unwrap(),
            b"MZ-test-binary"
        );
        assert_eq!(
            fs::read(tmp.path().join("sub").join("mtmd.dll")).unwrap(),
            b"dll-bytes"
        );

        let evil_zip = make_test_zip(&[("../escape.exe", b"bad")]);
        let out_dir = tmp.path().join("evil");
        fs::create_dir_all(&out_dir).unwrap();
        assert!(extract_zip_bytes(&evil_zip, &out_dir, &cancel).is_err());
        assert!(!tmp.path().join("escape.exe").exists());
    }

    struct MockLlama {
        port: u16,
        stop: Arc<AtomicBool>,
        completions: Arc<AtomicUsize>,
        thread: std::thread::JoinHandle<()>,
    }

    impl MockLlama {
        fn shutdown(self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect(("127.0.0.1", self.port));
            let _ = self.thread.join();
        }
    }

    fn spawn_mock_llama() -> MockLlama {
        let mock_llama = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mock_port = mock_llama.local_addr().unwrap().port();
        let stop_mock = Arc::new(AtomicBool::new(false));
        let stop_mock_t = stop_mock.clone();
        let completions = Arc::new(AtomicUsize::new(0));
        let completions_t = completions.clone();

        let mock_thread = std::thread::spawn(move || {
            while !stop_mock_t.load(Ordering::SeqCst) {
                let Ok((mut s, _)) = mock_llama.accept() else {
                    break;
                };
                if stop_mock_t.load(Ordering::SeqCst) {
                    break;
                }
                let mut reader = BufReader::new(s.try_clone().unwrap());
                let mut req_line = String::new();
                if reader.read_line(&mut req_line).unwrap_or(0) == 0 {
                    continue;
                }
                let path = req_line.split_whitespace().nth(1).unwrap_or("");
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':')
                        && k.trim().eq_ignore_ascii_case("content-length")
                    {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; len];
                if len > 0 {
                    let _ = reader.read_exact(&mut body);
                }
                if path == "/props" {
                    write_json_response(
                        &mut s,
                        200,
                        &json!({"modalities": {"vision": true}, "media_marker": "<__media__>"}),
                    );
                } else if path == "/tokenize" {
                    let v: Value = serde_json::from_slice(&body).unwrap();
                    let content = v["content"].as_str().unwrap_or("");
                    let slots = slot_labels();
                    let tokens: Vec<i64> =
                        if let Some(idx) = slots.iter().position(|l| l == content) {
                            vec![1000 + idx as i64]
                        } else {
                            content.bytes().map(|b| b as i64).collect()
                        };
                    write_json_response(&mut s, 200, &json!({ "tokens": tokens }));
                } else if path == "/completion" {
                    completions_t.fetch_add(1, Ordering::SeqCst);
                    let v: Value = serde_json::from_slice(&body).unwrap();
                    let n_predict = v["n_predict"].as_i64().unwrap_or(1);
                    let prompts = v["prompt"].as_array().unwrap();
                    if n_predict == 0 {
                        write_json_response(&mut s, 200, &json!([{"index": 0, "id_slot": 0}]));
                    } else {
                        let out: Vec<Value> = (0..prompts.len())
                            .map(|idx| {
                                json!({
                                    "index": idx,
                                    "id_slot": 0,
                                    "completion_probabilities": [{
                                        "top_logprobs": [
                                            {"id": 1000, "logprob": 3.0},
                                            {"id": 1001, "logprob": 0.5},
                                            {"id": 1002, "logprob": -1.0}
                                        ]
                                    }]
                                })
                            })
                            .collect();
                        write_json_response(&mut s, 200, &Value::Array(out));
                    }
                }
            }
        });

        MockLlama {
            port: mock_port,
            stop: stop_mock,
            completions,
            thread: mock_thread,
        }
    }

    fn spawn_test_gateway(upstream: Arc<UpstreamClient>) -> (OneJevGateway, SocketAddr) {
        let gw_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let gw_addr = gw_listener.local_addr().unwrap();
        let gw = OneJevGateway::spawn_with_upstream(
            "onejev_1_6_0_8b".into(),
            DetectedAccelerator {
                kind: AcceleratorKind::Metal,
                device_detail: Some("Apple M6".into()),
            },
            gw_listener,
            gw_addr,
            upstream,
            Arc::new(Mutex::new(None)),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        (gw, gw_addr)
    }

    #[test]
    fn gateway_round_trips_systemone_over_mock_llama_server() {
        let mock = spawn_mock_llama();
        let mock_port = mock.port;
        let upstream =
            Arc::new(UpstreamClient::connect(format!("http://127.0.0.1:{mock_port}")).unwrap());
        let (mut gw, gw_addr) = spawn_test_gateway(upstream);

        let client = reqwest::blocking::Client::new();
        let health: Value = client
            .get(format!("http://{gw_addr}/health"))
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(health["status"], "ok");
        assert_eq!(health["engine"], "onejev");
        assert_eq!(health["model"], "onejev_1_6_0_8b");

        // 跨站 Origin、Sec-Fetch-Site: cross-site 与非回环 Host（DNS 重绑定）一律返回 403
        let cross_origin = client
            .post(format!("http://{gw_addr}/v1/systemone"))
            .header("Origin", "https://evil.example")
            .json(&json!({ "state": {}, "questions": { "q": { "type": "noul" } } }))
            .send()
            .unwrap();
        assert_eq!(cross_origin.status().as_u16(), 403);

        let cross_site = client
            .get(format!("http://{gw_addr}/health"))
            .header("Sec-Fetch-Site", "cross-site")
            .send()
            .unwrap();
        assert_eq!(cross_site.status().as_u16(), 403);

        let mut rebind_stream = TcpStream::connect(gw_addr).unwrap();
        rebind_stream
            .write_all(
                b"GET /health HTTP/1.1\r\nHost: evil.example:18090\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let mut rebind_resp = String::new();
        rebind_stream.read_to_string(&mut rebind_resp).unwrap();
        assert!(
            rebind_resp.starts_with("HTTP/1.1 403"),
            "got: {rebind_resp}"
        );

        let resp: Value = client
            .post(format!("http://{gw_addr}/v1/systemone"))
            .json(&json!({
                "model": "jev-latest",
                "state": { "utterance": "查看雕刻机状态" },
                "questions": {
                    "unfinished": { "type": "noul", "instructions": "unfinished?" },
                    "route": {
                        "type": "choice",
                        "instructions": "pick route",
                        "criteria": { "status": "check status", "none": "other" }
                    }
                }
            }))
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(resp["answers"]["unfinished"]["type"], "noul");
        assert!(resp["answers"]["unfinished"]["noul"].as_f64().unwrap() > 0.9);
        assert_eq!(resp["answers"]["route"]["type"], "choice");
        assert!(resp["answers"]["route"]["confidence"].as_f64().unwrap() > 0.8);

        let not_found = client
            .post(format!("http://{gw_addr}/v1/systemone"))
            .json(&json!({
                "model": "multilingual",
                "state": {},
                "questions": { "q": { "type": "noul" } }
            }))
            .send()
            .unwrap();
        assert_eq!(not_found.status().as_u16(), 404);

        gw.stop();
        mock.shutdown();
    }

    #[test]
    fn gateway_drops_requests_abandoned_while_queued_behind_the_single_slot() {
        let mock = spawn_mock_llama();
        let upstream =
            Arc::new(UpstreamClient::connect(format!("http://127.0.0.1:{}", mock.port)).unwrap());
        let (mut gw, gw_addr) = spawn_test_gateway(upstream.clone());
        let body = serde_json::to_vec(&json!({
            "model": "jev-latest",
            "state": { "utterance": "帮我看下" },
            "questions": { "unfinished": { "type": "noul", "instructions": "unfinished?" } }
        }))
        .unwrap();
        let request = |close: bool| {
            let mut stream = TcpStream::connect(gw_addr).unwrap();
            let head = format!(
                "POST /v1/systemone HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                gw_addr.port(),
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
            if close {
                drop(stream);
                None
            } else {
                Some(stream)
            }
        };

        // 模拟推理正忙：占住单槽锁，期间到达的请求排队；调用方等不及先断开
        let busy = upstream.lock.lock().unwrap();
        request(true);
        request(true);
        std::thread::sleep(Duration::from_millis(300));
        drop(busy);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            mock.completions.load(Ordering::SeqCst),
            0,
            "已放弃的请求不应再推理"
        );

        // 仍在等待的请求照常推理并返回结果
        let mut alive = request(false).unwrap();
        let mut resp = String::new();
        alive.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 200"), "got: {resp}");
        assert!(resp.contains("\"unfinished\""), "got: {resp}");
        assert_eq!(mock.completions.load(Ordering::SeqCst), 1);

        gw.stop();
        mock.shutdown();
    }

    #[test]
    fn local_jev_budget_keeps_hold_inside_listen_window_and_flags_slow_cpu() {
        for id in ["onejev_1_6_0_8b", "onejev_1_6_4b", "onejev_1_6_9b"] {
            for gpu in [true, false] {
                let budget = local_jev_budget(id, gpu).unwrap();
                assert!(
                    budget.hold_ms.is_none_or(|ms| ms <= 800),
                    "{id} gpu={gpu}: {budget:?}"
                );
                assert!(
                    budget.turn_ms >= 1500 && budget.turn_ms <= 3500,
                    "{id} gpu={gpu}: {budget:?}"
                );
            }
        }
        assert_eq!(
            local_jev_budget("onejev_1_6_0_8b", true).unwrap().hold_ms,
            Some(600)
        );
        assert_eq!(
            local_jev_budget("onejev_1_6_9b", true).unwrap().hold_ms,
            None
        );
        assert_eq!(
            local_jev_budget("onejev_1_6_9b", true).unwrap().notice,
            Some(NOTICE_9B_NO_HOLD)
        );
        assert_eq!(
            local_jev_budget("onejev_1_6_4b", false).unwrap().notice,
            Some(NOTICE_SLOW_ON_CPU)
        );
        assert_eq!(
            local_jev_budget("onejev_1_6_4b", false).unwrap().hold_ms,
            None
        );
        assert!(
            local_jev_budget("onejev_1_6_0_8b", false)
                .unwrap()
                .notice
                .is_none()
        );
        assert!(local_jev_budget("laya_multilingual", true).is_none());
    }

    #[test]
    fn orphan_reaper_only_matches_moss_onejev_processes_adopted_by_init() {
        let roots = vec!["/Users/u/Library/Application Support/moss/models".to_owned()];
        let ps = "\
  101     1 /Users/u/Library/Application Support/moss/models/.runtime/llama-b12167/llama-server --host 127.0.0.1 --port 5000 -m /Users/u/Library/Application Support/moss/models/onejev_1_6_4b/OneJev1.6-4B.Q4_K_M.gguf
  102   900 /Users/u/Library/Application Support/moss/models/.runtime/llama-b12167/llama-server -m /Users/u/Library/Application Support/moss/models/onejev_1_6_0_8b/OneJev1.6-0.8B.Q8_0.gguf
  103     1 /opt/homebrew/bin/llama-server -m /Users/u/models/qwen.gguf
  104     1 /opt/homebrew/bin/llama-server -m /Users/u/Library/Application Support/moss/models/other.gguf
  105     1 /usr/bin/python3 -m laya.serve
garbage line
";
        assert_eq!(orphan_llama_pids(ps, &roots), vec![101]);
        assert!(orphan_llama_pids(ps, &[String::new()]).is_empty());
    }

    #[test]
    fn llama_log_summary_prefers_error_lines_and_stays_bounded() {
        let log = LlamaLog::default();
        assert_eq!(log.summary(3), "");
        for i in 0..100 {
            log.push(&format!("info line {i}"));
        }
        assert_eq!(log.0.lock().unwrap().len(), LLAMA_LOG_LINES);
        assert!(log.summary(2).ends_with("info line 99"));
        log.push("llama_model_load: error loading model: failed to open file");
        log.push(&"x".repeat(1000));
        assert!(log.summary(3).contains("error loading model"));
        assert!(log.0.lock().unwrap().back().unwrap().chars().count() <= LLAMA_LOG_LINE_CHARS);
        assert!(with_log_hint("启动失败".into(), &log).contains("推理进程输出"));
        assert_eq!(
            with_log_hint("启动失败".into(), &LlamaLog::default()),
            "启动失败"
        );
    }

    #[test]
    fn start_with_cancel_checks_port_conflict_and_releases_port_before_ready() {
        let dir = tempfile::tempdir().unwrap();
        let gguf = dir.path().join("OneJev1.6-0.8B.Q8_0.gguf");
        std::fs::write(&gguf, b"stub-gguf").unwrap();

        let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
        let occupied_addr = occupied.local_addr().unwrap();
        let cancel = AtomicBool::new(false);
        let err = match OneJevGateway::start_with_cancel(
            "onejev_1_6_0_8b",
            dir.path(),
            dir.path(),
            occupied_addr,
            None,
            &cancel,
        ) {
            Ok(mut gw) => {
                gw.stop();
                panic!("expected occupied port error");
            }
            Err(err) => err,
        };
        assert!(err.contains("绑定本地 Jev 端口"), "unexpected err: {err}");
        drop(occupied);

        // 当 cancel 已置位时，探测端口后立即退出，且端口保持可重新绑定（不残留 LISTEN 句柄）
        let cancelled = AtomicBool::new(true);
        let res = OneJevGateway::start_with_cancel(
            "onejev_1_6_0_8b",
            dir.path(),
            dir.path(),
            occupied_addr,
            None,
            &cancelled,
        );
        assert!(res.is_err());
        let rebound = TcpListener::bind(occupied_addr);
        assert!(
            rebound.is_ok(),
            "port should remain unbound after cancelled start"
        );
    }
}
