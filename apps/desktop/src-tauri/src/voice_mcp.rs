//! Only the main desktop window at this process's exact backend origin may manage external MCP.
use serde_json::{Value, json};
type CmdResult = Result<Value, Value>;
#[cfg(desktop)]
use std::sync::Arc;
#[cfg(desktop)]
use tauri::Manager;

#[cfg(desktop)]
pub struct McpState(Result<Arc<laser_host::external_mcp::ExternalMcp>, String>);
#[cfg(desktop)]
pub fn init(host: Option<Arc<laser_host::HostApplication>>) -> McpState {
    McpState(
        host.ok_or_else(|| "本机后端未启动".to_owned())
            .and_then(|host| {
                laser_host::external_mcp::ExternalMcp::attach(
                    &host.api_service,
                    host.db.clone(),
                    host.runtime_root.path.join("settings.json"),
                )
            }),
    )
}
#[cfg(desktop)]
pub fn shutdown(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<McpState>() {
        if let Ok(external) = &state.0 {
            external.shutdown();
        }
    }
}
#[cfg(desktop)]
fn trusted_window(label: &str, url: &tauri::Url, expected: &str) -> bool {
    label == "main"
        && url.username().is_empty()
        && url.password().is_none()
        && url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.origin().ascii_serialization() == expected
}
fn error(message: impl Into<String>) -> Value {
    json!({"code":"voice_mcp_error","message":message.into()})
}
#[cfg(desktop)]
async fn run<F>(window: tauri::WebviewWindow, task: F) -> CmdResult
where
    F: FnOnce(&laser_host::external_mcp::ExternalMcp) -> Result<Value, String> + Send + 'static,
{
    let app = window.app_handle();
    let expected = app
        .try_state::<crate::BackendOrigin>()
        .and_then(|s| s.0.as_ref().ok().cloned())
        .ok_or_else(|| error("桌面后端不可用"))?;
    let url = window.url().map_err(|_| error("无法核实窗口来源"))?;
    let has_session = app
        .try_state::<crate::DesktopSession>()
        .is_some_and(|s| s.0.is_some());
    if !has_session || !trusted_window(window.label(), &url, &expected) {
        return Err(error("只能在 MOSS 主窗口管理第三方 MCP"));
    }
    let external = app
        .try_state::<McpState>()
        .ok_or_else(|| error("MCP 未初始化"))?
        .0
        .clone()
        .map_err(error)?;
    tauri::async_runtime::spawn_blocking(move || task(&external).map_err(error))
        .await
        .map_err(|_| error("MCP 管理任务异常结束"))?
}

#[tauri::command]
pub async fn voice_mcp_status(window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    {
        run(window, |external| external.status()).await
    }
    #[cfg(mobile)]
    {
        let _ = window;
        Err(error("第三方 MCP 仅在桌面端可用"))
    }
}
#[tauri::command]
pub async fn voice_mcp_save(window: tauri::WebviewWindow, input: Value) -> CmdResult {
    #[cfg(desktop)]
    {
        run(window, move |external| {
            if serde_json::to_vec(&input)
                .map_err(|_| "配置格式不正确")?
                .len()
                > 128 * 1024
            {
                return Err("配置超过大小限制".into());
            }
            let input = serde_json::from_value(input).map_err(|_| "配置字段不正确")?;
            external.save(input)
        })
        .await
    }
    #[cfg(mobile)]
    {
        let _ = (window, input);
        Err(error("第三方 MCP 仅在桌面端可用"))
    }
}
#[tauri::command]
pub async fn voice_mcp_delete(
    window: tauri::WebviewWindow,
    name: String,
    revision: String,
    confirmed: bool,
) -> CmdResult {
    #[cfg(desktop)]
    {
        run(window, move |external| {
            external.delete(&name, &revision, confirmed)
        })
        .await
    }
    #[cfg(mobile)]
    {
        let _ = (window, name, revision, confirmed);
        Err(error("第三方 MCP 仅在桌面端可用"))
    }
}
#[tauri::command]
pub async fn voice_mcp_reconnect(
    window: tauri::WebviewWindow,
    name: String,
    confirmed: bool,
) -> CmdResult {
    #[cfg(desktop)]
    {
        run(window, move |external| external.reconnect(&name, confirmed)).await
    }
    #[cfg(mobile)]
    {
        let _ = (window, name, confirmed);
        Err(error("第三方 MCP 仅在桌面端可用"))
    }
}

#[cfg(test)]
pub const COMMANDS: [&str; 4] = [
    "voice_mcp_status",
    "voice_mcp_save",
    "voice_mcp_delete",
    "voice_mcp_reconnect",
];
#[cfg(all(test, desktop))]
mod tests {
    use super::*;
    #[test]
    fn management_is_pinned_to_the_main_window_and_actual_backend_port() {
        let expected = "http://127.0.0.1:19666";
        assert!(trusted_window(
            "main",
            &format!("{expected}/").parse().unwrap(),
            expected
        ));
        for (label, url) in [
            ("other", "http://127.0.0.1:19666/"),
            ("main", "http://127.0.0.1:19667/"),
            ("main", "https://example.test/"),
            ("main", "http://127.0.0.1.evil.test:19666/"),
            ("main", "http://u:p@127.0.0.1:19666/"),
        ] {
            assert!(!trusted_window(label, &url.parse().unwrap(), expected));
        }
    }
    #[test]
    fn external_management_permissions_are_not_granted_to_every_window() {
        let capability: Value =
            serde_json::from_str(include_str!("../capabilities/voice-mcp.json")).unwrap();
        assert_eq!(capability["windows"], json!(["main"]));
        let build = include_str!("../build.rs");
        for command in COMMANDS {
            assert!(build.contains(command));
            assert!(
                capability["permissions"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(format!("allow-{}", command.replace('_', "-"))))
            );
        }
    }
}
