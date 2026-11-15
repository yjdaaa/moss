//! Opus 编解码（单声道，16 位 PCM）。
//!
//! 后端为官方参考实现 libopus（opus2 `backend-libopus` + `bundled`，构建需要 CMake）。小智固件用的也是 libopus，
//! 两端必须逐位兼容：原用的纯 Rust 实现 mousiki 0.2.1 在语音模式下不兼容（技术书 n94），
//! 本模块的 `*_matches_reference_libopus` 测试用参考 libopus 编出的包把关。
//! 设备上行 16 kHz、下行 24 kHz，均为 60 ms 帧（技术书 §2.1）。

use opus2::{Application, Channels, Decoder, Encoder};

/// Opus 允许的采样率。
pub fn supported_rate(rate: u32) -> bool {
    matches!(rate, 8_000 | 12_000 | 16_000 | 24_000 | 48_000)
}

pub fn frame_samples(rate: u32, frame_ms: u32) -> usize {
    (rate as usize) * (frame_ms as usize) / 1000
}

/// 解码设备上行音频。每个会话一个，保留解码器状态（丢包隐藏依赖前一帧）。
pub struct OpusDecoder {
    inner: Decoder,
    buffer: Vec<i16>,
}

impl OpusDecoder {
    pub fn new(rate: u32) -> Result<Self, String> {
        if !supported_rate(rate) {
            return Err(format!("不支持的采样率 {rate} Hz"));
        }
        let inner = Decoder::new(rate, Channels::Mono)
            .map_err(|error| format!("创建 Opus 解码器失败：{error}"))?;
        // Opus 单包最长 120 ms
        Ok(Self {
            inner,
            buffer: vec![0; frame_samples(rate, 120)],
        })
    }

    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>, String> {
        let samples = self
            .inner
            .decode(packet, &mut self.buffer, false)
            .map_err(|error| format!("Opus 解码失败：{error}"))?;
        Ok(self.buffer[..samples].to_vec())
    }
}

/// 编码下行音频。整个会话只用一个编码器，句与句之间不重建，避免设备端出现爆音（海门连续性泵的做法）。
pub struct OpusEncoder {
    inner: Encoder,
    frame: usize,
    packet: Vec<u8>,
}

impl OpusEncoder {
    pub fn new(rate: u32, frame_ms: u32) -> Result<Self, String> {
        if !supported_rate(rate) {
            return Err(format!("不支持的采样率 {rate} Hz"));
        }
        // 与 xiaozhi-esp32-server 一致用 Audio 模式（Voip 模式在 24 kHz 下实测回放音量只有原声约 0.7）
        let mut inner = Encoder::new(rate, Channels::Mono, Application::Audio)
            .map_err(|error| format!("创建 Opus 编码器失败：{error}"))?;
        let _ = inner.set_vbr(true);
        let _ = inner.set_complexity(10);
        Ok(Self {
            inner,
            frame: frame_samples(rate, frame_ms),
            packet: vec![0; 4000],
        })
    }

    /// 每帧样本数。
    pub fn frame(&self) -> usize {
        self.frame
    }

    /// 编码一帧；不足一帧时补零。
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>, String> {
        let length = if pcm.len() == self.frame {
            self.inner.encode(pcm, &mut self.packet)
        } else {
            let mut block = pcm[..pcm.len().min(self.frame)].to_vec();
            block.resize(self.frame, 0);
            self.inner.encode(&block, &mut self.packet)
        }
        .map_err(|error| format!("Opus 编码失败：{error}"))?;
        Ok(self.packet[..length].to_vec())
    }

    /// 打断后清掉编码器内部状态。
    pub fn reset(&mut self) {
        let _ = self.inner.reset_state();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::test_signal::{rms, tone, vowel};

    #[test]
    fn round_trip_keeps_length_and_level() {
        for rate in [16_000, 24_000] {
            let mut encoder = OpusEncoder::new(rate, 60).unwrap();
            let mut decoder = OpusDecoder::new(rate).unwrap();
            let input = tone(rate, 440.0, 1200, 8000.0);
            let mut output = Vec::new();
            for chunk in input.chunks(encoder.frame()) {
                let packet = encoder.encode(chunk).unwrap();
                assert!(!packet.is_empty() && packet.len() < 1000);
                output.extend(decoder.decode(&packet).unwrap());
            }
            assert_eq!(output.len(), input.len(), "{rate} Hz 每帧解码出完整样本");
            // 跳过编解码延迟所在的第一帧再比较音量
            let skip = encoder.frame();
            let ratio = rms(&output[skip..]) / rms(&input[skip..]);
            assert!((0.7..1.3).contains(&ratio), "{rate} Hz 音量比 {ratio}");
        }
    }

    /// 设备上行由固件里的 libopus 编码（语音模式）。夹具是参考 libopus 1.5 以 16 kHz / 60 ms / VOIP
    /// 编出的一段合成元音（与 `test_signal::vowel` 同一算法）；解码结果必须与原信号同响度、能对上波形。
    /// 不兼容的解码器（如 mousiki 0.2.1）在这里得到约 3 倍响度、单帧最高 13 倍并削顶。
    #[test]
    fn uplink_decoding_matches_reference_libopus() {
        let bytes = include_bytes!("testdata/vowel16k_libopus_voip.pkts");
        let mut decoder = OpusDecoder::new(16_000).unwrap();
        let mut output = Vec::new();
        let mut rest = &bytes[..];
        while rest.len() >= 4 {
            let length = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
            output.extend(decoder.decode(&rest[4..4 + length]).unwrap());
            rest = &rest[4 + length..];
        }
        let input = vowel(16_000, 2400);
        assert_eq!(output.len(), input.len().div_ceil(960) * 960);
        assert_level_and_shape(&input, &output, "上行");
    }

    /// 设备用 libopus 解码下行：我们编出的包经（已被上一项证明符合参考实现的）解码器还原后，
    /// 响度必须和原声一致、不能削顶。合成元音会走 Opus 的语音编码路径（纯音调走不到）。
    #[test]
    fn downlink_encoding_matches_reference_libopus() {
        let input = vowel(24_000, 2400);
        let mut encoder = OpusEncoder::new(24_000, 60).unwrap();
        let mut decoder = OpusDecoder::new(24_000).unwrap();
        let mut output = Vec::new();
        for chunk in input.chunks(encoder.frame()) {
            output.extend(decoder.decode(&encoder.encode(chunk).unwrap()).unwrap());
        }
        assert_level_and_shape(&input, &output, "下行");
    }

    /// 响度比在 0.8–1.25、逐帧响度比不超过 1.6、不削顶，并且对齐后波形能对上（信噪比 ≥ 8 dB）。
    fn assert_level_and_shape(input: &[i16], output: &[i16], label: &str) {
        let ratio = rms(output) / rms(input);
        assert!((0.8..1.25).contains(&ratio), "{label} 整体响度比 {ratio}");
        let clipped = output.iter().filter(|s| s.unsigned_abs() >= 32_700).count();
        assert_eq!(clipped, 0, "{label} 削顶样本 {clipped}");
        let frame = input.len() / 40;
        let mut worst: f64 = 0.0;
        for (a, b) in input.chunks(frame).zip(output.chunks(frame)) {
            if rms(a) > 1500.0 {
                worst = worst.max(rms(b) / rms(a));
            }
        }
        assert!(worst < 1.6, "{label} 单帧响度比最高 {worst}");
        // 编解码有固定延迟：在 0–20 ms 内找最佳对齐
        let middle = &input[input.len() / 4..input.len() * 3 / 4];
        let start = input.len() / 4;
        let snr = (0..input.len() / 50)
            .map(|lag| {
                let error: f64 = middle
                    .iter()
                    .enumerate()
                    .map(|(i, &x)| {
                        let y = output.get(start + i + lag).copied().unwrap_or(0);
                        (f64::from(x) - f64::from(y)).powi(2)
                    })
                    .sum();
                let signal: f64 = middle.iter().map(|&x| f64::from(x).powi(2)).sum();
                10.0 * (signal / error.max(1.0)).log10()
            })
            .fold(f64::MIN, f64::max);
        assert!(snr >= 8.0, "{label} 对齐后信噪比 {snr:.1} dB");
    }

    #[test]
    fn short_input_is_padded_and_bad_rate_rejected() {
        let mut encoder = OpusEncoder::new(24_000, 60).unwrap();
        let mut decoder = OpusDecoder::new(24_000).unwrap();
        let packet = encoder.encode(&[1000; 100]).unwrap();
        assert_eq!(decoder.decode(&packet).unwrap().len(), 1440);
        assert!(OpusEncoder::new(44_100, 60).is_err());
        assert!(OpusDecoder::new(22_050).is_err());
        assert!(decoder.decode(&[0xff, 0xff, 0xff]).is_err() || decoder.decode(&[]).is_err());
    }
}
