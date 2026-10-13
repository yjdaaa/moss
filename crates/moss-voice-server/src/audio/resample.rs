//! 线性插值重采样：实现在 `moss-voice-providers`（豆包流式识别也要用），这里重新导出并保留测试。

pub use moss_voice_providers::resample::Resampler;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::test_signal::{rms, tone};

    #[test]
    fn upsampling_keeps_duration_and_level() {
        let input = tone(16_000, 300.0, 1000, 8000.0);
        let mut resampler = Resampler::new(16_000, 24_000);
        let output = resampler.process(&input);
        assert!((output.len() as i64 - 24_000).abs() <= 2, "{}", output.len());
        let ratio = rms(&output) / rms(&input);
        assert!((0.95..1.05).contains(&ratio), "{ratio}");
    }

    #[test]
    fn chunked_equals_whole() {
        let input = tone(16_000, 300.0, 300, 8000.0);
        let whole = Resampler::new(16_000, 24_000).process(&input);
        let mut resampler = Resampler::new(16_000, 24_000);
        let mut chunked = Vec::new();
        for chunk in input.chunks(333) {
            chunked.extend(resampler.process(chunk));
        }
        assert!((whole.len() as i64 - chunked.len() as i64).abs() <= 1);
        let common = whole.len().min(chunked.len());
        let max_diff = whole[..common].iter().zip(&chunked[..common]).map(|(a, b)| (*a as i32 - *b as i32).abs()).max();
        assert!(max_diff.unwrap_or(0) <= 1, "分块与整体结果一致：{max_diff:?}");
    }

    #[test]
    fn same_rate_is_passthrough() {
        let input = vec![1, 2, 3];
        assert_eq!(Resampler::new(24_000, 24_000).process(&input), input);
    }
}
