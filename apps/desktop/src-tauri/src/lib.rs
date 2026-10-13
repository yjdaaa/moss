use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[cfg(desktop)]
use tauri::{Emitter, Manager};

mod export;
mod voice;
mod voice_builtin;
mod voice_mcp;

#[derive(Clone, Default)]
pub struct ShutdownState {
    pub requested: Arc<AtomicBool>,
}

struct BackendOrigin(Result<String, String>);

fn loopback_origin(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

#[tauri::command]
fn get_backend_origin(state: tauri::State<'_, BackendOrigin>) -> Result<String, String> {
    state.0.clone()
}

/// 主窗口特权命令的来源判定：只认 `main` 窗口，且页面来源与本进程后端地址完全一致。
///
/// 主窗口启动后导航到 `http://127.0.0.1:<端口>`，capability 的 `remote` 只能写成
/// `http://127.0.0.1:*`；端口在命令内固定，其他本机端口的页面即使进入窗口也拿不到特权命令。
#[cfg(desktop)]
pub(crate) fn is_trusted_main(label: &str, url: &tauri::Url, expected: &str) -> bool {
    label == "main"
        && url.username().is_empty()
        && url.password().is_none()
        && url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.origin().ascii_serialization() == expected
}

/// 按 [`is_trusted_main`] 校验调用窗口；后端未启动时一律拒绝。
#[cfg(desktop)]
pub(crate) fn window_is_trusted_main(window: &tauri::WebviewWindow) -> bool {
    let Some(expected) = window
        .app_handle()
        .try_state::<BackendOrigin>()
        .and_then(|state| state.0.as_ref().ok().cloned())
    else {
        return false;
    };
    window.url().is_ok_and(|url| is_trusted_main(window.label(), &url, &expected))
}

/// 桌面会话值（技术书 §5.3）：每次启动随机生成，只交给本窗口的页面并保存在其内存中，
/// 后端据 `X-Moss-Desktop-Session` 头识别桌面端。Android 没有桌面会话。
struct DesktopSession(Option<String>);

#[tauri::command]
fn get_desktop_session(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, DesktopSession>,
) -> Result<String, String> {
    #[cfg(mobile)]
    {
        let _ = (window, state);
        Err("移动端没有桌面会话".into())
    }
    #[cfg(desktop)]
    {
        // 会话值能批准 R3 动作，只交给后端地址上的主窗口
        if !window_is_trusted_main(&window) {
            return Err("桌面会话只提供给 MOSS 主窗口".into());
        }
        state.0.clone().ok_or_else(|| "桌面会话不可用：本机后端未启动".into())
    }
}

impl ShutdownState {
    pub fn request(&self) -> bool {
        let _ = laser_host::global_shutdown_coordinator().trigger_shutdown();
        !self.requested.swap(true, Ordering::SeqCst)
    }
}

#[tauri::command]
fn minimize_to_tray(window: tauri::WebviewWindow) {
    let _ = window.hide();
}

/// 完全退出。`stop_server`：MOSS 启动的语音服务端是否一并停止（默认是，技术书 §9.4.3）。
/// 顺序不变：先急停（`ShutdownState::request` → `ShutdownCoordinator`），再处理服务端，最后退出。
#[tauri::command]
async fn exit_application(app: tauri::AppHandle, stop_server: Option<bool>) -> Result<(), String> {
    use tauri::Manager;
    app.state::<ShutdownState>().request();
    let stop = stop_server.unwrap_or(true);
    let handle = app.clone();
    // 停止服务端最长约 5 秒，放到阻塞线程，避免卡住窗口
    let _ = tauri::async_runtime::spawn_blocking(move || {
        #[cfg(desktop)]
        {
            voice::shutdown(&handle, stop);
            voice_builtin::shutdown(&handle);
            voice_mcp::shutdown(&handle);
        }
        #[cfg(mobile)]
        let _ = stop;
        handle.exit(0);
    })
    .await;
    Ok(())
}

/// Opens a second packaged Tauri webview for private voice settings.
/// The loopback-hosted main page can only request the window, not sensitive data.
#[tauri::command]
fn open_voice_private(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    #[cfg(desktop)]
    {
        let current = window.url().map_err(|_| "无法校验当前窗口来源")?;
        let backend = app.state::<BackendOrigin>();
        let on_expected_backend = backend.0.as_ref().ok()
            .is_some_and(|origin| current.origin().ascii_serialization() == *origin);
        let on_packaged_main = (
            matches!(current.scheme(), "http" | "https")
                && current.host_str() == Some("tauri.localhost")
                && current.port().is_none()
        ) || (current.scheme() == "tauri" && current.host_str() == Some("localhost"));
        if window.label() != "main" || (!on_expected_backend && !on_packaged_main) {
            return Err("只能从受信任的 MOSS 主窗口打开语音隐私设置".into());
        }

        if let Some(existing) = app.get_webview_window("voice-private") {
            existing.show().map_err(|_| "无法显示语音隐私窗口")?;
            existing.set_focus().map_err(|_| "无法聚焦语音隐私窗口")?;
            return Ok(());
        }
        let private = tauri::WebviewWindowBuilder::new(
            &app, "voice-private", tauri::WebviewUrl::App("voice-private.html".into()),
        )
        .title("MOSS · 语音隐私设置")
        .inner_size(980.0, 760.0)
        .min_inner_size(700.0, 500.0)
        .on_navigation(|url| {
            voice_builtin::is_voice_private_asset(
                "voice-private", url.scheme(), url.host_str(), url.port(),
                url.path(), url.query(), url.fragment(),
            )
        })
        .build()
        .map_err(|_| "无法创建语音隐私窗口")?;
        private.set_focus().map_err(|_| "无法聚焦语音隐私窗口")?;
        Ok(())
    }
    #[cfg(mobile)]
    {
        let _ = (app, window);
        Err("语音隐私设置仅支持 MOSS 桌面端".into())
    }
}

/// 启动进程内后端，返回（后端句柄, 前端使用的后端地址或失败原因, 导出设置状态）。
fn start_backend(
    runtime_dir: Option<std::path::PathBuf>,
) -> (Option<laser_host::HostApplication>, Result<String, String>, export::ExportState) {
    let (host, backend_origin) = match laser_host::HostApplication::init_with_runtime_dir(runtime_dir.as_deref()) {
        Ok(host) => {
            let origin = match host.spawn_http_server() {
                Ok((_handle, port)) => Ok(loopback_origin(port)),
                Err(e) => {
                    eprintln!("警告: 后端服务监听启动失败: {e}");
                    Err(format!("后端服务无法监听本机端口: {e}"))
                }
            };
            // 小智 MCP 桥接（MCP_ENDPOINT 未配置时静默跳过）
            let _mcp_bridge = host.spawn_mcp_bridge();
            (Some(host), origin)
        }
        Err(e) => {
            eprintln!("警告: 后端服务进程内初始化提示: {e}");
            // Windows 发行版与 Android 没有控制台，eprintln 看不到；把原因交给页面显示。
            (None, Err(format!("后端服务初始化失败: {e}")))
        }
    };
    // 导出设置保存在运行目录 settings.json（与后端共用文件，读-改-写时保留其他键）
    let export_state = export::ExportState {
        settings_file: host.as_ref().map(|host| host.runtime_root.path.join("settings.json")),
    };
    (host, backend_origin, export_state)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 路径 5: Panic Hook 同步脱机急停
    laser_host::install_panic_hook();
    // 路径 4: Windows 控制台关闭事件
    laser_host::install_console_ctrl_handler();

    // 桌面端：在进程内启动并持有 laser-host 后端服务（规格 §16），数据目录按默认规则（Windows 为 %APPDATA%\moss）。
    // 移动端：当前目录只读且没有 APPDATA，必须等 setup 拿到应用私有数据目录后再启动（见下方 mobile setup）。
    #[cfg(desktop)]
    let (_host_handle, backend_origin, export_state) = start_backend(None);

    // 只有桌面端开启会话；独立 moss-host 与 Android 都没有
    #[cfg(desktop)]
    let desktop_session = _host_handle
        .as_ref()
        .and_then(|host| host.api_service.enable_desktop_session().ok());

    // 语音中心（技术书 §9.4，仅桌面端）：后端交给语音中心共享持有，与进程同寿命
    #[cfg(desktop)]
    let _host_handle = _host_handle.map(Arc::new);
    // 内置语音服务（内置语音技术书 NV1 起）：语音链路为“内置服务”时随 MOSS 在后台启动，否则由语音中心手动启动
    #[cfg(desktop)]
    let mcp_state = voice_mcp::init(_host_handle.clone());
    #[cfg(desktop)]
    let builtin_state = voice_builtin::init(_host_handle.clone());
    #[cfg(desktop)]
    voice_builtin::autostart(&builtin_state);
    #[cfg(desktop)]
    let voice_state = voice::init(_host_handle.clone(), builtin_state.0.clone());

    let state = ShutdownState::default();
    let builder = tauri::Builder::default()
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![
            get_backend_origin,
            get_desktop_session,
            export::save_export_file,
            export::get_export_settings,
            export::pick_export_dir,
            export::reset_export_dir,
            export::set_export_ask_each_time,
            export::open_export_dir,
            export::reveal_exported_file,
            minimize_to_tray,
            exit_application,
            voice::voice_status,
            voice::voice_save_settings,
            voice::voice_mode_changes,
            voice::voice_set_mode,
            voice::voice_server_start,
            voice::voice_server_stop,
            voice::voice_server_restart,
            voice::voice_server_log,
            voice::voice_models_test,
            voice::voice_models_save,
            voice_builtin::voice_music_pick_directory,
            voice_builtin::voice_music_status,
            voice_builtin::voice_music_save,
            voice_builtin::voice_home_status,
            voice_builtin::voice_home_save,
            voice_builtin::voice_home_backup_export,
            voice_builtin::voice_home_backup_audit,
            voice_builtin::voice_home_backup_restore,
            voice_builtin::voice_home_token_save,
            voice_builtin::voice_home_voice_consent,
            voice_builtin::voice_home_approvals,
            voice_builtin::voice_home_approval_cancel,
            voice_builtin::voice_home_control_consent,
            voice_builtin::voice_home_approval_approve,
            voice_builtin::voice_speaker_manage,
            voice_builtin::voice_firmware_status,
            voice_builtin::voice_home_discover,
            voice_builtin::voice_agent_status,
            voice_builtin::voice_agent_save,
            voice_builtin::voice_vision_status,
            voice_builtin::voice_vision_save,
            voice_builtin::voice_vision_test,
            open_voice_private,
            voice_builtin::voice_plugins_status,
            voice_builtin::voice_plugins_save,
            voice_builtin::voice_memory_list,
            voice_builtin::voice_memory_set_enabled,
            voice_builtin::voice_memory_save,
            voice_builtin::voice_memory_clear,
            voice_builtin::voice_builtin_status,
            voice_builtin::voice_builtin_start,
            voice_builtin::voice_builtin_stop,
            voice_builtin::voice_device_set_status,
            voice_builtin::voice_device_rename,
            voice_builtin::voice_builtin_models_test,
            voice_builtin::voice_builtin_models_save,
            voice_builtin::voice_local_save,
            voice_builtin::voice_local_model_download,
            voice_builtin::voice_local_model_cancel,
            voice_builtin::voice_local_model_delete,
            voice_mcp::voice_mcp_status,
            voice_mcp::voice_mcp_save,
            voice_mcp::voice_mcp_delete,
            voice_mcp::voice_mcp_reconnect
        ]);

    #[cfg(desktop)]
    let builder = builder
        .manage(BackendOrigin(backend_origin.clone()))
        .manage(export_state)
        .manage(DesktopSession(desktop_session))
        .manage(voice_state)
        .manage(builtin_state)
        .manage(mcp_state);

    #[cfg(mobile)]
    let builder = builder.setup(|app| {
        use tauri::Manager;
        let (host, backend_origin, export_state) = match app.path().app_data_dir() {
            Ok(dir) => start_backend(Some(dir)),
            Err(e) => (
                None,
                Err(format!("后端服务初始化失败: 找不到应用数据目录: {e}")),
                export::ExportState { settings_file: None },
            ),
        };
        // 后端（数据库、数据目录锁、HTTP 服务）须与进程同寿命；移动端 run() 可能先于应用结束返回。
        if let Some(host) = host {
            Box::leak(Box::new(host));
        }
        app.manage(BackendOrigin(backend_origin));
        app.manage(export_state);
        app.manage(DesktopSession(None));
        Ok(())
    });

    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            use tauri::menu::{Menu, MenuItem};
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

            // 构建系统托盘右键菜单
            let show_i = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "退出程序", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &quit_i])?;

            // 获取默认窗口图标
            let icon = app.default_window_icon().cloned().expect("缺少默认窗口图标");

            // 同源窗口导航: 直接打开本地同源服务，彻底消除 CORS 跨域问题
            if let Ok(origin) = &backend_origin {
                if let Some(window) = app.get_webview_window("main") {
                    if let Ok(parsed) = format!("{origin}/").parse() {
                        let _ = window.navigate(parsed);
                    }
                }
            }

            let _tray = TrayIconBuilder::with_id("moss-tray")
                .icon(icon)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .tooltip("MOSS 激光加工系统")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.unminimize();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        if !voice::owns_running_server(app) {
                            let state = app.state::<ShutdownState>();
                            state.request();
                            app.exit(0);
                            return;
                        }
                        // MOSS 启动的语音服务端在运行：询问是否一并停止（默认停止，技术书 §9.4.3）
                        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
                        let handle = app.clone();
                        app.dialog()
                            .message("MOSS 启动的语音服务端（xiaozhi-server）仍在运行。\n退出 MOSS 时是否同时停止它？")
                            .title("退出 MOSS")
                            .kind(MessageDialogKind::Warning)
                            .buttons(MessageDialogButtons::OkCancelCustom(
                                "停止服务端并退出".into(),
                                "保留服务端并退出".into(),
                            ))
                            .show(move |stop| {
                                std::thread::spawn(move || {
                                    // 先急停，再处理服务端
                                    handle.state::<ShutdownState>().request();
                                    voice::shutdown(&handle, stop);
                                    voice_builtin::shutdown(&handle);
            voice_mcp::shutdown(&handle);
                                    handle.exit(0);
                                });
                            });
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            if window.is_visible().unwrap_or(false) {
                                let _ = window.hide();
                            } else {
                                let _ = window.show();
                                let _ = window.unminimize();
                                let _ = window.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(move |window, event| {
            // 路径 1: Tauri 窗口关闭事件拦截与自适应安全流转
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Private settings windows must be destroyed on close, not
                // trapped as hidden windows retaining personal memory data.
                if window.label() != "main" { return; }
                // 拦截操作系统粗暴强杀以保护可能正在运行的激光加工任务
                api.prevent_close();
                // 优先通知前端弹出“最小化到托盘 / 完全退出”选择框
                if let Err(e) = window.emit("moss:close-requested", ()) {
                    eprintln!("通知前端关闭弹窗失败，自动兜底最小化到系统托盘: {e}");
                    let _ = window.hide();
                }
            }
        });

    builder
        .run(tauri::generate_context!())
        .expect("error while running MOSS desktop");
}

#[cfg(test)]
mod tests {
    use super::{ShutdownState, loopback_origin};

    /// 前端 `invoke` 的命令必须同时登记在 build.rs 的 AppManifest 并在默认 capability 授权，
    /// 否则运行时报 “Command … not allowed by ACL”（2026-10-06 get_backend_origin 漏登记导致启动失败页）。
    #[test]
    fn frontend_invoked_commands_are_registered_and_granted() {
        let build_rs = include_str!("../build.rs");
        let capability = format!("{}\n{}\n{}\n{}\n{}\n{}\n{}", include_str!("../capabilities/default.json"), include_str!("../capabilities/voice-memory.json"), include_str!("../capabilities/voice-plugins.json"), include_str!("../capabilities/voice-vision.json"), include_str!("../capabilities/voice-agent.json"), include_str!("../capabilities/voice-music.json"), include_str!("../capabilities/voice-home.json"));
        for command in [
            "open_voice_private",
            "get_backend_origin",
            "get_desktop_session",
            "save_export_file",
            "get_export_settings",
            "pick_export_dir",
            "reset_export_dir",
            "set_export_ask_each_time",
            "open_export_dir",
            "reveal_exported_file",
            "minimize_to_tray",
            "exit_application",
        ]
        .into_iter()
        .chain(crate::voice::COMMANDS)
        .chain(crate::voice_builtin::COMMANDS)
        {
            assert!(build_rs.contains(&format!("\"{command}\"")), "build.rs 未登记 {command}");
            let permission = format!("\"allow-{}\"", command.replace('_', "-"));
            assert!(capability.contains(&permission), "capability 未授权 {permission}");
        }
    }

    #[test]
    fn voice_memory_permissions_are_packaged_private_window_only() {
        let private: serde_json::Value = serde_json::from_str(include_str!("../capabilities/voice-memory.json")).unwrap();
        let general = include_str!("../capabilities/default.json");
        assert_eq!(private["windows"], serde_json::json!(["voice-private"]));
        assert!(private.get("remote").is_none());
        for permission in private["permissions"].as_array().unwrap() {
            assert!(!general.contains(permission.as_str().unwrap()));
        }
    }

    /// Main navigates to loopback HTTP; privileged commands must NEVER be
    /// granted to it or to other remote windows.
    #[test]
    fn voice_private_acl_does_not_expose_sensitive_data_to_loopback() {
        let default: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        let memory: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/voice-memory.json")).unwrap();
        let plugins: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/voice-plugins.json")).unwrap();
        assert_eq!(default["windows"], serde_json::json!(["*"]));
        assert!(default["remote"].get("urls").is_some());
        assert!(default["permissions"].as_array().unwrap().iter()
            .any(|item| item == "allow-open-voice-private"));
        for (name, capability, expected) in [
            ("memory", &memory, &[
                "allow-voice-memory-list", "allow-voice-memory-set-enabled",
                "allow-voice-memory-save", "allow-voice-memory-clear",
                "allow-voice-speaker-manage",
                "allow-voice-firmware-status",
            ][..]),
            ("plugins", &plugins, &[
                "allow-voice-plugins-status", "allow-voice-plugins-save",
            ][..]),
        ] {
            assert_eq!(capability["windows"], serde_json::json!(["voice-private"]), "{name}");
            assert!(capability.get("remote").is_none(), "{name} cannot allow remote origins");
            assert!(capability.get("webviews").is_none(), "{name} must be scoped by window label");
            assert_eq!(capability["permissions"].as_array().unwrap().len(), expected.len());
            for permission in expected {
                assert!(capability["permissions"].as_array().unwrap()
                    .iter().any(|entry| entry == permission));
                assert!(!default["permissions"].as_array().unwrap()
                    .iter().any(|entry| entry == permission),
                    "{permission} must not appear in wildcard/remote default ACL");
            }
        }
        let main = include_str!("../../../../frontend/src/components/VoiceBuiltinCard.jsx");
        let private = include_str!("../../../../frontend/src/voicePrivate.jsx");
        assert!(!main.contains("<VoicePluginsCard"));
        assert!(!main.contains("<VoiceMemoryCard"));
        assert!(private.contains("<VoicePluginsCard"));
        assert!(private.contains("<VoiceMemoryCard"));
        assert!(include_str!("../../../../frontend/voice-private.html")
            .contains("src/voicePrivate.jsx"));
    }

    #[test]
    fn voice_home_assistant_settings_are_packaged_private_window_only() {
        let private: serde_json::Value = serde_json::from_str(
            include_str!("../capabilities/voice-home.json")
        ).unwrap();
        let default = include_str!("../capabilities/default.json");
        assert_eq!(private["windows"], serde_json::json!(["voice-private"]));
        assert!(private.get("remote").is_none());
        for permission in private["permissions"].as_array().unwrap() {
            assert!(!default.contains(permission.as_str().unwrap()));
        }
    }

    #[test]
    fn voice_music_folder_configuration_main_window_only() {
        let private:serde_json::Value = serde_json::from_str(
            include_str!("../capabilities/voice-music.json")).unwrap();
        let general = include_str!("../capabilities/default.json");
        assert_main_window_loopback_capability(&private);
        for permission in private["permissions"].as_array().unwrap() {
            assert!(!general.contains(permission.as_str().unwrap()));
        }
    }

    #[test]
    fn voice_plugin_credentials_are_packaged_private_window_only() {
        let private: serde_json::Value = serde_json::from_str(include_str!("../capabilities/voice-plugins.json")).unwrap();
        let general = include_str!("../capabilities/default.json");
        assert_eq!(private["windows"], serde_json::json!(["voice-private"]));
        assert!(private.get("remote").is_none());
        for permission in private["permissions"].as_array().unwrap() {
            assert!(!general.contains(permission.as_str().unwrap()));
        }
    }

    #[test]
    fn agent_approval_configuration_main_window_only(){
        let private:serde_json::Value=serde_json::from_str(include_str!("../capabilities/voice-agent.json")).unwrap();
        let general=include_str!("../capabilities/default.json");
        assert_main_window_loopback_capability(&private);
        for permission in private["permissions"].as_array().unwrap(){
            assert!(!general.contains(permission.as_str().unwrap()));
        }
    }

    #[test]
    fn vision_credentials_private_capability() {
        let private:serde_json::Value=serde_json::from_str(include_str!("../capabilities/voice-vision.json")).unwrap();
        let default=include_str!("../capabilities/default.json");
        assert_main_window_loopback_capability(&private);
        for value in private["permissions"].as_array().unwrap(){
            assert!(!default.contains(value.as_str().unwrap()));
        }
    }

    /// 主窗口导航到 `http://127.0.0.1:<端口>` 后属于远程来源：主窗口专用 capability 必须声明
    /// 回环 remote，否则 Tauri 只对本地打包页授权、主窗口调用一律被 ACL 拒绝（2026-10-13 审查）。
    /// 端口无法写进静态 capability，由命令内 `require_main_window` 固定为本进程后端地址。
    fn assert_main_window_loopback_capability(capability: &serde_json::Value) {
        assert_eq!(capability["windows"], serde_json::json!(["main"]));
        assert_eq!(capability["remote"]["urls"], serde_json::json!(["http://127.0.0.1:*"]));
        assert!(capability.get("webviews").is_none());
    }

    /// 改写设置、启停进程、保存密钥与读取私有配置的命令必须在函数开头固定主窗口来源。
    #[test]
    fn main_window_commands_pin_backend_origin() {
        let sources = [
            (include_str!("voice.rs"), &[
                "voice_save_settings", "voice_set_mode", "voice_server_start", "voice_server_stop",
                "voice_server_restart", "voice_server_log", "voice_models_test", "voice_models_save",
            ][..]),
            (include_str!("voice_builtin.rs"), &[
                "voice_builtin_start", "voice_builtin_stop", "voice_device_set_status", "voice_device_rename",
                "voice_builtin_models_test", "voice_builtin_models_save", "voice_local_save",
                "voice_local_model_download", "voice_local_model_cancel", "voice_local_model_delete",
                "voice_agent_status", "voice_agent_save", "voice_music_pick_directory", "voice_music_status",
                "voice_music_save", "voice_vision_status", "voice_vision_save", "voice_vision_test",
            ][..]),
        ];
        for (source, commands) in sources {
            for command in commands {
                let start = source.find(&format!("pub async fn {command}(")).expect(command);
                let body = &source[start..start + source[start..].find("
}
").expect(command)];
                let guard = body.find("require_main_window(&window)?;").unwrap_or_else(|| panic!("{command} 未校验主窗口来源"));
                assert!(guard < body.find("imp::run").or(body.find("spawn_blocking")).unwrap(), "{command} 须先校验来源");
            }
        }
        let session = include_str!("lib.rs");
        let start = session.find("fn get_desktop_session(").unwrap();
        assert!(session[start..start + 800].contains("window_is_trusted_main(&window)"));
    }

    #[cfg(desktop)]
    #[test]
    fn trusted_main_requires_main_label_and_exact_backend_origin() {
        let expected = "http://127.0.0.1:19666";
        assert!(super::is_trusted_main("main", &"http://127.0.0.1:19666/voice".parse().unwrap(), expected));
        for (label, url) in [
            ("voice-private", "http://127.0.0.1:19666/"),
            ("main", "http://127.0.0.1:8003/"),
            ("main", "http://localhost:19666/"),
            ("main", "http://tauri.localhost/"),
            ("main", "https://127.0.0.1:19666/"),
            ("main", "http://u@127.0.0.1:19666/"),
        ] {
            assert!(!super::is_trusted_main(label, &url.parse().unwrap(), expected), "{label} {url}");
        }
    }

    #[test]
    fn backend_origin_uses_actual_loopback_port() {
        assert_eq!(loopback_origin(9667), "http://127.0.0.1:9667");
    }

    #[test]
    fn shutdown_request_is_idempotent() {
        let state = ShutdownState::default();
        assert!(state.request());
        assert!(!state.request());
    }

    #[test]
    fn shutdown_triggers_host_coordinator() {
        let state = ShutdownState::default();
        state.request();
        assert!(laser_host::global_shutdown_coordinator().is_shutdown_requested());
    }
}
