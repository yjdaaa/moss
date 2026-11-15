//! llama-server 推理引擎的平台预编译包定位、硬件加速探测、环境检测与按需下载安装。

use super::*;

fn llama_binary_name() -> &'static str {
    if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

pub(crate) fn windows_vulkan_loader_present() -> bool {
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

pub(crate) fn linux_vulkan_loader_present() -> bool {
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

pub(crate) fn configure_no_window(cmd: &mut Command) {
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

pub(super) fn parse_list_devices_output_for_arch(
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
pub(super) fn parse_list_devices_output(output: &str) -> Option<DetectedAccelerator> {
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

/// 等待指定互斥锁；等待期间可取消（避免多路下载共用推理引擎时卡死在 0%）。
pub(crate) fn lock_mutex_with_wait<'a>(
    slot: &'a OnceLock<Mutex<()>>,
    cancel: &AtomicBool,
    progress: &dyn Fn(RuntimeProgress),
) -> Result<std::sync::MutexGuard<'a, ()>, String> {
    let lock = slot.get_or_init(|| Mutex::new(()));
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

/// 等待推理引擎安装锁；等待期间可取消（2026-11-07：多档 OneJev 同时下载时后点的不再卡死在 0%）。
pub(super) fn lock_runtime_install(
    cancel: &AtomicBool,
    progress: &dyn Fn(RuntimeProgress),
) -> Result<std::sync::MutexGuard<'static, ()>, String> {
    lock_mutex_with_wait(&RUNTIME_INSTALL_LOCK, cancel, progress)
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
