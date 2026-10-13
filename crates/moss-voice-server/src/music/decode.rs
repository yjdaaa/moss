//! NV14 bounded local-file decoder; no user music files are accessed by tests.
//!
//! No recording, network, file writing, actual device or audio output. Callers
//! supply an ALREADY OPENED trusted read-only File and a media extension hint.
//! The worker sends at most 20 x 100 ms of 24kHz mono PCM to its receiver.
use crate::audio::Resampler;
use std::{
    fs::File,
    io::ErrorKind,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use symphonia::{
    core::{
        audio::SampleBuffer,
        codecs::{CODEC_TYPE_NULL, DecoderOptions},
        errors::Error as SymphoniaError,
        formats::FormatOptions,
        io::MediaSourceStream,
        meta::MetadataOptions,
        probe::Hint,
    },
    default::{get_codecs, get_probe},
};

const RATE: u32 = 24_000;
const CHUNK_SAMPLES: usize = 2_400; // 100ms; matches 2s <=20 items queue.
const QUEUE_ITEMS: usize = 20;
const MAX_PACKET_FRAMES: usize = 65_536;
const MAX_INPUT_BYTES_PER_PACKET: usize = 512 * 1024;
const MAX_TRACK_SECONDS: u64 = 2 * 3600;
const MAX_CHANNELS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MusicDecodeError {
    InvalidFormat,
    UnsupportedCodec,
    CorruptMedia,
    InvalidAudioMetadata,
    InvalidSamples,
    LimitExceeded,
    Cancelled,
}

#[derive(Debug)]
pub enum MusicDecodeEvent {
    Pcm {
        generation: u64,
        samples: Vec<i16>,
    },
    Done {
        generation: u64,
    },
    Failed {
        generation: u64,
        cause: MusicDecodeError,
    },
}

/// A session-owned streaming worker. Drop cancels promptly but never waits on
/// CPU work on a device async thread. Worker cannot enqueue if the receiver
/// disappears, and full queues are polled for cancellation every 15 ms.
pub struct DecodeJob {
    pub receiver: Receiver<MusicDecodeEvent>,
    alive: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl DecodeJob {
    pub fn cancel(&self) {
        self.alive.store(false, Ordering::Release);
    }
    #[cfg(test)]
    fn join(mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
impl Drop for DecodeJob {
    fn drop(&mut self) {
        self.cancel();
        // Do NOT join here; no blocking work on the session loop.
        let _ = self.worker.take();
    }
}

fn extension_allowed(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "wav" | "mp3" | "flac" | "ogg"
    )
}

pub fn start_decode(
    file: File,
    extension: &str,
    generation: u64,
) -> Result<DecodeJob, MusicDecodeError> {
    if !extension_allowed(extension) {
        return Err(MusicDecodeError::InvalidFormat);
    }
    let alive = Arc::new(AtomicBool::new(true));
    let (tx, receiver) = mpsc::sync_channel(QUEUE_ITEMS);
    let guard = alive.clone();
    let extension = extension.to_owned();
    let worker = thread::Builder::new()
        .name("moss-music-decode".into())
        .spawn(move || {
            let result = decode_worker(file, &extension, generation, &guard, &tx);
            let terminal = match result {
                Ok(()) if guard.load(Ordering::Acquire) => MusicDecodeEvent::Done { generation },
                Err(MusicDecodeError::Cancelled) | Ok(()) => return,
                Err(cause) => MusicDecodeEvent::Failed { generation, cause },
            };
            let _ = send_cancellable(&tx, terminal, &guard);
        })
        .map_err(|_| MusicDecodeError::LimitExceeded)?;
    Ok(DecodeJob {
        receiver,
        alive,
        worker: Some(worker),
    })
}

fn send_cancellable(
    tx: &SyncSender<MusicDecodeEvent>,
    mut item: MusicDecodeEvent,
    alive: &AtomicBool,
) -> Result<(), MusicDecodeError> {
    loop {
        if !alive.load(Ordering::Acquire) {
            return Err(MusicDecodeError::Cancelled);
        }
        match tx.try_send(item) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Disconnected(_)) => return Err(MusicDecodeError::Cancelled),
            Err(TrySendError::Full(returned)) => {
                item = returned;
                thread::sleep(Duration::from_millis(15));
            }
        }
    }
}

fn decode_worker(
    file: File,
    extension: &str,
    generation: u64,
    alive: &AtomicBool,
    tx: &SyncSender<MusicDecodeEvent>,
) -> Result<(), MusicDecodeError> {
    let media = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(extension);
    let mut format = get_probe()
        .format(
            &hint,
            media,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|_| MusicDecodeError::InvalidFormat)?
        .format;
    let track = format
        .default_track()
        .ok_or(MusicDecodeError::UnsupportedCodec)?;
    let id = track.id;
    let params = &track.codec_params;
    if params.codec == CODEC_TYPE_NULL {
        return Err(MusicDecodeError::UnsupportedCodec);
    }
    let input_rate = params
        .sample_rate
        .ok_or(MusicDecodeError::InvalidAudioMetadata)?;
    let declared_channels = params
        .channels
        .ok_or(MusicDecodeError::InvalidAudioMetadata)?
        .count();
    if !(8_000..=192_000).contains(&input_rate) || !(1..=MAX_CHANNELS).contains(&declared_channels)
    {
        return Err(MusicDecodeError::InvalidAudioMetadata);
    }
    let mut decoder = get_codecs()
        .make(params, &DecoderOptions::default())
        .map_err(|_| MusicDecodeError::UnsupportedCodec)?;
    let mut resampler = Resampler::new(input_rate, RATE);
    let mut produced = 0u64;
    let mut consecutive_failures = 0usize;
    loop {
        if !alive.load(Ordering::Acquire) {
            return Err(MusicDecodeError::Cancelled);
        }
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error)) if error.kind() == ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(_) => return Err(MusicDecodeError::CorruptMedia),
        };
        if packet.track_id() != id {
            continue;
        }
        if packet.buf().len() > MAX_INPUT_BYTES_PER_PACKET {
            return Err(MusicDecodeError::LimitExceeded);
        }
        let decoded = match decoder.decode(&packet) {
            Ok(block) => {
                consecutive_failures = 0;
                block
            }
            Err(SymphoniaError::DecodeError(_)) if consecutive_failures < 3 => {
                consecutive_failures += 1;
                continue;
            }
            Err(_) => return Err(MusicDecodeError::CorruptMedia),
        };
        let spec = *decoded.spec();
        let frames = decoded.frames();
        let channels = spec.channels.count();
        if frames == 0 {
            continue;
        }
        if frames > MAX_PACKET_FRAMES
            || spec.rate != input_rate
            || channels != declared_channels
            || channels == 0
        {
            return Err(MusicDecodeError::InvalidAudioMetadata);
        }
        let mut interleaved = SampleBuffer::<f32>::new(frames as u64, spec);
        interleaved.copy_interleaved_ref(decoded);
        let mut mono = Vec::with_capacity(frames);
        for block in interleaved.samples().chunks_exact(channels) {
            let mut value = 0.0f64;
            for &sample in block {
                if !sample.is_finite() {
                    return Err(MusicDecodeError::InvalidSamples);
                }
                value += f64::from(sample);
            }
            value /= channels as f64;
            mono.push((value * 32767.0).round().clamp(-32768.0, 32767.0) as i16);
        }
        let samples = resampler.process(&mono);
        produced = produced.saturating_add(samples.len() as u64);
        if produced > MAX_TRACK_SECONDS * RATE as u64 {
            return Err(MusicDecodeError::LimitExceeded);
        }
        for chunk in samples.chunks(CHUNK_SAMPLES) {
            send_cancellable(
                tx,
                MusicDecodeEvent::Pcm {
                    generation,
                    samples: chunk.to_vec(),
                },
                alive,
            )?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{OpusDecoder, OpusEncoder, PlaybackPump, PumpTick};
    use std::io::Write;

    fn make_wav(stereo: bool, seconds: u32) -> File {
        let rate = 48_000u32;
        let channels = if stereo { 2u16 } else { 1u16 };
        let frames = rate * seconds;
        let data_bytes = frames * channels as u32 * 2;
        let mut file = tempfile::tempfile().unwrap();
        let mut head = Vec::new();
        head.extend_from_slice(b"RIFF");
        head.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        head.extend_from_slice(b"WAVEfmt ");
        head.extend_from_slice(&16u32.to_le_bytes());
        head.extend_from_slice(&1u16.to_le_bytes());
        head.extend_from_slice(&channels.to_le_bytes());
        head.extend_from_slice(&rate.to_le_bytes());
        head.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        head.extend_from_slice(&(channels * 2).to_le_bytes());
        head.extend_from_slice(&16u16.to_le_bytes());
        head.extend_from_slice(b"data");
        head.extend_from_slice(&data_bytes.to_le_bytes());
        file.write_all(&head).unwrap();
        for frame in 0..frames {
            let sample = (f64::from(frame) * 440.0 * std::f64::consts::TAU / f64::from(rate)).sin();
            let value = (sample * 10000.0).round() as i16;
            file.write_all(&value.to_le_bytes()).unwrap();
            if stereo {
                file.write_all(&(-value).to_le_bytes()).unwrap();
            }
        }
        use std::io::Seek;
        file.rewind().unwrap();
        file
    }
    #[test]
    fn synthetic_wav_streams_mono_to_24khz_then_existing_opus_pump() {
        let file = make_wav(false, 1);
        let job = start_decode(file, "wav", 42).unwrap();
        let mut pcm = Vec::new();
        loop {
            match job.receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
                MusicDecodeEvent::Pcm {
                    generation,
                    samples,
                } => {
                    assert_eq!(generation, 42);
                    assert!(!samples.is_empty() && samples.len() <= CHUNK_SAMPLES);
                    pcm.extend(samples);
                }
                MusicDecodeEvent::Done { generation } => {
                    assert_eq!(generation, 42);
                    break;
                }
                MusicDecodeEvent::Failed { cause, .. } => {
                    panic!("decoder unexpectedly failed: {cause:?}")
                }
            }
        }
        assert!(
            (pcm.len() as i64 - 24_000).abs() < 10,
            "length={}",
            pcm.len()
        );
        let mut pump = PlaybackPump::new(OpusEncoder::new(RATE, 60).unwrap(), RATE, 0);
        pump.begin();
        pump.push(&pcm);
        pump.finish();
        let mut decoder = OpusDecoder::new(RATE).unwrap();
        let mut frames = 0;
        let mut output = Vec::new();
        loop {
            match pump.next_frame().unwrap() {
                PumpTick::Frame { data, .. } => {
                    frames += 1;
                    output.extend(decoder.decode(&data).unwrap());
                }
                PumpTick::Done => break,
                PumpTick::Waiting => panic!("finished PCM should not wait"),
            }
        }
        assert_eq!(frames, 17);
        assert_eq!(output.len(), 17 * 1440);
        job.join();
    }
    #[test]
    fn synthetic_stereo_wav_downmix_and_continuous_resampling() {
        let job = start_decode(make_wav(true, 1), "wav", 123).unwrap();
        let mut samples = Vec::new();
        loop {
            match job.receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
                MusicDecodeEvent::Pcm { samples: chunk, .. } => samples.extend(chunk),
                MusicDecodeEvent::Done { generation } => {
                    assert_eq!(generation, 123);
                    break;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!((samples.len() as i64 - 24_000).abs() < 10);
        let max = samples.iter().map(|x| x.abs()).max().unwrap_or(0);
        assert!(max <= 1, "stereo opposite-phase should cancel: {max}");
        job.join();
    }
    /// Fixtures are 1s/440Hz synthetic tones generated with Ubuntu-signed
    /// FFmpeg in a disposable Docker container. No personal audio is used.
    fn decode_synthetic_fixture(
        bytes: &[u8],
        extension: &str,
    ) -> Result<Vec<i16>, MusicDecodeError> {
        use std::io::Seek;
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.rewind().unwrap();
        let job = start_decode(file, extension, 4242).unwrap();
        let mut audio = Vec::new();
        let answer = loop {
            match job.receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
                MusicDecodeEvent::Pcm {
                    generation,
                    samples,
                } => {
                    assert_eq!(generation, 4242);
                    assert!(!samples.is_empty() && samples.len() <= CHUNK_SAMPLES);
                    audio.extend(samples);
                }
                MusicDecodeEvent::Done { generation } => {
                    assert_eq!(generation, 4242);
                    break Ok(audio);
                }
                MusicDecodeEvent::Failed { generation, cause } => {
                    assert_eq!(generation, 4242);
                    break Err(cause);
                }
            }
        };
        job.join();
        answer
    }

    #[test]
    fn nv14_valid_synthetic_mp3_flac_ogg_vorbis_reach_24khz_mono_pcm() {
        let cases: [(&str, &[u8]); 3] = [
            (
                "mp3",
                include_bytes!("../../tests/fixtures/sine-48k-stereo.mp3"),
            ),
            (
                "flac",
                include_bytes!("../../tests/fixtures/sine-48k-stereo.flac"),
            ),
            (
                "ogg",
                include_bytes!("../../tests/fixtures/sine-48k-stereo.ogg"),
            ),
        ];
        for (extension, bytes) in cases {
            let samples = decode_synthetic_fixture(bytes, extension)
                .unwrap_or_else(|error| panic!("{extension} could not decode: {error:?}"));
            assert!(
                (20_000..=27_000).contains(&samples.len()),
                "decoded {extension}: {} samples, expected about 24k",
                samples.len()
            );
            assert!(
                samples.iter().any(|&sample| sample.abs() > 100),
                "decoded {extension} unexpectedly silent"
            );
        }
    }

    #[test]
    fn nv14_ogg_opus_is_not_claimed_to_be_supported_vorbis() {
        let ogg_opus = include_bytes!("../../tests/fixtures/sine-48k-stereo-opus.ogg");
        assert!(ogg_opus.starts_with(b"OggS"));
        let unsupported = decode_synthetic_fixture(ogg_opus, "ogg");
        assert!(
            matches!(
                unsupported,
                Err(MusicDecodeError::UnsupportedCodec | MusicDecodeError::InvalidFormat)
            ),
            "Ogg Opus must be reported unsupported, not silently decoded"
        );
    }

    #[test]
    fn nv14_truncated_synthetic_flac_does_not_emit_success() {
        let flac = include_bytes!("../../tests/fixtures/sine-48k-stereo.flac");
        let truncated = &flac[..flac.len().min(24)];
        assert!(decode_synthetic_fixture(truncated, "flac").is_err());
    }

    #[test]
    fn untrusted_extension_corrupt_media_and_cancel_full_queue() {
        assert!(matches!(
            start_decode(make_wav(false, 1), "exe", 1),
            Err(MusicDecodeError::InvalidFormat)
        ));
        let mut bad = tempfile::tempfile().unwrap();
        bad.write_all(b"not an audio file").unwrap();
        use std::io::Seek;
        bad.rewind().unwrap();
        let job = start_decode(bad, "ogg", 2).unwrap();
        assert!(matches!(
            job.receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            MusicDecodeEvent::Failed {
                generation: 2,
                cause: MusicDecodeError::InvalidFormat
            }
        ));
        job.join();

        let job = start_decode(make_wav(false, 5), "wav", 3).unwrap();
        // Do not drain: the 20-item queue must be bounded and cancellation
        // must wake the full-queue producer without deadlock.
        thread::sleep(Duration::from_millis(50));
        assert!(
            job.receiver
                .try_iter()
                .take(QUEUE_ITEMS)
                .all(|event| matches!(event, MusicDecodeEvent::Pcm { generation: 3, .. }))
        );
        job.cancel();
        job.join();
    }
}
