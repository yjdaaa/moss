//! 豆包双向流式语音合成（`/api/v3/tts/bidirection`）的二进制帧。
//!
//! 移植自 univoice `src/tts/protocol/volcengine.rs`（<https://github.com/shenjingnan/univoice>，`main-rs` 分支
//! `021113f` / crates.io 0.1.6，MIT，Copyright (c) 2026 shenjingnan；许可全文见 `THIRD_PARTY_NOTICES.md`）。
//! 改动：事件号保留原始数值（服务端的句子事件 350–352 等不在 univoice 的枚举里，原实现会丢成“无事件”）；
//! 错误类型换成 [`ProviderError`]；只保留 MOSS 用到的构造函数。
//!
//! 写入顺序：4 字节头 → 带事件标志时写事件号与会话号（连接级事件不写会话号）→ 序号 / 错误码 → 载荷长度与载荷。
//! 读取顺序：序号 / 错误码 → 事件号 → 会话号（连接级事件跳过）→ 连接号（只有连接事件有）→ 载荷。

use crate::ProviderError;

pub const START_CONNECTION: i32 = 1;
pub const FINISH_CONNECTION: i32 = 2;
pub const CONNECTION_STARTED: i32 = 50;
pub const CONNECTION_FAILED: i32 = 51;
pub const CONNECTION_FINISHED: i32 = 52;
pub const START_SESSION: i32 = 100;
pub const CANCEL_SESSION: i32 = 101;
pub const FINISH_SESSION: i32 = 102;
pub const SESSION_STARTED: i32 = 150;
pub const SESSION_CANCELED: i32 = 151;
pub const SESSION_FINISHED: i32 = 152;
pub const SESSION_FAILED: i32 = 153;
pub const TASK_REQUEST: i32 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgType {
    FullClientRequest,
    AudioOnlyClient,
    FullServerResponse,
    AudioOnlyServer,
    FrontEndResultServer,
    Error,
    Invalid,
}

impl MsgType {
    fn bits(self) -> u8 {
        match self {
            Self::FullClientRequest => 0b0001,
            Self::AudioOnlyClient => 0b0010,
            Self::FullServerResponse => 0b1001,
            Self::AudioOnlyServer => 0b1011,
            Self::FrontEndResultServer => 0b1100,
            Self::Error => 0b1111,
            Self::Invalid => 0,
        }
    }

    fn from_bits(bits: u8) -> Self {
        match bits {
            0b0001 => Self::FullClientRequest,
            0b0010 => Self::AudioOnlyClient,
            0b1001 => Self::FullServerResponse,
            0b1011 => Self::AudioOnlyServer,
            0b1100 => Self::FrontEndResultServer,
            0b1111 => Self::Error,
            _ => Self::Invalid,
        }
    }
}

const FLAG_POSITIVE_SEQ: u8 = 0b001;
const FLAG_NEGATIVE_SEQ: u8 = 0b011;
const FLAG_WITH_EVENT: u8 = 0b100;

/// 一条消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub msg_type: MsgType,
    pub flag: u8,
    /// 序列化：0 原始字节，1 JSON
    pub serialization: u8,
    pub event: Option<i32>,
    pub session_id: Option<String>,
    pub connect_id: Option<String>,
    pub sequence: Option<i32>,
    pub error_code: Option<u32>,
    pub payload: Vec<u8>,
}

impl Message {
    fn event(event: i32, session_id: Option<&str>, payload: Vec<u8>) -> Self {
        Self {
            msg_type: MsgType::FullClientRequest,
            flag: FLAG_WITH_EVENT,
            serialization: 1,
            event: Some(event),
            session_id: session_id.map(str::to_owned),
            connect_id: None,
            sequence: None,
            error_code: None,
            payload,
        }
    }

    pub fn start_connection() -> Self {
        Self::event(START_CONNECTION, None, b"{}".to_vec())
    }

    pub fn finish_connection() -> Self {
        Self::event(FINISH_CONNECTION, None, b"{}".to_vec())
    }

    pub fn start_session(session_id: &str, payload: Vec<u8>) -> Self {
        Self::event(START_SESSION, Some(session_id), payload)
    }

    pub fn task_request(session_id: &str, payload: Vec<u8>) -> Self {
        Self::event(TASK_REQUEST, Some(session_id), payload)
    }

    pub fn finish_session(session_id: &str) -> Self {
        Self::event(FINISH_SESSION, Some(session_id), b"{}".to_vec())
    }

    /// 载荷作为文字（错误说明等）。
    pub fn payload_text(&self) -> String {
        String::from_utf8_lossy(&self.payload).into_owned()
    }
}

/// 连接级事件不带会话号。
fn connection_level(event: Option<i32>) -> bool {
    matches!(event, Some(START_CONNECTION | FINISH_CONNECTION | CONNECTION_STARTED | CONNECTION_FAILED))
}

fn has_sequence(message: &Message) -> bool {
    message.msg_type != MsgType::Error
        && message.msg_type != MsgType::Invalid
        && (message.flag == FLAG_POSITIVE_SEQ || message.flag == FLAG_NEGATIVE_SEQ)
}

pub fn marshal(message: &Message) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(32 + message.payload.len());
    buffer.push(0x11);
    buffer.push((message.msg_type.bits() << 4) | message.flag);
    buffer.push(message.serialization << 4);
    buffer.push(0);
    if message.flag == FLAG_WITH_EVENT {
        buffer.extend_from_slice(&message.event.unwrap_or(0).to_be_bytes());
        if !connection_level(message.event) {
            let id = message.session_id.as_deref().unwrap_or("").as_bytes();
            buffer.extend_from_slice(&(id.len() as u32).to_be_bytes());
            buffer.extend_from_slice(id);
        }
    }
    if has_sequence(message) {
        buffer.extend_from_slice(&message.sequence.unwrap_or(0).to_be_bytes());
    }
    if message.msg_type == MsgType::Error {
        buffer.extend_from_slice(&message.error_code.unwrap_or(0).to_be_bytes());
    }
    buffer.extend_from_slice(&(message.payload.len() as u32).to_be_bytes());
    buffer.extend_from_slice(&message.payload);
    buffer
}

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl Reader<'_> {
    fn u32(&mut self) -> Result<u32, ProviderError> {
        let bytes = self
            .data
            .get(self.offset..self.offset + 4)
            .ok_or_else(|| ProviderError::new("合成服务返回的数据不完整"))?;
        self.offset += 4;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn bytes(&mut self) -> Result<&[u8], ProviderError> {
        let length = self.u32()? as usize;
        let bytes = self
            .data
            .get(self.offset..self.offset.saturating_add(length))
            .ok_or_else(|| ProviderError::new("合成服务返回的数据不完整"))?;
        self.offset += length;
        Ok(bytes)
    }
}

pub fn unmarshal(data: &[u8]) -> Result<Message, ProviderError> {
    if data.len() < 4 {
        return Err(ProviderError::new("合成服务返回的数据过短"));
    }
    if data[0] & 0x0f != 1 {
        return Err(ProviderError::new("合成服务返回了不支持的帧头"));
    }
    let mut message = Message {
        msg_type: MsgType::from_bits(data[1] >> 4),
        flag: data[1] & 0x0f,
        serialization: data[2] >> 4,
        event: None,
        session_id: None,
        connect_id: None,
        sequence: None,
        error_code: None,
        payload: Vec::new(),
    };
    let mut reader = Reader { data, offset: 4 };
    if has_sequence(&message) {
        message.sequence = Some(reader.u32()? as i32);
    }
    if message.msg_type == MsgType::Error {
        message.error_code = Some(reader.u32()?);
    }
    if message.flag == FLAG_WITH_EVENT {
        let event = reader.u32()? as i32;
        message.event = Some(event);
        if !connection_level(message.event) && event != CONNECTION_FINISHED {
            let id = String::from_utf8_lossy(reader.bytes()?).into_owned();
            message.session_id = (!id.is_empty()).then_some(id);
        }
        if matches!(event, CONNECTION_STARTED | CONNECTION_FAILED | CONNECTION_FINISHED) {
            let id = String::from_utf8_lossy(reader.bytes()?).into_owned();
            message.connect_id = (!id.is_empty()).then_some(id);
        }
    }
    message.payload = reader.bytes()?.to_vec();
    Ok(message)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 服务端消息（测试用）：带事件；会话事件带会话号，连接事件带连接号。
    pub(crate) fn server_event(msg_type: MsgType, event: i32, id: &str, payload: &[u8]) -> Vec<u8> {
        let mut buffer = vec![0x11, (msg_type.bits() << 4) | FLAG_WITH_EVENT, if msg_type == MsgType::AudioOnlyServer { 0 } else { 0x10 }, 0];
        buffer.extend_from_slice(&event.to_be_bytes());
        buffer.extend_from_slice(&(id.len() as u32).to_be_bytes());
        buffer.extend_from_slice(id.as_bytes());
        buffer.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        buffer.extend_from_slice(payload);
        buffer
    }

    pub(crate) fn server_error(code: u32, text: &str) -> Vec<u8> {
        let mut buffer = vec![0x11, 0xF0, 0x10, 0];
        buffer.extend_from_slice(&code.to_be_bytes());
        buffer.extend_from_slice(&(text.len() as u32).to_be_bytes());
        buffer.extend_from_slice(text.as_bytes());
        buffer
    }

    #[test]
    fn client_messages_match_univoice_bytes() {
        // univoice 测试 p1：StartConnection 共 14 字节
        let bytes = marshal(&Message::start_connection());
        assert_eq!(bytes, vec![0x11, 0x14, 0x10, 0x00, 0, 0, 0, 1, 0, 0, 0, 2, b'{', b'}']);
        // 会话事件写会话号
        let bytes = marshal(&Message::start_session("s-1", br#"{"a":1}"#.to_vec()));
        assert_eq!(&bytes[4..8], &[0, 0, 0, 100]);
        assert_eq!(&bytes[8..12], &[0, 0, 0, 3]);
        assert_eq!(&bytes[12..15], b"s-1");
        assert_eq!(&bytes[15..19], &[0, 0, 0, 7]);
        // 客户端消息能被自己解析回来
        let parsed = unmarshal(&marshal(&Message::task_request("s-1", b"{}".to_vec()))).unwrap();
        assert_eq!((parsed.event, parsed.session_id.as_deref()), (Some(TASK_REQUEST), Some("s-1")));
        assert_eq!(unmarshal(&marshal(&Message::finish_session("s-1"))).unwrap().event, Some(FINISH_SESSION));
    }

    #[test]
    fn server_messages_keep_unknown_events_and_reject_truncation() {
        let started = server_event(MsgType::FullServerResponse, CONNECTION_STARTED, "conn-9", b"{}");
        let parsed = unmarshal(&started).unwrap();
        assert_eq!((parsed.event, parsed.connect_id.as_deref(), parsed.session_id), (Some(50), Some("conn-9"), None));

        // 句子事件 352（univoice 的枚举里没有）：仍按会话事件读会话号
        let audio = server_event(MsgType::AudioOnlyServer, 352, "s-1", &[1, 0, 2, 0]);
        let parsed = unmarshal(&audio).unwrap();
        assert_eq!(parsed.msg_type, MsgType::AudioOnlyServer);
        assert_eq!((parsed.event, parsed.session_id.as_deref()), (Some(352), Some("s-1")));
        assert_eq!(parsed.payload, vec![1, 0, 2, 0]);

        let error = unmarshal(&server_error(45000000, "quota exceeded")).unwrap();
        assert_eq!((error.msg_type, error.error_code), (MsgType::Error, Some(45000000)));
        assert_eq!(error.payload_text(), "quota exceeded");

        assert!(unmarshal(&[0x11, 0x94]).is_err());
        assert!(unmarshal(&started[..started.len() - 1]).is_err(), "载荷不完整");
        let mut huge = started.clone();
        let len = huge.len();
        huge[len - 6..len - 2].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(unmarshal(&huge).is_err(), "载荷长度越界不 panic");
        assert!(unmarshal(&[0x12, 0x94, 0x10, 0]).is_err(), "头长不是 4 字节");
    }
}
