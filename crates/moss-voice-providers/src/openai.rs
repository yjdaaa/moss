//! OpenAI 兼容提供方：`/audio/transcriptions`（识别）、`/chat/completions`（流式 + 工具调用）、`/audio/speech`（合成）。
//!
//! 地址约定与语音中心 P4 的模型向导一致（`moss-voice::models`）：大模型填基础地址（如 `https://<host>/v1`），
//! 识别与合成填完整地址（填基础地址时自动补全）。所有请求不跟随重定向，连接超时 10 秒。

use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;

use crate::history::{ChatMessage, ToolCall, ToolSpec};
use crate::wav::{decode_wav, encode_wav};
use crate::{Asr, BoxFuture, Llm, LlmReply, Pcm, ProviderError, Tts};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 识别与合成单次请求的总超时
const AUDIO_TIMEOUT: Duration = Duration::from_secs(30);
/// 大模型：等响应头、以及流中两段数据之间的最长间隔
const LLM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// 大模型一次回复的总时长上限
const LLM_TOTAL_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_AUDIO_BYTES: usize = 20 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;

/// 大模型基础地址；误填了完整 `/chat/completions` 时去掉。
pub fn llm_base_url(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    trimmed.strip_suffix("/chat/completions").unwrap_or(trimmed).trim_end_matches('/').to_owned()
}

fn full_url(input: &str, suffix: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.ends_with(suffix) { trimmed.to_owned() } else { format!("{trimmed}{suffix}") }
}

pub fn asr_full_url(input: &str) -> String {
    full_url(input, "/audio/transcriptions")
}

pub fn tts_full_url(input: &str) -> String {
    full_url(input, "/audio/speech")
}

fn client() -> Result<reqwest::Client, ProviderError> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .tcp_nodelay(true)
                .pool_max_idle_per_host(8)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "无法创建网络客户端".to_owned())
        })
        .as_ref()
        .map(Clone::clone)
        .map_err(ProviderError::new)
}

/// 不使用 reqwest 的原始描述（它会带上完整地址）。
fn describe_send_error(label: &str, error: &reqwest::Error) -> ProviderError {
    ProviderError::new(if error.is_timeout() {
        format!("{label}请求超时")
    } else if error.is_connect() {
        format!("{label}无法连接到该地址")
    } else {
        format!("{label}请求失败（网络或 TLS 错误）")
    })
}

/// 从错误响应里取一段可读说明（只取 `error.message` 一类字段并截断；不回显整个响应）。
fn error_hint(bytes: &[u8]) -> String {
    let text = serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.pointer("/data/error/message"))
                .or_else(|| value.get("message"))
                .or_else(|| value.pointer("/data/message"))
                .or_else(|| value.get("error"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    // 防止服务端把密钥或地址回显在错误信息里
    crate::safe_hint(&text)
}

async fn read_body(label: &str, mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, ProviderError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| describe_send_error(label, &error))? {
        body.extend_from_slice(&chunk);
        if body.len() > limit {
            return Err(ProviderError::new(format!("{label}返回的数据过大")));
        }
    }
    Ok(body)
}

async fn check_status(label: &str, response: reqwest::Response) -> Result<reqwest::Response, ProviderError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = read_body(label, response, MAX_ERROR_BYTES).await.unwrap_or_default();
    let reason = match status.as_u16() {
        401 | 403 => "，密钥无效或没有权限",
        402 => "，账户余额不足",
        404 => "，地址或模型名不对",
        429 => "，请求太频繁或额度不足",
        300..=399 => "，服务要求跳转（MOSS 不跟随重定向，请填写最终地址）",
        _ => "",
    };
    Err(ProviderError::new(format!("{label}返回 HTTP {}{reason}{}", status.as_u16(), error_hint(&body))))
}

fn with_auth(builder: reqwest::RequestBuilder, api_key: &str) -> reqwest::RequestBuilder {
    if api_key.is_empty() { builder } else { builder.bearer_auth(api_key) }
}

fn is_cline_api_url(url: &str) -> bool {
    reqwest::Url::parse(url.trim())
        .ok()
        .and_then(|parsed| {
            parsed.host_str().map(|host| {
                let lower = host.to_ascii_lowercase();
                lower == "cline.bot" || lower.ends_with(".cline.bot")
            })
        })
        .unwrap_or(false)
}

fn with_llm_compat(builder: reqwest::RequestBuilder, url: &str, api_key: &str) -> reqwest::RequestBuilder {
    let builder = with_auth(builder, api_key);
    if is_cline_api_url(url) {
        builder
            .header("X-Response-Format", "openai")
            .header("HTTP-Referer", "https://cline.bot")
            .header("X-Title", "Cline")
            .header("X-IS-MULTIROOT", "false")
            .header("X-CLIENT-TYPE", "cline-desktop")
    } else {
        builder
    }
}

// ---------------------------------------------------------------- 识别

pub struct OpenAiAsr {
    prompt: Option<String>,
    client: reqwest::Client,
    url: String,
    api_key: String,
    model: String,
    language: Option<String>,
}

impl OpenAiAsr {
    pub fn new(url: &str, api_key: &str, model: &str, language: Option<&str>) -> Result<Self, ProviderError> {
        Ok(Self {
            prompt: None,
            client: client()?,
            url: asr_full_url(url),
            api_key: api_key.trim().to_owned(),
            model: model.trim().to_owned(),
            language: language.map(str::to_owned).filter(|language| !language.is_empty()),
        })
    }

    pub fn with_hotwords(mut self, words: &[String]) -> Self {
        let prompt = words.iter().take(64).map(|w| w.chars().filter(|c| !c.is_control()).take(32).collect::<String>()).collect::<Vec<_>>().join("、");
        self.prompt = (!prompt.is_empty()).then_some(prompt);
        self
    }

    fn multipart(&self, wav: &[u8], boundary: &str) -> Vec<u8> {
        let mut body = Vec::with_capacity(wav.len() + 512);
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"speech.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(wav);
        let mut field = |name: &str, value: &str| {
            body.extend_from_slice(
                format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}").as_bytes(),
            );
        };
        field("model", &self.model);
        field("response_format", "json");
        if let Some(prompt) = &self.prompt { field("prompt", prompt); }
        if let Some(language) = &self.language {
            field("language", language);
        }
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        body
    }
}

impl Asr for OpenAiAsr {
    fn transcribe<'a>(&'a self, pcm: &'a [i16], sample_rate: u32) -> BoxFuture<'a, Result<String, ProviderError>> {
        Box::pin(async move {
            const LABEL: &str = "语音识别";
            let boundary = "moss-voice-7d1a5c0e9b";
            let body = self.multipart(&encode_wav(pcm, sample_rate), boundary);
            let request = self
                .client
                .post(&self.url)
                .timeout(AUDIO_TIMEOUT)
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(body);
            let response =
                with_auth(request, &self.api_key).send().await.map_err(|error| describe_send_error(LABEL, &error))?;
            let response = check_status(LABEL, response).await?;
            let bytes = read_body(LABEL, response, MAX_ERROR_BYTES).await?;
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) => value
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| text.trim().to_owned())
                    .ok_or_else(|| ProviderError::new("语音识别返回的结果缺少 text 字段")),
                // response_format=text 的服务直接返回文字
                Err(_) => Ok(String::from_utf8_lossy(&bytes).trim().to_owned()),
            }
        })
    }
}

// ---------------------------------------------------------------- 合成

pub struct OpenAiTts {
    client: reqwest::Client,
    url: String,
    api_key: String,
    model: String,
    voice: String,
    speed: f32,
}

impl OpenAiTts {
    pub fn new(url: &str, api_key: &str, model: &str, voice: &str, speed: f32) -> Result<Self, ProviderError> {
        Ok(Self {
            client: client()?,
            url: tts_full_url(url),
            api_key: api_key.trim().to_owned(),
            model: model.trim().to_owned(),
            voice: voice.trim().to_owned(),
            speed,
        })
    }
}

impl Tts for OpenAiTts {
    fn synthesize<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
        Box::pin(async move {
            const LABEL: &str = "语音合成";
            let body = json!({
                "model": self.model,
                "input": text,
                "voice": self.voice,
                "speed": self.speed,
                "response_format": "wav",
            });
            let request = self.client.post(&self.url).timeout(AUDIO_TIMEOUT).json(&body);
            let response =
                with_auth(request, &self.api_key).send().await.map_err(|error| describe_send_error(LABEL, &error))?;
            let response = check_status(LABEL, response).await?;
            decode_wav(&read_body(LABEL, response, MAX_AUDIO_BYTES).await?)
        })
    }
}

// ---------------------------------------------------------------- 大模型

/// 服务器推送事件（SSE）解码：按行切分，收集 `data:` 字段，空行结束一个事件。
#[derive(Default)]
pub struct SseDecoder {
    pending: Vec<u8>,
    data: Vec<String>,
}

impl SseDecoder {
    /// 送入一段字节，返回完整事件的 data（多行 data 以换行连接）。半个 UTF-8 字符留到下一段。
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(value) = line.strip_prefix("data:") {
                self.data.push(value.strip_prefix(' ').unwrap_or(value).to_owned());
            }
        }
        events
    }

    /// 流结束：送出最后一个没有空行结尾的事件。
    pub fn finish(&mut self) -> Option<String> {
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            let line = String::from_utf8_lossy(&rest);
            if let Some(value) = line.trim_end().strip_prefix("data:") {
                self.data.push(value.trim_start().to_owned());
            }
        }
        (!self.data.is_empty()).then(|| std::mem::take(&mut self.data).join("\n"))
    }
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

/// 把流式的 `choices[0].delta` 累积成完整回复。
#[derive(Default)]
pub struct StreamAccumulator {
    text: String,
    calls: Vec<PartialCall>,
}

impl StreamAccumulator {
    /// 处理一个数据块，返回其中的文字增量。
    pub fn apply(&mut self, chunk: &Value) -> Result<Option<String>, ProviderError> {
        let payload = if chunk.get("choices").is_some() || chunk.get("error").is_some() {
            chunk
        } else {
            chunk.get("data").unwrap_or(chunk)
        };
        if let Some(error) = payload.get("error") {
            let message = error.get("message").and_then(Value::as_str).unwrap_or("未知错误");
            let short: String = message.chars().take(120).collect();
            return Err(ProviderError::new(format!("大模型返回错误：{short}")));
        }
        let Some(choice) = payload.get("choices").and_then(|choices| choices.get(0)) else { return Ok(None) };
        // 流式为 delta；不支持流式的服务返回完整 message
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        let Some(delta) = delta else { return Ok(None) };
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for (position, call) in calls.iter().enumerate() {
                let index = call.get("index").and_then(Value::as_u64).map(|index| index as usize).unwrap_or(position);
                while self.calls.len() <= index {
                    self.calls.push(PartialCall::default());
                }
                let slot = &mut self.calls[index];
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    slot.id = id.to_owned();
                }
                if let Some(function) = call.get("function") {
                    if let Some(name) = function.get("name").and_then(Value::as_str) {
                        slot.name.push_str(name);
                    }
                    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                        slot.arguments.push_str(arguments);
                    }
                }
            }
        }
        match delta.get("content").and_then(Value::as_str) {
            Some(text) if !text.is_empty() => {
                self.text.push_str(text);
                Ok(Some(text.to_owned()))
            }
            _ => Ok(None),
        }
    }

    pub fn finish(self) -> LlmReply {
        let tool_calls = self
            .calls
            .into_iter()
            .enumerate()
            .filter(|(_, call)| !call.name.is_empty())
            .map(|(index, call)| {
                let id = if call.id.is_empty() { format!("call_{index}") } else { call.id };
                let arguments = if call.arguments.trim().is_empty() { "{}".to_owned() } else { call.arguments };
                ToolCall::new(id, call.name, arguments)
            })
            .collect();
        LlmReply { text: self.text, tool_calls }
    }
}

pub struct OpenAiLlm {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAiLlm {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Result<Self, ProviderError> {
        Ok(Self {
            client: client()?,
            base_url: llm_base_url(base_url),
            api_key: api_key.trim().to_owned(),
            model: model.trim().to_owned(),
        })
    }

    pub fn request_body(&self, messages: &[ChatMessage], tools: &[ToolSpec]) -> Value {
        let mut body = json!({"model": self.model, "messages": messages, "stream": true});
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.iter().map(ToolSpec::to_openai).collect());
            body["tool_choice"] = json!("auto");
        }
        body
    }

    async fn run(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        deltas: UnboundedSender<String>,
    ) -> Result<LlmReply, ProviderError> {
        const LABEL: &str = "大模型";
        let deadline = Instant::now() + LLM_TOTAL_TIMEOUT;
        let endpoint = format!("{}/chat/completions", self.base_url);
        let request = self.client.post(&endpoint).json(&self.request_body(messages, tools));
        let send = with_llm_compat(request, &endpoint, &self.api_key).send();
        let response = tokio::time::timeout(LLM_IDLE_TIMEOUT, send)
            .await
            .map_err(|_| ProviderError::new("大模型请求超时（30 秒无响应）"))?
            .map_err(|error| describe_send_error(LABEL, &error))?;
        let mut response = check_status(LABEL, response).await?;
        let mut decoder = SseDecoder::default();
        let mut accumulator = StreamAccumulator::default();
        let mut raw = Vec::new();
        let mut saw_event = false;
        let handle = |payload: &str, accumulator: &mut StreamAccumulator| -> Result<bool, ProviderError> {
            if payload.trim() == "[DONE]" {
                return Ok(true);
            }
            let Ok(value) = serde_json::from_str::<Value>(payload) else { return Ok(false) };
            if let Some(text) = accumulator.apply(&value)? {
                let _ = deltas.send(text);
            }
            Ok(false)
        };
        'stream: loop {
            let wait = LLM_IDLE_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()));
            let chunk = tokio::time::timeout(wait, response.chunk())
                .await
                .map_err(|_| ProviderError::new("大模型输出中断（超时）"))?
                .map_err(|error| describe_send_error(LABEL, &error))?;
            let Some(chunk) = chunk else { break };
            if !saw_event && raw.len() < MAX_ERROR_BYTES {
                raw.extend_from_slice(&chunk);
            }
            for payload in decoder.push(&chunk) {
                saw_event = true;
                if handle(&payload, &mut accumulator)? {
                    break 'stream;
                }
            }
        }
        if let Some(payload) = decoder.finish() {
            saw_event = true;
            handle(&payload, &mut accumulator)?;
        }
        if !saw_event {
            // 服务忽略了 stream=true，直接返回完整 JSON
            let value: Value =
                serde_json::from_slice(&raw).map_err(|_| ProviderError::new("大模型返回的数据无法解析"))?;
            if let Some(text) = accumulator.apply(&value)? {
                let _ = deltas.send(text);
            }
        }
        Ok(accumulator.finish())
    }
}

impl Llm for OpenAiLlm {
    fn chat<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [ToolSpec],
        deltas: UnboundedSender<String>,
    ) -> BoxFuture<'a, Result<LlmReply, ProviderError>> {
        Box::pin(self.run(messages, tools, deltas))
    }
}

#[cfg(test)]
mod tests;
