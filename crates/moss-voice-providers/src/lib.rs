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

pub use history::{
    ChatMessage, FunctionCall, Role, ToolCall, ToolSpec, compact_stale_tool_results,
    truncate_history,
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 诊断分类（闭集，2026-11-07 技术书 n68）：语音中心“记录与诊断”按它给每条事件和每个提供方错误
/// 贴上“是哪一类问题”的标签，前端在 `voiceState.js` 的 `VOICE_DIAG_CATEGORIES` 里映射成大白话原因与下一步。
///
/// - `general`：一般流程（连接、识别完成、播放完成等正常记录）
/// - `network`：网络（连不上、超时、域名解析失败、TLS 证书、连接中途断开）
/// - `auth`：密钥或权限（HTTP 401 / 403、密钥缺失）
/// - `quota`：额度（HTTP 402 余额不足、HTTP 429 太频繁或额度用完）
/// - `config`：配置（地址、模型名、参数填写不对，HTTP 400 / 404 / 重定向）
/// - `device`：小智设备（断线、握手超时、设备工具调用失败、授权）
/// - `firmware`：固件与资源包（OTA、固件下载、唤醒词资源包）
/// - `model`：模型服务本身（云端 5xx、返回内容异常、本地模型程序出错）
/// - `audio`：声音处理（解码、判停、播放、音乐）
/// - `internal`：MOSS 程序内部（锁、线程、数据库、序列化）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagCategory {
    #[default]
    General,
    Network,
    Auth,
    Quota,
    Config,
    Device,
    Firmware,
    Model,
    Audio,
    Internal,
}

impl DiagCategory {
    pub const ALL: [DiagCategory; 10] = [
        Self::General,
        Self::Network,
        Self::Auth,
        Self::Quota,
        Self::Config,
        Self::Device,
        Self::Firmware,
        Self::Model,
        Self::Audio,
        Self::Internal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Network => "network",
            Self::Auth => "auth",
            Self::Quota => "quota",
            Self::Config => "config",
            Self::Device => "device",
            Self::Firmware => "firmware",
            Self::Model => "model",
            Self::Audio => "audio",
            Self::Internal => "internal",
        }
    }
}

/// 提供方错误：给用户看的中文原因（`message`），不含地址与密钥。
///
/// 2026-11-07（n68）：另带诊断分类 `category`、可选的下一步建议 `hint`（大白话）与技术细节 `detail`
/// （底层错误或服务方原话，已经 [`scrub_diagnostic_text`] 去掉密钥、带凭据的地址与 IP）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub message: String,
    pub category: DiagCategory,
    pub hint: Option<String>,
    pub detail: Option<String>,
}

impl ProviderError {
    /// 默认归为“模型服务”类；能判断具体原因时改用 [`Self::with_category`]。
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), category: DiagCategory::Model, hint: None, detail: None }
    }

    pub fn with_category(category: DiagCategory, message: impl Into<String>) -> Self {
        Self { category, ..Self::new(message) }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        let hint = hint.into();
        self.hint = (!hint.trim().is_empty()).then_some(hint);
        self
    }

    /// 技术细节：写入前统一脱敏，空串不记。
    pub fn detail(mut self, detail: impl AsRef<str>) -> Self {
        let detail = scrub_diagnostic_text(detail.as_ref());
        self.detail = (!detail.trim().is_empty()).then_some(detail);
        self
    }

    /// 在原因前加一段说明（如“流式识别失败，改用整段识别：”），分类、建议与细节保持不变。
    pub fn prefixed(mut self, prefix: &str) -> Self {
        self.message = format!("{prefix}{}", self.message);
        self
    }
}

impl From<String> for ProviderError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

/// Display 只输出原因；有技术细节时附在括号里，不丢信息。
impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some(detail) = &self.detail {
            write!(f, "（原始信息：{detail}）")?;
        }
        Ok(())
    }
}

/// 诊断文字脱敏（记录与诊断、诊断包共用）：疑似密钥的词、`token=` 一类参数、`Bearer` 后的值、
/// 地址里的账号密码与查询串替换为 `[REDACTED]`；IPv4 地址替换为 `<ip>`；最长保留 2000 字并标注截断。
pub fn scrub_diagnostic_text(raw: &str) -> String {
    const MAX_CHARS: usize = 2000;
    let mut out = String::with_capacity(raw.len().min(MAX_CHARS));
    let mut previous_bearer = false;
    for (index, token) in raw.split(' ').enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let lower = token.to_ascii_lowercase();
        let masked = if previous_bearer && !token.is_empty() {
            "[REDACTED]".to_owned()
        } else if lower.starts_with("sk-")
            || lower.contains("token=")
            || lower.contains("api_key")
            || lower.contains("apikey")
            || lower.contains("api-key")
            || lower.contains("password")
            || lower.contains("secret")
            || lower.contains("authorization")
        {
            "[REDACTED]".to_owned()
        } else if let Some(at) = lower.find("://") {
            mask_url(token, at)
        } else {
            mask_ipv4(token)
        };
        previous_bearer = lower == "bearer" || lower == "basic";
        out.push_str(&masked);
    }
    if out.chars().count() > MAX_CHARS {
        let mut cut: String = out.chars().take(MAX_CHARS).collect();
        cut.push_str("…（已截断）");
        return cut;
    }
    out
}

/// `scheme://user:pass@host:port/path?query` → 去掉账号密码与查询串，主机若是 IP 则替换为 `<ip>`。
fn mask_url(token: &str, scheme_end: usize) -> String {
    let (scheme, rest) = token.split_at(scheme_end + 3);
    let (rest, had_query) = match rest.find(['?', '#']) {
        Some(q) => (&rest[..q], true),
        None => (rest, false),
    };
    let (authority, path) = match rest.find('/') {
        Some(p) => (&rest[..p], &rest[p..]),
        None => (rest, ""),
    };
    let host = match authority.rfind('@') {
        Some(at) => format!("[REDACTED]@{}", mask_ipv4(&authority[at + 1..])),
        None => mask_ipv4(authority),
    };
    format!("{scheme}{host}{path}{}", if had_query { "?[REDACTED]" } else { "" })
}

/// 把文字里形如 `a.b.c.d`（每段 0..=255）的 IPv4 地址替换成 `<ip>`。
fn mask_ipv4(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        let boundary = index == 0 || !(chars[index - 1].is_ascii_digit() || chars[index - 1] == '.');
        if boundary && chars[index].is_ascii_digit() {
            let mut cursor = index;
            let mut parts = 0;
            let mut valid = true;
            loop {
                let start = cursor;
                while cursor < chars.len() && chars[cursor].is_ascii_digit() && cursor - start < 4 {
                    cursor += 1;
                }
                let digits: String = chars[start..cursor].iter().collect();
                if digits.is_empty() || digits.len() > 3 || digits.parse::<u16>().map_or(true, |v| v > 255) {
                    valid = false;
                    break;
                }
                parts += 1;
                if parts == 4 || cursor >= chars.len() || chars[cursor] != '.' {
                    break;
                }
                cursor += 1;
            }
            let ends_cleanly = cursor >= chars.len() || !(chars[cursor].is_ascii_digit() || chars[cursor] == '.' && cursor + 1 < chars.len() && chars[cursor + 1].is_ascii_digit());
            if valid && parts == 4 && ends_cleanly {
                out.push_str("<ip>");
                index = cursor;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
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

    /// 同步读取当前已到达的最新中间文字（消除 SpeechEnd 与 watch 通知的调度竞态）。
    pub fn latest_interim(&self) -> Option<String> {
        self.interim
            .as_ref()
            .map(|rx| rx.borrow().clone())
            .filter(|s| !s.is_empty())
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
    upstream_text(text).map(|short| format!("：{short}")).unwrap_or_default()
}

/// 服务方原话（截断到 120 字）；含地址或疑似密钥时不保留。
pub(crate) fn upstream_text(text: &str) -> Option<String> {
    let short: String = text.trim().chars().take(120).collect();
    if short.is_empty() || short.contains("http") || short.contains("sk-") || short.contains("wss:") {
        None
    } else {
        Some(short)
    }
}

#[cfg(test)]
mod diag_tests {
    use super::*;

    #[test]
    fn scrub_masks_secrets_url_credentials_and_ips_but_keeps_reason() {
        let raw = "tcp connect error 192.168.1.23:8000 Bearer abc.def token=xyz sk-live-1 https://user:pw@10.0.0.2:8443/v1/chat?key=k1 Connection refused (os error 61)";
        let out = scrub_diagnostic_text(raw);
        for leak in ["192.168.1.23", "abc.def", "xyz", "sk-live-1", "user:pw", "10.0.0.2", "k1"] {
            assert!(!out.contains(leak), "{leak} leaked: {out}");
        }
        assert!(out.contains("<ip>:8000"), "{out}");
        assert!(out.contains("https://[REDACTED]@<ip>:8443/v1/chat?[REDACTED]"), "{out}");
        assert!(out.contains("Connection refused (os error 61)"), "{out}");
        assert_eq!(scrub_diagnostic_text("固件 2.2.0 版本 1.2"), "固件 2.2.0 版本 1.2", "三段版本号不是 IP");
        let long = "字".repeat(2500);
        assert!(scrub_diagnostic_text(&long).ends_with("…（已截断）"), "超长时明确标注截断");
    }

    #[test]
    fn provider_error_keeps_category_hint_and_scrubbed_detail() {
        let error = ProviderError::with_category(DiagCategory::Auth, "大模型密钥不对")
            .hint("重新填写密钥")
            .detail("401 Unauthorized token=abc");
        assert_eq!(error.category, DiagCategory::Auth);
        assert_eq!(error.hint.as_deref(), Some("重新填写密钥"));
        assert!(error.detail.as_deref().unwrap().contains("[REDACTED]"));
        assert!(!error.to_string().contains("abc"));
        assert!(error.to_string().starts_with("大模型密钥不对"));
        assert_eq!(ProviderError::new("x").category, DiagCategory::Model);
        let json = serde_json::to_value(DiagCategory::ALL).unwrap();
        assert_eq!(json, serde_json::json!(["general", "network", "auth", "quota", "config", "device", "firmware", "model", "audio", "internal"]));
        for category in DiagCategory::ALL {
            assert_eq!(serde_json::to_value(category).unwrap(), category.as_str());
        }
    }
}
