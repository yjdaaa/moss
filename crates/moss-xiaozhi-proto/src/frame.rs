//! 二进制音频帧。
//!
//! 帧格式由连接的协议版本决定（固件在 WebSocket 请求头 `Protocol-Version` 与 hello 的 `version` 中声明，
//! 取值来自 OTA 响应 `websocket.version`）：
//!
//! | 版本 | 格式 |
//! | --- | --- |
//! | 1 | 裸 Opus 包，无头部（固件默认） |
//! | 2 | 16 字节头：version u16、type u16（0 Opus / 1 JSON）、reserved u32、timestamp u32（毫秒）、payload_size u32，均为大端 |
//! | 3 | 4 字节头：type u8、reserved u8、payload_size u16（大端） |
//!
//! 与海门 `crates/haimen-xiaozhi/src/protocol.rs`（`513d9e6`，MIT）的自动检测不同，这里**按连接声明的版本解析**，
//! 避免裸 Opus 首字节恰好是 0 / 1 时被误认成版本 3 的头。头部布局取自固件 `main/protocols/protocol.h`（`0d576d3`）。

use std::fmt;

const V2_HEADER: usize = 16;
const V3_HEADER: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// 不支持的协议版本
    UnsupportedVersion(u32),
    /// 数据短于头部或声明的负载长度
    Truncated,
    /// 头部字段不合法（版本号不符、类型不是音频）
    InvalidHeader,
    /// 负载为空
    Empty,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => write!(f, "不支持的协议版本 {version}"),
            Self::Truncated => f.write_str("音频帧长度不足"),
            Self::InvalidHeader => f.write_str("音频帧头部无效"),
            Self::Empty => f.write_str("音频帧为空"),
        }
    }
}

impl std::error::Error for FrameError {}

/// 解出的音频帧：Opus 负载与时间戳（只有版本 2 带时间戳，其余为 0）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    pub payload: Vec<u8>,
    pub timestamp: u32,
}

/// 按协议版本解析设备发来的二进制帧。
pub fn decode_frame(version: u32, data: &[u8]) -> Result<DecodedFrame, FrameError> {
    let frame = match version {
        1 => DecodedFrame {
            payload: data.to_vec(),
            timestamp: 0,
        },
        2 => {
            if data.len() < V2_HEADER {
                return Err(FrameError::Truncated);
            }
            let header_version = u16::from_be_bytes([data[0], data[1]]);
            let kind = u16::from_be_bytes([data[2], data[3]]);
            if header_version != 2 || kind != 0 {
                return Err(FrameError::InvalidHeader);
            }
            let timestamp = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
            let size = u32::from_be_bytes([data[12], data[13], data[14], data[15]]) as usize;
            let end = V2_HEADER.checked_add(size).ok_or(FrameError::Truncated)?;
            if data.len() < end {
                return Err(FrameError::Truncated);
            }
            DecodedFrame {
                payload: data[V2_HEADER..end].to_vec(),
                timestamp,
            }
        }
        3 => {
            if data.len() < V3_HEADER {
                return Err(FrameError::Truncated);
            }
            if data[0] != 0 {
                return Err(FrameError::InvalidHeader);
            }
            let size = u16::from_be_bytes([data[2], data[3]]) as usize;
            if data.len() < V3_HEADER + size {
                return Err(FrameError::Truncated);
            }
            DecodedFrame {
                payload: data[V3_HEADER..V3_HEADER + size].to_vec(),
                timestamp: 0,
            }
        }
        other => return Err(FrameError::UnsupportedVersion(other)),
    };
    if frame.payload.is_empty() {
        return Err(FrameError::Empty);
    }
    Ok(frame)
}

/// 按协议版本封装发往设备的 Opus 包。
pub fn encode_frame(version: u32, payload: &[u8], timestamp: u32) -> Result<Vec<u8>, FrameError> {
    match version {
        1 => Ok(payload.to_vec()),
        2 => {
            let mut buffer = Vec::with_capacity(V2_HEADER + payload.len());
            buffer.extend_from_slice(&2u16.to_be_bytes());
            buffer.extend_from_slice(&0u16.to_be_bytes());
            buffer.extend_from_slice(&0u32.to_be_bytes());
            buffer.extend_from_slice(&timestamp.to_be_bytes());
            buffer.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            buffer.extend_from_slice(payload);
            Ok(buffer)
        }
        3 => {
            let size = u16::try_from(payload.len()).map_err(|_| FrameError::InvalidHeader)?;
            let mut buffer = Vec::with_capacity(V3_HEADER + payload.len());
            buffer.push(0);
            buffer.push(0);
            buffer.extend_from_slice(&size.to_be_bytes());
            buffer.extend_from_slice(payload);
            Ok(buffer)
        }
        other => Err(FrameError::UnsupportedVersion(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPUS: [u8; 4] = [0x58, 0xFF, 0x00, 0x01];

    #[test]
    fn round_trip_all_versions() {
        for version in [1, 2, 3] {
            let encoded = encode_frame(version, &OPUS, 120).unwrap();
            let decoded = decode_frame(version, &encoded).unwrap();
            assert_eq!(decoded.payload, OPUS, "版本 {version}");
            assert_eq!(decoded.timestamp, if version == 2 { 120 } else { 0 });
        }
    }

    #[test]
    fn v2_layout_matches_firmware_struct() {
        let encoded = encode_frame(2, &OPUS, 0x0102_0304).unwrap();
        assert_eq!(&encoded[0..2], &[0, 2]);
        assert_eq!(&encoded[2..4], &[0, 0]);
        assert_eq!(&encoded[4..8], &[0, 0, 0, 0]);
        assert_eq!(&encoded[8..12], &[1, 2, 3, 4]);
        assert_eq!(&encoded[12..16], &[0, 0, 0, 4]);
        assert_eq!(&encoded[16..], &OPUS);
    }

    #[test]
    fn v3_layout_matches_firmware_struct() {
        let encoded = encode_frame(3, &OPUS, 99).unwrap();
        assert_eq!(&encoded[..4], &[0, 0, 0, 4]);
        assert_eq!(&encoded[4..], &OPUS);
    }

    #[test]
    fn raw_opus_starting_with_zero_is_not_mistaken_for_v3() {
        // 首字节为 0 的裸 Opus 在版本 1 下原样返回
        let raw = [0x00, 0x01, 0x02];
        assert_eq!(decode_frame(1, &raw).unwrap().payload, raw);
    }

    #[test]
    fn invalid_frames_are_rejected() {
        assert_eq!(decode_frame(2, &[0; 10]), Err(FrameError::Truncated));
        let mut wrong_version = encode_frame(2, &OPUS, 0).unwrap();
        wrong_version[1] = 3;
        assert_eq!(
            decode_frame(2, &wrong_version),
            Err(FrameError::InvalidHeader)
        );
        let mut json_type = encode_frame(2, &OPUS, 0).unwrap();
        json_type[3] = 1;
        assert_eq!(decode_frame(2, &json_type), Err(FrameError::InvalidHeader));
        let mut short = encode_frame(3, &OPUS, 0).unwrap();
        short.pop();
        assert_eq!(decode_frame(3, &short), Err(FrameError::Truncated));
        assert_eq!(decode_frame(1, &[]), Err(FrameError::Empty));
        assert_eq!(
            decode_frame(4, &OPUS),
            Err(FrameError::UnsupportedVersion(4))
        );
        assert!(encode_frame(3, &vec![0u8; 70_000], 0).is_err());
    }
}
