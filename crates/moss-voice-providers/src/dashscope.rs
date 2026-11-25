//! 千问 AI / 阿里云百炼（DashScope）原生语音合成与声音克隆客户端（n123）。
//!
//! - 语音合成：`POST <origin>/api/v1/services/audio/tts/SpeechSynthesizer`，开启 `X-DashScope-SSE: enable`
//!   流式接收 24 kHz 16-bit 单声道 PCM（`output.audio.data` Base64），首包到达即推送播放，同时兼容非流式 JSON 回退；
//! - 声音克隆：`POST <origin>/api/v1/services/audio/tts/customization`（`model = "voice-enrollment"`），
//!   支持上传本机 3～60 秒参考音频的 Base64 Data URI（无需额外公网 OSS），创建后轮询 `query_voice` 直至 `status == "OK"`；
//! - 错误信息与日志绝不包含 API Key 或完整带参 URL。

use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::time::Instant;

use crate::openai::{
    AUDIO_TIMEOUT, MAX_AUDIO_BYTES, MAX_ERROR_BYTES, SseDecoder, check_status,
    clean_scheme_and_whitespace, client, describe_send_error, read_body,
};
use crate::wav::decode_wav;
use crate::{BoxFuture, DiagCategory, Pcm, ProviderError, Tts};

pub const TTS_SAMPLE_RATE: u32 = 24_000;
pub const DEFAULT_API_BASE_URL: &str = "https://maas.qianwenaiapi.com/api/v1";
pub const DEFAULT_TTS_MODEL: &str = "qwen-audio-3.1-tts-flash";

const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const STREAM_TOTAL_TIMEOUT: Duration = Duration::from_secs(60);
const CLONE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CLONE_TOTAL_TIMEOUT: Duration = Duration::from_secs(60);
const CLONE_POLL_INTERVAL: Duration = Duration::from_millis(600);
const CLONE_MAX_POLLS: usize = 45;
const MAX_CLONE_RAW_BYTES: usize = 7 * 1024 * 1024;
const MAX_SSE_STREAM_BYTES: usize = MAX_AUDIO_BYTES * 2;

fn is_loopback_host(host: &str) -> bool {
    let clean = host.trim_matches(['[', ']']);
    clean.eq_ignore_ascii_case("127.0.0.1")
        || clean.eq_ignore_ascii_case("localhost")
        || clean == "::1"
}

fn is_private_or_internal_host(host: &str) -> bool {
    let clean = host.trim_matches(['[', ']']);
    if is_loopback_host(clean)
        || clean.ends_with(".localhost")
        || clean.ends_with(".local")
        || clean.ends_with(".internal")
    {
        return true;
    }
    let check_v4 = |v4: std::net::Ipv4Addr| {
        let octets = v4.octets();
        let is_cgnat = octets[0] == 100 && (octets[1] & 0b1100_0000) == 64;
        let is_benchmark = octets[0] == 198 && (octets[1] == 18 || octets[1] == 19);
        v4.is_loopback()
            || v4.is_private()
            || v4.is_link_local()
            || v4.is_unspecified()
            || v4.is_broadcast()
            || v4.is_multicast()
            || is_cgnat
            || is_benchmark
    };
    if let Ok(ip) = clean.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => check_v4(v4),
            std::net::IpAddr::V6(v6) => {
                if let Some(mapped) = v6.to_ipv4_mapped() {
                    return check_v4(mapped);
                }
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    || (v6.segments()[0] & 0xfe00) == 0xfc00
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
            }
        };
    }
    false
}

fn sanitize_short_token(raw: &str, fallback: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .take(48)
        .collect();
    if cleaned.is_empty() {
        fallback.to_owned()
    } else {
        cleaned
    }
}

/// 规范化千问 / 百炼 `/api/v1` 基础地址（与 `moss_voice::models::dashscope_api_base_url` 规则一致）。
pub fn dashscope_api_base_url(input: &str) -> String {
    let cleaned = clean_scheme_and_whitespace(input);
    if cleaned.is_empty() {
        return DEFAULT_API_BASE_URL.to_owned();
    }
    if !(cleaned.starts_with("http://") || cleaned.starts_with("https://"))
        || cleaned
            .chars()
            .any(|c| c.is_whitespace() || c == '<' || c == '>')
    {
        return cleaned;
    }
    let Ok(parsed) = reqwest::Url::parse(&cleaned) else {
        return cleaned;
    };
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.host_str().is_none()
    {
        return cleaned;
    }
    let origin = match parsed.port() {
        Some(port) => format!(
            "{}://{}:{port}",
            parsed.scheme(),
            parsed.host_str().unwrap_or_default()
        ),
        None => format!(
            "{}://{}",
            parsed.scheme(),
            parsed.host_str().unwrap_or_default()
        ),
    };
    let mut path = parsed.path().trim_end_matches('/');
    loop {
        let next = [
            "/services/audio/tts/SpeechSynthesizer",
            "/services/audio/tts/speechsynthesizer",
            "/services/audio/tts/customization",
            "/services/aigc/multimodal-generation/generation",
            "/services/aigc/text2audio/generation",
            "/chat/completions",
            "/audio/transcriptions",
            "/audio/speech",
            "/models",
            "/compatible-mode/v1",
            "/api/v1",
            "/v1",
        ]
        .into_iter()
        .find_map(|suffix| path.strip_suffix(suffix))
        .map(|s| s.trim_end_matches('/'));
        match next {
            Some(stripped) if stripped != path => path = stripped,
            _ => break,
        }
    }
    format!("{origin}{path}/api/v1")
}

pub fn dashscope_tts_url(input: &str) -> String {
    let base = dashscope_api_base_url(input);
    if base.ends_with("/api/v1") {
        format!("{base}/services/audio/tts/SpeechSynthesizer")
    } else {
        base
    }
}

pub fn dashscope_customization_url(input: &str) -> String {
    let base = dashscope_api_base_url(input);
    if base.ends_with("/api/v1") {
        format!("{base}/services/audio/tts/customization")
    } else {
        base
    }
}

fn validate_endpoint_and_key(label: &str, url: &str, api_key: &str) -> Result<(), ProviderError> {
    if !(url.starts_with("http://") || url.starts_with("https://"))
        || url
            .chars()
            .any(|c| c.is_whitespace() || c == '<' || c == '>')
    {
        return Err(ProviderError::with_category(
            DiagCategory::Config,
            format!("{label}地址无效（须以 https:// 开头）"),
        ));
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| {
        ProviderError::with_category(DiagCategory::Config, format!("{label}地址格式无效"))
    })?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ProviderError::with_category(
            DiagCategory::Config,
            format!("{label}地址不能包含账号或密码"),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(ProviderError::with_category(
            DiagCategory::Config,
            format!("{label}地址不能包含查询参数或 # 片段，请将密钥填在 API Key 栏"),
        ));
    }
    let host = parsed.host_str().ok_or_else(|| {
        ProviderError::with_category(DiagCategory::Config, format!("{label}地址缺少主机名"))
    })?;
    if parsed.scheme() == "http" && !is_loopback_host(host) {
        return Err(ProviderError::with_category(
            DiagCategory::Config,
            format!("{label}云端地址必须使用 https://（仅本机回环测试允许 http://）"),
        ));
    }
    let key = api_key.trim();
    if key.is_empty() {
        return Err(ProviderError::with_category(
            DiagCategory::Auth,
            format!("{label}缺少密钥"),
        ));
    }
    if key.starts_with("sk-sp-") {
        return Err(ProviderError::with_category(
            DiagCategory::Auth,
            format!("{label}不支持 sk-sp- 套餐密钥：请使用千问 / 百炼按量付费 API Key（sk- 开头）"),
        ));
    }
    Ok(())
}

fn validate_audio_download_url(
    tts_endpoint: &str,
    audio_url: &str,
) -> Result<reqwest::Url, ProviderError> {
    let parsed = reqwest::Url::parse(audio_url.trim()).map_err(|_| {
        ProviderError::with_category(DiagCategory::Model, "千问语音合成返回的音频地址无效")
    })?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ProviderError::with_category(
            DiagCategory::Model,
            "千问语音合成返回的音频地址包含非法凭据",
        ));
    }
    let Some(host) = parsed.host_str() else {
        return Err(ProviderError::with_category(
            DiagCategory::Model,
            "千问语音合成返回的音频地址缺少主机名",
        ));
    };
    let endpoint_is_loopback = reqwest::Url::parse(tts_endpoint)
        .ok()
        .and_then(|u| u.host_str().map(is_loopback_host))
        .unwrap_or(false);
    if endpoint_is_loopback {
        if parsed.scheme() != "https" && !(parsed.scheme() == "http" && is_loopback_host(host)) {
            return Err(ProviderError::with_category(
                DiagCategory::Model,
                "千问语音合成返回的音频地址协议不受信任",
            ));
        }
        return Ok(parsed);
    }
    if parsed.scheme() != "https" || is_private_or_internal_host(host) {
        return Err(ProviderError::with_category(
            DiagCategory::Model,
            "千问语音合成返回的音频下载源不受信任（仅允许公网 HTTPS 地址）",
        ));
    }
    Ok(parsed)
}

fn dashscope_json_error(label: &str, payload: &Value) -> Option<ProviderError> {
    let code = payload
        .get("code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let nested_err = payload
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if code.is_none() && nested_err.is_none() {
        return None;
    }
    let code_str = sanitize_short_token(code.unwrap_or("Error"), "Error");
    let raw_msg = payload
        .get("message")
        .and_then(Value::as_str)
        .or(nested_err)
        .unwrap_or("服务返回错误");
    let lower_code = code_str.to_ascii_lowercase();
    let category = if lower_code.contains("invalidapikey")
        || lower_code.contains("accessdenied")
        || lower_code.contains("unauthorized")
    {
        DiagCategory::Auth
    } else if lower_code.contains("arrearage")
        || lower_code.contains("throttl")
        || lower_code.contains("quota")
    {
        DiagCategory::Quota
    } else if lower_code.contains("invalidparameter")
        || lower_code.contains("modelnotfound")
        || lower_code.contains("preprocesserror")
    {
        DiagCategory::Config
    } else {
        DiagCategory::Model
    };
    let err = ProviderError::with_category(category, format!("{label}出错（{code_str}）"));
    Some(match crate::upstream_text(raw_msg) {
        Some(clean) => err.detail(format!("服务方原话：{clean}")),
        None => err,
    })
}

fn strip_wav_header_if_present(bytes: &[u8]) -> &[u8] {
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return bytes;
    }
    let mut at = 12usize;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size_u32 =
            u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]);
        let size = size_u32 as usize;
        let body = at + 8;
        if id == b"data" {
            if size_u32 == 0 || size_u32 == u32::MAX {
                return &bytes[body..];
            }
            let end = body.saturating_add(size).min(bytes.len());
            return &bytes[body..end];
        }
        at = body.saturating_add(size).saturating_add(size & 1);
    }
    &bytes[44..]
}

fn pcm_samples_with_carry(carry: &mut Option<u8>, bytes: &[u8]) -> Vec<i16> {
    let mut data = Vec::with_capacity(bytes.len() + 1);
    data.extend(carry.take());
    data.extend_from_slice(bytes);
    if data.len() % 2 == 1 {
        *carry = data.pop();
    }
    data.as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

pub struct DashscopeTts {
    client: reqwest::Client,
    tts_url: String,
    api_key: String,
    model: String,
    voice: String,
    speed: f32,
}

impl DashscopeTts {
    pub fn new(
        url: &str,
        api_key: &str,
        model: &str,
        voice: &str,
        speed: f32,
    ) -> Result<Self, ProviderError> {
        let tts_url = dashscope_tts_url(url);
        validate_endpoint_and_key("千问语音合成", &tts_url, api_key)?;
        let voice = voice.trim().to_owned();
        if voice.is_empty() {
            return Err(ProviderError::with_category(
                DiagCategory::Config,
                "千问语音合成缺少音色",
            ));
        }
        let model = model.trim();
        let model = if model.is_empty() {
            DEFAULT_TTS_MODEL.to_owned()
        } else {
            model.to_owned()
        };
        Ok(Self {
            client: client()?,
            tts_url,
            api_key: api_key.trim().to_owned(),
            model,
            voice,
            speed: speed.clamp(0.5, 2.0),
        })
    }

    async fn speak(&self, text: &str, chunks: &UnboundedSender<Pcm>) -> Result<(), ProviderError> {
        const LABEL: &str = "千问语音合成";
        let deadline = Instant::now() + STREAM_TOTAL_TIMEOUT;
        let body = json!({
            "model": self.model,
            "input": {
                "text": text,
                "voice": self.voice,
                "format": "pcm",
                "sample_rate": TTS_SAMPLE_RATE,
                "rate": self.speed,
            }
        });
        let send = self
            .client
            .post(&self.tts_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("X-DashScope-SSE", "enable")
            .json(&body)
            .send();
        let response = tokio::time::timeout(AUDIO_TIMEOUT, send)
            .await
            .map_err(|_| {
                ProviderError::with_category(DiagCategory::Network, "千问语音合成请求超时")
            })?
            .map_err(|error| describe_send_error(LABEL, &error))?;
        let mut response = check_status(LABEL, response).await?;

        let mut decoder = SseDecoder::default();
        let mut raw = Vec::new();
        let mut stream_bytes = 0usize;
        let mut saw_event = false;
        let mut first_audio = true;
        let mut carry: Option<u8> = None;
        let mut total_samples = 0usize;

        let mut process_event = |payload: &str| -> Result<bool, ProviderError> {
            let trimmed = payload.trim();
            if trimmed.is_empty() || trimmed == "[DONE]" {
                return Ok(trimmed == "[DONE]");
            }
            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                return Ok(false);
            };
            if let Some(err) = dashscope_json_error(LABEL, &value) {
                return Err(err);
            }
            if let Some(b64) = value
                .pointer("/output/audio/data")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|_| {
                        ProviderError::with_category(
                            DiagCategory::Audio,
                            "千问语音合成返回的音频编码无效",
                        )
                    })?;
                let slice = if first_audio {
                    first_audio = false;
                    strip_wav_header_if_present(&decoded)
                } else {
                    decoded.as_slice()
                };
                let samples = pcm_samples_with_carry(&mut carry, slice);
                if !samples.is_empty() {
                    let next_total = total_samples.saturating_add(samples.len());
                    if next_total.saturating_mul(2) > MAX_AUDIO_BYTES {
                        return Err(ProviderError::with_category(
                            DiagCategory::Audio,
                            "千问语音合成返回的音频超过大小上限",
                        ));
                    }
                    total_samples = next_total;
                    let _ = chunks.send(Pcm {
                        samples,
                        sample_rate: TTS_SAMPLE_RATE,
                    });
                }
            }
            let finished = value
                .pointer("/output/finish_reason")
                .and_then(Value::as_str)
                .is_some_and(|r| r == "stop");
            Ok(finished)
        };

        'stream: loop {
            let wait = STREAM_IDLE_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()));
            if wait.is_zero() {
                return Err(ProviderError::with_category(
                    DiagCategory::Network,
                    "千问语音合成总耗时超时",
                ));
            }
            let chunk = tokio::time::timeout(wait, response.chunk())
                .await
                .map_err(|_| {
                    ProviderError::with_category(DiagCategory::Network, "千问语音合成流式接收超时")
                })?
                .map_err(|error| describe_send_error(LABEL, &error))?;
            let Some(chunk) = chunk else { break };
            stream_bytes = stream_bytes.saturating_add(chunk.len());
            if stream_bytes > MAX_SSE_STREAM_BYTES {
                return Err(ProviderError::with_category(
                    DiagCategory::Audio,
                    "千问语音合成返回的数据超过大小上限",
                ));
            }
            if !saw_event {
                if raw.len().saturating_add(chunk.len()) > MAX_AUDIO_BYTES {
                    return Err(ProviderError::with_category(
                        DiagCategory::Audio,
                        "千问语音合成返回的数据超过大小上限",
                    ));
                }
                raw.extend_from_slice(&chunk);
            }
            for event_data in decoder.push(&chunk) {
                if !saw_event {
                    saw_event = true;
                    raw.clear();
                    raw.shrink_to_fit();
                }
                if process_event(&event_data)? {
                    break 'stream;
                }
            }
        }
        if let Some(event_data) = decoder.finish() {
            saw_event = true;
            let _ = process_event(&event_data)?;
        }

        if !saw_event && !raw.is_empty() {
            if raw.starts_with(b"RIFF") {
                let pcm = decode_wav(&raw)?;
                total_samples += pcm.samples.len();
                let _ = chunks.send(pcm);
            } else {
                let value: Value = serde_json::from_slice(&raw).map_err(|_| {
                    ProviderError::with_category(
                        DiagCategory::Model,
                        "千问语音合成返回的数据无法解析",
                    )
                })?;
                if let Some(err) = dashscope_json_error(LABEL, &value) {
                    return Err(err);
                }
                let _ = process_event(std::str::from_utf8(&raw).unwrap_or_default())?;
                if total_samples == 0
                    && let Some(audio_url) = value
                        .pointer("/output/audio/url")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|u| !u.is_empty())
                {
                    let safe_url = validate_audio_download_url(&self.tts_url, audio_url)?;
                    let dl = self
                        .client
                        .get(safe_url)
                        .timeout(AUDIO_TIMEOUT)
                        .send()
                        .await
                        .map_err(|error| describe_send_error(LABEL, &error))?;
                    let dl = check_status(LABEL, dl).await?;
                    let audio_bytes = read_body(LABEL, dl, MAX_AUDIO_BYTES).await?;
                    let pcm = if audio_bytes.starts_with(b"RIFF") {
                        decode_wav(&audio_bytes)?
                    } else {
                        let samples = pcm_samples_with_carry(&mut None, &audio_bytes);
                        Pcm {
                            samples,
                            sample_rate: TTS_SAMPLE_RATE,
                        }
                    };
                    if !pcm.samples.is_empty() {
                        total_samples += pcm.samples.len();
                        let _ = chunks.send(pcm);
                    }
                }
            }
        }

        if total_samples == 0 {
            return Err(ProviderError::with_category(
                DiagCategory::Model,
                "千问语音合成未返回有效音频数据（请确认音色 ID 与所选模型匹配）",
            ));
        }
        Ok(())
    }
}

impl Tts for DashscopeTts {
    fn synthesize<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
        Box::pin(async move {
            let (chunks_tx, mut chunks_rx) = unbounded_channel();
            self.synthesize_stream(text, chunks_tx).await?;
            let mut samples = Vec::new();
            let mut rate = TTS_SAMPLE_RATE;
            while let Ok(chunk) = chunks_rx.try_recv() {
                if chunk.sample_rate > 0 {
                    rate = chunk.sample_rate;
                }
                samples.extend(chunk.samples);
            }
            Ok(Pcm {
                samples,
                sample_rate: rate,
            })
        })
    }

    fn synthesize_stream<'a>(
        &'a self,
        text: &'a str,
        chunks: UnboundedSender<Pcm>,
    ) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move { self.speak(text, &chunks).await })
    }
}

// ---------------------------------------------------------------- 声音克隆与音色查询

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClonedVoiceInfo {
    pub voice_id: String,
    pub target_model: String,
    pub status: String,
    pub created_at: String,
}

/// 校验并返回支持 `voice-enrollment` 复刻的目标合成模型。
pub fn validate_clone_target_model(model: &str) -> Result<String, ProviderError> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return Ok(DEFAULT_TTS_MODEL.to_owned());
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("qwen3-tts") {
        return Err(ProviderError::with_category(
            DiagCategory::Config,
            "qwen3-tts 系列仅支持系统音色，不支持声音克隆；请将模型选为 qwen-audio-3.1-tts-flash 或 qwen-audio-3.1-tts-plus",
        ));
    }
    Ok(trimmed.to_owned())
}

/// 千问 `voice-enrollment` 的 `prefix` 仅允许英文字母与数字且不超过 10 个字符。
pub fn sanitize_voice_prefix(raw: &str) -> String {
    let stem = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .split('.')
        .next()
        .unwrap_or(raw);
    let clean: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .take(10)
        .collect();
    if clean.is_empty() {
        "moss".to_owned()
    } else {
        clean
    }
}

/// 校验本地上传的参考音频并封装为标准 Data URI（`data:audio/...;base64,...`）。
pub fn prepare_clone_audio_data_uri(
    audio_base64_or_data_url: &str,
    file_name: &str,
) -> Result<String, ProviderError> {
    let trimmed = audio_base64_or_data_url.trim();
    if trimmed.is_empty() {
        return Err(ProviderError::with_category(
            DiagCategory::Config,
            "未收到参考录音文件",
        ));
    }
    let b64_part = trimmed
        .split_once(',')
        .map(|(_, body)| body.trim())
        .unwrap_or(trimmed);
    if b64_part.len() > (MAX_CLONE_RAW_BYTES * 4 / 3) + 1024 {
        return Err(ProviderError::with_category(
            DiagCategory::Config,
            "参考录音体积过大（请控制在 7 MB 以内，推荐 10～20 秒清晰人声）",
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64_part)
        .map_err(|_| {
            ProviderError::with_category(DiagCategory::Config, "参考录音 Base64 编码无效")
        })?;
    if bytes.is_empty() {
        return Err(ProviderError::with_category(
            DiagCategory::Config,
            "参考录音文件为空",
        ));
    }
    if bytes.len() > MAX_CLONE_RAW_BYTES {
        return Err(ProviderError::with_category(
            DiagCategory::Config,
            "参考录音体积超过 7 MB 上限（请截取 10～20 秒清晰人声）",
        ));
    }
    let lower_name = file_name.trim().to_ascii_lowercase();
    let mime = if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        "audio/wav"
    } else if bytes.starts_with(b"ID3")
        || (bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0)
        || lower_name.ends_with(".mp3")
    {
        "audio/mpeg"
    } else if (bytes.len() >= 8 && &bytes[4..8] == b"ftyp") || lower_name.ends_with(".m4a") {
        "audio/mp4"
    } else if lower_name.ends_with(".wav") {
        "audio/wav"
    } else {
        return Err(ProviderError::with_category(
            DiagCategory::Config,
            "仅支持 WAV、MP3 或 M4A 格式的参考录音",
        ));
    };

    if mime == "audio/wav" {
        let pcm = decode_wav(&bytes).map_err(|err| {
            ProviderError::with_category(DiagCategory::Config, format!("参考 WAV 无法解析：{err}"))
        })?;
        if pcm.sample_rate < 16_000 {
            return Err(ProviderError::with_category(
                DiagCategory::Config,
                format!(
                    "参考录音采样率过低（当前 {} Hz，至少需要 16000 Hz）",
                    pcm.sample_rate
                ),
            ));
        }
        let secs = pcm.samples.len() as f64 / pcm.sample_rate as f64;
        if secs < 3.0 {
            return Err(ProviderError::with_category(
                DiagCategory::Config,
                format!(
                    "参考录音太短（当前 {secs:.1} 秒，至少需要 3 秒清晰连续说话，推荐 10～20 秒）"
                ),
            ));
        }
        if secs > 60.0 {
            return Err(ProviderError::with_category(
                DiagCategory::Config,
                format!("参考录音超过 60 秒上限（当前 {secs:.1} 秒，请截取 10～20 秒片段）"),
            ));
        }
    }

    let clean_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{mime};base64,{clean_b64}"))
}

async fn post_customization(
    http: &reqwest::Client,
    customization_url: &str,
    api_key: &str,
    body: &Value,
    timeout: Duration,
) -> Result<Value, ProviderError> {
    const LABEL: &str = "千问声音克隆";
    let wait = timeout.min(CLONE_REQUEST_TIMEOUT);
    if wait.is_zero() {
        return Err(ProviderError::with_category(
            DiagCategory::Network,
            "千问声音克隆等待总耗时超时",
        ));
    }
    let send = http
        .post(customization_url)
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .json(body)
        .send();
    let resp = tokio::time::timeout(wait, send)
        .await
        .map_err(|_| ProviderError::with_category(DiagCategory::Network, "千问声音克隆请求超时"))?
        .map_err(|error| describe_send_error(LABEL, &error))?;
    let resp = check_status(LABEL, resp).await?;
    let bytes = tokio::time::timeout(wait, read_body(LABEL, resp, MAX_ERROR_BYTES * 8))
        .await
        .map_err(|_| {
            ProviderError::with_category(DiagCategory::Network, "千问声音克隆响应接收超时")
        })??;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        ProviderError::with_category(DiagCategory::Model, "千问声音克隆返回的数据无法解析")
    })?;
    if let Some(err) = dashscope_json_error(LABEL, &value) {
        return Err(err);
    }
    Ok(value)
}

/// 上传参考音频创建复刻音色，并轮询等待服务端特征提取部署就绪（`status == "OK"`），返回最终 `voice_id`。
pub async fn enroll_voice(
    url: &str,
    api_key: &str,
    target_model: &str,
    prefix: &str,
    audio_data_uri: &str,
) -> Result<String, ProviderError> {
    let customization_url = dashscope_customization_url(url);
    validate_endpoint_and_key("千问声音克隆", &customization_url, api_key)?;
    let target_model = validate_clone_target_model(target_model)?;
    let prefix = sanitize_voice_prefix(prefix);
    let http = client()?;
    let deadline = Instant::now() + CLONE_TOTAL_TIMEOUT;

    let create_body = json!({
        "model": "voice-enrollment",
        "input": {
            "action": "create_voice",
            "target_model": target_model,
            "prefix": prefix,
            "audio": audio_data_uri,
            "language_hints": ["zh"],
        }
    });
    let created = post_customization(
        &http,
        &customization_url,
        api_key,
        &create_body,
        deadline.saturating_duration_since(Instant::now()),
    )
    .await?;
    let voice_id = created
        .pointer("/output/voice_id")
        .or_else(|| created.pointer("/output/voice"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ProviderError::with_category(DiagCategory::Model, "千问声音克隆未返回音色 ID")
        })?
        .to_owned();

    let query_body = json!({
        "model": "voice-enrollment",
        "input": {
            "action": "query_voice",
            "voice_id": voice_id,
        }
    });
    for attempt in 0..CLONE_MAX_POLLS {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let queried =
            post_customization(&http, &customization_url, api_key, &query_body, remaining).await?;
        let status = queried
            .pointer("/output/status")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("OK");
        if status.eq_ignore_ascii_case("OK")
            || status.eq_ignore_ascii_case("PUBLISHED")
            || status.eq_ignore_ascii_case("SUCCESS")
            || status.eq_ignore_ascii_case("SUCCEEDED")
        {
            return Ok(voice_id);
        }
        if status.eq_ignore_ascii_case("DEPLOYING")
            || status.eq_ignore_ascii_case("INIT")
            || status.eq_ignore_ascii_case("TRAINING")
            || status.eq_ignore_ascii_case("PROCESSING")
            || status.eq_ignore_ascii_case("PENDING")
            || status.eq_ignore_ascii_case("AUDITING")
            || status.eq_ignore_ascii_case("QUEUED")
            || status.eq_ignore_ascii_case("RUNNING")
        {
            if attempt + 1 < CLONE_MAX_POLLS {
                let sleep_for =
                    CLONE_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now()));
                if sleep_for.is_zero() {
                    break;
                }
                tokio::time::sleep(sleep_for).await;
                continue;
            }
        } else {
            let safe_status = sanitize_short_token(status, "UNQUALIFIED");
            return Err(ProviderError::with_category(
                DiagCategory::Config,
                format!(
                    "声音克隆未通过审核或质量检测（状态 {safe_status}）：请换一段 10～20 秒清晰、无背景音乐的单人说话录音重试"
                ),
            ));
        }
    }
    Err(ProviderError::with_category(
        DiagCategory::Network,
        format!("音色已创建（{voice_id}），但等待云端就绪超时；稍后可在音色下拉框读取选用"),
    ))
}

/// 读取当前千问 / 百炼账号下已创建的复刻音色列表（不返回密钥或样本音频链接）。
pub async fn list_cloned_voices(
    url: &str,
    api_key: &str,
    preferred_model: Option<&str>,
) -> Result<Vec<ClonedVoiceInfo>, ProviderError> {
    let customization_url = dashscope_customization_url(url);
    validate_endpoint_and_key("千问声音克隆", &customization_url, api_key)?;
    let http = client()?;
    let body = json!({
        "model": "voice-enrollment",
        "input": {
            "action": "list_voice",
            "page_index": 0,
            "page_size": 50,
        }
    });
    let value = post_customization(
        &http,
        &customization_url,
        api_key,
        &body,
        CLONE_REQUEST_TIMEOUT,
    )
    .await?;
    let mut list = Vec::new();
    if let Some(items) = value
        .pointer("/output/voice_list")
        .or_else(|| value.pointer("/output/voices"))
        .and_then(Value::as_array)
    {
        for item in items {
            let Some(voice_id) = item
                .get("voice_id")
                .or_else(|| item.get("voice"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("OK")
                .to_owned();
            if !status.eq_ignore_ascii_case("OK")
                && !status.eq_ignore_ascii_case("PUBLISHED")
                && !status.eq_ignore_ascii_case("DEPLOYING")
                && !status.eq_ignore_ascii_case("PROCESSING")
                && !status.eq_ignore_ascii_case("PENDING")
                && !status.eq_ignore_ascii_case("AUDITING")
                && !status.eq_ignore_ascii_case("TRAINING")
                && !status.eq_ignore_ascii_case("INIT")
                && !status.is_empty()
            {
                continue;
            }
            let target_model = item
                .get("target_model")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or_default()
                .to_owned();
            let created_at = item
                .get("gmt_create")
                .or_else(|| item.get("create_time"))
                .or_else(|| item.get("created_at"))
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or_default()
                .to_owned();
            list.push(ClonedVoiceInfo {
                voice_id: voice_id.to_owned(),
                target_model,
                status,
                created_at,
            });
        }
    }
    if let Some(model) = preferred_model.map(str::trim).filter(|s| !s.is_empty()) {
        list.sort_by_key(|v| !v.target_model.eq_ignore_ascii_case(model));
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wav::encode_wav;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn url_normalization_handles_compatible_mode_and_endpoints() {
        assert_eq!(
            dashscope_tts_url(""),
            "https://maas.qianwenaiapi.com/api/v1/services/audio/tts/SpeechSynthesizer"
        );
        assert_eq!(
            dashscope_tts_url("https://maas.qianwenaiapi.com/compatible-mode/v1"),
            "https://maas.qianwenaiapi.com/api/v1/services/audio/tts/SpeechSynthesizer"
        );
        assert_eq!(
            dashscope_customization_url(
                "https://maas.qianwenaiapi.com/api/v1/services/audio/tts/SpeechSynthesizer"
            ),
            "https://maas.qianwenaiapi.com/api/v1/services/audio/tts/customization"
        );
        assert_eq!(
            dashscope_tts_url("https://dashscope.aliyuncs.com"),
            "https://dashscope.aliyuncs.com/api/v1/services/audio/tts/SpeechSynthesizer"
        );
    }

    #[test]
    fn audio_validation_checks_wav_duration_and_sanitizes_prefix() {
        assert_eq!(sanitize_voice_prefix("我的550W_MOSS声线.wav"), "550wmoss");
        assert_eq!(sanitize_voice_prefix("纯中文名称.mp3"), "moss");

        let short_wav = encode_wav(&vec![1000i16; 16_000], 16_000);
        let short_b64 = base64::engine::general_purpose::STANDARD.encode(&short_wav);
        assert!(
            prepare_clone_audio_data_uri(&short_b64, "short.wav")
                .unwrap_err()
                .to_string()
                .contains("太短")
        );

        let good_wav = encode_wav(&vec![1200i16; 24_000 * 4], 24_000);
        let good_b64 = format!(
            "data:audio/wav;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&good_wav)
        );
        let uri = prepare_clone_audio_data_uri(&good_b64, "sample.wav").unwrap();
        assert!(uri.starts_with("data:audio/wav;base64,"));
        assert!(validate_clone_target_model("qwen3-tts-flash").is_err());
        assert_eq!(
            validate_clone_target_model("").unwrap(),
            "qwen-audio-3.1-tts-flash"
        );
    }

    #[tokio::test]
    async fn sse_streaming_synthesis_and_voice_enrollment_round_trip() {
        async fn read_http_request(sock: &mut tokio::net::TcpStream) -> String {
            let mut raw = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = sock.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&chunk[..n]);
                if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let body_start = pos + 4;
                    let head = String::from_utf8_lossy(&raw[..pos]);
                    let content_len = head
                        .lines()
                        .find_map(|line| {
                            let (k, v) = line.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if raw.len() >= body_start + content_len {
                        break;
                    }
                }
            }
            String::from_utf8_lossy(&raw).into_owned()
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            // 1) create_voice -> 2) query_voice -> 3) SpeechSynthesizer SSE
            for step in 0..3 {
                let (mut sock, _) = listener.accept().await.unwrap();
                let req = read_http_request(&mut sock).await;
                assert!(
                    req.contains("authorization: Bearer sk-test-secret")
                        || req.contains("Authorization: Bearer sk-test-secret"),
                    "missing bearer header in: {req}"
                );
                let resp = match step {
                    0 => {
                        assert!(req.contains("POST /api/v1/services/audio/tts/customization"));
                        assert!(req.contains("\"create_voice\""));
                        let body = r#"{"output":{"voice_id":"qwen-audio-3.1-tts-flash-moss-abc1"},"request_id":"r1"}"#;
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    }
                    1 => {
                        assert!(req.contains("\"query_voice\""));
                        let body = r#"{"output":{"status":"OK"},"request_id":"r2"}"#;
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    }
                    _ => {
                        assert!(req.contains("POST /api/v1/services/audio/tts/SpeechSynthesizer"));
                        assert!(
                            req.contains("x-dashscope-sse: enable")
                                || req.contains("X-DashScope-SSE: enable")
                        );
                        let pcm_bytes: Vec<u8> = [1000i16, -1000, 2000, -2000]
                            .into_iter()
                            .flat_map(i16::to_le_bytes)
                            .collect();
                        let b64_a =
                            base64::engine::general_purpose::STANDARD.encode(&pcm_bytes[..3]);
                        let b64_b =
                            base64::engine::general_purpose::STANDARD.encode(&pcm_bytes[3..]);
                        let sse = format!(
                            "id:1\nevent:result\ndata:{{\"output\":{{\"finish_reason\":\"null\",\"audio\":{{\"data\":\"{b64_a}\"}}}}}}\n\nid:2\nevent:result\ndata:{{\"output\":{{\"finish_reason\":\"stop\",\"audio\":{{\"data\":\"{b64_b}\"}}}}}}\n\n"
                        );
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            sse.len(),
                            sse
                        )
                    }
                };
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });

        let base = format!("http://{addr}/compatible-mode/v1");
        let voice_id = enroll_voice(
            &base,
            "sk-test-secret",
            "qwen-audio-3.1-tts-flash",
            "550w_voice",
            "data:audio/wav;base64,AAAA",
        )
        .await
        .unwrap();
        assert_eq!(voice_id, "qwen-audio-3.1-tts-flash-moss-abc1");

        let tts = DashscopeTts::new(
            &base,
            "sk-test-secret",
            "qwen-audio-3.1-tts-flash",
            &voice_id,
            1.0,
        )
        .unwrap();
        let pcm = tts.synthesize("你好").await.unwrap();
        assert_eq!(pcm.sample_rate, 24_000);
        assert_eq!(pcm.samples, vec![1000, -1000, 2000, -2000]);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn non_streaming_json_url_fallback_works_and_blocks_ssrf_and_cleartext_http() {
        // 公网 HTTP 地址被拒绝（防明文传输密钥与参考音频）
        assert!(
            DashscopeTts::new(
                "http://maas.qianwenaiapi.com/api/v1",
                "sk-test",
                "qwen-audio-3.1-tts-flash",
                "longanhuan_v3.1",
                1.0,
            )
            .is_err()
        );
        // 公网端点下，返回的音频下载链接不得指向回环、私网、IPv4-mapped IPv6 或明文 HTTP
        let prod_tts = "https://maas.qianwenaiapi.com/api/v1/services/audio/tts/SpeechSynthesizer";
        assert!(validate_audio_download_url(prod_tts, "http://127.0.0.1:8080/a.wav").is_err());
        assert!(validate_audio_download_url(prod_tts, "https://192.168.1.10/a.wav").is_err());
        assert!(validate_audio_download_url(prod_tts, "https://169.254.169.254/latest").is_err());
        assert!(validate_audio_download_url(prod_tts, "https://[::ffff:127.0.0.1]/a.wav").is_err());
        assert!(validate_audio_download_url(prod_tts, "http://cdn.example.com/a.wav").is_err());
        assert!(validate_audio_download_url(prod_tts, "https://cdn.example.com/a.wav").is_ok());

        // 非流式 JSON 返回 finish_reason=="stop" 且仅含 output.audio.url 时正常走回退下载
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let wav_bytes = encode_wav(&[321i16, -321, 654, -654], 24_000);
        let server = tokio::spawn(async move {
            for step in 0..2 {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await.unwrap();
                if step == 0 {
                    let body = format!(
                        r#"{{"output":{{"finish_reason":"stop","audio":{{"data":"","url":"http://{addr}/out.wav"}}}}}}"#
                    );
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    sock.write_all(resp.as_bytes()).await.unwrap();
                } else {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        wav_bytes.len()
                    );
                    sock.write_all(head.as_bytes()).await.unwrap();
                    sock.write_all(&wav_bytes).await.unwrap();
                }
            }
        });
        let tts = DashscopeTts::new(
            &format!("http://{addr}/api/v1"),
            "sk-test-secret",
            "qwen-audio-3.1-tts-flash",
            "longanhuan_v3.1",
            1.0,
        )
        .unwrap();
        let pcm = tts.synthesize("测试回退").await.unwrap();
        assert_eq!(pcm.samples, vec![321, -321, 654, -654]);
        server.await.unwrap();
    }
}
