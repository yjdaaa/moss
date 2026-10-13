//! 音频处理（NV2）：Opus 编解码、重采样、语音活动检测、播放节奏泵。全部是同步的纯计算，由会话调用。

pub mod codec;
pub mod pump;
pub mod resample;
pub mod vad;
pub(crate) mod worker;

pub use codec::{OpusDecoder, OpusEncoder, frame_samples, supported_rate};
pub use pump::{PlaybackPump, PumpStats, PumpTick};
pub use resample::Resampler;
pub use vad::{EnergyVad, LocalVad, LocalVadFactory, Vad, VadConfig, VadEvent, VadFactory};

/// 测试用的合成信号（音调、静音、确定性的伪随机噪声）。
#[cfg(test)]
pub(crate) mod test_signal {
    pub fn tone(rate: u32, hz: f64, ms: u32, amplitude: f64) -> Vec<i16> {
        let count = rate as usize * ms as usize / 1000;
        (0..count).map(|i| ((i as f64 / rate as f64 * hz * std::f64::consts::TAU).sin() * amplitude) as i16).collect()
    }

    pub fn silence(rate: u32, ms: u32) -> Vec<i16> {
        vec![0; rate as usize * ms as usize / 1000]
    }

    /// 均匀分布噪声，RMS 约为 `level`。
    pub fn noise(rate: u32, ms: u32, level: f64, seed: u64) -> Vec<i16> {
        let count = rate as usize * ms as usize / 1000;
        let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let peak = level * 3f64.sqrt();
        (0..count)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let unit = (state >> 11) as f64 / (1u64 << 53) as f64;
                ((unit * 2.0 - 1.0) * peak) as i16
            })
            .collect()
    }

    pub fn rms(samples: &[i16]) -> f64 {
        (samples.iter().map(|&value| (value as f64) * (value as f64)).sum::<f64>() / samples.len().max(1) as f64).sqrt()
    }
}
