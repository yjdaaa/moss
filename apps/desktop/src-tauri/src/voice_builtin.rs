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
    use std::sync::{Arc, Mutex};

    use moss_voice::builtin::{self, KeyChange};
    use moss_voice::local_models::{self, Engine, ModelKind, ModelManager};
    use moss_voice::settings::{BuiltinLocal, builtin_secret_name};
    use moss_voice::{CenterError, ModelsInput, ProviderKind, Section, SectionInput, VoiceMode};
    use moss_voice_providers::doubao::{DoubaoAsr, DoubaoTts};
    use moss_voice_providers::local::{
        AsrEngine, HELPER_FILE, LocalAsr, LocalHelper, LocalStreamingAsr, LocalTts, TtsEngine,
        TurnHelper, VadEngine, VadParams,
    };
    use moss_voice_providers::openai::{OpenAiAsr, OpenAiLlm, OpenAiTts};
    use moss_voice_providers::speech_cpp::{SpeechCppTts, SpeechCppWorker};
    use moss_voice_providers::{Asr, StreamingAsr, Tts};
    use moss_voice_server::{
        Conversation, ConversationSettings, DeviceRegistry, DeviceStatus, DeviceVisit,
        DiagCategory, EnergyVad, EventRecord, HostTools, JevFilterOptions, LocalVadFactory,
        OpenRouterDecisionClient, ServerConfig, ServerMode, ToolContext, ToolHost, ToolHostResult,
        TurnEndPredictor, Vad, VadConfig, VadEvent, VadFactory, VoiceJevFilter, VoiceServer,
    };
    use serde_json::Value;
    use tauri::Manager;

    use super::CmdResult;

    const TOKEN_SECRET_NAME: &str = "voice.device_token_secret";
    /// 流式合成与批量合成播放前攒的音频（3 帧预缓冲 + 2 帧服务端余量 = 300 ms，避免首轮连发后立即欠载）
    const STREAMING_PLAYBACK_START_MS: u32 = 300;

    use moss_voice_server::memory::{MemoryRecord, MemoryService, VoiceMemoryStore};
    use moss_voice_server::person_context::{
        CalibrationReport, CalibrationSample, IdentityRequest, IdentityResolver, PersonContext,
        PersonMemoryStore, PersonScope, PersonSummary, SpeakerTemplate, match_embedding,
    };

    use moss_voice_server::ToolExecutor;
    use moss_voice_server::home_tools::{HomeAvailability, HomeHost, HomeTools};
    use moss_voice_server::plugins::runtime::{CombinedTools, InfoTools, PluginHost};

    use moss_voice_server::firmware::sha256_hex;
    use moss_voice_server::persona::{
        CustomRole, PersonaConfig, PersonaStore, PersonaSwitch, PersonaTools,
    };

    mod chain_test;
    mod colab;
    mod colab_runner;
    mod conversation;
    mod devices;
    mod firmware;
    mod home;
    mod laya_jev;
    mod lifecycle;
    mod memory;
    mod models;
    mod music;
    mod paths;
    mod plugins;
    mod settings;
    mod speaker;
    mod status;
    mod tool_host;
    mod tts_preview;
    mod usb_provision;
    mod vision;
    mod wakeword;

    #[cfg(test)]
    mod device_switch_tests;
    #[cfg(test)]
    mod local_runtime_tests;
    #[cfg(test)]
    mod local_vision_tests;

    use self::colab::*;
    use self::conversation::*;
    use self::devices::*;
    use self::firmware::*;
    use self::home::*;
    use self::laya_jev::*;
    use self::lifecycle::*;
    pub use self::lifecycle::{autostart, init, record_moss_event};
    use self::memory::*;
    use self::music::*;
    use self::paths::*;
    use self::plugins::*;
    use self::settings::*;
    use self::speaker::*;
    use self::status::*;
    use self::tool_host::*;
    use self::tts_preview::*;
    use self::vision::*;
    use self::wakeword::*;

    pub struct BuiltinVoice {
        #[cfg_attr(test, allow(dead_code))]
        self_weak: std::sync::Weak<BuiltinVoice>,
        host: Arc<laser_host::HostApplication>,
        /// 串行化启停与重启编排，避免加载本地模型/扫描音乐时长时间阻塞 `self.server` 状态轮询。
        lifecycle: Mutex<()>,
        server: Arc<Mutex<Option<VoiceServer>>>,
        /// 仅内存、重启即清的实时对话调试流总开关（默认 false，不落盘）
        debug_trace_enabled: std::sync::atomic::AtomicBool,
        memory: Arc<MemoryService>,
        speaker: Arc<moss_voice_server::speaker_enrollment::SpeakerEnrollment>,
        speaker_backend: Arc<DesktopSpeakerBackend>,
        personal: Arc<PersonContext>,
        identity: Arc<DesktopIdentity>,
        home_control: Arc<laser_host::home_control::HomeControlCoordinator>,
        /// 最近一次自动启动失败的原因（语音链路为内置服务时随 MOSS 启动）
        last_error: Mutex<Option<String>>,
        /// 诊断记录本（2026-11-07 n68）：服务事件、对话耗时与桌面端后台失败（本地模型、Laya、固件记录等），
        /// 传给每次启动的语音服务，停止、重启、崩溃重启都不清空；导出诊断包时整份写出。
        diagnostics: Arc<moss_voice_server::DiagnosticsLog>,
        /// 本地音乐目录换机或拔盘不可用时的降级提示（不阻断内置语音主服务启动）
        music_warning: Mutex<Option<String>>,
        /// 声纹模型或辅助程序未就绪时的降级提示（保持个人上下文隔离，不回退共享设备记忆）
        speaker_warning: Mutex<Option<String>>,
        /// 本地模型下载与安装（默认运行目录 `models/`，可通过 `MOSS_LAYA_ROOT` 指定外置 `<path>/models`）
        models: ModelManager,
        /// Laya 的 Python 运行环境自动准备（2026-11-07：点下载 Laya 时在软件目录创建虚拟环境并安装 laya[serve]）
        laya_setup: moss_voice::laya_env::LayaSetup,
        /// 测试用 Laya 根目录（测试中不碰真实用户目录下的虚拟环境）
        #[cfg(test)]
        laya_root_for_test: Mutex<Option<std::path::PathBuf>>,
        /// 运行中的本地模型辅助程序（随内置服务启停）
        helper: Mutex<Option<LocalHelper>>,
        /// 运行中的尾音语义判停专用子进程（随内置服务启停，与主 `helper` 隔离）
        turn_worker: Mutex<Option<TurnHelper>>,
        /// 运行中的 `speech.cpp`（Qwen3-TTS）长驻子进程（随内置服务启停）
        speech_worker: Mutex<Option<SpeechCppWorker>>,
        /// 运行中的本地降级兜底合成句柄（用于停止服务与删除模型时隔离和回收延迟加载的子进程）
        fallback_tts: Mutex<Option<Arc<LazyLocalFallbackTts>>>,
        /// 运行中 `helper` 已加载的本地模型规格快照（配置一致时供全链连贯试跑直接复用，免去另起子进程重载模型）
        running_local: Mutex<Option<RunningLocalSpec>>,
        /// 全链路连贯试跑单飞互斥（拒绝并发重入）
        chain_test_lock: Mutex<()>,
        /// 串行化本地 Jev 决策服务（OneJev / Laya）启动与模型切换，不阻塞 `stop_laya`
        jev_start_lock: Arc<Mutex<()>>,
        /// n74 本地视觉模型运行管理
        local_vision: Arc<LocalVision>,
        /// n80 Colab CLI 登录检测与后台云端 T4 会话托管
        colab_cli: Arc<Mutex<ColabCliState>>,
        /// 串行化本机 `moss_ref.wav` 原子落盘/删除与 `ref_sync_gen` 推进（锁序恒为 `colab_ref_io -> colab_cli`）
        colab_ref_io: Mutex<()>,
        #[cfg(test)]
        colab_cli_installed_for_test: std::sync::atomic::AtomicBool,
        /// 运行中的本地 Jev `/v1/systemone` 决策服务（OneJev `llama-server` 网关或 Laya 子进程，随本地 Jev 开关启停）
        laya: Arc<Mutex<LocalLayaRunner>>,
        /// NV19 长期记忆、资料与恢复摘要；检索与提取默认关闭
        context: Arc<crate::voice_context::host::ContextHost>,
        /// n59 当前说话角色（全部设备共用；语音与界面切换都即时生效，不重启服务）
        persona: Arc<PersonaSwitch>,
        /// 轻量轮询（`lite = true`）复用的磁盘/网卡环境探测短缓存，避免 2s 轮询反复扫盘与序列化大 GIF
        status_probe_cache: Mutex<Option<StatusProbeCache>>,
        /// 本地合成模型各音色试听语音的后台自动缓存队列（下载完自动预热，独立临时进程合成完毕即退出）
        tts_preview_warmer: (Mutex<TtsPreviewWarmState>, std::sync::Condvar),
        /// n84 `base-assets.bin` 内容指纹缓存（按文件字节数 + 修改时间校验；导入/移除时主动刷新，免去轮询重复读盘哈希）
        wakeword_base_fp_cache: Mutex<Option<BasePackFpCache>>,
        /// USB 一键配网串口互斥锁，防止双击同时打开同一个串口
        usb_provision_lock: Mutex<()>,
    }

    impl BuiltinVoice {
        /// NV13 desktop-only inert configuration. Live execution remains locked
        /// until separately authorized real CLI sandbox-negative tests succeed.
        pub fn agent_stop(&self, run_id: &str) -> Value {
            let stopped = self.host.api_service.agent_stop(run_id);
            let mut status = self.agent_status();
            status["stopped"] = Value::Bool(stopped);
            status
        }
        pub fn agent_status(&self) -> Value {
            let mut status = self.host.api_service.agent_status();
            status["settings"] =
                serde_json::to_value(self.settings().builtin_agents).unwrap_or(Value::Null);
            status
        }
        /// NV13 Agent 配置（2026-10-15 起可开启；2026-10-25 支持留空自动读取本机 CLI 与换机自动适配）：
        /// 开启时必须有工作目录，且本机至少有一个手动填写或自动检测到的 CLI；每次运行仍要桌面批准并要求严格模式。
        pub fn agent_save(&self, input: &Value) -> Result<Value, CenterError> {
            use laser_host::voice_agent::detect_cli;
            use moss_voice::settings::BuiltinAgents;
            let invalid = |message: &str| CenterError::new("agent_config_invalid", 400, message);
            let raw: BuiltinAgents =
                serde_json::from_value(input.clone()).map_err(|_| invalid("Agent 配置格式无效"))?;
            let sensitive = [self.host.runtime_root.path.clone()];
            let mut validation_err: Option<CenterError> = None;
            moss_voice::settings::update_voice(&self.settings_file(), |settings| {
                match normalize_agent_save(raw, &settings.builtin_agents, &sensitive, detect_cli) {
                    Ok(config) => {
                        settings.builtin_agents = config;
                        Ok(())
                    }
                    Err(err) => {
                        validation_err = Some(err);
                        Err(String::new())
                    }
                }
            })
            .map_err(|_| {
                validation_err.unwrap_or_else(|| {
                    CenterError::new("agent_save_failed", 500, "Agent 配置保存失败")
                })
            })?;
            Ok(self.agent_status())
        }
    }

    /// 校验并清理 Agent 保存输入：
    /// 1. 关闭开关（`enabled = false`）时只检查长度与控制字符，不因旧电脑遗留的目录或命令路径拦住保存；
    /// 2. 开启状态下，若 CLI 路径明显来自异构系统，或沿用上次保存的旧路径但在本机已失效，自动清空切回自动读取；
    /// 3. 开启状态下，新填写或仍保留的手动路径及工作目录照常严格校验。
    fn normalize_agent_save(
        mut config: moss_voice::settings::BuiltinAgents,
        previous: &moss_voice::settings::BuiltinAgents,
        sensitive: &[std::path::PathBuf],
        detect: impl Fn(laser_host::voice_agent::Provider) -> Option<std::path::PathBuf>,
    ) -> Result<moss_voice::settings::BuiltinAgents, CenterError> {
        use laser_host::voice_agent::{Provider, executable, is_foreign_os_path, workspace};
        let invalid = |message: &str| CenterError::new("agent_config_invalid", 400, message);
        for text in [
            &config.workspace,
            &config.codex_executable,
            &config.claude_executable,
        ] {
            if text.len() > 1024 || text.chars().any(char::is_control) {
                return Err(invalid("Agent 路径最长 1024 字节且不能包含控制字符"));
            }
        }
        if !config.enabled {
            return Ok(config);
        }
        if config.workspace.trim().is_empty() {
            return Err(invalid("开启 Agent 前必须先填写工作目录"));
        }
        for (field, old_field, provider) in [
            (
                &mut config.codex_executable,
                &previous.codex_executable,
                Provider::Codex,
            ),
            (
                &mut config.claude_executable,
                &previous.claude_executable,
                Provider::ClaudeCode,
            ),
        ] {
            let trimmed = field.trim();
            if trimmed.is_empty() {
                continue;
            }
            let inherited_broken = trimmed == old_field.trim()
                && executable(std::path::Path::new(trimmed), provider).is_err();
            if is_foreign_os_path(trimmed) || inherited_broken {
                field.clear();
            }
        }
        if config.codex_executable.trim().is_empty()
            && config.claude_executable.trim().is_empty()
            && detect(Provider::Codex).is_none()
            && detect(Provider::ClaudeCode).is_none()
        {
            return Err(invalid(
                "未在本机自动检测到 Codex 或 Claude Code，请先安装或手动填写路径",
            ));
        }
        workspace(std::path::Path::new(&config.workspace), sensitive)
            .map_err(|_| invalid("工作目录必须为现有、独立、非敏感的本地目录"))?;
        if !config.codex_executable.trim().is_empty() {
            executable(
                std::path::Path::new(config.codex_executable.trim()),
                Provider::Codex,
            )
            .map_err(|_| invalid("Codex 路径无效：请填写本机原生可执行文件，或留空自动读取"))?;
        }
        if !config.claude_executable.trim().is_empty() {
            executable(
                std::path::Path::new(config.claude_executable.trim()),
                Provider::ClaudeCode,
            )
            .map_err(|_| {
                invalid("Claude Code 路径无效：请填写本机原生可执行文件，或留空自动读取")
            })?;
        }
        Ok(config)
    }

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

    pub fn parse_sections(values: &[String]) -> Result<Vec<Section>, CenterError> {
        values
            .iter()
            .map(|value| {
                Section::parse(value).ok_or_else(|| {
                    CenterError::new("model_input_invalid", 400, "分区只能是 llm、asr 或 tts")
                })
            })
            .collect()
    }

    /// MOSS 退出时停止内置语音服务、Colab CLI 后台进程及本地 Jev（OneJev / Laya）子进程（在急停之后调用）。
    pub fn shutdown(app: &tauri::AppHandle) {
        if let Ok(builtin) = builtin(app) {
            builtin.cancel_colab_cli_runner(None);
            builtin.laya_setup.cancel_and_wait();
            builtin.stop_all_tts_preview_warmups();
            builtin.stop();
            // 等待已被 starting_cancel 中断的后台启动线程完成子进程回收，杜绝退出时残留孤儿 llama-server
            builtin.stop_laya_and_wait();
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

/// 语音中心校验用的豆包默认值与模型提供方的默认值一致（两个 crate 各自定义，避免互相依赖）。
#[cfg(all(test, desktop))]
#[test]
fn doubao_defaults_match_the_provider() {
    use moss_voice::models::{
        DOUBAO_ASR_RESOURCE, DOUBAO_ASR_URL, DOUBAO_TTS_RESOURCE, DOUBAO_TTS_URL,
    };
    use moss_voice_providers::doubao;
    assert_eq!(DOUBAO_ASR_URL, doubao::DEFAULT_ASR_URL);
    assert_eq!(DOUBAO_ASR_RESOURCE, doubao::DEFAULT_ASR_RESOURCE);
    assert_eq!(DOUBAO_TTS_URL, doubao::DEFAULT_TTS_URL);
    assert_eq!(DOUBAO_TTS_RESOURCE, doubao::DEFAULT_TTS_RESOURCE);
    for sample in [
        "https://api.cline.bot/api/v1",
        "https://cline.bot/v1",
        "https://openrouter.ai/api/v1",
        "https://evil-cline.bot/v1",
    ] {
        assert_eq!(
            moss_voice::models::is_cline_api_url(sample),
            moss_voice_providers::openai::is_cline_api_url(sample),
            "Cline URL 判定须在 moss-voice 与 moss-voice-providers 间保持一致: {sample}"
        );
    }
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
        let audit=moss_voice::home_backup_audit::audit_selected_file(&path)
            .map_err(|_|serde_json::json!({"code":"home_audit_invalid","message":"文件不符合只读安全审计条件（路径、类型或大小）"}))?;
        let category=match audit.risk {
            moss_voice::home_backup_audit::BackupRisk::SuspectedLegacyCredential=>"suspected_legacy_credential",
            moss_voice::home_backup_audit::BackupRisk::EncryptedCredentialMaterial=>"encrypted_credential_material",
            moss_voice::home_backup_audit::BackupRisk::UnknownTreatAsSensitive=>"unknown_treat_as_sensitive",
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

#[tauri::command]
pub async fn voice_memory_list(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.memory_list()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}

#[tauri::command]
pub async fn voice_memory_set_enabled(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    enabled: bool,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.memory_enable(enabled)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, enabled));
    result
}

#[tauri::command]
pub async fn voice_memory_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    device_id: String,
    expected_revision: i64,
    summary: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| {
        voice.memory_edit(&device_id, expected_revision, &summary)
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, device_id, expected_revision, summary));
    result
}

#[tauri::command]
pub async fn voice_memory_clear(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    device_id: String,
    expected_revision: i64,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| {
        voice.memory_edit(&device_id, expected_revision, "")
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, device_id, expected_revision));
    result
}

/// 前端调用的内置语音命令（build.rs 登记与 capability 授权的测试会核对这张表）。
#[cfg(test)]
pub const COMMANDS: [&str; 48] = [
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
    "voice_memory_list",
    "voice_memory_set_enabled",
    "voice_memory_save",
    "voice_memory_clear",
    "voice_builtin_status",
    "voice_builtin_start",
    "voice_builtin_stop",
    "voice_diagnostics_export",
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
    "voice_usb_provision",
];
