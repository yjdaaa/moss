//! WAV 编解码：识别时把一段话打包成 WAV 上传；合成结果按 WAV 解析（16 位整数或 32 位浮点，单 / 双声道）。

use crate::{Pcm, ProviderError};

/// 16 位单声道 WAV。
pub fn encode_wav(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// 解析 WAV 为单声道 16 位 PCM。流式 WAV 的 data 长度常写成 0xFFFFFFFF，按实际剩余字节处理。
pub fn decode_wav(bytes: &[u8]) -> Result<Pcm, ProviderError> {
    let not_wav = || ProviderError::new("合成接口返回的不是 WAV 音频（请确认该接口支持 response_format=wav）");
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(not_wav());
    }
    let mut at = 12;
    let mut format: Option<(u16, u16, u32, u16)> = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32_at(bytes, at + 4).ok_or_else(not_wav)? as usize;
        let body = at + 8;
        if id == b"fmt " {
            let tag = u16_at(bytes, body).ok_or_else(not_wav)?;
            let channels = u16_at(bytes, body + 2).ok_or_else(not_wav)?;
            let rate = u32_at(bytes, body + 4).ok_or_else(not_wav)?;
            let bits = u16_at(bytes, body + 14).ok_or_else(not_wav)?;
            format = Some((tag, channels, rate, bits));
        } else if id == b"data" {
            let (tag, channels, rate, bits) =
                format.ok_or_else(|| ProviderError::new("WAV 缺少格式信息"))?;
            let end = body.saturating_add(size).min(bytes.len());
            let data = &bytes[body..end];
            let channels = channels.max(1) as usize;
            let mono: Vec<i16> = match (tag, bits) {
                (1, 16) => {
                    let frames = data.len() / (2 * channels);
                    (0..frames)
                        .map(|frame| {
                            let sum: i32 = (0..channels)
                                .map(|ch| {
                                    let at = (frame * channels + ch) * 2;
                                    i16::from_le_bytes([data[at], data[at + 1]]) as i32
                                })
                                .sum();
                            (sum / channels as i32) as i16
                        })
                        .collect()
                }
                (3, 32) => {
                    let frames = data.len() / (4 * channels);
                    (0..frames)
                        .map(|frame| {
                            let sum: f32 = (0..channels)
                                .map(|ch| {
                                    let at = (frame * channels + ch) * 4;
                                    f32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
                                })
                                .sum();
                            ((sum / channels as f32).clamp(-1.0, 1.0) * 32767.0) as i16
                        })
                        .collect()
                }
                _ => return Err(ProviderError::new(format!("不支持的 WAV 编码（格式 {tag}，{bits} 位）"))),
            };
            if rate == 0 {
                return Err(not_wav());
            }
            return Ok(Pcm { samples: mono, sample_rate: rate });
        }
        // 块按偶数字节对齐
        at = body.saturating_add(size).saturating_add(size & 1);
    }
    Err(not_wav())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let samples: Vec<i16> = (0..1000).map(|i| (i * 13 % 2000 - 1000) as i16).collect();
        let pcm = decode_wav(&encode_wav(&samples, 16_000)).unwrap();
        assert_eq!(pcm, Pcm { samples, sample_rate: 16_000 });
    }

    #[test]
    fn stereo_float_extra_chunks_and_streaming_length() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF\xff\xff\xff\xffWAVE");
        wav.extend_from_slice(b"LIST\x03\x00\x00\x00abc\x00"); // 奇数长度块 + 填充字节
        wav.extend_from_slice(b"fmt \x10\x00\x00\x00");
        wav.extend_from_slice(&3u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&24_000u32.to_le_bytes());
        wav.extend_from_slice(&(24_000u32 * 8).to_le_bytes());
        wav.extend_from_slice(&8u16.to_le_bytes());
        wav.extend_from_slice(&32u16.to_le_bytes());
        wav.extend_from_slice(b"data\xff\xff\xff\xff");
        for (left, right) in [(0.5f32, 0.5f32), (1.0, 0.0), (-2.0, -2.0)] {
            wav.extend_from_slice(&left.to_le_bytes());
            wav.extend_from_slice(&right.to_le_bytes());
        }
        let pcm = decode_wav(&wav).unwrap();
        assert_eq!(pcm.sample_rate, 24_000);
        assert_eq!(pcm.samples, vec![16383, 16383, -32767]);
    }

    #[test]
    fn rejects_non_wav() {
        assert!(decode_wav(b"ID3\x04mp3data....").unwrap_err().to_string().contains("不是 WAV"));
        assert!(decode_wav(b"").is_err());
        let mut eight_bit = encode_wav(&[0; 4], 8000);
        eight_bit[34] = 8;
        assert!(decode_wav(&eight_bit).unwrap_err().to_string().contains("不支持"));
    }
}
