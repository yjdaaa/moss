//! 设备 ↔ 服务端的 JSON 文本消息。
//!
//! 字段依据固件 `main/application.cc`、`main/protocols/websocket_protocol.cc`（`0d576d3`）与
//! xiaozhi-esp32-server `core/handle/textMessageType.py`（`7a260ee`）；结构参考海门
//! `crates/haimen-xiaozhi/src/types.rs`（`513d9e6`，MIT）。
//!
//! 解析时**不认识的消息类型不报错**（海门会因此回 `parse_error`），而是归入 [`ClientMessage::Other`]，
//! 由会话层决定忽略或记录，避免新版固件多发一种消息就断开。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 音频参数（hello 双向协商）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioParams {
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    #[serde(default = "default_channels")]
    pub channels: u8,
    #[serde(default = "default_frame_duration")]
    pub frame_duration: u32,
}

fn default_format() -> String {
    "opus".into()
}
const fn default_sample_rate() -> u32 {
    16_000
}
const fn default_channels() -> u8 {
    1
}
const fn default_frame_duration() -> u32 {
    60
}

impl Default for AudioParams {
    fn default() -> Self {
        Self {
            format: default_format(),
            sample_rate: default_sample_rate(),
            channels: default_channels(),
            frame_duration: default_frame_duration(),
        }
    }
}

impl AudioParams {
    pub fn opus(sample_rate: u32, frame_duration: u32) -> Self {
        Self { format: "opus".into(), sample_rate, channels: 1, frame_duration }
    }
}

/// 设备能力声明。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Features {
    #[serde(default)]
    pub aec: bool,
    #[serde(default)]
    pub mcp: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenState {
    /// 唤醒词检测（可带唤醒词文本）
    Detect,
    /// 开始拾音
    Start,
    /// 停止拾音（手动模式松开按键）
    Stop,
}

/// 设备发来的消息。
#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    Hello { version: u32, transport: String, audio_params: AudioParams, features: Features },
    Listen { state: ListenState, mode: Option<String>, text: Option<String> },
    Abort { reason: Option<String> },
    /// 设备 MCP 的 JSON-RPC 负载
    Mcp { payload: Value },
    /// 旧版 IoT 描述或状态
    Iot { body: Value },
    Ping,
    Goodbye,
    /// 不认识的类型，原样保留
    Other { kind: String, body: Value },
}

fn text_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

impl ClientMessage {
    /// 解析一条文本消息。只有不是 JSON 对象、缺少 `type` 或已知类型的必需字段不合法时才报错。
    pub fn parse(text: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(text).map_err(|_| "消息不是合法的 JSON".to_owned())?;
        let kind = value.get("type").and_then(Value::as_str).ok_or("消息缺少 type 字段")?.to_owned();
        Ok(match kind.as_str() {
            "hello" => Self::Hello {
                version: value.get("version").and_then(Value::as_u64).unwrap_or(1) as u32,
                transport: text_field(&value, "transport").unwrap_or_else(|| "websocket".into()),
                audio_params: value
                    .get("audio_params")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|_| "hello 的 audio_params 不合法".to_owned())?
                    .unwrap_or_default(),
                features: value
                    .get("features")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|_| "hello 的 features 不合法".to_owned())?
                    .unwrap_or_default(),
            },
            "listen" => {
                let state = match value.get("state").and_then(Value::as_str) {
                    Some("detect") => ListenState::Detect,
                    Some("start") => ListenState::Start,
                    Some("stop") => ListenState::Stop,
                    _ => return Err("listen 的 state 不合法".into()),
                };
                Self::Listen { state, mode: text_field(&value, "mode"), text: text_field(&value, "text") }
            }
            "abort" => Self::Abort { reason: text_field(&value, "reason") },
            "mcp" => Self::Mcp { payload: value.get("payload").cloned().unwrap_or(Value::Null) },
            "iot" => Self::Iot { body: value },
            "ping" => Self::Ping,
            "goodbye" => Self::Goodbye,
            _ => Self::Other { kind, body: value },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsState {
    Start,
    SentenceStart,
    Stop,
}

/// 发往设备的消息。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Hello { version: u32, transport: String, session_id: String, audio_params: AudioParams },
    /// 识别结果，设备显示为用户说的话
    Stt { session_id: String, text: String },
    /// 合成状态；`sentence_start` 带文字，设备显示为助手说的话
    Tts {
        session_id: String,
        state: TtsState,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// 表情
    Llm { session_id: String, text: String, emotion: String },
    /// 设备弹出提示（状态、文字、表情）
    Alert { status: String, message: String, emotion: String },
    /// 设备 MCP 的 JSON-RPC 负载
    Mcp { session_id: String, payload: Value },
    Pong,
}

impl ServerMessage {
    pub fn to_text(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_hello_is_parsed() {
        // 固件 GetHelloMessage 的实际格式
        let text = r#"{"type":"hello","version":1,"features":{"mcp":true},"transport":"websocket","audio_params":{"format":"opus","sample_rate":16000,"channels":1,"frame_duration":60}}"#;
        match ClientMessage::parse(text).unwrap() {
            ClientMessage::Hello { version, transport, audio_params, features } => {
                assert_eq!(version, 1);
                assert_eq!(transport, "websocket");
                assert_eq!(audio_params, AudioParams::opus(16_000, 60));
                assert!(features.mcp);
                assert!(!features.aec);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn listen_abort_and_unknown_types() {
        assert_eq!(
            ClientMessage::parse(r#"{"session_id":"s","type":"listen","state":"start","mode":"auto"}"#).unwrap(),
            ClientMessage::Listen { state: ListenState::Start, mode: Some("auto".into()), text: None }
        );
        assert_eq!(
            ClientMessage::parse(r#"{"type":"listen","state":"detect","text":"你好小智"}"#).unwrap(),
            ClientMessage::Listen { state: ListenState::Detect, mode: None, text: Some("你好小智".into()) }
        );
        assert_eq!(
            ClientMessage::parse(r#"{"type":"abort","reason":"wake_word_detected"}"#).unwrap(),
            ClientMessage::Abort { reason: Some("wake_word_detected".into()) }
        );
        match ClientMessage::parse(r#"{"type":"mcp","payload":{"jsonrpc":"2.0","id":1,"result":{}}}"#).unwrap() {
            ClientMessage::Mcp { payload } => assert_eq!(payload["id"], 1),
            other => panic!("{other:?}"),
        }
        match ClientMessage::parse(r#"{"type":"future_feature","x":1}"#).unwrap() {
            ClientMessage::Other { kind, .. } => assert_eq!(kind, "future_feature"),
            other => panic!("{other:?}"),
        }
        assert!(ClientMessage::parse("not json").is_err());
        assert!(ClientMessage::parse(r#"{"state":"start"}"#).is_err());
        assert!(ClientMessage::parse(r#"{"type":"listen","state":"pause"}"#).is_err());
    }

    #[test]
    fn server_messages_match_firmware_expectations() {
        let hello = ServerMessage::Hello {
            version: 1,
            transport: "websocket".into(),
            session_id: "abc".into(),
            audio_params: AudioParams::opus(24_000, 60),
        };
        let value: Value = serde_json::from_str(&hello.to_text()).unwrap();
        assert_eq!(value["type"], "hello");
        assert_eq!(value["transport"], "websocket");
        assert_eq!(value["audio_params"]["sample_rate"], 24_000);
        let tts = ServerMessage::Tts { session_id: "abc".into(), state: TtsState::SentenceStart, text: Some("你好".into()) };
        let value: Value = serde_json::from_str(&tts.to_text()).unwrap();
        assert_eq!(value["state"], "sentence_start");
        assert_eq!(value["text"], "你好");
        let stop = ServerMessage::Tts { session_id: "abc".into(), state: TtsState::Stop, text: None };
        assert!(!stop.to_text().contains("text"));
        let alert = ServerMessage::Alert { status: "s".into(), message: "m".into(), emotion: "sad".into() };
        let value: Value = serde_json::from_str(&alert.to_text()).unwrap();
        assert_eq!(value["type"], "alert");
        assert_eq!(value["emotion"], "sad");
    }
}
