//! NV14 generation-scoped music source: session-side PCM only, one shared Opus pump.
//! Wired to the opt-in live voice session only when the trusted desktop music
//! index is enabled; all tests use synthetic files and fake devices.
use super::decode::{DecodeJob, MusicDecodeError, MusicDecodeEvent, start_decode};
use crate::audio::PlaybackPump;
use std::{
    fs::File,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::TryRecvError,
    },
};
const MAX_PUMP_SAMPLES: usize = 24_000;
const MAX_DRAIN_PER_TICK: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MusicProgress {
    Playing,
    Draining,
    Finished,
    Cancelled,
    Failed(MusicDecodeError),
}
pub(crate) struct MusicController {
    allowed: Arc<AtomicBool>,
    generation: u64,
    active: Option<DecodeJob>,
    draining: bool,
}
impl MusicController {
    pub(crate) fn new(allowed: Arc<AtomicBool>) -> Self {
        Self {
            allowed,
            generation: 0,
            active: None,
            draining: false,
        }
    }
    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn active(&self) -> bool {
        self.active.is_some()
    }
    /// Trusted caller must first validate track ID and OPEN a readonly File.
    /// Invalid/failed start does not play the former song.
    pub(crate) fn start(
        &mut self,
        file: File,
        extension: &str,
        pump: &mut PlaybackPump,
    ) -> Result<u64, MusicDecodeError> {
        // Stop old audio first even when permission has just been revoked.
        self.cancel(pump);
        if !self.allowed.load(Ordering::Acquire) {
            return Err(MusicDecodeError::Cancelled);
        }
        self.generation = self.generation.wrapping_add(1).max(1);
        let job = start_decode(file, extension, self.generation)?;
        pump.begin();
        self.active = Some(job);
        self.draining = false;
        Ok(self.generation)
    }
    /// Stop must be called for abort, new voice turn, revoke or disconnect.
    pub(crate) fn cancel(&mut self, pump: &mut PlaybackPump) {
        self.generation = self.generation.wrapping_add(1).max(1);
        if let Some(job) = self.active.take() {
            job.cancel();
        }
        self.draining = false;
        pump.clear();
    }
    /// Nonblocking: stale-generation events are discarded and the pump is capped.
    pub(crate) fn refill(&mut self, pump: &mut PlaybackPump) -> MusicProgress {
        if !self.allowed.load(Ordering::Acquire) {
            self.cancel(pump);
            return MusicProgress::Cancelled;
        }
        if self.active.is_none() {
            return MusicProgress::Finished;
        }
        for _ in 0..MAX_DRAIN_PER_TICK {
            if pump.buffered_samples() > MAX_PUMP_SAMPLES - 2400 {
                break;
            }
            let event = self.active.as_ref().unwrap().receiver.try_recv();
            match event {
                Ok(MusicDecodeEvent::Pcm {
                    generation,
                    samples,
                }) => {
                    if generation != self.generation {
                        continue;
                    }
                    if samples.len() > 2400 {
                        self.cancel(pump);
                        return MusicProgress::Failed(MusicDecodeError::LimitExceeded);
                    }
                    pump.push(&samples);
                }
                Ok(MusicDecodeEvent::Done { generation }) if generation == self.generation => {
                    self.draining = true;
                    pump.finish();
                    break;
                }
                Ok(MusicDecodeEvent::Failed { generation, cause })
                    if generation == self.generation =>
                {
                    self.cancel(pump);
                    return MusicProgress::Failed(cause);
                }
                Ok(_) => continue,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.draining {
                        self.cancel(pump);
                        return MusicProgress::Failed(MusicDecodeError::CorruptMedia);
                    }
                    break;
                }
            }
        }
        if self.draining {
            MusicProgress::Draining
        } else {
            MusicProgress::Playing
        }
    }
    pub(crate) fn finish_after_pump_done(&mut self) -> MusicProgress {
        if self.draining {
            self.active.take();
            self.draining = false;
            MusicProgress::Finished
        } else {
            MusicProgress::Playing
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{OpusEncoder, PumpTick};
    use std::{
        io::{Seek, Write},
        thread,
        time::{Duration, Instant},
    };
    fn fake_wav() -> File {
        let mut file = tempfile::tempfile().unwrap();
        let rate = 24_000u32;
        let frames = rate / 2;
        let mut header = Vec::new();
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&(36 + frames * 2).to_le_bytes());
        header.extend_from_slice(b"WAVEfmt ");
        header.extend_from_slice(&16u32.to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&rate.to_le_bytes());
        header.extend_from_slice(&(rate * 2).to_le_bytes());
        header.extend_from_slice(&2u16.to_le_bytes());
        header.extend_from_slice(&16u16.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&(frames * 2).to_le_bytes());
        file.write_all(&header).unwrap();
        for i in 0..frames {
            let val = if i % 2 == 0 { 5000i16 } else { -5000i16 };
            file.write_all(&val.to_le_bytes()).unwrap();
        }
        file.rewind().unwrap();
        file
    }
    fn pump() -> PlaybackPump {
        PlaybackPump::new(OpusEncoder::new(24_000, 60).unwrap(), 24_000, 0)
    }
    fn finish(ctrl: &mut MusicController, pump: &mut PlaybackPump) -> usize {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut frames = 0;
        loop {
            assert!(Instant::now() < deadline, "missing decode EOS");
            match ctrl.refill(pump) {
                MusicProgress::Failed(error) => panic!("decoder failure: {error:?}"),
                MusicProgress::Cancelled => panic!("unexpected cancellation"),
                _ => {}
            }
            match pump.next_frame().unwrap() {
                PumpTick::Frame { .. } => frames += 1,
                PumpTick::Waiting => thread::sleep(Duration::from_millis(3)),
                PumpTick::Done => {
                    assert_eq!(ctrl.finish_after_pump_done(), MusicProgress::Finished);
                    return frames;
                }
            }
        }
    }
    #[test]
    fn two_fake_devices_play_separately_and_complete_eos() {
        let mut a = MusicController::new(Arc::new(AtomicBool::new(true)));
        let mut b = MusicController::new(Arc::new(AtomicBool::new(true)));
        let mut p_a = pump();
        let mut p_b = pump();
        assert_eq!(a.start(fake_wav(), "wav", &mut p_a).unwrap(), 1 + 1);
        assert_eq!(b.start(fake_wav(), "wav", &mut p_b).unwrap(), 1 + 1);
        assert_eq!(finish(&mut a, &mut p_a), 9);
        assert!(!a.active() && b.active());
        assert_eq!(finish(&mut b, &mut p_b), 9);
    }
    #[test]
    fn replace_abort_revoke_discard_old_music_and_tts_frames() {
        let allowed = Arc::new(AtomicBool::new(true));
        let mut ctrl = MusicController::new(allowed.clone());
        let mut p = pump();
        p.begin();
        p.push(&vec![1234; 24000]);
        let first = ctrl.start(fake_wav(), "wav", &mut p).unwrap();
        assert_eq!(p.buffered_samples(), 0, "old TTS removed");
        let second = ctrl.start(fake_wav(), "wav", &mut p).unwrap();
        assert_ne!(first, second);
        assert_eq!(finish(&mut ctrl, &mut p), 9);
        let third = ctrl.start(fake_wav(), "wav", &mut p).unwrap();
        ctrl.cancel(&mut p);
        assert_ne!(third, ctrl.generation());
        assert!(!ctrl.active() && p.buffered_samples() == 0);
        assert_eq!(p.next_frame().unwrap(), PumpTick::Done);
        ctrl.start(fake_wav(), "wav", &mut p).unwrap();
        allowed.store(false, Ordering::Release);
        assert_eq!(ctrl.refill(&mut p), MusicProgress::Cancelled);
        assert!(!ctrl.active() && p.buffered_samples() == 0);
        assert_eq!(p.next_frame().unwrap(), PumpTick::Done);
        assert!(matches!(
            ctrl.start(fake_wav(), "wav", &mut p),
            Err(MusicDecodeError::Cancelled)
        ));
    }
}
