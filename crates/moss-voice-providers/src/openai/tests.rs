//! 用本机假 HTTP 服务测试请求格式、流式解析与错误处理（不访问外网，不需要真实密钥）。

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

use super::*;

const KEY: &str = "sk-test-secret-123";

struct Recorded {
    head: String,
    body: Vec<u8>,
}

/// 接受一个连接，记录请求，按片段（片段之间停 20 ms）写回响应后关闭。
async fn serve_once(parts: Vec<Vec<u8>>) -> (String, oneshot::Receiver<Recorded>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut chunk).await.unwrap();
            buffer.extend_from_slice(&chunk[..n]);
            if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
        };
        let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
        let length = head
            .lines()
            .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
            .unwrap_or(0);
        while buffer.len() < header_end + length {
            let n = stream.read(&mut chunk).await.unwrap();
            buffer.extend_from_slice(&chunk[..n]);
        }
        let _ = tx.send(Recorded { head, body: buffer[header_end..header_end + length].to_vec() });
        for part in parts {
            stream.write_all(&part).await.unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    (url, rx)
}

fn response(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len())
        .into_bytes();
    out.extend_from_slice(body);
    out
}

fn sse_head() -> Vec<u8> {
    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n".to_vec()
}

#[test]
fn urls_are_normalized() {
    assert_eq!(llm_base_url(" https://h/v1/chat/completions/ "), "https://h/v1");
    assert_eq!(asr_full_url("https://h/v1/"), "https://h/v1/audio/transcriptions");
    assert_eq!(tts_full_url("https://h/v1/audio/speech"), "https://h/v1/audio/speech");
}

#[test]
fn sse_decoder_handles_split_lines_and_utf8() {
    let mut decoder = SseDecoder::default();
    let text = "data: {\"a\":\"你好\"}\r\n\r\ndata: [DONE]\n\n";
    let bytes = text.as_bytes();
    let mut events = Vec::new();
    // 逐字节送入，覆盖半个汉字、\r\n 被拆开的情况
    for byte in bytes {
        events.extend(decoder.push(std::slice::from_ref(byte)));
    }
    assert_eq!(events, vec!["{\"a\":\"你好\"}", "[DONE]"]);
    assert!(decoder.finish().is_none());
    let mut decoder = SseDecoder::default();
    assert!(decoder.push(b": comment\nevent: x\ndata: 1\ndata: 2").is_empty());
    assert_eq!(decoder.finish().as_deref(), Some("1\n2"));
}

#[test]
fn accumulator_merges_tool_call_fragments() {
    let mut accumulator = StreamAccumulator::default();
    let chunks = [
        json!({"choices":[{"delta":{"role":"assistant","content":"好的"}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"get_","arguments":""}}]}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"time","arguments":"{\"tz\":"}}]}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"status"}}]}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"cn\"}"}}]}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
    ];
    let mut text = Vec::new();
    for chunk in &chunks {
        text.extend(accumulator.apply(chunk).unwrap());
    }
    assert_eq!(text, vec!["好的"]);
    let reply = accumulator.finish();
    assert_eq!(reply.text, "好的");
    assert_eq!(reply.tool_calls, vec![ToolCall::new("call_a", "get_time", r#"{"tz":"cn"}"#), ToolCall::new("call_1", "status", "{}")]);
    let mut accumulator = StreamAccumulator::default();
    let error = accumulator.apply(&json!({"error":{"message":"model overloaded"}})).unwrap_err();
    assert!(error.0.contains("model overloaded"));
}

#[tokio::test]
async fn llm_streams_deltas_and_tool_calls() {
    let mut parts = vec![sse_head()];
    let stream = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"你好，\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"我是 MOSS。\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"get_time\",\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"DONE 之后的内容被忽略\"}}]}\n\n",
    );
    // 在汉字中间切开
    let bytes = stream.as_bytes();
    parts.push(bytes[..40].to_vec());
    parts.push(bytes[40..].to_vec());
    let (url, recorded) = serve_once(parts).await;
    let llm = OpenAiLlm::new(&format!("{url}/v1/chat/completions"), KEY, "test-model").unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let tools = vec![ToolSpec { name: "get_time".into(), description: "时间".into(), parameters: json!({"type":"object"}) }];
    let reply = llm.chat(&[ChatMessage::system("s"), ChatMessage::user("你好")], &tools, tx).await.unwrap();
    assert_eq!(reply.text, "你好，我是 MOSS。");
    assert_eq!(reply.tool_calls, vec![ToolCall::new("c1", "get_time", "{}")]);
    let mut deltas = Vec::new();
    while let Ok(delta) = rx.try_recv() {
        deltas.push(delta);
    }
    assert_eq!(deltas, vec!["你好，", "我是 MOSS。"]);
    let request = recorded.await.unwrap();
    assert!(request.head.starts_with("POST /v1/chat/completions "), "{}", request.head);
    assert!(request.head.to_ascii_lowercase().contains(&format!("authorization: bearer {KEY}").to_ascii_lowercase()));
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["messages"][1]["content"], "你好");
    assert_eq!(body["tools"][0]["function"]["name"], "get_time");
    assert_eq!(body["tool_choice"], "auto");
}

#[tokio::test]
async fn llm_accepts_non_streaming_json_and_omits_empty_tools() {
    let full = json!({"choices":[{"message":{"role":"assistant","content":"直接回答"}}]}).to_string();
    let (url, recorded) = serve_once(vec![response("200 OK", "application/json", full.as_bytes())]).await;
    let llm = OpenAiLlm::new(&format!("{url}/v1"), "", "m").unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let reply = llm.chat(&[ChatMessage::user("hi")], &[], tx).await.unwrap();
    assert_eq!(reply.text, "直接回答");
    assert_eq!(rx.try_recv().unwrap(), "直接回答");
    let request = recorded.await.unwrap();
    assert!(!request.head.to_ascii_lowercase().contains("authorization"), "没有密钥时不发 Authorization");
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert!(body.get("tools").is_none() && body.get("tool_choice").is_none());

    // 兼容 Cline 等网关在非流式下返回的 `{ success: true, data: { choices: [...] } }` 包裹结构
    let wrapped = json!({
        "success": true,
        "data": {
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "套壳回答",
                    "tool_calls": [{"id": "c9", "type": "function", "function": {"name": "get_time", "arguments": "{}"}}]
                }
            }]
        }
    })
    .to_string();
    let (url, _) = serve_once(vec![response("200 OK", "application/json", wrapped.as_bytes())]).await;
    let llm = OpenAiLlm::new(&format!("{url}/api/v1"), KEY, "cline-pass/deepseek-v4.1-flash").unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let reply = llm.chat(&[ChatMessage::user("hi")], &[], tx).await.unwrap();
    assert_eq!(reply.text, "套壳回答");
    assert_eq!(reply.tool_calls, vec![ToolCall::new("c9", "get_time", "{}")]);
}

#[tokio::test]
async fn errors_never_contain_key_or_url() {
    let body = json!({"error":{"message":"Incorrect API key provided: sk-test-secret-123"}}).to_string();
    let (url, _) = serve_once(vec![response("401 Unauthorized", "application/json", body.as_bytes())]).await;
    let llm = OpenAiLlm::new(&url, KEY, "m").unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let error = llm.chat(&[ChatMessage::user("hi")], &[], tx).await.unwrap_err();
    assert!(error.0.contains("HTTP 401") && error.0.contains("密钥"), "{error}");
    assert!(!error.0.contains(KEY) && !error.0.contains("127.0.0.1"), "{error}");

    // 连不上：端口已关闭
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = closed.local_addr().unwrap();
    drop(closed);
    let tts = OpenAiTts::new(&format!("http://{address}/v1"), KEY, "m", "v", 1.0).unwrap();
    let error = tts.synthesize("你好").await.unwrap_err();
    assert!(error.0.contains("无法连接"), "{error}");
    assert!(!error.0.contains(&address.to_string()) && !error.0.contains(KEY));

    // 流中返回错误对象
    let parts = vec![sse_head(), b"data: {\"error\":{\"message\":\"rate limited\"}}\n\n".to_vec()];
    let (url, _) = serve_once(parts).await;
    let llm = OpenAiLlm::new(&url, KEY, "m").unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    assert!(llm.chat(&[ChatMessage::user("hi")], &[], tx).await.unwrap_err().0.contains("rate limited"));
}

#[tokio::test]
async fn asr_uploads_wav_multipart() {
    let (url, recorded) = serve_once(vec![response("200 OK", "application/json", r#"{"text":" 打开 "}"#.as_bytes())]).await;
    let asr = OpenAiAsr::new(&format!("{url}/v1"), KEY, "whisper-1", Some("zh")).unwrap().with_hotwords(&["椴木".into(), "亚克力".into()]);
    let pcm = vec![100i16; 1600];
    assert_eq!(asr.transcribe(&pcm, 16_000).await.unwrap(), "打开");
    let request = recorded.await.unwrap();
    assert!(request.head.starts_with("POST /v1/audio/transcriptions "));
    assert!(request.head.contains("multipart/form-data; boundary=moss-voice-7d1a5c0e9b"));
    let body = String::from_utf8_lossy(&request.body);
    assert!(body.contains("name=\"file\"; filename=\"speech.wav\""));
    assert!(body.contains("RIFF") && body.contains("WAVEfmt "));
    assert!(body.contains("name=\"model\"\r\n\r\nwhisper-1"));
    assert!(body.contains("name=\"language\"\r\n\r\nzh"));
    assert!(body.contains("name=\"prompt\"\r\n\r\n椴木、亚克力"));
    assert!(body.trim_end().ends_with("--moss-voice-7d1a5c0e9b--"));
    // WAV 内容完整：16 位 1600 个样本
    let wav_start = request.body.windows(4).position(|w| w == b"RIFF").unwrap();
    let pcm_back = crate::wav::decode_wav(&request.body[wav_start..wav_start + 44 + 3200]).unwrap();
    assert_eq!(pcm_back.samples, pcm);

    let (url, _) = serve_once(vec![response("200 OK", "text/plain", "纯文本结果\n".as_bytes())]).await;
    let asr = OpenAiAsr::new(&url, "", "m", None).unwrap();
    assert_eq!(asr.transcribe(&pcm, 16_000).await.unwrap(), "纯文本结果");
}

#[tokio::test]
async fn tts_requests_wav_and_decodes_it() {
    let wav = crate::wav::encode_wav(&[1, 2, 3, 4], 22_050);
    let (url, recorded) = serve_once(vec![response("200 OK", "audio/wav", &wav)]).await;
    let tts = OpenAiTts::new(&format!("{url}/v1"), KEY, "tts-1", "alloy", 1.2).unwrap();
    let pcm = tts.synthesize("你好").await.unwrap();
    assert_eq!(pcm, Pcm { samples: vec![1, 2, 3, 4], sample_rate: 22_050 });
    let request = recorded.await.unwrap();
    assert!(request.head.starts_with("POST /v1/audio/speech "));
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["input"], "你好");
    assert_eq!(body["voice"], "alloy");
    assert_eq!(body["response_format"], "wav");
    assert!((body["speed"].as_f64().unwrap() - 1.2).abs() < 1e-6);

    let (url, _) = serve_once(vec![response("200 OK", "audio/mpeg", b"ID3\x04\x00mp3")]).await;
    let tts = OpenAiTts::new(&url, KEY, "m", "v", 1.0).unwrap();
    assert!(tts.synthesize("你好").await.unwrap_err().0.contains("不是 WAV"));
}
