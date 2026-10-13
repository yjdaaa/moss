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
    truncate_history,
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
    /// 只存提供方/模型名称，禁止地址与密钥。
    pub model_labels: Vec<String>,
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
            model_labels: Vec::new(),
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
    pub tools: Arc<dyn ToolExecutor>,
    pub settings: ConversationSettings,
    pub memory: Option<Arc<crate::memory::MemoryService>>,
    pub speaker_enrollment: Option<Arc<crate::speaker_enrollment::SpeakerEnrollment>>,
    pub personal_context:Option<Arc<crate::person_context::PersonContext>>,
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
            tools,
            settings,
            memory: None,
            speaker_enrollment: None,
            personal_context:None,
            cache: Mutex::new(Vec::new()),
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

    /// 启用流式识别（NV7）。
    pub fn with_streaming_asr(mut self, streaming_asr: Arc<dyn StreamingAsr>) -> Self {
        self.streaming_asr = Some(streaming_asr);
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
        let pcm = self
            .tts
            .synthesize(text)
            .await
            .map_err(|error| error.to_string())?;
        if let Ok(mut cache) = self.cache.lock() {
            cache.push((text.to_owned(), pcm.clone()));
        }
        Ok(pcm)
    }
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
    /// Trusted Rust-only identity; never a model or device JSON field.
    PersonResolved(Option<crate::person_context::PersonScope>),
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
            return "没有找到正在发送的加工任务，无法确定当前加工进度或材料。".into();
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

    async fn fast_answer(&self, text: &str) -> bool {
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
        self.timing("fast_route");
        let reply = if stop {
            let result = self
                .fixed_tool(
                    "laser_safe_action_tool",
                    serde_json::json!({"action":"emergency_stop","confirmed":true}),
                )
                .await;
            if result.success {
                "已发送停止请求，请确认设备状态。".into()
            } else {
                "停止请求发送失败，请使用设备急停按钮并检查连接。".into()
            }
        } else {
            self.read_reply(read.unwrap()).await
        };
        if stop {
            self.speak_fixed(&reply).await;
        } else if !self.speak(reply.clone()).await {
            self.send(TurnEvent::TextReply(reply.clone()));
        }
        self.send(TurnEvent::Done {
            messages: vec![ChatMessage::user(text), ChatMessage::assistant(reply)],
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
    async fn speak(&self, text: String) -> bool {
        self.timing("first_synthesis_start");
        let (chunks_tx, mut chunks_rx) = unbounded_channel::<Pcm>();
        let tts = self.conversation.tts.clone();
        let input = text.clone();
        let synthesis = async move { tts.synthesize_stream(&input, chunks_tx).await };
        let forwarding = async {
            let mut text = Some(text);
            while let Some(pcm) = chunks_rx.recv().await {
                if pcm.samples.is_empty() {
                    continue;
                }
                match text.take() {
                    Some(text) => {
                        self.timing("first_synthesis_ready");
                        self.send(TurnEvent::Speech { text, pcm });
                    }
                    None => self.send(TurnEvent::SpeechMore(pcm)),
                }
            }
            text.is_none()
        };
        let (result, spoke) = tokio::join!(synthesis, forwarding);
        if let Err(error) = result {
            self.send(TurnEvent::Failed(error.to_string()));
        }
        spoke
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
            let mut segmenter = Segmenter::new();
            let mut spoke = false;
            while let Some(delta) = delta_rx.recv().await {
                self.timing("first_llm_delta");
                if !*emotion_sent && let Some(emotion) = emotion_of(&delta) {
                    *emotion_sent = true;
                    self.send(TurnEvent::Emotion(emotion));
                }
                for sentence in segmenter.push(&delta, Instant::now().into_std()) {
                    self.timing("first_sentence");
                    spoke |= self.speak(sentence).await;
                }
            }
            if let Some(rest) = segmenter.finish() {
                self.timing("first_sentence");
                spoke |= self.speak(rest).await;
            }
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
        history: Vec<ChatMessage>,
    ) {
        let settings = &self.conversation.settings;
        let identity=self.conversation.personal_context.as_ref().map(|context|context.start(&self.context,&pcm,sample_rate));
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
        if !moss_voice_providers::text::speakable(&text) {
            self.send(TurnEvent::Ignored);
            return;
        }
        self.send(TurnEvent::Heard(text.clone()));
        if self.fast_answer(&text).await {
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
        let scope=match identity {Some(task)=>task.finish().await,None=>None};
        let history=if let Some(personal)=&self.conversation.personal_context {
            if !personal.permitted(&self.context,scope.as_ref()){return;}
            self.send(TurnEvent::PersonResolved(scope.clone()));
            personal.history(&self.context,scope.as_ref())
        }else{history};
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
        messages.push(ChatMessage::system(settings.system_prompt.clone()));
        if let Some(personal)=&self.conversation.personal_context {
            if let Some(context)=personal.prompt_for(&self.context,scope.as_ref()).await {messages.push(ChatMessage::system(context));}
        }else if let Some(memory) = &self.conversation.memory {
            if let Some(context) = memory.prompt_for(&self.context.device_id).await {
                messages.push(ChatMessage::system(context));
            }
        }
        messages.extend(history);
        let first_new = messages.len();
        messages.push(ChatMessage::user(text));
        let mut emotion_sent = false;
        let mut spoke_any = false;
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
            for (call, outcome) in self.run_tools(&reply.tool_calls).await {
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
                    self.speak(sentence).await;
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
    let (events, receiver) = unbounded_channel();
    let turn = Turn {
        conversation,
        context,
        events,
        device,
        started: Instant::now(),
        measured: Mutex::default(),
    };
    let handle = tokio::spawn(async move {
        match input {
            TurnInput::FastStop { text } => {
                turn.send(TurnEvent::Heard(text.clone()));
                turn.fast_answer(&text).await;
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
