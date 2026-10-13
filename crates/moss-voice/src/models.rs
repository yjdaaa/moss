//! 模型配置向导的输入与“测试连接”（技术书 §9.4.4，实施清单 P4-06）。
//!
//! - 三个独立分区：大模型（LLM）、语音识别（ASR）、语音合成（TTS），各自填写地址、密钥、模型；
//! - 测试连接只用内存中的值发一次最小请求：15 秒超时、**不跟随重定向**，请求结束即丢弃密钥；
//! - 结果、错误信息与日志都不包含密钥（错误不使用 reqwest 的原始描述，它会带上完整地址）。
//!
//! 内置语音服务（NV7）的识别与合成还可以选“豆包流式”（[`ProviderKind::Doubao`]，WebSocket 接口）：
//! 校验见 [`resolve_builtin`]，测试连接由桌面端调用 `moss-voice-providers` 完成；外部服务端链路只支持 OpenAI 兼容。

use std::hash::{Hash, Hasher};
use std::io::Read;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// 测试请求的超时（技术书 §9.4.4）。
pub const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    Llm,
    Asr,
    Tts,
}

impl Section {
    pub const ALL: [Section; 3] = [Section::Llm, Section::Asr, Section::Tts];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Llm => "llm",
            Self::Asr => "asr",
            Self::Tts => "tts",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "llm" => Some(Self::Llm),
            "asr" => Some(Self::Asr),
            "tts" => Some(Self::Tts),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Llm => "大模型",
            Self::Asr => "语音识别",
            Self::Tts => "语音合成",
        }
    }
}

/// 模型接口类型（NV7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// OpenAI 兼容 HTTP 接口
    #[default]
    Openai,
    /// 豆包（火山引擎）流式识别 / 双向流式合成，WebSocket 接口；“模型”填资源 ID
    Doubao,
}

impl ProviderKind {
    pub fn is_openai(&self) -> bool {
        *self == Self::Openai
    }
}

/// 豆包流式识别默认地址与资源 ID（与 `moss-voice-providers::doubao` 的默认值一致，桌面端测试核对）。
pub const DOUBAO_ASR_URL: &str = "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async";
pub const DOUBAO_ASR_RESOURCE: &str = "volc.bigasr.sauc.duration";
/// 豆包双向流式合成默认地址与资源 ID。
pub const DOUBAO_TTS_URL: &str = "wss://openspeech.bytedance.com/api/v3/tts/bidirection";
pub const DOUBAO_TTS_RESOURCE: &str = "seed-tts-2.0";

/// 一个分区的输入。`voice` / `speed` 只用于语音合成。
#[derive(Clone, Default, Deserialize)]
pub struct SectionInput {
    /// 接口类型，缺省为 OpenAI 兼容（只有内置服务的识别与合成可选豆包）
    #[serde(default)]
    pub kind: ProviderKind,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub voice: String,
    #[serde(default)]
    pub speed: Option<f64>,
}

// 手写 Debug：任何调试输出都不带密钥
impl std::fmt::Debug for SectionInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SectionInput")
            .field("kind", &self.kind)
            .field("url", &self.url)
            .field("api_key", &if self.api_key.trim().is_empty() { "" } else { "***" })
            .field("model", &self.model)
            .field("voice", &self.voice)
            .field("speed", &self.speed)
            .finish()
    }
}

/// 向导整体输入；某个分区缺省或全部留空时不修改该模块。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelsInput {
    #[serde(default)]
    pub llm: Option<SectionInput>,
    #[serde(default)]
    pub asr: Option<SectionInput>,
    #[serde(default)]
    pub tts: Option<SectionInput>,
}

impl ModelsInput {
    pub fn section(&self, section: Section) -> Option<&SectionInput> {
        match section {
            Section::Llm => self.llm.as_ref(),
            Section::Asr => self.asr.as_ref(),
            Section::Tts => self.tts.as_ref(),
        }
    }
}

/// 校验并规范化后的分区（`url` 已是最终写入 / 请求的地址）。
#[derive(Clone, PartialEq)]
pub struct ResolvedSection {
    pub section: Section,
    pub kind: ProviderKind,
    pub url: String,
    pub api_key: String,
    pub model: String,
    pub voice: String,
    pub speed: f64,
}

impl std::fmt::Debug for ResolvedSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedSection")
            .field("section", &self.section)
            .field("kind", &self.kind)
            .field("url", &self.url)
            .field("api_key", &if self.api_key.is_empty() { "" } else { "***" })
            .field("model", &self.model)
            .field("voice", &self.voice)
            .field("speed", &self.speed)
            .finish()
    }
}

impl ResolvedSection {
    /// 只在内存中比较“保存的内容是否就是测试通过的内容”。
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.section.hash(&mut hasher);
        self.kind.hash(&mut hasher);
        self.url.hash(&mut hasher);
        self.api_key.hash(&mut hasher);
        self.model.hash(&mut hasher);
        self.voice.hash(&mut hasher);
        self.speed.to_bits().hash(&mut hasher);
        hasher.finish()
    }
}

fn validate_http_url(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("请填写地址".into());
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("地址必须以 http:// 或 https:// 开头".into());
    }
    if url.chars().any(|c| c.is_whitespace() || c == '<' || c == '>') {
        return Err("地址包含空白或占位符".into());
    }
    Ok(())
}

/// 大模型写入 `url` 的基础地址（如 `https://<host>/v1`）；误填了完整 `/chat/completions` 时去掉。
pub fn llm_base_url(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    trimmed.strip_suffix("/chat/completions").unwrap_or(trimmed).trim_end_matches('/').to_owned()
}

fn full_url(input: &str, suffix: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.ends_with(suffix) { trimmed.to_owned() } else { format!("{trimmed}{suffix}") }
}

/// 语音识别 `base_url` 必须是完整的 `/audio/transcriptions` 地址；填基础地址时自动补全。
pub fn asr_full_url(input: &str) -> String {
    full_url(input, "/audio/transcriptions")
}

/// 语音合成 `api_url` 必须是完整的 `/audio/speech` 地址；填基础地址时自动补全。
pub fn tts_full_url(input: &str) -> String {
    full_url(input, "/audio/speech")
}

/// 分区是否留空（留空时保存不修改该模块）。
pub fn is_blank(input: &SectionInput) -> bool {
    input.url.trim().is_empty() && input.model.trim().is_empty() && input.api_key.trim().is_empty()
}

/// 校验并规范化一个分区（外部服务端链路：只支持 OpenAI 兼容）；留空返回 `Ok(None)`。
pub fn resolve(section: Section, input: &SectionInput) -> Result<Option<ResolvedSection>, String> {
    if is_blank(input) {
        return Ok(None);
    }
    if !input.kind.is_openai() {
        return Err(format!("{}：外部服务端链路只支持 OpenAI 兼容接口", section.label()));
    }
    resolve_openai(section, input)
}

/// 内置语音服务的分区校验（NV7）：识别与合成可选豆包流式；留空返回 `Ok(None)`。
///
/// 豆包：地址留空用默认地址（必须是 `wss://`），“模型”留空用默认资源 ID；合成必须填音色。
pub fn resolve_builtin(section: Section, input: &SectionInput) -> Result<Option<ResolvedSection>, String> {
    if is_blank(input) {
        return Ok(None);
    }
    match input.kind {
        ProviderKind::Openai => resolve_openai(section, input),
        ProviderKind::Doubao => resolve_doubao(section, input).map(Some),
    }
}

fn resolve_doubao(section: Section, input: &SectionInput) -> Result<ResolvedSection, String> {
    let (default_url, default_resource) = match section {
        Section::Llm => return Err("大模型只支持 OpenAI 兼容接口（豆包大模型请填火山方舟的 OpenAI 兼容地址）".into()),
        Section::Asr => (DOUBAO_ASR_URL, DOUBAO_ASR_RESOURCE),
        Section::Tts => (DOUBAO_TTS_URL, DOUBAO_TTS_RESOURCE),
    };
    let url = input.url.trim().trim_end_matches('/');
    let url = if url.is_empty() { default_url.to_owned() } else { url.to_owned() };
    if !url.starts_with("wss://") {
        return Err(format!("{}：豆包地址必须以 wss:// 开头", section.label()));
    }
    if url.chars().any(|c| c.is_whitespace() || c == '<' || c == '>') {
        return Err(format!("{}：地址包含空白或占位符", section.label()));
    }
    let model = input.model.trim();
    let model = if model.is_empty() { default_resource.to_owned() } else { model.to_owned() };
    let (voice, speed) = if section == Section::Tts {
        let voice = input.voice.trim().to_owned();
        if voice.is_empty() {
            return Err("语音合成：请填写豆包音色 ID（见豆包控制台的音色列表）".into());
        }
        let speed = input.speed.unwrap_or(1.0);
        if !(0.5..=2.0).contains(&speed) {
            return Err("语音合成：豆包语速须在 0.5 ~ 2.0 之间".into());
        }
        (voice, speed)
    } else {
        (String::new(), 1.0)
    };
    Ok(ResolvedSection {
        section,
        kind: ProviderKind::Doubao,
        url,
        api_key: input.api_key.trim().to_owned(),
        model,
        voice,
        speed,
    })
}

fn resolve_openai(section: Section, input: &SectionInput) -> Result<Option<ResolvedSection>, String> {
    let url = match section {
        Section::Llm => llm_base_url(&input.url),
        Section::Asr => asr_full_url(&input.url),
        Section::Tts => tts_full_url(&input.url),
    };
    if input.url.trim().is_empty() {
        return Err(format!("{}：请填写地址", section.label()));
    }
    validate_http_url(&url).map_err(|error| format!("{}：{error}", section.label()))?;
    let model = input.model.trim().to_owned();
    if model.is_empty() {
        return Err(format!("{}：请填写模型名", section.label()));
    }
    let (voice, speed) = if section == Section::Tts {
        let voice = input.voice.trim().to_owned();
        if voice.is_empty() {
            return Err("语音合成：请填写音色".into());
        }
        let speed = input.speed.unwrap_or(1.0);
        if !(0.25..=4.0).contains(&speed) {
            return Err("语音合成：语速须在 0.25 ~ 4.0 之间".into());
        }
        (voice, speed)
    } else {
        (String::new(), 1.0)
    };
    Ok(Some(ResolvedSection { section, kind: ProviderKind::Openai, url, api_key: input.api_key.trim().to_owned(), model, voice, speed }))
}

/// 一次测试的结果（不含密钥）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TestResult {
    pub section: Section,
    pub ok: bool,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// 实际请求的完整地址（即将写入配置的值，供页面显示）
    pub final_url: String,
    pub detail: String,
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(TEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "无法创建网络客户端".to_owned())
}

fn describe_send_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "请求超时（15 秒）".into()
    } else if error.is_connect() {
        "无法连接到该地址".into()
    } else {
        "请求失败（网络或 TLS 错误）".into()
    }
}

fn read_limited(response: reqwest::blocking::Response) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "响应读取失败".to_owned())?;
    Ok(bytes)
}

/// 从错误响应里取一段可读的说明（只取 `error.message` 一类字段，截断，并脱敏）。
fn error_hint(bytes: &[u8]) -> String {
    let text = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.get("message"))
                .or_else(|| value.get("error"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let short: String = text.chars().take(160).collect();
    crate::redact::redact_line(&short)
}

/// 用一个测试工具定义请求大模型，检查是否返回 `tool_calls`。
pub fn llm_request_body(model: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": "你是连接测试助手。用户询问时间时，必须调用 get_time 工具，不要直接回答。"},
            {"role": "user", "content": "现在几点？请调用工具查询。"}
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "get_time",
                "description": "查询当前时间",
                "parameters": {"type": "object", "properties": {}, "required": []}
            }
        }],
        "tool_choice": "auto",
        "max_tokens": 128,
        "stream": false
    })
}

/// 内置的一段短音频（1 秒、16 kHz、16 位单声道的低音量 440 Hz 正弦波 WAV）。
pub fn test_wav() -> Vec<u8> {
    let rate: u32 = 16_000;
    let samples = rate as usize;
    let data_len = (samples * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // 单声道
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for index in 0..samples {
        let t = index as f64 / rate as f64;
        let value = ((t * 440.0 * std::f64::consts::TAU).sin() * 1200.0) as i16;
        wav.extend_from_slice(&value.to_le_bytes());
    }
    wav
}

fn multipart_body(boundary: &str, model: &str, wav: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(wav.len() + 512);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"moss-test.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(wav);
    body.extend_from_slice(
        format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{model}\r\n--{boundary}--\r\n").as_bytes(),
    );
    body
}

fn with_auth(builder: reqwest::blocking::RequestBuilder, api_key: &str) -> reqwest::blocking::RequestBuilder {
    if api_key.is_empty() { builder } else { builder.bearer_auth(api_key) }
}

/// 执行一次测试连接。密钥只在本函数内使用，返回值不含密钥。
pub fn test_section(resolved: &ResolvedSection) -> TestResult {
    let started = Instant::now();
    let section = resolved.section;
    let request_url = match section {
        Section::Llm => format!("{}/chat/completions", resolved.url),
        _ => resolved.url.clone(),
    };
    let finish = |ok: bool, http_status: Option<u16>, detail: String| TestResult {
        section,
        ok,
        elapsed_ms: started.elapsed().as_millis() as u64,
        http_status,
        final_url: resolved.url.clone(),
        detail,
    };
    let client = match client() {
        Ok(client) => client,
        Err(detail) => return finish(false, None, detail),
    };
    let request = match section {
        Section::Llm => with_auth(client.post(&request_url), &resolved.api_key).json(&llm_request_body(&resolved.model)),
        Section::Asr => {
            let boundary = format!(
                "moss-voice-test-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0)
            );
            with_auth(client.post(&request_url), &resolved.api_key)
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(multipart_body(&boundary, &resolved.model, &test_wav()))
        }
        Section::Tts => with_auth(client.post(&request_url), &resolved.api_key).json(&serde_json::json!({
            "model": resolved.model,
            "input": "你好，我是 MOSS。",
            "voice": resolved.voice,
            "speed": resolved.speed,
            "response_format": "mp3"
        })),
    };
    let response = match request.send() {
        Ok(response) => response,
        Err(error) => return finish(false, None, describe_send_error(&error)),
    };
    let status = response.status();
    let code = status.as_u16();
    if status.is_redirection() {
        return finish(false, Some(code), format!("服务器返回重定向（HTTP {code}），按规则不跟随，请填写最终地址"));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let bytes = match read_limited(response) {
        Ok(bytes) => bytes,
        Err(detail) => return finish(false, Some(code), detail),
    };
    if !status.is_success() {
        let hint = error_hint(&bytes);
        let detail = if code == 401 || code == 403 {
            format!("鉴权失败（HTTP {code}），请检查密钥")
        } else if code == 404 {
            format!("地址不存在（HTTP {code}），请检查地址")
        } else {
            format!("HTTP {code}")
        };
        let detail = if hint.is_empty() { detail } else { format!("{detail}：{hint}") };
        return finish(false, Some(code), detail);
    }
    match section {
        Section::Llm => {
            let value: serde_json::Value = match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                Err(_) => return finish(false, Some(code), "返回的不是 JSON（地址可能不是 OpenAI 兼容接口）".into()),
            };
            let tool_calls = value
                .pointer("/choices/0/message/tool_calls")
                .and_then(serde_json::Value::as_array)
                .map(|calls| !calls.is_empty())
                .unwrap_or(false);
            if tool_calls {
                finish(true, Some(code), "连接成功，模型返回了工具调用（tool_calls）".into())
            } else if value.pointer("/choices/0/message").is_some() {
                finish(false, Some(code), "连接成功，但模型没有返回工具调用；MOSS 的工具需要支持函数调用的模型".into())
            } else {
                finish(false, Some(code), "返回格式不是 chat/completions".into())
            }
        }
        Section::Asr => {
            let value: Option<serde_json::Value> = serde_json::from_slice(&bytes).ok();
            match value.as_ref().and_then(|value| value.get("text")) {
                Some(serde_json::Value::String(_)) => finish(true, Some(code), "连接成功，识别接口已返回结果".into()),
                _ => finish(false, Some(code), "返回中没有 text 字段（地址可能不是 /audio/transcriptions）".into()),
            }
        }
        Section::Tts => {
            let looks_json = content_type.contains("json") || bytes.first() == Some(&b'{');
            if bytes.is_empty() || looks_json {
                finish(false, Some(code), "没有返回音频数据（地址可能不是 /audio/speech）".into())
            } else {
                finish(true, Some(code), format!("连接成功，合成了 {} 字节音频", bytes.len()))
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod fake_http {
    //! 测试用的一次性 HTTP 服务：记录收到的请求并返回给定响应。
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    pub struct Received {
        pub head: String,
        pub body: Vec<u8>,
    }

    pub fn serve_once(status_line: &'static str, headers: &'static str, body: Vec<u8>) -> (String, mpsc::Receiver<Received>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 8192];
            let (head_end, length) = loop {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    return;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buffer[..pos]).to_string();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    break (pos + 4, length);
                }
            };
            while buffer.len() < head_end + length {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
            }
            let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
            let request_body = buffer[head_end..].to_vec();
            let response = format!("{status_line}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&body);
            let _ = tx.send(Received { head, body: request_body });
        });
        (format!("http://127.0.0.1:{port}"), rx)
    }
}

#[cfg(test)]
mod tests {
    use super::fake_http::serve_once;
    use super::*;

    fn input(url: &str, key: &str, model: &str) -> SectionInput {
        SectionInput { url: url.into(), api_key: key.into(), model: model.into(), ..Default::default() }
    }

    #[test]
    fn urls_are_normalized_to_final_values() {
        assert_eq!(llm_base_url(" https://h/v1/ "), "https://h/v1");
        assert_eq!(llm_base_url("https://h/v1/chat/completions"), "https://h/v1");
        assert_eq!(asr_full_url("https://h/v1"), "https://h/v1/audio/transcriptions");
        assert_eq!(asr_full_url("https://h/v1/audio/transcriptions/"), "https://h/v1/audio/transcriptions");
        assert_eq!(tts_full_url("https://h/v1/"), "https://h/v1/audio/speech");
        assert_eq!(tts_full_url("https://h/v1/audio/speech"), "https://h/v1/audio/speech");
    }

    #[test]
    fn resolve_validates_and_blank_means_unchanged() {
        assert_eq!(resolve(Section::Llm, &SectionInput::default()).unwrap(), None);
        assert!(resolve(Section::Llm, &input("ftp://h", "k", "m")).is_err());
        assert!(resolve(Section::Llm, &input("https://h/v1", "k", "")).is_err());
        assert!(resolve(Section::Llm, &input("https://<host>/v1", "k", "m")).is_err());
        let tts = SectionInput { voice: String::new(), ..input("https://h/v1", "k", "tts-1") };
        assert!(resolve(Section::Tts, &tts).unwrap_err().contains("音色"));
        let tts = SectionInput { voice: "alloy".into(), speed: Some(9.0), ..input("https://h/v1", "k", "tts-1") };
        assert!(resolve(Section::Tts, &tts).unwrap_err().contains("语速"));
        let resolved = resolve(Section::Asr, &input("https://h/v1", " k ", "whisper-1")).unwrap().unwrap();
        assert_eq!(resolved.url, "https://h/v1/audio/transcriptions");
        assert_eq!(resolved.api_key, "k");
        // Debug 输出不含密钥
        let debug = format!("{resolved:?} {:?}", input("https://h", "sk-secret-123", "m"));
        assert!(!debug.contains("sk-secret-123"), "{debug}");
    }

    #[test]
    fn llm_test_checks_tool_calls_and_sends_key_only_in_header() {
        let reply = br#"{"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"1","type":"function","function":{"name":"get_time","arguments":"{}"}}]}}]}"#;
        let (base, rx) = serve_once("HTTP/1.1 200 OK", "Content-Type: application/json\r\n", reply.to_vec());
        let resolved = resolve(Section::Llm, &input(&format!("{base}/v1"), "sk-fake-llm-key", "m1")).unwrap().unwrap();
        let result = test_section(&resolved);
        assert!(result.ok, "{result:?}");
        assert_eq!(result.final_url, format!("{base}/v1"));
        let received = rx.recv().unwrap();
        assert!(received.head.starts_with("POST /v1/chat/completions"), "{}", received.head);
        assert!(received.head.to_ascii_lowercase().contains("authorization: bearer sk-fake-llm-key"));
        let body: serde_json::Value = serde_json::from_slice(&received.body).unwrap();
        assert_eq!(body["model"], "m1");
        assert_eq!(body["tools"][0]["function"]["name"], "get_time");
        let shown = serde_json::to_string(&result).unwrap();
        assert!(!shown.contains("sk-fake-llm-key"));

        // 有回答但没有工具调用：不算通过
        let reply = br#"{"choices":[{"message":{"role":"assistant","content":"12:00"}}]}"#;
        let (base, _rx) = serve_once("HTTP/1.1 200 OK", "Content-Type: application/json\r\n", reply.to_vec());
        let resolved = resolve(Section::Llm, &input(&base, "k", "m")).unwrap().unwrap();
        let result = test_section(&resolved);
        assert!(!result.ok);
        assert!(result.detail.contains("工具调用"), "{result:?}");
    }

    #[test]
    fn redirects_are_not_followed_and_auth_errors_are_explained() {
        let (base, _rx) = serve_once("HTTP/1.1 302 Found", "Location: http://127.0.0.1:1/elsewhere\r\n", Vec::new());
        let resolved = resolve(Section::Tts, &SectionInput { voice: "alloy".into(), ..input(&base, "k", "tts-1") }).unwrap().unwrap();
        let result = test_section(&resolved);
        assert!(!result.ok);
        assert_eq!(result.http_status, Some(302));
        assert!(result.detail.contains("不跟随"), "{result:?}");

        let (base, _rx) = serve_once(
            "HTTP/1.1 401 Unauthorized",
            "Content-Type: application/json\r\n",
            br#"{"error":{"message":"Incorrect API key provided: sk-fake***"}}"#.to_vec(),
        );
        let resolved = resolve(Section::Llm, &input(&base, "sk-fake-key", "m")).unwrap().unwrap();
        let result = test_section(&resolved);
        assert!(!result.ok);
        assert!(result.detail.contains("鉴权失败"), "{result:?}");
        assert!(!result.detail.contains("sk-fake-key"));
    }

    #[test]
    fn asr_sends_multipart_audio_and_tts_expects_audio() {
        let (base, rx) = serve_once("HTTP/1.1 200 OK", "Content-Type: application/json\r\n", br#"{"text":""}"#.to_vec());
        let resolved = resolve(Section::Asr, &input(&base, "k", "whisper-1")).unwrap().unwrap();
        let result = test_section(&resolved);
        assert!(result.ok, "{result:?}");
        let received = rx.recv().unwrap();
        assert!(received.head.starts_with("POST /audio/transcriptions"), "{}", received.head);
        assert!(received.head.contains("multipart/form-data; boundary="));
        let text = String::from_utf8_lossy(&received.body);
        assert!(text.contains("name=\"model\"") && text.contains("whisper-1") && text.contains("RIFF"));

        let (base, rx) = serve_once("HTTP/1.1 200 OK", "Content-Type: audio/mpeg\r\n", vec![0xFF, 0xFB, 0x90, 0x00, 1, 2, 3]);
        let tts = SectionInput { voice: "alloy".into(), speed: Some(1.2), ..input(&base, "k", "tts-1") };
        let result = test_section(&resolve(Section::Tts, &tts).unwrap().unwrap());
        assert!(result.ok, "{result:?}");
        let body: serde_json::Value = serde_json::from_slice(&rx.recv().unwrap().body).unwrap();
        assert_eq!(body["voice"], "alloy");
        assert_eq!(body["speed"], 1.2);

        let (base, _rx) = serve_once("HTTP/1.1 200 OK", "Content-Type: application/json\r\n", br#"{"error":"x"}"#.to_vec());
        let tts = SectionInput { voice: "alloy".into(), ..input(&base, "k", "tts-1") };
        assert!(!test_section(&resolve(Section::Tts, &tts).unwrap().unwrap()).ok);
    }

    #[test]
    fn doubao_is_only_for_builtin_asr_and_tts() {
        let doubao = |url: &str, model: &str| SectionInput { kind: ProviderKind::Doubao, ..input(url, "k", model) };
        // 外部服务端链路不接受豆包
        assert!(resolve(Section::Asr, &doubao("", "")).unwrap_err().contains("外部服务端"));
        assert!(resolve_builtin(Section::Llm, &doubao("", "")).unwrap_err().contains("OpenAI 兼容"));
        // 地址与资源 ID 留空用默认值
        let asr = resolve_builtin(Section::Asr, &doubao("", "")).unwrap().unwrap();
        assert_eq!((asr.kind, asr.url.as_str(), asr.model.as_str()), (ProviderKind::Doubao, DOUBAO_ASR_URL, DOUBAO_ASR_RESOURCE));
        let custom = resolve_builtin(Section::Asr, &doubao("wss://h/api/v3/sauc/bigmodel/", "volc.seedasr.sauc.duration")).unwrap().unwrap();
        assert_eq!((custom.url.as_str(), custom.model.as_str()), ("wss://h/api/v3/sauc/bigmodel", "volc.seedasr.sauc.duration"));
        assert!(resolve_builtin(Section::Asr, &doubao("https://h", "")).unwrap_err().contains("wss://"));
        // 合成：音色必填，语速 0.5 ~ 2.0
        assert!(resolve_builtin(Section::Tts, &doubao("", "")).unwrap_err().contains("音色"));
        let tts = SectionInput { voice: "zh_female_test".into(), speed: Some(1.5), ..doubao("", "") };
        let resolved = resolve_builtin(Section::Tts, &tts).unwrap().unwrap();
        assert_eq!((resolved.url.as_str(), resolved.model.as_str(), resolved.speed), (DOUBAO_TTS_URL, DOUBAO_TTS_RESOURCE, 1.5));
        assert!(resolve_builtin(Section::Tts, &SectionInput { speed: Some(3.0), ..tts.clone() }).unwrap_err().contains("0.5"));
        // 接口类型参与“测试通过的就是保存的”比对
        let openai = resolve_builtin(Section::Tts, &SectionInput { kind: ProviderKind::Openai, url: "https://h/v1".into(), model: "tts-1".into(), ..tts }).unwrap().unwrap();
        assert_ne!(openai.fingerprint(), resolved.fingerprint());
        // 旧版输入没有 kind：按 OpenAI 兼容处理
        let legacy: SectionInput = serde_json::from_str(r#"{"url":"https://h/v1","model":"m"}"#).unwrap();
        assert_eq!(legacy.kind, ProviderKind::Openai);
    }

    #[test]
    fn unreachable_address_reports_without_url() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let resolved = resolve(Section::Llm, &input(&format!("http://127.0.0.1:{port}/v1?key=abc"), "k", "m")).unwrap().unwrap();
        let result = test_section(&resolved);
        assert!(!result.ok);
        assert!(!result.detail.contains("key=abc"), "{result:?}");
    }

    #[test]
    fn test_wav_is_a_valid_one_second_pcm_file() {
        let wav = test_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(wav.len(), 44 + 32_000);
    }
}
