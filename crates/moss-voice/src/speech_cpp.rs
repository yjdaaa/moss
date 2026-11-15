//! 本地 Qwen3-TTS 推理引擎管理（`nyosegawa/speech.cpp` `v0.10.1`，MIT 许可，免 Python）。
//!
//! - 不打进安装包；下载 `qwen3_tts_0_6b` 模型或首次合成时按需自动拉取对应平台官方预编译包
//!   并逐字节核对 SHA-256，解压到 `<models>/.runtime/speech-v0.10.1/<platform>/`；
//! - macOS/Linux 官方包内含动态库符号链接（如 `libspeech.dylib -> libspeech.4.dylib`），解压时
//!   严格校验同目录单段相对目标后还原，并赋予二进制及共享库可执行权限；
//! - 删掉最后一个需要 `speech.cpp` 的本地模型时自动清理 `.runtime/speech-*`。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

use crate::local_models::fetch_verified_file;
use crate::onejev::{
    RuntimeProgress, extract_zip_with_symlinks, linux_vulkan_loader_present, lock_mutex_with_wait,
    windows_vulkan_loader_present,
};

pub const SPEECH_CPP_RELEASE_TAG: &str = "v0.10.1";
const SPEECH_GITHUB_RELEASE_BASE: &str =
    "https://github.com/nyosegawa/speech.cpp/releases/download/v0.10.1";
const SPEECH_GHFAST_MIRROR_BASE: &str =
    "https://ghfast.top/https://github.com/nyosegawa/speech.cpp/releases/download/v0.10.1";

static SPEECH_RUNTIME_INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SpeechReleaseAsset {
    platform_dir: &'static str,
    file_name: &'static str,
    sha256: &'static str,
    size: u64,
    expected_accel: &'static str,
}

fn speech_binary_name() -> &'static str {
    if cfg!(windows) {
        "speech.exe"
    } else {
        "speech"
    }
}

fn current_platform_asset() -> Option<SpeechReleaseAsset> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(SpeechReleaseAsset {
            platform_dir: "macos-arm64-metal",
            file_name: "speech-0.10.1-macos-arm64-metal.zip",
            sha256: "ff24f6f58c9d2dedb01caa88ba043ae845c76eff228e4cbfae5de749174404c1",
            size: 6_087_757,
            expected_accel: "Apple Metal 统一内存硬件加速",
        })
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some(SpeechReleaseAsset {
            platform_dir: "windows-x64-vulkan",
            file_name: "speech-0.10.1-windows-x64-vulkan.zip",
            sha256: "a8fe0e4b4f463c460e7ace9d38c363bec08b254cac5a7f764b7967af35d4fa1a",
            size: 29_548_632,
            expected_accel: if windows_vulkan_loader_present() {
                "Windows Vulkan 显卡加速"
            } else {
                "Windows Vulkan / CPU 推理"
            },
        })
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        if linux_vulkan_loader_present() {
            Some(SpeechReleaseAsset {
                platform_dir: "linux-x64-vulkan",
                file_name: "speech-0.10.1-linux-x64-vulkan.zip",
                sha256: "187d639813c1836f24bcd22f518b9050f8c10bfd6af3b8198c2d8fb6122165ba",
                size: 32_437_928,
                expected_accel: "Linux Vulkan 显卡加速",
            })
        } else {
            Some(SpeechReleaseAsset {
                platform_dir: "linux-x64-cpu",
                file_name: "speech-0.10.1-linux-x64-cpu.zip",
                sha256: "3396d6b2ec38c2922ff0f24a760f7b45c4ac601788f53f03eb77134393bc768b",
                size: 6_206_720,
                expected_accel: "Linux x64 CPU 推理",
            })
        }
    } else {
        None
    }
}

fn managed_runtime_dir(models_root: &Path, asset: &SpeechReleaseAsset) -> PathBuf {
    models_root
        .join(".runtime")
        .join(format!("speech-{SPEECH_CPP_RELEASE_TAG}"))
        .join(asset.platform_dir)
}

pub fn has_platform_runtime_asset() -> bool {
    current_platform_asset().is_some()
}

/// 查找可用的 `speech` 二进制：环境变量 `MOSS_SPEECH_BIN` → SHA-256 校验落盘的托管目录
/// → 用户自放目录（`<models>/speech` / `<models>/../speech`）→ 无内置平台资产时回退 `PATH`。
pub fn resolve_speech_binary(models_root: &Path) -> Option<PathBuf> {
    let bin = speech_binary_name();
    if let Some(env_bin) = std::env::var_os("MOSS_SPEECH_BIN").map(PathBuf::from)
        && env_bin.is_file()
    {
        return Some(env_bin);
    }
    if let Some(asset) = current_platform_asset() {
        let managed = managed_runtime_dir(models_root, &asset).join(bin);
        if managed.is_file() {
            return Some(managed);
        }
    }
    let mut local_dirs = vec![models_root.join("speech")];
    if let Some(parent) = models_root.parent() {
        local_dirs.push(parent.join("speech"));
    }
    for dir in &local_dirs {
        let cand = dir.join(bin);
        if cand.is_file() {
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

/// 删除 MOSS 自动下载的 `speech.cpp` 推理引擎（`<models>/.runtime/speech-*`）；用户自放 `<models>/speech` 不动。
pub fn remove_managed_runtime(models_root: &Path) -> Result<bool, String> {
    let runtime = models_root.join(".runtime");
    let Ok(entries) = fs::read_dir(&runtime) else {
        return Ok(false);
    };
    let _guard = SPEECH_RUNTIME_INSTALL_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut removed = false;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("speech-")
            || !entry.file_type().is_ok_and(|ft| ft.is_dir())
        {
            continue;
        }
        fs::remove_dir_all(entry.path())
            .map_err(|error| format!("删除 speech.cpp 推理引擎失败（可能仍在运行）：{error}"))?;
        removed = true;
    }
    let _ = fs::remove_dir(&runtime);
    Ok(removed)
}

#[cfg(unix)]
fn mark_runtime_executables(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let is_exec = name_str == "speech"
            || name_str.ends_with(".dylib")
            || name_str.contains(".dylib.")
            || name_str.ends_with(".so")
            || name_str.contains(".so.");
        if is_exec {
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o755));
        }
    }
}

pub fn ensure_speech_runtime(
    models_root: &Path,
    mirror_hint: Option<&str>,
    cancel: &AtomicBool,
    progress: &dyn Fn(RuntimeProgress),
) -> Result<PathBuf, String> {
    if let Some(existing) = resolve_speech_binary(models_root) {
        return Ok(existing);
    }
    let _guard = lock_mutex_with_wait(&SPEECH_RUNTIME_INSTALL_LOCK, cancel, progress)?;
    if let Some(existing) = resolve_speech_binary(models_root) {
        return Ok(existing);
    }
    let asset = current_platform_asset().ok_or_else(|| {
        "当前操作系统/架构未内置预编译 speech.cpp，请手动安装并配置 MOSS_SPEECH_BIN".to_owned()
    })?;
    let target_dir = managed_runtime_dir(models_root, &asset);
    let target_bin = target_dir.join(speech_binary_name());
    if target_bin.is_file() {
        return Ok(target_bin);
    }

    let staging = models_root.join(".download");
    fs::create_dir_all(&staging).map_err(|e| format!("创建运行时下载目录失败：{e}"))?;
    let part = staging.join(format!("speech-runtime-{}.part", asset.file_name));
    let prefer_china_mirror = mirror_hint
        .is_some_and(|m| m.contains("hf-mirror") || m.contains("ghfast") || m.contains("mirror"));
    let bases = if prefer_china_mirror {
        [SPEECH_GHFAST_MIRROR_BASE, SPEECH_GITHUB_RELEASE_BASE]
    } else {
        [SPEECH_GITHUB_RELEASE_BASE, SPEECH_GHFAST_MIRROR_BASE]
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
            "下载 Qwen3-TTS 原生推理引擎（speech.cpp）失败：{err}"
        ));
    }

    let extract_root = models_root.join(format!(".extract-speech-{}", asset.platform_dir));
    let _ = fs::remove_dir_all(&extract_root);
    fs::create_dir_all(&extract_root).map_err(|e| format!("创建解压目录失败：{e}"))?;

    let unpack_res = (|| {
        extract_zip_with_symlinks(&part, &extract_root, cancel, true)?;
        let extracted_bin = extract_root.join(speech_binary_name());
        if !extracted_bin.is_file() {
            return Err(format!("推理引擎压缩包缺少 {}", speech_binary_name()));
        }
        #[cfg(unix)]
        mark_runtime_executables(&extract_root);

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

pub fn detect_speech_env(models_root: &Path) -> Value {
    if resolve_speech_binary(models_root).is_some() {
        let accel = current_platform_asset()
            .map(|a| a.expected_accel)
            .unwrap_or("原生推理");
        return json!({
            "ready": true,
            "installed": true,
            "binary_found": true,
            "accelerator_label": accel,
            "summary": format!("Qwen3-TTS 推理引擎已就绪（{accel} · 免装 Python）"),
        });
    }
    if let Some(asset) = current_platform_asset() {
        let mb = (asset.size as f64) / (1024.0 * 1024.0);
        return json!({
            "ready": true,
            "installed": false,
            "binary_found": false,
            "accelerator_label": asset.expected_accel,
            "summary": format!(
                "支持开箱即用：将启用{}（下载模型时自动准备约 {:.0} MB speech.cpp 引擎，免装 Python）",
                asset.expected_accel, mb
            ),
        });
    }
    json!({
        "ready": false,
        "installed": false,
        "binary_found": false,
        "accelerator_label": "需手动提供 speech",
        "summary": "当前系统架构暂无内置 speech.cpp 预编译包，请手动安装并设置 MOSS_SPEECH_BIN。",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onejev::extract_zip_bytes_with_options;

    #[derive(Clone, Copy)]
    struct ZipTestEntry<'a> {
        name: &'a str,
        payload: &'a [u8],
        unix_mode_type: u32,
    }

    fn build_zip(entries: &[ZipTestEntry<'_>]) -> Vec<u8> {
        let mut out = Vec::new();
        struct Central {
            name: String,
            offset: u32,
            size: u32,
            unix_mode_type: u32,
        }
        let mut centrals = Vec::new();
        for entry in entries {
            let offset = out.len() as u32;
            let name_bytes = entry.name.as_bytes();
            let size = entry.payload.len() as u32;
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name_bytes);
            out.extend_from_slice(entry.payload);
            centrals.push(Central {
                name: entry.name.to_owned(),
                offset,
                size,
                unix_mode_type: entry.unix_mode_type,
            });
        }
        let cd_offset = out.len() as u32;
        for c in &centrals {
            let name_bytes = c.name.as_bytes();
            let ext_attr = (c.unix_mode_type | 0o755) << 16;
            out.extend_from_slice(b"PK\x01\x02");
            out.extend_from_slice(&((3u16 << 8) | 20u16).to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&c.size.to_le_bytes());
            out.extend_from_slice(&c.size.to_le_bytes());
            out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&ext_attr.to_le_bytes());
            out.extend_from_slice(&c.offset.to_le_bytes());
            out.extend_from_slice(name_bytes);
        }
        let cd_size = out.len() as u32 - cd_offset;
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(centrals.len() as u16).to_le_bytes());
        out.extend_from_slice(&(centrals.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn zip_symlink_extraction_restores_intra_dir_links_and_blocks_escapes() {
        let tmp = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let ok_zip = build_zip(&[
            ZipTestEntry {
                name: "libspeech.so",
                payload: b"libspeech.so.4",
                unix_mode_type: 0o120000,
            },
            ZipTestEntry {
                name: "libspeech.so.4",
                payload: b"elf-bytes",
                unix_mode_type: 0o100000,
            },
            ZipTestEntry {
                name: "speech",
                payload: b"bin-bytes",
                unix_mode_type: 0o100000,
            },
        ]);
        let ok_dir = tmp.path().join("ok");
        fs::create_dir_all(&ok_dir).unwrap();
        extract_zip_bytes_with_options(&ok_zip, &ok_dir, &cancel, true).unwrap();
        assert_eq!(fs::read(ok_dir.join("speech")).unwrap(), b"bin-bytes");
        #[cfg(unix)]
        {
            let link_meta = fs::symlink_metadata(ok_dir.join("libspeech.so")).unwrap();
            assert!(link_meta.file_type().is_symlink());
            assert_eq!(
                fs::read_link(ok_dir.join("libspeech.so")).unwrap(),
                PathBuf::from("libspeech.so.4")
            );
            assert_eq!(fs::read(ok_dir.join("libspeech.so")).unwrap(), b"elf-bytes");
        }

        for bad_target in [
            b"../evil".as_slice(),
            b"/etc/passwd".as_slice(),
            b"sub/evil".as_slice(),
        ] {
            let evil_zip = build_zip(&[ZipTestEntry {
                name: "badlink",
                payload: bad_target,
                unix_mode_type: 0o120000,
            }]);
            let evil_dir = tmp.path().join("evil");
            fs::create_dir_all(&evil_dir).unwrap();
            let res = extract_zip_bytes_with_options(&evil_zip, &evil_dir, &cancel, true);
            #[cfg(unix)]
            assert!(res.is_err(), "expected error for {:?}", bad_target);
            #[cfg(not(unix))]
            let _ = res;
            assert!(!evil_dir.join("badlink").exists());
        }
    }

    #[test]
    fn remove_managed_runtime_only_removes_speech_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let speech_dir = tmp.path().join(".runtime/speech-v0.10.1/macos-arm64-metal");
        let llama_dir = tmp.path().join(".runtime/llama-b12167/macos-arm64");
        fs::create_dir_all(&speech_dir).unwrap();
        fs::create_dir_all(&llama_dir).unwrap();
        fs::write(speech_dir.join("speech"), b"stub").unwrap();
        fs::write(llama_dir.join("llama-server"), b"stub").unwrap();

        assert!(remove_managed_runtime(tmp.path()).unwrap());
        assert!(!speech_dir.exists());
        assert!(llama_dir.is_dir());
        assert!(!remove_managed_runtime(tmp.path()).unwrap());
    }

    #[test]
    fn detect_speech_env_reports_summary_without_exposing_binary_path() {
        let tmp = tempfile::tempdir().unwrap();
        let before = detect_speech_env(tmp.path());
        assert_eq!(before["installed"], false);
        assert!(before["summary"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(before.get("binary_path").is_none());

        let speech_dir = tmp.path().join("speech");
        fs::create_dir_all(&speech_dir).unwrap();
        let bin = speech_dir.join(speech_binary_name());
        fs::write(&bin, b"stub").unwrap();

        let after = detect_speech_env(tmp.path());
        assert_eq!(after["ready"], true);
        assert_eq!(after["installed"], true);
        assert_eq!(after["binary_found"], true);
        assert!(after.get("binary_path").is_none());
    }
}
