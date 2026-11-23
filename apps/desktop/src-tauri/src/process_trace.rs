//! 完整过程记录的保存位置只允许受信任桌面主窗口通过系统选择框修改。

use std::path::Path;

use laser_runtime::{ProcessTrace, process_trace::TraceDirectoryStatus};

/// 与后端、语音共享同一个记录器；后端未启动时为空。
pub struct ProcessTraceState(pub Option<ProcessTrace>);

fn recorder(state: &ProcessTraceState) -> Result<ProcessTrace, String> {
    if !cfg!(desktop) {
        return Err("当前平台不支持修改过程记录保存位置".into());
    }
    state
        .0
        .clone()
        .ok_or_else(|| "本机后端未启动，无法设置过程记录保存位置".into())
}

#[tauri::command]
pub async fn pick_process_trace_dir(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, ProcessTraceState>,
) -> Result<Option<TraceDirectoryStatus>, String> {
    crate::require_trusted_app_window(&window, "选择过程记录保存位置")?;
    let trace = recorder(&state)?;
    tauri::async_runtime::spawn_blocking(move || pick_directory(&app, &trace))
        .await
        .map_err(|error| format!("打开文件夹选择框失败：{error}"))?
}

#[cfg(desktop)]
fn pick_directory(
    app: &tauri::AppHandle,
    trace: &ProcessTrace,
) -> Result<Option<TraceDirectoryStatus>, String> {
    use tauri_plugin_dialog::DialogExt;
    let current = trace.dir();
    let mut dialog = app.dialog().file().set_title("选择过程记录保存文件夹");
    if current.is_dir() {
        dialog = dialog.set_directory(&current);
    }
    let Some(picked) = dialog.blocking_pick_folder() else {
        return Ok(None);
    };
    let path = picked
        .into_path()
        .map_err(|error| format!("无法识别所选文件夹：{error}"))?;
    trace.set_directory(Some(&path))?;
    Ok(Some(trace.directory_status()))
}

#[cfg(not(desktop))]
fn pick_directory(
    _app: &tauri::AppHandle,
    _trace: &ProcessTrace,
) -> Result<Option<TraceDirectoryStatus>, String> {
    Err("当前平台不支持选择过程记录保存位置".into())
}

#[tauri::command]
pub async fn reset_process_trace_dir(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, ProcessTraceState>,
) -> Result<TraceDirectoryStatus, String> {
    crate::require_trusted_app_window(&window, "恢复过程记录默认保存位置")?;
    let trace = recorder(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        trace.set_directory(None)?;
        Ok(trace.directory_status())
    })
    .await
    .map_err(|error| format!("恢复默认保存位置失败：{error}"))?
}

#[tauri::command]
pub async fn open_process_trace_dir(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, ProcessTraceState>,
) -> Result<(), String> {
    crate::require_trusted_app_window(&window, "打开过程记录文件夹")?;
    let trace = recorder(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        open_directory_with(&trace, |path| {
            crate::export::open_in_file_manager(path, false)
        })
    })
    .await
    .map_err(|error| format!("打开过程记录文件夹失败：{error}"))?
}

fn open_directory_with(
    trace: &ProcessTrace,
    open: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let status = trace.directory_status();
    if !status.available {
        return Err(status.error.unwrap_or_else(|| "所选目录当前不可用".into()));
    }
    if !status.dir.is_dir() {
        return Err("尚无过程记录，保存文件夹还未创建".into());
    }
    open(&status.dir)
}

#[cfg(all(test, desktop))]
mod tests {
    use super::*;

    #[test]
    fn open_uses_the_current_directory_without_creating_or_falling_back() {
        let runtime = tempfile::tempdir().unwrap();
        let trace = ProcessTrace::new(runtime.path());
        let error = open_directory_with(&trace, |_| panic!("未创建的目录不能打开")).unwrap_err();
        assert_eq!(error, "尚无过程记录，保存文件夹还未创建");
        assert!(!trace.dir().exists());

        let selected = tempfile::tempdir().unwrap();
        trace.set_directory(Some(selected.path())).unwrap();
        let current = trace.dir();
        open_directory_with(&trace, |path| {
            assert_eq!(path, current);
            Ok(())
        })
        .unwrap();

        std::fs::rename(&current, selected.path().join("unplugged")).unwrap();
        assert!(open_directory_with(&trace, |_| panic!("不可用时不能打开其他目录")).is_err());
        assert!(!current.exists(), "不得重新创建已失效的自定义目录");
        assert!(
            !runtime
                .path()
                .join(laser_runtime::process_trace::TRACE_DIR)
                .exists()
        );
    }
}
