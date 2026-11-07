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

/// 计算一帧 16-bit PCM 的归一化 RMS 振幅（0..=100），仅用于界面声波与机械眼状态同步，不含音频内容。
pub fn pcm_rms_level(samples: &[i16]) -> u8 {
    if samples.is_empty() {
        return 0;
    }
    let sum_sq: f64 = samples
        .iter()
        .map(|&sample| {
            let value = f64::from(sample);
            value * value
        })
        .sum();
    let rms = (sum_sq / (samples.len() as f64)).sqrt();
    if rms < 80.0 {
        return 0;
    }
    ((rms / 8000.0).sqrt() * 100.0).round().clamp(0.0, 100.0) as u8
}

/// 分句 TTS 拼接首尾线性淡入淡出时长（毫秒），消除分句直流跳变爆音（Click/Pop）。
pub const BOUNDARY_FADE_MS: u32 = 12;
/// 触发句尾冗余静音裁剪的阈值（毫秒）：句尾连续静音超过此值时裁剪至 `TRAILING_SILENCE_KEEP_MS`。
pub const TRAILING_SILENCE_TRIGGER_MS: u32 = 120;
/// 句尾裁剪后保留的自然气口静音时长（毫秒）。
pub const TRAILING_SILENCE_KEEP_MS: u32 = 60;
const SILENCE_SAMPLE_THRESHOLD: i16 = 24;

/// 对 PCM 头部 `fade_ms` 毫秒做线性淡入（0 -> 1），消除句首突变爆音。
pub fn fade_head_i16(samples: &mut [i16], sample_rate: u32, fade_ms: u32) {
    if samples.is_empty() || sample_rate == 0 || fade_ms == 0 {
        return;
    }
    let fade_len = ((u64::from(sample_rate) * u64::from(fade_ms)) / 1000) as usize;
    let n = fade_len.min(samples.len());
    if n <= 1 {
        return;
    }
    let denom = n as i64;
    for (i, sample) in samples[..n].iter_mut().enumerate() {
        *sample = ((i64::from(*sample) * (i as i64)) / denom) as i16;
    }
}

/// 对 PCM 尾部 `fade_ms` 毫秒做线性淡出（1 -> 0），消除句尾截断爆音。
pub fn fade_tail_i16(samples: &mut [i16], sample_rate: u32, fade_ms: u32) {
    if samples.is_empty() || sample_rate == 0 || fade_ms == 0 {
        return;
    }
    let fade_len = ((u64::from(sample_rate) * u64::from(fade_ms)) / 1000) as usize;
    let n = fade_len.min(samples.len());
    if n <= 1 {
        return;
    }
    let start = samples.len() - n;
    let denom = n as i64;
    for (i, sample) in samples[start..].iter_mut().enumerate() {
        let remain = (n - 1 - i) as i64;
        *sample = ((i64::from(*sample) * remain) / denom) as i16;
    }
}

/// 裁剪句尾超过 `trigger_ms` 的冗余静音，保留 `keep_ms` 自然停顿气口（全静音切片保持原样）。
pub fn trim_trailing_silence_i16(
    samples: &mut Vec<i16>,
    sample_rate: u32,
    keep_ms: u32,
    trigger_ms: u32,
) {
    if samples.is_empty() || sample_rate == 0 {
        return;
    }
    let Some(last_active) = samples
        .iter()
        .rposition(|&s| s.saturating_abs() > SILENCE_SAMPLE_THRESHOLD)
    else {
        return;
    };
    let trailing_silent = samples.len() - (last_active + 1);
    let trigger_samples = ((u64::from(sample_rate) * u64::from(trigger_ms)) / 1000) as usize;
    let keep_samples = ((u64::from(sample_rate) * u64::from(keep_ms)) / 1000) as usize;
    if trailing_silent > trigger_samples && keep_samples < trailing_silent {
        samples.truncate(last_active + 1 + keep_samples);
    }
}
/// 测试用的合成信号（音调、静音、确定性的伪随机噪声）。
#[cfg(test)]
pub(crate) mod test_signal {
    pub fn tone(rate: u32, hz: f64, ms: u32, amplitude: f64) -> Vec<i16> {
        let count = rate as usize * ms as usize / 1000;
        (0..count)
            .map(|i| {
                ((i as f64 / rate as f64 * hz * std::f64::consts::TAU).sin() * amplitude) as i16
            })
            .collect()
    }

    pub fn silence(rate: u32, ms: u32) -> Vec<i16> {
        vec![0; rate as usize * ms as usize / 1000]
    }

    /// 均匀分布噪声，RMS 约为 `level`。
    pub fn noise(rate: u32, ms: u32, level: f64, seed: u64) -> Vec<i16> {
        let count = rate as usize * ms as usize / 1000;
        let mut state = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let peak = level * 3f64.sqrt();
        (0..count)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let unit = (state >> 11) as f64 / (1u64 << 53) as f64;
                ((unit * 2.0 - 1.0) * peak) as i16
            })
            .collect()
    }

    pub fn rms(samples: &[i16]) -> f64 {
        (samples
            .iter()
            .map(|&value| (value as f64) * (value as f64))
            .sum::<f64>()
            / samples.len().max(1) as f64)
            .sqrt()
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::test_signal::{silence, tone};
    use super::*;

    #[test]
    fn boundary_fade_and_trailing_silence_trim_smooth_edges_without_touching_active_tones() {
        let rate = 16_000;
        // 1. 纯正弦音调无尾部静音，长度保持不变，首尾采样点平滑归零
        let mut active = tone(rate, 440.0, 300, 6000.0);
        active[0] = 5000;
        *active.last_mut().unwrap() = -5000;
        let orig_len = active.len();
        trim_trailing_silence_i16(
            &mut active,
            rate,
            TRAILING_SILENCE_KEEP_MS,
            TRAILING_SILENCE_TRIGGER_MS,
        );
        assert_eq!(active.len(), orig_len, "无尾部冗余静音时不改变样本数");
        fade_head_i16(&mut active, rate, BOUNDARY_FADE_MS);
        fade_tail_i16(&mut active, rate, BOUNDARY_FADE_MS);
        assert_eq!(active[0], 0, "句首第一个样本平滑归零");
        assert_eq!(*active.last().unwrap(), 0, "句尾最后一个样本平滑归零");

        // 2. 句尾带 300ms 冗余静音时，自动裁剪至 60ms 自然气口
        let mut padded = tone(rate, 440.0, 200, 6000.0);
        padded.extend(silence(rate, 300));
        trim_trailing_silence_i16(
            &mut padded,
            rate,
            TRAILING_SILENCE_KEEP_MS,
            TRAILING_SILENCE_TRIGGER_MS,
        );
        // 200ms 语音 + 60ms 保留气口 ≈ 260ms (4160 样本)
        let expected_max = (rate as usize * 265) / 1000;
        let expected_min = (rate as usize * 250) / 1000;
        assert!(
            (expected_min..=expected_max).contains(&padded.len()),
            "裁剪后长度应约为 260ms，实际 {} 样本",
            padded.len()
        );
    }
}
