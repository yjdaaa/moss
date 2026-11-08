//! OTA 接口（`POST /xiaozhi/ota/`）。
//!
//! 固件启动时发送：请求头 `Device-Id`（MAC）、`Client-Id`（设备 UUID）、`Activation-Version`、`User-Agent`，
//! 正文为系统信息 JSON（含 `application.version`、`board.type` 等）。响应中设备读取：
//! `websocket{url, token, version}`（写入设备设置后用于连接）、`server_time{timestamp(ms), timezone_offset(分钟)}`、
//! `firmware{version, url[, force]}`。依据固件 `main/ota.cc`（`0d576d3`）与 xiaozhi-esp32-server
//! `core/api/ota_handler.py`（`7a260ee`，MIT）。
//!
//! 响应**不包含** `mqtt` 段（固件据此改用 WebSocket），也不包含 `activation`（MOSS 用桌面端“允许设备”代替激活码）。

use serde::Serialize;
use serde_json::Value;

/// 从 OTA 请求中取出的设备信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OtaInfo {
    pub device_id: String,
    pub client_id: String,
    /// 设备型号（`board.type`），用于固件匹配
    pub model: String,
    /// 当前固件版本（`application.version`）
    pub version: String,
    /// 设备上报的名称（`board.name`，可能为空）
    pub board_name: String,
}

fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty() && text.len() <= 128)
}

impl OtaInfo {
    /// `header` 按名称（小写）取请求头；`body` 为请求正文。
    pub fn from_request(
        header: impl Fn(&str) -> Option<String>,
        body: &str,
    ) -> Result<Self, String> {
        let device_id = clean(header("device-id")).ok_or("OTA 请求缺少 Device-Id")?;
        let client_id = clean(header("client-id")).ok_or("OTA 请求缺少 Client-Id")?;
        let json: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let model = clean(header("device-model"))
            .or_else(|| {
                clean(
                    json.pointer("/board/type")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                )
            })
            .unwrap_or_else(|| "default".into());
        let version = clean(header("device-version"))
            .or_else(|| {
                clean(
                    json.pointer("/application/version")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                )
            })
            .unwrap_or_else(|| "0.0.0".into());
        let board_name = clean(
            json.pointer("/board/name")
                .and_then(Value::as_str)
                .map(str::to_owned),
        )
        .unwrap_or_default();
        Ok(Self {
            device_id,
            client_id,
            model,
            version,
            board_name,
        })
    }
}

/// 固件下发信息（NV17 使用；NV1 不下发新固件）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FirmwareOffer {
    pub version: String,
    pub url: String,
}

/// n71 常连模式：OTA 回复里的 `mqtt` 段。固件见到它就改用 MQTT + UDP（优先于 `websocket`），
/// 下次 OTA 回复不带它时回到 WebSocket。端口 8883 会让固件改走 TLS，不要用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MqttOffer {
    /// `主机:端口`
    pub endpoint: String,
    pub client_id: String,
    pub username: String,
    pub password: String,
    pub publish_topic: String,
    pub keepalive: u32,
}

/// OTA 响应。
#[derive(Debug, Clone, PartialEq)]
pub struct OtaReply {
    pub websocket_url: String,
    /// 未允许的设备为空串
    pub token: String,
    /// 音频帧协议版本（见 `frame` 模块）
    pub protocol_version: u32,
    pub now_ms: u64,
    pub timezone_offset_minutes: i32,
    /// 设备当前版本（无新固件时原样返回，设备据此判断无需升级）
    pub current_version: String,
    pub firmware: Option<FirmwareOffer>,
    /// n71 常连模式（只发给已允许的设备）；`None` 时设备用 WebSocket
    pub mqtt: Option<MqttOffer>,
}

impl OtaReply {
    pub fn to_json(&self) -> Value {
        let firmware = match &self.firmware {
            Some(offer) => serde_json::json!({"version": offer.version, "url": offer.url}),
            None => serde_json::json!({"version": self.current_version, "url": ""}),
        };
        let mut reply = serde_json::json!({
            "server_time": {"timestamp": self.now_ms, "timezone_offset": self.timezone_offset_minutes},
            "firmware": firmware,
            "websocket": {"url": self.websocket_url, "token": self.token, "version": self.protocol_version},
        });
        if let Some(mqtt) = &self.mqtt {
            reply["mqtt"] = serde_json::json!({
                "endpoint": mqtt.endpoint,
                "client_id": mqtt.client_id,
                "username": mqtt.username,
                "password": mqtt.password,
                "publish_topic": mqtt.publish_topic,
                "keepalive": mqtt.keepalive,
            });
        }
        reply
    }
}

fn numeric_parts(version: &str) -> Vec<u64> {
    let mut parts = Vec::new();
    let mut current: Option<u64> = None;
    for ch in version.chars() {
        if let Some(digit) = ch.to_digit(10) {
            current = Some(
                current
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit as u64),
            );
        } else if let Some(value) = current.take() {
            parts.push(value);
        }
    }
    if let Some(value) = current {
        parts.push(value);
    }
    if parts.is_empty() { vec![0] } else { parts }
}

/// `candidate` 是否比 `current` 新（逐段比较数字，缺的段按 0 计；与 esp32-server `_is_higher_version` 相同）。
pub fn is_newer_version(candidate: &str, current: &str) -> bool {
    let (a, b) = (numeric_parts(candidate), numeric_parts(current));
    for index in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(index).copied().unwrap_or(0),
            b.get(index).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn headers(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    #[test]
    fn request_fields_come_from_headers_and_body() {
        let body = r#"{"application":{"name":"xiaozhi","version":"1.9.2"},"board":{"type":"lichuang-dev","name":"立创实战派"}}"#;
        let info = OtaInfo::from_request(
            headers(&[("device-id", "aa:bb:cc:dd:ee:ff"), ("client-id", "uuid-1")]),
            body,
        )
        .unwrap();
        assert_eq!(info.device_id, "aa:bb:cc:dd:ee:ff");
        assert_eq!(info.model, "lichuang-dev");
        assert_eq!(info.version, "1.9.2");
        assert_eq!(info.board_name, "立创实战派");
        let info =
            OtaInfo::from_request(headers(&[("device-id", "d"), ("client-id", "c")]), "").unwrap();
        assert_eq!(info.model, "default");
        assert_eq!(info.version, "0.0.0");
        assert!(OtaInfo::from_request(headers(&[("client-id", "c")]), "").is_err());
        assert!(
            OtaInfo::from_request(headers(&[("device-id", "d"), ("client-id", " ")]), "").is_err()
        );
    }

    #[test]
    fn reply_has_no_mqtt_or_activation() {
        let reply = OtaReply {
            websocket_url: "ws://192.168.1.5:8000/xiaozhi/v1/".into(),
            token: "t.1".into(),
            protocol_version: 1,
            now_ms: 1_000,
            timezone_offset_minutes: 480,
            current_version: "1.9.2".into(),
            firmware: None,
            mqtt: None,
        };
        let json = reply.to_json();
        assert_eq!(
            json["websocket"]["url"],
            "ws://192.168.1.5:8000/xiaozhi/v1/"
        );
        assert_eq!(json["websocket"]["version"], 1);
        assert_eq!(json["firmware"]["version"], "1.9.2");
        assert_eq!(json["firmware"]["url"], "");
        assert_eq!(json["server_time"]["timezone_offset"], 480);
        assert!(json.get("mqtt").is_none() && json.get("activation").is_none());

        // n71 常连模式：带上 mqtt 段，websocket 段照旧保留（关掉常连后设备据此回到 WebSocket）
        let with_mqtt = OtaReply {
            mqtt: Some(MqttOffer {
                endpoint: "192.168.1.5:1883".into(),
                client_id: "uuid-1".into(),
                username: "aa:bb".into(),
                password: "t.1".into(),
                publish_topic: "device-server".into(),
                keepalive: 240,
            }),
            ..reply
        }
        .to_json();
        assert_eq!(with_mqtt["mqtt"]["endpoint"], "192.168.1.5:1883");
        assert_eq!(with_mqtt["mqtt"]["keepalive"], 240);
        assert_eq!(with_mqtt["mqtt"]["password"], "t.1");
        assert_eq!(with_mqtt["websocket"]["token"], "t.1");
    }

    #[test]
    fn version_compare() {
        assert!(is_newer_version("1.10.0", "1.9.9"));
        assert!(is_newer_version("2.0", "1.99.99"));
        assert!(!is_newer_version("1.9.2", "1.9.2"));
        assert!(!is_newer_version("1.9", "1.9.0"));
        assert!(is_newer_version("1.9.0.1", "1.9"));
        assert!(!is_newer_version("abc", "0.0.1"));
    }
}
