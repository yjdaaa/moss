//! NV14 offline music core: opt-in read-only index, bounded Symphonia decoder,
//! and generation-scoped PCM playback controller. Only exposed when user explicitly
//! enables the local music folder in the desktop main-window settings.
mod decode;
pub use decode::{DecodeJob, MusicDecodeError, MusicDecodeEvent, start_decode};
mod index;
mod tools;
pub use tools::MusicTools;
pub(crate) mod playback;
pub use index::{MusicChoice, MusicError, MusicIndex, TrackCandidate};
// This synthetic test exercises index -> checked path -> decoded PCM -> Opus
// without any real device, microphone, model, or personal music file.
// Fake Xiaozhi WebSocket session playback/abort/revoke tests live in session tests.
#[cfg(test)]
mod offline_integration {
    use super::playback::{MusicController, MusicProgress};
    use super::*;
    use crate::audio::{OpusEncoder, PlaybackPump, PumpTick};
    use std::{
        fs::{self, File},
        io::Write,
        sync::{Arc, atomic::AtomicBool},
        thread,
        time::{Duration, Instant},
    };
    #[test]
    fn selected_synthetic_track_runs_through_one_device_pump() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("合成 音乐.wav");
        let mut file = File::create(&file_path).unwrap();
        let rate = 24_000u32;
        let frames = rate / 5; // 200ms
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
        for _ in 0..frames {
            file.write_all(&2000i16.to_le_bytes()).unwrap();
        }
        drop(file);

        let indexed = MusicIndex::scan(dir.path()).unwrap();
        let MusicChoice::Selected(candidate) = indexed.choose(Some("合成 音乐"), false).unwrap()
        else {
            panic!()
        };
        assert_eq!(candidate.title, "合成 音乐");
        assert!(!candidate.track_id.contains("\\") && !candidate.track_id.contains("/"));
        let canonical = indexed.resolve(&candidate.track_id).unwrap();
        let file = File::open(canonical).unwrap();
        let allowed = Arc::new(AtomicBool::new(true));
        let mut ctrl = MusicController::new(allowed);
        let mut pump = PlaybackPump::new(OpusEncoder::new(rate, 60).unwrap(), rate, 0);
        let generation = ctrl.start(file, "wav", &mut pump).unwrap();
        assert_ne!(generation, 0);
        let mut frames = 0;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            assert!(Instant::now() < deadline);
            assert!(!matches!(ctrl.refill(&mut pump), MusicProgress::Failed(_)));
            match pump.next_frame().unwrap() {
                PumpTick::Frame { .. } => frames += 1,
                PumpTick::Waiting => thread::sleep(Duration::from_millis(3)),
                PumpTick::Done => break,
            }
        }
        assert_eq!(frames, 4, "200ms encoded as 4 x 60ms Opus frames");
        assert_eq!(ctrl.finish_after_pump_done(), MusicProgress::Finished);
        fs::remove_file(&file_path).unwrap();
        assert_eq!(
            indexed.resolve(&candidate.track_id),
            Err(MusicError::Unavailable)
        );
    }
}
