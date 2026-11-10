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
    trimmed
        .strip_suffix("/chat/completions")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_owned()
}

fn full_url(input: &str, suffix: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.ends_with(suffix) {
        trimmed.to_owned()
    } else {
        format!("{trimmed}{suffix}")
    }
}

pub fn asr_full_url(input: &str) -> String {
    full_url(input, "/audio/transcriptions")
}

pub fn tts_full_url(input: &str) -> String {
    full_url(input, "/audio/speech")
}

fn client() -> Result<reqwest::Client, ProviderError> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
        std::sync::OnceLock::new();
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
        .clone()
        .map_err(ProviderError::new)
}

/// 不使用 reqwest 的原始描述（它会带上完整地址）；只取底层原因链（DNS / 拒绝连接 / 证书……）判断具体原因，
/// 原因链脱敏后放进 `detail`。
fn describe_send_error(label: &str, error: &reqwest::Error) -> ProviderError {
    let mut chain = Vec::new();
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        chain.push(inner.to_string());
        source = inner.source();
    }
    classify_transport_error(
        label,
        error.is_timeout(),
        error.is_connect(),
        &chain.join(": "),
    )
}

/// 按底层原因区分网络错误（纯函数，测试用假输入覆盖）。每种原因各自一句大白话和下一步。
pub(crate) fn classify_transport_error(
    label: &str,
    timeout: bool,
    connect: bool,
    chain: &str,
) -> ProviderError {
    use crate::DiagCategory::Network;
    let lower = chain.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
    let error = if timeout {
        ProviderError::with_category(Network, format!("{label}请求超时：对方服务器太久没有回应"))
            .hint("检查电脑网络是否通畅；服务方繁忙时稍后再试，一直超时就换一个更近或更稳定的服务地址")
    } else if has(&[
        "dns",
        "failed to lookup",
        "nodename nor servname",
        "name or service not known",
        "no such host",
        "name resolution",
    ]) {
        ProviderError::with_category(Network, format!("{label}找不到服务器：域名解析失败"))
            .hint("检查地址里的域名有没有拼错，电脑能不能正常上网；用了代理或公司网络时确认它没有拦截这个域名")
    } else if has(&["certificate", "tls", "ssl", "handshake"]) {
        ProviderError::with_category(
            Network,
            format!("{label}安全连接失败：TLS 握手或证书校验没通过"),
        )
        .hint("确认地址是 https:// 且证书有效；电脑时间不对、代理软件替换了证书时也会这样")
    } else if has(&["refused"]) {
        ProviderError::with_category(
            Network,
            format!("{label}无法连接：对方拒绝了连接（服务没开或端口填错）"),
        )
        .hint("确认服务已经启动、地址和端口填对；本机服务（如 127.0.0.1）要先在电脑上运行起来")
    } else if has(&["unreachable", "no route"]) {
        ProviderError::with_category(
            Network,
            format!("{label}无法连接：网络不通，到不了这个地址"),
        )
        .hint("检查电脑是否联网、Wi-Fi 或网线是否正常；内网地址要和电脑在同一个网络里")
    } else if connect {
        ProviderError::with_category(Network, format!("{label}无法连接到该地址"))
            .hint("检查地址、端口和电脑网络；具体原因见技术细节")
    } else if has(&["reset", "broken pipe", "closed", "eof", "incomplete"]) {
        ProviderError::with_category(
            Network,
            format!("{label}连接中途断开：数据还没收完对方就断了"),
        )
        .hint("多半是网络不稳或服务方主动断开；稍后再试，一直这样就换个网络或服务地址")
    } else {
        ProviderError::with_category(Network, format!("{label}请求失败：网络传输出错"))
            .hint("检查电脑网络后再试；具体原因见技术细节")
    };
    error.detail(chain)
}

/// HTTP 错误状态 → 分类与大白话（纯函数）。`upstream` 是服务方原话（已截断、不含地址），放进技术细节。
pub(crate) fn classify_http_status(
    label: &str,
    status: u16,
    upstream: Option<String>,
) -> ProviderError {
    use crate::DiagCategory::{Auth, Config, Model, Quota};
    let (category, reason, hint) = match status {
        401 => (
            Auth,
            "，密钥无效或没有权限",
            "密钥填错、过期或被停用了：到服务方控制台复制新密钥重新填写",
        ),
        403 => (
            Auth,
            "，密钥无效或没有权限",
            "这个密钥没有调用该模型的权限，或账号被限制：到服务方控制台检查权限",
        ),
        402 => (
            Quota,
            "，账户余额不足",
            "到服务方控制台充值或换一个有余额的账号",
        ),
        429 => (
            Quota,
            "，请求太频繁或额度不足",
            "调用太快或今天的额度用完了：等一会儿再试，或到控制台提高限额",
        ),
        404 => (
            Config,
            "，地址或模型名不对",
            "检查服务地址和模型名称是否和服务方文档一致",
        ),
        400 | 422 => (
            Config,
            "，请求参数不被接受",
            "通常是模型名、音色或参数不被这个服务支持：对照服务方文档检查设置",
        ),
        300..=399 => (
            Config,
            "，服务要求跳转（MOSS 不跟随重定向，请填写最终地址）",
            "把地址换成跳转后的最终地址",
        ),
        500..=599 => (
            Model,
            "，服务方服务器出错",
            "这是服务方那边的问题，不是你的设置：稍后再试或换一个模型服务",
        ),
        _ => (Model, "", "具体原因见技术细节"),
    };
    let error =
        ProviderError::with_category(category, format!("{label}返回 HTTP {status}{reason}"))
            .hint(hint);
    match upstream {
        Some(text) => error.detail(format!("服务方原话：{text}")),
        None => error,
    }
}

/// 从错误响应里取服务方原话（只取 `error.message` 一类字段并截断；不回显整个响应，含地址或疑似密钥时不保留）。
fn error_upstream(bytes: &[u8]) -> Option<String> {
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
    crate::upstream_text(&text)
}

async fn read_body(
    label: &str,
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, ProviderError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| describe_send_error(label, &error))?
    {
        body.extend_from_slice(&chunk);
        if body.len() > limit {
            return Err(ProviderError::new(format!("{label}返回的数据过大")));
        }
    }
    Ok(body)
}

async fn check_status(
    label: &str,
    response: reqwest::Response,
) -> Result<reqwest::Response, ProviderError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = read_body(label, response, MAX_ERROR_BYTES)
        .await
        .unwrap_or_default();
    Err(classify_http_status(
        label,
        status.as_u16(),
        error_upstream(&body),
    ))
}

fn with_auth(builder: reqwest::RequestBuilder, api_key: &str) -> reqwest::RequestBuilder {
    if api_key.is_empty() {
        builder
    } else {
        builder.bearer_auth(api_key)
    }
}

/// “快速回答”：让思考型模型跳过推理直接回答的请求参数（2026-11-08，n73）。
///
/// 语音对话每轮都要等大模型开口，思考型模型（Qwen3、豆包 Seed、GLM-4.5+、GPT-5 等）默认先推理
/// 数秒；动机器前的复述确认、桌面审批与急停都由代码把关，不依赖模型推理。各家参数写法不同，
/// 只对认识的服务商给参数；服务不认（返回 400 / 422）时由调用方去掉参数重试一次并记入
/// [`mark_thinking_off_unsupported`]，之后同一地址与模型不再携带。推理专用模型（如 DeepSeek-R1）关不掉，照常运行。
pub fn thinking_off_params(base_url: &str, model: &str) -> Option<serde_json::Map<String, Value>> {
    let host = reqwest::Url::parse(base_url.trim())
        .ok()?
        .host_str()?
        .to_ascii_lowercase();
    let model = model.trim().to_ascii_lowercase();
    let on = |suffix: &str| host == suffix || host.ends_with(&format!(".{suffix}"));
    let value = if on("dashscope.aliyuncs.com")
        || on("dashscope-intl.aliyuncs.com")
        || on("siliconflow.cn")
        || on("siliconflow.com")
    {
        json!({"enable_thinking": false})
    } else if on("volces.com") || on("bigmodel.cn") || on("z.ai") {
        json!({"thinking": {"type": "disabled"}})
    } else if on("openrouter.ai") {
        json!({"reasoning": {"enabled": false}})
    } else if on("api.openai.com") && model.starts_with("gpt-5") {
        json!({"reasoning_effort": "minimal"})
    } else {
        return None;
    };
    value.as_object().cloned()
}

fn thinking_unsupported() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(Default::default)
}

fn thinking_key(base_url: &str, model: &str) -> String {
    format!("{}|{}", llm_base_url(base_url), model.trim())
}

/// 这个地址与模型已证实不认“关闭思考”参数（本次运行内有效）。
pub fn mark_thinking_off_unsupported(base_url: &str, model: &str) {
    if let Ok(mut set) = thinking_unsupported().lock() {
        set.insert(thinking_key(base_url, model));
    }
}

/// 开启“快速回答”时本次请求要附带的参数：服务商不在表里或已证实不支持时为 `None`。
pub fn fast_reply_params(base_url: &str, model: &str) -> Option<serde_json::Map<String, Value>> {
    let unsupported = thinking_unsupported()
        .lock()
        .map(|set| set.contains(&thinking_key(base_url, model)))
        .unwrap_or(false);
    if unsupported {
        return None;
    }
    thinking_off_params(base_url, model)
}

pub fn is_cline_api_url(url: &str) -> bool {
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

fn with_llm_compat(
    builder: reqwest::RequestBuilder,
    url: &str,
    api_key: &str,
) -> reqwest::RequestBuilder {
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
    pub fn new(
        url: &str,
        api_key: &str,
        model: &str,
        language: Option<&str>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            prompt: None,
            client: client()?,
            url: asr_full_url(url),
            api_key: api_key.trim().to_owned(),
            model: model.trim().to_owned(),
            language: language
                .map(str::to_owned)
                .filter(|language| !language.is_empty()),
        })
    }

    pub fn with_hotwords(mut self, words: &[String]) -> Self {
        let prompt = words
            .iter()
            .take(64)
            .map(|w| {
                w.chars()
                    .filter(|c| !c.is_control())
                    .take(32)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("、");
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
        if let Some(prompt) = &self.prompt {
            field("prompt", prompt);
        }
        if let Some(language) = &self.language {
            field("language", language);
        }
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        body
    }
}

impl Asr for OpenAiAsr {
    fn transcribe<'a>(
        &'a self,
        pcm: &'a [i16],
        sample_rate: u32,
    ) -> BoxFuture<'a, Result<String, ProviderError>> {
        Box::pin(async move {
            const LABEL: &str = "语音识别";
            let boundary = "moss-voice-7d1a5c0e9b";
            let body = self.multipart(&encode_wav(pcm, sample_rate), boundary);
            let request = self
                .client
                .post(&self.url)
                .timeout(AUDIO_TIMEOUT)
                .header(
                    "Content-Type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(body);
            let response = with_auth(request, &self.api_key)
                .send()
                .await
                .map_err(|error| describe_send_error(LABEL, &error))?;
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
    pub fn new(
        url: &str,
        api_key: &str,
        model: &str,
        voice: &str,
        speed: f32,
    ) -> Result<Self, ProviderError> {
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
            let request = self
                .client
                .post(&self.url)
                .timeout(AUDIO_TIMEOUT)
                .json(&body);
            let response = with_auth(request, &self.api_key)
                .send()
                .await
                .map_err(|error| describe_send_error(LABEL, &error))?;
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
                self.data
                    .push(value.strip_prefix(' ').unwrap_or(value).to_owned());
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
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("未知错误");
            let short: String = message.chars().take(120).collect();
            return Err(ProviderError::new(format!("大模型返回错误：{short}")));
        }
        let Some(choice) = payload.get("choices").and_then(|choices| choices.get(0)) else {
            return Ok(None);
        };
        // 流式为 delta；不支持流式的服务返回完整 message
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        let Some(delta) = delta else { return Ok(None) };
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for (position, call) in calls.iter().enumerate() {
                let index = call
                    .get("index")
                    .and_then(Value::as_u64)
                    .map(|index| index as usize)
                    .unwrap_or(position);
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
                let id = if call.id.is_empty() {
                    format!("call_{index}")
                } else {
                    call.id
                };
                let arguments = if call.arguments.trim().is_empty() {
                    "{}".to_owned()
                } else {
                    call.arguments
                };
                ToolCall::new(id, call.name, arguments)
            })
            .collect();
        LlmReply {
            text: self.text,
            tool_calls,
        }
    }
}

pub struct OpenAiLlm {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    /// 快速回答开启且服务商在表里时要附带的关闭思考参数（见 [`thinking_off_params`]）
    thinking_off: Option<serde_json::Map<String, Value>>,
}

impl OpenAiLlm {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Result<Self, ProviderError> {
        Ok(Self {
            client: client()?,
            base_url: llm_base_url(base_url),
            api_key: api_key.trim().to_owned(),
            model: model.trim().to_owned(),
            thinking_off: None,
        })
    }

    /// 开启后对认识的服务商附带“关闭思考”参数（语音中心“对话体验 › 快速回答”，默认开）。
    pub fn with_fast_reply(mut self, enabled: bool) -> Self {
        self.thinking_off = if enabled {
            thinking_off_params(&self.base_url, &self.model)
        } else {
            None
        };
        self
    }

    /// 测试用：直接指定关闭思考参数（本机假服务的地址不在服务商表里）。
    #[cfg(test)]
    fn with_thinking_off(mut self, params: serde_json::Map<String, Value>) -> Self {
        self.thinking_off = Some(params);
        self
    }

    pub fn request_body(&self, messages: &[ChatMessage], tools: &[ToolSpec]) -> Value {
        let extra = self.extra_params();
        self.request_body_with(messages, tools, extra.as_ref())
    }

    fn extra_params(&self) -> Option<serde_json::Map<String, Value>> {
        let unsupported = thinking_unsupported()
            .lock()
            .map(|set| set.contains(&thinking_key(&self.base_url, &self.model)))
            .unwrap_or(false);
        if unsupported {
            None
        } else {
            self.thinking_off.clone()
        }
    }

    fn request_body_with(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        extra: Option<&serde_json::Map<String, Value>>,
    ) -> Value {
        let mut body = json!({"model": self.model, "messages": messages, "stream": true});
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.iter().map(ToolSpec::to_openai).collect());
            body["tool_choice"] = json!("auto");
        }
        if let (Some(extra), Some(map)) = (extra, body.as_object_mut()) {
            for (key, value) in extra {
                map.insert(key.clone(), value.clone());
            }
        }
        body
    }

    async fn send_once(
        &self,
        endpoint: &str,
        body: &Value,
    ) -> Result<reqwest::Response, ProviderError> {
        let send = with_llm_compat(
            self.client.post(endpoint).json(body),
            endpoint,
            &self.api_key,
        )
        .send();
        tokio::time::timeout(LLM_IDLE_TIMEOUT, send)
            .await
            .map_err(|_| ProviderError::new("大模型请求超时（30 秒无响应）"))?
            .map_err(|error| describe_send_error("大模型", &error))
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
        let extra = self.extra_params();
        let mut response = self
            .send_once(
                &endpoint,
                &self.request_body_with(messages, tools, extra.as_ref()),
            )
            .await?;
        if extra.is_some() && matches!(response.status().as_u16(), 400 | 422) {
            // 服务不认“关闭思考”参数：去掉后重试一次；重试成功才记为不支持，免得把别的 400 误记
            let retry = self
                .send_once(&endpoint, &self.request_body_with(messages, tools, None))
                .await?;
            if retry.status().is_success() {
                mark_thinking_off_unsupported(&self.base_url, &self.model);
            }
            response = retry;
        }
        let mut response = check_status(LABEL, response).await?;
        let mut decoder = SseDecoder::default();
        let mut accumulator = StreamAccumulator::default();
        let mut raw = Vec::new();
        let mut saw_event = false;
        let handle =
            |payload: &str, accumulator: &mut StreamAccumulator| -> Result<bool, ProviderError> {
                if payload.trim() == "[DONE]" {
                    return Ok(true);
                }
                let Ok(value) = serde_json::from_str::<Value>(payload) else {
                    return Ok(false);
                };
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
            let value: Value = serde_json::from_slice(&raw)
                .map_err(|_| ProviderError::new("大模型返回的数据无法解析"))?;
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
