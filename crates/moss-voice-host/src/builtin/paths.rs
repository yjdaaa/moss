//! 本地辅助程序与模型存放目录定位、历史安装回退、Windows.old 重装提醒、Laya 环境变量解析与本机地址探测。

use super::*;

/// 辅助程序与 MOSS 放在同一目录。
pub(super) fn helper_path() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|dir| dir.join(HELPER_FILE))
}

fn norm_path_key(path: &std::path::Path) -> String {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    resolved
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

fn path_starts_with_norm(path: &std::path::Path, prefix: &std::path::Path) -> bool {
    if prefix.as_os_str().is_empty() {
        return false;
    }
    let p = norm_path_key(path);
    let pre = norm_path_key(prefix);
    !pre.is_empty() && (p == pre || p.starts_with(&format!("{pre}\\")))
}

/// 回退候选用的主程序所在目录：只排除 Cargo `target/`、macOS `.app/Contents/MacOS` 与系统临时目录，
/// 保留 `...\bin` 等目录，兼容旧版解压在 `bin\` 下且已有 `bin\models` 的存量模型。
fn fallback_exe_dir_from(
    exe: &std::path::Path,
    temp_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let exe_dir = exe.parent()?.to_path_buf();
    let norm = exe_dir
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase();
    if norm.ends_with("\\target\\release")
        || norm.ends_with("\\target\\debug")
        || norm.contains("\\target\\")
        || norm.contains(".app\\contents\\macos")
        || path_starts_with_norm(&exe_dir, temp_dir)
    {
        return None;
    }
    Some(exe_dir)
}

/// 排除 Cargo `target/`、macOS `.app/Contents/MacOS`、Unix `bin`/`sbin` 与系统临时目录后的主程序所在目录。
pub(super) fn standalone_exe_dir_from(
    exe: &std::path::Path,
    temp_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let exe_dir = fallback_exe_dir_from(exe, temp_dir)?;
    let norm = exe_dir
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase();
    if norm.ends_with("\\bin") || norm.ends_with("\\sbin") {
        return None;
    }
    Some(exe_dir)
}

pub(super) fn existing_exe_install_dir() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    standalone_exe_dir_from(&exe, &std::env::temp_dir())
}

/// Tauri NSIS 安装目录（含 `uninstall.exe`）：模型与设置同归 `%APPDATA%\moss`，卸载保留数据或换盘重装不丢模型。
fn is_nsis_install_dir(exe_dir: &std::path::Path) -> bool {
    exe_dir.join("uninstall.exe").is_file()
}

/// 在已存在的目录上用带进程号与递增序号的唯一临时文件探测写权限，不创建任何子目录，避免并发探针争用。
fn dir_is_writable(dir: &std::path::Path) -> bool {
    static PROBE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    if !dir.is_dir() {
        return false;
    }
    let pid = std::process::id();
    let seq = PROBE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let probe = dir.join(format!(".moss-write-probe-{pid}-{seq}"));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 仅当运行在无 `uninstall.exe` 的独立免安装便携目录且可写时，返回该便携目录（探测期不提前建空 `models/`）。
pub(super) fn writable_portable_dir_from(
    exe: &std::path::Path,
    temp_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let exe_dir = standalone_exe_dir_from(exe, temp_dir)?;
    if is_nsis_install_dir(&exe_dir) {
        return None;
    }
    let models_dir = exe_dir.join("models");
    let writable = if models_dir.exists() {
        dir_is_writable(&models_dir)
    } else {
        dir_is_writable(&exe_dir)
    };
    writable.then_some(exe_dir)
}

pub(super) fn writable_portable_dir() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        return None;
    }
    static CACHED: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    CACHED
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            writable_portable_dir_from(&exe, &std::env::temp_dir())
        })
        .clone()
}

/// 解析默认模型写入目录与只读回退目录列表：
/// - NSIS 安装版、开发目录与 macOS `.app` 主写目录统一为 `<runtime_dir>/models`（Windows 为 `%APPDATA%\moss\models`）；
/// - 无 `uninstall.exe` 的独立便携版主写目录为 `<exe_dir>/models`；
/// - 已存在的 `<runtime_dir>/models`、`<exe_dir>/models` 与 `%LOCALAPPDATA%\MOSS\models` 自动挂为回退目录，
///   兼容旧版在安装目录下好的模型、卸载保留数据后换目录重装以及便携版与安装版互切。
pub(super) fn resolve_model_roots_from(
    runtime_dir: &std::path::Path,
    exe: Option<&std::path::Path>,
    temp_dir: &std::path::Path,
    local_appdata: Option<&std::ffi::OsStr>,
) -> (std::path::PathBuf, Vec<std::path::PathBuf>) {
    let runtime_models = runtime_dir.join("models");
    let primary = exe
        .and_then(|e| writable_portable_dir_from(e, temp_dir))
        .map(|dir| dir.join("models"))
        .unwrap_or_else(|| runtime_models.clone());
    let primary_key = norm_path_key(&primary);

    let mut seen = vec![primary_key];
    let mut fallbacks = Vec::new();
    let candidates = [
        Some(runtime_models),
        exe.and_then(|e| fallback_exe_dir_from(e, temp_dir))
            .map(|dir| dir.join("models")),
        parse_abs_env_path(local_appdata).map(|la| la.join("MOSS").join("models")),
    ];
    for cand in candidates.into_iter().flatten() {
        if !cand.is_dir() {
            continue;
        }
        let key = norm_path_key(&cand);
        if !seen.contains(&key) {
            seen.push(key);
            fallbacks.push(cand);
        }
    }
    (primary, fallbacks)
}

pub(super) fn resolve_model_roots(
    runtime_dir: &std::path::Path,
) -> (std::path::PathBuf, Vec<std::path::PathBuf>) {
    if cfg!(test) {
        return (runtime_dir.join("models"), Vec::new());
    }
    resolve_model_roots_from(
        runtime_dir,
        std::env::current_exe().ok().as_deref(),
        &std::env::temp_dir(),
        std::env::var_os("LOCALAPPDATA").as_deref(),
    )
}

/// Windows 系统重置后遗留在 `Windows.old` 同名用户目录下的旧版 MOSS 模型或语音配置提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WindowsOldMossNotice {
    pub roaming_moss_dir: Option<std::path::PathBuf>,
    pub roaming_model_count: usize,
    pub has_voice_config: bool,
    pub local_models_dir: Option<std::path::PathBuf>,
    pub local_model_count: usize,
    pub total_model_count: usize,
}

impl WindowsOldMossNotice {
    fn missing_state<'a>(
        &'a self,
        models: &ModelManager,
        settings: &moss_voice::VoiceSettings,
    ) -> (bool, bool, Vec<&'a std::path::Path>) {
        let user_models_empty = !local_models::CATALOG.iter().any(|spec| {
            spec.id != local_models::XIAOZHI_NOTO_V1_FULL_ID
                && models.path_if_installed(spec.id).is_some()
        });
        let voice_unconfigured = settings.builtin_models.is_empty()
            && !settings.builtin_local.any()
            && !settings.builtin_colab.enabled;
        let models_missing = self.total_model_count > 0 && user_models_empty;
        let config_missing = self.has_voice_config && voice_unconfigured;

        let mut dirs = Vec::new();
        if ((config_missing && self.has_voice_config)
            || (models_missing && self.roaming_model_count > 0))
            && let Some(dir) = self.roaming_moss_dir.as_deref()
        {
            dirs.push(dir);
        }
        if models_missing
            && self.local_model_count > 0
            && let Some(dir) = self.local_models_dir.as_deref()
        {
            dirs.push(dir);
        }
        (models_missing, config_missing, dirs)
    }

    fn summary_for(&self, models_missing: bool, config_missing: bool) -> String {
        match (config_missing, models_missing) {
            (true, true) => format!("旧版语音配置与 {} 个本地模型包", self.total_model_count),
            (false, true) => format!("{} 个旧版已下载本地模型包", self.total_model_count),
            _ => "旧版语音与模型配置".to_owned(),
        }
    }

    #[cfg(test)]
    pub(super) fn still_relevant(
        &self,
        models: &ModelManager,
        settings: &moss_voice::VoiceSettings,
    ) -> bool {
        !self.missing_state(models, settings).2.is_empty()
    }

    pub(super) fn warning_text(
        &self,
        models: &ModelManager,
        settings: &moss_voice::VoiceSettings,
    ) -> Option<String> {
        let (models_missing, config_missing, dirs) = self.missing_state(models, settings);
        if dirs.is_empty() {
            return None;
        }
        let dir_list = dirs
            .into_iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join("；");
        Some(format!(
            "检测到系统重装备份目录 Windows.old 中保留有{}（{}）；Windows 默认会在重置满 10 天后自动清空 Windows.old，请尽快将旧目录文件迁回当前数据目录",
            self.summary_for(models_missing, config_missing),
            dir_list
        ))
    }

    pub(super) fn event_record(
        &self,
        models: &ModelManager,
        settings: &moss_voice::VoiceSettings,
    ) -> Option<EventRecord> {
        let (models_missing, config_missing, dirs) = self.missing_state(models, settings);
        if dirs.is_empty() {
            return None;
        }
        let dir_list = dirs
            .into_iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join("；");
        Some(
            EventRecord::warn(
                DiagCategory::Model,
                format!(
                    "检测到系统重装备份目录 Windows.old 中保留有{}",
                    self.summary_for(models_missing, config_missing)
                ),
            )
            .hint("Windows 默认会在系统重置满 10 天后自动清理 Windows.old；关闭 MOSS 后把旧目录里的 models、settings.json 等迁回当前运行目录即可恢复")
            .detail(dir_list),
        )
    }
}

/// 仅按当前登录用户的 `APPDATA` 路径推导同名用户的 `Windows.old\Users\<当前用户>`，不枚举其他系统账户。
#[cfg(any(windows, test))]
pub(super) fn detect_windows_old_moss_from(
    appdata: &std::path::Path,
) -> Option<WindowsOldMossNotice> {
    let ancestors: Vec<&std::path::Path> = appdata.ancestors().collect();
    let users_idx = ancestors.iter().position(|anc| {
        anc.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("Users"))
    })?;
    if users_idx == 0 {
        return None;
    }
    let drive_root = ancestors.get(users_idx + 1)?;
    let user_name = ancestors[users_idx - 1].file_name()?;
    let old_user_root = drive_root.join("Windows.old").join("Users").join(user_name);
    if !old_user_root.is_dir() {
        return None;
    }

    let old_roaming_moss = old_user_root.join("AppData").join("Roaming").join("moss");
    let old_roaming_models = old_roaming_moss.join("models");
    let old_local_models = old_user_root
        .join("AppData")
        .join("Local")
        .join("MOSS")
        .join("models");

    let count_installed_in = |models_dir: &std::path::Path,
                              all: &mut std::collections::BTreeSet<&'static str>|
     -> usize {
        if !models_dir.is_dir() {
            return 0;
        }
        let mut count = 0;
        for spec in &local_models::CATALOG {
            if spec.id != local_models::XIAOZHI_NOTO_V1_FULL_ID
                && local_models::installed(models_dir, spec)
            {
                all.insert(spec.id);
                count += 1;
            }
        }
        count
    };

    let mut installed_ids = std::collections::BTreeSet::new();
    let roaming_model_count = count_installed_in(&old_roaming_models, &mut installed_ids);
    let local_model_count = count_installed_in(&old_local_models, &mut installed_ids);

    let old_settings = moss_voice::settings::load(&old_roaming_moss.join("settings.json"));
    let has_voice_config = !old_settings.builtin_models.is_empty()
        || old_settings.builtin_local.any()
        || old_settings.builtin_colab.enabled;

    let total_model_count = installed_ids.len();
    if total_model_count == 0 && !has_voice_config {
        return None;
    }

    Some(WindowsOldMossNotice {
        roaming_moss_dir: (has_voice_config || roaming_model_count > 0).then_some(old_roaming_moss),
        roaming_model_count,
        has_voice_config,
        local_models_dir: (local_model_count > 0).then_some(old_local_models),
        local_model_count,
        total_model_count,
    })
}

pub(super) fn detect_windows_old_moss() -> Option<WindowsOldMossNotice> {
    #[cfg(windows)]
    {
        let appdata = parse_abs_env_path(std::env::var_os("APPDATA").as_deref())?;
        detect_windows_old_moss_from(&appdata)
    }
    #[cfg(not(windows))]
    {
        None
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nsis_install_uses_runtime_models_and_falls_back_to_legacy_install_and_localappdata() {
        let tmp = tempfile::tempdir().unwrap();
        let sys_temp = tmp.path().join("Temp");
        std::fs::create_dir_all(&sys_temp).unwrap();

        let runtime_dir = tmp.path().join("Roaming").join("moss");
        std::fs::create_dir_all(&runtime_dir).unwrap();

        // 1. NSIS 安装目录（含 uninstall.exe）：即使目录可写，主写目录仍走 runtime_dir/models，旧 <install>/models 作只读回退
        let nsis_dir = tmp.path().join("CustomDisk").join("MOSS");
        let nsis_exe = nsis_dir.join("moss-desktop.exe");
        let nsis_legacy_models = nsis_dir.join("models");
        std::fs::create_dir_all(&nsis_legacy_models).unwrap();
        std::fs::write(&nsis_exe, b"exe").unwrap();
        std::fs::write(nsis_dir.join("uninstall.exe"), b"uninst").unwrap();

        let local_appdata = tmp.path().join("Local");
        let leftover_local_models = local_appdata.join("MOSS").join("models");
        std::fs::create_dir_all(&leftover_local_models).unwrap();

        let (primary, fallbacks) = resolve_model_roots_from(
            &runtime_dir,
            Some(&nsis_exe),
            &sys_temp,
            Some(local_appdata.as_os_str()),
        );
        assert_eq!(primary, runtime_dir.join("models"));
        assert_eq!(fallbacks, vec![nsis_legacy_models, leftover_local_models]);

        // 2. 免安装独立便携目录（无 uninstall.exe）：主写目录跟随便携目录，已存在的 runtime_dir/models 挂作回退
        std::fs::create_dir_all(runtime_dir.join("models")).unwrap();
        let portable_dir = tmp.path().join("PortableMOSS");
        let portable_exe = portable_dir.join("moss-desktop.exe");
        std::fs::create_dir_all(&portable_dir).unwrap();
        std::fs::write(&portable_exe, b"exe").unwrap();

        let (p_primary, p_fallbacks) =
            resolve_model_roots_from(&runtime_dir, Some(&portable_exe), &sys_temp, None);
        assert_eq!(p_primary, portable_dir.join("models"));
        assert_eq!(p_fallbacks, vec![runtime_dir.join("models")]);
        assert!(
            !portable_dir.join("models").exists(),
            "可写探测不得在未下载模型前提前创建空 models 目录"
        );

        // 3. Cargo target、macOS .app 包、Unix bin 与系统临时目录绝不建同目录 models
        let dev_exe = tmp
            .path()
            .join("repo")
            .join("target")
            .join("release")
            .join("moss-desktop.exe");
        let app_exe = tmp
            .path()
            .join("MOSS.app")
            .join("Contents")
            .join("MacOS")
            .join("moss-desktop");
        let bin_exe = tmp
            .path()
            .join("usr")
            .join("local")
            .join("bin")
            .join("moss-desktop");
        std::fs::create_dir_all(bin_exe.parent().unwrap()).unwrap();
        let temp_exe = sys_temp.join("moss-desktop.exe");
        for non_portable in [&dev_exe, &app_exe, &bin_exe, &temp_exe] {
            assert!(writable_portable_dir_from(non_portable, &sys_temp).is_none());
        }

        // 4. 旧版解压在 ...\bin\ 下且已有 ...\bin\models 时：新下载仍走 runtime_dir/models，但 bin\models 加入只读回退
        let bin_install_dir = tmp.path().join("LegacyUnpacked").join("bin");
        let bin_install_exe = bin_install_dir.join("moss-desktop.exe");
        let bin_legacy_models = bin_install_dir.join("models");
        std::fs::create_dir_all(&bin_legacy_models).unwrap();
        std::fs::write(&bin_install_exe, b"exe").unwrap();
        let clean_runtime = tmp.path().join("FreshRoaming").join("moss");
        std::fs::create_dir_all(&clean_runtime).unwrap();

        let (bin_primary, bin_fallbacks) =
            resolve_model_roots_from(&clean_runtime, Some(&bin_install_exe), &sys_temp, None);
        assert_eq!(bin_primary, clean_runtime.join("models"));
        assert_eq!(bin_fallbacks, vec![bin_legacy_models]);
    }

    #[test]
    fn windows_old_detection_warns_only_while_current_models_or_config_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let appdata = tmp
            .path()
            .join("Users")
            .join("demo")
            .join("AppData")
            .join("Roaming");
        std::fs::create_dir_all(&appdata).unwrap();

        assert!(detect_windows_old_moss_from(&appdata).is_none());

        // 真实历史形态：Roaming\moss 存 settings.json，而模型装在 Local\MOSS\models（避免被空 Roaming\moss\models 遮蔽）
        let old_moss = tmp
            .path()
            .join("Windows.old")
            .join("Users")
            .join("demo")
            .join("AppData")
            .join("Roaming")
            .join("moss");
        let old_local_models = tmp
            .path()
            .join("Windows.old")
            .join("Users")
            .join("demo")
            .join("AppData")
            .join("Local")
            .join("MOSS")
            .join("models");
        std::fs::create_dir_all(&old_moss).unwrap();
        std::fs::create_dir_all(&old_local_models).unwrap();

        let vad_spec = local_models::spec("silero_vad").unwrap();
        let vad_file = local_models::install_path(&old_local_models, vad_spec);
        std::fs::File::create(&vad_file)
            .unwrap()
            .set_len(vad_spec.size)
            .unwrap();

        let mut old_voice = moss_voice::VoiceSettings::default();
        old_voice.builtin_local.vad = true;
        moss_voice::settings::save(&old_moss.join("settings.json"), &old_voice).unwrap();

        let notice = detect_windows_old_moss_from(&appdata).expect("should detect Windows.old");
        assert_eq!(notice.roaming_moss_dir.as_deref(), Some(old_moss.as_path()));
        assert_eq!(
            notice.local_models_dir.as_deref(),
            Some(old_local_models.as_path())
        );
        assert_eq!(notice.total_model_count, 1);
        assert!(notice.has_voice_config);

        let cur_models_dir = appdata.join("moss").join("models");
        let empty_manager = ModelManager::new(cur_models_dir.clone());
        let empty_settings = moss_voice::VoiceSettings::default();
        assert!(notice.still_relevant(&empty_manager, &empty_settings));
        let initial_warn = notice
            .warning_text(&empty_manager, &empty_settings)
            .expect("both missing");
        assert!(initial_warn.contains("Windows.old"));
        assert!(initial_warn.contains(&old_moss.display().to_string()));
        assert!(initial_warn.contains(&old_local_models.display().to_string()));

        // 用户仅迁回了 Roaming\moss\settings.json、漏迁 Local\MOSS\models 时：继续精确提示 Local\MOSS\models
        let models_only_warn = notice
            .warning_text(&empty_manager, &old_voice)
            .expect("models still missing");
        assert!(models_only_warn.contains(&old_local_models.display().to_string()));
        assert!(!models_only_warn.contains(&old_moss.display().to_string()));

        // 再把模型迁回当前目录后，提醒自动静默
        std::fs::create_dir_all(&cur_models_dir).unwrap();
        std::fs::File::create(local_models::install_path(&cur_models_dir, vad_spec))
            .unwrap()
            .set_len(vad_spec.size)
            .unwrap();
        assert!(!notice.still_relevant(&empty_manager, &old_voice));
        assert!(notice.warning_text(&empty_manager, &old_voice).is_none());
    }
}
