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
mod history;
pub mod local;
pub mod openai;
pub mod resample;
pub mod speech_cpp;
pub mod text;
pub mod vision;
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
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
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
        Self {
            message: message.into(),
            category: DiagCategory::Model,
            hint: None,
            detail: None,
        }
    }

    pub fn with_category(category: DiagCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            ..Self::new(message)
        }
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

/// 诊断文字脱敏（记录与诊断、诊断包共用）：疑似密钥的词、`token=` / `key: val` 一类键值、
/// `Bearer` / `Basic` 及敏感标签词后跟随的值、地址里的账号密码与查询串替换为 `[REDACTED]`；
/// IPv4 地址替换为 `<ip>`；最长保留 2000 字并标注截断。
pub fn scrub_diagnostic_text(raw: &str) -> String {
    scrub_text_with_options(raw, true, 2000, true).0
}

/// 通用文本凭据脱敏：支持键值写法、空白分隔的标签值、URL userinfo/查询串/fragment、
/// `sk-` 密钥与可选 IPv4 掩码；返回 `(脱敏后文本, 是否超过 max_chars 被截断)`。
pub fn scrub_text_with_options(
    raw: &str,
    mask_ips: bool,
    max_chars: usize,
    append_ellipsis: bool,
) -> (String, bool) {
    let chars: Vec<char> = raw.chars().collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_ascii_lowercase()).collect();
    let mut out = String::with_capacity(raw.len().min(max_chars.max(64)));
    let mut index = 0;

    while index < chars.len() {
        if let Some((next_idx, replaced)) = try_mask_url(&chars, &lower, index, mask_ips) {
            out.push_str(&replaced);
            index = next_idx;
            continue;
        }
        if let Some((next_idx, replaced)) = try_mask_auth_scheme(&chars, &lower, index) {
            out.push_str(&replaced);
            index = next_idx;
            continue;
        }
        if let Some((next_idx, replaced)) = try_mask_kv_or_label(&chars, &lower, index) {
            out.push_str(&replaced);
            index = next_idx;
            continue;
        }
        if let Some(next_idx) = try_mask_sk_token(&chars, &lower, index) {
            out.push_str("[REDACTED]");
            index = next_idx;
            continue;
        }
        if mask_ips && let Some(next_idx) = try_mask_ipv4(&chars, index) {
            out.push_str("<ip>");
            index = next_idx;
            continue;
        }
        out.push(chars[index]);
        index += 1;
    }

    if max_chars > 0 && out.chars().count() > max_chars {
        let mut cut: String = out.chars().take(max_chars).collect();
        if append_ellipsis {
            cut.push_str("…（已截断）");
        }
        return (cut, true);
    }
    (out, false)
}

fn is_value_stop(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | ',' | '&' | '}' | ']' | ')' | ';')
}

fn is_already_redacted(val: &[char]) -> bool {
    val.iter().all(|c| *c == '*')
}

fn starts_with_ascii_slice(lower: &[char], index: usize, needle: &str) -> bool {
    let bytes = needle.as_bytes();
    if index + bytes.len() > lower.len() {
        return false;
    }
    lower[index..index + bytes.len()]
        .iter()
        .zip(bytes)
        .all(|(c, b)| *c == char::from(*b))
}

fn try_mask_url(
    chars: &[char],
    lower: &[char],
    index: usize,
    mask_ips: bool,
) -> Option<(usize, String)> {
    if index > 0 && chars[index - 1].is_ascii_alphanumeric() {
        return None;
    }
    let scheme = ["https://", "http://", "wss://", "ws://"]
        .into_iter()
        .find(|s| starts_with_ascii_slice(lower, index, s))?;
    let mut end = index + scheme.len();
    while end < chars.len() {
        // 已脱敏的 URL 中占位符是完整内容，不能把其括号误认成 URL 结束。
        if starts_with_ascii_slice(lower, end, "[redacted]") {
            end += "[redacted]".len();
        } else if starts_with_ascii_slice(lower, end, "<ip>") {
            end += "<ip>".len();
        } else if chars[end].is_whitespace()
            || matches!(
                chars[end],
                '"' | '\'' | '<' | '>' | ')' | ']' | '}' | ',' | ';'
            )
        {
            break;
        } else {
            end += 1;
        }
    }
    let raw_url: String = chars[index..end].iter().collect();
    Some((end, mask_url(&raw_url, scheme.len() - 3, mask_ips)))
}

fn try_mask_auth_scheme(chars: &[char], lower: &[char], index: usize) -> Option<(usize, String)> {
    if index > 0 && (chars[index - 1].is_ascii_alphanumeric() || chars[index - 1] == '_') {
        return None;
    }
    let scheme_len = if starts_with_ascii_slice(lower, index, "bearer") {
        6
    } else if starts_with_ascii_slice(lower, index, "basic") {
        5
    } else {
        return None;
    };
    let mut cursor = index + scheme_len;
    if cursor >= chars.len() {
        return None;
    }
    if matches!(chars[cursor], ':' | '=') {
        cursor += 1;
        while cursor < chars.len() && chars[cursor].is_whitespace() {
            cursor += 1;
        }
    } else if chars[cursor].is_whitespace() {
        while cursor < chars.len() && chars[cursor].is_whitespace() {
            cursor += 1;
        }
    } else {
        return None;
    }
    if cursor < chars.len() && matches!(chars[cursor], '"' | '\'' | '\\') {
        cursor += 1;
    }
    if starts_with_ascii_slice(lower, cursor, "[redacted]") {
        return None;
    }
    let value_start = cursor;
    while cursor < chars.len() && !is_value_stop(chars[cursor]) {
        cursor += 1;
    }
    if cursor == value_start || is_already_redacted(&chars[value_start..cursor]) {
        return None;
    }
    let mut replaced: String = chars[index..value_start].iter().collect();
    replaced.push_str("[REDACTED]");
    Some((cursor, replaced))
}

fn try_mask_kv_or_label(chars: &[char], lower: &[char], index: usize) -> Option<(usize, String)> {
    const STRONG_MARKERS: [&str; 12] = [
        "client_secret",
        "private_key",
        "access_token",
        "authorization",
        "api_key",
        "api-key",
        "apikey",
        "password",
        "passwd",
        "secret",
        "x-api-key",
        "token",
    ];
    let marker = STRONG_MARKERS
        .into_iter()
        .find(|m| starts_with_ascii_slice(lower, index, m))?;
    let after_marker = index + marker.len();
    if after_marker < chars.len()
        && (chars[after_marker].is_ascii_alphabetic() || chars[after_marker] == '_')
    {
        return None;
    }

    let mut cursor = after_marker;
    while cursor < chars.len() && matches!(chars[cursor], '"' | '\'' | '\\') {
        cursor += 1;
    }
    while cursor < chars.len() && matches!(chars[cursor], ' ' | '\t') {
        cursor += 1;
    }
    if cursor < chars.len() && matches!(chars[cursor], ':' | '=') {
        cursor += 1;
        while cursor < chars.len() && matches!(chars[cursor], ' ' | '\t' | '"' | '\'' | '\\') {
            cursor += 1;
        }
        return finish_label_mask(chars, lower, index, cursor);
    }

    // 空白分隔的敏感标签词 + 值（如 `Authorization Bearer fake_val`、`api_key fake_val`、`password fake_val`、`--token fake_val`）
    let had_ws = chars[after_marker..cursor]
        .iter()
        .any(|c| c.is_whitespace());
    if !had_ws || cursor >= chars.len() {
        return None;
    }
    if matches!(chars[cursor], '"' | '\'' | '\\') {
        cursor += 1;
    }
    finish_label_mask(chars, lower, index, cursor)
}

fn skip_optional_auth_scheme(chars: &[char], lower: &[char], mut cursor: usize) -> usize {
    for scheme in ["bearer", "basic"] {
        if starts_with_ascii_slice(lower, cursor, scheme) {
            let s_end = cursor + scheme.len();
            if s_end < chars.len() && chars[s_end].is_whitespace() {
                cursor = s_end;
                while cursor < chars.len() && matches!(chars[cursor], ' ' | '\t') {
                    cursor += 1;
                }
                if cursor < chars.len() && matches!(chars[cursor], '"' | '\'' | '\\') {
                    cursor += 1;
                }
                return cursor;
            }
        }
    }
    cursor
}

fn finish_label_mask(
    chars: &[char],
    lower: &[char],
    index: usize,
    cursor: usize,
) -> Option<(usize, String)> {
    let mut val_start = skip_optional_auth_scheme(chars, lower, cursor);
    if starts_with_ascii_slice(lower, val_start, "[redacted]") {
        return None;
    }
    let mut val_end = val_start;
    while val_end < chars.len()
        && !is_value_stop(chars[val_end])
        && !(chars[val_end] == '\\' && chars.get(val_end + 1) == Some(&'"'))
    {
        val_end += 1;
    }
    if val_end == val_start && val_start > cursor {
        // 形如 `Authorization: Bearer` 后面没有实际凭据值时，退回掩码 `Bearer` 本身或忽略
        val_start = cursor;
        val_end = val_start;
        while val_end < chars.len() && !is_value_stop(chars[val_end]) {
            val_end += 1;
        }
    }
    if val_end == val_start || is_already_redacted(&chars[val_start..val_end]) {
        return None;
    }
    let mut replaced: String = chars[index..val_start].iter().collect();
    replaced.push_str("[REDACTED]");
    Some((val_end, replaced))
}

fn try_mask_sk_token(chars: &[char], lower: &[char], index: usize) -> Option<usize> {
    if index > 0 && chars[index - 1].is_ascii_alphanumeric() {
        return None;
    }
    if !starts_with_ascii_slice(lower, index, "sk-") {
        return None;
    }
    let mut cursor = index + 3;
    if cursor >= chars.len() || is_value_stop(chars[cursor]) {
        return None;
    }
    while cursor < chars.len() && !is_value_stop(chars[cursor]) {
        cursor += 1;
    }
    Some(cursor)
}

fn try_mask_ipv4(chars: &[char], index: usize) -> Option<usize> {
    let boundary = index == 0 || !(chars[index - 1].is_ascii_digit() || chars[index - 1] == '.');
    if !boundary || !chars[index].is_ascii_digit() {
        return None;
    }
    let mut cursor = index;
    let mut parts = 0;
    loop {
        let start = cursor;
        while cursor < chars.len() && chars[cursor].is_ascii_digit() && cursor - start < 4 {
            cursor += 1;
        }
        let digits: String = chars[start..cursor].iter().collect();
        if digits.is_empty() || digits.len() > 3 || digits.parse::<u16>().map_or(true, |v| v > 255)
        {
            return None;
        }
        parts += 1;
        if parts == 4 || cursor >= chars.len() || chars[cursor] != '.' {
            break;
        }
        cursor += 1;
    }
    let ends_cleanly = cursor >= chars.len()
        || !(chars[cursor].is_ascii_digit()
            || chars[cursor] == '.'
                && cursor + 1 < chars.len()
                && chars[cursor + 1].is_ascii_digit());
    (parts == 4 && ends_cleanly).then_some(cursor)
}

/// `scheme://user:pass@host:port/path?query` → 去掉账号密码与查询串/fragment，主机若是 IP 且 `mask_ips` 为真则替换为 `<ip>`。
fn mask_url(token: &str, scheme_end: usize, mask_ips: bool) -> String {
    let (scheme, rest) = token.split_at(scheme_end + 3);
    let (rest, suffix) = match rest.find(['?', '#']) {
        Some(q) if rest[q..].starts_with('?') => (&rest[..q], "?[REDACTED]"),
        Some(q) => (&rest[..q], "#[REDACTED]"),
        None => (rest, ""),
    };
    let (authority, path) = match rest.find('/') {
        Some(p) => (&rest[..p], &rest[p..]),
        None => (rest, ""),
    };
    let maybe_mask_host = |h: &str| {
        if mask_ips { mask_ipv4(h) } else { h.to_owned() }
    };
    let host = match authority.rfind('@') {
        Some(at) => format!("[REDACTED]@{}", maybe_mask_host(&authority[at + 1..])),
        None => maybe_mask_host(authority),
    };
    format!("{scheme}{host}{path}{suffix}")
}

/// 把文字里形如 `a.b.c.d`（每段 0..=255）的 IPv4 地址替换成 `<ip>`。
fn mask_ipv4(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        if let Some(next_idx) = try_mask_ipv4(&chars, index) {
            out.push_str("<ip>");
            index = next_idx;
            continue;
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
    fn transcribe<'a>(
        &'a self,
        pcm: &'a [i16],
        sample_rate: u32,
    ) -> BoxFuture<'a, Result<String, ProviderError>>;
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
    /// 本地识别报告的这一路降噪情况（协议 8，完整过程记录用；云端识别为空）
    denoise: DenoiseSlot,
}

/// 一路识别的降噪情况（本地模型辅助程序打开识别流时填写）。
pub type DenoiseSlot = std::sync::Arc<std::sync::Mutex<Option<local::DenoiseReport>>>;

impl AsrStream {
    /// 由实现方构造：后台任务从 `audio` 读音频（发送端关闭即说完），把最终文字送到 `result`。
    pub fn new(
        audio: tokio::sync::mpsc::UnboundedSender<Vec<i16>>,
        result: tokio::sync::oneshot::Receiver<Result<String, ProviderError>>,
        task: tokio::task::JoinHandle<()>,
    ) -> Self {
        Self {
            audio: Some(audio),
            result,
            task,
            interim: None,
            denoise: DenoiseSlot::default(),
        }
    }

    pub fn with_denoise(mut self, denoise: DenoiseSlot) -> Self {
        self.denoise = denoise;
        self
    }

    /// 这一路的降噪情况（流打开后由实现方填写；在 [`Self::finish`] 之前取走句柄，结束后读）。
    pub fn denoise_slot(&self) -> DenoiseSlot {
        self.denoise.clone()
    }

    /// 取走这一路的降噪情况（只交一次：会话在第一个中间结果时取走，没取走的由对话任务在识别成功后取）。
    pub fn take_denoise(&self) -> Option<local::DenoiseReport> {
        self.denoise.lock().ok().and_then(|mut slot| slot.take())
    }

    pub fn with_interim(mut self, interim: tokio::sync::watch::Receiver<String>) -> Self {
        self.interim = Some(interim);
        self
    }

    /// 收音期间等待下一次中间结果；提供方没有此能力或已关闭时不忙轮询。
    pub async fn next_interim(&mut self) -> String {
        if let Some(interim) = &mut self.interim
            && interim.changed().await.is_ok()
        {
            return interim.borrow_and_update().clone();
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
        (&mut self.result)
            .await
            .unwrap_or_else(|_| Err(ProviderError::new("流式识别意外结束")))
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
    upstream_text(text)
        .map(|short| format!("：{short}"))
        .unwrap_or_default()
}

/// 服务方原话（截断到 120 字）；含地址或疑似密钥时不保留。
pub(crate) fn upstream_text(text: &str) -> Option<String> {
    let short: String = text.trim().chars().take(120).collect();
    if short.is_empty() || short.contains("http") || short.contains("sk-") || short.contains("wss:")
    {
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
        for leak in [
            "192.168.1.23",
            "abc.def",
            "xyz",
            "sk-live-1",
            "user:pw",
            "10.0.0.2",
            "k1",
        ] {
            assert!(!out.contains(leak), "{leak} leaked: {out}");
        }
        assert!(out.contains("<ip>:8000"), "{out}");
        assert!(
            out.contains("https://[REDACTED]@<ip>:8443/v1/chat?[REDACTED]"),
            "{out}"
        );
        assert!(out.contains("Connection refused (os error 61)"), "{out}");
        assert_eq!(
            scrub_diagnostic_text("固件 2.2.0 版本 1.2"),
            "固件 2.2.0 版本 1.2",
            "三段版本号不是 IP"
        );
        let long = "字".repeat(2500);
        assert!(
            scrub_diagnostic_text(&long).ends_with("…（已截断）"),
            "超长时明确标注截断"
        );
    }

    #[test]
    fn scrub_masks_space_separated_auth_kv_json_and_preserves_counter_words() {
        let cases = [
            (
                "Bearer fake_val",
                vec!["fake_val"],
                vec!["Bearer [REDACTED]"],
            ),
            (
                "Bearer   fake_val",
                vec!["fake_val"],
                vec!["Bearer   [REDACTED]"],
            ),
            (
                "Authorization Bearer fake_val",
                vec!["fake_val"],
                vec!["Authorization Bearer [REDACTED]"],
            ),
            (
                "Authorization: Bearer fake_val",
                vec!["fake_val"],
                vec!["Authorization: Bearer [REDACTED]"],
            ),
            (
                "api_key: fake_val api_key = fake_v2 api_key fake_v3",
                vec!["fake_val", "fake_v2", "fake_v3"],
                vec![
                    "api_key: [REDACTED]",
                    "api_key = [REDACTED]",
                    "api_key [REDACTED]",
                ],
            ),
            (
                "password fake_pw --token fake_tok",
                vec!["fake_pw", "fake_tok"],
                vec!["password [REDACTED]", "--token [REDACTED]"],
            ),
            (
                r#"{"token":"fake_json","material":"椴木"}"#,
                vec!["fake_json"],
                vec![r#"{"token":"[REDACTED]","material":"椴木"}"#],
            ),
            (
                "wss://fakeuser:fakepass@example.invalid/ws?k=fake1&token=fake2",
                vec!["fakeuser", "fakepass", "fake1", "fake2"],
                vec!["wss://[REDACTED]@example.invalid/ws?[REDACTED]"],
            ),
            (
                r#"{\"api_key\":\"abc123\",\"password\":\"x-secret\"}"#,
                vec!["abc123", "x-secret"],
                vec![r#"{\"api_key\":\"[REDACTED]\",\"password\":\"[REDACTED]\"}"#],
            ),
            (
                "dbPassword=hunter2 accessToken=abc authToken=ghi nestedSecret=jkl",
                vec!["hunter2", "abc", "ghi", "jkl"],
                vec![
                    "dbPassword=[REDACTED]",
                    "accessToken=[REDACTED]",
                    "authToken=[REDACTED]",
                    "nestedSecret=[REDACTED]",
                ],
            ),
            (
                "Basic already-basic https://test.invalid/?a=hidden",
                vec!["already-basic", "hidden"],
                vec!["Basic [REDACTED]", "https://test.invalid/?[REDACTED]"],
            ),
            (
                "authToken 测试凭据",
                vec!["测试凭据"],
                vec!["authToken [REDACTED]"],
            ),
            ("tokens_used: 120", vec![], vec!["tokens_used: 120"]),
        ];
        for (input, forbidden, expected) in cases {
            let out = scrub_text_with_options(input, false, 2000, false).0;
            for secret in &forbidden {
                assert!(!out.contains(secret), "{secret} leaked in {out}");
            }
            for needle in &expected {
                assert!(out.contains(needle), "expected {needle} in {out}");
            }
            assert_eq!(scrub_diagnostic_text(&out), out, "二次脱敏必须保持原样");
        }
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
        assert_eq!(
            json,
            serde_json::json!([
                "general", "network", "auth", "quota", "config", "device", "firmware", "model",
                "audio", "internal"
            ])
        );
        for category in DiagCategory::ALL {
            assert_eq!(serde_json::to_value(category).unwrap(), category.as_str());
        }
    }
}
