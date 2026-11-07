//! NV8b：独立的保守插话判停。这里只检测已在设备端消除回声的音频，不冒充 AEC。
use crate::audio::{EnergyVad, Vad, VadConfig, VadEvent};
use std::collections::VecDeque;

pub(crate) struct BargeIn {
    vad: EnergyVad,
    preroll: VecDeque<i16>,
    max_samples: usize,
}

impl BargeIn {
    pub fn new(rate: u32, speech_ms: u32, base: &VadConfig) -> Self {
        let config = VadConfig {
            min_speech_ms: speech_ms,
            start_rms: (base.start_rms * 1.5).max(900.0),
            start_over_noise: base.start_over_noise.max(6.0),
            ..base.clone()
        };
        Self {
            vad: EnergyVad::new(rate, config),
            preroll: VecDeque::new(),
            max_samples: rate as usize,
        }
    }

    pub fn reset(&mut self) {
        self.vad.reset();
        self.preroll.clear();
    }

    /// guarded 用于开播保护；保护期音频不累积为后续插话。
    pub fn accept(&mut self, pcm: &[i16], guarded: bool) -> Option<Vec<i16>> {
        if guarded {
            self.reset();
            return None;
        }
        self.preroll.extend(pcm.iter().copied());
        let excess = self.preroll.len().saturating_sub(self.max_samples);
        self.preroll.drain(..excess);
        if self.vad.accept(pcm).contains(&VadEvent::SpeechStart) {
            let head = self.preroll.drain(..).collect();
            self.vad.reset();
            Some(head)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::test_signal::{silence, tone};

    #[test]
    fn continuous_speech_not_bursts_or_guarded_audio_triggers() {
        for duration in [300, 400, 600] {
            let mut detector = BargeIn::new(16_000, duration, &VadConfig::default());
            assert!(
                detector
                    .accept(&tone(16_000, 300.0, 600, 8000.0), true)
                    .is_none()
            );
            assert!(
                detector
                    .accept(&tone(16_000, 300.0, duration - 20, 8000.0), false)
                    .is_none()
            );
            assert!(detector.accept(&silence(16_000, 40), false).is_none());
            assert!(
                detector
                    .accept(&tone(16_000, 300.0, duration - 20, 8000.0), false)
                    .is_none()
            );
            let head = detector
                .accept(&tone(16_000, 300.0, 20, 8000.0), false)
                .unwrap();
            assert!(head.len() >= (16 * duration) as usize && head.len() <= 16_000);
        }
    }

    #[test]
    fn quiet_noise_is_bounded_and_does_not_trigger() {
        let mut detector = BargeIn::new(16_000, 400, &VadConfig::default());
        for _ in 0..100 {
            assert!(
                detector
                    .accept(&tone(16_000, 300.0, 60, 200.0), false)
                    .is_none()
            );
            assert!(detector.preroll.len() <= 16_000);
        }
    }
}
