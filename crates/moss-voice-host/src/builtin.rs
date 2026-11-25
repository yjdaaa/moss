//! 内置语音服务编排核心（设备登记、对话流水线、本地模型、视觉、家居、唤醒词、固件与 USB 配网；零 Tauri/UI 依赖）。

use std::sync::{Arc, Mutex};

use moss_voice::builtin::{self, KeyChange};
use moss_voice::local_models::{self, Engine, ModelKind, ModelManager};
use moss_voice::settings::{BuiltinLocal, builtin_secret_name};
use moss_voice::{CenterError, ModelsInput, ProviderKind, Section, SectionInput, VoiceMode};
use moss_voice_providers::dashscope::DashscopeTts;
use moss_voice_providers::doubao::{DoubaoAsr, DoubaoTts};
use moss_voice_providers::local::{
    AsrEngine, HELPER_FILE, LocalAsr, LocalHelper, LocalStreamingAsr, LocalTts, TtsEngine,
    TurnHelper, VadEngine, VadParams,
};
use moss_voice_providers::openai::{OpenAiAsr, OpenAiLlm, OpenAiTts};
use moss_voice_providers::speech_cpp::{SpeechCppTts, SpeechCppWorker};
use moss_voice_providers::{Asr, StreamingAsr, Tts};
use moss_voice_server::{
    Conversation, ConversationSettings, DeviceRegistry, DeviceStatus, DeviceVisit, DiagCategory,
    EnergyVad, EventRecord, HostTools, JevFilterOptions, LocalVadFactory, OpenRouterDecisionClient,
    ServerConfig, ServerMode, ToolContext, ToolHost, ToolHostResult, TurnEndPredictor, Vad,
    VadConfig, VadEvent, VadFactory, VoiceJevFilter, VoiceServer,
};
use serde_json::Value;

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
pub use self::lifecycle::{BuiltinState, autostart, init, record_moss_event};
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
    /// 仅内存、重启即清的实时对话调试流总开关（与持久配置同步，并与 DbRegistry 共享）
    debug_trace_enabled: Arc<std::sync::atomic::AtomicBool>,
    /// 对话记录关闭/清空代次：每次关闭或清表递增，防止在途轮次回写数据库
    chat_log_epoch: Arc<std::sync::atomic::AtomicU64>,
    memory: Arc<MemoryService>,
    speaker: Arc<moss_voice_server::speaker_enrollment::SpeakerEnrollment>,
    speaker_backend: Arc<DesktopSpeakerBackend>,
    personal: Arc<PersonContext>,
    identity: Arc<DesktopIdentity>,
    home_control: Arc<moss_home::HomeControlCoordinator>,
    /// 最近一次自动启动失败的原因（语音链路为内置服务时随 MOSS 启动）
    last_error: Mutex<Option<String>>,
    /// 诊断记录本（2026-11-07 n68）：服务事件、对话耗时与桌面端后台失败（本地模型、Laya、固件记录等），
    /// 传给每次启动的语音服务，停止、重启、崩溃重启都不清空；导出诊断包时整份写出。
    diagnostics: Arc<moss_voice_server::DiagnosticsLog>,
    /// 本地音乐目录换机或拔盘不可用时的降级提示（不阻断内置语音主服务启动）
    music_warning: Mutex<Option<String>>,
    /// 声纹模型或辅助程序未就绪时的降级提示（保持个人上下文隔离，不回退共享设备记忆）
    speaker_warning: Mutex<Option<String>>,
    /// Windows 系统重置后检测到 `Windows.old` 同名用户目录下仍有未迁回的旧版语音配置或模型时的启动提示
    windows_old_notice: Option<WindowsOldMossNotice>,
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
    context: Arc<crate::context::ContextHost>,
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
        use moss_agent::detect_cli;
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
            validation_err
                .unwrap_or_else(|| CenterError::new("agent_save_failed", 500, "Agent 配置保存失败"))
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
    detect: impl Fn(moss_agent::Provider) -> Option<std::path::PathBuf>,
) -> Result<moss_voice::settings::BuiltinAgents, CenterError> {
    use moss_agent::{Provider, executable, is_foreign_os_path, workspace};
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
        .map_err(|_| invalid("Claude Code 路径无效：请填写本机原生可执行文件，或留空自动读取"))?;
    }
    Ok(config)
}

impl BuiltinVoice {
    /// MOSS 退出时停止内置语音服务、Colab CLI 后台进程及本地 Jev（OneJev / Laya）子进程（在急停之后调用）。
    pub fn shutdown(&self) {
        self.cancel_colab_cli_runner(None);
        self.laya_setup.cancel_and_wait();
        self.stop_all_tts_preview_warmups();
        self.stop();
        // 等待已被 starting_cancel 中断的后台启动线程完成子进程回收，杜绝退出时残留孤儿 llama-server
        self.stop_laya_and_wait();
    }
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

/// 语音中心校验用的豆包默认值与模型提供方的默认值一致（两个 crate 各自定义，避免互相依赖）。
#[cfg(test)]
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
