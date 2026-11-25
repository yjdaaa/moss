// SPDX-License-Identifier: GPL-3.0-or-later
//! MOSS 本地模型辅助程序 `moss-voice-local`（内置语音技术书 §11.2，NV6；可选模型见 §13.7）。
//!
//! 由 MOSS 启动，经标准输入输出按 `moss-voice-local-proto` 的帧协议通信；标准输入关闭即退出。
//! 判停（Silero / TEN-VAD）、识别（SenseVoice / 流式 Zipformer / Qwen3-ASR / FireRedASR2-CTC）、
//! 合成（Kokoro / Matcha / ZipVoice）及辅助降噪/尾音判停（GTCRN / SmartTurn）各在自己的线程里
//! 加载与运行，一个较慢的合成不会拖住判停。模型对象在各自线程内创建，不跨线程共享。
//!
//! 本程序静态链接 sherpa-onnx（Apache-2.0），其中包含 espeak-ng（GPL-3.0），因此以 GPL-3.0-or-later 发布。

mod kokoro_prosody;
mod smart_turn;
mod vits_zh;

use moss_voice_local_proto::{
    SPEAKER_DIMENSION, SPEAKER_MODEL_BYTES, SPEAKER_MODEL_FILE, SPEAKER_MODEL_ID,
    SPEAKER_MODEL_SHA256, SPEAKER_SAMPLE_RATE, embedding_to_bytes, valid_speaker_pcm,
};
use sha2::{Digest, Sha256};
use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;

use moss_voice_local_proto::{
    AsrEngine, DenoiseReport, Op, PROTOCOL_VERSION, Reply, Request, Response, TtsEngine, VadEngine,
    VadEventKind, VadParams, bytes_to_pcm, pcm_to_bytes, read_frame, write_frame,
};
use sherpa_onnx::{
    GenerationConfig, KeywordSpotter, KeywordSpotterConfig, OfflineFireRedAsrCtcModelConfig,
    OfflineQwen3ASRModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineSenseVoiceModelConfig, OfflineSpeechDenoiser, OfflineSpeechDenoiserConfig,
    OfflineSpeechDenoiserDpdfNetModelConfig, OfflineSpeechDenoiserGtcrnModelConfig,
    OfflineSpeechDenoiserModelConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsMatchaModelConfig, OfflineTtsModelConfig, OfflineTtsVitsModelConfig,
    OfflineTtsZipvoiceModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineSpeechDenoiser,
    OnlineSpeechDenoiserConfig, OnlineStream, OnlineTransducerModelConfig, SileroVadModelConfig,
    TenVadModelConfig, VadModelConfig, VoiceActivityDetector, Wave,
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

/// 合成输出隔直高通的截止频率（Hz）：一阶滤波在 80 Hz 以上的人声基频处衰减不到 0.5 dB。
const TTS_DC_BLOCK_HZ: f32 = 25.0;

/// 一阶隔直高通 `y[n] = x[n] - x[n-1] + R·y[n-1]`。Kokoro 声码器输出带随音量起伏的正向直流偏移
/// （实测每 100 ms 均值达有效值的 0.4～0.6 倍、0～20 Hz 占全段约三成能量，正峰 0.63 / 负峰 0.29），
/// 不滤掉会让扬声器在每个分句开头被推一下、负向余量只用一半、软限幅只压正峰。非有限值按 0 处理，避免污染滤波状态。
fn remove_dc(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    if sample_rate == 0 {
        return samples.to_vec();
    }
    let r = (-2.0 * std::f32::consts::PI * TTS_DC_BLOCK_HZ / sample_rate as f32).exp();
    let (mut prev_x, mut prev_y) = (0.0f32, 0.0f32);
    samples
        .iter()
        .map(|&sample| {
            let x = if sample.is_finite() { sample } else { 0.0 };
            let y = x - prev_x + r * prev_y;
            (prev_x, prev_y) = (x, y);
            y
        })
        .collect()
}

/// 合成模型浮点输出先隔直再转 16-bit PCM。
fn tts_pcm(samples: &[f32], sample_rate: u32) -> Vec<i16> {
    to_i16(&remove_dc(samples, sample_rate))
}

/// 平滑本地流式子分段拼接边缘：非末段裁除模型尾部超长死寂（>220ms 裁至 130ms 自然换气）并做 6ms 淡出，
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
    const SUBSEG_SILENCE_TRIGGER_MS: u64 = 220;
    const SUBSEG_SILENCE_KEEP_MS: u64 = 130;
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

/// 中文数字、日期、电话号码的读法规则（模型目录里有就用）。Kokoro 包里带 `-zh` 后缀；
/// 中文 VITS 用 sherpa 官方中文 VITS 包里不带后缀的那份，只对它启用，免得其他模型包里同名文件悄悄改了读法。
fn rule_fsts(dir: &Path, engine: TtsEngine) -> Option<String> {
    let names: &[&str] = if matches!(engine, TtsEngine::Vits) {
        &["phone.fst", "date.fst", "number.fst"]
    } else {
        &["phone-zh.fst", "date-zh.fst", "number-zh.fst"]
    };
    let found: Vec<String> = names
        .iter()
        .map(|name| dir.join(name))
        .filter(|path| path.is_file())
        .map(|path| native_path_string(&path))
        .collect();
    (!found.is_empty()).then(|| found.join(","))
}

#[derive(Default)]
struct DenoiserSlot {
    generation: u64,
    model_path: Option<String>,
}

#[derive(Default)]
struct Loaded {
    vad: AtomicBool,
    asr: AtomicBool,
    tts: AtomicBool,
    denoiser: AtomicBool,
    smart_turn: AtomicBool,
    kws: AtomicBool,
    denoiser_slot: Mutex<DenoiserSlot>,
    synth: SynthCancel,
}

/// 降噪模型按文件名区分：`dpdfnet*.onnx` 走 DPDFNet（sherpa-onnx 1.12.30 起支持，16 kHz 版本），其余按 GTCRN。
fn is_dpdfnet(model_path: &str) -> bool {
    Path::new(model_path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().starts_with("dpdfnet"))
}

fn denoiser_model_config(model_path: &str) -> OfflineSpeechDenoiserModelConfig {
    let (gtcrn, dpdfnet) = if is_dpdfnet(model_path) {
        (
            OfflineSpeechDenoiserGtcrnModelConfig::default(),
            OfflineSpeechDenoiserDpdfNetModelConfig {
                model: Some(model_path.to_owned()),
                ..Default::default()
            },
        )
    } else {
        (
            OfflineSpeechDenoiserGtcrnModelConfig {
                model: Some(model_path.to_owned()),
            },
            OfflineSpeechDenoiserDpdfNetModelConfig::default(),
        )
    };
    OfflineSpeechDenoiserModelConfig {
        gtcrn,
        dpdfnet,
        num_threads: 1,
        debug: false,
        provider: Some("cpu".into()),
    }
}

fn create_online_denoiser(model_path: &str) -> Option<OnlineSpeechDenoiser> {
    OnlineSpeechDenoiser::create(&OnlineSpeechDenoiserConfig {
        model: denoiser_model_config(model_path),
    })
}

fn create_offline_denoiser(model_path: &str) -> Option<OfflineSpeechDenoiser> {
    OfflineSpeechDenoiser::create(&OfflineSpeechDenoiserConfig {
        model: denoiser_model_config(model_path),
    })
}

/// 一路判停 / 识别流打开时的降噪情况（协议 8，n110）：如实说明这一路音频有没有先经降噪。
/// sherpa-onnx 建降噪器失败时只返回空、不给原因，这里照实写“没有返回原因”。
fn denoise_report(model_path: Option<&str>, created: bool) -> DenoiseReport {
    let Some(path) = model_path else {
        return DenoiseReport::default();
    };
    DenoiseReport {
        applied: created,
        model: Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        engine: if is_dpdfnet(path) { "dpdfnet" } else { "gtcrn" }.into(),
        error: (!created)
            .then(|| "降噪器没建起来（sherpa-onnx 没有返回原因），这一路改用原始音频".to_owned()),
    }
}

fn current_denoiser_spec(loaded: &Loaded) -> (u64, Option<String>) {
    loaded
        .denoiser_slot
        .lock()
        .map(|slot| (slot.generation, slot.model_path.clone()))
        .unwrap_or((0, None))
}

fn denoise_online_chunk<'a>(
    stream_denoiser: &mut Option<(u64, OnlineSpeechDenoiser)>,
    loaded: &Loaded,
    samples: &'a [f32],
) -> Cow<'a, [f32]> {
    let (generation, model_path) = current_denoiser_spec(loaded);
    let needs_sync = match (stream_denoiser.as_ref(), model_path.as_ref()) {
        (Some((g, _)), Some(_)) => *g != generation,
        (None, Some(_)) | (Some(_), None) => true,
        (None, None) => false,
    };
    if needs_sync {
        *stream_denoiser = model_path
            .as_deref()
            .and_then(create_online_denoiser)
            .map(|d| (generation, d));
    }
    if let Some((_, denoiser)) = stream_denoiser.as_ref() {
        Cow::Owned(denoiser.run(samples, 16_000).samples)
    } else {
        Cow::Borrowed(samples)
    }
}

fn flush_online_denoiser(stream_denoiser: &mut Option<(u64, OnlineSpeechDenoiser)>) -> Vec<f32> {
    match stream_denoiser.take() {
        Some((_, denoiser)) => denoiser.flush().samples,
        None => Vec::new(),
    }
}

/// 合成请求的取消状态：主线程收到 `Cancel` 时登记，合成线程开工前与合成过程中查看。
#[derive(Default)]
struct SynthCancel {
    /// （正在合成的请求编号，0 为空闲；已被取消但还在排队的请求编号）
    state: Mutex<(u64, std::collections::HashSet<u64>)>,
    /// 正在合成的那个请求被取消
    stop: Arc<AtomicBool>,
}

impl SynthCancel {
    /// 合成线程取到一个请求：已被取消则返回 `false`（不合成）。
    fn begin(&self, id: u64) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return true;
        };
        if state.1.remove(&id) {
            return false;
        }
        state.0 = id;
        self.stop.store(false, Ordering::SeqCst);
        true
    }

    fn end(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.0 = 0;
        }
    }

    fn cancel(&self, target: u64) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.0 == target {
            self.stop.store(true, Ordering::SeqCst);
        } else {
            // 目标可能已经合成完（登记不会再被取走）：封顶，不让集合无限增长
            if state.1.len() >= 1024 {
                state.1.clear();
            }
            state.1.insert(target);
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// 传给 sherpa 的进度回调：返回 `false` 即中止当前合成。
    fn keep_going(&self) -> impl FnMut(&[f32], f32) -> bool + 'static {
        let stop = self.stop.clone();
        move |_, _| !stop.load(Ordering::SeqCst)
    }
}

/// 模型自己生成的停顿（200 ms 以上的静音）保留多少。实测 Kokoro v1.1：sherpa 默认的 0.2 把逗号和句号
/// 都压到约 200 ms，分不出层次、听着不喘气；不压（1.0）时逗号 0.5–1 秒、句号约 1 秒，又太拖。
/// 取 0.35：逗号约 190–360 ms，句号约 350 ms，句尾约 210–260 ms。
const TTS_SILENCE_SCALE: f32 = 0.35;

/// Kokoro 在一段话很长时会越说越赶（官方 v1.1-zh 示例脚本用按长度放慢来缓解）：
/// 28 字以内不变，之后每多一个字放慢 0.6%，最多放慢到 0.82 倍。
fn long_segment_speed(chars: usize) -> f32 {
    (1.0 - chars.saturating_sub(28) as f32 * 0.006).max(0.82)
}

/// 推理线程数：默认取逻辑核数的一半，限制在 2–6。实测 Kokoro v1.1 int8（12 逻辑核 Mac）
/// 2 / 4 / 6 线程的实时率为 0.78 / 0.61 / 0.54；再多收益很小，还会和界面抢核。
/// 环境变量 `MOSS_VOICE_THREADS`（1–16）可覆盖，用于在具体机器上调优。
fn inference_threads() -> i32 {
    if let Some(value) = std::env::var("MOSS_VOICE_THREADS")
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok())
        .filter(|value| (1..=16).contains(value))
    {
        return value;
    }
    let cores = thread::available_parallelism().map_or(4, |n| n.get());
    (cores / 2).clamp(2, 6) as i32
}

// ---------------------------------------------------------------- 判停

struct VadStream {
    detector: VoiceActivityDetector,
    in_speech: bool,
    denoiser: Option<(u64, OnlineSpeechDenoiser)>,
    /// 命令词检测流（协议 7）：与它所属的检测器同一代，模型重新加载后按需重建
    kws: Option<(u64, OnlineStream)>,
}

/// 命令词检测模型（sherpa-onnx 关键词检测 zipformer）：在判停线程里与判停流同步运行。
struct Spotter {
    generation: u64,
    spotter: KeywordSpotter,
}

fn load_spotter(dir: &str, keywords: &str) -> Result<KeywordSpotter, String> {
    let dir = require(Path::new(dir))?;
    let dir = Path::new(&dir);
    let mut config = KeywordSpotterConfig::default();
    config.model_config.transducer = OnlineTransducerModelConfig {
        encoder: Some(first_of(
            dir,
            &[
                "encoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx",
                "encoder.int8.onnx",
                "encoder.onnx",
            ],
        )?),
        decoder: Some(first_of(
            dir,
            &[
                "decoder-epoch-12-avg-2-chunk-16-left-64.onnx",
                "decoder.onnx",
            ],
        )?),
        joiner: Some(first_of(
            dir,
            &[
                "joiner-epoch-12-avg-2-chunk-16-left-64.int8.onnx",
                "joiner.int8.onnx",
                "joiner.onnx",
            ],
        )?),
    };
    config.model_config.tokens = Some(require(&dir.join("tokens.txt"))?);
    config.model_config.provider = Some("cpu".into());
    config.model_config.num_threads = 1;
    config.model_config.debug = false;
    config.keywords_buf = Some(keywords.to_owned());
    KeywordSpotter::create(&config).ok_or_else(|| "命令词检测模型加载失败".to_owned())
}

/// 把一段 16 kHz 原始音频送进命令词检测流，返回检测到的词（检测到一次就重置，避免同一个词连报）。
fn spot_keywords(spotter: &KeywordSpotter, stream: &OnlineStream, raw: &[f32]) -> Vec<String> {
    stream.accept_waveform(16_000, raw);
    let mut found = Vec::new();
    while spotter.is_ready(stream) {
        spotter.decode(stream);
        if let Some(result) = spotter.get_result(stream)
            && !result.keyword.is_empty()
        {
            found.push(result.keyword);
            spotter.reset(stream);
        }
    }
    found
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
    let mut spotter: Option<Spotter> = None;
    let mut spotter_generation = 0u64;
    for (id, op, payload) in jobs {
        match op {
            Op::LoadKws { dir, keywords } => {
                // 旧模型建的检测流先全部释放，再换模型
                for state in streams.values_mut() {
                    state.kws = None;
                }
                spotter = None;
                loaded.kws.store(false, Ordering::SeqCst);
                if dir.is_empty() {
                    send(
                        &out,
                        id,
                        Reply::Loaded {
                            sample_rate: None,
                            speakers: None,
                        },
                        &[],
                    );
                    continue;
                }
                match load_spotter(&dir, &keywords) {
                    Ok(created) => {
                        spotter_generation += 1;
                        spotter = Some(Spotter {
                            generation: spotter_generation,
                            spotter: created,
                        });
                        loaded.kws.store(true, Ordering::SeqCst);
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
                    let (generation, model_path) = current_denoiser_spec(&loaded);
                    let denoiser = model_path
                        .as_deref()
                        .and_then(create_online_denoiser)
                        .map(|d| (generation, d));
                    let denoise = denoise_report(model_path.as_deref(), denoiser.is_some());
                    streams.insert(
                        stream,
                        VadStream {
                            detector,
                            in_speech: false,
                            denoiser,
                            kws: None,
                        },
                    );
                    send(&out, id, Reply::Opened { denoise }, &[]);
                }
                None => error(&out, id, "判停模型未加载"),
            },
            Op::VadFeed { stream } => {
                let Some(state) = streams.get_mut(&stream) else {
                    error(&out, id, "判停流不存在");
                    continue;
                };
                let raw = to_f32(&bytes_to_pcm(&payload));
                // 命令词检测吃原始音频（实测降噪会让短词更难认）；检测流按需建、模型换代后重建
                let keywords = match spotter.as_ref() {
                    Some(current) => {
                        if state
                            .kws
                            .as_ref()
                            .is_none_or(|(generation, _)| *generation != current.generation)
                        {
                            state.kws = Some((current.generation, current.spotter.create_stream()));
                        }
                        state
                            .kws
                            .as_ref()
                            .map(|(_, kws)| spot_keywords(&current.spotter, kws, &raw))
                            .unwrap_or_default()
                    }
                    None => Vec::new(),
                };
                let denoised = denoise_online_chunk(&mut state.denoiser, &loaded, &raw);
                state.detector.accept_waveform(&denoised);
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
                send(&out, id, Reply::VadEvents { events, keywords }, &[]);
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
                // 固定中文：auto 会把短句误判成粤语 / 日语 / 英语；中文模式下夹杂的英文单词仍能识别
                language: Some("zh".into()),
                use_itn: true,
            };
            config.model_config.tokens = Some(require(&dir.join("tokens.txt"))?);
            config.model_config.provider = Some("cpu".into());
            config.model_config.num_threads = inference_threads();
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
            config.model_config.num_threads = inference_threads();
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
        AsrEngine::Qwen3Asr => {
            let mut config = OfflineRecognizerConfig::default();
            config.model_config.qwen3_asr = OfflineQwen3ASRModelConfig {
                conv_frontend: Some(require(&dir.join("conv_frontend.onnx"))?),
                encoder: Some(first_of(dir, &["encoder.int8.onnx", "encoder.onnx"])?),
                decoder: Some(first_of(dir, &["decoder.int8.onnx", "decoder.onnx"])?),
                tokenizer: Some(require(&dir.join("tokenizer"))?),
                hotwords: (!options.hotwords.is_empty()).then(|| options.hotwords.join(",")),
                ..Default::default()
            };
            config.model_config.tokens = Some(String::new());
            config.model_config.provider = Some("cpu".into());
            config.model_config.num_threads = inference_threads();
            config.model_config.debug = false;
            OfflineRecognizer::create(&config)
                .map(Recognizer::Offline)
                .ok_or_else(|| "Qwen3-ASR 模型加载失败".to_owned())
        }
        AsrEngine::FireRedAsr2Ctc => {
            let mut config = OfflineRecognizerConfig::default();
            config.model_config.fire_red_asr_ctc = OfflineFireRedAsrCtcModelConfig {
                model: Some(first_of(dir, &["model.int8.onnx", "model.onnx"])?),
            };
            config.model_config.tokens = Some(require(&dir.join("tokens.txt"))?);
            config.model_config.provider = Some("cpu".into());
            config.model_config.num_threads = inference_threads();
            config.model_config.debug = false;
            OfflineRecognizer::create(&config)
                .map(Recognizer::Offline)
                .ok_or_else(|| "FireRedASR2-CTC 模型加载失败".to_owned())
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

/// 离线识别一次整段采样并取裁剪后的文字。
fn decode_offline(recognizer: &OfflineRecognizer, sample_rate: i32, samples: &[f32]) -> String {
    let stream = recognizer.create_stream();
    stream.accept_waveform(sample_rate, samples);
    recognizer.decode(&stream);
    stream
        .get_result()
        .map(|result| result.text.trim().to_owned())
        .unwrap_or_default()
}

const OFFLINE_ASR_SAMPLE_RATE: i32 = 16_000;
/// 离线模型流式会话首次触发试认的最短采样数（320 ms @ 16 kHz，兼顾单字“停”/“急停”）。
const OFFLINE_INTERIM_FIRST_SAMPLES: usize = 16_000 * 8 / 25;
/// 离线模型流式会话后续周期重认步长（500 ms @ 16 kHz，说话期间约每半秒重认一次已录片段）。
const OFFLINE_INTERIM_STEP_SAMPLES: usize = 16_000 / 2;
/// 单路离线流式缓冲滑窗上限（30 秒 @ 16 kHz），限制内存与单次重认耗时。
const OFFLINE_STREAM_MAX_SAMPLES: usize = 16_000 * 30;
/// 同时打开的识别流上限，防止异常未关闭流无限累积。
const MAX_ACTIVE_ASR_STREAMS: usize = 64;

/// 离线模型（SenseVoice）的伪流式增量重认缓冲：
/// 说话期间按约 0.5 秒步长重认当前滑窗，不足步长时快速返回 `None`（对应 `Reply::Done`）。
#[derive(Default)]
struct OfflineAsrBuffer {
    samples: Vec<f32>,
    pending_samples: usize,
    /// 已至少产出过一次非空识别文字：此后切入 500ms 稳态重认步长。
    /// 自动拾音起音前若带静音 preroll（如 1s 静音 + 60ms 辅音头）导致首轮试认出空串，
    /// 保持 320ms 首认步长不退化为 500ms，确保真实开口后约 320ms 即出首屏字或命中急停。
    has_nonempty_decode: bool,
    /// `last_text` 是否已对应当前完整 `samples`（无新增尾音时 `AsrFinish` 直接复用）。
    has_clean_decode: bool,
    last_text: String,
}

impl OfflineAsrBuffer {
    fn feed_with(
        &mut self,
        incoming: &[f32],
        mut decode: impl FnMut(&[f32]) -> String,
    ) -> Option<String> {
        if !incoming.is_empty() {
            self.samples.extend_from_slice(incoming);
            if self.samples.len() > OFFLINE_STREAM_MAX_SAMPLES {
                let drop_count = self.samples.len() - OFFLINE_STREAM_MAX_SAMPLES;
                self.samples.drain(..drop_count);
            }
            self.pending_samples = self.pending_samples.saturating_add(incoming.len());
            self.has_clean_decode = false;
        }
        let step = if self.has_nonempty_decode {
            OFFLINE_INTERIM_STEP_SAMPLES
        } else {
            OFFLINE_INTERIM_FIRST_SAMPLES
        };
        if self.pending_samples < step {
            return None;
        }
        let text = decode(&self.samples);
        if !text.trim().is_empty() {
            self.has_nonempty_decode = true;
        }
        self.last_text.clone_from(&text);
        self.pending_samples = 0;
        self.has_clean_decode = true;
        Some(text)
    }

    fn push_tail(&mut self, tail: &[f32]) {
        if tail.is_empty() {
            return;
        }
        self.samples.extend_from_slice(tail);
        if self.samples.len() > OFFLINE_STREAM_MAX_SAMPLES {
            let drop_count = self.samples.len() - OFFLINE_STREAM_MAX_SAMPLES;
            self.samples.drain(..drop_count);
        }
        self.pending_samples = self.pending_samples.saturating_add(tail.len());
        self.has_clean_decode = false;
    }

    fn finish_with(self, mut decode: impl FnMut(&[f32]) -> String) -> String {
        if self.samples.is_empty() {
            return String::new();
        }
        if self.has_clean_decode && self.pending_samples == 0 {
            return self.last_text;
        }
        decode(&self.samples)
    }
}

enum ActiveAsrStreamKind {
    Online(OnlineStream),
    Offline(OfflineAsrBuffer),
}

struct ActiveAsrStream {
    kind: ActiveAsrStreamKind,
    denoiser: Option<(u64, OnlineSpeechDenoiser)>,
}

fn insert_asr_stream(streams: &mut HashMap<u64, ActiveAsrStream>, id: u64, entry: ActiveAsrStream) {
    if streams.len() >= MAX_ACTIVE_ASR_STREAMS
        && !streams.contains_key(&id)
        && let Some(&oldest) = streams.keys().min()
    {
        streams.remove(&oldest);
    }
    streams.insert(id, entry);
}

fn denoise_offline_pcm<'a>(
    offline_denoiser: &mut Option<(u64, OfflineSpeechDenoiser)>,
    loaded: &Loaded,
    samples: &'a [f32],
    sample_rate: u32,
) -> (Cow<'a, [f32]>, i32) {
    let (generation, model_path) = current_denoiser_spec(loaded);
    let needs_sync = match (offline_denoiser.as_ref(), model_path.as_ref()) {
        (Some((g, _)), Some(_)) => *g != generation,
        (None, Some(_)) | (Some(_), None) => true,
        (None, None) => false,
    };
    if needs_sync {
        *offline_denoiser = model_path
            .as_deref()
            .and_then(create_offline_denoiser)
            .map(|d| (generation, d));
    }
    if let Some((_, denoiser)) = offline_denoiser.as_ref() {
        let out = denoiser.run(samples, sample_rate as i32);
        let rate = if out.sample_rate > 0 {
            out.sample_rate
        } else {
            OFFLINE_ASR_SAMPLE_RATE
        };
        (Cow::Owned(out.samples), rate)
    } else {
        (Cow::Borrowed(samples), sample_rate as i32)
    }
}

fn asr_worker(jobs: Receiver<Job>, out: Out, loaded: Arc<Loaded>) {
    let mut recognizer: Option<Recognizer> = None;
    let mut streams: HashMap<u64, ActiveAsrStream> = HashMap::new();
    let mut offline_denoiser: Option<(u64, OfflineSpeechDenoiser)> = None;
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
                let raw = to_f32(&bytes_to_pcm(&payload));
                let (samples, rate) =
                    denoise_offline_pcm(&mut offline_denoiser, &loaded, &raw, sample_rate);
                let text = match recognizer.as_ref() {
                    None => {
                        error(&out, id, "识别模型未加载");
                        continue;
                    }
                    Some(Recognizer::Offline(recognizer)) => {
                        decode_offline(recognizer, rate, &samples)
                    }
                    Some(Recognizer::Online(recognizer)) => {
                        let stream = recognizer.create_stream();
                        stream.accept_waveform(rate, &samples);
                        finish_online(recognizer, &stream)
                    }
                };
                send(&out, id, Reply::Text { text }, &[]);
            }
            Op::AsrOpen { stream } => {
                let kind = match recognizer.as_ref() {
                    Some(Recognizer::Online(recognizer)) => {
                        ActiveAsrStreamKind::Online(recognizer.create_stream())
                    }
                    Some(Recognizer::Offline(_)) => {
                        ActiveAsrStreamKind::Offline(OfflineAsrBuffer::default())
                    }
                    None => {
                        error(&out, id, "识别模型未加载");
                        continue;
                    }
                };
                let (generation, model_path) = current_denoiser_spec(&loaded);
                let denoiser = model_path
                    .as_deref()
                    .and_then(create_online_denoiser)
                    .map(|d| (generation, d));
                let denoise = denoise_report(model_path.as_deref(), denoiser.is_some());
                insert_asr_stream(&mut streams, stream, ActiveAsrStream { kind, denoiser });
                send(&out, id, Reply::Opened { denoise }, &[]);
            }
            Op::AsrFeed { stream } => match (recognizer.as_ref(), streams.get_mut(&stream)) {
                (
                    Some(Recognizer::Online(recognizer)),
                    Some(ActiveAsrStream {
                        kind: ActiveAsrStreamKind::Online(online),
                        denoiser,
                    }),
                ) => {
                    let raw = to_f32(&bytes_to_pcm(&payload));
                    let denoised = denoise_online_chunk(denoiser, &loaded, &raw);
                    if !denoised.is_empty() {
                        online.accept_waveform(OFFLINE_ASR_SAMPLE_RATE, &denoised);
                        decode_ready(recognizer, online);
                    }
                    let text = recognizer
                        .get_result(online)
                        .map(|result| result.text.trim().to_owned())
                        .unwrap_or_default();
                    send(&out, id, Reply::Text { text }, &[]);
                }
                (
                    Some(Recognizer::Offline(recognizer)),
                    Some(ActiveAsrStream {
                        kind: ActiveAsrStreamKind::Offline(buffer),
                        denoiser,
                    }),
                ) => {
                    let raw = to_f32(&bytes_to_pcm(&payload));
                    let denoised = denoise_online_chunk(denoiser, &loaded, &raw);
                    match buffer.feed_with(&denoised, |samples| {
                        decode_offline(recognizer, OFFLINE_ASR_SAMPLE_RATE, samples)
                    }) {
                        Some(text) => send(&out, id, Reply::Text { text }, &[]),
                        None => send(&out, id, Reply::Done, &[]),
                    }
                }
                _ => error(&out, id, "识别流不存在"),
            },
            Op::AsrFinish { stream } => match (recognizer.as_ref(), streams.remove(&stream)) {
                (
                    Some(Recognizer::Online(recognizer)),
                    Some(ActiveAsrStream {
                        kind: ActiveAsrStreamKind::Online(online),
                        mut denoiser,
                    }),
                ) => {
                    let flushed = flush_online_denoiser(&mut denoiser);
                    if !flushed.is_empty() {
                        online.accept_waveform(OFFLINE_ASR_SAMPLE_RATE, &flushed);
                    }
                    let text = finish_online(recognizer, &online);
                    send(&out, id, Reply::Text { text }, &[]);
                }
                (
                    Some(Recognizer::Offline(recognizer)),
                    Some(ActiveAsrStream {
                        kind: ActiveAsrStreamKind::Offline(mut buffer),
                        mut denoiser,
                    }),
                ) => {
                    let flushed = flush_online_denoiser(&mut denoiser);
                    buffer.push_tail(&flushed);
                    let text = buffer.finish_with(|samples| {
                        decode_offline(recognizer, OFFLINE_ASR_SAMPLE_RATE, samples)
                    });
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

#[derive(Clone)]
struct ZipVoiceRef {
    samples: Vec<f32>,
    sample_rate: i32,
    prompt_text: String,
}

struct TtsRuntime {
    tts: OfflineTts,
    speakers: i32,
    kokoro: bool,
    /// 中文 VITS：送进模型前先整理文字并在词间插入空格标记（[`vits_zh::normalize_text`] / [`vits_zh::insert_word_boundaries`]）
    vits_words: Option<std::collections::HashSet<String>>,
    zipvoice_refs: Option<Vec<ZipVoiceRef>>,
}

fn load_zipvoice_refs(dir: &Path) -> Result<Vec<ZipVoiceRef>, String> {
    const SPECS: &[(&str, &str)] = &[
        (
            "test_wavs/news-female.wav",
            "各位村民, 大家新年好! 近期, 湖北省武汉市等多个地区",
        ),
        (
            "test_wavs/news-female-2.wav",
            "本台消息, 中共中央国务院, 近日印发关于构建数据基础制度, 更好发挥数据要素作用的意见.",
        ),
    ];
    let mut out = Vec::with_capacity(SPECS.len());
    for &(rel_path, prompt_text) in SPECS {
        let wav_path = require(&dir.join(rel_path))?;
        let wave = Wave::read(&wav_path).ok_or_else(|| format!("参考音频读取失败：{rel_path}"))?;
        let samples = wave.samples().to_vec();
        let sample_rate = wave.sample_rate();
        if samples.is_empty() || sample_rate <= 0 {
            return Err(format!("参考音频内容无效：{rel_path}"));
        }
        out.push(ZipVoiceRef {
            samples,
            sample_rate,
            prompt_text: prompt_text.to_owned(),
        });
    }
    Ok(out)
}

fn build_tts_gen_config(runtime: &TtsRuntime, speaker: i32, speed: f32) -> GenerationConfig {
    let clamped_speed = speed.clamp(0.25, 4.0);
    if let Some(refs) = runtime.zipvoice_refs.as_deref() {
        let idx = (speaker.max(0) as usize).min(refs.len() - 1);
        let ref_voice = &refs[idx];
        return GenerationConfig {
            silence_scale: TTS_SILENCE_SCALE,
            speed: clamped_speed,
            sid: 0,
            reference_audio: Some(ref_voice.samples.clone()),
            reference_sample_rate: ref_voice.sample_rate,
            reference_text: Some(ref_voice.prompt_text.clone()),
            num_steps: 4,
            extra: Some(HashMap::from([(
                "min_char_in_sentence".to_owned(),
                serde_json::json!(10),
            )])),
        };
    }
    // 单音色模型忽略音色编号
    let sid = if runtime.speakers > 1 {
        speaker.clamp(0, runtime.speakers - 1)
    } else {
        0
    };
    GenerationConfig {
        sid,
        speed: clamped_speed,
        silence_scale: TTS_SILENCE_SCALE,
        ..Default::default()
    }
}

#[cfg(test)]
fn tts_config(dir: &Path, engine: TtsEngine) -> Result<OfflineTtsConfig, String> {
    tts_config_and_vits_words(dir, engine).map(|(cfg, _)| cfg)
}

fn tts_config_and_vits_words(
    dir: &Path,
    engine: TtsEngine,
) -> Result<(OfflineTtsConfig, Option<std::collections::HashSet<String>>), String> {
    let mut vits_words = None;
    let model = match engine {
        TtsEngine::Kokoro => {
            let en_lexicon = require(&dir.join("lexicon-us-en.txt"))?;
            let raw_zh_lexicon = require(&dir.join("lexicon-zh.txt"))?;
            let raw_tokens = require(&dir.join("tokens.txt"))?;
            let (tokens_path, zh_lexicon) = kokoro_prosody::prepare_kokoro_prosody_files(dir)
                .map(|(t, l)| (native_path_string(&t), native_path_string(&l)))
                .unwrap_or((raw_tokens, raw_zh_lexicon));
            let lexicon = format!("{en_lexicon},{zh_lexicon}");
            OfflineTtsModelConfig {
                kokoro: OfflineTtsKokoroModelConfig {
                    model: Some(first_of(dir, &["model.onnx", "model.int8.onnx"])?),
                    voices: Some(require(&dir.join("voices.bin"))?),
                    tokens: Some(tokens_path),
                    data_dir: Some(require(&dir.join("espeak-ng-data"))?),
                    dict_dir: Some(require(&dir.join("dict"))?),
                    lexicon: Some(lexicon),
                    length_scale: 1.0,
                    ..Default::default()
                },
                num_threads: inference_threads(),
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
            num_threads: inference_threads(),
            debug: false,
            ..Default::default()
        },
        TtsEngine::ZipVoice => OfflineTtsModelConfig {
            zipvoice: OfflineTtsZipvoiceModelConfig {
                tokens: Some(require(&dir.join("tokens.txt"))?),
                encoder: Some(first_of(dir, &["encoder.int8.onnx", "encoder.onnx"])?),
                decoder: Some(first_of(dir, &["decoder.int8.onnx", "decoder.onnx"])?),
                vocoder: Some(first_of(dir, &["vocos_24khz.onnx", "vocoder.onnx"])?),
                data_dir: Some(require(&dir.join("espeak-ng-data"))?),
                lexicon: Some(require(&dir.join("lexicon.txt"))?),
                feat_scale: 0.1,
                t_shift: 0.5,
                target_rms: 0.1,
                guidance_scale: 1.0,
            },
            num_threads: inference_threads(),
            debug: false,
            ..Default::default()
        },
        TtsEngine::Vits => {
            let files = vits_zh::prepare_vits_files(dir)?;
            let model_cfg = OfflineTtsModelConfig {
                vits: OfflineTtsVitsModelConfig {
                    model: Some(native_path_string(&files.model)),
                    lexicon: Some(native_path_string(&files.lexicon)),
                    tokens: Some(native_path_string(&files.tokens)),
                    // 小样本微调 VITS 降低随机波形/时长噪声（0.667/0.8 -> 0.5/0.5），减少字音随机飘移
                    noise_scale: 0.5,
                    noise_scale_w: 0.5,
                    length_scale: 1.0,
                    ..Default::default()
                },
                num_threads: inference_threads(),
                debug: false,
                ..Default::default()
            };
            vits_words = Some(files.words);
            model_cfg
        }
    };
    Ok((
        OfflineTtsConfig {
            model,
            rule_fsts: rule_fsts(dir, engine),
            ..Default::default()
        },
        vits_words,
    ))
}

fn generate_vits_sentences(
    rt: &TtsRuntime,
    words: &std::collections::HashSet<String>,
    normalized_text: &str,
    config: &GenerationConfig,
    loaded: &Loaded,
) -> Option<(u32, Vec<f32>)> {
    let mut all_samples = Vec::new();
    let mut sample_rate = rt.tts.sample_rate().max(16_000) as u32;
    for sentence in vits_zh::split_sentences(normalized_text) {
        let segmented = vits_zh::insert_word_boundaries(&sentence, words);
        if !vits_zh::has_speakable_content(&segmented) {
            continue;
        }
        let audio =
            rt.tts
                .generate_with_config(&segmented, config, Some(loaded.synth.keep_going()))?;
        if loaded.synth.stopped() {
            return None;
        }
        let sr = audio.sample_rate().max(0) as u32;
        if sr > 0 {
            sample_rate = sr;
        }
        if !all_samples.is_empty() && !audio.samples().is_empty() && sample_rate > 0 {
            all_samples.resize(all_samples.len() + (sample_rate as usize * 200) / 1000, 0.0);
        }
        all_samples.extend_from_slice(audio.samples());
    }
    Some((sample_rate, all_samples))
}

fn tts_worker(jobs: Receiver<Job>, out: Out, loaded: Arc<Loaded>) {
    let mut runtime: Option<TtsRuntime> = None;
    for (id, op, _) in jobs {
        match op {
            Op::LoadTts { dir, engine } => {
                let dir_path = Path::new(&dir);
                let created = (|| -> Result<TtsRuntime, String> {
                    let (config, vits_words) = tts_config_and_vits_words(dir_path, engine)?;
                    let zipvoice_refs = if matches!(engine, TtsEngine::ZipVoice) {
                        Some(load_zipvoice_refs(dir_path)?)
                    } else {
                        None
                    };
                    let name = match engine {
                        TtsEngine::Kokoro => "Kokoro",
                        TtsEngine::Matcha => "Matcha",
                        TtsEngine::ZipVoice => "ZipVoice",
                        TtsEngine::Vits => "MOSS 原声（VITS）",
                    };
                    let created = OfflineTts::create(&config)
                        .ok_or_else(|| format!("{name} 模型加载失败"))?;
                    let speakers = created.num_speakers().max(0);
                    Ok(TtsRuntime {
                        tts: created,
                        speakers,
                        kokoro: matches!(engine, TtsEngine::Kokoro),
                        vits_words,
                        zipvoice_refs,
                    })
                })();
                match created {
                    Ok(rt) => {
                        let sample_rate = rt.tts.sample_rate().max(0) as u32;
                        let speakers = rt
                            .zipvoice_refs
                            .as_ref()
                            .map_or(rt.speakers.max(0) as u32, |refs| refs.len() as u32);
                        // 预热：第一次推理要做图优化与内存分配，明显慢于之后；放在加载阶段做掉，
                        // 不让它落在用户听到的第一句话上
                        let warmup_cfg = build_tts_gen_config(&rt, 0, 1.0);
                        let warmup_text = rt.vits_words.as_ref().map_or_else(
                            || "好的。".to_owned(),
                            |words| vits_zh::insert_word_boundaries("好的。", words),
                        );
                        let _ = rt.tts.generate_with_config(
                            &warmup_text,
                            &warmup_cfg,
                            None::<fn(&[f32], f32) -> bool>,
                        );
                        runtime = Some(rt);
                        loaded.tts.store(true, Ordering::SeqCst);
                        send(
                            &out,
                            id,
                            Reply::Loaded {
                                sample_rate: Some(sample_rate),
                                speakers: Some(speakers),
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
                let Some(rt) = runtime.as_ref() else {
                    error(&out, id, "合成模型未加载");
                    continue;
                };
                let config = build_tts_gen_config(rt, speaker, speed);
                if !loaded.synth.begin(id) {
                    error(&out, id, "合成已取消");
                    continue;
                }
                if let Some(words) = rt.vits_words.as_ref() {
                    let normalized = vits_zh::normalize_text(&text);
                    let generated =
                        generate_vits_sentences(rt, words, &normalized, &config, &loaded);
                    let cancelled = loaded.synth.stopped();
                    loaded.synth.end();
                    match generated {
                        _ if cancelled => error(&out, id, "合成已取消"),
                        Some((sample_rate, samples)) if !samples.is_empty() => {
                            let pcm = tts_pcm(&samples, sample_rate);
                            send(&out, id, Reply::Audio { sample_rate }, &pcm_to_bytes(&pcm));
                        }
                        _ => error(&out, id, "合成失败"),
                    }
                } else {
                    let generated = rt.tts.generate_with_config(
                        &text,
                        &config,
                        Some(loaded.synth.keep_going()),
                    );
                    let cancelled = loaded.synth.stopped();
                    loaded.synth.end();
                    match generated {
                        _ if cancelled => error(&out, id, "合成已取消"),
                        Some(audio) if !audio.samples().is_empty() => {
                            let sample_rate = audio.sample_rate().max(0) as u32;
                            let pcm = tts_pcm(audio.samples(), sample_rate);
                            send(&out, id, Reply::Audio { sample_rate }, &pcm_to_bytes(&pcm));
                        }
                        _ => error(&out, id, "合成失败"),
                    }
                }
            }
            Op::SynthesizeStream {
                text,
                speaker,
                speed,
                first_max_chars,
                short_opener,
            } => {
                let Some(rt) = runtime.as_ref() else {
                    error(&out, id, "合成模型未加载");
                    continue;
                };
                let config = build_tts_gen_config(rt, speaker, speed);
                if !loaded.synth.begin(id) {
                    error(&out, id, "合成已取消");
                    continue;
                }
                let mut failed = false;
                let text = if rt.vits_words.is_some() {
                    vits_zh::normalize_text(&text)
                } else {
                    text
                };
                let mut segments = moss_voice_local_proto::synthesis_segments_with(
                    &text,
                    first_max_chars,
                    short_opener,
                );
                if let Some(words) = rt.vits_words.as_ref() {
                    segments.retain(|seg| {
                        vits_zh::split_sentences(seg).iter().any(|s| {
                            vits_zh::has_speakable_content(&vits_zh::insert_word_boundaries(
                                s, words,
                            ))
                        })
                    });
                }
                let total = segments.len();
                let mut emitted_chunks = 0usize;
                for (idx, segment) in segments.into_iter().enumerate() {
                    let mut config = config.clone();
                    if rt.kokoro {
                        config.speed *= long_segment_speed(segment.chars().count());
                    }
                    let generated_samples = if let Some(words) = rt.vits_words.as_ref() {
                        generate_vits_sentences(rt, words, &segment, &config, &loaded)
                    } else {
                        rt.tts
                            .generate_with_config(
                                &segment,
                                &config,
                                Some(loaded.synth.keep_going()),
                            )
                            .map(|audio| {
                                (audio.sample_rate().max(0) as u32, audio.samples().to_vec())
                            })
                    };
                    if loaded.synth.stopped() {
                        error(&out, id, "合成已取消");
                        failed = true;
                        break;
                    }
                    match generated_samples {
                        Some((_, samples)) if samples.is_empty() => continue,
                        Some((sample_rate, samples)) => {
                            let mut pcm = tts_pcm(&samples, sample_rate);
                            if total > 1 {
                                smooth_stream_subsegment(
                                    &mut pcm,
                                    sample_rate,
                                    emitted_chunks == 0,
                                    idx + 1 == total,
                                );
                            }
                            emitted_chunks += 1;
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
                loaded.synth.end();
                if !failed {
                    if emitted_chunks == 0 {
                        error(&out, id, "分段合成失败");
                    } else {
                        send(&out, id, Reply::Done, &[]);
                    }
                }
            }
            _ => error(&out, id, "合成线程收到不支持的请求"),
        }
    }
}

fn aux_worker(jobs: Receiver<Job>, out: Out, loaded: Arc<Loaded>) {
    let mut smart_turn: Option<smart_turn::SmartTurnModel> = None;
    for (id, op, payload) in jobs {
        match op {
            Op::LoadDenoiser { model } => {
                let trimmed = model.trim();
                if trimmed.is_empty() {
                    if let Ok(mut slot) = loaded.denoiser_slot.lock() {
                        slot.generation = slot.generation.wrapping_add(1);
                        slot.model_path = None;
                    }
                    loaded.denoiser.store(false, Ordering::SeqCst);
                    send(
                        &out,
                        id,
                        Reply::Loaded {
                            sample_rate: None,
                            speakers: None,
                        },
                        &[],
                    );
                    continue;
                }
                let candidate = Path::new(trimmed);
                let resolved = if candidate.is_dir() {
                    require(&candidate.join("gtcrn_simple.onnx"))
                } else {
                    require(candidate)
                };
                match resolved.and_then(|clean_path| {
                    let online = create_online_denoiser(&clean_path)
                        .ok_or_else(|| "GTCRN 在线降噪模型加载失败".to_owned())?;
                    if online.sample_rate() != 16_000 {
                        return Err(format!(
                            "降噪模型采样率不受支持：{} Hz",
                            online.sample_rate()
                        ));
                    }
                    let _offline = create_offline_denoiser(&clean_path)
                        .ok_or_else(|| "GTCRN 离线降噪模型加载失败".to_owned())?;
                    Ok(clean_path)
                }) {
                    Ok(clean_path) => {
                        if let Ok(mut slot) = loaded.denoiser_slot.lock() {
                            slot.generation = slot.generation.wrapping_add(1);
                            slot.model_path = Some(clean_path);
                        }
                        loaded.denoiser.store(true, Ordering::SeqCst);
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
            Op::LoadSmartTurn { model } => {
                let trimmed = model.trim();
                if trimmed.is_empty() {
                    smart_turn = None;
                    loaded.smart_turn.store(false, Ordering::SeqCst);
                    send(
                        &out,
                        id,
                        Reply::Loaded {
                            sample_rate: None,
                            speakers: None,
                        },
                        &[],
                    );
                    continue;
                }
                match smart_turn::SmartTurnModel::load(Path::new(trimmed)) {
                    Ok(model) => {
                        smart_turn = Some(model);
                        loaded.smart_turn.store(true, Ordering::SeqCst);
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
            Op::SmartTurnPredict { sample_rate } => {
                let Some(model) = smart_turn.as_ref() else {
                    error(&out, id, "尾音判停模型未加载");
                    continue;
                };
                if !payload.len().is_multiple_of(2) {
                    error(&out, id, "尾音判停音频字节长度无效");
                    continue;
                }
                let pcm = bytes_to_pcm(&payload);
                match model.predict(&pcm, sample_rate) {
                    Ok((probability, complete)) => {
                        send(
                            &out,
                            id,
                            Reply::SmartTurn {
                                probability,
                                complete,
                            },
                            &[],
                        );
                    }
                    Err(message) => error(&out, id, message),
                }
            }
            _ => error(&out, id, "辅助线程收到不支持的请求"),
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
    let aux = spawn_worker("aux", aux_worker, &out, &loaded);
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
            Op::Cancel { target } => {
                // 不进合成队列（队列里排在作废请求后面就没有意义了），也不回复
                loaded.synth.cancel(*target);
                continue;
            }
            Op::Hello => {
                let reply = Reply::Hello {
                    protocol: PROTOCOL_VERSION,
                    version: env!("CARGO_PKG_VERSION").into(),
                    vad: loaded.vad.load(Ordering::SeqCst),
                    asr: loaded.asr.load(Ordering::SeqCst),
                    tts: loaded.tts.load(Ordering::SeqCst),
                    denoiser: loaded.denoiser.load(Ordering::SeqCst),
                    smart_turn: loaded.smart_turn.load(Ordering::SeqCst),
                    kws: loaded.kws.load(Ordering::SeqCst),
                };
                send(&out, id, reply, &[]);
                continue;
            }
            Op::LoadVad { .. }
            | Op::LoadKws { .. }
            | Op::VadOpen { .. }
            | Op::VadFeed { .. }
            | Op::VadClose { .. } => &vad,
            Op::LoadAsr { .. }
            | Op::Transcribe { .. }
            | Op::AsrOpen { .. }
            | Op::AsrFeed { .. }
            | Op::AsrFinish { .. }
            | Op::AsrClose { .. } => &asr,
            Op::LoadTts { .. } | Op::Synthesize { .. } | Op::SynthesizeStream { .. } => &tts,
            Op::LoadDenoiser { .. } | Op::LoadSmartTurn { .. } | Op::SmartTurnPredict { .. } => {
                &aux
            }
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
    fn denoise_report_says_which_model_or_why_raw_audio_was_used() {
        assert_eq!(denoise_report(None, false), DenoiseReport::default());
        let applied = denoise_report(Some("<models>/denoise/gtcrn_simple.onnx"), true);
        assert!(applied.applied);
        assert_eq!(applied.model, "gtcrn_simple.onnx");
        assert_eq!(applied.engine, "gtcrn");
        assert_eq!(applied.error, None);
        let failed = denoise_report(Some("<models>/dpdfnet_baseline.onnx"), false);
        assert!(!failed.applied);
        assert_eq!(failed.engine, "dpdfnet");
        assert!(failed.error.as_deref().unwrap().contains("原始音频"));
    }

    #[test]
    fn denoiser_engine_follows_model_file_name() {
        let dpdf = denoiser_model_config("<models>/dpdfnet_baseline.onnx");
        assert_eq!(
            dpdf.dpdfnet.model.as_deref(),
            Some("<models>/dpdfnet_baseline.onnx")
        );
        assert!(dpdf.gtcrn.model.is_none());
        let gtcrn = denoiser_model_config("<models>/gtcrn_simple.onnx");
        assert_eq!(
            gtcrn.gtcrn.model.as_deref(),
            Some("<models>/gtcrn_simple.onnx")
        );
        assert!(gtcrn.dpdfnet.model.is_none());
        // 目录名里出现 dpdfnet 不算，只看文件名
        assert!(
            denoiser_model_config("<models>/dpdfnet/gtcrn_simple.onnx")
                .dpdfnet
                .model
                .is_none()
        );
    }

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
    fn synth_cancel_skips_queued_requests_and_stops_the_running_one() {
        let cancel = SynthCancel::default();
        // 还在排队就被取消：取到时直接跳过，登记随即清除
        cancel.cancel(7);
        assert!(!cancel.begin(7));
        assert!(cancel.begin(7), "登记只作用一次");
        cancel.end();
        // 正在合成时被取消：回调返回 false 让 sherpa 停下
        assert!(cancel.begin(8));
        let mut keep_going = cancel.keep_going();
        assert!(keep_going(&[], 0.5));
        cancel.cancel(8);
        assert!(!keep_going(&[], 0.6));
        assert!(cancel.stopped());
        cancel.end();
        // 下一个请求不受上一个的取消影响
        assert!(cancel.begin(9));
        assert!(!cancel.stopped());
        // 取消别的编号不影响正在合成的这个
        cancel.cancel(10);
        assert!(!cancel.stopped());
        cancel.end();
        assert!(!cancel.begin(10));
    }

    #[test]
    fn long_segments_are_slowed_down_gradually() {
        assert_eq!(long_segment_speed(10), 1.0);
        assert_eq!(long_segment_speed(28), 1.0);
        assert!((long_segment_speed(45) - 0.898).abs() < 0.001);
        assert_eq!(long_segment_speed(200), 0.82);
    }

    #[test]
    fn inference_threads_stay_in_a_sane_range() {
        assert!((1..=16).contains(&inference_threads()));
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
            seg1.len() <= 2400 + 3150,
            "非末段超长静音被裁至约 130ms: {}",
            seg1.len()
        );
        assert_eq!(*seg1.last().unwrap(), 0, "非末段结尾平滑归零");

        let mut seg2 = vec![-5000i16; 2400];
        smooth_stream_subsegment(&mut seg2, rate, false, true);
        assert_eq!(seg2[0], 0, "非首段开头平滑淡入消除直流跳变");
        assert_eq!(*seg2.last().unwrap(), -5000, "末段结尾交由上层统一收尾");
    }

    #[test]
    fn tts_dc_block_removes_envelope_offset_keeps_voice_band_and_settles_in_silence() {
        let rate = 24_000u32;
        let voiced = (rate / 2) as usize; // 500ms 人声 + 200ms 静音
        let samples: Vec<f32> = (0..voiced + (rate / 5) as usize)
            .map(|i| {
                if i >= voiced {
                    return 0.0;
                }
                let t = i as f32 / rate as f32;
                // 150 Hz 基频 + 随包络起伏的正向偏移（复现 Kokoro 实测形态）
                let envelope = (std::f32::consts::PI * t / 0.5).sin();
                0.3 * envelope * (2.0 * std::f32::consts::PI * 150.0 * t).sin() + 0.1 * envelope
            })
            .collect();
        let mid = (rate / 10) as usize..(rate * 2 / 5) as usize;
        let mean = |s: &[f32]| s.iter().sum::<f32>() / s.len() as f32;
        assert!(mean(&samples[mid.clone()]) > 0.08, "构造的输入带明显直流");

        let filtered = remove_dc(&samples, rate);
        assert_eq!(filtered.len(), samples.len());
        let dc = mean(&filtered[mid.clone()]);
        assert!(dc.abs() < 0.01, "包络直流被滤掉: {dc}");
        // 150 Hz 分量幅度基本不变：去掉直流后两者的交流有效值相差 <3%
        let ac_rms = |s: &[f32]| {
            let m = mean(s);
            (s.iter().map(|v| (v - m).powi(2)).sum::<f32>() / s.len() as f32).sqrt()
        };
        let raw_ac = ac_rms(&samples[mid.clone()]);
        let out_ac = ac_rms(&filtered[mid.clone()]);
        assert!(
            (out_ac / raw_ac - 1.0).abs() < 0.03,
            "人声频段无损: {raw_ac} -> {out_ac}"
        );
        // 人声结束 100ms 后回落到静音裁剪门限（24/32768）以下，不影响子分段尾静音判定
        let tail_start = voiced + (rate / 10) as usize;
        let tail_peak = filtered[tail_start..]
            .iter()
            .fold(0.0f32, |acc, v| acc.max(v.abs()));
        assert!(tail_peak < 24.0 / 32_768.0, "静音段残留: {tail_peak}");

        let pcm = tts_pcm(&samples, rate);
        let (max, min) = (*pcm.iter().max().unwrap(), *pcm.iter().min().unwrap());
        assert!(
            (f32::from(max) / -f32::from(min) - 1.0).abs() < 0.15,
            "隔直后正负峰基本对称: {max} / {min}"
        );

        let guarded = remove_dc(&[f32::NAN, 0.5, f32::INFINITY, 0.5], rate);
        assert!(
            guarded.iter().all(|v| v.is_finite()),
            "非有限值不污染滤波状态"
        );
        assert_eq!(remove_dc(&[0.2, 0.3], 0), vec![0.2, 0.3]);
    }

    #[test]
    fn offline_asr_buffer_redecodes_every_half_second_caps_window_and_reuses_clean_finish() {
        let mut decodes: Vec<usize> = Vec::new();
        let mut buf = OfflineAsrBuffer::default();

        // 不足 320ms（5120 采样）时不触发试认
        for _ in 0..5 {
            let out = buf.feed_with(&vec![0.1; 960], |s| {
                decodes.push(s.len());
                format!("len={}", s.len())
            });
            assert_eq!(out, None);
        }
        assert!(decodes.is_empty());

        // 满 320ms 首次触发试认（兼顾短急停词“停”/“急停”）
        let first = buf.feed_with(&vec![0.1; 960], |s| {
            decodes.push(s.len());
            format!("len={}", s.len())
        });
        assert_eq!(first.as_deref(), Some("len=5760"));
        assert_eq!(decodes, vec![5760]);

        // 未再攒够 500ms（8000 采样）前返回 None，不重复消耗 CPU
        for _ in 0..8 {
            let out = buf.feed_with(&vec![0.1; 960], |s| {
                decodes.push(s.len());
                format!("len={}", s.len())
            });
            assert_eq!(out, None);
        }
        assert_eq!(decodes.len(), 1);

        // 累计新增满 8000 采样（9 * 960 = 8640 >= 8000）触发第二次重认
        let second = buf.feed_with(&vec![0.1; 960], |s| {
            decodes.push(s.len());
            format!("len={}", s.len())
        });
        assert_eq!(second.as_deref(), Some("len=14400"));
        assert_eq!(decodes, vec![5760, 14400]);

        // 刚重认完且无新增采样时，finish 直接复用上次文字，0 次额外重认
        let exact_final = buf.finish_with(|s| {
            decodes.push(s.len());
            format!("final={}", s.len())
        });
        assert_eq!(exact_final, "len=14400");
        assert_eq!(decodes, vec![5760, 14400]);

        // 有新增尾音（不足 500ms）时，finish 补跑一次完整收尾识别，绝不漏字
        let mut tail_buf = OfflineAsrBuffer::default();
        let _ = tail_buf.feed_with(&vec![0.2; OFFLINE_INTERIM_FIRST_SAMPLES], |_| {
            "前半句".into()
        });
        assert_eq!(
            tail_buf.feed_with(&vec![0.2; 1600], |_| "不应提前触发".into()),
            None
        );
        let mut final_called = 0usize;
        let full_text = tail_buf.finish_with(|s| {
            final_called = s.len();
            "前半句尾音".into()
        });
        assert_eq!(full_text, "前半句尾音");
        assert_eq!(final_called, OFFLINE_INTERIM_FIRST_SAMPLES + 1600);

        // 超过 30 秒滑窗上限时自动裁旧保新，且满窗后每半秒仍正常继续重认
        let mut long_buf = OfflineAsrBuffer::default();
        let _ = long_buf.feed_with(&vec![0.3; OFFLINE_STREAM_MAX_SAMPLES + 3200], |s| {
            assert_eq!(s.len(), OFFLINE_STREAM_MAX_SAMPLES);
            "满窗首认".into()
        });
        assert_eq!(
            long_buf.feed_with(&vec![0.4; 4000], |_| "未满半秒".into()),
            None
        );
        let after_cap = long_buf.feed_with(&vec![0.5; 4000], |s| {
            assert_eq!(s.len(), OFFLINE_STREAM_MAX_SAMPLES);
            assert!((s.last().copied().unwrap_or_default() - 0.5).abs() < 1e-6);
            "满窗续认".into()
        });
        assert_eq!(after_cap.as_deref(), Some("满窗续认"));

        // 自动拾音 1s 静音 preroll + 60ms 起音首轮认出空串时，不消耗 320ms 首认步长；
        // 且若刚认出空串即无新增采样收尾，finish 同样零额外重认复用空串
        let mut preroll_buf = OfflineAsrBuffer::default();
        assert_eq!(
            preroll_buf.feed_with(&vec![0.0; 16_000], |_| String::new()),
            Some(String::new())
        );
        assert_eq!(
            preroll_buf.feed_with(&vec![0.2; 3_840], |_| "不足320ms".into()),
            None
        );
        assert_eq!(
            preroll_buf.feed_with(&vec![0.2; 1_920], |_| "急停".into()),
            Some("急停".into()),
            "前导静音空串不应退化为 8000 步长，新增满 5120 采样（360ms）应立即触发重认"
        );
        assert_eq!(
            preroll_buf.feed_with(&vec![0.2; 5_760], |_| "已非空后须满8000".into()),
            None
        );

        let mut silent_clean = OfflineAsrBuffer::default();
        assert_eq!(
            silent_clean.feed_with(&vec![0.0; OFFLINE_INTERIM_FIRST_SAMPLES], |_| String::new()),
            Some(String::new())
        );
        assert_eq!(
            silent_clean.finish_with(|_| panic!("clean empty decode must not re-run on finish")),
            ""
        );

        // 空缓冲直接返回空串
        assert_eq!(
            OfflineAsrBuffer::default().finish_with(|_| panic!("empty buffer must not decode")),
            ""
        );

        // 降噪 flush 尾音注入后不得复用旧的 clean_decode
        let mut flushed_buf = OfflineAsrBuffer::default();
        assert_eq!(
            flushed_buf.feed_with(&vec![0.1; OFFLINE_INTERIM_FIRST_SAMPLES], |_| {
                "之前".into()
            }),
            Some("之前".into())
        );
        flushed_buf.push_tail(&[0.2; 128]);
        let after_tail = flushed_buf.finish_with(|s| format!("len={}", s.len()));
        assert_eq!(
            after_tail,
            format!("len={}", OFFLINE_INTERIM_FIRST_SAMPLES + 128)
        );
    }

    #[test]
    fn zipvoice_tts_config_requires_vocoder_and_populates_paths() {
        let unique = format!(
            "moss-voice-local-zipvoice-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(dir.join("espeak-ng-data")).unwrap();
        for file in [
            "tokens.txt",
            "lexicon.txt",
            "encoder.int8.onnx",
            "decoder.int8.onnx",
        ] {
            std::fs::write(dir.join(file), b"stub").unwrap();
        }
        // 缺 vocos_24khz.onnx 时报错
        assert!(tts_config(&dir, TtsEngine::ZipVoice).is_err());

        std::fs::write(dir.join("vocos_24khz.onnx"), b"stub").unwrap();
        let cfg = tts_config(&dir, TtsEngine::ZipVoice).unwrap();
        let zv = &cfg.model.zipvoice;
        assert!(
            zv.tokens
                .as_deref()
                .is_some_and(|p| p.ends_with("tokens.txt"))
        );
        assert!(
            zv.vocoder
                .as_deref()
                .is_some_and(|p| p.ends_with("vocos_24khz.onnx"))
        );
        assert!((zv.feat_scale - 0.1).abs() < 1e-6);
        assert!((zv.t_shift - 0.5).abs() < 1e-6);
        assert!((zv.guidance_scale - 1.0).abs() < 1e-6);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vits_tts_config_uses_derived_files_and_plain_named_rule_fsts() {
        let root = std::env::temp_dir().join(format!(
            "moss-voice-local-vits-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let model_dir = root.join("moss-vits-zh");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("moss.onnx"), b"stub").unwrap();
        std::fs::write(model_dir.join("tokens.txt"), "_ 0\nㄋ 13\n  49\n").unwrap();
        std::fs::write(model_dir.join("lexicon.txt"), "你 ㄋ ㄧ ˇ\n").unwrap();
        for fst in ["number.fst", "date.fst", "phone.fst"] {
            std::fs::write(model_dir.join(fst), b"stub").unwrap();
        }
        let cfg = tts_config(&model_dir, TtsEngine::Vits).unwrap();
        let vits = &cfg.model.vits;
        let derived = model_dir.join("derived-v2");
        for (path, name) in [
            (&vits.model, "model.onnx"),
            (&vits.tokens, "tokens.txt"),
            (&vits.lexicon, "lexicon.txt"),
        ] {
            assert_eq!(
                path.as_deref(),
                Some(native_path_string(&derived.join(name)).as_str())
            );
        }
        assert!((vits.noise_scale - 0.5).abs() < 1e-6);
        assert!((vits.noise_scale_w - 0.5).abs() < 1e-6);
        assert!((vits.length_scale - 1.0).abs() < 1e-6);
        let fsts = cfg.rule_fsts.unwrap();
        assert!(
            ["phone.fst", "date.fst", "number.fst"]
                .iter()
                .all(|f| fsts.contains(f))
        );
        // 不带后缀的读法规则只给中文 VITS 用，其他引擎照旧只认 `-zh` 那份
        assert_eq!(rule_fsts(&model_dir, TtsEngine::Matcha), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
