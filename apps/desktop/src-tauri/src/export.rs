//! 导出文件与导出目录设置（技术书 §9.7，工作包 EXP）。
//!
//! - 前端所有导出都经 `save_export_file`；网页 `<a download>` 在桌面端 / Android 内嵌页里不可靠。
//! - 导出目录只能由桌面壳经系统文件夹选择框修改，保存在运行目录 `settings.json` 的 `export` 键；
//!   `/api/settings` 不接受该键，局域网页面与 HTTP 客户端无法把导出变成任意目录写文件的通道。
//! - 自定义目录不可用时**不自动建目录、不静默改存到别处**，返回 `export_dir_unavailable:<路径>`，
//!   由用户选择“重新选择”或“本次保存到下载文件夹”（`fallback_default=true`）。

use std::path::{Path, PathBuf};

/// 前端导出文件允许的扩展名（画板 PNG / 场景 JSON、材料库 JSON、G-code）。不允许写可执行文件。
pub(crate) const EXPORT_EXTENSIONS: [&str; 5] = ["png", "json", "gcode", "nc", "txt"];
pub(crate) const EXPORT_MAX_BYTES: usize = 50 * 1024 * 1024;
/// 自定义导出目录不可用时返回给前端的错误前缀
pub(crate) const EXPORT_DIR_UNAVAILABLE: &str = "export_dir_unavailable:";

/// 桌面壳持有的导出设置位置：运行目录下的 settings.json（后端初始化失败时为 None）。
pub struct ExportState {
    pub settings_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ExportSettings {
    pub dir: Option<String>,
    pub ask_each_time: bool,
}

/// 只保留文件名部分，替换 Windows 不允许的字符，并校验扩展名。
pub(crate) fn sanitize_export_name(name: &str) -> Result<String, String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_control() || r#"<>:"|?*"#.contains(c) { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim().to_owned();
    if cleaned.is_empty() {
        return Err("导出文件名为空".into());
    }
    check_export_extension(Path::new(&cleaned))?;
    Ok(cleaned)
}

fn check_export_extension(path: &Path) -> Result<(), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if EXPORT_EXTENSIONS.contains(&ext.as_str()) {
        Ok(())
    } else {
        Err(format!("不支持导出该类型文件: .{ext}"))
    }
}

/// 同名文件已存在时追加 “ (1)”、“ (2)”…，不覆盖用户已有文件。
/// 在 `dir` 下新建导出文件并写入：重名时依次尝试 `名 (1).扩展名`……；用 `create_new` 原子占位，
/// 两次同名导出同时进行也不会互相覆盖。
pub(crate) fn write_new_export(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    use std::io::Write;
    let path = Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("export");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let candidates = std::iter::once(dir.join(name))
        .chain((1..10_000).map(|n| dir.join(format!("{stem} ({n}).{ext}"))));
    for candidate in candidates {
        ensure_inside(dir, &candidate)?;
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(mut file) => {
                file.write_all(bytes).map_err(|e| format!("写入文件失败: {e}"))?;
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("写入文件失败: {e}")),
        }
    }
    Err("同名导出文件过多，请清理导出目录后重试".into())
}

/// 读取 settings.json 中的 `export` 设置；文件缺失或格式不对时返回默认值。
pub(crate) fn read_export_settings(settings_file: &Path) -> ExportSettings {
    let Some(value) = std::fs::read(settings_file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    else {
        return ExportSettings::default();
    };
    let export = &value["export"];
    ExportSettings {
        dir: export["dir"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        ask_each_time: export["ask_each_time"].as_bool().unwrap_or(false),
    }
}

/// 写回 `export` 设置：保留文件中其他键，先写临时文件再重命名，避免写出半截文件。
pub(crate) fn write_export_settings(settings_file: &Path, settings: &ExportSettings) -> Result<(), String> {
    let apply = |root: &mut serde_json::Map<String, serde_json::Value>| {
        let mut export = serde_json::Map::new();
        if let Some(dir) = &settings.dir {
            export.insert("dir".into(), serde_json::Value::String(dir.clone()));
        }
        export.insert("ask_each_time".into(), serde_json::Value::Bool(settings.ask_each_time));
        root.insert("export".into(), serde_json::Value::Object(export));
    };
    #[cfg(desktop)]
    {
        moss_voice::settings::update_root(settings_file, apply)
    }
    #[cfg(not(desktop))]
    {
        let mut root = match std::fs::read(settings_file) {
            Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => serde_json::Map::new(),
            Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|v| v.as_object().cloned())
                .ok_or_else(|| "settings.json 格式损坏，拒绝覆盖".to_owned())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
            Err(e) => return Err(format!("读取设置失败: {e}")),
        };
        apply(&mut root);
        let data = serde_json::to_vec_pretty(&root).map_err(|e| format!("序列化设置失败: {e}"))?;
        if let Some(parent) = settings_file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建设置目录失败: {e}"))?;
        }
        let tmp = settings_file.with_extension(format!("json.tmp-{}", std::process::id()));
        std::fs::write(&tmp, data).map_err(|e| format!("写入设置失败: {e}"))?;
        std::fs::rename(&tmp, settings_file).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("保存设置失败: {e}")
        })
    }
}

/// 导出目录校验：绝对路径、目录存在、可写（创建并删除一个探测文件）。
pub(crate) fn validate_export_dir(dir: &Path) -> Result<PathBuf, String> {
    if !dir.is_absolute() {
        return Err("导出目录必须是绝对路径".into());
    }
    if !dir.is_dir() {
        return Err("导出目录不存在".into());
    }
    let probe = dir.join(format!(".moss-write-probe-{}", std::process::id()));
    std::fs::write(&probe, b"").map_err(|_| "导出目录不可写".to_owned())?;
    let _ = std::fs::remove_file(&probe);
    Ok(dir.to_path_buf())
}

/// 最终路径必须仍在所选目录内（防止文件名中的路径分隔符逃逸）。
pub(crate) fn ensure_inside(dir: &Path, target: &Path) -> Result<(), String> {
    match target.parent() {
        Some(parent) if parent == dir => Ok(()),
        _ => Err("导出路径超出所选目录".into()),
    }
}

/// 决定本次导出写到哪里：
/// - 设置了自定义目录且未要求回退：校验通过就用它，不通过返回 `export_dir_unavailable:<路径>`，绝不改存别处；
/// - 未设置或用户选择“本次保存到下载文件夹”：用默认回退顺序（下载 → 文档 → 应用数据 exports）。
pub(crate) fn resolve_export_dir(
    custom: Option<&str>,
    fallback_default: bool,
    default_dir: impl FnOnce() -> Result<PathBuf, String>,
) -> Result<PathBuf, String> {
    if let (Some(custom), false) = (custom, fallback_default) {
        return validate_export_dir(Path::new(custom))
            .map_err(|reason| format!("{EXPORT_DIR_UNAVAILABLE}{custom}|{reason}"));
    }
    let dir = default_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建保存目录失败: {e}"))?;
    Ok(dir)
}

fn settings_file(state: &ExportState) -> Result<&Path, String> {
    state
        .settings_file
        .as_deref()
        .ok_or_else(|| "运行目录不可用，无法保存导出设置".to_owned())
}

/// 默认导出目录：下载 → 文档 → 应用本地数据目录下的 exports
fn default_export_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    let paths = app.path();
    paths
        .download_dir()
        .or_else(|_| paths.document_dir())
        .or_else(|_| paths.app_local_data_dir().map(|d| d.join("exports")))
        .map_err(|e| format!("找不到可写的保存目录: {e}"))
}

#[derive(serde::Serialize)]
pub struct ExportSettingsView {
    /// 用户自定义目录；未设置为 null
    dir: Option<String>,
    /// 默认目录（下载文件夹等）的实际路径
    default_dir: Option<String>,
    /// 现在导出会写到的目录
    effective_dir: Option<String>,
    ask_each_time: bool,
    /// 自定义目录当前是否可用（未设置时恒为 true）
    available: bool,
    /// 当前平台是否支持选择目录（Android 不支持）
    supported: bool,
}

fn view(app: &tauri::AppHandle, settings: &ExportSettings) -> ExportSettingsView {
    let default_dir = default_export_dir(app).ok().map(|p| p.display().to_string());
    let available = settings
        .dir
        .as_deref()
        .map(|d| validate_export_dir(Path::new(d)).is_ok())
        .unwrap_or(true);
    ExportSettingsView {
        effective_dir: settings.dir.clone().or_else(|| default_dir.clone()),
        dir: settings.dir.clone(),
        default_dir,
        ask_each_time: settings.ask_each_time,
        available,
        supported: cfg!(desktop),
    }
}

#[tauri::command]
pub fn get_export_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, ExportState>,
) -> Result<ExportSettingsView, String> {
    let settings = state
        .settings_file
        .as_deref()
        .map(read_export_settings)
        .unwrap_or_default();
    Ok(view(&app, &settings))
}

/// 调起系统文件夹选择框；选中并校验通过后保存。取消返回 None，设置不变。
#[tauri::command]
pub async fn pick_export_dir(
    app: tauri::AppHandle,
    state: tauri::State<'_, ExportState>,
) -> Result<Option<ExportSettingsView>, String> {
    let settings_file = state.settings_file.clone();
    tauri::async_runtime::spawn_blocking(move || pick_export_dir_impl(&app, settings_file.as_deref()))
        .await
        .map_err(|_| "打开导出目录选择框失败".to_owned())?
}

#[cfg(desktop)]
fn pick_export_dir_impl(
    app: &tauri::AppHandle,
    settings_path: Option<&Path>,
) -> Result<Option<ExportSettingsView>, String> {
    use tauri_plugin_dialog::DialogExt;
    let file = settings_path
        .ok_or_else(|| "运行目录不可用，无法保存导出设置".to_owned())?
        .to_path_buf();
    let current = read_export_settings(&file);
    let mut builder = app.dialog().file().set_title("选择导出文件夹");
    if let Some(dir) = current.dir.as_deref().filter(|d| Path::new(d).is_dir()) {
        builder = builder.set_directory(dir);
    }
    let Some(picked) = builder.blocking_pick_folder() else {
        return Ok(None);
    };
    let path = picked
        .into_path()
        .map_err(|e| format!("无法识别所选文件夹: {e}"))?;
    let path = validate_export_dir(&path)?;
    let updated = ExportSettings {
        dir: Some(path.display().to_string()),
        ..current
    };
    write_export_settings(&file, &updated)?;
    Ok(Some(view(app, &updated)))
}

#[cfg(not(desktop))]
fn pick_export_dir_impl(
    _app: &tauri::AppHandle,
    _settings_path: Option<&Path>,
) -> Result<Option<ExportSettingsView>, String> {
    Err("当前平台暂不支持选择导出目录".into())
}

#[tauri::command]
pub fn reset_export_dir(
    app: tauri::AppHandle,
    state: tauri::State<'_, ExportState>,
) -> Result<ExportSettingsView, String> {
    let file = settings_file(&state)?;
    let updated = ExportSettings {
        dir: None,
        ..read_export_settings(file)
    };
    write_export_settings(file, &updated)?;
    Ok(view(&app, &updated))
}

#[tauri::command]
pub fn set_export_ask_each_time(
    app: tauri::AppHandle,
    state: tauri::State<'_, ExportState>,
    enabled: bool,
) -> Result<ExportSettingsView, String> {
    let file = settings_file(&state)?;
    let updated = ExportSettings {
        ask_each_time: enabled && cfg!(desktop),
        ..read_export_settings(file)
    };
    write_export_settings(file, &updated)?;
    Ok(view(&app, &updated))
}

/// 用系统文件管理器打开当前生效的导出目录
#[tauri::command]
pub fn open_export_dir(app: tauri::AppHandle, state: tauri::State<'_, ExportState>) -> Result<(), String> {
    let settings = state
        .settings_file
        .as_deref()
        .map(read_export_settings)
        .unwrap_or_default();
    let dir = match settings.dir {
        Some(dir) => validate_export_dir(Path::new(&dir))?,
        None => default_export_dir(&app)?,
    };
    open_in_file_manager(&dir, false)
}

/// 在文件管理器中定位刚导出的文件（只接受已存在的文件）
#[tauri::command]
pub fn reveal_exported_file(path: String) -> Result<(), String> {
    let path = PathBuf::from(path);
    if !path.is_absolute() || !path.is_file() {
        return Err("文件不存在".into());
    }
    open_in_file_manager(&path, true)
}

#[cfg(windows)]
fn open_in_file_manager(path: &Path, select: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("explorer.exe");
    if select {
        // explorer 只认 `/select,"路径"`；整个参数被加引号时（路径含空格）会忽略选择、打开默认目录
        cmd.raw_arg(format!("/select,\"{}\"", path.display()));
    } else {
        cmd.arg(path);
    }
    cmd.spawn().map(|_| ()).map_err(|e| format!("无法打开文件夹: {e}"))
}

#[cfg(all(desktop, target_os = "macos"))]
fn open_in_file_manager(path: &Path, select: bool) -> Result<(), String> {
    let mut cmd = std::process::Command::new("open");
    if select {
        cmd.arg("-R");
    }
    cmd.arg(path).spawn().map(|_| ()).map_err(|e| format!("无法打开文件夹: {e}"))
}

#[cfg(all(desktop, not(windows), not(target_os = "macos")))]
fn open_in_file_manager(path: &Path, select: bool) -> Result<(), String> {
    let dir = if select { path.parent().unwrap_or(path) } else { path };
    std::process::Command::new("xdg-open")
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("无法打开文件夹: {e}"))
}

#[cfg(not(desktop))]
fn open_in_file_manager(_path: &Path, _select: bool) -> Result<(), String> {
    Err("当前平台不支持打开文件夹".into())
}

/// 保存前端导出的文件，返回实际路径；“每次询问”模式下用户取消另存为时返回 None。
#[tauri::command]
pub async fn save_export_file(
    app: tauri::AppHandle,
    state: tauri::State<'_, ExportState>,
    filename: String,
    bytes: Vec<u8>,
    fallback_default: Option<bool>,
) -> Result<Option<String>, String> {
    let name = sanitize_export_name(&filename)?;
    if bytes.len() > EXPORT_MAX_BYTES {
        return Err("导出文件超过 50MB，已取消".into());
    }
    let settings = state
        .settings_file
        .as_deref()
        .map(read_export_settings)
        .unwrap_or_default();
    let fallback = fallback_default.unwrap_or(false);
    let app_for_default = app.clone();
    let dir = resolve_export_dir(settings.dir.as_deref(), fallback, move || {
        default_export_dir(&app_for_default)
    })?;

    if settings.ask_each_time && !fallback {
        let app_for_dialog = app.clone();
        let dir_for_dialog = dir.clone();
        let name_for_dialog = name.clone();
        return tauri::async_runtime::spawn_blocking(move || {
            save_via_dialog(&app_for_dialog, &dir_for_dialog, &name_for_dialog, &bytes)
        })
        .await
        .map_err(|_| "打开保存位置选择框失败".to_owned())?;
    }

    let target = write_new_export(&dir, &name, &bytes)?;
    Ok(Some(target.display().to_string()))
}

/// “每次询问”：弹出系统“另存为”框，初始目录与文件名为本次默认值；取消返回 None。
#[cfg(desktop)]
fn save_via_dialog(app: &tauri::AppHandle, dir: &Path, name: &str, bytes: &[u8]) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app
        .dialog()
        .file()
        .set_title("导出文件")
        .set_directory(dir)
        .set_file_name(name)
        .blocking_save_file();
    let Some(picked) = picked else {
        return Ok(None);
    };
    let target = picked
        .into_path()
        .map_err(|e| format!("无法识别保存位置: {e}"))?;
    // 用户在“另存为”里亲自选的位置；仍只允许白名单扩展名
    check_export_extension(&target)?;
    std::fs::write(&target, bytes).map_err(|e| format!("写入文件失败: {e}"))?;
    Ok(Some(target.display().to_string()))
}

#[cfg(not(desktop))]
fn save_via_dialog(_app: &tauri::AppHandle, _dir: &Path, _name: &str, _bytes: &[u8]) -> Result<Option<String>, String> {
    Err("当前平台不支持每次询问保存位置".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("moss-export-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn export_names_are_sanitized_and_restricted() {
        assert_eq!(sanitize_export_name("excalidraw-laser-lab.png").unwrap(), "excalidraw-laser-lab.png");
        assert_eq!(sanitize_export_name("..\\..\\evil/plan.json").unwrap(), "plan.json");
        assert_eq!(sanitize_export_name("a:b?.gcode").unwrap(), "a_b_.gcode");
        assert_eq!(sanitize_export_name("材料库.JSON").unwrap(), "材料库.JSON");
        assert!(sanitize_export_name("run.exe").is_err());
        assert!(sanitize_export_name("noext").is_err());
        assert!(sanitize_export_name("   ").is_err());
        assert!(sanitize_export_name("../").is_err());
    }

    #[test]
    fn export_paths_never_overwrite_existing_files() {
        let dir = temp_dir("unique");
        let first = write_new_export(&dir, "scene.json", b"{}").unwrap();
        assert_eq!(first, dir.join("scene.json"));
        let second = write_new_export(&dir, "scene.json", b"[]").unwrap();
        assert_eq!(second, dir.join("scene (1).json"));
        assert_eq!(std::fs::read(&first).unwrap(), b"{}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_settings_round_trip_preserves_other_keys() {
        let dir = temp_dir("settings");
        let file = dir.join("settings.json");
        std::fs::write(&file, br#"{"web_port":9777,"mcp_endpoint":"wss://example.test/mcp/"}"#).unwrap();
        assert_eq!(read_export_settings(&file), ExportSettings::default());

        let custom = ExportSettings { dir: Some(dir.display().to_string()), ask_each_time: true };
        write_export_settings(&file, &custom).unwrap();
        assert_eq!(read_export_settings(&file), custom);
        let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(raw["web_port"], 9777, "其他键必须保留");
        assert_eq!(raw["mcp_endpoint"], "wss://example.test/mcp/");

        write_export_settings(&file, &ExportSettings { dir: None, ask_each_time: false }).unwrap();
        assert_eq!(read_export_settings(&file), ExportSettings::default());
        assert!(std::fs::read_dir(&dir).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains(".tmp-")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_broken_settings_file_gives_defaults() {
        let dir = temp_dir("broken");
        assert_eq!(read_export_settings(&dir.join("absent.json")), ExportSettings::default());
        let file = dir.join("settings.json");
        std::fs::write(&file, b"{not json").unwrap();
        assert_eq!(read_export_settings(&file), ExportSettings::default());
        // 非空损坏文件拒绝覆盖，防止抹掉其他模块配置；空文件或缺失文件可正常写入
        assert!(write_export_settings(&file, &ExportSettings { dir: None, ask_each_time: true }).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"{not json");
        let fresh = dir.join("fresh.json");
        write_export_settings(&fresh, &ExportSettings { dir: None, ask_each_time: true }).unwrap();
        assert!(read_export_settings(&fresh).ask_each_time);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unavailable_custom_dir_never_falls_back_silently() {
        let base = temp_dir("resolve");
        let missing = base.join("unplugged-usb");
        let missing_str = missing.display().to_string();
        let mut default_called = false;
        let err = resolve_export_dir(Some(&missing_str), false, || {
            default_called = true;
            Ok(base.clone())
        })
        .unwrap_err();
        assert!(err.starts_with(EXPORT_DIR_UNAVAILABLE), "{err}");
        assert!(err.contains(&missing_str));
        assert!(!default_called, "自定义目录不可用时不得改存默认目录");
        assert!(!missing.exists(), "不得自动创建导出目录");

        // 用户明确选择“本次保存到下载文件夹”
        let fallback = resolve_export_dir(Some(&missing_str), true, || Ok(base.join("downloads"))).unwrap();
        assert_eq!(fallback, base.join("downloads"));

        // 可用的自定义目录直接使用
        let ok = resolve_export_dir(Some(&base.display().to_string()), false, || unreachable!()).unwrap();
        assert_eq!(ok, base);

        // 相对路径被拒
        assert!(resolve_export_dir(Some("relative/dir"), false, || Ok(base.clone())).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn export_target_must_stay_inside_selected_dir() {
        let dir = temp_dir("inside");
        assert!(ensure_inside(&dir, &dir.join("a.png")).is_ok());
        assert!(ensure_inside(&dir, &dir.join("sub").join("a.png")).is_err());
        assert!(ensure_inside(&dir, &dir.join("..").join("a.png")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
