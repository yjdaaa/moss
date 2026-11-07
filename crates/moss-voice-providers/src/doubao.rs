//! 豆包（火山引擎）流式语音识别与双向流式语音合成（内置语音技术书 NV7）。
//!
//! 帧协议移植自 univoice（MIT，见 [`sauc`]、[`bidirection`] 的说明）；连接、鉴权、超时与错误处理按 MOSS 规则重写：
//! - 依赖与仓库其他部分一致（tokio-tungstenite 0.29 与 axum 共用、rustls），不引入 univoice 的命令行与日志依赖；
//! - 错误信息只有中文原因，**不含地址与密钥**；服务端说明经 [`crate::safe_hint`] 过滤后才回显。
//!
//! 鉴权：新版控制台只需 API Key（请求头 `X-Api-Key`）；旧版控制台把“APP ID:Access Token”填在密钥栏，
//! 改用 `X-Api-App-Key` 与 `X-Api-Access-Key`。“模型”一栏填资源 ID（`X-Api-Resource-Id`）。
//!
//! 识别（[`DoubaoAsr`]）：用户开始说话时建立连接并发参数帧，之后每 100 ms 左右发一包 16 kHz PCM，说完只需
//! 发最后一包、等最终结果——识别时间大部分被说话时间掩盖。也实现整段识别（[`Asr`]），作为流式失败时的退路。
//!
//! 合成（[`DoubaoTts`]）：每句话一个会话，音频分段到达即交给播放；一次连接可以连续开多个会话，空闲连接留一条
//! 给下一句复用（省去握手），复用的连接已被服务端关闭时自动重连一次。

pub mod bidirection;
pub mod sauc;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{self, Message as WsMessage};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use crate::resample::Resampler;
use crate::{Asr, AsrStream, BoxFuture, Pcm, ProviderError, StreamingAsr, Tts, safe_hint};
use bidirection::{Message, MsgType};

/// 流式识别默认地址（优化版双向流式：结果有变化才推送）。
pub const DEFAULT_ASR_URL: &str = "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async";
/// 识别默认资源 ID（豆包流式语音识别模型 1.0，小时版）。
pub const DEFAULT_ASR_RESOURCE: &str = "volc.bigasr.sauc.duration";
/// 双向流式合成默认地址。
pub const DEFAULT_TTS_URL: &str = "wss://openspeech.bytedance.com/api/v3/tts/bidirection";
/// 合成默认资源 ID（豆包语音合成模型 2.0）。
pub const DEFAULT_TTS_RESOURCE: &str = "seed-tts-2.0";
/// 合成输出采样率（与设备下行一致，不需要重采样）。
pub const TTS_SAMPLE_RATE: u32 = 24_000;
/// 识别要求的输入采样率。
pub const ASR_SAMPLE_RATE: u32 = 16_000;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 说完（发出最后一包）后等最终结果的上限
const FINAL_TIMEOUT: Duration = Duration::from_secs(10);
/// 整段识别的总上限
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(30);
/// 合成时两条消息之间的最长间隔
const TTS_IDLE_TIMEOUT: Duration = Duration::from_secs(15);
/// 一句话合成的总上限
const TTS_TOTAL_TIMEOUT: Duration = Duration::from_secs(60);
/// 每包音频的样本数（100 ms；文档建议 100–200 ms 一包）
const PACKET_SAMPLES: usize = 1_600;
/// 单条 WebSocket 消息上限
const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// 连接参数。`Debug` 不输出密钥。
#[derive(Clone)]
pub struct DoubaoConfig {
    pub url: String,
    pub api_key: String,
    pub resource_id: String,
}

impl std::fmt::Debug for DoubaoConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DoubaoConfig")
            .field("url", &self.url)
            .field("api_key", &if self.api_key.is_empty() { "" } else { "***" })
            .field("resource_id", &self.resource_id)
            .finish()
    }
}

impl DoubaoConfig {
    fn new(
        url: &str,
        api_key: &str,
        resource_id: &str,
        default_url: &str,
    ) -> Result<Self, ProviderError> {
        let url = url.trim();
        let url = if url.is_empty() { default_url } else { url };
        if !(url.starts_with("wss://") || url.starts_with("ws://")) {
            return Err(ProviderError::with_category(
                crate::DiagCategory::Config,
                "豆包地址必须以 wss:// 开头",
            ));
        }
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err(ProviderError::with_category(
                crate::DiagCategory::Auth,
                "豆包需要填写密钥（API Key）",
            ));
        }
        let resource_id = resource_id.trim();
        if resource_id.is_empty() {
            return Err(ProviderError::with_category(
                crate::DiagCategory::Config,
                "豆包需要填写资源 ID",
            ));
        }
        Ok(Self {
            url: url.to_owned(),
            api_key: api_key.to_owned(),
            resource_id: resource_id.to_owned(),
        })
    }
}

/// 鉴权请求头（不含连接号）。“APP ID:Access Token”（APP ID 为纯数字）按旧版控制台处理。
pub fn auth_headers(api_key: &str, resource_id: &str) -> Vec<(&'static str, String)> {
    let mut headers = match api_key.split_once(':') {
        Some((app, token))
            if !app.is_empty() && app.chars().all(|c| c.is_ascii_digit()) && !token.is_empty() =>
        {
            vec![
                ("X-Api-App-Key", app.to_owned()),
                ("X-Api-Access-Key", token.to_owned()),
            ]
        }
        _ => vec![("X-Api-Key", api_key.to_owned())],
    };
    headers.push(("X-Api-Resource-Id", resource_id.to_owned()));
    headers
}

/// 随机 UUID（v4 格式），用作连接号与会话号。
fn uuid() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// TLS 配置：明确使用 ring 与内置根证书，不依赖进程级默认加密实现。
fn tls_config() -> Result<Arc<rustls::ClientConfig>, ProviderError> {
    static CONFIG: OnceLock<Option<Arc<rustls::ClientConfig>>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .ok()
            .map(|builder| Arc::new(builder.with_root_certificates(roots).with_no_client_auth()))
        })
        .clone()
        .ok_or_else(|| {
            ProviderError::with_category(crate::DiagCategory::Internal, "TLS 初始化失败")
        })
}

/// 不使用 tungstenite 的原始描述（可能带上地址）；底层原因脱敏后放进技术细节。
fn describe_connect_error(label: &str, error: tungstenite::Error) -> ProviderError {
    use crate::DiagCategory::{Auth, Config, Network, Quota};
    match error {
        tungstenite::Error::Http(response) => {
            let status = response.status().as_u16();
            let upstream = response
                .body()
                .as_deref()
                .and_then(|body| crate::upstream_text(&String::from_utf8_lossy(body)));
            let (category, reason, hint) = match status {
                401 | 403 => (
                    Auth,
                    "，密钥或资源 ID 不对，或没有开通该服务",
                    "到火山引擎控制台核对 API Key 与资源 ID，并确认已开通这项语音服务",
                ),
                404 => (
                    Config,
                    "，地址不对",
                    "把地址恢复成默认值，或对照火山引擎文档填写",
                ),
                429 => (
                    Quota,
                    "，请求太频繁或额度不足",
                    "等一会儿再试，或到控制台查看并发与额度",
                ),
                500..=599 => (
                    crate::DiagCategory::Model,
                    "，服务方服务器出错",
                    "这是服务方那边的问题：稍后再试",
                ),
                _ => (Network, "", "具体原因见技术细节"),
            };
            let error = ProviderError::with_category(
                category,
                format!("{label}连接被拒绝（HTTP {status}{reason}）"),
            )
            .hint(hint);
            match upstream {
                Some(text) => error.detail(format!("服务方原话：{text}")),
                None => error,
            }
        }
        tungstenite::Error::Io(io) => crate::openai::classify_transport_error(
            label,
            io.kind() == std::io::ErrorKind::TimedOut,
            true,
            &io.to_string(),
        ),
        tungstenite::Error::Tls(tls) => ProviderError::with_category(
            Network,
            format!("{label}安全连接失败：TLS 握手或证书校验没通过"),
        )
        .hint("确认电脑时间正确；代理软件替换了证书时也会这样")
        .detail(tls.to_string()),
        tungstenite::Error::Url(_) => {
            ProviderError::with_category(Config, format!("{label}地址无效"))
                .hint("地址应以 wss:// 开头，可以清空恢复默认地址")
        }
        other => {
            ProviderError::with_category(Network, format!("{label}连接失败（网络或协议错误）"))
                .hint("检查电脑网络后再试；具体原因见技术细节")
                .detail(other.to_string())
        }
    }
}

/// 连接已建立后中途断开（发送或接收失败）。
fn network_drop(label: &str, error: &tungstenite::Error) -> ProviderError {
    ProviderError::with_category(
        crate::DiagCategory::Network,
        format!("{label}连接中断：数据传到一半网络断了"),
    )
    .hint("多半是网络不稳；稍后再试，一直这样就换个网络")
    .detail(error.to_string())
}

/// 服务方主动关闭了连接（没给出原因）。
fn server_closed(label: &str) -> ProviderError {
    ProviderError::new(format!("{label}连接被服务端关闭"))
        .hint("服务方提前结束了这次请求：检查资源 ID、音色是否已开通，或稍后再试")
}

async fn connect(config: &DoubaoConfig, label: &str) -> Result<Ws, ProviderError> {
    let mut request = config.url.as_str().into_client_request().map_err(|_| {
        ProviderError::with_category(crate::DiagCategory::Config, format!("{label}地址无效"))
    })?;
    let headers = request.headers_mut();
    for (name, value) in auth_headers(&config.api_key, &config.resource_id)
        .into_iter()
        .chain([("X-Api-Connect-Id", uuid())])
    {
        let value = HeaderValue::from_str(&value).map_err(|_| {
            ProviderError::with_category(
                crate::DiagCategory::Config,
                format!("{label}密钥或资源 ID 含有不能放进请求头的字符"),
            )
        })?;
        headers.insert(name, value);
    }
    let connector = if config.url.starts_with("wss://") {
        Some(Connector::Rustls(tls_config()?))
    } else {
        None
    };
    let ws_config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let connecting =
        tokio_tungstenite::connect_async_tls_with_config(request, Some(ws_config), true, connector);
    match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(error)) => Err(describe_connect_error(label, error)),
        Err(_) => Err(ProviderError::with_category(
            crate::DiagCategory::Network,
            format!("{label}连接超时：10 秒内没连上服务器"),
        )
        .hint("检查电脑网络；服务方繁忙时稍后再试")),
    }
}

// ---------------------------------------------------------------- 识别

/// 豆包流式语音识别。
pub struct DoubaoAsr {
    config: Arc<DoubaoConfig>,
    hotwords: Vec<String>,
}

impl DoubaoAsr {
    /// `url` 留空用 [`DEFAULT_ASR_URL`]；`resource_id` 即页面上的“模型”。
    pub fn new(url: &str, api_key: &str, resource_id: &str) -> Result<Self, ProviderError> {
        Ok(Self {
            config: Arc::new(DoubaoConfig::new(
                url,
                api_key,
                resource_id,
                DEFAULT_ASR_URL,
            )?),
            hotwords: Vec::new(),
        })
    }
    pub fn with_hotwords(mut self, words: &[String]) -> Self {
        // 官方限制 100 tokens；保守限制为 64 个字符，超出时保留前面的词。
        let mut count = 0;
        for word in words.iter().take(64) {
            let word: String = word.chars().filter(|c| !c.is_control()).take(32).collect();
            let length = word.chars().count();
            if length > 0 && count + length <= 64 {
                count += length;
                self.hotwords.push(word);
            }
        }
        self
    }
}

fn full_request_with_hotwords(hotwords: &[String]) -> sauc::FullRequest {
    sauc::FullRequest {
        user: sauc::User { uid: "moss".into() },
        audio: sauc::AudioConfig { format: "pcm".into(), codec: "raw".into(), rate: ASR_SAMPLE_RATE, bits: 16, channel: 1 },
        request: sauc::RequestConfig {
            model_name: "bigmodel".into(), enable_itn: true, enable_punc: true, enable_ddc: false,
            corpus: (!hotwords.is_empty()).then(|| sauc::Corpus { context: serde_json::json!({"hotwords":hotwords.iter().map(|word| serde_json::json!({"word":word})).collect::<Vec<_>>()}).to_string() }),
        },
    }
}

async fn send_binary<S>(write: &mut S, bytes: Vec<u8>, label: &str) -> Result<(), ProviderError>
where
    S: futures_util::Sink<WsMessage, Error = tungstenite::Error> + Unpin,
{
    write
        .send(WsMessage::Binary(bytes.into()))
        .await
        .map_err(|error| network_drop(label, &error))
}

/// 一段识别的完整过程：连接、发参数帧、边收音频边发包，音频通道关闭（说完）后发最后一包并等最终结果。
async fn recognize(
    config: Arc<DoubaoConfig>,
    mut audio: UnboundedReceiver<Vec<i16>>,
    interim: Option<tokio::sync::watch::Sender<String>>,
    hotwords: Vec<String>,
) -> Result<String, ProviderError> {
    const LABEL: &str = "豆包识别";
    let ws = connect(&config, LABEL).await?;
    let (mut write, mut read) = ws.split();
    send_binary(
        &mut write,
        sauc::encode_full_request(&full_request_with_hotwords(&hotwords))?,
        LABEL,
    )
    .await?;
    let mut pending: Vec<i16> = Vec::new();
    let mut sequence: i32 = 2;
    let mut ready = false;
    let mut audio_open = true;
    let mut last_sent = false;
    let mut text = String::new();
    // 等参数帧确认的期限；发出最后一包后改为等最终结果的期限；说话期间不限（会话层限制一段话最长 30 秒）
    let mut deadline = Some(Instant::now() + CONNECT_TIMEOUT);
    loop {
        if ready {
            while pending.len() >= PACKET_SAMPLES {
                let packet: Vec<i16> = pending.drain(..PACKET_SAMPLES).collect();
                send_binary(
                    &mut write,
                    sauc::encode_audio(sequence, &packet, false)?,
                    LABEL,
                )
                .await?;
                sequence += 1;
            }
            if !audio_open && !last_sent {
                let rest = std::mem::take(&mut pending);
                send_binary(
                    &mut write,
                    sauc::encode_audio(sequence, &rest, true)?,
                    LABEL,
                )
                .await?;
                last_sent = true;
                deadline = Some(Instant::now() + FINAL_TIMEOUT);
            }
        }
        let wait = deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            chunk = audio.recv(), if audio_open => match chunk {
                Some(chunk) => pending.extend_from_slice(&chunk),
                None => audio_open = false,
            },
            message = read.next() => {
                let data = match message {
                    Some(Ok(WsMessage::Binary(data))) => data,
                    Some(Ok(WsMessage::Close(_))) | None => return Err(server_closed(LABEL)),
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => return Err(network_drop(LABEL, &error)),
                };
                let response = sauc::parse_response(&data)?;
                if response.code != 0 {
                    return Err(ProviderError::new(format!(
                        "{LABEL}出错（{}）{}",
                        sauc::error_message(response.code),
                        safe_hint(&response.message)
                    )));
                }
                if !ready {
                    ready = true;
                    if audio_open {
                        deadline = None;
                    }
                }
                // 每条结果都是到目前为止的完整文字；空结果不覆盖已有文字
                if let Some(result) = response.payload.and_then(|payload| payload.result)
                    && !result.text.trim().is_empty()
                {
                    text = result.text;
                    if let Some(interim) = &interim {
                        interim.send_if_modified(|current| {
                            if *current == text { false } else { *current = text.clone(); true }
                        });
                    }
                }
                if response.is_last {
                    let _ = write.send(WsMessage::Close(None)).await;
                    return Ok(text.trim().to_owned());
                }
            }
            _ = tokio::time::sleep_until(wait) => {
                return Err(ProviderError::new(if ready { format!("{LABEL}等待结果超时") } else { format!("{LABEL}服务没有响应") }));
            }
        }
    }
}

impl StreamingAsr for DoubaoAsr {
    fn start(&self) -> AsrStream {
        let (audio_tx, audio_rx) = unbounded_channel();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let (interim_tx, interim) = tokio::sync::watch::channel(String::new());
        let config = self.config.clone();
        let hotwords = self.hotwords.clone();
        let task = tokio::spawn(async move {
            let _ = result_tx.send(recognize(config, audio_rx, Some(interim_tx), hotwords).await);
        });
        AsrStream::new(audio_tx, result_rx, task).with_interim(interim)
    }
}

impl Asr for DoubaoAsr {
    /// 整段识别：一次送完整段音频（流式识别失败时的退路，也用于“测试连接”）。
    fn transcribe<'a>(
        &'a self,
        pcm: &'a [i16],
        sample_rate: u32,
    ) -> BoxFuture<'a, Result<String, ProviderError>> {
        Box::pin(async move {
            let (audio_tx, audio_rx) = unbounded_channel();
            let samples = Resampler::new(sample_rate, ASR_SAMPLE_RATE).process(pcm);
            for chunk in samples.chunks(PACKET_SAMPLES) {
                let _ = audio_tx.send(chunk.to_vec());
            }
            drop(audio_tx);
            tokio::time::timeout(
                TRANSCRIBE_TIMEOUT,
                recognize(self.config.clone(), audio_rx, None, self.hotwords.clone()),
            )
            .await
            .unwrap_or_else(|_| Err(ProviderError::new("豆包识别超时")))
        })
    }
}

// ---------------------------------------------------------------- 合成

/// 豆包双向流式语音合成。
pub struct DoubaoTts {
    config: DoubaoConfig,
    speaker: String,
    /// 语速：-50（0.5 倍）~ 100（2 倍），0 为正常
    speech_rate: i32,
    /// 上一句用完的连接，留给下一句复用
    idle: Mutex<Option<Ws>>,
}

/// 合成失败：`sent` 表示失败前是否已经送出过音频（送出过就不能重试，否则会重复播放）。
struct SessionError {
    error: ProviderError,
    sent: bool,
}

impl DoubaoTts {
    /// `speed` 为倍速（页面上的 0.25 ~ 4.0），换算为豆包的 -50 ~ 100。
    pub fn new(
        url: &str,
        api_key: &str,
        resource_id: &str,
        speaker: &str,
        speed: f32,
    ) -> Result<Self, ProviderError> {
        let speaker = speaker.trim();
        if speaker.is_empty() {
            return Err(ProviderError::with_category(
                crate::DiagCategory::Config,
                "豆包合成需要填写音色",
            ));
        }
        Ok(Self {
            config: DoubaoConfig::new(url, api_key, resource_id, DEFAULT_TTS_URL)?,
            speaker: speaker.to_owned(),
            speech_rate: speech_rate(speed),
            idle: Mutex::new(None),
        })
    }

    fn audio_params(&self) -> serde_json::Value {
        serde_json::json!({"format": "pcm", "sample_rate": TTS_SAMPLE_RATE, "speech_rate": self.speech_rate})
    }

    fn session_payload(&self, event: i32, text: Option<&str>) -> Vec<u8> {
        let mut params =
            serde_json::json!({"speaker": self.speaker, "audio_params": self.audio_params()});
        if let Some(text) = text {
            params["text"] = serde_json::Value::String(text.to_owned());
        }
        serde_json::json!({
            "user": {"uid": "moss"},
            "event": event,
            "namespace": "BidirectionalTTS",
            "req_params": params,
        })
        .to_string()
        .into_bytes()
    }

    /// 新建连接并完成连接握手。
    async fn open(&self) -> Result<Ws, ProviderError> {
        let mut ws = connect(&self.config, "豆包合成").await?;
        ws.send(WsMessage::Binary(
            bidirection::marshal(&Message::start_connection()).into(),
        ))
        .await
        .map_err(|error| network_drop("豆包合成", &error))?;
        loop {
            let message = next_message(&mut ws).await?;
            match (message.msg_type, message.event) {
                (MsgType::FullServerResponse, Some(bidirection::CONNECTION_STARTED)) => {
                    return Ok(ws);
                }
                (_, Some(bidirection::CONNECTION_FAILED)) | (MsgType::Error, _) => {
                    return Err(service_error(&message));
                }
                _ => {}
            }
        }
    }

    /// 在一条连接上合成一句话；成功时把连接还回来供复用。
    async fn session(
        &self,
        mut ws: Ws,
        text: &str,
        chunks: &UnboundedSender<Pcm>,
    ) -> Result<Ws, SessionError> {
        let fail = |error: ProviderError, sent: bool| SessionError { error, sent };
        let session_id = uuid();
        let start = Message::start_session(
            &session_id,
            self.session_payload(bidirection::START_SESSION, None),
        );
        send_message(&mut ws, &start)
            .await
            .map_err(|error| fail(error, false))?;
        loop {
            let message = next_message(&mut ws)
                .await
                .map_err(|error| fail(error, false))?;
            match (message.msg_type, message.event) {
                (MsgType::FullServerResponse, Some(bidirection::SESSION_STARTED)) => break,
                (_, Some(bidirection::SESSION_FAILED | bidirection::CONNECTION_FAILED))
                | (MsgType::Error, _) => {
                    return Err(fail(service_error(&message), false));
                }
                _ => {}
            }
        }
        let task = Message::task_request(
            &session_id,
            self.session_payload(bidirection::TASK_REQUEST, Some(text)),
        );
        send_message(&mut ws, &task)
            .await
            .map_err(|error| fail(error, false))?;
        send_message(&mut ws, &Message::finish_session(&session_id))
            .await
            .map_err(|error| fail(error, false))?;
        let mut sent = false;
        // 一个样本两个字节：分包可能在样本中间断开，余下的半个样本留到下一包
        let mut carry: Option<u8> = None;
        loop {
            let message = next_message(&mut ws)
                .await
                .map_err(|error| fail(error, sent))?;
            match (message.msg_type, message.event) {
                (MsgType::AudioOnlyServer, _) => {
                    let samples = pcm_samples(&mut carry, &message.payload);
                    if !samples.is_empty() {
                        sent = true;
                        let _ = chunks.send(Pcm {
                            samples,
                            sample_rate: TTS_SAMPLE_RATE,
                        });
                    }
                }
                (MsgType::FullServerResponse, Some(bidirection::SESSION_FINISHED)) => {
                    let status = serde_json::from_slice::<serde_json::Value>(&message.payload)
                        .ok()
                        .and_then(|value| {
                            value.get("status_code").and_then(serde_json::Value::as_i64)
                        });
                    return match status {
                        Some(code) if code != 20_000_000 => {
                            Err(fail(service_error(&message), sent))
                        }
                        _ if !sent => Err(fail(ProviderError::new("豆包合成没有返回音频"), false)),
                        _ => Ok(ws),
                    };
                }
                (
                    _,
                    Some(
                        bidirection::SESSION_FAILED
                        | bidirection::SESSION_CANCELED
                        | bidirection::CONNECTION_FAILED,
                    ),
                )
                | (MsgType::Error, _) => return Err(fail(service_error(&message), sent)),
                _ => {}
            }
        }
    }

    async fn speak(&self, text: &str, chunks: &UnboundedSender<Pcm>) -> Result<(), ProviderError> {
        let reused = self.idle.lock().ok().and_then(|mut idle| idle.take());
        let (ws, was_reused) = match reused {
            Some(ws) => (ws, true),
            None => (self.open().await?, false),
        };
        let ws = match self.session(ws, text, chunks).await {
            Ok(ws) => ws,
            // 复用的连接可能已被服务端关闭：还没送出音频时换新连接重试一次
            Err(SessionError { sent: false, .. }) if was_reused => {
                let ws = self.open().await?;
                self.session(ws, text, chunks)
                    .await
                    .map_err(|failure| failure.error)?
            }
            Err(failure) => return Err(failure.error),
        };
        if let Ok(mut idle) = self.idle.lock()
            && idle.is_none()
        {
            *idle = Some(ws);
        }
        Ok(())
    }
}

/// 倍速 → 豆包 `speech_rate`（-50 ~ 100，线性：0.5 倍为 -50，2 倍为 100）。
pub fn speech_rate(speed: f32) -> i32 {
    (((speed - 1.0) * 100.0).round() as i32).clamp(-50, 100)
}

fn pcm_samples(carry: &mut Option<u8>, bytes: &[u8]) -> Vec<i16> {
    let mut data = Vec::with_capacity(bytes.len() + 1);
    data.extend(carry.take());
    data.extend_from_slice(bytes);
    if data.len() % 2 == 1 {
        *carry = data.pop();
    }
    data.chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

fn service_error(message: &Message) -> ProviderError {
    let text = message.payload_text();
    let value: Option<serde_json::Value> = serde_json::from_str(&text).ok();
    let detail = value
        .as_ref()
        .and_then(|value| value.get("message").or_else(|| value.get("error")))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or(text);
    let code = message
        .error_code
        .map(|code| format!("（错误码 {code}）"))
        .unwrap_or_default();
    ProviderError::new(format!("豆包合成出错{code}{}", safe_hint(&detail)))
}

async fn send_message(ws: &mut Ws, message: &Message) -> Result<(), ProviderError> {
    ws.send(WsMessage::Binary(bidirection::marshal(message).into()))
        .await
        .map_err(|error| network_drop("豆包合成", &error))
}

async fn next_message(ws: &mut Ws) -> Result<Message, ProviderError> {
    loop {
        let message = tokio::time::timeout(TTS_IDLE_TIMEOUT, ws.next())
            .await
            .map_err(|_| ProviderError::new("豆包合成等待超时"))?;
        match message {
            Some(Ok(WsMessage::Binary(data))) => return bidirection::unmarshal(&data),
            Some(Ok(WsMessage::Close(_))) | None => return Err(server_closed("豆包合成")),
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(network_drop("豆包合成", &error)),
        }
    }
}

impl Tts for DoubaoTts {
    fn synthesize<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
        Box::pin(async move {
            let (chunks_tx, mut chunks_rx) = unbounded_channel();
            self.synthesize_stream(text, chunks_tx).await?;
            let mut samples = Vec::new();
            while let Ok(chunk) = chunks_rx.try_recv() {
                samples.extend(chunk.samples);
            }
            Ok(Pcm {
                samples,
                sample_rate: TTS_SAMPLE_RATE,
            })
        })
    }

    fn synthesize_stream<'a>(
        &'a self,
        text: &'a str,
        chunks: UnboundedSender<Pcm>,
    ) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            tokio::time::timeout(TTS_TOTAL_TIMEOUT, self.speak(text, &chunks))
                .await
                .unwrap_or_else(|_| Err(ProviderError::new("豆包合成超时")))
        })
    }
}

#[cfg(test)]
mod tests;
