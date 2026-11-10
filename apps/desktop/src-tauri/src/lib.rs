use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[cfg(desktop)]
use tauri::{Emitter, Manager};

mod app_update;
mod export;
mod voice;
mod voice_builtin;
mod voice_context;
mod voice_mcp;

#[derive(Clone, Default)]
pub struct ShutdownState {
    pub requested: Arc<AtomicBool>,
    pub services_stopped: Arc<AtomicBool>,
}

struct BackendOrigin(Result<String, String>);

#[cfg(desktop)]
struct DevUiOrigin(Option<String>);

fn loopback_origin(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// 开发热重载前端地址白名单：只允许本机 `127.0.0.1` 上的固定 Vite 端口（`5177` / `5173`）。
#[cfg(desktop)]
#[cfg_attr(not(debug_assertions), allow(dead_code))]
pub(crate) fn parse_dev_ui_origin(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim().trim_end_matches('/');
    matches!(trimmed, "http://127.0.0.1:5177" | "http://127.0.0.1:5173").then(|| trimmed.to_owned())
}

/// 读取开发热重载前端来源：release 构建恒为 `None`，环境变量在正式版中完全失效。
#[cfg(desktop)]
fn resolve_dev_ui_origin() -> Option<String> {
    #[cfg(debug_assertions)]
    {
        parse_dev_ui_origin(std::env::var("MOSS_DEV_UI_ORIGIN").ok().as_deref())
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
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

/// 叠加 debug 开发热重载来源的主窗口校验：正式构建只认 `expected`（本进程后端地址）。
#[cfg(desktop)]
pub(crate) fn is_trusted_main_with_dev(
    label: &str,
    url: &tauri::Url,
    expected: &str,
    dev_ui_origin: Option<&str>,
) -> bool {
    if is_trusted_main(label, url, expected) {
        return true;
    }
    #[cfg(debug_assertions)]
    if let Some(dev_origin) = parse_dev_ui_origin(dev_ui_origin) {
        return is_trusted_main(label, url, &dev_origin);
    }
    #[cfg(not(debug_assertions))]
    let _ = dev_ui_origin;
    false
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
    let dev_ui_origin = window
        .app_handle()
        .try_state::<DevUiOrigin>()
        .and_then(|state| state.0.clone());
    window.url().is_ok_and(|url| {
        is_trusted_main_with_dev(window.label(), &url, &expected, dev_ui_origin.as_deref())
    })
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
        state
            .0
            .clone()
            .ok_or_else(|| "桌面会话不可用：本机后端未启动".into())
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
    let _ = tauri::async_runtime::spawn_blocking(move || shutdown_and_exit(&handle, stop)).await;
    Ok(())
}

/// 幂等停止桌面端托管子进程与内置语音服务（`shutdown_and_exit` 与 macOS `Cmd+Q` / Dock `RunEvent::Exit` 共用，仅执行一次）。
fn shutdown_desktop_services(handle: &tauri::AppHandle, stop_server: bool) {
    use tauri::Manager;
    let state = handle.state::<ShutdownState>();
    state.request();
    if state.services_stopped.swap(true, Ordering::SeqCst) {
        return;
    }
    #[cfg(desktop)]
    {
        voice::shutdown(handle, stop_server);
        voice_builtin::shutdown(handle);
        voice_mcp::shutdown(handle);
    }
    #[cfg(mobile)]
    let _ = stop_server;
}

/// 退出的唯一收尾顺序：先急停，再停止（或放开）语音服务端、内置语音服务与第三方 MCP 子进程，最后退出。
/// 关闭窗口退出与托盘退出都走这里（2026-10-14 前托盘在没有自有服务端时直接退出，漏了后两项）。
fn shutdown_and_exit(handle: &tauri::AppHandle, stop_server: bool) {
    shutdown_desktop_services(handle, stop_server);
    handle.exit(0);
}

/// 启动进程内后端，返回（后端句柄, 前端使用的后端地址或失败原因, 导出设置状态）。
fn start_backend(
    runtime_dir: Option<std::path::PathBuf>,
) -> (
    Option<laser_host::HostApplication>,
    Result<String, String>,
    export::ExportState,
) {
    let (host, backend_origin) =
        match laser_host::HostApplication::init_with_runtime_dir(runtime_dir.as_deref()) {
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
        settings_file: host
            .as_ref()
            .map(|host| host.runtime_root.path.join("settings.json")),
    };
    (host, backend_origin, export_state)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 路径 5: Panic Hook 同步脱机急停
    laser_host::install_panic_hook();
    // 路径 4: Windows 控制台关闭事件
    laser_host::install_console_ctrl_handler();

    // 桌面端：在进程内启动并持有 laser-host 后端服务（规格 §16），数据目录按平台默认规则
    // （Windows 为 %APPDATA%\moss，macOS 为 ~/Library/Application Support/moss，Linux 为 ~/.local/share/moss）。
    // 移动端：当前目录只读且无桌面用户环境变量，必须等 setup 拿到应用私有数据目录后再启动（见下方 mobile setup）。
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
            app_update::app_update_status,
            app_update::app_update_install,
            voice::voice_status,
            voice::voice_detect_python,
            voice::voice_strict_set,
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
            voice_builtin::voice_firmware_import,
            voice_builtin::voice_firmware_manage,
            voice_builtin::voice_wakeword_status,
            voice_builtin::voice_wakeword_import_base,
            voice_builtin::voice_wakeword_manage,
            voice_builtin::voice_home_discover,
            voice_builtin::voice_agent_status,
            voice_builtin::voice_agent_save,
            voice_builtin::voice_agent_stop,
            voice_builtin::voice_vision_status,
            voice_builtin::voice_vision_save,
            voice_builtin::voice_vision_test,
            voice_builtin::voice_plugins_status,
            voice_builtin::voice_plugins_save,
            voice_builtin::voice_memory_list,
            voice_builtin::voice_memory_set_enabled,
            voice_builtin::voice_memory_save,
            voice_builtin::voice_memory_clear,
            voice_context::voice_context_manage,
            voice_context::voice_context_pick_folder,
            voice_builtin::voice_builtin_status,
            voice_builtin::voice_builtin_start,
            voice_builtin::voice_builtin_stop,
            voice_builtin::voice_diagnostics_export,
            voice_builtin::voice_colab_export_notebook,
            voice_builtin::voice_device_set_status,
            voice_builtin::voice_device_rename,
            voice_builtin::voice_device_request,
            voice_builtin::voice_builtin_models_test,
            voice_builtin::voice_builtin_models_list,
            voice_builtin::voice_builtin_models_save,
            voice_builtin::voice_local_save,
            voice_builtin::voice_local_model_download,
            voice_builtin::voice_local_model_cancel,
            voice_builtin::voice_local_laya_setup,
            voice_builtin::voice_local_model_delete,
            voice_mcp::voice_mcp_status,
            voice_mcp::voice_mcp_save,
            voice_mcp::voice_mcp_delete,
            voice_mcp::voice_mcp_reconnect
        ]);

    #[cfg(desktop)]
    let dev_ui_origin = resolve_dev_ui_origin();

    #[cfg(desktop)]
    let builder = builder
        .manage(BackendOrigin(backend_origin.clone()))
        .manage(DevUiOrigin(dev_ui_origin.clone()))
        .manage(export_state)
        .manage(DesktopSession(desktop_session))
        .manage(voice_state)
        .manage(builtin_state)
        .manage(mcp_state)
        .manage(app_update::init(_host_handle.clone(), None));

    #[cfg(mobile)]
    let builder = builder.setup(|app| {
        use tauri::Manager;
        let (host, backend_origin, export_state) = match app.path().app_data_dir() {
            Ok(dir) => start_backend(Some(dir)),
            Err(e) => (
                None,
                Err(format!("后端服务初始化失败: 找不到应用数据目录: {e}")),
                export::ExportState {
                    settings_file: None,
                },
            ),
        };
        let host = host.map(Arc::new);
        app.manage(app_update::init(
            host.clone(),
            app.path().app_cache_dir().ok(),
        ));
        // 后端（数据库、数据目录锁、HTTP 服务）须与进程同寿命；移动端 run() 可能先于应用结束返回。
        if let Some(host) = host {
            std::mem::forget(host);
        }
        app.manage(BackendOrigin(backend_origin));
        app.manage(export_state);
        app.manage(DesktopSession(None));
        Ok(())
    });

    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_dialog::init())
        // 应用内更新：插件的前端命令不授权给任何页面，只由 app_update 模块在 Rust 侧调用
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            use tauri::menu::{Menu, MenuItem};
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

            // 构建系统托盘右键菜单
            let show_i = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "退出程序", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &quit_i])?;

            // 获取默认窗口图标
            let icon = app.default_window_icon().cloned().expect("缺少默认窗口图标");

            // 同源窗口导航: 默认打开本地同源后端服务；debug 热重载模式下可打开白名单回环 Vite 页面
            if let Ok(origin) = &backend_origin {
                let target_origin = dev_ui_origin.as_deref().unwrap_or(origin.as_str());
                if let Some(window) = app.get_webview_window("main")
                    && let Ok(parsed) = format!("{target_origin}/").parse() {
                        let _ = window.navigate(parsed);
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
                            app.state::<ShutdownState>().request();
                            let handle = app.clone();
                            std::thread::spawn(move || shutdown_and_exit(&handle, true));
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
                                std::thread::spawn(move || shutdown_and_exit(&handle, stop));
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
        .build(tauri::generate_context!())
        .expect("error while building MOSS desktop")
        .run(|app_handle, event| match event {
            // macOS：窗口隐藏到菜单栏图标后，点击 Dock 图标重新唤醒并聚焦主窗口
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen {
                has_visible_windows: false,
                ..
            } => {
                if let Some(window) = app_handle.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            }
            // 应用级退出（含 macOS Cmd+Q / Dock 退出）：幂等触发急停 0x18 并清理语音/本地推理子进程
            tauri::RunEvent::Exit => {
                shutdown_desktop_services(app_handle, true);
            }
            _ => {
                let _ = app_handle;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::{ShutdownState, loopback_origin};

    /// 前端 `invoke` 的命令必须同时登记在 build.rs 的 AppManifest 并在默认 capability 授权，
    /// 否则运行时报 “Command … not allowed by ACL”（2026-10-06 get_backend_origin 漏登记导致启动失败页）。
    #[test]
    fn frontend_invoked_commands_are_registered_and_granted() {
        let build_rs = include_str!("../build.rs");
        let capability = [
            include_str!("../capabilities/default.json"),
            include_str!("../capabilities/voice-vision.json"),
            include_str!("../capabilities/voice-agent.json"),
            include_str!("../capabilities/voice-music.json"),
            include_str!("../capabilities/voice-mcp.json"),
            include_str!("../capabilities/voice-private-main.json"),
        ]
        .join("\n");
        for command in [
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
        .chain(crate::app_update::COMMANDS)
        .chain(crate::voice::COMMANDS)
        .chain(crate::voice_builtin::COMMANDS)
        .chain(crate::voice_context::COMMANDS)
        .chain(crate::voice_mcp::COMMANDS)
        {
            assert!(
                build_rs.contains(&format!("\"{command}\"")),
                "build.rs 未登记 {command}"
            );
            let permission = format!("\"allow-{}\"", command.replace('_', "-"));
            assert!(
                capability.contains(&permission),
                "capability 未授权 {permission}"
            );
        }
    }

    /// Windows deadlocks when a webview window is built inside a synchronous command
    /// (2026-10-15: the private window stayed white and the app hung, AppHangB1).
    #[test]
    fn window_creating_commands_are_async() {
        // 只查测试模块之前的产品代码（测试体里的字符串本身不算）；n67 起已没有建窗口的命令
        let full = include_str!("lib.rs");
        let source = &full[..full.find("#[cfg(test)]\nmod tests").expect("tests module")];
        for (at, _) in source.match_indices("WebviewWindowBuilder::new(") {
            let head = &source[..at];
            let function = head.rfind("fn ").expect("builder outside a function");
            assert!(
                head[..function].ends_with("async "),
                "window builder inside a non-async fn"
            );
        }
    }

    /// 敏感语音设置（记忆、信息服务、声纹、家居、长期记忆、唤醒词、固件）只授权给主窗口：
    /// n26（2026-10-18）四项就地展开，n67（2026-11-07）取消独立安全窗口、唤醒词与固件放进语音中心。
    /// 通配 / 远程 default capability 不得含这些权限；每条命令开头都由 require_private_settings_window
    /// 把来源固定为本进程后端上的主窗口。
    #[test]
    fn sensitive_voice_settings_are_trusted_main_window_only() {
        const SENSITIVE: &[&str] = &[
            "allow-voice-memory-list",
            "allow-voice-memory-set-enabled",
            "allow-voice-memory-save",
            "allow-voice-memory-clear",
            "allow-voice-speaker-manage",
            "allow-voice-plugins-status",
            "allow-voice-plugins-save",
            "allow-voice-home-status",
            "allow-voice-home-save",
            "allow-voice-home-backup-export",
            "allow-voice-home-backup-audit",
            "allow-voice-home-backup-restore",
            "allow-voice-home-token-save",
            "allow-voice-home-voice-consent",
            "allow-voice-home-discover",
            "allow-voice-home-approvals",
            "allow-voice-home-approval-cancel",
            "allow-voice-home-control-consent",
            "allow-voice-home-approval-approve",
            "allow-voice-context-manage",
            "allow-voice-context-pick-folder",
            "allow-voice-firmware-status",
            "allow-voice-firmware-import",
            "allow-voice-firmware-manage",
            "allow-voice-wakeword-status",
            "allow-voice-wakeword-import-base",
            "allow-voice-wakeword-manage",
        ];
        let default: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        assert_eq!(default["windows"], serde_json::json!(["*"]));
        let default_permissions = default["permissions"].as_array().unwrap();
        assert!(
            !default_permissions
                .iter()
                .any(|item| item == "allow-open-voice-private"),
            "独立安全窗口已取消"
        );
        let main: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/voice-private-main.json")).unwrap();
        assert_main_window_loopback_capability(&main);
        let main_permissions = main["permissions"].as_array().unwrap();
        assert_eq!(main_permissions.len(), SENSITIVE.len());
        for permission in SENSITIVE {
            assert!(
                main_permissions.iter().any(|entry| entry == permission),
                "{permission} 应授权给主窗口"
            );
            assert!(
                !default_permissions.iter().any(|entry| entry == permission),
                "{permission} must not appear in wildcard/remote default ACL"
            );
        }
        let guarded = [
            (
                include_str!("voice_builtin.rs"),
                &[
                    "voice_memory_list",
                    "voice_memory_set_enabled",
                    "voice_memory_save",
                    "voice_memory_clear",
                    "voice_speaker_manage",
                    "voice_plugins_status",
                    "voice_plugins_save",
                    "voice_firmware_status",
                    "voice_firmware_import",
                    "voice_firmware_manage",
                    "voice_wakeword_status",
                    "voice_wakeword_import_base",
                    "voice_wakeword_manage",
                    "voice_home_status",
                    "voice_home_save",
                    "voice_home_backup_export",
                    "voice_home_backup_audit",
                    "voice_home_backup_restore",
                    "voice_home_token_save",
                    "voice_home_voice_consent",
                    "voice_home_discover",
                    "voice_home_approvals",
                    "voice_home_approval_cancel",
                    "voice_home_control_consent",
                    "voice_home_approval_approve",
                ][..],
            ),
            (
                include_str!("voice_context.rs"),
                &["voice_context_manage", "voice_context_pick_folder"][..],
            ),
        ];
        for (source, commands) in guarded {
            for command in commands {
                let start = source
                    .find(&format!("pub async fn {command}("))
                    .expect(command);
                let body = &source[start..start + source[start..].find("\n}\n").expect(command)];
                assert!(
                    body.contains("require_private_settings_window(&window)?"),
                    "{command} 须固定主窗口来源"
                );
            }
        }
        let guard = include_str!("voice_builtin.rs");
        let start = guard.find("fn require_private_settings_window(").unwrap();
        let body = &guard[start..start + guard[start..].find("\n}\n").unwrap()];
        assert!(body.contains("crate::window_is_trusted_main(window)"));
        assert!(
            !guard.contains("is_voice_private_asset"),
            "打包私密页判定已随独立窗口删除"
        );
        // 卡片都挂在主窗口语音中心
        let builtin_card = include_str!("../../../../frontend/src/components/VoiceBuiltinCard.jsx");
        for card in [
            "<VoiceMemoryCard",
            "<VoicePluginsCard",
            "<VoiceSpeakerCard",
            "<VoiceHomeCard",
        ] {
            assert!(builtin_card.contains(card), "主语音页需要 {card}");
        }
        let voice_page = include_str!("../../../../frontend/src/pages/VoicePage.jsx");
        assert!(
            voice_page.contains("<VoiceFirmwareCard"),
            "固件卡挂在“设备”标签"
        );
        // n69：“唤醒词与音量”标签并入“设备”，唤醒词卡由 VoiceWakeWordSection 挂载
        assert!(
            voice_page.contains("<VoiceWakeWordSection"),
            "唤醒词区块挂在“设备”标签"
        );
        let wake_section =
            include_str!("../../../../frontend/src/components/VoiceWakeWordSection.jsx");
        assert!(
            wake_section.contains("<VoiceWakeWordCard"),
            "唤醒词卡挂在主窗口语音中心"
        );
    }

    #[test]
    fn voice_music_folder_configuration_main_window_only() {
        let private: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/voice-music.json")).unwrap();
        let general = include_str!("../capabilities/default.json");
        assert_main_window_loopback_capability(&private);
        for permission in private["permissions"].as_array().unwrap() {
            assert!(!general.contains(permission.as_str().unwrap()));
        }
    }

    #[test]
    fn agent_approval_configuration_main_window_only() {
        let private: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/voice-agent.json")).unwrap();
        let general = include_str!("../capabilities/default.json");
        assert_main_window_loopback_capability(&private);
        for permission in private["permissions"].as_array().unwrap() {
            assert!(!general.contains(permission.as_str().unwrap()));
        }
    }

    #[test]
    fn vision_credentials_private_capability() {
        let private: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/voice-vision.json")).unwrap();
        let default = include_str!("../capabilities/default.json");
        assert_main_window_loopback_capability(&private);
        for value in private["permissions"].as_array().unwrap() {
            assert!(!default.contains(value.as_str().unwrap()));
        }
    }

    /// 主窗口导航到 `http://127.0.0.1:<端口>` 后属于远程来源：主窗口专用 capability 必须声明
    /// 回环 remote，否则 Tauri 只对本地打包页授权、主窗口调用一律被 ACL 拒绝（2026-10-13 审查）。
    /// 端口无法写进静态 capability，由命令内 `require_main_window` 固定为本进程后端地址。
    fn assert_main_window_loopback_capability(capability: &serde_json::Value) {
        assert_eq!(capability["windows"], serde_json::json!(["main"]));
        assert_eq!(
            capability["remote"]["urls"],
            serde_json::json!(["http://127.0.0.1:*"])
        );
        assert!(capability.get("webviews").is_none());
    }

    /// 改写设置、启停进程、保存密钥与读取私有配置的命令必须在函数开头固定主窗口来源。
    #[test]
    fn main_window_commands_pin_backend_origin() {
        let sources = [
            (
                include_str!("voice.rs"),
                &[
                    "voice_save_settings",
                    "voice_set_mode",
                    "voice_server_start",
                    "voice_server_stop",
                    "voice_server_restart",
                    "voice_server_log",
                    "voice_models_test",
                    "voice_models_save",
                ][..],
            ),
            (
                include_str!("voice_builtin.rs"),
                &[
                    "voice_builtin_start",
                    "voice_builtin_stop",
                    "voice_colab_export_notebook",
                    "voice_device_set_status",
                    "voice_device_rename",
                    "voice_device_request",
                    "voice_builtin_models_test",
                    "voice_builtin_models_list",
                    "voice_builtin_models_save",
                    "voice_local_save",
                    "voice_local_model_download",
                    "voice_local_model_cancel",
                    "voice_local_model_delete",
                    "voice_local_laya_setup",
                    "voice_agent_status",
                    "voice_agent_save",
                    "voice_agent_stop",
                    "voice_music_pick_directory",
                    "voice_music_status",
                    "voice_music_save",
                    "voice_vision_status",
                    "voice_vision_save",
                    "voice_vision_test",
                ][..],
            ),
        ];
        for (source, commands) in sources {
            for command in commands {
                let start = source
                    .find(&format!("pub async fn {command}("))
                    .expect(command);
                let body = &source[start
                    ..start
                        + source[start..]
                            .find(
                                "
}
",
                            )
                            .expect(command)];
                let guard = body
                    .find("require_main_window(&window)?;")
                    .unwrap_or_else(|| panic!("{command} 未校验主窗口来源"));
                assert!(
                    guard
                        < body
                            .find("imp::run")
                            .or(body.find("spawn_blocking"))
                            .unwrap(),
                    "{command} 须先校验来源"
                );
            }
        }
        // 2026-11-05：状态读取含设备 IP 与调试原文，只给主窗口（n67 起独立安全窗口已取消）
        let builtin = include_str!("voice_builtin.rs");
        let start = builtin
            .find("pub async fn voice_builtin_status(")
            .expect("voice_builtin_status");
        let body = &builtin[start
            ..start
                + builtin[start..]
                    .find("\n}\n")
                    .expect("voice_builtin_status")];
        let guard = body
            .find("require_status_window(&window)?;")
            .expect("voice_builtin_status 未校验窗口来源");
        assert!(
            guard < body.find("imp::run").unwrap(),
            "voice_builtin_status 须先校验来源"
        );
        // n68：诊断包含事件与审批结果，与状态读取同权限、先校验来源再生成
        let start = builtin
            .find("pub async fn voice_diagnostics_export(")
            .expect("voice_diagnostics_export");
        let body = &builtin[start
            ..start
                + builtin[start..]
                    .find("\n}\n")
                    .expect("voice_diagnostics_export")];
        let guard = body
            .find("require_status_window(&window)?;")
            .expect("voice_diagnostics_export 未校验窗口来源");
        assert!(
            guard < body.find("imp::run").unwrap(),
            "voice_diagnostics_export 须先校验来源"
        );
        assert!(
            body.find("imp::run").unwrap() < body.find("blocking_save_file").unwrap(),
            "保存路径只来自系统对话框"
        );
        let helper = builtin
            .find("fn require_status_window(")
            .expect("require_status_window");
        let helper = &builtin[helper
            ..helper
                + builtin[helper..]
                    .find("\n}\n")
                    .expect("require_status_window")];
        assert!(helper.contains("require_private_settings_window(window)"));
        let session = include_str!("lib.rs");
        let start = session.find("fn get_desktop_session(").unwrap();
        assert!(session[start..start + 800].contains("window_is_trusted_main(&window)"));
        let updater = include_str!("app_update.rs");
        for command in crate::app_update::COMMANDS {
            let start = updater
                .find(&format!("pub async fn {command}("))
                .expect(command);
            let body = &updater[start..start + updater[start..].find("\n}\n").expect(command)];
            let open_brace = body.find("{\n").expect(command);
            assert!(
                body[open_brace..]
                    .trim_start_matches("{\n")
                    .trim_start()
                    .starts_with("require_main(&window)?;"),
                "{command} 必须在函数首行固定主窗口来源"
            );
        }
        let guard = updater.find("fn require_main(").expect("require_main");
        let guard_body =
            &updater[guard..guard + updater[guard..].find("\n}\n").expect("require_main")];
        assert!(guard_body.contains("crate::window_is_trusted_main(window)"));
        assert!(guard_body.contains("is_trusted_mobile_main(window.label(), &url)"));
    }

    #[cfg(desktop)]
    #[test]
    fn trusted_main_requires_main_label_and_exact_backend_origin() {
        let expected = "http://127.0.0.1:19666";
        assert!(super::is_trusted_main(
            "main",
            &"http://127.0.0.1:19666/voice".parse().unwrap(),
            expected
        ));
        for (label, url) in [
            ("voice-private", "http://127.0.0.1:19666/"),
            ("main", "http://127.0.0.1:8003/"),
            ("main", "http://localhost:19666/"),
            ("main", "http://tauri.localhost/"),
            ("main", "https://127.0.0.1:19666/"),
            ("main", "http://u@127.0.0.1:19666/"),
        ] {
            assert!(
                !super::is_trusted_main(label, &url.parse().unwrap(), expected),
                "{label} {url}"
            );
        }
    }

    #[cfg(desktop)]
    #[test]
    fn dev_ui_origin_accepts_only_loopback_vite_ports_and_is_scoped_to_main() {
        assert_eq!(
            super::parse_dev_ui_origin(Some("http://127.0.0.1:5177/")),
            Some("http://127.0.0.1:5177".to_owned())
        );
        assert_eq!(
            super::parse_dev_ui_origin(Some(" http://127.0.0.1:5173 ")),
            Some("http://127.0.0.1:5173".to_owned())
        );
        for rejected in [
            None,
            Some(""),
            Some("http://localhost:5177"),
            Some("http://127.0.0.1:5178"),
            Some("http://127.0.0.1:8080"),
            Some("https://127.0.0.1:5177"),
            Some("http://evil.example:5177"),
        ] {
            assert_eq!(super::parse_dev_ui_origin(rejected), None, "{rejected:?}");
        }

        let expected = "http://127.0.0.1:19666";
        let vite_url: tauri::Url = "http://127.0.0.1:5177/voice".parse().unwrap();
        assert!(!super::is_trusted_main_with_dev(
            "main", &vite_url, expected, None
        ));
        assert!(!super::is_trusted_main_with_dev(
            "voice-private",
            &vite_url,
            expected,
            Some("http://127.0.0.1:5177"),
        ));
        assert!(!super::is_trusted_main_with_dev(
            "main",
            &"http://127.0.0.1:8080/voice".parse().unwrap(),
            expected,
            Some("http://127.0.0.1:8080"),
        ));
        #[cfg(debug_assertions)]
        assert!(super::is_trusted_main_with_dev(
            "main",
            &vite_url,
            expected,
            Some("http://127.0.0.1:5177"),
        ));
        #[cfg(not(debug_assertions))]
        assert!(!super::is_trusted_main_with_dev(
            "main",
            &vite_url,
            expected,
            Some("http://127.0.0.1:5177"),
        ));
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
    /// Android 版“连接电脑”要访问局域网里任意 IP 的 MOSS（http://<电脑>:9666），只在 Android 覆盖配置里放开 connect-src；
    /// 桌面版保持只连本机。两份 CSP 除了这一处必须一致（2026-10-14）。
    #[test]
    fn android_csp_only_adds_lan_connect_sources() {
        let csp = |text: &str| -> String {
            let value: serde_json::Value = serde_json::from_str(text).unwrap();
            value["app"]["security"]["csp"].as_str().unwrap().to_owned()
        };
        let desktop = csp(include_str!("../tauri.conf.json"));
        let android = csp(include_str!("../tauri.android.conf.json"));
        assert!(
            !desktop.split([' ', ';']).any(|token| token == "http:"),
            "桌面版不能放开任意 http 来源: {desktop}"
        );
        assert_eq!(
            android,
            format!("{desktop} http: ws:"),
            "Android 只在 connect-src 末尾追加 http: ws:"
        );
        assert!(
            desktop
                .trim_end()
                .split(';')
                .next_back()
                .unwrap()
                .trim_start()
                .starts_with("connect-src"),
            "connect-src 必须是最后一条指令: {desktop}"
        );
    }
}
