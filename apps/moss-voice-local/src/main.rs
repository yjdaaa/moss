// SPDX-License-Identifier: GPL-3.0-or-later
//! MOSS 本地模型辅助程序 `moss-voice-local`（内置语音技术书 §11.2，NV6；可选模型见 §13.7）。
//!
//! 由 MOSS 启动，经标准输入输出按 `moss-voice-local-proto` 的帧协议通信；标准输入关闭即退出。
//! 判停（Silero / TEN-VAD）、识别（SenseVoice / 流式 Zipformer）、合成（Kokoro / Matcha）各在自己的线程里
//! 加载与运行，一个较慢的合成不会拖住判停。模型对象在各自线程内创建，不跨线程共享。
//!
//! 本程序静态链接 sherpa-onnx（Apache-2.0），其中包含 espeak-ng（GPL-3.0），因此以 GPL-3.0-or-later 发布。

use moss_voice_local_proto::{
    SPEAKER_DIMENSION, SPEAKER_MODEL_BYTES, SPEAKER_MODEL_FILE, SPEAKER_MODEL_ID,
    SPEAKER_MODEL_SHA256, SPEAKER_SAMPLE_RATE, embedding_to_bytes, valid_speaker_pcm,
};
use sha2::{Digest, Sha256};
use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;

use moss_voice_local_proto::{
    AsrEngine, Op, PROTOCOL_VERSION, Reply, Request, Response, TtsEngine, VadEngine, VadEventKind,
    VadParams, bytes_to_pcm, pcm_to_bytes, read_frame, write_frame,
};
use sherpa_onnx::{
    GenerationConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig,
    OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig, OfflineTtsMatchaModelConfig,
    OfflineTtsModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineStream,
    OnlineTransducerModelConfig, SileroVadModelConfig, TenVadModelConfig, VadModelConfig,
    VoiceActivityDetector,
};

type Out = Arc<Mutex<BufWriter<io::Stdout>>>;
type Job = (u64, Op, Vec<u8>);

/// 流式识别说完后补的静音：把最后几个字从模型的右侧上下文里“推”出来（实测 0.66 秒会丢末字，1.0 秒不丢，只多约 30 ms）
const STREAM_TAIL_SECONDS: f32 = 1.0;

fn send(out: &Out, id: u64, reply: Reply, payload: &[u8]) {
    if let Ok(mut writer) = out.lock()
        && write_frame(&mut *writer, &Response { id, reply }, payload).is_err()
    {
        // MOSS 已退出：结束进程
        std::process::exit(0);
    }
}

fn error(out: &Out, id: u64, message: impl Into<String>) {
    send(
        out,
        id,
        Reply::Error {
            message: message.into(),
        },
        &[],
    );
}

fn to_f32(pcm: &[i16]) -> Vec<f32> {
    pcm.iter().map(|&sample| sample as f32 / 32768.0).collect()
}

/// 浮点波形转 16-bit PCM（带 C1 连续平滑拐点限幅，避免神经声码器重音超调 >1.0 时硬削波产生高音量破音）。
fn to_i16(samples: &[f32]) -> Vec<i16> {
    const KNEE: f32 = 0.62;
    const HEADROOM: f32 = 0.22;
    samples
        .iter()
        .map(|&sample| {
            if !sample.is_finite() {
                return 0;
            }
            let mag = sample.abs();
            let shaped = if mag <= KNEE {
                mag
            } else {
                let excess = mag - KNEE;
                KNEE + (HEADROOM * excess) / (excess + HEADROOM)
            };
            let signed = sample.signum() * shaped;
            (signed * 32767.0).round().clamp(-32768.0, 32767.0) as i16
        })
        .collect()
}

/// 平滑本地流式子分段拼接边缘：非末段裁除模型尾部超长死寂（>200ms 裁至 80ms 自然换气）并做 6ms 淡出，
/// 非首段做 6ms 淡入，消除同一句内多个 `AudioChunk` 独立合成时的直流跳变咔哒声。
fn smooth_stream_subsegment(
    samples: &mut Vec<i16>,
    sample_rate: u32,
    is_first: bool,
    is_last: bool,
) {
    if samples.is_empty() || sample_rate == 0 {
        return;
    }
    const SILENCE_THRESHOLD: i16 = 24;
    const SUBSEG_FADE_MS: u64 = 6;
    const SUBSEG_SILENCE_TRIGGER_MS: u64 = 200;
    const SUBSEG_SILENCE_KEEP_MS: u64 = 80;
    if !is_last
        && let Some(last_active) = samples
            .iter()
            .rposition(|&s| s.saturating_abs() > SILENCE_THRESHOLD)
    {
        let trailing_silent = samples.len() - (last_active + 1);
        let trigger = ((u64::from(sample_rate) * SUBSEG_SILENCE_TRIGGER_MS) / 1000) as usize;
        let keep = ((u64::from(sample_rate) * SUBSEG_SILENCE_KEEP_MS) / 1000) as usize;
        if trailing_silent > trigger && keep < trailing_silent {
            samples.truncate(last_active + 1 + keep);
        }
    }
    let fade_len = ((u64::from(sample_rate) * SUBSEG_FADE_MS) / 1000) as usize;
    let n = fade_len.min(samples.len());
    if n <= 1 {
        return;
    }
    let denom = n as i64;
    if !is_first {
        for (i, sample) in samples[..n].iter_mut().enumerate() {
            *sample = ((i64::from(*sample) * (i as i64)) / denom) as i16;
        }
    }
    if !is_last {
        let start = samples.len() - n;
        for (i, sample) in samples[start..].iter_mut().enumerate() {
            let remain = (n - 1 - i) as i64;
            *sample = ((i64::from(*sample) * remain) / denom) as i16;
        }
    }
}

/// Windows 上 `std::fs::canonicalize` 会返回 `\\?\C:\...` 或 `\\?\UNC\...` 逐字路径前缀。
/// `sherpa-onnx` C++ 层（如 Kokoro / Matcha 拼接 `espeak-ng-data/phontab`、`dict/hmm_model.utf8`）在内部硬编码正斜杠 `/`；
/// 一旦带 `\\?\` 前缀，Windows 内核不再把 `/` 归一化为 `\`，导致文件打开失败。交给 C++ 配置前剥离常规盘符与 UNC 逐字前缀。
#[cfg(any(windows, test))]
fn strip_windows_verbatim_prefix(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        if !rest.is_empty() && !rest.contains('/') {
            return format!(r"\\{rest}");
        }
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\'
            && !rest.contains('/')
        {
            return rest.to_owned();
        }
    }
    raw.to_owned()
}

fn native_path_string(path: &Path) -> String {
    let raw = path.to_string_lossy();
    #[cfg(windows)]
    {
        strip_windows_verbatim_prefix(&raw)
    }
    #[cfg(not(windows))]
    {
        raw.into_owned()
    }
}

fn require(path: &Path) -> Result<String, String> {
    if path.exists() {
        Ok(native_path_string(path))
    } else {
        Err(format!("模型文件不存在：{}", path.display()))
    }
}

/// 目录中按顺序找第一个存在的文件（同一模型的全精度 / int8 版本文件名不同）。
fn first_of(dir: &Path, names: &[&str]) -> Result<String, String> {
    names
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.exists())
        .map(|path| native_path_string(&path))
        .ok_or_else(|| format!("模型文件不存在：{}", dir.join(names[0]).display()))
}

/// 中文数字、日期、电话号码的读法规则（模型目录里有就用）。
fn rule_fsts(dir: &Path) -> Option<String> {
    let found: Vec<String> = ["phone-zh.fst", "date-zh.fst", "number-zh.fst"]
        .iter()
        .map(|name| dir.join(name))
        .filter(|path| path.is_file())
        .map(|path| native_path_string(&path))
        .collect();
    (!found.is_empty()).then(|| found.join(","))
}

#[derive(Default)]
struct Loaded {
    vad: AtomicBool,
    asr: AtomicBool,
    tts: AtomicBool,
}

// ---------------------------------------------------------------- 判停

struct VadStream {
    detector: VoiceActivityDetector,
    in_speech: bool,
}

fn vad_config(engine: VadEngine, model: &str, params: &VadParams) -> VadModelConfig {
    let mut config = VadModelConfig {
        sample_rate: 16_000,
        num_threads: 1,
        provider: Some("cpu".into()),
        debug: false,
        ..Default::default()
    };
    let (threshold, silence, speech) = (
        params.threshold,
        params.min_silence_ms as f32 / 1000.0,
        params.min_speech_ms as f32 / 1000.0,
    );
    match engine {
        VadEngine::Silero => {
            config.silero_vad = SileroVadModelConfig {
                model: Some(model.to_owned()),
                threshold,
                min_silence_duration: silence,
                min_speech_duration: speech,
                max_speech_duration: 30.0,
                ..Default::default()
            };
        }
        VadEngine::Ten => {
            config.ten_vad = TenVadModelConfig {
                model: Some(model.to_owned()),
                threshold,
                min_silence_duration: silence,
                min_speech_duration: speech,
                // TEN-VAD 以 16 ms（256 点）为一帧
                window_size: 256,
                max_speech_duration: 30.0,
            };
        }
    }
    config
}

fn vad_worker(jobs: Receiver<Job>, out: Out, loaded: Arc<Loaded>) {
    let mut config: Option<VadModelConfig> = None;
    let mut streams: HashMap<u64, VadStream> = HashMap::new();
    for (id, op, payload) in jobs {
        match op {
            Op::LoadVad {
                model,
                params,
                engine,
            } => {
                // 试建一个检测器确认模型可用
                match require(Path::new(&model)).and_then(|clean_model| {
                    let candidate = vad_config(engine, &clean_model, &params);
                    VoiceActivityDetector::create(&candidate, 30.0)
                        .map(|_| candidate)
                        .ok_or_else(|| "判停模型加载失败".to_owned())
                }) {
                    Ok(candidate) => {
                        config = Some(candidate);
                        streams.clear();
                        loaded.vad.store(true, Ordering::SeqCst);
                        send(
                            &out,
                            id,
                            Reply::Loaded {
                                sample_rate: Some(16_000),
                                speakers: None,
                            },
                            &[],
                        );
                    }
                    Err(message) => error(&out, id, message),
                }
            }
            Op::VadOpen { stream } => match config
                .as_ref()
                .and_then(|config| VoiceActivityDetector::create(config, 30.0))
            {
                Some(detector) => {
                    streams.insert(
                        stream,
                        VadStream {
                            detector,
                            in_speech: false,
                        },
                    );
                    send(&out, id, Reply::Done, &[]);
                }
                None => error(&out, id, "判停模型未加载"),
            },
            Op::VadFeed { stream } => {
                let Some(state) = streams.get_mut(&stream) else {
                    error(&out, id, "判停流不存在");
                    continue;
                };
                state
                    .detector
                    .accept_waveform(&to_f32(&bytes_to_pcm(&payload)));
                let mut events = Vec::new();
                // 已完成的语音段：之前没报过开始的先补开始
                while state.detector.front().is_some() {
                    state.detector.pop();
                    if !state.in_speech {
                        events.push(VadEventKind::SpeechStart);
                    }
                    events.push(VadEventKind::SpeechEnd);
                    state.in_speech = false;
                }
                if state.detector.detected() && !state.in_speech {
                    state.in_speech = true;
                    events.push(VadEventKind::SpeechStart);
                }
                send(&out, id, Reply::VadEvents { events }, &[]);
            }
            Op::VadClose { stream } => {
                streams.remove(&stream);
                send(&out, id, Reply::Done, &[]);
            }
            _ => error(&out, id, "判停线程收到不支持的请求"),
        }
    }
}

// ---------------------------------------------------------------- 识别

enum Recognizer {
    /// 整段识别（SenseVoice）
    Offline(OfflineRecognizer),
    /// 流式识别（Zipformer Transducer）：也能整段识别
    Online(OnlineRecognizer),
}

fn load_recognizer(
    dir: &Path,
    engine: AsrEngine,
    options: moss_voice_local_proto::AsrDecodeOptions,
) -> Result<Recognizer, String> {
    if options.hotwords.len() > 64
        || options
            .hotwords
            .iter()
            .any(|word| word.chars().count() > 32 || word.chars().any(char::is_control))
    {
        return Err("热词超过限制或包含控制字符".into());
    }
    match engine {
        AsrEngine::SenseVoice => {
            let mut config = OfflineRecognizerConfig::default();
            config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
                model: Some(require(&dir.join("model.int8.onnx"))?),
                language: Some("auto".into()),
                use_itn: true,
            };
            config.model_config.tokens = Some(require(&dir.join("tokens.txt"))?);
            config.model_config.provider = Some("cpu".into());
            config.model_config.num_threads = 2;
            config.model_config.debug = false;
            OfflineRecognizer::create(&config)
                .map(Recognizer::Offline)
                .ok_or_else(|| "SenseVoice 模型加载失败".to_owned())
        }
        AsrEngine::StreamingTransducer => {
            let mut config = OnlineRecognizerConfig::default();
            config.model_config.transducer = OnlineTransducerModelConfig {
                encoder: Some(first_of(dir, &["encoder.int8.onnx", "encoder.onnx"])?),
                decoder: Some(first_of(dir, &["decoder.int8.onnx", "decoder.onnx"])?),
                joiner: Some(first_of(dir, &["joiner.int8.onnx", "joiner.onnx"])?),
            };
            config.model_config.tokens = Some(require(&dir.join("tokens.txt"))?);
            config.model_config.provider = Some("cpu".into());
            config.model_config.num_threads = 2;
            config.model_config.debug = false;
            config.decoding_method = Some(
                if options.beam_search {
                    "modified_beam_search"
                } else {
                    "greedy_search"
                }
                .into(),
            );
            config.max_active_paths = 4;
            config.hotwords_score = 1.5;
            config.model_config.modeling_unit = Some("cjkchar".into());
            if options.beam_search && !options.hotwords.is_empty() {
                let tokens = std::fs::read_to_string(dir.join("tokens.txt"))
                    .map_err(|_| "无法读取识别词表".to_owned())?;
                let buffer = moss_voice_local_proto::cjk_hotwords(&options.hotwords, &tokens);
                if !buffer.is_empty() {
                    config.hotwords_buf = Some(buffer);
                }
            }
            OnlineRecognizer::create(&config)
                .map(Recognizer::Online)
                .ok_or_else(|| "流式识别模型加载失败".to_owned())
        }
    }
}

/// 解码已经送入的音频（流式识别边收边解码，说完时只剩最后一小段）。
fn decode_ready(recognizer: &OnlineRecognizer, stream: &OnlineStream) {
    while recognizer.is_ready(stream) {
        recognizer.decode(stream);
    }
}

/// 说完：补静音、结束输入、解码余下部分并取文字。
fn finish_online(recognizer: &OnlineRecognizer, stream: &OnlineStream) -> String {
    stream.accept_waveform(
        16_000,
        &vec![0.0; (16_000.0 * STREAM_TAIL_SECONDS) as usize],
    );
    stream.input_finished();
    decode_ready(recognizer, stream);
    recognizer
        .get_result(stream)
        .map(|result| result.text.trim().to_owned())
        .unwrap_or_default()
}

fn asr_worker(jobs: Receiver<Job>, out: Out, loaded: Arc<Loaded>) {
    let mut recognizer: Option<Recognizer> = None;
    let mut streams: HashMap<u64, OnlineStream> = HashMap::new();
    for (id, op, payload) in jobs {
        match op {
            Op::LoadAsr {
                dir,
                engine,
                options,
            } => {
                // 先释放旧模型上的流，再换模型
                streams.clear();
                match load_recognizer(Path::new(&dir), engine, options) {
                    Ok(created) => {
                        recognizer = Some(created);
                        loaded.asr.store(true, Ordering::SeqCst);
                        send(
                            &out,
                            id,
                            Reply::Loaded {
                                sample_rate: None,
                                speakers: None,
                            },
                            &[],
                        );
                    }
                    Err(message) => error(&out, id, message),
                }
            }
            Op::Transcribe { sample_rate } => {
                let samples = to_f32(&bytes_to_pcm(&payload));
                let text = match recognizer.as_ref() {
                    None => {
                        error(&out, id, "识别模型未加载");
                        continue;
                    }
                    Some(Recognizer::Offline(recognizer)) => {
                        let stream = recognizer.create_stream();
                        stream.accept_waveform(sample_rate as i32, &samples);
                        recognizer.decode(&stream);
                        stream
                            .get_result()
                            .map(|result| result.text.trim().to_owned())
                            .unwrap_or_default()
                    }
                    Some(Recognizer::Online(recognizer)) => {
                        let stream = recognizer.create_stream();
                        stream.accept_waveform(sample_rate as i32, &samples);
                        finish_online(recognizer, &stream)
                    }
                };
                send(&out, id, Reply::Text { text }, &[]);
            }
            Op::AsrOpen { stream } => match recognizer.as_ref() {
                Some(Recognizer::Online(recognizer)) => {
                    streams.insert(stream, recognizer.create_stream());
                    send(&out, id, Reply::Done, &[]);
                }
                Some(Recognizer::Offline(_)) => error(&out, id, "当前识别模型不支持流式识别"),
                None => error(&out, id, "识别模型未加载"),
            },
            Op::AsrFeed { stream } => match (recognizer.as_ref(), streams.get(&stream)) {
                (Some(Recognizer::Online(recognizer)), Some(online)) => {
                    online.accept_waveform(16_000, &to_f32(&bytes_to_pcm(&payload)));
                    decode_ready(recognizer, online);
                    let text = recognizer
                        .get_result(online)
                        .map(|result| result.text.trim().to_owned())
                        .unwrap_or_default();
                    send(&out, id, Reply::Text { text }, &[]);
                }
                _ => error(&out, id, "识别流不存在"),
            },
            Op::AsrFinish { stream } => match (recognizer.as_ref(), streams.remove(&stream)) {
                (Some(Recognizer::Online(recognizer)), Some(online)) => {
                    let text = finish_online(recognizer, &online);
                    send(&out, id, Reply::Text { text }, &[]);
                }
                _ => error(&out, id, "识别流不存在"),
            },
            Op::AsrClose { stream } => {
                streams.remove(&stream);
                send(&out, id, Reply::Done, &[]);
            }
            _ => error(&out, id, "识别线程收到不支持的请求"),
        }
    }
}

// ---------------------------------------------------------------- 合成

fn tts_config(dir: &Path, engine: TtsEngine) -> Result<OfflineTtsConfig, String> {
    let model = match engine {
        TtsEngine::Kokoro => {
            let lexicon = format!(
                "{},{}",
                require(&dir.join("lexicon-us-en.txt"))?,
                require(&dir.join("lexicon-zh.txt"))?
            );
            OfflineTtsModelConfig {
                kokoro: OfflineTtsKokoroModelConfig {
                    model: Some(first_of(dir, &["model.onnx", "model.int8.onnx"])?),
                    voices: Some(require(&dir.join("voices.bin"))?),
                    tokens: Some(require(&dir.join("tokens.txt"))?),
                    data_dir: Some(require(&dir.join("espeak-ng-data"))?),
                    dict_dir: Some(require(&dir.join("dict"))?),
                    lexicon: Some(lexicon),
                    length_scale: 1.0,
                    ..Default::default()
                },
                num_threads: 2,
                debug: false,
                ..Default::default()
            }
        }
        TtsEngine::Matcha => OfflineTtsModelConfig {
            matcha: OfflineTtsMatchaModelConfig {
                acoustic_model: Some(first_of(dir, &["model-steps-3.onnx", "model.onnx"])?),
                vocoder: Some(first_of(
                    dir,
                    &["vocos-16khz-univ.onnx", "vocos-22khz-univ.onnx"],
                )?),
                lexicon: Some(require(&dir.join("lexicon.txt"))?),
                tokens: Some(require(&dir.join("tokens.txt"))?),
                data_dir: Some(require(&dir.join("espeak-ng-data"))?),
                dict_dir: dir
                    .join("dict")
                    .is_dir()
                    .then(|| native_path_string(&dir.join("dict"))),
                ..Default::default()
            },
            num_threads: 2,
            debug: false,
            ..Default::default()
        },
    };
    Ok(OfflineTtsConfig {
        model,
        rule_fsts: rule_fsts(dir),
        ..Default::default()
    })
}

fn tts_worker(jobs: Receiver<Job>, out: Out, loaded: Arc<Loaded>) {
    let mut tts: Option<(OfflineTts, i32)> = None;
    for (id, op, _) in jobs {
        match op {
            Op::LoadTts { dir, engine } => {
                let created = tts_config(Path::new(&dir), engine).and_then(|config| {
                    let name = match engine {
                        TtsEngine::Kokoro => "Kokoro",
                        TtsEngine::Matcha => "Matcha",
                    };
                    OfflineTts::create(&config).ok_or_else(|| format!("{name} 模型加载失败"))
                });
                match created {
                    Ok(created) => {
                        let sample_rate = created.sample_rate().max(0) as u32;
                        let speakers = created.num_speakers().max(0);
                        tts = Some((created, speakers));
                        loaded.tts.store(true, Ordering::SeqCst);
                        send(
                            &out,
                            id,
                            Reply::Loaded {
                                sample_rate: Some(sample_rate),
                                speakers: Some(speakers as u32),
                            },
                            &[],
                        );
                    }
                    Err(message) => error(&out, id, message),
                }
            }
            Op::Synthesize {
                text,
                speaker,
                speed,
            } => {
                let Some((tts, speakers)) = tts.as_ref() else {
                    error(&out, id, "合成模型未加载");
                    continue;
                };
                // 单音色模型忽略音色编号
                let sid = if *speakers > 1 {
                    speaker.clamp(0, speakers - 1)
                } else {
                    0
                };
                let config = GenerationConfig {
                    sid,
                    speed: speed.clamp(0.25, 4.0),
                    ..Default::default()
                };
                match tts.generate_with_config(&text, &config, None::<fn(&[f32], f32) -> bool>) {
                    Some(audio) => {
                        let pcm = to_i16(audio.samples());
                        send(
                            &out,
                            id,
                            Reply::Audio {
                                sample_rate: audio.sample_rate().max(0) as u32,
                            },
                            &pcm_to_bytes(&pcm),
                        );
                    }
                    None => error(&out, id, "合成失败"),
                }
            }
            Op::SynthesizeStream {
                text,
                speaker,
                speed,
            } => {
                let Some((tts, speakers)) = tts.as_ref() else {
                    error(&out, id, "合成模型未加载");
                    continue;
                };
                let sid = if *speakers > 1 {
                    speaker.clamp(0, speakers - 1)
                } else {
                    0
                };
                let config = GenerationConfig {
                    sid,
                    speed: speed.clamp(0.25, 4.0),
                    ..Default::default()
                };
                let mut failed = false;
                let segments = moss_voice_local_proto::synthesis_segments(&text);
                let total = segments.len();
                for (idx, segment) in segments.into_iter().enumerate() {
                    match tts.generate_with_config(
                        &segment,
                        &config,
                        None::<fn(&[f32], f32) -> bool>,
                    ) {
                        Some(audio) => {
                            let sample_rate = audio.sample_rate().max(0) as u32;
                            let mut pcm = to_i16(audio.samples());
                            if total > 1 {
                                smooth_stream_subsegment(
                                    &mut pcm,
                                    sample_rate,
                                    idx == 0,
                                    idx + 1 == total,
                                );
                            }
                            send(
                                &out,
                                id,
                                Reply::AudioChunk { sample_rate },
                                &pcm_to_bytes(&pcm),
                            );
                        }
                        None => {
                            error(&out, id, "分段合成失败");
                            failed = true;
                            break;
                        }
                    }
                }
                if !failed {
                    send(&out, id, Reply::Done, &[]);
                }
            }
            _ => error(&out, id, "合成线程收到不支持的请求"),
        }
    }
}

const WORKER_QUEUE_CAPACITY: usize = 64;

fn spawn_worker(
    name: &str,
    worker: fn(Receiver<Job>, Out, Arc<Loaded>),
    out: &Out,
    loaded: &Arc<Loaded>,
) -> SyncSender<Job> {
    let (sender, receiver) = sync_channel::<Job>(WORKER_QUEUE_CAPACITY);
    let (out, loaded) = (out.clone(), loaded.clone());
    thread::Builder::new()
        .name(name.into())
        .spawn(move || worker(receiver, out, loaded))
        .expect("创建工作线程失败");
    sender
}

fn speaker_worker(jobs: Receiver<Job>, out: Out) {
    // Keep the verified file open for the extractor lifetime. On Windows the
    // share mode forbids writes/deletion while native ORT opens the same file.
    let mut extractor: Option<(SpeakerEmbeddingExtractor, std::fs::File, Vec<std::fs::File>)> =
        None;
    for (id, op, payload) in jobs {
        match op {
            Op::LoadSpeaker {
                model_id,
                models_root,
            } => {
                extractor = None;
                let result=(||->Result<(SpeakerEmbeddingExtractor,std::fs::File,Vec<std::fs::File>),String>{
                    if model_id!=SPEAKER_MODEL_ID || !payload.is_empty(){return Err("声纹模型编号无效".into());}
                    let root=std::fs::canonicalize(&models_root).map_err(|_|"声纹模型目录不可读")?;
                    let mut directory_guards=Vec::new();
                    #[cfg(windows)] {
                        use std::os::windows::fs::OpenOptionsExt;
                        // A locked file alone cannot prevent an ancestor rename.
                        // Hold all canonical directories until model unload.
                        for directory in root.ancestors() {
                            let guard=std::fs::OpenOptions::new().read(true).share_mode(1)
                                .custom_flags(0x0200_0000).open(directory)
                                .map_err(|_|"声纹模型目录无法锁定")?;
                            directory_guards.push(guard);
                        }
                        if std::fs::canonicalize(&models_root).map_err(|_|"声纹模型目录失效")?!=root {
                            return Err("声纹模型目录已变化".into());
                        }
                    }
                    #[cfg(unix)] {
                        for directory in root.ancestors() {
                            let dir_meta=std::fs::symlink_metadata(directory).map_err(|_|"声纹模型目录不可读")?;
                            if dir_meta.file_type().is_symlink() || !dir_meta.is_dir() {
                                return Err("声纹模型目录不得包含符号链接".into());
                            }
                            let guard=std::fs::File::open(directory).map_err(|_|"声纹模型目录无法打开")?;
                            directory_guards.push(guard);
                        }
                        if std::fs::canonicalize(&models_root).map_err(|_|"声纹模型目录失效")?!=root {
                            return Err("声纹模型目录已变化".into());
                        }
                    }
                    #[cfg(not(any(windows, unix)))]
                    return Err("当前声纹模型安全加载仅支持桌面操作系统".into());
                    let path=root.join(SPEAKER_MODEL_FILE);
                    let metadata=std::fs::symlink_metadata(&path).map_err(|_|"声纹模型未安装")?;
                    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len()!=SPEAKER_MODEL_BYTES{return Err("声纹模型大小或文件类型不匹配".into());}
                    #[cfg(windows)] {
                        use std::os::windows::fs::MetadataExt;
                        if metadata.file_attributes() & 0x400!=0{return Err("声纹模型不得为重解析文件".into());}
                    }
                    let mut options=std::fs::OpenOptions::new();options.read(true);
                    #[cfg(windows)] { use std::os::windows::fs::OpenOptionsExt; options.share_mode(1); }
                    #[cfg(target_os = "macos")] { use std::os::unix::fs::OpenOptionsExt; options.custom_flags(0x0100); }
                    let mut file=options.open(&path).map_err(|_|"声纹模型不可读或正被修改")?;
                    let opened_meta=file.metadata().map_err(|_|"声纹模型不可读")?;
                    if opened_meta.len()!=SPEAKER_MODEL_BYTES{return Err("声纹模型大小不匹配".into());}
                    #[cfg(unix)] {
                        use std::os::unix::fs::MetadataExt;
                        if opened_meta.nlink()!=1 || opened_meta.dev()!=metadata.dev() || opened_meta.ino()!=metadata.ino() {
                            return Err("声纹模型文件链接状态异常".into());
                        }
                    }
                    let mut digest=Sha256::new();let mut total=0u64;let mut block=[0;65536];
                    loop {use std::io::Read;let n=file.read(&mut block).map_err(|_|"声纹模型读取失败")?;if n==0{break;}total+=n as u64;if total>SPEAKER_MODEL_BYTES{return Err("声纹模型过大".into());}digest.update(&block[..n]);}
                    if total!=SPEAKER_MODEL_BYTES || format!("{:x}",digest.finalize())!=SPEAKER_MODEL_SHA256{return Err("声纹模型 SHA-256 不匹配".into());}
                    #[cfg(unix)] {
                        use std::os::unix::fs::MetadataExt;
                        let recheck=std::fs::symlink_metadata(&path).map_err(|_|"声纹模型校验后失效")?;
                        if recheck.file_type().is_symlink() || recheck.dev()!=opened_meta.dev() || recheck.ino()!=opened_meta.ino() || recheck.len()!=SPEAKER_MODEL_BYTES {
                            return Err("声纹模型加载前发生变更".into());
                        }
                    }
                    let config=SpeakerEmbeddingExtractorConfig{model:Some(native_path_string(&path)),num_threads:1,debug:false,provider:Some("cpu".into())};
                    let created=SpeakerEmbeddingExtractor::create(&config).ok_or("声纹模型加载失败")?;
                    if created.dim()!=SPEAKER_DIMENSION as i32{return Err("声纹模型输出维度不匹配".into());}
                    Ok((created,file,directory_guards))
                })();
                match result {
                    Ok(created) => {
                        extractor = Some(created);
                        send(
                            &out,
                            id,
                            Reply::SpeakerLoaded {
                                model_id: SPEAKER_MODEL_ID.into(),
                                sample_rate: SPEAKER_SAMPLE_RATE,
                                dimension: SPEAKER_DIMENSION,
                            },
                            &[],
                        );
                    }
                    Err(message) => error(&out, id, message),
                }
            }
            Op::SpeakerEmbed {
                model_id,
                sample_rate,
                utterance_generation,
            } => {
                if model_id != SPEAKER_MODEL_ID || !valid_speaker_pcm(sample_rate, payload.len()) {
                    error(&out, id, "声纹采样必须为2～10秒16kHz单声道PCM");
                    continue;
                }
                let Some((extractor, _verified_file, _directories)) = extractor.as_ref() else {
                    error(&out, id, "声纹模型未加载");
                    continue;
                };
                let Some(stream) = extractor.create_stream() else {
                    error(&out, id, "声纹流创建失败");
                    continue;
                };
                stream.accept_waveform(sample_rate as i32, &to_f32(&bytes_to_pcm(&payload)));
                stream.input_finished();
                if !extractor.is_ready(&stream) {
                    error(&out, id, "声纹片段不足");
                    continue;
                }
                match extractor
                    .compute(&stream)
                    .and_then(|v| embedding_to_bytes(&v).ok())
                {
                    Some(bytes) => send(
                        &out,
                        id,
                        Reply::Embedding {
                            model_id: SPEAKER_MODEL_ID.into(),
                            sample_rate,
                            dimension: SPEAKER_DIMENSION,
                            utterance_generation,
                            format: "f32le".into(),
                        },
                        &bytes,
                    ),
                    None => error(&out, id, "声纹向量无效"),
                }
            }
            _ => error(&out, id, "声纹线程收到不支持的请求"),
        }
    }
}

fn main() {
    if std::env::args().any(|arg| arg == "--version") {
        println!(
            "moss-voice-local {} (protocol {PROTOCOL_VERSION}, GPL-3.0-or-later)",
            env!("CARGO_PKG_VERSION")
        );
        return;
    }
    let out: Out = Arc::new(Mutex::new(BufWriter::new(io::stdout())));
    let loaded = Arc::new(Loaded::default());
    let vad = spawn_worker("vad", vad_worker, &out, &loaded);
    let asr = spawn_worker("asr", asr_worker, &out, &loaded);
    let tts = spawn_worker("tts", tts_worker, &out, &loaded);
    let (speaker, speaker_jobs) = std::sync::mpsc::sync_channel::<Job>(2);
    let speaker_out = out.clone();
    thread::Builder::new()
        .name("speaker".into())
        .spawn(move || speaker_worker(speaker_jobs, speaker_out))
        .expect("创建声纹线程失败");
    let mut input = BufReader::new(io::stdin());
    // 标准输入关闭（MOSS 退出）或数据损坏：退出
    while let Ok(Some((request, payload))) = read_frame::<_, Request>(&mut input) {
        let Request { id, op } = request;
        if matches!(&op, Op::LoadSpeaker { .. } | Op::SpeakerEmbed { .. }) {
            if let Op::SpeakerEmbed { sample_rate, .. } = &op
                && !valid_speaker_pcm(*sample_rate, payload.len())
            {
                error(&out, id, "声纹采样格式无效");
                continue;
            }
            if speaker.try_send((id, op, payload)).is_err() {
                error(&out, id, "声纹队列已满或退出");
            }
            continue;
        }
        let target = match &op {
            Op::Hello => {
                let reply = Reply::Hello {
                    protocol: PROTOCOL_VERSION,
                    version: env!("CARGO_PKG_VERSION").into(),
                    vad: loaded.vad.load(Ordering::SeqCst),
                    asr: loaded.asr.load(Ordering::SeqCst),
                    tts: loaded.tts.load(Ordering::SeqCst),
                };
                send(&out, id, reply, &[]);
                continue;
            }
            Op::LoadVad { .. } | Op::VadOpen { .. } | Op::VadFeed { .. } | Op::VadClose { .. } => {
                &vad
            }
            Op::LoadAsr { .. }
            | Op::Transcribe { .. }
            | Op::AsrOpen { .. }
            | Op::AsrFeed { .. }
            | Op::AsrFinish { .. }
            | Op::AsrClose { .. } => &asr,
            Op::LoadTts { .. } | Op::Synthesize { .. } | Op::SynthesizeStream { .. } => &tts,
            Op::LoadSpeaker { .. } | Op::SpeakerEmbed { .. } => {
                unreachable!("speaker routed independently")
            }
        };
        match target.try_send((id, op, payload)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => error(&out, id, "工作队列已满"),
            Err(TrySendError::Disconnected(_)) => error(&out, id, "工作线程已退出"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_windows_verbatim_prefix_handles_drive_unc_and_plain_paths() {
        assert_eq!(
            strip_windows_verbatim_prefix(
                r"\\?\C:\Users\demo\AppData\Roaming\moss\models\kokoro-multi-lang-v1_0\espeak-ng-data"
            ),
            r"C:\Users\demo\AppData\Roaming\moss\models\kokoro-multi-lang-v1_0\espeak-ng-data"
        );
        assert_eq!(
            strip_windows_verbatim_prefix(r"\\?\UNC\server\share\moss\models"),
            r"\\server\share\moss\models"
        );
        assert_eq!(
            strip_windows_verbatim_prefix(r"C:\Users\demo\models"),
            r"C:\Users\demo\models"
        );
        assert_eq!(
            strip_windows_verbatim_prefix("/Users/demo/Library/Application Support/moss/models"),
            "/Users/demo/Library/Application Support/moss/models"
        );
    }

    #[cfg(windows)]
    #[test]
    fn tts_config_strips_verbatim_prefix_from_canonicalized_model_dirs() {
        let unique = format!(
            "moss-voice-local-path-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let temp = std::env::temp_dir().join(unique);
        let kokoro_dir = temp.join("kokoro");
        std::fs::create_dir_all(kokoro_dir.join("espeak-ng-data")).unwrap();
        std::fs::create_dir_all(kokoro_dir.join("dict")).unwrap();
        for file in [
            "model.onnx",
            "voices.bin",
            "tokens.txt",
            "lexicon-us-en.txt",
            "lexicon-zh.txt",
            "phone-zh.fst",
        ] {
            std::fs::write(kokoro_dir.join(file), b"stub").unwrap();
        }
        let canonical = std::fs::canonicalize(&kokoro_dir).unwrap();
        assert!(canonical.to_string_lossy().starts_with(r"\\?\"));

        let cfg = tts_config(&canonical, TtsEngine::Kokoro).unwrap();
        let kokoro = &cfg.model.kokoro;
        for field in [
            kokoro.model.as_deref(),
            kokoro.voices.as_deref(),
            kokoro.tokens.as_deref(),
            kokoro.data_dir.as_deref(),
            kokoro.dict_dir.as_deref(),
            kokoro.lexicon.as_deref(),
            cfg.rule_fsts.as_deref(),
        ] {
            let value = field.expect("populated kokoro path");
            assert!(
                !value.contains(r"\\?\"),
                "unexpected verbatim prefix in {value}"
            );
        }

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn worker_queue_is_bounded_and_rejects_overflow_without_blocking() {
        let out: Out = Arc::new(Mutex::new(BufWriter::new(io::stdout())));
        let loaded = Arc::new(Loaded::default());
        let gate = Arc::new(std::sync::Barrier::new(2));
        let (picked_tx, picked_rx) = sync_channel::<()>(1);
        let (tx, rx) = sync_channel::<Job>(WORKER_QUEUE_CAPACITY);
        let gate_worker = gate.clone();
        let handle = thread::spawn(move || {
            let _first = rx.recv().unwrap();
            picked_tx.send(()).unwrap();
            gate_worker.wait();
        });
        // First item is consumed by the blocked worker; next WORKER_QUEUE_CAPACITY items fill the queue.
        tx.try_send((1, Op::Hello, Vec::new())).unwrap();
        picked_rx.recv().unwrap();
        let mut accepted = 0;
        for id in 2..=(WORKER_QUEUE_CAPACITY as u64 + 10) {
            if tx.try_send((id, Op::Hello, Vec::new())).is_ok() {
                accepted += 1;
            }
        }
        assert_eq!(
            accepted, WORKER_QUEUE_CAPACITY,
            "bounded queue must accept exact WORKER_QUEUE_CAPACITY={WORKER_QUEUE_CAPACITY}"
        );
        assert!(matches!(
            tx.try_send((9999, Op::Hello, Vec::new())),
            Err(TrySendError::Full(_))
        ));
        gate.wait();
        handle.join().unwrap();
        let _ = (out, loaded);
    }

    #[test]
    fn to_i16_soft_knee_and_subsegment_smoothing_avoid_clipping_and_seam_clicks() {
        let pcm = to_i16(&[0.0, 0.5, -0.5, 0.8, 1.0, 1.35, -1.35, f32::NAN]);
        assert_eq!(pcm[0], 0);
        assert_eq!(pcm[1], 16_384);
        assert_eq!(pcm[2], -16_384);
        assert!(
            pcm[3] < pcm[4] && pcm[4] < pcm[5],
            "超幅峰值平滑单调无平顶削波: {pcm:?}"
        );
        assert!(pcm[5] < 28_000 && pcm[6] > -28_000, "预留峰值余量: {pcm:?}");
        assert_eq!(pcm[7], 0);

        let rate = 24_000;
        let mut seg1 = vec![6000i16; 2400];
        seg1.extend(vec![0i16; 7200]); // 300ms 尾部死寂
        smooth_stream_subsegment(&mut seg1, rate, true, false);
        assert_eq!(seg1[0], 6000, "首段起点由上层统一淡入，子分段内不重复衰减");
        assert!(
            seg1.len() <= 2400 + 1950,
            "非末段超长静音被裁至约 80ms: {}",
            seg1.len()
        );
        assert_eq!(*seg1.last().unwrap(), 0, "非末段结尾平滑归零");

        let mut seg2 = vec![-5000i16; 2400];
        smooth_stream_subsegment(&mut seg2, rate, false, true);
        assert_eq!(seg2[0], 0, "非首段开头平滑淡入消除直流跳变");
        assert_eq!(*seg2.last().unwrap(), -5000, "末段结尾交由上层统一收尾");
    }
}
