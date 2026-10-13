//! 豆包流式语音识别（大模型 SAUC，`/api/v3/sauc/*`）的二进制帧。
//!
//! 移植自 univoice `src/asr/protocol/sauc.rs`（<https://github.com/shenjingnan/univoice>，`main-rs` 分支
//! `021113f` / crates.io 0.1.6，MIT，Copyright (c) 2026 shenjingnan；许可全文见 `THIRD_PARTY_NOTICES.md`）。
//! 改动：错误类型换成 [`ProviderError`]；去掉 MOSS 用不到的字段；解析时越界一律报错而不 panic。
//!
//! 帧：4 字节头（版本 1 / 头长 1 | 消息类型 / 标志 | 序列化 / 压缩 | 保留）→ 按标志可选的序号（4 字节大端）
//! 与事件号 → 错误帧的错误码 → 载荷长度（4 字节大端）与载荷。

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::ProviderError;

const CLIENT_FULL_REQUEST: u8 = 0b0001;
const CLIENT_AUDIO_ONLY: u8 = 0b0010;
const SERVER_FULL_RESPONSE: u8 = 0b1001;
const SERVER_ERROR: u8 = 0b1111;

const POS_SEQUENCE: u8 = 0b0001;
const NEG_WITH_SEQUENCE: u8 = 0b0011;

const NO_SERIALIZATION: u8 = 0b0000;
const JSON: u8 = 0b0001;
const GZIP: u8 = 0b0001;

/// 首帧（Full Client Request）的内容。
#[derive(Debug, Clone, Serialize)]
pub struct FullRequest {
    pub user: User,
    pub audio: AudioConfig,
    pub request: RequestConfig,
}

#[derive(Debug, Clone, Serialize)]
pub struct User {
    pub uid: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioConfig {
    pub format: String,
    pub codec: String,
    pub rate: u32,
    pub bits: u8,
    pub channel: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corpus: Option<Corpus>,
    pub model_name: String,
    pub enable_itn: bool,
    pub enable_punc: bool,
    pub enable_ddc: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Corpus { pub context: String }

/// 服务端的一条响应。
#[derive(Debug, Clone, Default)]
pub struct Response {
    /// 错误码（错误帧才有，0 表示正常）
    pub code: i32,
    /// 最后一包（序号为负）
    pub is_last: bool,
    pub sequence: i32,
    pub payload: Option<Payload>,
    /// 错误帧载荷中的说明文字
    pub message: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Payload {
    #[serde(default)]
    pub result: Option<RecognitionResult>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RecognitionResult {
    #[serde(default)]
    pub text: String,
}

fn header(message_type: u8, flags: u8, serialization: u8, compression: u8) -> [u8; 4] {
    [0x11, (message_type << 4) | flags, (serialization << 4) | compression, 0x00]
}

fn frame(header: [u8; 4], sequence: i32, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(12 + payload.len());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&sequence.to_be_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// 首帧：JSON 参数，gzip 压缩，序号 1。
pub fn encode_full_request(request: &FullRequest) -> Result<Vec<u8>, ProviderError> {
    let json = serde_json::to_vec(request).map_err(|_| ProviderError::new("识别参数编码失败"))?;
    Ok(frame(header(CLIENT_FULL_REQUEST, POS_SEQUENCE, JSON, GZIP), 1, &gzip(&json)?))
}

/// 音频帧：16 位小端 PCM，gzip 压缩；最后一包序号取负。
pub fn encode_audio(sequence: i32, pcm: &[i16], is_last: bool) -> Result<Vec<u8>, ProviderError> {
    let bytes: Vec<u8> = pcm.iter().flat_map(|sample| sample.to_le_bytes()).collect();
    let (flags, sequence) = if is_last { (NEG_WITH_SEQUENCE, -sequence) } else { (POS_SEQUENCE, sequence) };
    Ok(frame(header(CLIENT_AUDIO_ONLY, flags, NO_SERIALIZATION, GZIP), sequence, &gzip(&bytes)?))
}

fn read_i32(data: &[u8], offset: usize) -> Result<i32, ProviderError> {
    data.get(offset..offset + 4)
        .map(|bytes| i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .ok_or_else(|| ProviderError::new("识别服务返回的数据不完整"))
}

/// 解析服务端响应。载荷长度超出实际数据时按实际长度截取（与 univoice 一致）；载荷不是合法 JSON 时 `payload` 为空。
pub fn parse_response(data: &[u8]) -> Result<Response, ProviderError> {
    if data.len() < 4 {
        return Err(ProviderError::new("识别服务返回的数据过短"));
    }
    let header_size = (data[0] & 0x0f) as usize * 4;
    let message_type = data[1] >> 4;
    let flags = data[1] & 0x0f;
    let serialization = data[2] >> 4;
    let compression = data[2] & 0x0f;
    let mut offset = header_size;
    let mut response = Response::default();
    if flags & 0x01 != 0 {
        response.sequence = read_i32(data, offset)?;
        offset += 4;
    }
    response.is_last = flags & 0x02 != 0;
    if flags & 0x04 != 0 {
        // 事件号：识别接口不使用，跳过
        read_i32(data, offset)?;
        offset += 4;
    }
    let payload_size = match message_type {
        CLIENT_FULL_REQUEST => return Ok(response),
        SERVER_FULL_RESPONSE => {
            let size = read_i32(data, offset)? as u32;
            offset += 4;
            size
        }
        SERVER_ERROR => {
            response.code = read_i32(data, offset)?;
            let size = read_i32(data, offset + 4)? as u32;
            offset += 8;
            size
        }
        other => return Err(ProviderError::new(format!("识别服务返回了未知的消息类型 {other}"))),
    };
    let available = data.len().saturating_sub(offset);
    let raw = &data[offset.min(data.len())..offset.min(data.len()) + (payload_size as usize).min(available)];
    let body = if compression == GZIP && !raw.is_empty() { gunzip(raw).unwrap_or_else(|_| raw.to_vec()) } else { raw.to_vec() };
    if response.code != 0 {
        let value: Option<serde_json::Value> = serde_json::from_slice(&body).ok();
        response.message = value
            .as_ref()
            .and_then(|value| value.get("error").or_else(|| value.get("message")))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| String::from_utf8_lossy(&body).into_owned());
    } else if serialization == JSON && !body.is_empty() {
        response.payload = serde_json::from_slice(&body).ok();
    }
    Ok(response)
}

/// 错误码的中文说明。
pub fn error_message(code: i32) -> String {
    match code {
        20000000 => "成功".into(),
        45000001 => "请求参数无效".into(),
        45000002 => "空音频".into(),
        45000081 => "等包超时".into(),
        45000151 => "音频格式不正确".into(),
        55000031 => "服务器繁忙".into(),
        _ if (55000000..56000000).contains(&code) => "服务内部处理错误".into(),
        _ => format!("错误码 {code}"),
    }
}

pub(crate) fn gzip(data: &[u8]) -> Result<Vec<u8>, ProviderError> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(data).map_err(|_| ProviderError::new("音频压缩失败"))?;
    encoder.finish().map_err(|_| ProviderError::new("音频压缩失败"))
}

pub(crate) fn gunzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut decoder = flate2::read::GzDecoder::new(data);
    let mut buffer = Vec::new();
    decoder.read_to_end(&mut buffer)?;
    Ok(buffer)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn request() -> FullRequest {
        FullRequest {
            user: User { uid: "moss".into() },
            audio: AudioConfig { format: "pcm".into(), codec: "raw".into(), rate: 16000, bits: 16, channel: 1 },
            request: RequestConfig { model_name: "bigmodel".into(), enable_itn: true, enable_punc: true, enable_ddc: false, corpus: None },
        }
    }

    /// 构造服务端响应帧（univoice 测试中的 `build_server_response`）。
    pub(crate) fn server_response(json: &str, sequence: i32, is_last: bool) -> Vec<u8> {
        let payload = gzip(json.as_bytes()).unwrap();
        let flags = if is_last { NEG_WITH_SEQUENCE } else { POS_SEQUENCE };
        let mut frame = vec![0x11, (SERVER_FULL_RESPONSE << 4) | flags, 0x11, 0x00];
        frame.extend_from_slice(&sequence.to_be_bytes());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&payload);
        frame
    }

    #[test]
    fn headers_match_the_protocol() {
        // 与 univoice 测试 h1 / h2 / h3 相同的字节
        let full = encode_full_request(&request()).unwrap();
        assert_eq!(&full[0..4], &[0x11, 0x11, 0x11, 0x00]);
        assert_eq!(&full[4..8], &1i32.to_be_bytes());
        let size = u32::from_be_bytes(full[8..12].try_into().unwrap()) as usize;
        let json: serde_json::Value = serde_json::from_slice(&gunzip(&full[12..12 + size]).unwrap()).unwrap();
        assert_eq!(json["audio"]["rate"], 16000);
        assert_eq!(json["request"]["model_name"], "bigmodel");

        let audio = encode_audio(2, &[1, -2, 3], false).unwrap();
        assert_eq!(&audio[0..4], &[0x11, 0x21, 0x01, 0x00]);
        assert_eq!(&audio[4..8], &2i32.to_be_bytes());
        let size = u32::from_be_bytes(audio[8..12].try_into().unwrap()) as usize;
        assert_eq!(gunzip(&audio[12..12 + size]).unwrap(), vec![1, 0, 0xFE, 0xFF, 3, 0]);

        let last = encode_audio(5, &[], true).unwrap();
        assert_eq!(&last[0..4], &[0x11, 0x23, 0x01, 0x00]);
        assert_eq!(&last[4..8], &(-5i32).to_be_bytes());
    }

    #[test]
    fn parses_results_errors_and_bad_frames() {
        let response = parse_response(&server_response(r#"{"result":{"text":"你好世界"}}"#, 1, false)).unwrap();
        assert_eq!((response.code, response.is_last), (0, false));
        assert_eq!(response.payload.unwrap().result.unwrap().text, "你好世界");

        let last = parse_response(&server_response(r#"{"result":{"text":"完"}}"#, -3, true)).unwrap();
        assert!(last.is_last);
        assert_eq!(last.sequence, -3);

        // 错误帧：错误码 + 说明
        let body = gzip(br#"{"error":"busy"}"#).unwrap();
        let mut frame = vec![0x11, 0xF0, 0x11, 0x00];
        frame.extend_from_slice(&55000031i32.to_be_bytes());
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&body);
        let error = parse_response(&frame).unwrap();
        assert_eq!(error.code, 55000031);
        assert_eq!(error.message, "busy");
        assert_eq!(error_message(error.code), "服务器繁忙");
        assert_eq!(error_message(55000100), "服务内部处理错误");

        // 过短、截断、载荷长度越界、未知类型：报错或忽略载荷，不 panic
        assert!(parse_response(&[0x11, 0x91]).is_err());
        assert!(parse_response(&[0x11, 0x91, 0x11, 0x00, 0, 0]).is_err());
        let mut overflow = vec![0x11, 0x91, 0x11, 0x00];
        overflow.extend_from_slice(&1i32.to_be_bytes());
        overflow.extend_from_slice(&1_000_000u32.to_be_bytes());
        overflow.extend_from_slice(&[0u8; 10]);
        assert!(parse_response(&overflow).unwrap().payload.is_none());
        assert!(parse_response(&[0x11, 0x21, 0x01, 0x00, 0, 0, 0, 1]).is_err());
        // 回显的首帧被忽略
        assert!(parse_response(&encode_full_request(&request()).unwrap()).unwrap().payload.is_none());
    }
}
