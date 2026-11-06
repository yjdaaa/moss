//! 对话模式（NV3）：一段话 → 识别 → 大模型（流式，可调用工具）→ 切句 → 逐句合成 → 交给会话播放。
//!
//! 一轮对话（turn）在独立的 tokio 任务中运行，通过 [`TurnEvent`] 把识别结果、每句音频与结束信息送回会话；
//! 会话随时可以中止任务（设备打断）。规则取 xiaozhi-esp32-server `core/connection.py`（`7a260ee`）：
//! - 多个工具调用并行执行，各 30 秒超时；结果回填后再问模型，最多 5 轮；
//! - 工具可要求“直接播报”某段话而不再问模型；
//! - 退出词直接说再见并结束会话；历史按长度预算截断（esp32-server 没有上限）。
//!
//! 工具来源（NV4）：服务级的 [`ToolExecutor`]（MOSS 工具，见 [`crate::tools`]）与每台设备自带的 MCP 工具
//! （[`DeviceTools`]，经会话的 WebSocket 中继）。
//!
//! 流式厂商（NV7）：配置了 [`StreamingAsr`] 时会话在用户开始说话时就建立识别连接、边说边送音频，说完只等最后
//! 结果（失败时退回整段识别一次）；合成用 [`Tts::synthesize_stream`]，每句的第一段音频一到就交给会话播放。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use moss_voice_providers::text::{Segmenter, emotion_of};
use moss_voice_providers::{
    Asr, AsrStream, BoxFuture, ChatMessage, Llm, Pcm, StreamingAsr, ToolCall, ToolSpec, Tts,
    compact_stale_tool_results, truncate_history,
};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio::time::Instant;

/// 默认系统提示（NV4 接入雕刻工具时补充工具使用规则）。
pub const DEFAULT_SYSTEM_PROMPT: &str = "你是 MOSS，一个运行在用户电脑上的中文语音助手，通过小智设备与用户对话。\
回答要简短、口语化，一般不超过三句话；不要使用 Markdown、列表、代码块和表情符号，因为你的回答会被直接朗读出来。\
不知道的事情就直接说不知道，不要编造。\
需要让激光机运动或出光（开始加工、发送任务、续雕、设定原点、走边框等）时：先复述材料、厚度、内容与功率速度次数，\
问用户“确认开始吗”，只有用户在下一句明确说“确认”或“开始”后，才能带 confirmed=true 调用工具；不要替用户确认，也不要猜参数。\
用户说停、急停、取消时立即调用对应的停止工具，不需要确认。";

/// 不可被任何风格预设或自定义补充提示覆盖的底层语音与硬件安全铁律。
pub const IMMUTABLE_SAFETY_RULES: &str = "回答要简短、口语化，一般不超过三句话；不要使用 Markdown、列表、代码块和表情符号，因为你的回答会被直接朗读出来。\
不知道的事情就直接说不知道，不要编造。\
需要让激光机运动或出光（开始加工、发送任务、续雕、设定原点、走边框等）时：先复述材料、厚度、内容与功率速度次数，\
问用户“确认开始吗”，只有用户在下一句明确说“确认”或“开始”后，才能带 confirmed=true 调用工具；不要替用户确认，也不要猜参数。\
用户说停、急停、取消时立即调用对应的停止工具，不需要确认。";

/// 组合「内置角色开头 + 用户工坊背景补充 + 锁死在末尾的底层安全铁律」（只含内置角色；自建角色见 [`crate::persona`]）。
pub fn compose_persona_system_prompt(style: &str, custom_prompt: &str) -> String {
    crate::persona::PersonaConfig {
        active: style.trim().to_owned(),
        background: custom_prompt.to_owned(),
        custom: Vec::new(),
    }
    .system_prompt()
}

pub fn local_clock_line() -> String {
    use chrono::{Datelike, Local, Timelike, Weekday};
    let now = Local::now();
    let weekday = match now.weekday() {
        Weekday::Mon => "星期一",
        Weekday::Tue => "星期二",
        Weekday::Wed => "星期三",
        Weekday::Thu => "星期四",
        Weekday::Fri => "星期五",
        Weekday::Sat => "星期六",
        Weekday::Sun => "星期日",
    };
    format!(
        "当前本机时间：{:04}-{:02}-{:02}（{}）{:02}:{:02}。",
        now.year(),
        now.month(),
        now.day(),
        weekday,
        now.hour(),
        now.minute()
    )
}

pub fn build_turn_system_prompt(base_prompt: &str, include_clock: bool, ongoing_dialogue: bool) -> String {
    let mut out = String::with_capacity(base_prompt.len() + 160);
    out.push_str(base_prompt);
    if include_clock {
        out.push('\n');
        out.push_str(&local_clock_line());
    }
    if ongoing_dialogue {
        out.push_str("\n对话连贯约束：当前为同一场对话的后续轮次，除用户主动询问身份或称呼外，请直接切入回答，不要每句话开头重复机械喊出用户称呼。");
    }
    out
}

/// 0ms 本地 ASR 同音错词纠偏：急停/停止短令原样保护，其余按词长优先顺序替换。
pub fn apply_asr_corrections(raw: &str, rules: &[(String, String)]) -> String {
    if rules.is_empty() || raw.is_empty() || crate::fast::is_stop(raw) {
        return raw.to_owned();
    }
    let mut text = raw.to_owned();
    for (wrong, right) in rules {
        if !wrong.is_empty() && wrong != right && text.contains(wrong.as_str()) {
            text = text.replace(wrong.as_str(), right.as_str());
        }
    }
    text
}

/// 工具调用的上下文（NV4 用于构造 `CallerContext::Voice` 与审计）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolContext {
    pub device_id: String,
    pub session_id: String,
}

/// 工具执行结果的处理方式（esp32-server 的 `Action`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutcome {
    /// 结果回填给模型，由模型组织回答（`REQLLM`）
    Reply(String),
    /// 直接朗读这段话，不再询问模型（`RESPONSE`）；同时作为工具结果记入历史
    Speak(String),
    /// Trusted read-only music selection; only the device session may open it.
    PlayMusic { track_id: String, title: String },
}

/// 工具执行方。实现须线程安全；单次调用由对话层加 30 秒超时。
pub trait ToolExecutor: Send + Sync {
    fn tools(&self) -> Vec<ToolSpec>;
    fn call<'a>(
        &'a self,
        context: &'a ToolContext,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, ToolOutcome>;
    /// 固定路由读取完整结果，不能使用为模型截断的 JSON 详情。
    fn call_raw<'a>(
        &'a self,
        context: &'a ToolContext,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, crate::tools::ToolHostResult> {
        Box::pin(async move {
            let text = match self.call(context, name, arguments).await {
                ToolOutcome::Reply(s) | ToolOutcome::Speak(s) => s,
                ToolOutcome::PlayMusic { .. } => {
                    return crate::tools::ToolHostResult {
                        success: false,
                        result: "音乐必须通过设备会话播放".into(),
                        detail: None,
                    };
                }
            };
            let value: Value = serde_json::from_str(&text).unwrap_or_default();
            crate::tools::ToolHostResult {
                success: value["success"].as_bool().unwrap_or(false),
                result: value["result"]
                    .as_str()
                    .unwrap_or("工具未返回可验证的结果")
                    .to_owned(),
                detail: value["detail"].as_str().map(str::to_owned),
            }
        })
    }
    /// 只读读取最近的加工任务（加工结束播报轮询用）。
    /// 默认不支持：宿主没有提供“不计入工具调用统计”的只读通道时，不做任何轮询。
    fn recent_workflows<'a>(
        &'a self,
        _context: &'a ToolContext,
    ) -> BoxFuture<'a, Option<Value>> {
        Box::pin(async { None })
    }
}

/// 不提供任何工具（NV3 默认）。
pub struct NoTools;

impl ToolExecutor for NoTools {
    fn tools(&self) -> Vec<ToolSpec> {
        Vec::new()
    }

    fn call<'a>(
        &'a self,
        _: &'a ToolContext,
        name: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move { ToolOutcome::Reply(format!("工具 {name} 不存在")) })
    }
}

/// 语音急停快通道成功后的固定反馈话术
pub const STOP_OK_SPEECH: &str = "已发送停止请求，请确认设备状态。";
/// 语音急停快通道失败后的固定反馈话术
pub const STOP_ERR_SPEECH: &str = "停止请求发送失败，请使用设备急停按钮并检查连接。";
/// 慢速外部信息查询工具（天气/新闻/联网搜索/RAGFlow 知识库）触发时的过渡垫音
pub const SLOW_QUERY_FILLER: &str = "稍等，正在查询。";
/// 慢速 AI 生图/刀路预览生成工具触发时的过渡垫音（仅预览生成，不涉及硬件动作）
pub const SLOW_GENERATE_FILLER: &str = "稍等，正在生成预览。";

fn arg_flag_true(arguments: &Value, key: &str) -> bool {
    match arguments.get(key) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(text)) => text.trim().eq_ignore_ascii_case("true") || text.trim() == "1",
        Some(Value::Number(num)) => num.as_i64() == Some(1),
        _ => false,
    }
}

fn is_slow_query_call(call: &ToolCall) -> bool {
    matches!(
        call.function.name.as_str(),
        "web_search" | "search_rag" | "get_weather" | "get_news"
    )
}

fn is_slow_generate_preview_call(call: &ToolCall) -> bool {
    let name = call.function.name.as_str();
    match name {
        "generate_text_image_tool" => true,
        "ai_laser_gcode_tool"
        | "generate_text_image_gcode_tool"
        | "generate_text_laser_task_tool" => {
            let parsed: Value = serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);
            !(arg_flag_true(&parsed, "confirmed") && arg_flag_true(&parsed, "send_after_generate"))
        }
        "regenerate_text_laser_task_tool" => {
            let parsed: Value = serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);
            !parsed
                .get("feedback_id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.trim().is_empty())
        }
        _ => false,
    }
}

/// 判定本轮工具调用是否需要先行播放缓存过渡垫音：
/// - 仅当整批调用全部属于慢速查询或慢速预览生成时才返回垫音；
/// - 一旦包含任何控制、R3 审批、音乐或本地毫秒级快工具，一律返回 `None`。
pub fn slow_tool_filler(calls: &[ToolCall]) -> Option<&'static str> {
    if calls.is_empty() {
        return None;
    }
    let mut has_generate = false;
    for call in calls {
        if is_slow_generate_preview_call(call) {
            has_generate = true;
        } else if !is_slow_query_call(call) {
            return None;
        }
    }
    if has_generate {
        Some(SLOW_GENERATE_FILLER)
    } else {
        Some(SLOW_QUERY_FILLER)
    }
}

#[derive(Debug, Clone)]
pub struct ConversationSettings {
    pub system_prompt: String,
    /// 历史长度预算（字符数，约等于中文 token 数；不含系统提示）
    pub history_budget: usize,
    /// 说出其中之一（去掉标点后完全相同）即结束会话
    pub exit_words: Vec<String>,
    /// 设备检测到唤醒词时的回应；为空则不回应
    pub wake_reply: Option<String>,
    /// 退出与空闲断开前说的话
    pub goodbye: String,
    /// 识别或大模型出错时说的话
    pub error_reply: String,
    pub max_tool_rounds: usize,
    pub tool_timeout: Duration,
    pub read_only_shortcuts: bool,
    /// 加工结束后在空闲且设备在线时主动播报（2026-11-02 用户决定，语音中心可关）
    pub announce_job_done: bool,
    /// 加工发送期间是否将小智屏幕同步为实时加工副屏 HUD（随 `screen_preview_sync` 开关控制）
    pub screen_preview_sync: bool,
    pub model_labels: Vec<String>,
    /// 是否在每轮系统提示中注入当前本机日期、星期与时间（默认开）
    pub include_local_clock: bool,
    /// 0ms 本地 ASR 同音错词纠偏规则（按错词长度降序）
    pub asr_corrections: Vec<(String, String)>,
}

impl Default for ConversationSettings {
    fn default() -> Self {
        Self {
            system_prompt: DEFAULT_SYSTEM_PROMPT.into(),
            history_budget: 4000,
            exit_words: ["退出", "再见", "拜拜", "退下", "关闭对话"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            wake_reply: Some("我在".into()),
            goodbye: "好的，再见".into(),
            error_reply: "抱歉，我这边出了点问题，请稍后再试".into(),
            max_tool_rounds: 5,
            tool_timeout: Duration::from_secs(30),
            read_only_shortcuts: true,
            announce_job_done: true,
            screen_preview_sync: true,
            model_labels: Vec::new(),
            include_local_clock: true,
            asr_corrections: Vec::new(),
        }
    }
}

/// 对话所需的模型与工具。整个服务共享一份，每台设备的历史在各自的会话里。
pub struct Conversation {
    pub asr: Arc<dyn Asr>,
    /// 流式识别（NV7）；有时会话边说边识别，`asr` 作为它失败时的退路
    pub streaming_asr: Option<Arc<dyn StreamingAsr>>,
    pub llm: Arc<dyn Llm>,
    pub tts: Arc<dyn Tts>,
    /// 主云端 TTS 失败且未产出本句音频时的本地降级 TTS（如本机已安装的 Kokoro / Matcha）。
    pub fallback_tts: Option<Arc<dyn Tts>>,
    pub tools: Arc<dyn ToolExecutor>,
    pub settings: ConversationSettings,
    pub memory: Option<Arc<crate::memory::MemoryService>>,
    pub speaker_enrollment: Option<Arc<crate::speaker_enrollment::SpeakerEnrollment>>,
    pub personal_context:Option<Arc<crate::person_context::PersonContext>>,
    /// NV19-04 长期记忆检索；宿主不接入时为空，NV10 摘要照常使用
    pub context_retrieval: Option<crate::context::retrieve::ContextRetrieval>,
    /// NV19-03 会话结束后的候选提取；宿主不接入时为空
    pub context_extraction: Option<crate::context::extract::ContextExtraction>,
    /// 可选 Jev 信息筛选与快答分流器；默认未挂载，开启后不携带任何动作授权。
    pub jev_filter: Option<Arc<crate::jev::VoiceJevFilter>>,
    /// n59 当前说话角色；挂载后每轮用它的提示词代替 `settings.system_prompt`（可被语音或界面热切换）
    pub persona: Option<Arc<crate::persona::PersonaSwitch>>,
    /// 固定话术（唤醒回应、再见）的合成结果缓存
    cache: Mutex<Vec<(String, Pcm)>>,
}

impl fmt::Debug for Conversation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Conversation")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl Conversation {
    pub fn new(
        asr: Arc<dyn Asr>,
        llm: Arc<dyn Llm>,
        tts: Arc<dyn Tts>,
        tools: Arc<dyn ToolExecutor>,
        settings: ConversationSettings,
    ) -> Self {
        Self {
            asr,
            streaming_asr: None,
            llm,
            tts,
            fallback_tts: None,
            tools,
            settings,
            memory: None,
            speaker_enrollment: None,
            personal_context:None,
            context_retrieval: None,
            context_extraction: None,
            jev_filter: None,
            persona: None,
            cache: Mutex::new(Vec::new()),
        }
    }

    /// 挂载 n59 说话角色：提示词随语音或界面切换即时更新。
    pub fn with_persona(mut self, persona: Arc<crate::persona::PersonaSwitch>) -> Self {
        self.persona = Some(persona);
        self
    }

    /// 本轮的基础系统提示：挂了角色用角色的，否则用固定设置。
    pub fn system_prompt(&self) -> String {
        match &self.persona {
            Some(persona) => persona.system_prompt(),
            None => self.settings.system_prompt.clone(),
        }
    }

    pub fn with_memory(mut self, memory: Arc<crate::memory::MemoryService>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_speaker_enrollment(mut self, enrollment:Arc<crate::speaker_enrollment::SpeakerEnrollment>)->Self {
        self.speaker_enrollment=Some(enrollment);self
    }
    pub fn with_personal_context(mut self,context:Arc<crate::person_context::PersonContext>)->Self {
        self.personal_context=Some(context);self
    }
    pub fn with_context_retrieval(mut self, retrieval: crate::context::retrieve::ContextRetrieval) -> Self {
        self.context_retrieval = Some(retrieval);
        self
    }
    pub fn with_context_extraction(mut self, extraction: crate::context::extract::ContextExtraction) -> Self {
        self.context_extraction = Some(extraction);
        self
    }
    pub fn with_jev_filter(mut self, filter: Arc<crate::jev::VoiceJevFilter>) -> Self {
        self.jev_filter = Some(filter);
        self
    }

    /// 启用流式识别（NV7）。
    pub fn with_streaming_asr(mut self, streaming_asr: Arc<dyn StreamingAsr>) -> Self {
        self.streaming_asr = Some(streaming_asr);
        self
    }

    /// 配置主 TTS 失败时的本地降级 TTS（当句尚未产出音频即报错时自动降级，避免整轮静音）。
    pub fn with_fallback_tts(mut self, fallback_tts: Arc<dyn Tts>) -> Self {
        self.fallback_tts = Some(fallback_tts);
        self
    }
    fn is_exit(&self, text: &str) -> bool {
        let normalized: String = text.chars().filter(|c| c.is_alphanumeric()).collect();
        !normalized.is_empty()
            && self.settings.exit_words.iter().any(|word| {
                word.chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect::<String>()
                    == normalized
            })
    }

    async fn cached_speech(&self, text: &str) -> Result<Pcm, String> {
        if let Ok(cache) = self.cache.lock()
            && let Some((_, pcm)) = cache.iter().find(|(key, _)| key == text)
        {
            return Ok(pcm.clone());
        }
        let mut pcm = match self.tts.synthesize(text).await {
            Ok(pcm) => pcm,
            Err(primary_err) => {
                if let Some(fallback) = &self.fallback_tts {
                    fallback
                        .synthesize(text)
                        .await
                        .map_err(|_| primary_err.to_string())?
                } else {
                    return Err(primary_err.to_string());
                }
            }
        };
        crate::audio::trim_trailing_silence_i16(
            &mut pcm.samples,
            pcm.sample_rate,
            crate::audio::TRAILING_SILENCE_KEEP_MS,
            crate::audio::TRAILING_SILENCE_TRIGGER_MS,
        );
        crate::audio::fade_head_i16(&mut pcm.samples, pcm.sample_rate, crate::audio::BOUNDARY_FADE_MS);
        crate::audio::fade_tail_i16(&mut pcm.samples, pcm.sample_rate, crate::audio::BOUNDARY_FADE_MS);
        const MAX_SPEECH_CACHE_ENTRIES: usize = 32;
        if let Ok(mut cache) = self.cache.lock()
            && !cache.iter().any(|(key, _)| key == text)
        {
            if cache.len() >= MAX_SPEECH_CACHE_ENTRIES {
                let pinned = self.fixed_phrases();
                let evict_idx = cache
                    .iter()
                    .position(|(key, _)| !pinned.iter().any(|p| p == key))
                    .unwrap_or(0);
                cache.remove(evict_idx);
            }
            cache.push((text.to_owned(), pcm.clone()));
        }
        Ok(pcm)
    }
    /// 返回需要预合成缓存的高频固定话术清单（去重、过滤空白）。
    pub fn fixed_phrases(&self) -> Vec<String> {
        let mut phrases = Vec::with_capacity(9);
        let mut push_unique = |text: &str| {
            let trimmed = text.trim();
            if !trimmed.is_empty() && !phrases.iter().any(|existing| existing == trimmed) {
                phrases.push(trimmed.to_owned());
            }
        };
        if let Some(wake) = self.settings.wake_reply.as_deref() {
            push_unique(wake);
        }
        push_unique(&self.settings.goodbye);
        push_unique(&self.settings.error_reply);
        push_unique(STOP_OK_SPEECH);
        push_unique(STOP_ERR_SPEECH);
        push_unique(crate::tools::APPROVAL_SPEECH);
        push_unique(crate::tools::SUBMITTED_APPROVAL_SPEECH);
        push_unique(SLOW_QUERY_FILLER);
        push_unique(SLOW_GENERATE_FILLER);
        phrases
    }

    /// 提前合成高频固定话术与慢工具过渡垫音并写入内存缓存，返回成功缓存的条数。
    /// 单条合成失败不会中断其余条目，后续实际触发时仍会按需重试合成。
    pub async fn prewarm_cache(&self) -> usize {
        let mut warmed = 0;
        for phrase in self.fixed_phrases() {
            if self.cached_speech(&phrase).await.is_ok() {
                warmed += 1;
            }
        }
        warmed
    }

    #[cfg(test)]
    pub(crate) async fn prewarm_or_speak_for_test(&self, text: &str) {
        let _ = self.cached_speech(text).await;
    }

    #[cfg(test)]
    pub(crate) fn speech_cache_len_for_test(&self) -> usize {
        self.cache.lock().map(|c| c.len()).unwrap_or(0)
    }
}

/// 任务取消守卫：对话被打断丢弃时，自动 abort 后台正在预合成的 TTS 任务。
struct AbortOnDrop<T>(Option<tokio::task::JoinHandle<T>>);

impl<T> AbortOnDrop<T> {
    fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self(Some(handle))
    }
    async fn join(&mut self) -> Option<Result<T, tokio::task::JoinError>> {
        Some(self.0.take()?.await)
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

/// 一句已启动后台 TTS 合成的待播句子（用于二级流水线预取，消除句间 TTS 启动间隙）。
struct PreparedSentence {
    text: String,
    chunks_rx: tokio::sync::mpsc::UnboundedReceiver<Pcm>,
    handle: AbortOnDrop<Result<(), moss_voice_providers::ProviderError>>,
}

/// 设备自带 MCP 工具的一次调用：由对话任务发给会话，会话经 WebSocket 转给设备并把结果文字送回。
#[derive(Debug)]
pub struct DeviceCall {
    /// 设备上报的原始工具名
    pub name: String,
    pub arguments: Value,
    pub reply: oneshot::Sender<String>,
}

/// 设备自带的 MCP 工具（内置语音技术书 §5.3）。工具名加 `device__` 前缀，并把模型接口不接受的字符
/// （如固件工具名中的 `.`）换成 `_`；调用时换回原名。
#[derive(Clone)]
pub struct DeviceTools {
    specs: Vec<ToolSpec>,
    names: HashMap<String, String>,
    calls: UnboundedSender<DeviceCall>,
}

impl DeviceTools {
    /// `tools` 为设备 `tools/list` 返回的条目（`name`、`description`、`inputSchema`）。
    pub fn new(tools: &[Value], calls: UnboundedSender<DeviceCall>) -> Self {
        let mut specs = Vec::new();
        let mut names = HashMap::new();
        for tool in tools {
            let Some(original) = tool["name"].as_str().filter(|name| !name.is_empty()) else {
                continue;
            };
            let cleaned: String = original
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let name: String = format!("device__{cleaned}").chars().take(64).collect();
            if names.contains_key(&name) {
                // 清理后重名：后面的不登记（不静默覆盖）
                continue;
            }
            names.insert(name.clone(), original.to_owned());
            specs.push(ToolSpec {
                name,
                description: tool["description"].as_str().unwrap_or_default().to_owned(),
                parameters: tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type": "object"})),
            });
        }
        Self {
            specs,
            names,
            calls,
        }
    }

    pub fn specs(&self) -> &[ToolSpec] {
        &self.specs
    }

    fn original(&self, name: &str) -> Option<&str> {
        self.names.get(name).map(String::as_str)
    }

    async fn call(&self, original: &str, arguments: &str) -> ToolOutcome {
        let arguments = if arguments.trim().is_empty() {
            "{}"
        } else {
            arguments
        };
        let Ok(arguments) = serde_json::from_str::<Value>(arguments) else {
            return ToolOutcome::Reply(format!("调用设备工具 {original} 的参数不是合法的 JSON"));
        };
        let (reply, result) = oneshot::channel();
        if self
            .calls
            .send(DeviceCall {
                name: original.to_owned(),
                arguments,
                reply,
            })
            .is_err()
        {
            return ToolOutcome::Reply("设备已断开".into());
        }
        ToolOutcome::Reply(result.await.unwrap_or_else(|_| "设备没有返回结果".into()))
    }
}

/// 一轮对话的输入。
pub enum TurnInput {
    /// 流式中间结果命中后立即执行；该段识别流由会话丢弃，最终结果不会重复执行。
    FastStop { text: String },
    /// 用户说的一段话（PCM 与采样率）；`stream` 为说话期间已在进行的流式识别
    Utterance {
        pcm: Vec<i16>,
        sample_rate: u32,
        stream: Option<AsrStream>,
    },
    /// 固定话术（唤醒回应、空闲再见）；`close` 为真时说完结束会话
    Say { text: String, close: bool },
}

/// 一轮对话送回会话的事件。
#[derive(Debug)]
pub enum TurnEvent {
    /// Trusted Rust-only identity; `verified` is true ONLY when voiceprint matched on this turn's audio.
    PersonResolved {
        scope: Option<crate::person_context::PersonScope>,
        verified: bool,
    },
    /// 退出词已经确认，合成再见之前就关闭插话检测。
    Closing,
    Timing {
        stage: &'static str,
        ms: u64,
    },
    ToolTiming {
        name: String,
        ms: u64,
    },
    TextReply(String),
    /// 识别结果（会话发 `stt`）
    Heard(String),
    /// 原始识别结果与 0ms 同音纠偏后的文本（供默认关闭的仅内存排障流使用）
    RawHeard {
        raw: String,
        corrected: String,
    },
    /// 工具调用详情摘要（仅在开启内存排障流时留存脱敏预览）
    ToolDebug {
        name: String,
        arguments: String,
        result: String,
    },
    /// 没有识别出文字：继续拾音
    Ignored,
    /// 模型回复中的第一个表情（会话发 `llm` 表情消息）
    Emotion(&'static str),
    /// 一句话的文字与（第一段）音频
    Speech {
        text: String,
        pcm: Pcm,
    },
    /// 同一句话后续到达的音频（流式合成）
    SpeechMore(Pcm),
    /// 只有受信工具能产生此事件，模型参数永远不成为文件路径。
    PlayMusic {
        track_id: String,
        title: String,
    },
    /// 出错（记入事件；不含密钥与地址）
    Failed(String),
    /// 结束：要追加到历史的消息；`close` 为真时播完后结束会话
    Done {
        messages: Vec<ChatMessage>,
        close: bool,
    },
}

struct Turn {
    started: Instant,
    measured: Mutex<std::collections::HashSet<&'static str>>,
    conversation: Arc<Conversation>,
    context: ToolContext,
    events: UnboundedSender<TurnEvent>,
    device: Option<DeviceTools>,
    _session_person_hint: Option<crate::person_context::PersonScope>,
}

impl Turn {
    async fn fixed_tool(&self, name: &str, arguments: Value) -> crate::tools::ToolHostResult {
        let began = Instant::now();
        let arguments = arguments.to_string();
        let result = tokio::time::timeout(
            self.conversation.settings.tool_timeout,
            self.conversation
                .tools
                .call_raw(&self.context, name, &arguments),
        )
        .await
        .unwrap_or_else(|_| crate::tools::ToolHostResult {
            success: false,
            result: "工具执行超时".into(),
            detail: None,
        });
        self.send(TurnEvent::ToolTiming {
            name: name.into(),
            ms: began.elapsed().as_millis() as u64,
        });
        self.send(TurnEvent::ToolDebug {
            name: name.into(),
            arguments,
            result: result.result.clone(),
        });
        result
    }

    async fn read_reply(&self, intent: crate::fast::ReadIntent) -> String {
        use crate::fast::{ReadIntent, body};
        use serde_json::json;
        if matches!(intent, ReadIntent::Status | ReadIntent::Connection) {
            let (tool, args) = if intent == ReadIntent::Status {
                ("laser_safe_action_tool", json!({"action":"status"}))
            } else {
                ("check_laser_connection_tool", json!({}))
            };
            let result = self.fixed_tool(tool, args).await;
            return if result.success {
                format!("查询结果：{}", result.result)
            } else {
                format!("查询失败：{}", result.result)
            };
        }
        let recent = self
            .fixed_tool("laser_workflow_tool", json!({"action":"recent"}))
            .await;
        if !recent.success {
            return format!("查询失败：{}", recent.result);
        }
        let recent = body(&recent);
        let active: Vec<&Value> = recent["workflows"]
            .as_array()
            .map(|items| items.iter().filter(|v| v["status"] == "sending").collect())
            .unwrap_or_default();
        if active.is_empty() {
            // 没有在途任务：不再一律回“没有找到”。材料只在加工中有意义，
            // 进度与剩余时间改为按最近一条记录回答终态（2026-11-02 用户要求）。
            if intent == ReadIntent::Material {
                return "没有找到正在发送的加工任务，无法确定当前加工进度或材料。".into();
            }
            let latest = recent["workflows"].as_array().and_then(|items| {
                items
                    .iter()
                    .max_by_key(|record| record["updated_at"].as_u64().unwrap_or(0))
            });
            return match latest {
                Some(record) => terminal_reply(record),
                None => "还没有任何加工任务记录。".into(),
            };
        }
        if active.len() != 1 {
            return "找到多个正在发送的任务，无法确定你询问的是哪个，请说明任务。".into();
        }
        let Some(id) = active[0]["workflow_id"].as_str() else {
            return "任务记录缺少编号，无法读取当前进度。".into();
        };
        let result = self
            .fixed_tool(
                "laser_workflow_tool",
                json!({"action":"status","workflow_id":id}),
            )
            .await;
        if !result.success {
            return format!("查询失败：{}", result.result);
        }
        let record = body(&result);
        if intent == ReadIntent::Material {
            let input = &record["input"];
            let (Some(material), Some(thickness)) =
                (input["material"].as_str(), input["thickness_mm"].as_f64())
            else {
                return "当前任务没有完整的材料与厚度记录，无法确定材料参数。".into();
            };
            let mut args = json!({"action":"get","material":material,"thickness_mm":thickness});
            for key in ["laser_mode", "engraving_mode"] {
                if let Some(v) = input[key].as_str() {
                    args[key] = json!(v);
                }
            }
            let result = self.fixed_tool("material_params_tool", args).await;
            if !result.success {
                return format!("材料参数查询失败：{}", result.result);
            }
            let params = body(&result);
            let (Some(power), Some(speed), Some(passes)) = (
                params["laser_max_power"].as_f64(),
                params["feed_rate"].as_f64(),
                params["passes"].as_u64(),
            ) else {
                return format!("当前材料是{material}，厚度{thickness}毫米；参数记录不完整。");
            };
            return format!(
                "当前材料是{material}，厚度{thickness}毫米；材料库建议功率{power}，速度每分钟{speed}毫米，{passes}次。建议参数不代表设备当前设置。"
            );
        }
        let progress = &record["send_result"];
        let prefix = match (
            progress["lines_sent"].as_u64(),
            progress["total_lines"].as_u64(),
        ) {
            (Some(sent), Some(total)) if total > 0 && sent <= total => format!(
                "已发送{sent}行，共{total}行，发送进度百分之{}。这不是物理加工完成比例。",
                sent.saturating_mul(100) / total
            ),
            _ => "当前任务没有可信的发送进度数据。".into(),
        };
        if intent == ReadIntent::Remaining {
            format!("{prefix}目前没有可信的剩余加工时间估计。")
        } else {
            prefix
        }
    }

    async fn resolve_person(
        &self,
        identity: Option<crate::person_context::IdentityTask>,
    ) -> Result<Option<crate::person_context::PersonScope>, ()> {
        let Some(personal) = &self.conversation.personal_context else {
            return Ok(None);
        };
        let (scope, verified) = match identity {
            Some(task) => match task.finish_outcome().await {
                crate::person_context::IdentityOutcome::Resolved(Some(scope)) => (Some(scope), true),
                crate::person_context::IdentityOutcome::Resolved(None)
                | crate::person_context::IdentityOutcome::SkippedShortDuration
                | crate::person_context::IdentityOutcome::Denied => (None, false),
            },
            None => (None, false),
        };
        if !personal.permitted(&self.context, scope.as_ref()) {
            return Err(());
        }
        self.send(TurnEvent::PersonResolved {
            scope: scope.clone(),
            verified,
        });
        Ok(scope)
    }

    async fn fast_answer(
        &self,
        text: &str,
        identity: &mut Option<crate::person_context::IdentityTask>,
        personal_prefix: &mut Vec<ChatMessage>,
    ) -> bool {
        let stop = crate::fast::is_stop(text);
        let read = self
            .conversation
            .settings
            .read_only_shortcuts
            .then(|| crate::fast::read_intent(text))
            .flatten();
        if !stop && read.is_none() {
            return false;
        }
        if stop {
            let _ = identity.take();
        } else if self.resolve_person(identity.take()).await.is_err() {
            return true;
        }
        self.timing("fast_route");
        let reply = if stop {
            let result = self
                .fixed_tool(
                    "laser_safe_action_tool",
                    serde_json::json!({"action":"emergency_stop","confirmed":true}),
                )
                .await;
            if result.success {
                STOP_OK_SPEECH.into()
            } else {
                STOP_ERR_SPEECH.into()
            }
        } else {
            self.read_reply(read.unwrap()).await
        };
        if stop {
            self.speak_fixed(&reply).await;
        } else if !self.speak(reply.clone()).await {
            self.send(TurnEvent::TextReply(reply.clone()));
        }
        let mut done_msgs = if self.conversation.personal_context.is_some() {
            std::mem::take(personal_prefix)
        } else {
            Vec::new()
        };
        done_msgs.push(ChatMessage::user(text));
        done_msgs.push(ChatMessage::assistant(reply));
        self.send(TurnEvent::Done {
            messages: done_msgs,
            close: false,
        });
        true
    }
    fn timing(&self, stage: &'static str) {
        if !self
            .measured
            .lock()
            .map(|mut stages| stages.insert(stage))
            .unwrap_or(false)
        {
            return;
        }
        self.send(TurnEvent::Timing {
            stage,
            ms: self.started.elapsed().as_millis() as u64,
        });
    }
    fn send(&self, event: TurnEvent) {
        let _ = self.events.send(event);
    }

    async fn speak_fixed(&self, text: &str) -> bool {
        self.timing("first_synthesis_start");
        match self.conversation.cached_speech(text).await {
            Ok(pcm) => {
                self.timing("first_synthesis_ready");
                self.send(TurnEvent::Speech {
                    text: text.to_owned(),
                    pcm,
                });
                true
            }
            Err(error) => {
                self.send(TurnEvent::Failed(error));
                self.send(TurnEvent::TextReply(text.to_owned()));
                false
            }
        }
    }

    /// 合成并送出一句话：第一段音频带文字（会话据此发 `sentence_start`），之后的分段直接追加。
    /// 返回是否送出过音频（中途失败时已送出的部分照常播放）。
    fn start_sentence_tts_with(
        &self,
        tts: Arc<dyn Tts>,
        text: String,
        record_timing: bool,
    ) -> PreparedSentence {
        if record_timing {
            self.timing("first_synthesis_start");
        }
        let (chunks_tx, chunks_rx) = unbounded_channel::<Pcm>();
        let input = text.clone();
        let handle = tokio::spawn(async move { tts.synthesize_stream(&input, chunks_tx).await });
        PreparedSentence {
            text,
            chunks_rx,
            handle: AbortOnDrop::new(handle),
        }
    }

    fn start_sentence_tts(&self, text: String) -> PreparedSentence {
        self.start_sentence_tts_with(self.conversation.tts.clone(), text, true)
    }

    /// 消费一句已启动的 TTS 音频流（首尾 12ms 线性淡入淡出 + 句尾冗余静音裁剪）；
    /// 若传入 `sentence_rx` 与 `lookahead`，则在等待当前句音频块的同时后台并发启动下一句 TTS 预取。
    async fn drain_prepared_sentence(
        &self,
        mut current: PreparedSentence,
        mut sentence_rx: Option<&mut tokio::sync::mpsc::Receiver<String>>,
        mut lookahead: Option<&mut Option<PreparedSentence>>,
        sentence_rx_closed: &mut bool,
        allow_fallback: bool,
    ) -> bool {
        let mut pending_text = Some(current.text);
        let mut held_tail: Vec<i16> = Vec::new();
        let mut sample_rate: u32 = 0;

        loop {
            let can_prefetch = sentence_rx.is_some()
                && lookahead.as_ref().is_some_and(|slot| slot.is_none())
                && !*sentence_rx_closed;
            let maybe_pcm = if can_prefetch {
                let rx = sentence_rx.as_deref_mut().unwrap();
                tokio::select! {
                    biased;
                    pcm = current.chunks_rx.recv() => pcm,
                    next = rx.recv() => {
                        match next {
                            Some(next_sentence) => {
                                if let Some(slot) = lookahead.as_deref_mut() {
                                    *slot = Some(self.start_sentence_tts(next_sentence));
                                }
                            }
                            None => {
                                *sentence_rx_closed = true;
                            }
                        }
                        continue;
                    }
                }
            } else {
                current.chunks_rx.recv().await
            };

            let Some(mut pcm) = maybe_pcm else {
                break;
            };
            if pcm.samples.is_empty() {
                continue;
            }
            sample_rate = pcm.sample_rate;
            crate::audio::trim_trailing_silence_i16(
                &mut pcm.samples,
                sample_rate,
                crate::audio::TRAILING_SILENCE_KEEP_MS,
                crate::audio::TRAILING_SILENCE_TRIGGER_MS,
            );
            if pcm.samples.is_empty() {
                continue;
            }
            if pending_text.is_some() && held_tail.is_empty() {
                crate::audio::fade_head_i16(&mut pcm.samples, sample_rate, crate::audio::BOUNDARY_FADE_MS);
            }
            let hold_samples = ((u64::from(sample_rate) * u64::from(crate::audio::BOUNDARY_FADE_MS)) / 1000) as usize;
            if hold_samples > 1 {
                let mut combined = if held_tail.is_empty() {
                    pcm.samples
                } else {
                    let mut buf = std::mem::take(&mut held_tail);
                    buf.extend_from_slice(&pcm.samples);
                    buf
                };
                if combined.len() <= hold_samples {
                    held_tail = combined;
                    continue;
                }
                let split_at = combined.len() - hold_samples;
                held_tail = combined.split_off(split_at);
                let emit_pcm = Pcm {
                    samples: combined,
                    sample_rate,
                };
                match pending_text.take() {
                    Some(text) => {
                        self.timing("first_synthesis_ready");
                        self.send(TurnEvent::Speech { text, pcm: emit_pcm });
                    }
                    None => self.send(TurnEvent::SpeechMore(emit_pcm)),
                }
            } else {
                match pending_text.take() {
                    Some(text) => {
                        self.timing("first_synthesis_ready");
                        self.send(TurnEvent::Speech { text, pcm });
                    }
                    None => self.send(TurnEvent::SpeechMore(pcm)),
                }
            }
        }

        if !held_tail.is_empty() && sample_rate > 0 {
            if pending_text.is_some() {
                crate::audio::fade_head_i16(&mut held_tail, sample_rate, crate::audio::BOUNDARY_FADE_MS);
            }
            crate::audio::fade_tail_i16(&mut held_tail, sample_rate, crate::audio::BOUNDARY_FADE_MS);
            let tail_pcm = Pcm {
                samples: held_tail,
                sample_rate,
            };
            match pending_text.take() {
                Some(text) => {
                    self.timing("first_synthesis_ready");
                    self.send(TurnEvent::Speech { text, pcm: tail_pcm });
                }
                None => self.send(TurnEvent::SpeechMore(tail_pcm)),
            }
        }

        if let Some(Ok(Err(error))) = current.handle.join().await {
            if allow_fallback
                && let Some(fallback) = self.conversation.fallback_tts.clone()
                && let Some(retry_text) = pending_text.take()
            {
                self.send(TurnEvent::Failed(format!(
                    "主语音合成失败（{error}），本句已自动降级为本地语音合成"
                )));
                let fb_prepared = self.start_sentence_tts_with(fallback, retry_text, false);
                let mut fb_closed = true;
                return Box::pin(self.drain_prepared_sentence(
                    fb_prepared,
                    None,
                    None,
                    &mut fb_closed,
                    false,
                ))
                .await;
            }
            self.send(TurnEvent::Failed(error.to_string()));
        }
        pending_text.is_none()
    }

    async fn speak(&self, text: String) -> bool {
        let prepared = self.start_sentence_tts(text);
        let mut closed = true;
        self.drain_prepared_sentence(prepared, None, None, &mut closed, true)
            .await
    }

    /// 识别用户的话：有流式识别时取它的结果，失败则用整段音频再识别一次。
    async fn transcribe(
        &self,
        pcm: &[i16],
        sample_rate: u32,
        stream: Option<AsrStream>,
    ) -> Result<String, String> {
        if let Some(stream) = stream {
            match stream.finish().await {
                Ok(text) => return Ok(text),
                Err(error) => self.send(TurnEvent::Failed(format!(
                    "流式识别失败，改用整段识别：{error}"
                ))),
            }
        }
        self.conversation
            .asr
            .transcribe(pcm, sample_rate)
            .await
            .map_err(|error| error.to_string())
    }

    /// 让模型生成一轮回复，边生成边切句合成。返回（回复，是否已朗读过内容）或错误。
    async fn generate(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        emotion_sent: &mut bool,
    ) -> (Result<moss_voice_providers::LlmReply, String>, bool) {
        let (delta_tx, mut delta_rx) = unbounded_channel::<String>();
        let llm = self.conversation.llm.clone();
        let generation = async move {
            llm.chat(messages, tools, delta_tx)
                .await
                .map_err(|error| error.to_string())
        };
        let speaking = async {
            let (sentence_tx, mut sentence_rx) = tokio::sync::mpsc::channel::<String>(4);
            let feeder = async {
                let sentence_tx = sentence_tx;
                let mut segmenter = Segmenter::new();
                while let Some(delta) = delta_rx.recv().await {
                    self.timing("first_llm_delta");
                    if !*emotion_sent && let Some(emotion) = emotion_of(&delta) {
                        *emotion_sent = true;
                        self.send(TurnEvent::Emotion(emotion));
                    }
                    for sentence in segmenter.push(&delta, Instant::now().into_std()) {
                        self.timing("first_sentence");
                        if sentence_tx.send(sentence).await.is_err() {
                            return;
                        }
                    }
                }
                if let Some(rest) = segmenter.finish() {
                    self.timing("first_sentence");
                    let _ = sentence_tx.send(rest).await;
                }
            };
            let tts_consumer = async {
                let mut spoke = false;
                let mut lookahead: Option<PreparedSentence> = None;
                let mut rx_closed = false;
                loop {
                    let next_prepared = if let Some(prefetched) = lookahead.take() {
                        Some(prefetched)
                    } else if rx_closed {
                        None
                    } else {
                        match sentence_rx.recv().await {
                            Some(sentence) => Some(self.start_sentence_tts(sentence)),
                            None => {
                                rx_closed = true;
                                None
                            }
                        }
                    };
                    let Some(current) = next_prepared else {
                        break;
                    };
                    spoke |= self
                        .drain_prepared_sentence(
                            current,
                            Some(&mut sentence_rx),
                            Some(&mut lookahead),
                            &mut rx_closed,
                            true,
                        )
                        .await;
                }
                spoke
            };
            let (_, spoke) = tokio::join!(feeder, tts_consumer);
            spoke
        };
        tokio::join!(generation, speaking)
    }

    async fn run_tools(&self, calls: &[ToolCall]) -> Vec<(ToolCall, ToolOutcome)> {
        let mut set = tokio::task::JoinSet::new();
        for (index, call) in calls.iter().cloned().enumerate() {
            let tools = self.conversation.tools.clone();
            let device = self.device.clone();
            let context = self.context.clone();
            let timeout = self.conversation.settings.tool_timeout;
            let events = self.events.clone();
            set.spawn(async move {
                let began = Instant::now();
                let work = async {
                    match device
                        .as_ref()
                        .filter(|_| call.function.name != "play_music")
                        .and_then(|device| {
                            device
                                .original(&call.function.name)
                                .map(|name| (device, name))
                        }) {
                        Some((device, original)) => {
                            device.call(original, &call.function.arguments).await
                        }
                        None => {
                            tools
                                .call(&context, &call.function.name, &call.function.arguments)
                                .await
                        }
                    }
                };
                let outcome = tokio::time::timeout(timeout, work)
                    .await
                    .unwrap_or_else(|_| {
                        ToolOutcome::Reply(format!("工具 {} 执行超时", call.function.name))
                    });
                let _ = events.send(TurnEvent::ToolTiming {
                    name: call.function.name.clone(),
                    ms: began.elapsed().as_millis() as u64,
                });
                let out_preview = match &outcome {
                    ToolOutcome::Reply(s) | ToolOutcome::Speak(s) => s.chars().take(240).collect(),
                    ToolOutcome::PlayMusic { title, .. } => format!("播放本地音乐：{title}"),
                };
                let _ = events.send(TurnEvent::ToolDebug {
                    name: call.function.name.clone(),
                    arguments: call.function.arguments.chars().take(240).collect(),
                    result: out_preview,
                });
                (index, call.id, outcome)
            });
        }
        let mut outcomes: Vec<Option<ToolOutcome>> = vec![None; calls.len()];
        while let Some(joined) = set.join_next().await {
            if let Ok((index, _, outcome)) = joined {
                outcomes[index] = Some(outcome);
            }
        }
        // 工具任务 panic 时没有结果：补“执行失败”，保证每个调用都有对应结果（否则模型接口会拒绝下一轮请求）
        calls
            .iter()
            .cloned()
            .zip(outcomes)
            .map(|(call, outcome)| {
                let outcome = outcome.unwrap_or_else(|| {
                    ToolOutcome::Reply(format!("工具 {} 执行失败", call.function.name))
                });
                (call, outcome)
            })
            .collect()
    }

    async fn answer(
        &self,
        pcm: Vec<i16>,
        sample_rate: u32,
        stream: Option<AsrStream>,
        mut history: Vec<ChatMessage>,
    ) {
        let settings = &self.conversation.settings;
        let mut identity = self
            .conversation
            .personal_context
            .as_ref()
            .map(|context| context.start(&self.context, &pcm, sample_rate));
        let text = match self.transcribe(&pcm, sample_rate, stream).await {
            Ok(text) => text,
            Err(error) => {
                self.send(TurnEvent::Failed(error));
                self.speak_fixed(&settings.error_reply).await;
                self.send(TurnEvent::Done {
                    messages: Vec::new(),
                    close: false,
                });
                return;
            }
        };
        self.timing("final_asr");
        let raw_text = text;
        let text = apply_asr_corrections(&raw_text, &settings.asr_corrections);
        if !moss_voice_providers::text::speakable(&text) || crate::fast::is_noise_filler(&text) {
            self.send(TurnEvent::Ignored);
            return;
        }
        self.send(TurnEvent::RawHeard {
            raw: raw_text,
            corrected: text.clone(),
        });
        self.send(TurnEvent::Heard(text.clone()));
        if self.fast_answer(&text, &mut identity, &mut history).await {
            return;
        }
        if self.conversation.is_exit(&text) {
            self.send(TurnEvent::Closing);
            self.speak_fixed(&settings.goodbye).await;
            self.send(TurnEvent::Done {
                messages: Vec::new(),
                close: true,
            });
            return;
        }
        // Stop and exit shortcuts above never wait for speaker work. Unknown
        // identity has no personal history or fallback device summary.
        let Ok(scope) = self.resolve_person(identity).await else {
            return;
        };
        let (history, personal_turn_prefix) = if let Some(personal) = &self.conversation.personal_context {
            (personal.history(&self.context, scope.as_ref()), history)
        } else {
            (history, Vec::new())
        };
        let mut tools = self.conversation.tools.tools();
        if let Some(device) = &self.device {
            // Reserved play_music is always owned by the trusted desktop index,
            // never by an untrusted device MCP advertisement.
            tools.extend(
                device
                    .specs()
                    .iter()
                    .filter(|spec| spec.name != "play_music")
                    .cloned(),
            );
        }
        let mut messages = Vec::with_capacity(history.len() + 4);
        messages.push(ChatMessage::system(build_turn_system_prompt(
            &self.conversation.system_prompt(),
            settings.include_local_clock,
            !history.is_empty(),
        )));

        // n57: once structured memory retrieval is on, the old NV10 per-device summary (imported
        // as candidates when the switch was turned on) is no longer injected next to it.
        let nv10_summary = !self
            .conversation
            .context_retrieval
            .as_ref()
            .is_some_and(|retrieval| retrieval.retriever.enabled());
        let active_jev = self
            .conversation
            .jev_filter
            .as_ref()
            .filter(|f| f.options().active());

        if let Some(jev) = active_jev {
            // 先取本地 SQLite 粗召回候选（~2ms，受 FAST_TIMEOUT 约束），使同一次 Jev 扇出顺手完成记忆撞词过滤；
            // 同时与个人/设备摘要读取并发执行，不产生额外串行等待。
            let retrieved = match &self.conversation.context_retrieval {
                Some(retrieval) => match retrieval.access.access(&self.context, scope.as_ref()) {
                    Some((access, device)) => {
                        retrieval.retriever.retrieve_candidates(access, &text, device).await
                    }
                    None => None,
                },
                None => None,
            };
            let mem_texts = retrieved
                .as_ref()
                .map(|r| r.candidate_texts())
                .unwrap_or_default();
            let summary_fut = async {
                if let Some(personal) = &self.conversation.personal_context {
                    personal.prompt_for(&self.context, scope.as_ref()).await
                } else if let Some(memory) = self.conversation.memory.as_ref().filter(|_| nv10_summary) {
                    memory.prompt_for(&self.context.device_id).await
                } else {
                    None
                }
            };
            let jev_fut = jev.evaluate_turn(&text, &history, &tools, &mem_texts);
            let (summary_block, decision) = tokio::join!(summary_fut, jev_fut);
            if decision.applied {
                self.timing("jev_filter");
            }
            if settings.read_only_shortcuts
                && let Some(intent) = decision.fast_read
            {
                self.timing("fast_route");
                let reply = self.read_reply(intent).await;
                if !self.speak(reply.clone()).await {
                    self.send(TurnEvent::TextReply(reply.clone()));
                }
                let mut done_msgs = personal_turn_prefix;
                done_msgs.push(ChatMessage::user(text));
                done_msgs.push(ChatMessage::assistant(reply));
                self.send(TurnEvent::Done {
                    messages: done_msgs,
                    close: false,
                });
                return;
            }
            tools = decision.filter_tools(&tools, &text, &history);
            if let Some(context) = summary_block {
                messages.push(ChatMessage::system(context));
            }
            if let Some(candidates) = retrieved
                && let Some(context) = candidates.render(decision.keep_memories.as_deref())
            {
                messages.push(ChatMessage::system(context));
            }
        } else {
            if let Some(personal) = &self.conversation.personal_context {
                if let Some(context) = personal.prompt_for(&self.context, scope.as_ref()).await {
                    messages.push(ChatMessage::system(context));
                }
            } else if let Some(memory) = self.conversation.memory.as_ref().filter(|_| nv10_summary) {
                if let Some(context) = memory.prompt_for(&self.context.device_id).await {
                    messages.push(ChatMessage::system(context));
                }
            }
            // NV19-04: bounded, labelled read-only data; skipped for small talk; never blocks
            // beyond its own short timeout and adds nothing on any failure.
            if let Some(retrieval) = &self.conversation.context_retrieval {
                if let Some((access, device)) =
                    retrieval.access.access(&self.context, scope.as_ref())
                {
                    if let Some(context) =
                        retrieval.retriever.prompt_for(access, &text, device).await
                    {
                        messages.push(ChatMessage::system(context));
                    }
                }
            }
        }
        messages.extend(compact_stale_tool_results(&history, 2));
        let first_new = messages.len();
        messages.extend(personal_turn_prefix);
        messages.push(ChatMessage::user(text));
        let mut emotion_sent = false;
        let mut spoke_any = false;
        let mut filler_spoken = false;
        let mut rounds = 0;
        loop {
            if self.conversation.personal_context.as_ref().is_some_and(|personal|!personal.permitted(&self.context,scope.as_ref())) {
                self.send(TurnEvent::Done{messages:vec![],close:false});return;
            }
            let (reply, spoke) = self.generate(&messages, &tools, &mut emotion_sent).await;
            spoke_any |= spoke;
            let reply = match reply {
                Ok(reply) => reply,
                Err(error) => {
                    self.send(TurnEvent::Failed(error));
                    if !spoke_any {
                        self.speak_fixed(&settings.error_reply).await;
                    }
                    break;
                }
            };
            if reply.tool_calls.is_empty() {
                messages.push(ChatMessage::assistant(reply.text));
                break;
            }
            rounds += 1;
            messages.push(ChatMessage::assistant_tools(
                Some(reply.text),
                reply.tool_calls.clone(),
            ));
            if rounds > settings.max_tool_rounds {
                // 不执行这一轮的工具；补上结果，保证历史中每个调用都有对应结果
                for call in &reply.tool_calls {
                    messages.push(ChatMessage::tool(
                        call.id.clone(),
                        "工具调用轮数超过上限，未执行",
                    ));
                }
                self.send(TurnEvent::Failed(format!(
                    "工具调用超过 {} 轮，已停止",
                    settings.max_tool_rounds
                )));
                self.speak_fixed(&settings.error_reply).await;
                break;
            }
            // Never mix a music playback request with any other tool in the
            // same model turn; other tools could have side effects.
            if reply
                .tool_calls
                .iter()
                .any(|call| call.function.name == "play_music")
                && reply.tool_calls.len() != 1
            {
                self.send(TurnEvent::Failed("音乐播放不能和其他工具同时执行".into()));
                self.send(TurnEvent::Done {
                    messages: Vec::new(),
                    close: false,
                });
                return;
            }
            let mut direct = Vec::new();
            let mut music = None;
            let tool_outcomes = if !spoke_any
                && !filler_spoken
                && rounds == 1
                && let Some(filler) = slow_tool_filler(&reply.tool_calls)
            {
                filler_spoken = true;
                let (_, outcomes) = tokio::join!(
                    self.speak_fixed(filler),
                    self.run_tools(&reply.tool_calls),
                );
                outcomes
            } else {
                self.run_tools(&reply.tool_calls).await
            };
            for (call, outcome) in tool_outcomes {
                let content = match outcome {
                    ToolOutcome::Reply(content) => content,
                    ToolOutcome::Speak(content) => {
                        direct.push(content.clone());
                        content
                    }
                    ToolOutcome::PlayMusic { track_id, title } => {
                        music = Some((track_id, title));
                        "歌曲已由设备会话接管，等待开始播放".into()
                    }
                };
                messages.push(ChatMessage::tool(call.id, content));
            }
            if let Some((track_id, title)) = music {
                self.send(TurnEvent::PlayMusic { track_id, title });
                // Music is an independent playback source, not a spoken LLM answer.
                // Do not copy tool parameters/paths into persistent dialog history.
                self.send(TurnEvent::Done {
                    messages: Vec::new(),
                    close: false,
                });
                return;
            }
            if !direct.is_empty() {
                let spoken = direct.join("。");
                for sentence in direct {
                    self.speak_fixed(&sentence).await;
                }
                messages.push(ChatMessage::assistant(spoken));
                break;
            }
        }
        self.send(TurnEvent::Done {
            messages: messages.split_off(first_new),
            close: false,
        });
    }
}

/// 没有在途任务时，用最近一条记录回答“上一个任务怎么样了”。
/// 只说记录里已有的行数，不编造物理加工进度或剩余时间。
fn terminal_reply(record: &Value) -> String {
    let send = &record["send_result"];
    let sent = send["lines_sent"].as_u64();
    let total = send["total_lines"].as_u64();
    match record["status"].as_str().unwrap_or("") {
        "completed" => match total {
            Some(total) if total > 0 => format!("上一个任务已经完成，共 {total} 行。"),
            _ => "上一个任务已经完成。".into(),
        },
        "cancelled" => match sent {
            Some(sent) if sent > 0 => format!("上一个任务已经取消，停在第 {sent} 行。"),
            _ => "上一个任务已经取消。".into(),
        },
        "failed" => match sent {
            Some(sent) if sent > 0 => {
                format!("上一个任务失败了，停在第 {sent} 行，请检查设备。")
            }
            _ => "上一个任务失败了，请检查设备。".into(),
        },
        "preview_ready" | "preview" | "planned" | "confirmed" => {
            "现在没有正在加工的任务，最近一条任务还没开始发送。".into()
        }
        _ => "现在没有正在加工的任务。".into(),
    }
}

/// 启动一轮对话。返回的任务句柄由会话持有，打断时中止。
pub fn spawn_turn(
    conversation: Arc<Conversation>,
    context: ToolContext,
    input: TurnInput,
    history: Vec<ChatMessage>,
    device: Option<DeviceTools>,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::UnboundedReceiver<TurnEvent>,
) {
    spawn_turn_with_person_hint(conversation, context, input, history, device, None)
}

pub fn spawn_turn_with_person_hint(
    conversation: Arc<Conversation>,
    context: ToolContext,
    input: TurnInput,
    history: Vec<ChatMessage>,
    device: Option<DeviceTools>,
    session_person_hint: Option<crate::person_context::PersonScope>,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::UnboundedReceiver<TurnEvent>,
) {
    let (events, receiver) = unbounded_channel();
    let turn = Turn {
        conversation,
        context,
        events,
        device,
        _session_person_hint: session_person_hint,
        started: Instant::now(),
        measured: Mutex::default(),
    };
    let handle = tokio::spawn(async move {
        match input {
            TurnInput::FastStop { text } => {
                let mut no_identity = None;
                let mut no_prefix = Vec::new();
                let corrected = apply_asr_corrections(&text, &turn.conversation.settings.asr_corrections);
                turn.send(TurnEvent::RawHeard {
                    raw: text,
                    corrected: corrected.clone(),
                });
                turn.send(TurnEvent::Heard(corrected.clone()));
                turn.fast_answer(&corrected, &mut no_identity, &mut no_prefix).await;
            }
            TurnInput::Utterance {
                pcm,
                sample_rate,
                stream,
            } => turn.answer(pcm, sample_rate, stream, history).await,
            TurnInput::Say { text, close } => {
                turn.speak_fixed(&text).await;
                turn.send(TurnEvent::Done {
                    messages: Vec::new(),
                    close,
                });
            }
        }
    });
    (handle, receiver)
}

/// 把一轮的新消息追加到历史并截断。
pub fn append_history(history: &mut Vec<ChatMessage>, messages: Vec<ChatMessage>, budget: usize) {
    history.extend(messages);
    truncate_history(history, budget);
}
