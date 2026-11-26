//! 内置语音服务的 Tauri 命令（内置语音技术书 NV1 起）：启动 / 停止、状态、设备允许与撤销、模型配置（NV5）。
//!
//! 与 `voice.rs` 一样以 Tauri 命令提供，只有 MOSS 桌面窗口能调用；Android 不编译服务本身，命令直接返回错误。
//! 设备登记存 SQLite `voice_devices`（schema v6），设备 token 的签名密钥与模型密钥存 `app_secrets`
//! （`voice.device_token_secret`、`voice.llm.main` 等，任何命令都不返回原文）。
//!
//! 识别、大模型、合成三项都可用时以对话模式启动，否则以回声模式启动；对话中调用 MOSS 工具时身份为
//! `CallerContext::Voice`（运动与出光跟随严格模式开关）。语音链路为“内置服务”时随 MOSS 启动。
//!
//! 本地模型（NV6）：识别（SenseVoice）、合成（Kokoro）、判停（Silero）可改用本机模型，由与 MOSS 同目录的
//! 辅助程序 `moss-voice-local`（GPL-3.0）运行；模型按需下载到运行目录 `models/` 并校验 SHA-256。
//!
//! 豆包流式（NV7）：识别与合成可选豆包。识别在用户开始说话时就建立连接、边说边传；合成每句的第一段音频
//! 一到就播放（播放启动缓冲相应从 600 ms 降到 300 ms）。

use serde_json::Value;

type CmdResult = Result<Value, Value>;

/// 敏感语音设置窗口：只接受经 [`crate::window_is_trusted_main`] 确认的 MOSS 主窗口。
///
/// 2026-10-18 用户决定（技术书 n26）：记忆、信息服务、声纹、家居四项在主语音页就地展开；
/// 2026-11-07 用户决定（n67）：取消独立的本机安全设置窗口，唤醒词与固件升级也放进语音中心，
/// 推送前逐台确认不变。Tauri ACL（`voice-private-main.json`）是第一层，这里固定来源为本进程后端。
#[cfg(desktop)]
pub(crate) fn require_private_settings_window(window: &tauri::WebviewWindow) -> Result<(), Value> {
    if crate::window_is_trusted_main(window) {
        return Ok(());
    }
    Err(serde_json::json!({
        "code": "voice_acl_denied",
        "message": "这项语音设置只能在 MOSS 主窗口里修改"
    }))
}

/// 主窗口专用命令：只接受后端地址上的 `main` 窗口（见 [`crate::is_trusted_main`]）。
#[cfg(desktop)]
fn require_main_window(window: &tauri::WebviewWindow) -> Result<(), Value> {
    if crate::window_is_trusted_main(window) {
        return Ok(());
    }
    Err(serde_json::json!({
        "code": "voice_acl_denied",
        "message": "只能在 MOSS 主窗口修改内置语音设置"
    }))
}

/// 读取内置语音状态的窗口：经 [`crate::window_is_trusted_main`] 确认的主窗口。
/// 判定与 [`require_private_settings_window`] 相同，只换成“查看状态”的提示。
#[cfg(desktop)]
fn require_status_window(window: &tauri::WebviewWindow) -> Result<(), Value> {
    require_private_settings_window(window).map_err(|_| {
        serde_json::json!({
            "code": "voice_acl_denied",
            "message": "内置语音状态只能在 MOSS 主窗口查看"
        })
    })
}

/// NV19-08: entry points for `voice_context.rs` commands (the backend type stays private).
#[cfg(desktop)]
pub(crate) async fn context_manage(app: tauri::AppHandle, input: Value) -> CmdResult {
    imp::run(app, move |voice| voice.context_manage(&input)).await
}

#[cfg(desktop)]
pub(crate) async fn context_add_folder(
    app: tauri::AppHandle,
    path: std::path::PathBuf,
) -> CmdResult {
    imp::run(app, move |voice| voice.context_add_folder(&path)).await
}

#[cfg(mobile)]
fn desktop_only<T>(_: T) -> CmdResult {
    Err(
        serde_json::json!({"code": "voice_desktop_only", "message": "内置语音服务仅在 MOSS 桌面端可用"}),
    )
}

#[cfg(desktop)]
mod imp {
    use std::sync::Arc;

    use moss_voice::CenterError;
    use serde_json::Value;
    use tauri::Manager;

    use super::CmdResult;
    pub use moss_voice_host::{
        BuiltinState, BuiltinVoice, autostart, init, parse_sections, record_moss_event,
    };

    fn builtin(app: &tauri::AppHandle) -> Result<Arc<BuiltinVoice>, Value> {
        app.try_state::<BuiltinState>()
            .and_then(|state| state.0.clone())
            .ok_or_else(|| serde_json::json!({"code": "voice_unavailable", "message": "内置语音服务不可用：本机后端未启动"}))
    }

    /// 在阻塞线程池中执行（启动、停止会创建或关闭运行时，不能放在异步线程里）。
    pub async fn run<F>(app: tauri::AppHandle, task: F) -> CmdResult
    where
        F: FnOnce(&BuiltinVoice) -> Result<Value, CenterError> + Send + 'static,
    {
        let builtin = builtin(&app)?;
        tauri::async_runtime::spawn_blocking(move || {
            task(&builtin).map_err(|error| serde_json::to_value(error).unwrap_or_default())
        })
        .await
        .map_err(|_| serde_json::json!({"code": "voice_internal", "message": "内置语音服务任务异常结束"}))?
    }

    /// MOSS 退出时停止内置语音服务、Colab CLI 后台进程及本地 Jev（OneJev / Laya）子进程（在急停之后调用）。
    pub fn shutdown(app: &tauri::AppHandle) {
        if let Ok(builtin) = builtin(app) {
            builtin.shutdown();
        }
    }
}

#[cfg(desktop)]
pub use imp::{BuiltinVoice, autostart, init, record_moss_event, shutdown};

/// 内置语音状态含设备 IP、会话事件，调试开启时还有原始识别文字与工具参数。
/// 2026-11-05（n67 起独立安全窗口已取消）：只给经 `require_status_window` 确认的 MOSS 主窗口，
/// 其他窗口即使拿到 `default` capability（`windows:["*"]`）也读不到。
#[tauri::command]
pub async fn voice_builtin_status(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    lite: Option<bool>,
) -> CmdResult {
    #[cfg(desktop)]
    require_status_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| {
        Ok(builtin.status_with_options(lite.unwrap_or(false)))
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, lite));
    result
}

#[tauri::command]
pub async fn voice_builtin_start(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |builtin| builtin.start()).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

#[tauri::command]
pub async fn voice_builtin_stop(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |builtin| Ok(builtin.stop())).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

/// 导出诊断包（2026-11-07 技术书 n68）：后端生成并脱敏 JSON，弹出系统“另存为”对话框写入用户选的位置；
/// 路径与内容都不来自网页。与状态读取同权限：只给经 `require_status_window` 确认的 MOSS 主窗口。
/// 用户取消时返回 `{saved: false}`；成功返回文件名（不回传完整路径）、字节数与事件条数。
#[tauri::command]
pub async fn voice_diagnostics_export(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_status_window(&window)?;
    #[cfg(desktop)]
    let result = {
        let bundle = imp::run(app.clone(), |builtin| Ok(builtin.diagnostics_bundle())).await?;
        let events = bundle["events"].as_array().map(Vec::len).unwrap_or(0);
        let text = serde_json::to_string_pretty(&bundle).map_err(|_| {
            serde_json::json!({"code": "diagnostics_export_failed", "message": "诊断包生成失败（内容无法转成 JSON）"})
        })?;
        let default_name = format!(
            "moss-voice-diagnostics-{}.json",
            bundle["generated_at_ms"].as_u64().unwrap_or(0) / 1000
        );
        let picker = app.clone();
        let name = default_name.clone();
        let picked = tauri::async_runtime::spawn_blocking(move || {
            use tauri_plugin_dialog::DialogExt;
            picker
                .dialog()
                .file()
                .set_title("保存语音诊断包")
                .set_file_name(&name)
                .add_filter("JSON", &["json"])
                .blocking_save_file()
                .and_then(|path| path.into_path().ok())
        })
        .await
        .map_err(|_| serde_json::json!({"code": "diagnostics_export_failed", "message": "打开保存对话框失败"}))?;
        match picked {
            None => Ok(serde_json::json!({"saved": false})),
            Some(path) => match std::fs::write(&path, text.as_bytes()) {
                Ok(()) => Ok(serde_json::json!({
                    "saved": true,
                    "file_name": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(default_name),
                    "bytes": text.len(),
                    "events": events,
                })),
                Err(error) => Err(serde_json::json!({
                    "code": "diagnostics_export_failed",
                    "message": "诊断包没能写入所选位置：可能没有写入权限或磁盘已满，换个文件夹再试",
                    "detail": error.kind().to_string(),
                })),
            },
        }
    };
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

/// n80：导出 Colab T4 专属联动笔记本（`.ipynb`）。后端生成固定审计过的 JSON，弹出系统“另存为”对话框写入；
/// 路径与内容都不来自网页，且不含任何本机路径或密钥。
#[tauri::command]
pub async fn voice_colab_export_notebook(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = {
        let text = moss_voice::colab::notebook_json();
        let default_name = moss_voice::colab::COLAB_NOTEBOOK_FILENAME.to_owned();
        let picker = app.clone();
        let name = default_name.clone();
        let picked = tauri::async_runtime::spawn_blocking(move || {
            use tauri_plugin_dialog::DialogExt;
            picker
                .dialog()
                .file()
                .set_title("保存 Colab T4 笔记本")
                .set_file_name(&name)
                .add_filter("Jupyter Notebook", &["ipynb"])
                .blocking_save_file()
                .and_then(|path| path.into_path().ok())
        })
        .await
        .map_err(
            |_| serde_json::json!({"code": "colab_export_failed", "message": "打开保存对话框失败"}),
        )?;
        match picked {
            None => Ok(serde_json::json!({"saved": false})),
            Some(path) => match std::fs::write(&path, text.as_bytes()) {
                Ok(()) => Ok(serde_json::json!({
                    "saved": true,
                    "file_name": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(default_name),
                    "bytes": text.len(),
                })),
                Err(error) => Err(serde_json::json!({
                    "code": "colab_export_failed",
                    "message": "笔记本没能写入所选位置：可能没有写入权限或磁盘已满，换个文件夹再试",
                    "detail": error.kind().to_string(),
                })),
            },
        }
    };
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

/// 允许（`allowed`）或撤销（`revoked`）设备。允许后设备需重启以重新获取 token。
#[tauri::command]
pub async fn voice_device_set_status(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    device_id: String,
    status: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| {
        builtin.set_device_status(&device_id, &status)
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, device_id, status));
    result
}

#[tauri::command]
pub async fn voice_device_request(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    device_id: String,
    request: String,
    value: Option<Value>,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| {
        builtin.device_request(&device_id, &request, value.as_ref())
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, device_id, request, value));
    result
}

#[tauri::command]
pub async fn voice_device_rename(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    device_id: String,
    name: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.rename_device(&device_id, &name)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, device_id, name));
    result
}

/// n134：移除已允许或待允许的小智设备（断开在线会话、作废待消费审批、删除设备记录与快捷偏好；已撤销设备拒绝移除）。
#[tauri::command]
pub async fn voice_device_remove(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    device_id: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.remove_device(&device_id)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, device_id));
    result
}

/// 测试内置服务的一个模型分区（`llm` / `asr` / `tts` / `jev` / `colab`）；密钥留空时用已保存的密钥。
#[tauri::command]
pub async fn voice_builtin_models_test(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    section: String,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| {
        if section.trim() == "jev" {
            return builtin.jev_test(&input);
        }
        if section.trim() == "colab" {
            return builtin.colab_test(&input);
        }
        if section.trim() == "chain" {
            return builtin.chain_test(&input);
        }
        if section.trim() == "local_tts" {
            return builtin.local_tts_preview(&input);
        }
        if section.trim() == "tts_clone" {
            return builtin.tts_voice_clone(&input);
        }
        if section.trim() == "local_tts_clone" {
            return builtin.local_tts_clone_manage(&input);
        }
        let section = imp::parse_sections(std::slice::from_ref(&section))?[0];
        let input: moss_voice::SectionInput = serde_json::from_value(input).map_err(|_| {
            moss_voice::CenterError::new("model_input_invalid", 400, "模型配置格式不正确")
        })?;
        builtin.models_test(section, &input)
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, section, input));
    result
}

/// 读取内置服务一个模型分区（`llm` / `asr` / `tts` / `jev`）支持的模型列表；密钥留空时用已保存的密钥。
#[tauri::command]
pub async fn voice_builtin_models_list(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    section: String,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| {
        if section.trim() == "jev" {
            return builtin.jev_models_list(&input);
        }
        if section.trim() == "local_tts" {
            return builtin.local_tts_voices_list(&input);
        }
        if section.trim() == "tts_voices" {
            return builtin.tts_cloned_voices_list(&input);
        }
        let section = imp::parse_sections(std::slice::from_ref(&section))?[0];
        let input: moss_voice::SectionInput = serde_json::from_value(input).map_err(|_| {
            moss_voice::CenterError::new("model_input_invalid", 400, "模型配置格式不正确")
        })?;
        builtin.models_list(section, &input)
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, section, input));
    result
}

/// 保存内置服务的模型设置。`clear` 中的分区删除设置与密钥。
#[tauri::command]
pub async fn voice_builtin_models_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
    clear: Option<Vec<String>>,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| {
        let clear_list = clear.unwrap_or_default();
        let clear_jev = clear_list.iter().any(|s| s.trim() == "jev");
        let jev_input = input.get("jev");
        if clear_jev && clear_list.len() == 1 && input.as_object().is_none_or(|o| o.is_empty()) {
            return builtin.jev_save(None, true);
        }
        if !clear_jev
            && clear_list.is_empty()
            && jev_input.is_some()
            && input.as_object().is_some_and(|o| o.len() == 1)
        {
            return builtin.jev_save(jev_input, false);
        }
        let non_jev_clear: Vec<String> = clear_list
            .into_iter()
            .filter(|s| s.trim() != "jev")
            .collect();
        let mut non_jev_input = input.clone();
        if let Some(obj) = non_jev_input.as_object_mut() {
            obj.remove("jev");
        }
        let parsed_input: moss_voice::ModelsInput =
            serde_json::from_value(non_jev_input).map_err(|_| {
                moss_voice::CenterError::new("model_input_invalid", 400, "模型配置格式不正确")
            })?;
        let parsed_clear = imp::parse_sections(&non_jev_clear)?;
        if clear_jev || jev_input.is_some() {
            builtin.precheck_jev_save_input(jev_input, clear_jev, &parsed_input, &parsed_clear)?;
        }
        let mut res = builtin.models_save(&parsed_input, &parsed_clear)?;
        if clear_jev || jev_input.is_some() {
            res = builtin.jev_save(jev_input, clear_jev)?;
        }
        Ok(res)
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, input, clear));
    result
}

/// 保存本地模型设置（`asr` / `tts` / `vad` 开关、`tts_speaker`、`tts_speed`、`mirror`）。
#[tauri::command]
pub async fn voice_local_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    settings: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.local_save(&settings)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, settings));
    result
}

/// 开始下载本地模型（id 见 `local_models::CATALOG`）；进度在状态的 `local.models` 中。
#[tauri::command]
pub async fn voice_local_model_download(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    id: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.model_download(&id)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, id));
    result
}

#[tauri::command]
pub async fn voice_local_model_cancel(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    id: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| Ok(builtin.model_cancel(&id))).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, id));
    result
}

#[tauri::command]
pub async fn voice_local_model_delete(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    id: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.model_delete(&id)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, id));
    result
}

/// 自动准备 Laya 的 Python 运行环境（2026-11-07）：`action` 为 `start` / `cancel`。
#[tauri::command]
pub async fn voice_local_laya_setup(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    action: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.laya_setup_action(&action)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, action));
    result
}

#[tauri::command]
pub async fn voice_agent_status(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| Ok(voice.agent_status())).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}
#[tauri::command]
pub async fn voice_agent_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.agent_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, input));
    result
}
/// 停止正在运行的 Agent 任务（进程树随 Job Object 结束）。
#[tauri::command]
pub async fn voice_agent_stop(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    run_id: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| Ok(voice.agent_stop(&run_id))).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, run_id));
    result
}

#[tauri::command]
pub async fn voice_music_pick_directory(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        // Opens only after an explicit click in the desktop main window.
        // The user-selected path is returned to the form, not saved or scanned.
        let Some(picked) = app
            .dialog()
            .file()
            .set_title("选择本地音乐文件夹")
            .blocking_pick_folder()
        else {
            return Ok(Value::Null);
        };
        let path = picked.into_path().map_err(|_| {
            serde_json::json!({
                "code":"music_invalid","message":"无法识别选择的音乐目录"
            })
        })?;
        let name = path.to_str().ok_or_else(|| {
            serde_json::json!({
                "code":"music_invalid","message":"音乐目录包含不支持的字符"
            })
        })?;
        if !path.is_absolute() || name.len() > 1024 || name.chars().any(char::is_control) {
            return Err(serde_json::json!({
                "code":"music_invalid","message":"所选路径无效或过长"
            }));
        }
        Ok(serde_json::json!({"directory":name}))
    })
    .await
    .map_err(|_| {
        serde_json::json!({
            "code":"voice_internal","message":"选择音乐目录失败"
        })
    })?;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

#[tauri::command]
pub async fn voice_music_status(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| Ok(voice.music_status())).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}
#[tauri::command]
pub async fn voice_music_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.music_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, input));
    result
}

#[tauri::command]
pub async fn voice_home_status(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| Ok(voice.home_status())).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
#[tauri::command]
pub async fn voice_home_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}

#[tauri::command]
pub async fn voice_home_backup_export(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.home_backup_export()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
/// Explicit local-file picker; no path or bytes are supplied by web content.
#[tauri::command]
pub async fn voice_home_backup_audit(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result=tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        let Some(picked)=app.dialog().file().set_title("选择历史备份文件（仅审计，不恢复）").blocking_pick_file() else {
            return Ok(Value::Null);
        };
        let path=picked.into_path().map_err(|_|serde_json::json!({"code":"home_audit_invalid","message":"无法识别所选文件"}))?;
        let audit=moss_voice_host::home_backup_audit::audit_selected_file(&path)
            .map_err(|_|serde_json::json!({"code":"home_audit_invalid","message":"文件不符合只读安全审计条件（路径、类型或大小）"}))?;
        let category=match audit.risk {
            moss_voice_host::home_backup_audit::BackupRisk::SuspectedLegacyCredential=>"suspected_legacy_credential",
            moss_voice_host::home_backup_audit::BackupRisk::EncryptedCredentialMaterial=>"encrypted_credential_material",
            moss_voice_host::home_backup_audit::BackupRisk::UnknownTreatAsSensitive=>"unknown_treat_as_sensitive",
        };
        Ok(serde_json::json!({"risk":category,"inspected_bytes":audit.inspected_bytes,"safe_to_share":false}))
    }).await.map_err(|_|serde_json::json!({"code":"home_audit_failed","message":"历史文件审计未完成"}))?;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}

#[tauri::command]
pub async fn voice_home_backup_restore(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_backup_restore(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_home_token_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_token_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_home_voice_consent(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_voice_consent(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_home_discover(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.home_discover()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
#[tauri::command]
pub async fn voice_home_approvals(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.home_approvals()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
#[tauri::command]
pub async fn voice_home_approval_cancel(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_approval_cancel(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_home_control_consent(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_control_consent(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_speaker_manage(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.speaker_manage(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_firmware_status(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.firmware_status()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
/// 弹出文件选择框导入固件；路径和内容都不来自网页。
#[tauri::command]
pub async fn voice_firmware_import(
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
                .set_title("选择 ESP32 应用固件（.bin）")
                .add_filter("固件", &["bin"])
                .blocking_pick_file()
                .and_then(|p| p.into_path().ok())
        })
        .await
        .map_err(|_| serde_json::json!({"code":"firmware_invalid","message":"选择固件失败"}))?;
        match picked {
            None => Ok(Value::Null),
            Some(path) => imp::run(app, move |voice| voice.firmware_import(&path)).await,
        }
    };
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
#[tauri::command]
pub async fn voice_firmware_manage(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.firmware_manage(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
/// n61 设备唤醒词（n67 起与固件一样在主窗口语音中心操作，来源固定为本进程后端上的主窗口）。
#[tauri::command]
pub async fn voice_wakeword_status(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.wakeword_status()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
/// 弹出文件选择框导入基础资源包；路径和内容都不来自网页。
#[tauri::command]
pub async fn voice_wakeword_import_base(
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
                .set_title("选择小智资源包（assets.bin）")
                .add_filter("资源包", &["bin"])
                .blocking_pick_file()
                .and_then(|p| p.into_path().ok())
        })
        .await
        .map_err(|_| serde_json::json!({"code":"wakeword_invalid","message":"选择资源包失败"}))?;
        match picked {
            None => Ok(Value::Null),
            Some(path) => imp::run(app, move |voice| voice.wakeword_import_base(&path)).await,
        }
    };
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
#[tauri::command]
pub async fn voice_wakeword_manage(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.wakeword_manage(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_usb_provision(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    // macOS 要有定位授权才给出 Wi-Fi 名称：授权框须由主线程上的 CLLocationManager 弹出
    #[cfg(target_os = "macos")]
    if matches!(
        input.get("action").and_then(Value::as_str),
        Some("wifi_current" | "wifi_scan")
    ) {
        let _ = app.run_on_main_thread(moss_voice_host::prepare_wifi_location_permission);
    }
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.usb_provision(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_home_approval_approve(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_approval_approve(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_vision_status(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| Ok(voice.vision_status())).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}
#[tauri::command]
pub async fn voice_vision_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.vision_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, input));
    result
}
#[tauri::command]
pub async fn voice_vision_test(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.vision_test()).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

#[tauri::command]
pub async fn voice_plugins_status(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| Ok(voice.plugins_status())).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}

#[tauri::command]
pub async fn voice_plugins_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.plugins_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}

/// 前端调用的内置语音命令（build.rs 登记与 capability 授权的测试会核对这张表）。
#[cfg(test)]
pub const COMMANDS: [&str; 45] = [
    "voice_firmware_status",
    "voice_firmware_import",
    "voice_firmware_manage",
    "voice_speaker_manage",
    "voice_music_pick_directory",
    "voice_music_status",
    "voice_music_save",
    "voice_home_status",
    "voice_home_save",
    "voice_home_backup_export",
    "voice_home_backup_audit",
    "voice_home_backup_restore",
    "voice_home_token_save",
    "voice_home_voice_consent",
    "voice_home_approvals",
    "voice_home_approval_cancel",
    "voice_home_control_consent",
    "voice_home_approval_approve",
    "voice_home_discover",
    "voice_agent_status",
    "voice_agent_save",
    "voice_agent_stop",
    "voice_vision_status",
    "voice_vision_save",
    "voice_vision_test",
    "voice_plugins_status",
    "voice_plugins_save",
    "voice_builtin_status",
    "voice_builtin_start",
    "voice_builtin_stop",
    "voice_diagnostics_export",
    "voice_colab_export_notebook",
    "voice_device_set_status",
    "voice_device_rename",
    "voice_device_remove",
    "voice_device_request",
    "voice_builtin_models_test",
    "voice_builtin_models_list",
    "voice_builtin_models_save",
    "voice_local_save",
    "voice_local_model_download",
    "voice_local_model_cancel",
    "voice_local_model_delete",
    "voice_local_laya_setup",
    "voice_usb_provision",
];
