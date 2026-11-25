//! MOSS ↔ 本地模型辅助程序（`moss-voice-local`）的帧协议（内置语音技术书 §11.2，NV6）。
//!
//! 两个独立程序经标准输入输出通信（V5：本地模型推理放在单独以 GPL-3.0 发布的辅助程序中，MOSS 本体保持 MIT）。
//!
//! 一帧 = `头长度 u32 LE` + `负载长度 u32 LE` + 头（UTF-8 JSON）+ 负载（二进制）。
//! 请求头为 [`Request`]，响应头为 [`Response`]；音频负载一律是 16 位小端单声道 PCM。
//! 每个请求带 `id`，响应原样带回；辅助程序内识别、合成、判停各在自己的线程处理，响应顺序可能与请求不同。
//!
//! 协议 2（本地模型可选）：加载请求带模型引擎（[`VadEngine`] / [`AsrEngine`] / [`TtsEngine`]），
//! 并增加流式识别（`asr_open` / `asr_feed` / `asr_finish` / `asr_close`）。
//! 协议 3（NV8a）：识别解码选项与热词；流式中间文字；分段合成 `synthesize_stream` / `audio_chunk` / `done`。
//! 协议 4（NV16）：固定 CampPlus 模型和独立声纹请求；embedding 负载为 f32le，附模型、维度、采样率和代次。
//! 协议 5（n89）：新增请求取消 `Op::Cancel`，流式合成中途放弃时通知辅助程序停止排队与后续分段。
//! 协议 6（n93）：新增 `AsrEngine::Qwen3Asr` / `AsrEngine::FireRedAsr2Ctc`、`TtsEngine::ZipVoice`、
//! `GTCRN` 前端语音降噪（`Op::LoadDenoiser`）及 `SmartTurn v3` 尾音语义判停（`Op::LoadSmartTurn` / `Op::SmartTurnPredict`）。
//! 协议 7（n101）：流式合成可选 `short_opener`（快机器上短开场白先合成）；判停流可附带关键词检测（`Op::LoadKws`、`Reply::VadEvents::keywords`）。
//! 协议 8（n110）：打开判停流 / 流式识别流回 [`Reply::Opened`]，如实说明这一路音频有没有先经前端降噪
//! （[`DenoiseReport`]：用的哪个模型，或降噪器没建起来、改用原始音频的原因），供完整过程记录。
//! 协议 9（n122）：新增 `TtsEngine::Vits`（单说话人中文 VITS，首个为 MOSS 原声模型 `moss_vits`）。
//! 辅助程序与桌面程序须一起升级，旧协议在握手时拒绝。

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// 协议版本：辅助程序与 MOSS 不一致时拒绝使用。
pub const PROTOCOL_VERSION: u32 = 9;

/// NV16 fixed official CampPlus ONNX, acquired and hashed on 2026-10-11.
/// ModelScope iic/speech_campplus_sv_zh-cn_16k-common v1.0.0: Apache-2.0.
pub const SPEAKER_MODEL_ID: &str = "campplus_sv_zh_cn_16k_common";
pub const SPEAKER_MODEL_FILE: &str = "3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx";
pub const SPEAKER_MODEL_BYTES: u64 = 28_281_138;
pub const SPEAKER_MODEL_SHA256: &str =
    "f682b514c05d947ee3fa91cd6ec6c5c7543479a128373fa29b1faedccd21fd11";
pub const SPEAKER_DIMENSION: u32 = 192;
pub const SPEAKER_SAMPLE_RATE: u32 = 16_000;

pub fn valid_speaker_pcm(sample_rate: u32, bytes: usize) -> bool {
    sample_rate == SPEAKER_SAMPLE_RATE
        && bytes.is_multiple_of(2)
        && (64_000..=320_000).contains(&bytes)
}
/// Embeddings are f32 little-endian, unlike PCM i16. Reject nonfinite/zero vectors.
pub fn embedding_to_bytes(vector: &[f32]) -> io::Result<Vec<u8>> {
    if vector.len() != SPEAKER_DIMENSION as usize
        || vector.iter().any(|v| !v.is_finite())
        || vector.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() <= 1e-16
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid speaker embedding",
        ));
    }
    Ok(vector.iter().flat_map(|v| v.to_le_bytes()).collect())
}
pub fn bytes_to_embedding(bytes: &[u8]) -> io::Result<Vec<f32>> {
    if bytes.len() != SPEAKER_DIMENSION as usize * 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid embedding length",
        ));
    }
    let values = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect::<Vec<_>>();
    embedding_to_bytes(&values)?;
    Ok(values)
}

/// 单帧头部上限（防止损坏的数据导致大内存分配）
pub const MAX_HEADER: u32 = 64 * 1024;
/// 单帧负载上限：60 秒 48 kHz 16 位音频
pub const MAX_PAYLOAD: u32 = 60 * 48_000 * 2;

/// Silero VAD 参数（xiaozhi-esp32-server 的语义：阈值 0.5）。静音 250 ms 即交给识别与大模型抢跑，
/// 开口前的实际等待由会话按句子是否说完决定（2026-11-13 n86）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VadParams {
    pub threshold: f32,
    pub min_silence_ms: u32,
    pub min_speech_ms: u32,
}

impl Default for VadParams {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            min_silence_ms: 250,
            min_speech_ms: 250,
        }
    }
}

/// 判停模型（单个 onnx 文件）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VadEngine {
    #[default]
    Silero,
    /// TEN-VAD（声网）
    Ten,
}

/// 识别模型（目录）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsrEngine {
    /// SenseVoice：离线整段模型，流式会话中按约 0.5 秒周期增量重认已录片段（`model.int8.onnx`、`tokens.txt`）
    #[default]
    SenseVoice,
    /// 流式 Zipformer Transducer（`encoder*.onnx`、`decoder*.onnx`、`joiner*.onnx`、`tokens.txt`）：原生逐块流式识别
    StreamingTransducer,
    /// Qwen3-ASR 0.6B（`conv_frontend.onnx`、`encoder.int8.onnx`、`decoder.int8.onnx`、`tokenizer/`）
    Qwen3Asr,
    /// FireRedASR2-CTC 中文/中英多方言流式切片识别（`model.int8.onnx`、`tokens.txt`）
    FireRedAsr2Ctc,
}

impl AsrEngine {
    pub fn streaming(self) -> bool {
        matches!(
            self,
            Self::SenseVoice | Self::StreamingTransducer | Self::Qwen3Asr | Self::FireRedAsr2Ctc
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AsrDecodeOptions {
    pub beam_search: bool,
    pub hotwords: Vec<String>,
}
impl Default for AsrDecodeOptions {
    fn default() -> Self {
        Self {
            beam_search: true,
            hotwords: Vec::new(),
        }
    }
}

/// 当前模型包只有 CJK 字符词表：仅保留能逐字映射到 tokens.txt 的中文短语。
/// 英文需要匹配的 BPE 词表，不盲拆英文字母。缓冲格式每行一个短语、token 用空格分隔。
pub fn cjk_hotwords(words: &[String], tokens: &str) -> Vec<u8> {
    let vocabulary: std::collections::HashSet<&str> = tokens
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    words
        .iter()
        .take(64)
        .filter_map(|word| {
            let chars: Vec<char> = word.chars().collect();
            if chars.is_empty()
                || chars.len() > 32
                || !chars.iter().all(|c| {
                    ('\u{3400}'..='\u{9fff}').contains(c)
                        && vocabulary.contains(c.to_string().as_str())
                })
            {
                return None;
            }
            Some(
                chars
                    .into_iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

/// 合成模型（目录）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsEngine {
    /// Kokoro 多语言（v1.0 / v1.1）
    #[default]
    Kokoro,
    /// Matcha（声学模型 + `vocos` 声码器）
    Matcha,
    /// ZipVoice-Distill 零样本克隆（`encoder` + `decoder` + `vocos_24khz`）
    ZipVoice,
    /// 单说话人中文 VITS（注音符号输入，`moss.onnx` + 词表 + 数字读法规则；MOSS 原声，n122）
    Vits,
}

impl VadParams {
    /// 各判停模型的参数。TEN-VAD 处理设备上传的 Opus 音频时，“停止”这类短词在 250 ms 门槛下检测不到、
    /// 长句也要晚 1–1.8 秒才确认，所以最短语音降到 100 ms（代价：突发的宽带噪声偶尔被当成一小段语音）。
    pub fn for_engine(engine: VadEngine) -> Self {
        match engine {
            VadEngine::Silero => Self::default(),
            VadEngine::Ten => Self {
                min_speech_ms: 100,
                ..Self::default()
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// 握手：返回版本与当前已加载的能力
    Hello,
    /// Only the fixed catalog ID is accepted; root is host-selected, never model input.
    LoadSpeaker {
        model_id: String,
        models_root: String,
    },
    /// A bounded 2..10s mono PCM clip. The generation is echoed, never trusted as identity.
    SpeakerEmbed {
        model_id: String,
        sample_rate: u32,
        utterance_generation: u64,
    },
    /// 加载判停模型文件
    LoadVad {
        model: String,
        params: VadParams,
        #[serde(default)]
        engine: VadEngine,
    },
    /// 加载识别模型目录
    LoadAsr {
        dir: String,
        #[serde(default)]
        engine: AsrEngine,
        #[serde(default)]
        options: AsrDecodeOptions,
    },
    /// 加载合成模型目录
    LoadTts {
        dir: String,
        #[serde(default)]
        engine: TtsEngine,
    },
    /// 加载前端语音降噪模型（GTCRN ONNX；空路径表示卸载）
    LoadDenoiser {
        model: String,
    },
    /// 加载尾音语义判停模型（SmartTurn v3.2 ONNX 目录或文件；空路径表示卸载）
    LoadSmartTurn {
        model: String,
    },
    /// 加载命令词检测模型（协议 7；sherpa-onnx 关键词检测 zipformer 目录，空路径表示卸载）。
    /// 加载后每一路判停流顺带检测 `keywords`（每行“拼音声母韵母 @显示文字”，见 [`command_keywords_text`]），
    /// 检测到的词随 [`Reply::VadEvents`] 的 `keywords` 返回。只在本地判停流上运行，送入原始音频（不降噪）。
    LoadKws {
        dir: String,
        #[serde(default)]
        keywords: String,
    },
    /// 打开一路判停流（每个设备会话一路），只支持 16 kHz
    VadOpen {
        stream: u64,
    },
    /// 送入一段 PCM（负载），返回期间的事件
    VadFeed {
        stream: u64,
    },
    VadClose {
        stream: u64,
    },
    /// 识别负载中的一段话（流式模型同样支持）
    Transcribe {
        sample_rate: u32,
    },
    /// 打开一路流式识别（只支持流式识别模型，16 kHz）
    AsrOpen {
        stream: u64,
    },
    /// 送入一段 16 kHz PCM（负载）；辅助程序边收边解码
    AsrFeed {
        stream: u64,
    },
    /// 说完：返回最终文字（`Text`）并关闭该路
    AsrFinish {
        stream: u64,
    },
    /// 放弃一路识别（打断、重新拾音）
    AsrClose {
        stream: u64,
    },
    /// 合成一句话
    Synthesize {
        text: String,
        speaker: i32,
        speed: f32,
    },
    /// 一次请求返回多个 AudioChunk，最后以 Done / Error 结束（协议 3）。
    SynthesizeStream {
        text: String,
        speaker: i32,
        speed: f32,
        /// 这一句第一段最多可以有多长（字）；0 表示按最保守的阶梯。由调用方按“已经排在播放队列里的
        /// 语音还能撑多久”和这台机器的合成速度算出：机器快、前面还有存货时整句合成（语调连贯），
        /// 机器慢或刚开口时才切小段（尽快出声、不断流）。
        #[serde(default)]
        first_max_chars: u32,
        /// 允许把很短的开场白（“好的，”这类第一个逗号前的分句）单独先合成（协议 7）：
        /// 只在这台机器合成足够快、下一段能在开场白播完前算好时由调用方打开，开口能早 0.3–0.4 秒。
        #[serde(default)]
        short_opener: bool,
    },
    /// 放弃编号为 `target` 的合成请求（协议 5）：还在排队的不再合成，正在合成的尽快停下。
    /// 对话被打断或合并重来时，作废的句子不再占着合成线程，新回答的第一句不用排在它们后面。
    /// 本请求自身不回复；被放弃的请求以 Error 结束。
    Cancel {
        target: u64,
    },
    /// 预测尾音语义是否说完（协议 6：负载为 i16le 单声道 PCM）
    SmartTurnPredict {
        sample_rate: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub op: Op,
}

/// 一路判停 / 识别流的前端降噪情况（协议 8，n110）：只描述事实，不含目录（模型只写文件名）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenoiseReport {
    /// 这一路音频是否先经降噪再送判停 / 识别
    pub applied: bool,
    /// 降噪模型文件名（没配降噪时为空）
    #[serde(default)]
    pub model: String,
    /// 降噪引擎：`gtcrn` / `dpdfnet`（没配降噪时为空）
    #[serde(default)]
    pub engine: String,
    /// 配了降噪、但这一路降噪器没建起来的原因（这一路用原始音频）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VadEventKind {
    SpeechStart,
    SpeechEnd,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reply {
    Hello {
        protocol: u32,
        version: String,
        vad: bool,
        asr: bool,
        tts: bool,
        #[serde(default)]
        denoiser: bool,
        #[serde(default)]
        smart_turn: bool,
        #[serde(default)]
        kws: bool,
    },
    Loaded {
        sample_rate: Option<u32>,
        speakers: Option<u32>,
    },
    SpeakerLoaded {
        model_id: String,
        sample_rate: u32,
        dimension: u32,
    },
    Embedding {
        model_id: String,
        sample_rate: u32,
        dimension: u32,
        utterance_generation: u64,
        format: String,
    },
    SmartTurn {
        probability: f32,
        complete: bool,
    },
    Done,
    /// 判停流 / 流式识别流已打开（协议 8）：这一路音频的前端降噪情况
    Opened {
        denoise: DenoiseReport,
    },
    VadEvents {
        events: Vec<VadEventKind>,
        /// 这段音频里检测到的命令词（显示文字，协议 7）
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keywords: Vec<String>,
    },
    Text {
        text: String,
    },
    /// 负载为合成的 PCM
    Audio {
        sample_rate: u32,
    },
    /// 非终态响应；同一请求 id 的分段音频，结束必须再发送 Done。
    AudioChunk {
        sample_rate: u32,
    },
    /// 失败：中文原因，不含路径以外的敏感信息
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(flatten)]
    pub reply: Reply,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// 写一帧。
pub fn write_frame<W: Write, H: Serialize>(
    writer: &mut W,
    header: &H,
    payload: &[u8],
) -> io::Result<()> {
    let header = serde_json::to_vec(header).map_err(|error| invalid(error.to_string()))?;
    if header.len() as u64 > MAX_HEADER as u64 || payload.len() as u64 > MAX_PAYLOAD as u64 {
        return Err(invalid("帧过大"));
    }
    let mut frame = Vec::with_capacity(8 + header.len() + payload.len());
    frame.extend_from_slice(&(header.len() as u32).to_le_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(payload);
    writer.write_all(&frame)?;
    writer.flush()
}

/// 读一帧；对端关闭时返回 `Ok(None)`。
pub fn read_frame<R: Read, H: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> io::Result<Option<(H, Vec<u8>)>> {
    let mut lengths = [0u8; 8];
    match reader.read_exact(&mut lengths) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let header_len = u32::from_le_bytes([lengths[0], lengths[1], lengths[2], lengths[3]]);
    let payload_len = u32::from_le_bytes([lengths[4], lengths[5], lengths[6], lengths[7]]);
    if header_len > MAX_HEADER || payload_len > MAX_PAYLOAD {
        return Err(invalid("帧长度超出上限"));
    }
    let mut header = vec![0u8; header_len as usize];
    reader.read_exact(&mut header)?;
    let mut payload = vec![0u8; payload_len as usize];
    reader.read_exact(&mut payload)?;
    let header = serde_json::from_slice(&header)
        .map_err(|error| invalid(format!("帧头无法解析：{error}")))?;
    Ok(Some((header, payload)))
}

pub fn pcm_to_bytes(samples: &[i16]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect()
}

pub fn bytes_to_pcm(bytes: &[u8]) -> Vec<i16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect()
}

/// 命令词的类别：停止类走急停快路径，退出类道别后回到待命。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKeyword {
    Stop,
    Exit,
}

/// 命令词表（协议 7，n101）：拼音按关键词模型的建模单元（声母 + 带调韵母）写，“一”按变调写。
/// 表由客户端随 `LoadKws` 送给辅助程序，增删条目不改协议号。
/// 只收短促、在噪声里通用识别容易听错、又必须可靠的说法（同一说法可写多种读音）；显示文字与 `fast::is_stop` /
/// 退出词表里的说法一致，会话据此把识别结果纠正回命令。
pub const COMMAND_KEYWORDS: &[(&str, &str, CommandKeyword)] = &[
    ("t íng", "停", CommandKeyword::Stop),
    ("t íng x ià", "停下", CommandKeyword::Stop),
    ("t íng zh ǐ", "停止", CommandKeyword::Stop),
    ("j í t íng", "急停", CommandKeyword::Stop),
    // “暂停”不收（n114）：它的本意是可恢复的暂停，停止类命中会走急停（0x18 复位），交给大模型用 pause
    ("t uì x ià", "退下", CommandKeyword::Exit),
    // “一”按字典读音（yī）与变调（yí）各写一条：模型按 wenetspeech 的字典读音训练，实测只写变调认不出
    ("t uì y ī x ià", "退一下", CommandKeyword::Exit),
    ("t uì y í x ià", "退一下", CommandKeyword::Exit),
    ("z ài j iàn", "再见", CommandKeyword::Exit),
];

/// 交给关键词检测模型的命令词表（每行“拼音 @显示文字”）。
pub fn command_keywords_text() -> String {
    COMMAND_KEYWORDS
        .iter()
        .map(|(tokens, label, _)| format!("{tokens} @{label}\n"))
        .collect()
}

/// 检测结果（显示文字）属于哪一类命令。
pub fn command_keyword_kind(label: &str) -> Option<CommandKeyword> {
    COMMAND_KEYWORDS
        .iter()
        .find(|(_, known, _)| *known == label)
        .map(|(_, _, kind)| *kind)
}

/// 本地流式合成按停顿分段：首段在首个满 10 字的逗号/句末处切出以降低首包延迟；
/// 后续分句优先在句末标点或较长分句（≥22 字）处切分，保护千分位数字与词组连贯韵律，避免每 8 字切碎导致机械跳调。
pub fn synthesis_segments(text: &str) -> Vec<String> {
    synthesis_segments_from(text, 0)
}

/// 同 [`synthesis_segments`]，但第一段的长度上限至少放宽到 `first_max_chars`（之后仍逐段放大 1.5 倍）。
/// 上限越大切得越少；足够大时只在句号处分段，一句话整句合成。
pub fn synthesis_segments_from(text: &str, first_max_chars: u32) -> Vec<String> {
    synthesis_segments_with(text, first_max_chars, false)
}

/// 同 [`synthesis_segments_from`]；`short_opener` 时第一个逗号前的分句再短（≥2 字）也单独成段，
/// 第二段相应收短（6–10 字），保证快机器上它能在开场白播完前算好、中间不断。
pub fn synthesis_segments_with(
    text: &str,
    first_max_chars: u32,
    short_opener: bool,
) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if first_max_chars > 0 && !short_opener && chars.len() <= first_max_chars as usize {
        let trimmed = text.trim();
        return if trimmed.is_empty() {
            Vec::new()
        } else {
            vec![text.to_owned()]
        };
    }
    let widen = |index: usize, (floor, cap): (usize, usize)| -> (usize, usize) {
        let wanted = (first_max_chars as f64 * 1.5f64.powi(index as i32)).min(72.0) as usize;
        if wanted <= cap {
            (floor, cap)
        } else {
            // 逗号处够长就切的下限跟着上限走，不然上限放宽了还是会在第一个逗号切开
            (floor.max(wanted * 3 / 5), wanted)
        }
    };
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut length = 0;
    for (i, &ch) in chars.iter().enumerate() {
        current.push(ch);
        length += 1;
        let numeric_comma = ch == ','
            && i > 0
            && chars[i - 1].is_ascii_digit()
            && chars.get(i + 1).is_some_and(|next| next.is_ascii_digit());
        let is_sentence_end = matches!(ch, '。' | '！' | '？' | '；' | ';' | '!' | '?' | '\n');
        let is_comma = matches!(ch, '，' | ',') && !numeric_comma;
        let is_soft_pause = matches!(ch, '、' | '：' | ':' | ' ');
        // 分段长度逐段放大而不是一步到位：本地合成 1 秒语音要算 0.5–0.8 秒，下一段必须在上一段
        // 播完之前算完才不会断。第一段短（尽快出声），之后每段约为前一段的 1.5 倍；
        // 原先第二段直接 22 字起，慢机器上第一段播完时它还没算完，中间会空一截。
        // （逗号处够长就切的下限，这一段的长度上限）
        let index = segments.len();
        let (min_comma_len, max_len) = widen(
            index,
            match index {
                0 if short_opener => (2, 14),
                0 => (8, 14),
                1 if short_opener => (6, 10),
                1 => (12, 20),
                2 => (18, 30),
                _ => (22, 45),
            },
        );
        // 这个逗号后面那个分句的长度：不在这里切的话，这一段会不会超过上限
        let next_clause = || {
            chars[i + 1..]
                .iter()
                .position(|c| {
                    matches!(
                        c,
                        '，' | ',' | '。' | '！' | '？' | '；' | ';' | '!' | '?' | '\n'
                    )
                })
                .map_or(chars.len() - i - 1, |at| at + 1)
        };
        let remaining = chars.len() - i - 1;
        let orphan_sentence_tail = !short_opener
            && remaining > 0
            && remaining <= 6
            && length + remaining <= max_len.max(15)
            && !chars[i + 1..].iter().any(|c| matches!(c, '，' | ','));
        let should_cut = (length >= 8 && is_sentence_end)
            || (is_comma
                && !orphan_sentence_tail
                && (length >= min_comma_len || (length >= 5 && length + next_clause() > max_len)))
            || (length >= 45 && is_soft_pause)
            || (length >= 72
                && !chars
                    .get(i + 1)
                    .is_some_and(|next| next.is_ascii_alphanumeric()));
        if should_cut {
            if !current.trim().is_empty() {
                segments.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
            length = 0;
        }
    }
    if !current.trim().is_empty() {
        segments.push(current);
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_payload_has_strict_audio_and_finite_embedding_boundaries() {
        assert!(valid_speaker_pcm(16000, 64000));
        assert!(valid_speaker_pcm(16000, 320000));
        for (rate, size) in [
            (24000, 64000),
            (16000, 63998),
            (16000, 64001),
            (16000, 320002),
        ] {
            assert!(!valid_speaker_pcm(rate, size));
        }
        let vector = vec![0.25; 192];
        let encoded = embedding_to_bytes(&vector).unwrap();
        assert_eq!(encoded.len(), 768);
        assert_eq!(bytes_to_embedding(&encoded).unwrap(), vector);
        assert!(bytes_to_embedding(&encoded[..767]).is_err());
        for values in [
            vec![0.; 192],
            vec![f32::NAN; 192],
            vec![f32::INFINITY; 192],
            vec![1.; 191],
        ] {
            assert!(embedding_to_bytes(&values).is_err());
        }
    }

    #[test]
    fn frames_round_trip_and_eof_is_none() {
        let mut buffer = Vec::new();
        let request = Request {
            id: 7,
            op: Op::Synthesize {
                text: "你好".into(),
                speaker: 50,
                speed: 1.0,
            },
        };
        write_frame(&mut buffer, &request, &[]).unwrap();
        let pcm = vec![1i16, -2, 32767, -32768];
        let response = Response {
            id: 7,
            reply: Reply::Audio {
                sample_rate: 24_000,
            },
        };
        write_frame(&mut buffer, &response, &pcm_to_bytes(&pcm)).unwrap();
        let mut reader = buffer.as_slice();
        let (back, payload): (Request, Vec<u8>) = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(back, request);
        assert!(payload.is_empty());
        let (back, payload): (Response, Vec<u8>) = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(back, response);
        assert_eq!(bytes_to_pcm(&payload), pcm);
        assert!(read_frame::<_, Request>(&mut reader).unwrap().is_none());
    }

    #[test]
    fn json_shape_is_flat_and_readable() {
        let value = serde_json::to_value(Request {
            id: 1,
            op: Op::VadFeed { stream: 3 },
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"id": 1, "op": "vad_feed", "stream": 3})
        );
        let value = serde_json::to_value(Response {
            id: 1,
            reply: Reply::VadEvents {
                events: vec![VadEventKind::SpeechStart],
                keywords: Vec::new(),
            },
        })
        .unwrap();
        // 没检测到命令词时不带 keywords 字段（与协议 6 的负载一致）
        assert_eq!(
            value,
            serde_json::json!({"id": 1, "kind": "vad_events", "events": ["speech_start"]})
        );
        // 协议 7：命令词随判停事件返回；旧辅助程序的回复（没有 keywords）读作空
        let with_keyword: Response = serde_json::from_value(serde_json::json!(
            {"id": 2, "kind": "vad_events", "events": [], "keywords": ["退下"]}
        ))
        .unwrap();
        assert_eq!(
            with_keyword.reply,
            Reply::VadEvents {
                events: Vec::new(),
                keywords: vec!["退下".into()],
            }
        );
        // 命令词表：每行“拼音 @显示文字”，显示文字能映射回类别
        let table = command_keywords_text();
        assert!(table.lines().any(|line| line == "t uì y ī x ià @退一下"));
        assert_eq!(table.lines().count(), COMMAND_KEYWORDS.len());
        assert_eq!(command_keyword_kind("急停"), Some(CommandKeyword::Stop));
        assert_eq!(command_keyword_kind("再见"), Some(CommandKeyword::Exit));
        assert_eq!(command_keyword_kind("问一下"), None);
        // “暂停”是可恢复的暂停，不能当停止类命令词走急停（n114）
        assert_eq!(command_keyword_kind("暂停"), None);
        // 协议 8：打开判停 / 识别流时说明降噪情况；没出错时不带 error 字段
        let opened = serde_json::to_value(Response {
            id: 4,
            reply: Reply::Opened {
                denoise: DenoiseReport {
                    applied: true,
                    model: "gtcrn_simple.onnx".into(),
                    engine: "gtcrn".into(),
                    error: None,
                },
            },
        })
        .unwrap();
        assert_eq!(
            opened,
            serde_json::json!({"id": 4, "kind": "opened", "denoise":
                {"applied": true, "model": "gtcrn_simple.onnx", "engine": "gtcrn"}})
        );
        let failed: Response = serde_json::from_value(serde_json::json!(
            {"id": 5, "kind": "opened", "denoise": {"applied": false, "model": "gtcrn_simple.onnx",
                "engine": "gtcrn", "error": "降噪器没建起来"}}
        ))
        .unwrap();
        assert_eq!(
            failed.reply,
            Reply::Opened {
                denoise: DenoiseReport {
                    applied: false,
                    model: "gtcrn_simple.onnx".into(),
                    engine: "gtcrn".into(),
                    error: Some("降噪器没建起来".into()),
                }
            }
        );
        let value = serde_json::to_value(Request {
            id: 2,
            op: Op::LoadAsr {
                dir: "d".into(),
                engine: AsrEngine::StreamingTransducer,
                options: AsrDecodeOptions::default(),
            },
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"id": 2, "op": "load_asr", "dir": "d", "engine": "streaming_transducer", "options":{"beam_search":true,"hotwords":[]}})
        );
        // 不带引擎的旧请求按默认引擎解析
        let request: Request =
            serde_json::from_value(serde_json::json!({"id": 3, "op": "load_tts", "dir": "k"}))
                .unwrap();
        assert_eq!(
            request.op,
            Op::LoadTts {
                dir: "k".into(),
                engine: TtsEngine::Kokoro
            }
        );
        let request: Request = serde_json::from_value(serde_json::json!({"id": 4, "op": "load_vad", "model": "m", "params": {"threshold": 0.5, "min_silence_ms": 200, "min_speech_ms": 250}, "engine": "ten"})).unwrap();
        assert!(matches!(
            request.op,
            Op::LoadVad {
                engine: VadEngine::Ten,
                ..
            }
        ));
        assert!(
            AsrEngine::StreamingTransducer.streaming()
                && AsrEngine::SenseVoice.streaming()
                && AsrEngine::Qwen3Asr.streaming()
                && AsrEngine::FireRedAsr2Ctc.streaming()
        );
        assert_eq!(
            VadParams::for_engine(VadEngine::Silero),
            VadParams::default()
        );
        assert_eq!(VadParams::for_engine(VadEngine::Ten).min_speech_ms, 100);
        // 协议 6 新增变体往返验证（协议 7、8 起仍兼容这些请求）
        assert_eq!(PROTOCOL_VERSION, 9);
        let qwen_asr: Request = serde_json::from_value(serde_json::json!({
            "id": 5,
            "op": "load_asr",
            "dir": "qwen3",
            "engine": "qwen3_asr"
        }))
        .unwrap();
        assert!(matches!(
            qwen_asr.op,
            Op::LoadAsr {
                engine: AsrEngine::Qwen3Asr,
                ..
            }
        ));
        let zipvoice: Request = serde_json::from_value(serde_json::json!({
            "id": 6,
            "op": "load_tts",
            "dir": "zip",
            "engine": "zip_voice"
        }))
        .unwrap();
        assert!(matches!(
            zipvoice.op,
            Op::LoadTts {
                engine: TtsEngine::ZipVoice,
                ..
            }
        ));
        let vits: Request = serde_json::from_value(serde_json::json!({
            "id": 6,
            "op": "load_tts",
            "dir": "moss-vits",
            "engine": "vits"
        }))
        .unwrap();
        assert!(matches!(
            vits.op,
            Op::LoadTts {
                engine: TtsEngine::Vits,
                ..
            }
        ));
        for req in [
            Request {
                id: 7,
                op: Op::LoadDenoiser {
                    model: "gtcrn.onnx".into(),
                },
            },
            Request {
                id: 8,
                op: Op::LoadSmartTurn {
                    model: "smart-turn".into(),
                },
            },
            Request {
                id: 9,
                op: Op::SmartTurnPredict {
                    sample_rate: 16_000,
                },
            },
        ] {
            let mut buf = Vec::new();
            write_frame(&mut buf, &req, &[]).unwrap();
            let (back, _): (Request, Vec<u8>) = read_frame(&mut buf.as_slice()).unwrap().unwrap();
            assert_eq!(back, req);
        }
        let hello: Response = serde_json::from_value(serde_json::json!({
            "id": 1,
            "kind": "hello",
            "protocol": 6,
            "version": "0.3.7",
            "vad": true,
            "asr": false,
            "tts": false
        }))
        .unwrap();
        assert!(matches!(
            hello.reply,
            Reply::Hello {
                denoiser: false,
                smart_turn: false,
                ..
            }
        ));
        let st = Response {
            id: 9,
            reply: Reply::SmartTurn {
                probability: 0.25,
                complete: false,
            },
        };
        let mut st_buf = Vec::new();
        write_frame(&mut st_buf, &st, &[]).unwrap();
        let (st_back, _): (Response, Vec<u8>) =
            read_frame(&mut st_buf.as_slice()).unwrap().unwrap();
        assert_eq!(st_back, st);
    }

    #[test]
    fn segments_preserve_text_and_hotwords_are_vocabulary_checked() {
        let text = "这是第一段测试内容，然后讲第二段的0.5毫米参数。";
        let segments = synthesis_segments(text);
        assert!(segments.len() >= 2);
        assert_eq!(segments.concat(), text);
        assert!(segments.iter().any(|s| s.contains("0.5")));
        // 逗号处的分段逐段变长：第一段尽快出声，后面的段不会比前一段长太多
        let long = "先把材料放平整，再把防护盖轻轻关好，然后在电脑上确认一下功率和速度这两个参数，没有问题的话就可以按下开始键让机器动起来了。";
        let pieces = synthesis_segments(long);
        assert_eq!(pieces.concat(), long);
        let lengths: Vec<usize> = pieces.iter().map(|s| s.chars().count()).collect();
        assert_eq!(lengths[0], 8, "{pieces:?}");
        assert!(lengths[1] <= 16, "第二段不能一下子变得很长：{pieces:?}");
        assert!(pieces.len() >= 4, "{pieces:?}");
        // 机器快或前面还有存货：上限放宽后少切甚至不切，一句话的语调保持连贯
        assert_eq!(synthesis_segments_from(long, 72), vec![long.to_owned()]);
        let relaxed = synthesis_segments_from(long, 30);
        assert_eq!(relaxed.concat(), long);
        assert!(
            relaxed.len() < pieces.len() && relaxed[0].chars().count() > 14,
            "{relaxed:?}"
        );
        assert_eq!(synthesis_segments_from(long, 0), pieces);
        // 快机器上允许短开场白单独先合成：“好的，”先出声，第二段收短；关掉时与原来一致
        assert_eq!(
            synthesis_segments_with("好的，激光机现在空闲。", 0, true),
            vec!["好的，".to_owned(), "激光机现在空闲。".to_owned()]
        );
        assert_eq!(
            synthesis_segments_with("好的，激光机现在空闲。", 0, false),
            vec!["好的，激光机现在空闲。".to_owned()]
        );
        // 首句冷启动（first_max_chars=0）遇到末尾仅剩 ≤6 字收尾短半句时保持整句合成；有垫底上限 ≥ 句长时也不再按逗号二次切碎
        assert_eq!(
            synthesis_segments_with("三毫米椴木刻字，把握较大。", 0, false),
            vec!["三毫米椴木刻字，把握较大。".to_owned()]
        );
        assert_eq!(
            synthesis_segments_with("激光雕刻机目前处于空闲待命状态，把握较大。", 28, false),
            vec!["激光雕刻机目前处于空闲待命状态，把握较大。".to_owned()]
        );
        assert_eq!(synthesis_segments_with(long, 0, false), pieces);
        let num_text = "当前雕刻速度为 1,200 毫米每分钟，功率已设好，随时可以开始。";
        let num_segments = synthesis_segments(num_text);
        assert_eq!(num_segments.concat(), num_text);
        assert!(
            num_segments.iter().any(|s| s.contains("1,200")),
            "千分位数字不得被逗号切碎"
        );
        let words = vec!["椴木".into(), "MOSS".into(), "未知".into(), "椴\0木".into()];
        assert_eq!(cjk_hotwords(&words, "椴 1\n木 2\n"), "椴 木".as_bytes());
        let request: Request =
            serde_json::from_value(serde_json::json!({"id":1,"op":"load_asr","dir":"d"})).unwrap();
        assert!(matches!(
            request.op,
            Op::LoadAsr {
                options: AsrDecodeOptions {
                    beam_search: true,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn oversized_or_truncated_frames_are_rejected() {
        let mut bad = Vec::new();
        bad.extend_from_slice(&(MAX_HEADER + 1).to_le_bytes());
        bad.extend_from_slice(&0u32.to_le_bytes());
        assert!(read_frame::<_, Request>(&mut bad.as_slice()).is_err());
        let mut buffer = Vec::new();
        write_frame(
            &mut buffer,
            &Request {
                id: 1,
                op: Op::Hello,
            },
            &[1, 2, 3],
        )
        .unwrap();
        buffer.truncate(buffer.len() - 1);
        assert!(
            read_frame::<_, Request>(&mut buffer.as_slice()).is_err(),
            "截断的帧报错而不是当作结束"
        );
        assert!(
            write_frame(
                &mut Vec::new(),
                &Request {
                    id: 1,
                    op: Op::Hello
                },
                &vec![0; MAX_PAYLOAD as usize + 1]
            )
            .is_err()
        );
    }
}
