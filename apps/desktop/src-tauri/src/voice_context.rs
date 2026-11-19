//! NV19-08 desktop wiring for long-term context (内置语音技术书 §15, 实施细则 §11).
//!
//! Two commands, both only from the trusted MOSS main window (Tauri ACL in
//! `capabilities/voice-private-main.json` plus `require_private_settings_window`; the separate
//! packaged private window was removed on 2026-11-07, n67):
//! `voice_context_manage` dispatches on `action`; `voice_context_pick_folder` opens the folder
//! dialog and indexes the chosen folder for the current project (the path never returns to JS).
//! Retrieval and extraction are off until the user switches them on.
//! 零 UI 编排主体位于 `crates/moss-voice-host/src/context.rs`。

use serde_json::Value;

type CmdResult = Result<Value, Value>;

#[cfg(desktop)]
use crate::voice_builtin::require_private_settings_window;

#[tauri::command]
pub async fn voice_context_manage(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = crate::voice_builtin::context_manage(app, input).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, input));
    result
}

/// Opens only after an explicit click in the private window; indexes the chosen folder for the
/// current project. The absolute path stays on the native side.
#[tauri::command]
pub async fn voice_context_pick_folder(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = {
        let picker = app.clone();
        let picked = tauri::async_runtime::spawn_blocking(move || {
            use tauri_plugin_dialog::DialogExt;
            picker
                .dialog()
                .file()
                .set_title("选择资料文件夹（只读取文本文件）")
                .blocking_pick_folder()
                .and_then(|p| p.into_path().ok())
        })
        .await
        .map_err(
            |_| serde_json::json!({"code": "voice_internal", "message": "选择资料文件夹失败"}),
        )?;
        match picked {
            None => Ok(Value::Null),
            Some(path) => crate::voice_builtin::context_add_folder(app, path).await,
        }
    };
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

#[cfg(mobile)]
fn desktop_only<T>(_: T) -> CmdResult {
    Err(
        serde_json::json!({"code": "voice_desktop_only", "message": "长期记忆管理仅在 MOSS 桌面端可用"}),
    )
}

/// Commands the private page calls (build.rs registration and capability tests check this).
#[cfg(test)]
pub const COMMANDS: [&str; 2] = ["voice_context_manage", "voice_context_pick_folder"];
