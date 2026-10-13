//! 语音中心的 Tauri 命令（技术书 §9.4.6，实施清单 P4-07）。
//!
//! 以 Tauri 命令实现而不是 HTTP 路由：只有 MOSS 桌面窗口能调用，浏览器、局域网、手机与
//! 小智桥都够不到（等价于“仅桌面会话”）；独立的 `moss-host` 与 Android 没有这些接口，
//! HTTP `/v1/voice/*` 在那里返回 404（A-68）。进程管理代码只在桌面端编译（A-69）。
//!
//! 错误统一为 `{code, message, status}`，页面据 `code` 显示原因。

use serde_json::Value;

type CmdResult = Result<Value, Value>;

fn error_value(code: &str, message: &str) -> Value {
    serde_json::json!({"code": code, "message": message})
}

#[cfg(mobile)]
fn desktop_only<T>(_: T) -> CmdResult {
    Err(error_value("voice_desktop_only", "语音中心仅在 MOSS 桌面端可用"))
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
    pub struct VoiceState(pub Option<Arc<VoiceCenter>>);

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
            self.host.api_service.set_strict_mode(enabled || self.host.strict_from_env);
        }

        fn strict_from_env(&self) -> bool {
            self.host.strict_from_env
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
        assert_eq!(moss_voice::MAX_EXTRA_ENDPOINTS, laser_host::MAX_EXTRA_ENDPOINTS);
    }

    pub fn init(
        host: Option<Arc<laser_host::HostApplication>>,
        builtin: Option<Arc<crate::voice_builtin::BuiltinVoice>>,
    ) -> VoiceState {
        VoiceState(host.map(|host| {
            let runtime_dir = host.runtime_root.path.clone();
            let center = Arc::new(VoiceCenter::new(&runtime_dir, Arc::new(DesktopHooks { host, builtin })));
            // 自建模式下每 5 秒查一次接入点 health，供状态卡与顶栏“等待设备”使用
            center.spawn_health_poller(Duration::from_secs(5));
            center
        }))
    }

    fn center(app: &tauri::AppHandle) -> Result<Arc<VoiceCenter>, Value> {
        app.try_state::<VoiceState>()
            .and_then(|state| state.0.clone())
            .ok_or_else(|| error_value("voice_unavailable", "语音中心不可用：本机后端未启动"))
    }

    fn to_value(error: CenterError) -> Value {
        serde_json::to_value(error).unwrap_or_else(|_| error_value("voice_internal", "语音中心内部错误"))
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
            _ => Err(CenterError::new("voice_mode_invalid", 400, "链路模式只能是 official、self_hosted 或 builtin")),
        }
    }

    pub fn parse_section(section: &str) -> Result<moss_voice::Section, CenterError> {
        moss_voice::Section::parse(section)
            .ok_or_else(|| CenterError::new("model_input_invalid", 400, "分区只能是 llm、asr 或 tts"))
    }

    /// 是否有 MOSS 启动的服务端在运行（托盘退出时决定是否询问）。
    pub fn owns_running_server(app: &tauri::AppHandle) -> bool {
        center(app).map(|center| center.supervisor().owns_running_server()).unwrap_or(false)
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
pub async fn voice_save_settings(app: tauri::AppHandle, settings: Value) -> CmdResult {
    #[cfg(desktop)]
    let result = imp::run(app, move |center| center.save_settings(&settings)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, settings));
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
pub async fn voice_set_mode(app: tauri::AppHandle, mode: String) -> CmdResult {
    #[cfg(desktop)]
    let result = imp::run(app, move |center| center.set_mode(imp::parse_mode(&mode)?)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, mode));
    result
}

#[tauri::command]
pub async fn voice_server_start(app: tauri::AppHandle) -> CmdResult {
    #[cfg(desktop)]
    let result = imp::run(app, |center| center.server_start()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}

#[tauri::command]
pub async fn voice_server_stop(app: tauri::AppHandle) -> CmdResult {
    #[cfg(desktop)]
    let result = imp::run(app, |center| center.server_stop()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}

#[tauri::command]
pub async fn voice_server_restart(app: tauri::AppHandle) -> CmdResult {
    #[cfg(desktop)]
    let result = imp::run(app, |center| center.server_restart()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}

/// 已脱敏的服务端日志尾部（默认 200 行）。
#[tauri::command]
pub async fn voice_server_log(app: tauri::AppHandle, tail: Option<usize>) -> CmdResult {
    #[cfg(desktop)]
    let result = imp::run(app, move |center| Ok(serde_json::json!({"lines": center.log_tail(tail.unwrap_or(200))}))).await;
    #[cfg(mobile)]
    let result = desktop_only((app, tail));
    result
}

/// 测试一个分区。请求体中的密钥只在本次调用的内存中使用，结果不回显密钥。
#[tauri::command]
pub async fn voice_models_test(app: tauri::AppHandle, section: String, input: Value) -> CmdResult {
    #[cfg(desktop)]
    let result = imp::run(app, move |center| {
        let section = imp::parse_section(&section)?;
        let input: moss_voice::SectionInput = serde_json::from_value(input)
            .map_err(|_| moss_voice::CenterError::new("model_input_invalid", 400, "测试内容格式不正确"))?;
        let outcome = center.models_test(section, &input)?;
        serde_json::to_value(outcome).map_err(|_| moss_voice::CenterError::new("voice_internal", 500, "结果序列化失败"))
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, section, input));
    result
}

/// 保存到服务端 `data/.config.yaml`（每个非空分区须与测试通过的内容一致）。
#[tauri::command]
pub async fn voice_models_save(app: tauri::AppHandle, input: Value) -> CmdResult {
    #[cfg(desktop)]
    let result = imp::run(app, move |center| {
        let input: moss_voice::ModelsInput = serde_json::from_value(input)
            .map_err(|_| moss_voice::CenterError::new("model_input_invalid", 400, "保存内容格式不正确"))?;
        center.models_save(&input)
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}

/// 前端调用的语音中心命令（build.rs 登记与 capability 授权的测试会核对这张表）。
#[cfg(test)]
pub const COMMANDS: [&str; 10] = [
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
];

#[cfg(all(test, desktop))]
mod tests {
    use super::imp::{parse_mode, parse_section};

    #[test]
    fn mode_and_section_parsing() {
        assert_eq!(parse_mode("self_hosted").unwrap(), moss_voice::VoiceMode::SelfHosted);
        assert_eq!(parse_mode(" official ").unwrap(), moss_voice::VoiceMode::Official);
        assert_eq!(parse_mode("builtin").unwrap(), moss_voice::VoiceMode::Builtin);
        assert_eq!(parse_mode("gateway").unwrap_err().code, "voice_mode_invalid");
        assert_eq!(parse_section("tts").unwrap(), moss_voice::Section::Tts);
        assert!(parse_section("vad").is_err());
    }
}
