//! 语音中心的 Tauri 命令（技术书 §9.4.6，实施清单 P4-07）。
//!
//! 以 Tauri 命令实现而不是 HTTP 路由：只有 MOSS 桌面窗口能调用，浏览器、局域网、手机与
//! 小智桥都够不到（等价于“仅桌面会话”）；独立的 `moss-host` 与 Android 没有这些接口，
//! HTTP `/v1/voice/*` 在那里返回 404（A-68）。语音中心只在桌面端编译（A-69）。
//!
//! 2026-11-17 删除自建服务端链路：服务端启停、日志、外部模型向导与 Python 探测命令一并删除。
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

/// 改写设置、切换链路与严格模式的命令只接受后端地址上的主窗口（见 [`crate::is_trusted_main`]）。
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

        fn strict_from_env(&self) -> bool {
            self.host.strict_from_env()
        }

        fn restart_bridge(&self) -> bool {
            self.host.restart_mcp_bridge()
        }

        fn bridge_connected(&self) -> bool {
            laser_host::mcp_bridge_connected(&self.host.api_service)
        }

        fn set_device_sessions(&self, sessions: Option<u32>) {
            laser_host::set_voice_device_sessions(&self.host.api_service, sessions);
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

        fn builtin_chat_ready(&self, voice: &moss_voice::VoiceSettings) -> Option<bool> {
            self.builtin
                .as_ref()
                .map(|builtin| builtin.chat_ready(voice))
        }

        fn mcp_overview(&self) -> Value {
            self.host.mcp_overview()
        }

        fn apply_disabled_tools(&self, names: &[String]) -> Result<Vec<String>, String> {
            let accepted = self.host.validate_disabled_tools(names)?;
            self.host.api_service.set_disabled_tools(accepted.clone());
            Ok(accepted)
        }

        fn apply_full_tool_list(&self, full: bool) {
            self.host.api_service.set_full_tool_list(full);
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
                Arc::new(VoiceCenter::new(
                    &runtime_dir,
                    Arc::new(DesktopHooks { host, builtin }),
                ))
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

    /// 在阻塞线程池中执行（切换链路会启停内置服务、保存设置会重连小智桥，不能卡住界面）。
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
            "builtin" => Ok(moss_voice::VoiceMode::Builtin),
            _ => Err(CenterError::new(
                "voice_mode_invalid",
                400,
                "链路模式只能是 official 或 builtin",
            )),
        }
    }
}

#[cfg(desktop)]
pub use imp::init;

#[tauri::command]
pub async fn voice_status(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |center| Ok(center.status())).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

/// 保存内置服务端口与常连模式、官方接入点、附加接入点与工具开关（不含任何模型密钥）。
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
pub async fn voice_mode_changes(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    mode: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |center| {
        let changes = center.mode_changes(imp::parse_mode(&mode)?)?;
        Ok(serde_json::json!({"changes": changes}))
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, mode));
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

/// 严格模式开关（技术书 §6.2）：写运行目录 `.env`，开与关都立即生效（n27，2026-10-18 用户要求）。
///
/// 关闭时同步启动时的 `.env` 快照，否则 `enabled || from_env` 会把热关挡回去；`.env` 仍写入，
/// 重启后以文件为准。（2026-11-17 起没有自建链路，也就没有“不能关闭”的例外。）
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
        Ok(payload)
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
pub const COMMANDS: [&str; 5] = [
    "voice_status",
    "voice_save_settings",
    "voice_mode_changes",
    "voice_set_mode",
    "voice_strict_set",
];

#[cfg(all(test, desktop))]
mod tests {
    use super::imp::parse_mode;

    /// 2026-11-17 起只有官方小智与 MOSS 内置服务两条链路，`self_hosted` 被拒绝。
    #[test]
    fn mode_parsing() {
        assert_eq!(
            parse_mode("self_hosted").unwrap_err().code,
            "voice_mode_invalid"
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
}
