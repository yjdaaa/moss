use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[cfg(desktop)]
use tauri::{Emitter, Manager};

#[derive(Clone, Default)]
pub struct ShutdownState {
    pub requested: Arc<AtomicBool>,
}

impl ShutdownState {
    pub fn request(&self) -> bool {
        let _ = laser_host::global_shutdown_coordinator().trigger_shutdown();
        !self.requested.swap(true, Ordering::SeqCst)
    }
}

#[tauri::command]
fn request_shutdown(state: tauri::State<'_, ShutdownState>) -> bool {
    state.request()
}

#[tauri::command]
fn minimize_to_tray(window: tauri::WebviewWindow) {
    let _ = window.hide();
}

#[tauri::command]
fn exit_application(app: tauri::AppHandle, state: tauri::State<'_, ShutdownState>) {
    state.request();
    app.exit(0);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 路径 5: Panic Hook 同步脱机急停
    laser_host::install_panic_hook();
    // 路径 4: Windows 控制台关闭事件
    laser_host::install_console_ctrl_handler();

    // 在进程内启动并持有 laser-host 后端服务（规格 §16）
    let (_host_handle, _actual_port) = match laser_host::HostApplication::init() {
        Ok(host) => {
            let actual_port = match host.spawn_http_server() {
                Ok((_handle, port)) => port,
                Err(e) => {
                    eprintln!("警告: 后端服务监听启动失败: {e}");
                    9666
                }
            };
            // 小智 MCP 桥接（MCP_ENDPOINT 未配置时静默跳过）
            let _mcp_bridge = host.spawn_mcp_bridge();
            (Some(host), actual_port)
        }
        Err(e) => {
            eprintln!("警告: 后端服务进程内初始化提示: {e}");
            (None, 9666)
        }
    };

    let state = ShutdownState::default();
    let builder = tauri::Builder::default()
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![
            request_shutdown,
            minimize_to_tray,
            exit_application
        ]);

    #[cfg(desktop)]
    let builder = builder
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
            if let Some(window) = app.get_webview_window("main") {
                let url = format!("http://127.0.0.1:{_actual_port}/");
                if let Ok(parsed) = url.parse() {
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
                        let state = app.state::<ShutdownState>();
                        state.request();
                        app.exit(0);
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
    use super::ShutdownState;

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
