//! 语音中心的 Tauri 命令（技术书 §9.4.6，实施清单 P4-07）。
//!
//! 以 Tauri 命令实现而不是 HTTP 路由：只有 MOSS 桌面窗口能调用，浏览器、局域网、手机与
//! 小智桥都够不到（等价于“仅桌面会话”）；独立的 `moss-host` 与 Android 没有这些接口，
//! HTTP `/v1/voice/*` 在那里返回 404（A-68）。进程管理代码只在桌面端编译（A-69）。
//!
//! 错误统一为 `{code, message, status}`，页面据 `code` 显示原因。

use serde_json::Value;
#[cfg(desktop)]
use tauri::Manager as _;

type CmdResult = Result<Value, Value>;

fn error_value(code: &str, message: &str) -> Value {
    serde_json::json!({"code": code, "message": message})
}

#[cfg(mobile)]
fn desktop_only<T>(_: T) -> CmdResult {
    Err(error_value(
        "voice_desktop_only",
        "语音中心仅在 MOSS 桌面端可用",
    ))
}

/// 改写设置、启停进程、读日志的命令只接受后端地址上的主窗口（见 [`crate::is_trusted_main`]）。
#[cfg(desktop)]
fn require_main_window(window: &tauri::WebviewWindow) -> Result<(), Value> {
    if crate::window_is_trusted_main(window) {
        Ok(())
    } else {
        Err(error_value(
            "voice_acl_denied",
            "只能在 MOSS 主窗口修改语音中心",
        ))
    }
}

#[cfg(desktop)]
mod imp {
    use std::sync::Arc;
    use std::time::Duration;

    use moss_voice::{CenterError, HostHooks, VoiceCenter};
    use serde_json::Value;
    use tauri::Manager;

    use super::{CmdResult, error_value};

    /// Tauri 托管状态：后端未启动时为空。
    pub struct VoiceState(
        pub Option<Arc<VoiceCenter>>,
        /// 同一份后端句柄：严格模式开关要写运行目录 `.env` 并即时生效（n27）。
        pub Option<Arc<laser_host::HostApplication>>,
    );

    /// 自动探测本机 Python 3：只检查文件是否存在，不启动任何程序。
    ///
    /// 自建服务端向导要填「Python 解释器」，不该让用户自己去安装目录里找路径。
    /// 兼容 Windows（`python.exe`、`%LOCALAPPDATA%\Programs\Python`）与 macOS / Linux
    /// （`std::env::split_paths`、Homebrew、Python.framework、pyenv / conda 等常见位置）。
    pub fn detect_python() -> Value {
        let local = std::env::var_os("LOCALAPPDATA");
        let pf = std::env::var_os("ProgramFiles");
        let pf86 = std::env::var_os("ProgramFiles(x86)");
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
        let candidates = detect_python_candidates(
            cfg!(windows),
            std::env::var_os("PATH").as_deref(),
            local.as_deref(),
            &[pf.as_deref(), pf86.as_deref()],
            home.as_deref(),
            &[
                std::path::Path::new("/opt/homebrew/bin/python3"),
                std::path::Path::new("/usr/local/bin/python3"),
                std::path::Path::new("/usr/bin/python3"),
            ],
            Some(std::path::Path::new(
                "/Library/Frameworks/Python.framework/Versions",
            )),
        );
        serde_json::json!({ "candidates": candidates, "suggested": candidates.first().cloned() })
    }

    pub(crate) fn detect_python_candidates(
        is_windows: bool,
        path_env: Option<&std::ffi::OsStr>,
        local_app_data: Option<&std::ffi::OsStr>,
        program_files: &[Option<&std::ffi::OsStr>],
        home: Option<&std::ffi::OsStr>,
        unix_static_bins: &[&std::path::Path],
        macos_framework_versions: Option<&std::path::Path>,
    ) -> Vec<String> {
        fn collect_versioned_dirs(
            root: &std::path::Path,
            prefix: &str,
            out: &mut Vec<std::path::PathBuf>,
        ) {
            let Ok(entries) = std::fs::read_dir(root) else {
                return;
            };
            let mut dirs: Vec<std::path::PathBuf> = entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|dir| {
                    dir.is_dir()
                        && dir
                            .file_name()
                            .is_some_and(|name| name.to_string_lossy().starts_with(prefix))
                })
                .collect();
            dirs.sort();
            dirs.reverse();
            out.extend(dirs);
        }
        let mut candidates: Vec<String> = Vec::new();
        let push = |path: std::path::PathBuf, list: &mut Vec<String>| {
            if path.is_file() {
                let text = path.to_string_lossy().to_string();
                if !list.contains(&text) {
                    list.push(text);
                }
            }
        };
        let names: &[&str] = if is_windows {
            &["python.exe", "python3.exe", "py.exe"]
        } else {
            &["python3", "python"]
        };
        if let Some(paths) = path_env {
            for dir in std::env::split_paths(paths).filter(|dir| !dir.as_os_str().is_empty()) {
                for name in names {
                    push(dir.join(name), &mut candidates);
                }
            }
        }
        if is_windows {
            let mut version_dirs: Vec<std::path::PathBuf> = Vec::new();
            if let Some(local) = local_app_data {
                let root = std::path::PathBuf::from(local)
                    .join("Programs")
                    .join("Python");
                collect_versioned_dirs(&root, "Python3", &mut version_dirs);
            }
            for root in program_files.iter().flatten() {
                collect_versioned_dirs(
                    &std::path::PathBuf::from(root),
                    "Python3",
                    &mut version_dirs,
                );
            }
            for dir in version_dirs {
                push(dir.join("python.exe"), &mut candidates);
            }
        } else {
            for bin in unix_static_bins {
                push((*bin).to_path_buf(), &mut candidates);
            }
            if let Some(versions_root) = macos_framework_versions {
                let mut version_dirs = Vec::new();
                collect_versioned_dirs(versions_root, "3.", &mut version_dirs);
                for dir in version_dirs {
                    push(dir.join("bin").join("python3"), &mut candidates);
                }
            }
            if let Some(home) = home.filter(|h| !h.is_empty()).map(std::path::PathBuf::from) {
                for parts in [
                    &[".pyenv", "shims", "python3"][..],
                    &["miniforge3", "bin", "python3"][..],
                    &["miniconda3", "bin", "python3"][..],
                    &["anaconda3", "bin", "python3"][..],
                ] {
                    push(
                        parts.iter().fold(home.clone(), |acc, part| acc.join(part)),
                        &mut candidates,
                    );
                }
            }
        }
        candidates
    }

    /// 把进程内 MOSS 后端的能力交给语音中心。
    struct DesktopHooks {
        host: Arc<laser_host::HostApplication>,
        /// 内置语音服务（语音链路切到“内置服务”时启动，离开时停止，NV5）
        builtin: Option<Arc<crate::voice_builtin::BuiltinVoice>>,
    }

    impl HostHooks for DesktopHooks {
        fn strict_mode(&self) -> bool {
            self.host.api_service.strict_mode()
        }

        fn set_strict_mode(&self, enabled: bool) {
            // `.env` 开启的严格模式不能被语音中心关闭
            self.host
                .api_service
                .set_strict_mode(enabled || self.host.strict_from_env());
        }

        fn strict_from_env(&self) -> bool {
            self.host.strict_from_env()
        }

        fn restart_bridge(&self) -> bool {
            self.host.restart_mcp_bridge()
        }

        fn bridge_connected(&self) -> bool {
            laser_host::mcp_bridge_connected()
        }

        fn set_device_sessions(&self, sessions: Option<u32>) {
            laser_host::set_voice_device_sessions(sessions);
        }

        fn builtin_start(&self) -> Result<(), String> {
            let builtin = self.builtin.as_ref().ok_or("内置语音服务不可用")?;
            builtin.start().map(|_| ()).map_err(|error| error.message)
        }

        fn builtin_stop(&self) {
            if let Some(builtin) = &self.builtin {
                builtin.stop();
            }
        }

        fn mcp_overview(&self) -> Value {
            self.host.mcp_overview()
        }

        fn apply_disabled_tools(&self, names: &[String]) -> Result<Vec<String>, String> {
            let accepted = self.host.validate_disabled_tools(names)?;
            self.host.api_service.set_disabled_tools(accepted.clone());
            Ok(accepted)
        }
    }

    /// 语音中心与小智桥对附加接入点数量上限的约定一致（两个 crate 各自定义，避免互相依赖）。
    #[test]
    fn extra_endpoint_limit_matches_the_bridge() {
        assert_eq!(
            moss_voice::MAX_EXTRA_ENDPOINTS,
            laser_host::MAX_EXTRA_ENDPOINTS
        );
    }

    pub fn init(
        host: Option<Arc<laser_host::HostApplication>>,
        builtin: Option<Arc<crate::voice_builtin::BuiltinVoice>>,
    ) -> VoiceState {
        let host_ref = host.clone();
        VoiceState(
            host.map(|host| {
                let runtime_dir = host.runtime_root.path.clone();
                let center = Arc::new(VoiceCenter::new(
                    &runtime_dir,
                    Arc::new(DesktopHooks { host, builtin }),
                ));
                // 自建模式下每 5 秒查一次接入点 health，供状态卡与顶栏“等待设备”使用
                center.spawn_health_poller(Duration::from_secs(5));
                center
            }),
            host_ref,
        )
    }

    /// 运行目录 `.env` 只改一个布尔项，保留其他行；先写临时文件再改名，避免半截文件。
    pub fn write_env_flag(
        path: &std::path::Path,
        key: &str,
        value: &str,
    ) -> std::io::Result<std::path::PathBuf> {
        let existing = std::fs::read_to_string(path).unwrap_or_default();
        let prefix = format!("{key}=");
        let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
        let mut replaced = false;
        for line in lines.iter_mut() {
            if line.trim_start().starts_with(&prefix) {
                *line = format!("{key}={value}");
                replaced = true;
            }
        }
        if !replaced {
            lines.push(format!("{key}={value}"));
        }
        let mut text = lines.join("\n");
        text.push('\n');
        let temp = path.with_file_name(format!(".env.tmp-{}", std::process::id()));
        std::fs::write(&temp, text)?;
        if let Err(err) = std::fs::rename(&temp, path) {
            let _ = std::fs::remove_file(&temp);
            return Err(err);
        }
        Ok(path.to_path_buf())
    }

    /// 若启动工作目录或其祖先目录中的首个 `.env` 已显式定义该开关（`dotenvy` 启动时会读取），
    /// 同步更新对应行，避免下次启动被旧工作目录 `.env` 反向覆盖；不存在或未定义时不改动。
    pub fn sync_existing_startup_env_flag(
        start_dir: &std::path::Path,
        runtime_env: &std::path::Path,
        key: &str,
        value: &str,
    ) {
        let prefix = format!("{key}=");
        let runtime_canon = std::fs::canonicalize(runtime_env).ok();
        for ancestor in start_dir.ancestors() {
            let candidate = ancestor.join(".env");
            if !candidate.is_file() {
                continue;
            }
            let same = runtime_canon
                .as_ref()
                .zip(std::fs::canonicalize(&candidate).ok())
                .is_some_and(|(a, b)| a == &b)
                || candidate == runtime_env;
            if !same
                && let Ok(existing) = std::fs::read_to_string(&candidate)
                && existing
                    .lines()
                    .any(|line| line.trim_start().starts_with(&prefix))
            {
                let _ = write_env_flag(&candidate, key, value);
            }
            break;
        }
    }

    fn center(app: &tauri::AppHandle) -> Result<Arc<VoiceCenter>, Value> {
        app.try_state::<VoiceState>()
            .and_then(|state| state.0.clone())
            .ok_or_else(|| error_value("voice_unavailable", "语音中心不可用：本机后端未启动"))
    }

    fn to_value(error: CenterError) -> Value {
        serde_json::to_value(error)
            .unwrap_or_else(|_| error_value("voice_internal", "语音中心内部错误"))
    }

    /// 在阻塞线程池中执行（测试连接最长 15 秒、停止服务端最长 5 秒，不能卡住界面）。
    pub async fn run<F>(app: tauri::AppHandle, task: F) -> CmdResult
    where
        F: FnOnce(&VoiceCenter) -> Result<Value, CenterError> + Send + 'static,
    {
        let center = center(&app)?;
        tauri::async_runtime::spawn_blocking(move || task(&center).map_err(to_value))
            .await
            .map_err(|_| error_value("voice_internal", "语音中心任务异常结束"))?
    }

    pub fn parse_mode(mode: &str) -> Result<moss_voice::VoiceMode, CenterError> {
        match mode.trim() {
            "official" => Ok(moss_voice::VoiceMode::Official),
            "self_hosted" => Ok(moss_voice::VoiceMode::SelfHosted),
            "builtin" => Ok(moss_voice::VoiceMode::Builtin),
            _ => Err(CenterError::new(
                "voice_mode_invalid",
                400,
                "链路模式只能是 official、self_hosted 或 builtin",
            )),
        }
    }

    pub fn parse_section(section: &str) -> Result<moss_voice::Section, CenterError> {
        moss_voice::Section::parse(section).ok_or_else(|| {
            CenterError::new("model_input_invalid", 400, "分区只能是 llm、asr 或 tts")
        })
    }

    /// 是否有 MOSS 启动的服务端在运行（托盘退出时决定是否询问）。
    pub fn owns_running_server(app: &tauri::AppHandle) -> bool {
        center(app)
            .map(|center| center.supervisor().owns_running_server())
            .unwrap_or(false)
    }

    /// MOSS 退出时处理服务端。必须在 `ShutdownState::request()`（急停）之后调用。
    pub fn shutdown(app: &tauri::AppHandle, stop: bool) {
        if let Ok(center) = center(app) {
            center.shutdown(stop);
        }
    }
}

#[cfg(desktop)]
pub use imp::{init, owns_running_server, shutdown};

#[tauri::command]
pub async fn voice_status(app: tauri::AppHandle) -> CmdResult {
    #[cfg(desktop)]
    let result = imp::run(app, |center| Ok(center.status())).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}

/// 保存服务端目录、Python、启动参数、端口、自建接入点与健康检查地址（不含任何模型密钥）。
#[tauri::command]
pub async fn voice_save_settings(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    settings: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |center| center.save_settings(&settings)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, settings));
    result
}

/// 切换前列出将改动的项（确认框内容）。
#[tauri::command]
pub async fn voice_mode_changes(app: tauri::AppHandle, mode: String) -> CmdResult {
    #[cfg(desktop)]
    let result = imp::run(app, move |center| {
        let changes = center.mode_changes(imp::parse_mode(&mode)?)?;
        Ok(serde_json::json!({"changes": changes}))
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, mode));
    result
}

#[tauri::command]
pub async fn voice_set_mode(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    mode: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |center| center.set_mode(imp::parse_mode(&mode)?)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, mode));
    result
}

#[tauri::command]
pub async fn voice_server_start(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |center| center.server_start()).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

#[tauri::command]
pub async fn voice_server_stop(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |center| center.server_stop()).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

#[tauri::command]
pub async fn voice_server_restart(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |center| center.server_restart()).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

/// 已脱敏的服务端日志尾部（默认 200 行）。
#[tauri::command]
pub async fn voice_server_log(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    tail: Option<usize>,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |center| {
        Ok(serde_json::json!({"lines": center.log_tail(tail.unwrap_or(200))}))
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, tail));
    result
}

/// 测试一个分区。请求体中的密钥只在本次调用的内存中使用，结果不回显密钥。
#[tauri::command]
pub async fn voice_models_test(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    section: String,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |center| {
        let section = imp::parse_section(&section)?;
        let input: moss_voice::SectionInput = serde_json::from_value(input).map_err(|_| {
            moss_voice::CenterError::new("model_input_invalid", 400, "测试内容格式不正确")
        })?;
        let outcome = center.models_test(section, &input)?;
        serde_json::to_value(outcome)
            .map_err(|_| moss_voice::CenterError::new("voice_internal", 500, "结果序列化失败"))
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, section, input));
    result
}

/// 保存到服务端 `data/.config.yaml`（每个非空分区须与测试通过的内容一致）。
#[tauri::command]
pub async fn voice_models_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |center| {
        let input: moss_voice::ModelsInput = serde_json::from_value(input).map_err(|_| {
            moss_voice::CenterError::new("model_input_invalid", 400, "保存内容格式不正确")
        })?;
        center.models_save(&input)
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, input));
    result
}

/// 自动探测本机 Python 3 解释器路径（只检查文件存在，不执行程序）。
#[tauri::command]
pub async fn voice_detect_python(window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = Ok(imp::detect_python());
    #[cfg(mobile)]
    let result = {
        let _ = window;
        Err(error_value(
            "voice_desktop_only",
            "语音中心仅在 MOSS 桌面端可用",
        ))
    };
    result
}

/// 严格模式开关（技术书 §6.2）：写运行目录 `.env`，开与关都立即生效（n27，2026-10-18 用户要求）。
///
/// 唯一例外是自建链路：那里严格模式必须开着，命令与界面都不允许关闭。关闭时同步启动时的
/// `.env` 快照，否则 `enabled || from_env` 会把热关挡回去；`.env` 仍写入，重启后以文件为准。
#[tauri::command]
pub async fn voice_strict_set(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    enabled: bool,
    confirmed: bool,
) -> CmdResult {
    #[cfg(desktop)]
    {
        require_main_window(&window)?;
        if !confirmed {
            return Err(error_value(
                "strict_confirm_required",
                "开启或关闭严格模式前需要先确认",
            ));
        }
        let Some(state) = app.try_state::<imp::VoiceState>() else {
            return Err(error_value(
                "voice_unavailable",
                "严格模式开关不可用：本机后端未启动",
            ));
        };
        let host = state.1.clone().ok_or_else(|| {
            error_value("voice_unavailable", "严格模式开关不可用：本机后端未启动")
        })?;
        let locked = state
            .0
            .clone()
            .map(|center| {
                center.status()["strict"]["locked"]
                    .as_bool()
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if !enabled && locked {
            return Err(error_value(
                "strict_mode_locked",
                "自建链路必须开启严格模式，不能在语音中心关闭",
            ));
        }
        let flag = if enabled { "true" } else { "false" };
        let env_file = host.runtime_root.path.join(".env");
        imp::write_env_flag(&env_file, "MOSS_FUSION_STRICT", flag).map_err(|error| {
            error_value("strict_write_failed", &format!("写入 .env 失败：{error}"))
        })?;
        if let Ok(cwd) = std::env::current_dir() {
            imp::sync_existing_startup_env_flag(&cwd, &env_file, "MOSS_FUSION_STRICT", flag);
        }
        host.set_strict_from_env(enabled);
        host.api_service.set_strict_mode(enabled);
        let (stopped_cleanly, expected_bridges, spawned_bridges) =
            host.restart_mcp_bridge_checked();
        let bridge_ok = stopped_cleanly && spawned_bridges == expected_bridges;
        let active = host.api_service.strict_mode();
        let mut payload = serde_json::json!({
            "enabled": enabled,
            "applied": active == enabled && bridge_ok,
            "needs_restart": active != enabled || !bridge_ok,
            "bridge_restarted": bridge_ok,
            "env_file": env_file.display().to_string(),
        });
        if !bridge_ok {
            payload["bridge_error"] = serde_json::Value::String(
                "小智 MCP 桥未能立即完成重连，建议重启 MOSS 确保工具列表与严格模式完全同步".into(),
            );
        }
        return Ok(payload);
    }
    #[cfg(mobile)]
    {
        let _ = (app, window, enabled, confirmed);
        Err(error_value(
            "voice_desktop_only",
            "语音中心仅在 MOSS 桌面端可用",
        ))
    }
}

/// 前端调用的语音中心命令（build.rs 登记与 capability 授权的测试会核对这张表）。
#[cfg(test)]
pub const COMMANDS: [&str; 12] = [
    "voice_status",
    "voice_save_settings",
    "voice_mode_changes",
    "voice_set_mode",
    "voice_server_start",
    "voice_server_stop",
    "voice_server_restart",
    "voice_server_log",
    "voice_models_test",
    "voice_models_save",
    "voice_detect_python",
    "voice_strict_set",
];

#[cfg(all(test, desktop))]
mod tests {
    use super::imp::{parse_mode, parse_section};

    #[test]
    fn mode_and_section_parsing() {
        assert_eq!(
            parse_mode("self_hosted").unwrap(),
            moss_voice::VoiceMode::SelfHosted
        );
        assert_eq!(
            parse_mode(" official ").unwrap(),
            moss_voice::VoiceMode::Official
        );
        assert_eq!(
            parse_mode("builtin").unwrap(),
            moss_voice::VoiceMode::Builtin
        );
        assert_eq!(
            parse_mode("gateway").unwrap_err().code,
            "voice_mode_invalid"
        );
        assert_eq!(parse_section("tts").unwrap(), moss_voice::Section::Tts);
        assert!(parse_section("vad").is_err());
    }

    /// 严格模式开关只改 `.env` 里的一行，其他配置与注释必须原样保留。
    #[test]
    fn env_flag_replaces_one_line_and_appends_when_missing() {
        let dir = std::env::temp_dir().join(format!("moss-env-flag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(&path, "MOSS_PORT=8080\nMOSS_FUSION_STRICT=false\n# 注释\n").unwrap();
        super::imp::write_env_flag(&path, "MOSS_FUSION_STRICT", "true").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("MOSS_FUSION_STRICT=true"));
        assert!(!text.contains("MOSS_FUSION_STRICT=false"));
        assert!(text.contains("MOSS_PORT=8080"), "其他键必须保留");
        assert!(text.contains("# 注释"), "注释必须保留");
        super::imp::write_env_flag(&path, "MOSS_GATEWAY_ENABLED", "false").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("MOSS_GATEWAY_ENABLED=false"),
            "缺失的键要追加"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_existing_startup_env_updates_only_when_key_already_present() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("sub").join("nested");
        std::fs::create_dir_all(&cwd).unwrap();
        let runtime_env = root.path().join("runtime").join(".env");
        std::fs::create_dir_all(runtime_env.parent().unwrap()).unwrap();
        std::fs::write(&runtime_env, "MOSS_FUSION_STRICT=false\n").unwrap();

        let startup_env = root.path().join(".env");
        std::fs::write(&startup_env, "MOSS_PORT=8080\n").unwrap();
        super::imp::sync_existing_startup_env_flag(
            &cwd,
            &runtime_env,
            "MOSS_FUSION_STRICT",
            "false",
        );
        assert_eq!(
            std::fs::read_to_string(&startup_env).unwrap(),
            "MOSS_PORT=8080\n",
            "未定义 MOSS_FUSION_STRICT 的工作目录 .env 不应被追加"
        );

        std::fs::write(&startup_env, "MOSS_PORT=8080\nMOSS_FUSION_STRICT=true\n").unwrap();
        super::imp::sync_existing_startup_env_flag(
            &cwd,
            &runtime_env,
            "MOSS_FUSION_STRICT",
            "false",
        );
        let updated = std::fs::read_to_string(&startup_env).unwrap();
        assert!(updated.contains("MOSS_FUSION_STRICT=false"));
        assert!(updated.contains("MOSS_PORT=8080"));
    }

    #[test]
    fn detect_python_supports_unix_and_macos_layouts() {
        let root = tempfile::tempdir().unwrap();
        let brew = root.path().join("homebrew-python3");
        std::fs::write(&brew, b"").unwrap();
        let fw_310 = root.path().join("Versions").join("3.10").join("bin");
        let fw_312 = root.path().join("Versions").join("3.12").join("bin");
        std::fs::create_dir_all(&fw_310).unwrap();
        std::fs::create_dir_all(&fw_312).unwrap();
        std::fs::write(fw_310.join("python3"), b"").unwrap();
        std::fs::write(fw_312.join("python3"), b"").unwrap();
        let home = root.path().join("home");
        std::fs::create_dir_all(home.join("miniconda3").join("bin")).unwrap();
        std::fs::write(home.join("miniconda3").join("bin").join("python3"), b"").unwrap();

        let found = super::imp::detect_python_candidates(
            false,
            None,
            None,
            &[],
            Some(home.as_os_str()),
            &[brew.as_path()],
            Some(&root.path().join("Versions")),
        );
        assert_eq!(found.len(), 4);
        assert_eq!(found[0], brew.to_string_lossy());
        assert_eq!(
            found[1],
            fw_312.join("python3").to_string_lossy(),
            "更高版本在前"
        );
        assert_eq!(found[2], fw_310.join("python3").to_string_lossy());
        assert_eq!(
            found[3],
            home.join("miniconda3")
                .join("bin")
                .join("python3")
                .to_string_lossy()
        );
    }
}
