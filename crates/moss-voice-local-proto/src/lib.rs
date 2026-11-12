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
//! 辅助程序与桌面程序须一起升级，旧协议在握手时拒绝。

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// 协议版本：辅助程序与 MOSS 不一致时拒绝使用。
pub const PROTOCOL_VERSION: u32 = 4;

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

/// Silero VAD 参数（xiaozhi-esp32-server 的语义：阈值 0.5，静音 200 ms 判停）。
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
            min_silence_ms: 200,
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
    /// SenseVoice：整段识别（`model.int8.onnx`、`tokens.txt`）
    #[default]
    SenseVoice,
    /// 流式 Zipformer Transducer（`encoder*.onnx`、`decoder*.onnx`、`joiner*.onnx`、`tokens.txt`）：支持流式识别
    StreamingTransducer,
}

impl AsrEngine {
    pub fn streaming(self) -> bool {
        matches!(self, Self::StreamingTransducer)
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
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub op: Op,
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
    Done,
    VadEvents {
        events: Vec<VadEventKind>,
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

/// 本地流式合成按停顿分段：首段在首个满 10 字的逗号/句末处切出以降低首包延迟；
/// 后续分句优先在句末标点或较长分句（≥22 字）处切分，保护千分位数字与词组连贯韵律，避免每 8 字切碎导致机械跳调。
pub fn synthesis_segments(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
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
        let min_comma_len = if segments.is_empty() { 10 } else { 22 };
        let should_cut = (length >= 8 && is_sentence_end)
            || (length >= min_comma_len && is_comma)
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
            },
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"id": 1, "kind": "vad_events", "events": ["speech_start"]})
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
        assert!(AsrEngine::StreamingTransducer.streaming() && !AsrEngine::SenseVoice.streaming());
        assert_eq!(
            VadParams::for_engine(VadEngine::Silero),
            VadParams::default()
        );
        assert_eq!(VadParams::for_engine(VadEngine::Ten).min_speech_ms, 100);
    }

    #[test]
    fn segments_preserve_text_and_hotwords_are_vocabulary_checked() {
        let text = "这是第一段测试内容，然后讲第二段的0.5毫米参数。";
        let segments = synthesis_segments(text);
        assert!(segments.len() >= 2);
        assert_eq!(segments.concat(), text);
        assert!(segments.iter().any(|s| s.contains("0.5")));
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
