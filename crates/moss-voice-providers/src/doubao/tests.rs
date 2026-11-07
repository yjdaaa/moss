//! 豆包识别与合成：用本机假 WebSocket 服务按协议逐字节应答（不连接真实服务）。

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{HeaderMap, StatusCode};

use super::bidirection::tests::{server_error, server_event};
use super::bidirection::{self, MsgType};
use super::sauc::tests::server_response;
use super::sauc::{self, gunzip};
use super::*;

type ServerWs = WebSocketStream<TcpStream>;

#[test]
fn hotwords_use_official_corpus_context_shape_and_are_bounded() {
    let asr = DoubaoAsr::new("", "fake", "volc.bigasr.sauc.duration")
        .unwrap()
        .with_hotwords(&vec!["椴木".into(); 100]);
    let request = serde_json::to_value(full_request_with_hotwords(&asr.hotwords)).unwrap();
    let context: serde_json::Value =
        serde_json::from_str(request["request"]["corpus"]["context"].as_str().unwrap()).unwrap();
    assert_eq!(context["hotwords"][0], serde_json::json!({"word":"椴木"}));
    assert_eq!(context["hotwords"].as_array().unwrap().len(), 32);
    let empty = serde_json::to_value(full_request_with_hotwords(&[])).unwrap();
    assert!(empty["request"].get("corpus").is_none());
}

struct FakeServer {
    url: String,
    connections: Arc<AtomicUsize>,
    headers: Arc<Mutex<Vec<HeaderMap>>>,
}

/// 启动假服务：每个连接记录请求头后交给 `handler`；`reject` 不为空时以该状态码拒绝握手。
async fn serve<F, Fut>(reject: Option<StatusCode>, handler: F) -> FakeServer
where
    F: Fn(ServerWs, usize) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/api", listener.local_addr().unwrap());
    let connections = Arc::new(AtomicUsize::new(0));
    let headers = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(handler);
    let (count, seen) = (connections.clone(), headers.clone());
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let index = count.fetch_add(1, Ordering::SeqCst);
            let (seen, handler) = (seen.clone(), handler.clone());
            tokio::spawn(async move {
                let callback = |request: &Request,
                                response: Response|
                 -> Result<Response, ErrorResponse> {
                    seen.lock().unwrap().push(request.headers().clone());
                    match reject {
                        Some(status) => {
                            let mut error =
                                ErrorResponse::new(Some("invalid api key sk-echoed-key".into()));
                            *error.status_mut() = status;
                            Err(error)
                        }
                        None => Ok(response),
                    }
                };
                if let Ok(ws) = tokio_tungstenite::accept_hdr_async(stream, callback).await {
                    handler(ws, index).await;
                }
            });
        }
    });
    FakeServer {
        url,
        connections,
        headers,
    }
}

async fn recv_binary(ws: &mut ServerWs) -> Option<Vec<u8>> {
    loop {
        match ws.next().await? {
            Ok(WsMessage::Binary(data)) => return Some(data.to_vec()),
            Ok(WsMessage::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

/// 客户端识别帧：(序号, 是否最后一包, 解压后的载荷)。
fn client_frame(data: &[u8]) -> (i32, bool, Vec<u8>) {
    let sequence = i32::from_be_bytes(data[4..8].try_into().unwrap());
    let size = u32::from_be_bytes(data[8..12].try_into().unwrap()) as usize;
    (
        sequence,
        data[1] & 0x0f == 0b0011,
        gunzip(&data[12..12 + size]).unwrap(),
    )
}

#[test]
fn auth_headers_support_new_and_legacy_consoles() {
    let new = auth_headers("api-key-1", "volc.bigasr.sauc.duration");
    assert_eq!(
        new,
        vec![
            ("X-Api-Key", "api-key-1".into()),
            ("X-Api-Resource-Id", "volc.bigasr.sauc.duration".into())
        ]
    );
    let legacy = auth_headers("123456:token-x", "seed-tts-2.0");
    assert_eq!(legacy[0], ("X-Api-App-Key", "123456".into()));
    assert_eq!(legacy[1], ("X-Api-Access-Key", "token-x".into()));
    // 冒号前不是纯数字：整体当作新版 API Key
    assert_eq!(auth_headers("ab:cd", "r")[0], ("X-Api-Key", "ab:cd".into()));
    assert_eq!(uuid().len(), 36);
    assert_ne!(uuid(), uuid());
    let config = DoubaoConfig::new("", "secret-key-123", "r", DEFAULT_ASR_URL).unwrap();
    assert_eq!(config.url, DEFAULT_ASR_URL, "地址留空用默认地址");
    assert!(!format!("{config:?}").contains("secret-key-123"));
    assert!(DoubaoConfig::new("https://h", "k", "r", DEFAULT_ASR_URL).is_err());
    assert!(
        DoubaoConfig::new("", "", "r", DEFAULT_ASR_URL)
            .unwrap_err()
            .to_string()
            .contains("密钥")
    );
    assert!(
        DoubaoConfig::new("", "k", " ", DEFAULT_ASR_URL)
            .unwrap_err()
            .to_string()
            .contains("资源 ID")
    );
}

#[test]
fn speed_maps_to_doubao_speech_rate() {
    assert_eq!(speech_rate(1.0), 0);
    assert_eq!(speech_rate(0.5), -50);
    assert_eq!(speech_rate(2.0), 100);
    assert_eq!(speech_rate(1.2), 20);
    assert_eq!(speech_rate(0.25), -50);
    assert_eq!(speech_rate(4.0), 100);
}

#[test]
fn pcm_chunks_split_inside_a_sample_are_rejoined() {
    let mut carry = None;
    assert_eq!(pcm_samples(&mut carry, &[1, 0, 2]), vec![1]);
    assert_eq!(carry, Some(2));
    assert_eq!(pcm_samples(&mut carry, &[1, 3, 0]), vec![0x0102, 3]);
    assert_eq!(carry, None);
}

/// 假识别服务：确认参数帧，每收到一包推一次中间结果，最后一包后推最终结果。记录收到的包。
async fn asr_server(
    packets: Arc<Mutex<Vec<(i32, bool, usize)>>>,
    params: Arc<Mutex<Option<serde_json::Value>>>,
) -> FakeServer {
    serve(None, move |mut ws, _| {
        let (packets, params) = (packets.clone(), params.clone());
        async move {
            let Some(first) = recv_binary(&mut ws).await else {
                return;
            };
            let (sequence, _, body) = client_frame(&first);
            assert_eq!(sequence, 1);
            *params.lock().unwrap() = serde_json::from_slice(&body).ok();
            ws.send(WsMessage::Binary(server_response("{}", 1, false).into()))
                .await
                .unwrap();
            while let Some(data) = recv_binary(&mut ws).await {
                let (sequence, last, body) = client_frame(&data);
                packets
                    .lock()
                    .unwrap()
                    .push((sequence, last, body.len() / 2));
                if last {
                    let reply =
                        server_response(r#"{"result":{"text":"打开激光机。"}}"#, sequence, true);
                    ws.send(WsMessage::Binary(reply.into())).await.unwrap();
                    return;
                }
                let partial = server_response(r#"{"result":{"text":"打开"}}"#, sequence, false);
                ws.send(WsMessage::Binary(partial.into())).await.unwrap();
            }
        }
    })
    .await
}

#[tokio::test]
async fn streaming_recognition_sends_packets_while_speaking_and_returns_final_text() {
    let packets = Arc::new(Mutex::new(Vec::new()));
    let params = Arc::new(Mutex::new(None));
    let server = asr_server(packets.clone(), params.clone()).await;
    let asr = DoubaoAsr::new(&server.url, "api-key-asr", "volc.bigasr.sauc.duration").unwrap();
    let stream = asr.start();
    // 设备每 60 ms 一帧（16 kHz 为 960 个样本），共 10 帧
    for index in 0..10 {
        stream.feed(&vec![index as i16; 960]);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // 说完之前已经有包发出（边说边传）
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !packets.lock().unwrap().is_empty(),
        "说话期间就应发送音频包"
    );
    let started = std::time::Instant::now();
    let text = stream.finish().await.unwrap();
    assert_eq!(text, "打开激光机。");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "说完后只等最后一包的结果"
    );

    let packets = packets.lock().unwrap().clone();
    let total: usize = packets.iter().map(|(_, _, samples)| samples).sum();
    assert_eq!(total, 9600, "音频不丢不重");
    assert!(
        packets[..packets.len() - 1]
            .iter()
            .all(|(_, last, samples)| !last && *samples == PACKET_SAMPLES)
    );
    let (last_sequence, last, _) = *packets.last().unwrap();
    assert!(last && last_sequence < 0, "最后一包序号取负");
    let sequences: Vec<i32> = packets
        .iter()
        .map(|(sequence, _, _)| sequence.abs())
        .collect();
    assert_eq!(
        sequences,
        (2..2 + packets.len() as i32).collect::<Vec<_>>(),
        "序号从 2 起连续"
    );

    let params = params.lock().unwrap().clone().unwrap();
    assert_eq!(params["audio"]["rate"], 16000);
    assert_eq!(params["audio"]["format"], "pcm");
    let headers = server.headers.lock().unwrap()[0].clone();
    assert_eq!(headers["x-api-key"], "api-key-asr");
    assert_eq!(headers["x-api-resource-id"], "volc.bigasr.sauc.duration");
    assert_eq!(headers["x-api-connect-id"].len(), 36);
}

#[tokio::test]
async fn whole_utterance_recognition_resamples_to_16k() {
    let packets = Arc::new(Mutex::new(Vec::new()));
    let server = asr_server(packets.clone(), Arc::new(Mutex::new(None))).await;
    let asr = DoubaoAsr::new(&server.url, "123:legacy-token", "r").unwrap();
    // 24 kHz 的 1 秒音频 → 约 16000 个样本
    let text = asr.transcribe(&vec![100; 24_000], 24_000).await.unwrap();
    assert_eq!(text, "打开激光机。");
    let total: usize = packets
        .lock()
        .unwrap()
        .iter()
        .map(|(_, _, samples)| samples)
        .sum();
    assert!((total as i64 - 16_000).abs() <= 2, "{total}");
    let headers = server.headers.lock().unwrap()[0].clone();
    assert_eq!(headers["x-api-app-key"], "123");
    assert_eq!(headers["x-api-access-key"], "legacy-token");
    assert!(headers.get("x-api-key").is_none());
}

#[tokio::test]
async fn recognition_errors_are_explained_without_secrets() {
    // 服务端错误帧
    let server = serve(None, |mut ws, _| async move {
        let _ = recv_binary(&mut ws).await;
        let body = sauc::gzip(br#"{"error":"bad audio"}"#).unwrap();
        let mut frame = vec![0x11, 0xF0, 0x11, 0x00];
        frame.extend_from_slice(&45000151i32.to_be_bytes());
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&body);
        ws.send(WsMessage::Binary(frame.into())).await.unwrap();
        let _ = recv_binary(&mut ws).await;
    })
    .await;
    let asr = DoubaoAsr::new(&server.url, "sk-secret-asr", "r").unwrap();
    let stream = asr.start();
    stream.feed(&[0; 960]);
    let error = stream.finish().await.unwrap_err().to_string();
    assert!(
        error.contains("音频格式不正确") && error.contains("bad audio"),
        "{error}"
    );

    // 握手被拒（401），服务端回显的内容含疑似密钥：不回显
    let server = serve(Some(StatusCode::UNAUTHORIZED), |_, _| async {}).await;
    let asr = DoubaoAsr::new(&server.url, "sk-secret-asr", "r").unwrap();
    let error = asr
        .transcribe(&[0; 1600], 16_000)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("HTTP 401") && error.contains("密钥"),
        "{error}"
    );
    assert!(
        !error.contains("sk-") && !error.contains("127.0.0.1"),
        "{error}"
    );

    // 地址连不上
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let asr = DoubaoAsr::new(&format!("ws://127.0.0.1:{port}/x?token=abc"), "k", "r").unwrap();
    let error = asr
        .transcribe(&[0; 1600], 16_000)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("无法连接") && !error.contains("token=abc"),
        "{error}"
    );
}

#[tokio::test]
async fn dropping_a_stream_stops_the_connection() {
    let closed = Arc::new(AtomicUsize::new(0));
    let flag = closed.clone();
    let server = serve(None, move |mut ws, _| {
        let flag = flag.clone();
        async move {
            let _ = recv_binary(&mut ws).await;
            ws.send(WsMessage::Binary(server_response("{}", 1, false).into()))
                .await
                .unwrap();
            while recv_binary(&mut ws).await.is_some() {}
            flag.fetch_add(1, Ordering::SeqCst);
        }
    })
    .await;
    let asr = DoubaoAsr::new(&server.url, "k", "r").unwrap();
    let stream = asr.start();
    stream.feed(&[1; 3200]);
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(stream);
    for _ in 0..50 {
        if closed.load(Ordering::SeqCst) == 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("打断后连接应关闭");
}

/// 假合成服务：完成连接与会话握手；每句话把音频拆成 3 段（第一段在样本中间断开）后发 SessionFinished。
/// `close_after_first` 为真时第一句结束后关闭连接（模拟服务端关闭空闲连接）。
async fn tts_server(
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    close_after_first: bool,
) -> FakeServer {
    serve(None, move |mut ws, _| {
        let requests = requests.clone();
        async move {
            let mut sessions = 0;
            while let Some(data) = recv_binary(&mut ws).await {
                let message = bidirection::unmarshal(&data).unwrap();
                let session = message.session_id.clone().unwrap_or_default();
                match message.event {
                    Some(bidirection::START_CONNECTION) => {
                        let reply = server_event(
                            MsgType::FullServerResponse,
                            bidirection::CONNECTION_STARTED,
                            "conn-1",
                            b"{}",
                        );
                        ws.send(WsMessage::Binary(reply.into())).await.unwrap();
                    }
                    Some(bidirection::START_SESSION) => {
                        let reply = server_event(
                            MsgType::FullServerResponse,
                            bidirection::SESSION_STARTED,
                            &session,
                            b"{}",
                        );
                        ws.send(WsMessage::Binary(reply.into())).await.unwrap();
                    }
                    Some(bidirection::TASK_REQUEST) => {
                        requests
                            .lock()
                            .unwrap()
                            .push(serde_json::from_slice(&message.payload).unwrap());
                    }
                    Some(bidirection::FINISH_SESSION) => {
                        let audio: Vec<u8> = (0..300i16)
                            .flat_map(|sample| sample.to_le_bytes())
                            .collect();
                        for part in [&audio[..101], &audio[101..400], &audio[400..]] {
                            let reply = server_event(MsgType::AudioOnlyServer, 352, &session, part);
                            ws.send(WsMessage::Binary(reply.into())).await.unwrap();
                        }
                        let done = br#"{"status_code":20000000,"message":"ok"}"#;
                        let reply = server_event(
                            MsgType::FullServerResponse,
                            bidirection::SESSION_FINISHED,
                            &session,
                            done,
                        );
                        ws.send(WsMessage::Binary(reply.into())).await.unwrap();
                        sessions += 1;
                        if close_after_first && sessions == 1 {
                            let _ = ws.close(None).await;
                            return;
                        }
                    }
                    _ => {}
                }
            }
        }
    })
    .await
}

#[tokio::test]
async fn synthesis_streams_chunks_and_reuses_the_connection() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let server = tts_server(requests.clone(), false).await;
    let tts = DoubaoTts::new(
        &server.url,
        "api-key-tts",
        "seed-tts-2.0",
        "zh_female_test",
        1.2,
    )
    .unwrap();
    let (chunks_tx, mut chunks_rx) = unbounded_channel();
    tts.synthesize_stream("你好。", chunks_tx).await.unwrap();
    let mut chunks = Vec::new();
    while let Ok(chunk) = chunks_rx.try_recv() {
        chunks.push(chunk);
    }
    assert_eq!(chunks.len(), 3, "音频分段送出");
    assert!(
        chunks
            .iter()
            .all(|chunk| chunk.sample_rate == TTS_SAMPLE_RATE)
    );
    let samples: Vec<i16> = chunks.into_iter().flat_map(|chunk| chunk.samples).collect();
    assert_eq!(
        samples,
        (0..300).collect::<Vec<i16>>(),
        "跨包的半个样本拼接正确"
    );

    // 第二句复用同一条连接
    let pcm = tts.synthesize("第二句。").await.unwrap();
    assert_eq!(pcm.samples.len(), 300);
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests[0]["req_params"]["text"], "你好。");
    assert_eq!(requests[0]["req_params"]["speaker"], "zh_female_test");
    assert_eq!(requests[0]["req_params"]["audio_params"]["format"], "pcm");
    assert_eq!(
        requests[0]["req_params"]["audio_params"]["sample_rate"],
        24000
    );
    assert_eq!(requests[0]["req_params"]["audio_params"]["speech_rate"], 20);
    assert_eq!(requests[1]["req_params"]["text"], "第二句。");
    let headers = server.headers.lock().unwrap()[0].clone();
    assert_eq!(headers["x-api-key"], "api-key-tts");
    assert_eq!(headers["x-api-resource-id"], "seed-tts-2.0");
}

#[tokio::test]
async fn closed_idle_connection_is_replaced_once() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let server = tts_server(requests.clone(), true).await;
    let tts = DoubaoTts::new(&server.url, "k", "r", "v", 1.0).unwrap();
    assert_eq!(tts.synthesize("一。").await.unwrap().samples.len(), 300);
    tokio::time::sleep(Duration::from_millis(50)).await;
    // 留下的连接已被服务端关闭：换新连接重试，用户无感
    assert_eq!(tts.synthesize("二。").await.unwrap().samples.len(), 300);
    assert_eq!(server.connections.load(Ordering::SeqCst), 2);
    assert_eq!(requests.lock().unwrap().len(), 2, "每句只合成一次");
}

#[tokio::test]
async fn synthesis_failures_are_reported() {
    // 会话失败
    let server = serve(None, |mut ws, _| async move {
        while let Some(data) = recv_binary(&mut ws).await {
            let message = bidirection::unmarshal(&data).unwrap();
            let reply = match message.event {
                Some(bidirection::START_CONNECTION) => server_event(
                    MsgType::FullServerResponse,
                    bidirection::CONNECTION_STARTED,
                    "c",
                    b"{}",
                ),
                Some(bidirection::START_SESSION) => server_event(
                    MsgType::FullServerResponse,
                    bidirection::SESSION_FAILED,
                    message.session_id.as_deref().unwrap_or(""),
                    br#"{"status_code":45000001,"message":"speaker not found"}"#,
                ),
                _ => continue,
            };
            ws.send(WsMessage::Binary(reply.into())).await.unwrap();
        }
    })
    .await;
    let tts = DoubaoTts::new(&server.url, "k", "r", "bad-voice", 1.0).unwrap();
    let error = tts.synthesize("你好").await.unwrap_err().to_string();
    assert!(error.contains("speaker not found"), "{error}");

    // 错误帧
    let server = serve(None, |mut ws, _| async move {
        let _ = recv_binary(&mut ws).await;
        ws.send(WsMessage::Binary(
            server_error(45000003, "quota exceeded").into(),
        ))
        .await
        .unwrap();
        let _ = recv_binary(&mut ws).await;
    })
    .await;
    let tts = DoubaoTts::new(&server.url, "k", "r", "v", 1.0).unwrap();
    let error = tts.synthesize("你好").await.unwrap_err().to_string();
    assert!(
        error.contains("45000003") && error.contains("quota exceeded"),
        "{error}"
    );

    assert!(
        DoubaoTts::new("", "k", "r", " ", 1.0)
            .err()
            .unwrap()
            .to_string()
            .contains("音色")
    );
}

/// 真实服务的 TLS 与鉴权链路（不在常规测试中运行）：用假密钥连真实地址，应在 TLS 握手成功后被鉴权拒绝。
/// `cargo test -p moss-voice-providers real_doubao_rejects_fake_key -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn real_doubao_rejects_fake_key() {
    let tts = DoubaoTts::new(
        "",
        "moss-fake-key-for-tls-check",
        DEFAULT_TTS_RESOURCE,
        "test",
        1.0,
    )
    .unwrap();
    let error = tts.synthesize("你好").await.unwrap_err().to_string();
    println!("合成：{error}");
    assert!(
        error.contains("HTTP 4") || error.contains("豆包合成出错"),
        "{error}"
    );
    let asr = DoubaoAsr::new("", "moss-fake-key-for-tls-check", DEFAULT_ASR_RESOURCE).unwrap();
    let error = asr
        .transcribe(&[0; 1600], 16_000)
        .await
        .unwrap_err()
        .to_string();
    println!("识别：{error}");
    assert!(
        error.contains("HTTP 4") || error.contains("豆包识别出错"),
        "{error}"
    );
    assert!(!error.contains("moss-fake-key"));
}
