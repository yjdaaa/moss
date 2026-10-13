//! MOSS 内置语音服务的模型提供方（`docs/integration/moss-native-voice-spec.md` §5.1、§7，NV3 起）。
//!
//! - [`Asr`] / [`Llm`] / [`Tts`]：对话编排只依赖这三个接口，测试用假实现，NV6 / NV7 增加本地与流式厂商实现；
//! - [`openai`]：OpenAI 兼容的 `/audio/transcriptions`、`/chat/completions`（流式 + 工具调用）、`/audio/speech`；
//! - [`text`]：切句（首句遇逗号即送合成）、Markdown 与表情清洗、表情映射；
//! - [`history`]：对话消息与按长度预算截断（esp32-server 没有上限，这里补上）；
//! - [`wav`]：WAV 编解码（识别上传、合成结果解析）；
//! - [`local`]：本地模型辅助程序 `moss-voice-local` 的客户端（NV6：Silero 判停、SenseVoice 识别、Kokoro 合成）；
//! - [`doubao`]：豆包（火山引擎）流式识别与双向流式合成（NV7，协议移植自 univoice `021113f`，MIT）；
//! - [`resample`]：线性插值重采样（会话与豆包识别共用）。
//!
//! 错误信息只描述原因，**不含地址与密钥**（不使用 reqwest 的原始错误描述，它会带上完整地址）。

pub mod doubao;
pub mod history;
pub mod local;
pub mod vision;
pub mod openai;
pub mod resample;
pub mod text;
pub mod wav;

use std::fmt;
use std::future::Future;
use std::pin::Pin;

pub use history::{ChatMessage, FunctionCall, Role, ToolCall, ToolSpec, truncate_history};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 提供方错误：给用户看的中文原因，不含地址与密钥。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError(pub String);

impl ProviderError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProviderError {}

/// 单声道 16 位 PCM。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcm {
    pub samples: Vec<i16>,
    pub sample_rate: u32,
}

/// 语音识别：一段完整的话 → 文字。
pub trait Asr: Send + Sync {
    fn transcribe<'a>(&'a self, pcm: &'a [i16], sample_rate: u32) -> BoxFuture<'a, Result<String, ProviderError>>;
}

/// 流式识别（NV7）：用户开始说话时建立连接，边说边送音频，说完后只需等最后一包的结果。
pub trait StreamingAsr: Send + Sync {
    /// 开始一段识别（音频为 16 kHz 单声道）。立即返回；连接在后台建立，期间送入的音频先缓存。
    /// 必须在 tokio 运行时中调用。
    fn start(&self) -> AsrStream;
}

/// 一段进行中的流式识别。丢弃时中止后台任务（打断、重新拾音）。
pub struct AsrStream {
    audio: Option<tokio::sync::mpsc::UnboundedSender<Vec<i16>>>,
    result: tokio::sync::oneshot::Receiver<Result<String, ProviderError>>,
    task: tokio::task::JoinHandle<()>,
    /// 只保留最新中间文字，避免说话期间事件无界积压。
    interim: Option<tokio::sync::watch::Receiver<String>>,
}

impl AsrStream {
    /// 由实现方构造：后台任务从 `audio` 读音频（发送端关闭即说完），把最终文字送到 `result`。
    pub fn new(
        audio: tokio::sync::mpsc::UnboundedSender<Vec<i16>>,
        result: tokio::sync::oneshot::Receiver<Result<String, ProviderError>>,
        task: tokio::task::JoinHandle<()>,
    ) -> Self {
        Self { audio: Some(audio), result, task, interim: None }
    }

    pub fn with_interim(mut self, interim: tokio::sync::watch::Receiver<String>) -> Self {
        self.interim = Some(interim);
        self
    }

    /// 收音期间等待下一次中间结果；提供方没有此能力或已关闭时不忙轮询。
    pub async fn next_interim(&mut self) -> String {
        if let Some(interim) = &mut self.interim {
            if interim.changed().await.is_ok() {
                return interim.borrow_and_update().clone();
            }
        }
        std::future::pending().await
    }

    /// 送入一段 16 kHz 音频（不阻塞；连接失败后送入的音频被丢弃，错误在 [`Self::finish`] 返回）。
    pub fn feed(&self, pcm: &[i16]) {
        if let Some(audio) = &self.audio
            && !pcm.is_empty()
        {
            let _ = audio.send(pcm.to_vec());
        }
    }

    /// 说完：等待最终结果。
    pub async fn finish(mut self) -> Result<String, ProviderError> {
        self.audio = None;
        (&mut self.result).await.unwrap_or_else(|_| Err(ProviderError::new("流式识别意外结束")))
    }
}

impl Drop for AsrStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 大模型的一次回复：完整文字与工具调用。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlmReply {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
}

/// 大模型：流式生成，文字增量随到随发给 `deltas`，结束时返回完整回复。
pub trait Llm: Send + Sync {
    fn chat<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [ToolSpec],
        deltas: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> BoxFuture<'a, Result<LlmReply, ProviderError>>;
}

/// 语音合成：一句话 → PCM（采样率由提供方决定，调用方重采样）。
pub trait Tts: Send + Sync {
    fn synthesize<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>>;

    /// 流式合成（NV7）：音频分段到达时依次送给 `chunks`，第一段到达即可开始播放。
    /// 默认整句合成后一次送出；流式厂商覆盖此方法。
    fn synthesize_stream<'a>(
        &'a self,
        text: &'a str,
        chunks: tokio::sync::mpsc::UnboundedSender<Pcm>,
    ) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            let pcm = self.synthesize(text).await?;
            let _ = chunks.send(pcm);
            Ok(())
        })
    }
}

/// 服务端返回的错误说明：截断，含地址或疑似密钥时不回显（服务端可能把请求内容带回来）。
pub(crate) fn safe_hint(text: &str) -> String {
    let short: String = text.trim().chars().take(120).collect();
    if short.is_empty() || short.contains("http") || short.contains("sk-") || short.contains("wss:") {
        String::new()
    } else {
        format!("：{short}")
    }
}
