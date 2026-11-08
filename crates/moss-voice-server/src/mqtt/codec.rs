//! MQTT 3.1.1 精简编解码：只实现小智固件（ESP-IDF `esp-mqtt`）会用到的报文。
//!
//! 设备发：CONNECT、PUBLISH（QoS 0/1/2）、PUBREL、SUBSCRIBE、UNSUBSCRIBE、PINGREQ、DISCONNECT；
//! MOSS 发：CONNACK、PUBLISH（QoS 0）、PUBACK、PUBREC、PUBCOMP、SUBACK、UNSUBACK、PINGRESP。
//! 小智从不订阅主题，MOSS 直接把下行消息写到这台设备的连接上（与官方网关做法相同）。

use tokio::io::{AsyncRead, AsyncReadExt};

/// 单个报文上限：设备上行的 JSON（含 MCP 工具清单）远小于此。
pub(crate) const MAX_PACKET: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Connect {
    pub protocol_level: u8,
    pub client_id: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub keep_alive: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Packet {
    Connect(Connect),
    Publish {
        topic: String,
        qos: u8,
        packet_id: Option<u16>,
        payload: Vec<u8>,
    },
    PubRel(u16),
    Subscribe {
        packet_id: u16,
        topics: usize,
    },
    Unsubscribe(u16),
    PingReq,
    Disconnect,
    /// 设备回给 MOSS 的确认等，MOSS 只发 QoS 0，不需要处理
    Other(u8),
}

/// 读一个完整报文；对端正常关闭返回 `Ok(None)`。
pub(crate) async fn read_packet<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Packet>, String> {
    let mut first = [0u8; 1];
    match reader.read_exact(&mut first).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }
    let mut len = 0usize;
    let mut shift = 0;
    loop {
        let mut byte = [0u8; 1];
        reader
            .read_exact(&mut byte)
            .await
            .map_err(|error| error.to_string())?;
        len |= usize::from(byte[0] & 0x7F) << shift;
        if byte[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 21 {
            return Err("剩余长度字段超过 4 字节".into());
        }
    }
    if len > MAX_PACKET {
        return Err(format!("报文 {len} 字节，超过上限"));
    }
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|error| error.to_string())?;
    parse(first[0], &body).map(Some)
}

struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn u8(&mut self) -> Result<u8, String> {
        let value = *self.data.get(self.at).ok_or("报文过短")?;
        self.at += 1;
        Ok(value)
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from(self.u8()?) << 8 | u16::from(self.u8()?))
    }
    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let len = usize::from(self.u16()?);
        let end = self.at.checked_add(len).ok_or("报文长度溢出")?;
        let slice = self.data.get(self.at..end).ok_or("字段超出报文")?;
        self.at = end;
        Ok(slice)
    }
    fn string(&mut self) -> Result<String, String> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| "字段不是 UTF-8".to_owned())
    }
    fn rest(&self) -> &'a [u8] {
        &self.data[self.at.min(self.data.len())..]
    }
}

fn parse(header: u8, body: &[u8]) -> Result<Packet, String> {
    let mut cur = Cursor { data: body, at: 0 };
    match header >> 4 {
        1 => {
            let name = cur.string()?;
            let level = cur.u8()?;
            if !matches!((name.as_str(), level), ("MQTT", 4) | ("MQIsdp", 3)) {
                return Err(format!("不支持的协议 {name} v{level}"));
            }
            let flags = cur.u8()?;
            if flags & 0x01 != 0 {
                return Err("CONNECT 保留位不为 0".into());
            }
            let keep_alive = cur.u16()?;
            let client_id = cur.string()?;
            if flags & 0x04 != 0 {
                cur.string()?; // will topic
                cur.bytes()?; // will message
            }
            let username = if flags & 0x80 != 0 {
                Some(cur.string()?)
            } else {
                None
            };
            let password = if flags & 0x40 != 0 {
                Some(String::from_utf8_lossy(cur.bytes()?).into_owned())
            } else {
                None
            };
            Ok(Packet::Connect(Connect {
                protocol_level: level,
                client_id,
                username,
                password,
                keep_alive,
            }))
        }
        3 => {
            let qos = (header >> 1) & 0x03;
            if qos > 2 {
                return Err("PUBLISH QoS 无效".into());
            }
            let topic = cur.string()?;
            let packet_id = if qos > 0 { Some(cur.u16()?) } else { None };
            Ok(Packet::Publish {
                topic,
                qos,
                packet_id,
                payload: cur.rest().to_vec(),
            })
        }
        6 => Ok(Packet::PubRel(cur.u16()?)),
        8 => {
            let packet_id = cur.u16()?;
            let mut topics = 0;
            while !cur.rest().is_empty() {
                cur.string()?;
                cur.u8()?;
                topics += 1;
            }
            Ok(Packet::Subscribe { packet_id, topics })
        }
        10 => Ok(Packet::Unsubscribe(cur.u16()?)),
        12 => Ok(Packet::PingReq),
        14 => Ok(Packet::Disconnect),
        kind @ (4 | 5 | 7) => Ok(Packet::Other(kind)),
        kind => Err(format!("不认识的报文类型 {kind}")),
    }
}

fn frame(header: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push(header);
    let mut len = body.len();
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
    out.extend_from_slice(body);
    out
}

/// CONNACK：`code` 0 接受，4 用户名或密码错误，5 未授权。
pub(crate) fn connack(code: u8) -> Vec<u8> {
    frame(0x20, &[0, code])
}

/// QoS 0 的 PUBLISH。
pub(crate) fn publish(topic: &str, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(topic.len() + payload.len() + 2);
    body.extend_from_slice(&(topic.len() as u16).to_be_bytes());
    body.extend_from_slice(topic.as_bytes());
    body.extend_from_slice(payload);
    frame(0x30, &body)
}

pub(crate) fn puback(id: u16) -> Vec<u8> {
    frame(0x40, &id.to_be_bytes())
}

pub(crate) fn pubrec(id: u16) -> Vec<u8> {
    frame(0x50, &id.to_be_bytes())
}

pub(crate) fn pubcomp(id: u16) -> Vec<u8> {
    frame(0x70, &id.to_be_bytes())
}

/// 每个订阅都按 QoS 0 授予。
pub(crate) fn suback(id: u16, topics: usize) -> Vec<u8> {
    let mut body = id.to_be_bytes().to_vec();
    body.extend(std::iter::repeat_n(0u8, topics.max(1)));
    frame(0x90, &body)
}

pub(crate) fn unsuback(id: u16) -> Vec<u8> {
    frame(0xB0, &id.to_be_bytes())
}

pub(crate) fn pingresp() -> Vec<u8> {
    frame(0xD0, &[])
}

/// 测试与假设备用：编码一个 CONNECT（3.1.1，带用户名密码）。
#[cfg(test)]
pub(crate) fn connect(client_id: &str, username: &str, password: &str, keep_alive: u16) -> Vec<u8> {
    let mut body = vec![0, 4];
    body.extend_from_slice(b"MQTT");
    body.push(4);
    body.push(0x80 | 0x40 | 0x02);
    body.extend_from_slice(&keep_alive.to_be_bytes());
    for field in [client_id, username, password] {
        body.extend_from_slice(&(field.len() as u16).to_be_bytes());
        body.extend_from_slice(field.as_bytes());
    }
    frame(0x10, &body)
}

/// 测试与假设备用：编码一个 QoS 1 的 PUBLISH。
#[cfg(test)]
pub(crate) fn publish_qos1(topic: &str, id: u16, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&(topic.len() as u16).to_be_bytes());
    body.extend_from_slice(topic.as_bytes());
    body.extend_from_slice(&id.to_be_bytes());
    body.extend_from_slice(payload);
    frame(0x32, &body)
}

/// 测试用：解析 MOSS 发出的报文（与设备上行同一套规则，额外认 CONNACK 等下行报文）。
#[cfg(test)]
pub(crate) fn parse_downlink(bytes: &[u8]) -> (u8, Vec<u8>) {
    let mut len = 0usize;
    let mut shift = 0;
    let mut at = 1;
    loop {
        let byte = bytes[at];
        at += 1;
        len |= usize::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    (bytes[0], bytes[at..at + len].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn roundtrip(bytes: Vec<u8>) -> Packet {
        let mut reader = bytes.as_slice();
        read_packet(&mut reader).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn parses_device_packets() {
        let packet = roundtrip(connect("uuid-1", "aa:bb", "tok", 240)).await;
        assert_eq!(
            packet,
            Packet::Connect(Connect {
                protocol_level: 4,
                client_id: "uuid-1".into(),
                username: Some("aa:bb".into()),
                password: Some("tok".into()),
                keep_alive: 240,
            })
        );
        let packet = roundtrip(publish_qos1("device-server", 7, b"{\"type\":\"hello\"}")).await;
        assert_eq!(
            packet,
            Packet::Publish {
                topic: "device-server".into(),
                qos: 1,
                packet_id: Some(7),
                payload: b"{\"type\":\"hello\"}".to_vec(),
            }
        );
        assert_eq!(roundtrip(vec![0xC0, 0]).await, Packet::PingReq);
        assert_eq!(roundtrip(vec![0xE0, 0]).await, Packet::Disconnect);
        let mut empty: &[u8] = &[];
        assert_eq!(read_packet(&mut empty).await.unwrap(), None);
    }

    #[tokio::test]
    async fn large_publish_uses_multibyte_length() {
        let payload = vec![b'x'; 20_000];
        let bytes = publish("t", &payload);
        assert_eq!(bytes[1] & 0x80, 0x80, "长度要多字节编码");
        match roundtrip(bytes).await {
            Packet::Publish {
                payload: got, qos, ..
            } => {
                assert_eq!(got.len(), 20_000);
                assert_eq!(qos, 0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_bad_packets() {
        let mut oversized: &[u8] = &[0x30, 0xFF, 0xFF, 0xFF, 0x7F];
        assert!(read_packet(&mut oversized).await.is_err());
        let mut bad_proto = connect("c", "u", "p", 60);
        bad_proto[4] = b'X'; // 协议名 MQTT → XQTT
        let mut reader = bad_proto.as_slice();
        assert!(read_packet(&mut reader).await.is_err());
    }

    #[test]
    fn encodes_server_packets() {
        assert_eq!(connack(0), vec![0x20, 2, 0, 0]);
        assert_eq!(connack(5), vec![0x20, 2, 0, 5]);
        assert_eq!(pingresp(), vec![0xD0, 0]);
        assert_eq!(puback(0x0102), vec![0x40, 2, 1, 2]);
        assert_eq!(suback(3, 2), vec![0x90, 4, 0, 3, 0, 0]);
        let (header, body) = parse_downlink(&publish("devices/p2p/x", b"hi"));
        assert_eq!(header, 0x30);
        assert_eq!(&body[2..15], b"devices/p2p/x");
        assert_eq!(&body[15..], b"hi");
    }
}
