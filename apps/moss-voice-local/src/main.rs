// SPDX-License-Identifier: GPL-3.0-or-later
//! MOSS 本地模型辅助程序 `moss-voice-local`（内置语音技术书 §11.2，NV6；可选模型见 §13.7）。
//!
//! 由 MOSS 启动，经标准输入输出按 `moss-voice-local-proto` 的帧协议通信；标准输入关闭即退出。
//! 判停（Silero / TEN-VAD）、识别（SenseVoice / 流式 Zipformer）、合成（Kokoro / Matcha）各在自己的线程里
//! 加载与运行，一个较慢的合成不会拖住判停。模型对象在各自线程内创建，不跨线程共享。
//!
//! 本程序静态链接 sherpa-onnx（Apache-2.0），其中包含 espeak-ng（GPL-3.0），因此以 GPL-3.0-or-later 发布。

use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use sha2::{Digest,Sha256};
use moss_voice_local_proto::{SPEAKER_MODEL_ID,SPEAKER_MODEL_FILE,SPEAKER_MODEL_BYTES,SPEAKER_MODEL_SHA256,SPEAKER_DIMENSION,SPEAKER_SAMPLE_RATE,valid_speaker_pcm,embedding_to_bytes};
use sherpa_onnx::{SpeakerEmbeddingExtractor,SpeakerEmbeddingExtractorConfig};

use moss_voice_local_proto::{
    AsrEngine, Op, PROTOCOL_VERSION, Reply, Request, Response, TtsEngine, VadEngine, VadEventKind, VadParams,
    bytes_to_pcm, pcm_to_bytes, read_frame, write_frame,
};
use sherpa_onnx::{
    GenerationConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig, OfflineTts,
    OfflineTtsConfig, OfflineTtsKokoroModelConfig, OfflineTtsMatchaModelConfig, OfflineTtsModelConfig, OnlineRecognizer,
    OnlineRecognizerConfig, OnlineStream, OnlineTransducerModelConfig, SileroVadModelConfig, TenVadModelConfig,
    VadModelConfig, VoiceActivityDetector,
};

type Out = Arc<Mutex<BufWriter<io::Stdout>>>;
type Job = (u64, Op, Vec<u8>);

/// 流式识别说完后补的静音：把最后几个字从模型的右侧上下文里“推”出来（实测 0.66 秒会丢末字，1.0 秒不丢，只多约 30 ms）
const STREAM_TAIL_SECONDS: f32 = 1.0;

fn send(out: &Out, id: u64, reply: Reply, payload: &[u8]) {
    if let Ok(mut writer) = out.lock() {
        if write_frame(&mut *writer, &Response { id, reply }, payload).is_err() {
            // MOSS 已退出：结束进程
            std::process::exit(0);
        }
    }
}

fn error(out: &Out, id: u64, message: impl Into<String>) {
    send(out, id, Reply::Error { message: message.into() }, &[]);
}

fn to_f32(pcm: &[i16]) -> Vec<f32> {
    pcm.iter().map(|&sample| sample as f32 / 32768.0).collect()
}

fn to_i16(samples: &[f32]) -> Vec<i16> {
    samples.iter().map(|&sample| (sample.clamp(-1.0, 1.0) * 32767.0) as i16).collect()
}

fn require(path: &Path) -> Result<String, String> {
    if path.exists() {
        Ok(path.to_string_lossy().into_owned())
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
        .map(|path| path.to_string_lossy().into_owned())
        .ok_or_else(|| format!("模型文件不存在：{}", dir.join(names[0]).display()))
}

/// 中文数字、日期、电话号码的读法规则（模型目录里有就用）。
fn rule_fsts(dir: &Path) -> Option<String> {
    let found: Vec<String> = ["phone-zh.fst", "date-zh.fst", "number-zh.fst"]
        .iter()
        .map(|name| dir.join(name))
        .filter(|path| path.is_file())
        .map(|path| path.to_string_lossy().into_owned())
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
    let (threshold, silence, speech) =
        (params.threshold, params.min_silence_ms as f32 / 1000.0, params.min_speech_ms as f32 / 1000.0);
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
            Op::LoadVad { model, params, engine } => {
                let candidate = vad_config(engine, &model, &params);
                // 试建一个检测器确认模型可用
                match require(Path::new(&model)).and_then(|_| {
                    VoiceActivityDetector::create(&candidate, 30.0).map(|_| ()).ok_or_else(|| "判停模型加载失败".to_owned())
                }) {
                    Ok(()) => {
                        config = Some(candidate);
                        streams.clear();
                        loaded.vad.store(true, Ordering::SeqCst);
                        send(&out, id, Reply::Loaded { sample_rate: Some(16_000), speakers: None }, &[]);
                    }
                    Err(message) => error(&out, id, message),
                }
            }
            Op::VadOpen { stream } => match config.as_ref().and_then(|config| VoiceActivityDetector::create(config, 30.0)) {
                Some(detector) => {
                    streams.insert(stream, VadStream { detector, in_speech: false });
                    send(&out, id, Reply::Done, &[]);
                }
                None => error(&out, id, "判停模型未加载"),
            },
            Op::VadFeed { stream } => {
                let Some(state) = streams.get_mut(&stream) else {
                    error(&out, id, "判停流不存在");
                    continue;
                };
                state.detector.accept_waveform(&to_f32(&bytes_to_pcm(&payload)));
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

fn load_recognizer(dir: &Path, engine: AsrEngine, options: moss_voice_local_proto::AsrDecodeOptions) -> Result<Recognizer, String> {
    if options.hotwords.len() > 64 || options.hotwords.iter().any(|word| word.chars().count() > 32 || word.chars().any(char::is_control)) {
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
            OfflineRecognizer::create(&config).map(Recognizer::Offline).ok_or_else(|| "SenseVoice 模型加载失败".to_owned())
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
            config.decoding_method = Some(if options.beam_search { "modified_beam_search" } else { "greedy_search" }.into());
            config.max_active_paths = 4;
            config.hotwords_score = 1.5;
            config.model_config.modeling_unit = Some("cjkchar".into());
            if options.beam_search && !options.hotwords.is_empty() {
                let tokens = std::fs::read_to_string(dir.join("tokens.txt")).map_err(|_| "无法读取识别词表".to_owned())?;
                let buffer = moss_voice_local_proto::cjk_hotwords(&options.hotwords, &tokens);
                if !buffer.is_empty() { config.hotwords_buf = Some(buffer); }
            }
            OnlineRecognizer::create(&config).map(Recognizer::Online).ok_or_else(|| "流式识别模型加载失败".to_owned())
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
    stream.accept_waveform(16_000, &vec![0.0; (16_000.0 * STREAM_TAIL_SECONDS) as usize]);
    stream.input_finished();
    decode_ready(recognizer, stream);
    recognizer.get_result(stream).map(|result| result.text.trim().to_owned()).unwrap_or_default()
}

fn asr_worker(jobs: Receiver<Job>, out: Out, loaded: Arc<Loaded>) {
    let mut recognizer: Option<Recognizer> = None;
    let mut streams: HashMap<u64, OnlineStream> = HashMap::new();
    for (id, op, payload) in jobs {
        match op {
            Op::LoadAsr { dir, engine, options } => {
                // 先释放旧模型上的流，再换模型
                streams.clear();
                match load_recognizer(Path::new(&dir), engine, options) {
                    Ok(created) => {
                        recognizer = Some(created);
                        loaded.asr.store(true, Ordering::SeqCst);
                        send(&out, id, Reply::Loaded { sample_rate: None, speakers: None }, &[]);
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
                        stream.get_result().map(|result| result.text.trim().to_owned()).unwrap_or_default()
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
                    let text = recognizer.get_result(online).map(|result| result.text.trim().to_owned()).unwrap_or_default();
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
            let lexicon = format!("{},{}", require(&dir.join("lexicon-us-en.txt"))?, require(&dir.join("lexicon-zh.txt"))?);
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
                vocoder: Some(first_of(dir, &["vocos-16khz-univ.onnx", "vocos-22khz-univ.onnx"])?),
                lexicon: Some(require(&dir.join("lexicon.txt"))?),
                tokens: Some(require(&dir.join("tokens.txt"))?),
                data_dir: Some(require(&dir.join("espeak-ng-data"))?),
                dict_dir: dir.join("dict").is_dir().then(|| dir.join("dict").to_string_lossy().into_owned()),
                ..Default::default()
            },
            num_threads: 2,
            debug: false,
            ..Default::default()
        },
    };
    Ok(OfflineTtsConfig { model, rule_fsts: rule_fsts(dir), ..Default::default() })
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
                        send(&out, id, Reply::Loaded { sample_rate: Some(sample_rate), speakers: Some(speakers as u32) }, &[]);
                    }
                    Err(message) => error(&out, id, message),
                }
            }
            Op::Synthesize { text, speaker, speed } => {
                let Some((tts, speakers)) = tts.as_ref() else {
                    error(&out, id, "合成模型未加载");
                    continue;
                };
                // 单音色模型忽略音色编号
                let sid = if *speakers > 1 { speaker.clamp(0, speakers - 1) } else { 0 };
                let config = GenerationConfig { sid, speed: speed.clamp(0.25, 4.0), ..Default::default() };
                match tts.generate_with_config(&text, &config, None::<fn(&[f32], f32) -> bool>) {
                    Some(audio) => {
                        let pcm = to_i16(audio.samples());
                        send(&out, id, Reply::Audio { sample_rate: audio.sample_rate().max(0) as u32 }, &pcm_to_bytes(&pcm));
                    }
                    None => error(&out, id, "合成失败"),
                }
            }
            Op::SynthesizeStream { text, speaker, speed } => {
                let Some((tts, speakers)) = tts.as_ref() else {
                    error(&out, id, "合成模型未加载");
                    continue;
                };
                let sid = if *speakers > 1 { speaker.clamp(0, speakers - 1) } else { 0 };
                let config = GenerationConfig { sid, speed: speed.clamp(0.25, 4.0), ..Default::default() };
                let mut failed = false;
                for segment in moss_voice_local_proto::synthesis_segments(&text) {
                    match tts.generate_with_config(&segment, &config, None::<fn(&[f32], f32) -> bool>) {
                        Some(audio) => send(&out, id, Reply::AudioChunk { sample_rate: audio.sample_rate().max(0) as u32 }, &pcm_to_bytes(&to_i16(audio.samples()))),
                        None => { error(&out, id, "分段合成失败"); failed = true; break; }
                    }
                }
                if !failed { send(&out, id, Reply::Done, &[]); }
            }
            _ => error(&out, id, "合成线程收到不支持的请求"),
        }
    }
}

fn spawn_worker(name: &str, worker: fn(Receiver<Job>, Out, Arc<Loaded>), out: &Out, loaded: &Arc<Loaded>) -> Sender<Job> {
    let (sender, receiver) = channel();
    let (out, loaded) = (out.clone(), loaded.clone());
    thread::Builder::new()
        .name(name.into())
        .spawn(move || worker(receiver, out, loaded))
        .expect("创建工作线程失败");
    sender
}

fn speaker_worker(jobs:Receiver<Job>,out:Out){
    // Keep the verified file open for the extractor lifetime. On Windows the
    // share mode forbids writes/deletion while native ORT opens the same file.
    let mut extractor:Option<(SpeakerEmbeddingExtractor,std::fs::File,Vec<std::fs::File>)>=None;
    for (id,op,payload) in jobs {
        match op {
            Op::LoadSpeaker{model_id,models_root}=>{
                extractor=None;
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
                    #[cfg(not(windows))]
                    return Err("当前声纹模型安全加载仅支持 Windows".into());
                    let path=root.join(SPEAKER_MODEL_FILE);
                    let metadata=std::fs::symlink_metadata(&path).map_err(|_|"声纹模型未安装")?;
                    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len()!=SPEAKER_MODEL_BYTES{return Err("声纹模型大小或文件类型不匹配".into());}
                    #[cfg(windows)] {
                        use std::os::windows::fs::MetadataExt;
                        if metadata.file_attributes() & 0x400!=0{return Err("声纹模型不得为重解析文件".into());}
                    }
                    let mut options=std::fs::OpenOptions::new();options.read(true);
                    #[cfg(windows)] { use std::os::windows::fs::OpenOptionsExt; options.share_mode(1); }
                    let mut file=options.open(&path).map_err(|_|"声纹模型不可读或正被修改")?;
                    if file.metadata().map_err(|_|"声纹模型不可读")?.len()!=SPEAKER_MODEL_BYTES{return Err("声纹模型大小不匹配".into());}
                    let mut digest=Sha256::new();let mut total=0u64;let mut block=[0;65536];
                    loop {use std::io::Read;let n=file.read(&mut block).map_err(|_|"声纹模型读取失败")?;if n==0{break;}total+=n as u64;if total>SPEAKER_MODEL_BYTES{return Err("声纹模型过大".into());}digest.update(&block[..n]);}
                    if total!=SPEAKER_MODEL_BYTES || format!("{:x}",digest.finalize())!=SPEAKER_MODEL_SHA256{return Err("声纹模型 SHA-256 不匹配".into());}
                    let config=SpeakerEmbeddingExtractorConfig{model:Some(path.to_string_lossy().into_owned()),num_threads:1,debug:false,provider:Some("cpu".into())};
                    let created=SpeakerEmbeddingExtractor::create(&config).ok_or("声纹模型加载失败")?;
                    if created.dim()!=SPEAKER_DIMENSION as i32{return Err("声纹模型输出维度不匹配".into());}
                    Ok((created,file,directory_guards))
                })();
                match result {
                    Ok(created)=>{extractor=Some(created);send(&out,id,Reply::SpeakerLoaded{model_id:SPEAKER_MODEL_ID.into(),sample_rate:SPEAKER_SAMPLE_RATE,dimension:SPEAKER_DIMENSION},&[]);},
                    Err(message)=>error(&out,id,message),
                }
            },
            Op::SpeakerEmbed{model_id,sample_rate,utterance_generation}=>{
                if model_id!=SPEAKER_MODEL_ID || !valid_speaker_pcm(sample_rate,payload.len()){error(&out,id,"声纹采样必须为2～10秒16kHz单声道PCM");continue;}
                let Some((extractor,_verified_file,_directories))=extractor.as_ref() else{error(&out,id,"声纹模型未加载");continue;};
                let Some(stream)=extractor.create_stream() else{error(&out,id,"声纹流创建失败");continue;};
                stream.accept_waveform(sample_rate as i32,&to_f32(&bytes_to_pcm(&payload)));
                stream.input_finished();
                if !extractor.is_ready(&stream){error(&out,id,"声纹片段不足");continue;}
                match extractor.compute(&stream).and_then(|v|embedding_to_bytes(&v).ok()) {
                    Some(bytes)=>send(&out,id,Reply::Embedding{model_id:SPEAKER_MODEL_ID.into(),sample_rate,dimension:SPEAKER_DIMENSION,utterance_generation,format:"f32le".into()},&bytes),
                    None=>error(&out,id,"声纹向量无效"),
                }
            },
            _=>error(&out,id,"声纹线程收到不支持的请求"),
        }
    }
}

fn main() {
    if std::env::args().any(|arg| arg == "--version") {
        println!("moss-voice-local {} (protocol {PROTOCOL_VERSION}, GPL-3.0-or-later)", env!("CARGO_PKG_VERSION"));
        return;
    }
    let out: Out = Arc::new(Mutex::new(BufWriter::new(io::stdout())));
    let loaded = Arc::new(Loaded::default());
    let vad = spawn_worker("vad", vad_worker, &out, &loaded);
    let asr = spawn_worker("asr", asr_worker, &out, &loaded);
    let tts = spawn_worker("tts", tts_worker, &out, &loaded);
    let (speaker,speaker_jobs)=std::sync::mpsc::sync_channel::<Job>(2);
    let speaker_out=out.clone();
    thread::Builder::new().name("speaker".into()).spawn(move||speaker_worker(speaker_jobs,speaker_out)).expect("创建声纹线程失败");
    let mut input = BufReader::new(io::stdin());
    loop {
        let (request, payload): (Request, Vec<u8>) = match read_frame(&mut input) {
            Ok(Some(frame)) => frame,
            // 标准输入关闭（MOSS 退出）或数据损坏：退出
            Ok(None) | Err(_) => break,
        };
        let Request { id, op } = request;
        if matches!(&op,Op::LoadSpeaker{..}|Op::SpeakerEmbed{..}) {
            if let Op::SpeakerEmbed{sample_rate,..}=&op {
                if !valid_speaker_pcm(*sample_rate,payload.len()){error(&out,id,"声纹采样格式无效");continue;}
            }
            if speaker.try_send((id,op,payload)).is_err(){error(&out,id,"声纹队列已满或退出");}
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
            Op::LoadVad { .. } | Op::VadOpen { .. } | Op::VadFeed { .. } | Op::VadClose { .. } => &vad,
            Op::LoadAsr { .. }
            | Op::Transcribe { .. }
            | Op::AsrOpen { .. }
            | Op::AsrFeed { .. }
            | Op::AsrFinish { .. }
            | Op::AsrClose { .. } => &asr,
            Op::LoadTts { .. } | Op::Synthesize { .. } | Op::SynthesizeStream { .. } => &tts,
            Op::LoadSpeaker{..}|Op::SpeakerEmbed{..}=>unreachable!("speaker routed independently"),
        };
        if target.send((id, op, payload)).is_err() {
            error(&out, id, "工作线程已退出");
        }
    }
}
