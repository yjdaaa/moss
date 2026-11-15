//! 整条语音链路连在一起实测（不在常规测试中运行）。
//!
//! 一段段“设备端说话”（2 秒前导 + 一句话 + 2.5 秒尾巴，可叠加突发噪声）按固件节奏每 60 ms 上行一帧
//! Opus，经真实会话状态机、本地判停（或内置能量判停）、本地识别、可选 Smart Turn、模拟云端大模型
//! （首字延迟）、本地合成，下行按官方固件播放规则回放。自动模式下与固件一致：收到 `tts start`
//! 停止上行，`tts stop` 后再发 `listen start` 接着说。每段记录用户真正感受到的：说完到听见回答多久、
//! 识别出的文字、播放断流、噪声是否抢答、说“退下”是否断开回到待机。
//!
//! ```text
//! MOSS_VOICE_LOCAL_EXE=<moss-voice-local> MOSS_LOCAL_MODELS=<模型目录> MOSS_CHAIN_CLIPS=<片段目录> \
//! MOSS_CHAIN_OUT=<结果.jsonl> [MOSS_CHAIN_ONLY=rec,energy,…] [MOSS_CHAIN_LLM_FIRST_MS=500] \
//! cargo test -p moss-voice-server real_local_chain_matrix -- --ignored --nocapture
//! ```
//! 片段目录由 `meta.json`（`file`、`name`、`level`、`ref`、`note`、`on_ms`、`off_ms`）与 16 kHz s16 文件组成；
//! 每段一行 JSON 追加到 `MOSS_CHAIN_OUT`。片段用 `scripts/voice-bench/chain_clips.py` 生成，
//! 统计（字错率按数字读法归一、延迟、误触发、退出）用 `scripts/voice-bench/chain_report.py`。
use std::io::Write as _;

use serde_json::json;

use moss_voice_providers::local::{
    AsrEngine, LocalAsr, LocalHelper, LocalStreamingAsr, LocalTts, TtsEngine, TurnHelper,
    VadEngine, VadParams,
};

use super::playback::replay_like_firmware;
use super::*;
use crate::conversation::TurnEndPredictor;

struct Combo {
    name: &'static str,
    /// `None` 为内置能量判停
    vad: Option<(&'static str, VadEngine)>,
    asr: (&'static str, AsrEngine),
    tts: (&'static str, TtsEngine, i32),
    smart_turn: bool,
    /// 前端降噪模型文件（判停与识别之前；GTCRN / DPDFNet）
    denoise: Option<&'static str>,
    /// 命令词检测模型目录（协议 7：判停流顺带检测停止 / 退出命令词）
    kws: Option<&'static str>,
}

/// sherpa-onnx 中文命令词检测模型（3.3M zipformer，wenetspeech）
const KWS: &str = "sherpa-onnx-kws-zipformer-wenetspeech-3.3M-2024-01-01";

const SILERO: Option<(&str, VadEngine)> = Some(("silero_vad.onnx", VadEngine::Silero));
/// Silero v6.2（`snakers4/silero-vad` v6.2 标签下的 `silero_vad.onnx`）
const SILERO6: Option<(&str, VadEngine)> = Some(("silero_vad_v6_v6.2.onnx", VadEngine::Silero));
const SENSE_VOICE: (&str, AsrEngine) = (
    "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17",
    AsrEngine::SenseVoice,
);
const KOKORO: (&str, TtsEngine, i32) = ("kokoro-multi-lang-v1_0", TtsEngine::Kokoro, 50);
const QWEN3: (&str, AsrEngine) = (
    "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25",
    AsrEngine::Qwen3Asr,
);

fn combos() -> Vec<Combo> {
    let base = |name, vad, asr, tts, smart_turn| Combo {
        name,
        vad,
        asr,
        tts,
        smart_turn,
        denoise: None,
        kws: None,
    };
    vec![
        base("rec", SILERO, SENSE_VOICE, KOKORO, false),
        base("energy", None, SENSE_VOICE, KOKORO, false),
        base(
            "ten",
            Some(("ten-vad.onnx", VadEngine::Ten)),
            SENSE_VOICE,
            KOKORO,
            false,
        ),
        base("qwen3", SILERO, QWEN3, KOKORO, false),
        base(
            "firered",
            SILERO,
            (
                "sherpa-onnx-fire-red-asr2-ctc-zh_en-int8-2026-02-25",
                AsrEngine::FireRedAsr2Ctc,
            ),
            KOKORO,
            false,
        ),
        base(
            "kokoro11",
            SILERO,
            SENSE_VOICE,
            ("kokoro-int8-multi-lang-v1_1", TtsEngine::Kokoro, 3),
            false,
        ),
        base(
            "matcha",
            SILERO,
            SENSE_VOICE,
            ("matcha-icefall-zh-en", TtsEngine::Matcha, 0),
            false,
        ),
        base("smartturn", SILERO, SENSE_VOICE, KOKORO, true),
        Combo {
            denoise: Some("gtcrn_simple.onnx"),
            ..base("denoise", SILERO, SENSE_VOICE, KOKORO, false)
        },
        base("silero6", SILERO6, SENSE_VOICE, KOKORO, false),
        Combo {
            denoise: Some("gtcrn_simple.onnx"),
            ..base("silero6_gtcrn", SILERO6, SENSE_VOICE, KOKORO, false)
        },
        Combo {
            denoise: Some("dpdfnet_baseline.onnx"),
            ..base("silero6_dpdf", SILERO6, SENSE_VOICE, KOKORO, false)
        },
        base("silero6_turn", SILERO6, SENSE_VOICE, KOKORO, true),
        base("silero6_qwen3", SILERO6, QWEN3, KOKORO, false),
        Combo {
            kws: Some(KWS),
            ..base("silero6_kws", SILERO6, SENSE_VOICE, KOKORO, false)
        },
        Combo {
            kws: Some(KWS),
            ..base("silero6_qwen3_kws", SILERO6, QWEN3, KOKORO, false)
        },
    ]
}

/// 云端大模型的样子：等首字，然后每 30 ms 吐一小段。
struct CloudLikeLlm {
    first: Duration,
}

const ANSWER: [&str; 6] = [
    "好的，",
    "激光机现在",
    "空闲。",
    "要我先帮你",
    "对一下焦",
    "吗？",
];

impl Llm for CloudLikeLlm {
    fn chat<'a>(
        &'a self,
        _messages: &'a [ChatMessage],
        _tools: &'a [ToolSpec],
        deltas: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> BoxFuture<'a, Result<LlmReply, ProviderError>> {
        Box::pin(async move {
            tokio::time::sleep(self.first).await;
            let mut text = String::new();
            for part in ANSWER {
                text.push_str(part);
                let _ = deltas.send(part.to_owned());
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            Ok(LlmReply {
                text,
                tool_calls: Vec::new(),
            })
        })
    }
}

/// 与桌面端 `LocalTurnEndPredictor` 相同的接法。
struct SmartTurn(TurnHelper);

impl TurnEndPredictor for SmartTurn {
    fn predict_turn_end<'a>(
        &'a self,
        pcm: &'a [i16],
        sample_rate: u32,
    ) -> BoxFuture<'a, Option<f32>> {
        let helper = self.0.clone();
        let samples = pcm.to_vec();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                helper
                    .predict_blocking(sample_rate, &samples, Duration::from_millis(250))
                    .ok()
                    .map(|(_, prob)| prob)
            })
            .await
            .ok()
            .flatten()
        })
    }
}

fn ms(t0: Instant, at: Instant) -> u64 {
    at.saturating_duration_since(t0).as_millis() as u64
}

fn is_timeout(error: &tungstenite::Error) -> bool {
    matches!(error, tungstenite::Error::Io(e)
        if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}

/// 一轮下行播放（`tts start` … `tts stop`）。
struct Turn {
    start: Instant,
    arrivals: Vec<Instant>,
}

/// 像固件那样说完一段并听完回答；返回这一段的记录和连接是否已被服务端关闭。
fn drive_clip(device: &mut Device, pcm: &[i16], on_ms: u64, off_ms: u64) -> (Value, bool) {
    const FRAME: Duration = Duration::from_millis(60);
    let frames = uplink(pcm);
    let on_index = (on_ms / 60) as usize;
    let off_index = ((off_ms / 60) as usize).min(frames.len() - 1);
    send_json(device, r#"{"type":"listen","state":"start","mode":"auto"}"#);
    let t0 = Instant::now();
    let mut next_send = t0 + FRAME;
    let mut sent = 0usize;
    let (mut on_sent, mut off_sent) = (None, None);
    let mut last_turn_end: Option<Instant> = None;
    let mut speaking: Option<Turn> = None;
    let mut events = Vec::new();
    let mut turns = Vec::new();
    let mut closed = None;
    // 片段放完后固件仍在上行现场声音：循环播放说完半秒后的尾巴（同样的噪声）
    let tail_start = (off_index + 8).min(frames.len() - 1);
    let mut clip_done: Option<Instant> = None;
    loop {
        let now = Instant::now();
        if speaking.is_none() && now >= next_send {
            let index = if sent < frames.len() {
                sent
            } else {
                tail_start + (sent - frames.len()) % (frames.len() - tail_start)
            };
            let frame = encode_frame(1, &frames[index], 0).unwrap();
            if device.send(Message::binary(frame)).is_err() {
                closed = Some(now);
                break;
            }
            if sent == on_index {
                on_sent = Some(now);
            }
            if sent == off_index {
                off_sent = Some(now);
            }
            sent += 1;
            if sent == frames.len() {
                clip_done = Some(now);
            }
            next_send += FRAME;
        }
        if speaking.is_none()
            && let Some(done) = clip_done
        {
            // 听到回答（且回答播完）后再收 1.5 秒看有没有多余的回合；一直没回答，片段放完后最多等 8 秒
            let deadline = last_turn_end
                .filter(|end| off_sent.is_some_and(|off| *end > off))
                .map(|end| end.max(done) + Duration::from_millis(1500))
                .unwrap_or(done + Duration::from_secs(8));
            if now >= deadline {
                break;
            }
        }
        let wait = if speaking.is_none() {
            next_send.saturating_duration_since(now)
        } else {
            Duration::from_millis(20)
        }
        .clamp(Duration::from_millis(1), Duration::from_millis(20));
        device.get_mut().set_read_timeout(Some(wait)).unwrap();
        let message = match device.read() {
            Ok(message) => message,
            Err(error) if is_timeout(&error) => continue,
            Err(_) => {
                closed = Some(Instant::now());
                break;
            }
        };
        let at = Instant::now();
        match message {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                match (value["type"].as_str(), value["state"].as_str()) {
                    (Some("stt"), _) => events.push(json!([
                        ms(t0, at),
                        "stt",
                        value["text"].as_str().unwrap_or_default()
                    ])),
                    (Some("tts"), Some("start")) => {
                        events.push(json!([ms(t0, at), "tts_start", ""]));
                        speaking = Some(Turn {
                            start: at,
                            arrivals: Vec::new(),
                        });
                    }
                    (Some("tts"), Some("sentence_start")) => events.push(json!([
                        ms(t0, at),
                        "say",
                        value["text"].as_str().unwrap_or_default()
                    ])),
                    (Some("tts"), Some("stop")) => {
                        events.push(json!([ms(t0, at), "tts_stop", ""]));
                        if let Some(turn) = speaking.take() {
                            let heard = replay_like_firmware(turn.start, &turn.arrivals);
                            let start = ms(t0, turn.start);
                            turns.push(json!({
                                "start": start,
                                "first_audio": start + heard.first_audio.as_millis() as u64,
                                "stalled": heard.stalled.as_millis() as u64,
                                "longest_stall": heard.longest_stall.as_millis() as u64,
                                "dropped": heard.dropped,
                                "max_queue": heard.max_queue,
                                "frames": heard.frames,
                                "end": ms(t0, at),
                            }));
                        }
                        last_turn_end = Some(at);
                        // 固件自动模式：播完回到聆听；还没说完的部分接着上行
                        if device
                            .send(Message::text(
                                r#"{"type":"listen","state":"start","mode":"auto"}"#,
                            ))
                            .is_err()
                        {
                            closed = Some(at);
                            break;
                        }
                        next_send = at + FRAME;
                    }
                    (Some(kind @ ("llm" | "mcp")), _) => {
                        let _ = kind;
                    }
                    (Some(kind), state) => {
                        events.push(json!([ms(t0, at), kind, state.unwrap_or_default()]))
                    }
                    _ => {}
                }
            }
            Message::Binary(_) => {
                if let Some(turn) = speaking.as_mut() {
                    turn.arrivals.push(at);
                }
            }
            Message::Close(_) => {
                closed = Some(at);
                break;
            }
            _ => {}
        }
    }
    let record = json!({
        "on_sent": on_sent.map(|at| ms(t0, at)),
        "off_sent": off_sent.map(|at| ms(t0, at)),
        "events": events,
        "turns": turns,
        "closed": closed.map(|at| ms(t0, at)),
    });
    (record, closed.is_some())
}

fn read_clip(dir: &std::path::Path, file: &str) -> Vec<i16> {
    std::fs::read(dir.join(file))
        .unwrap()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect()
}

fn run_combo(
    combo: &Combo,
    exe: &std::path::Path,
    models: &std::path::Path,
    clips_dir: &std::path::Path,
    clips: &[Value],
    llm_first: Duration,
    out: &mut std::fs::File,
) {
    let helper = LocalHelper::spawn(exe).unwrap();
    if let Some((file, engine)) = combo.vad {
        helper
            .load_vad(&models.join(file), VadParams::for_engine(engine), engine)
            .unwrap();
    }
    if let Some(file) = combo.denoise {
        helper.load_denoiser(Some(&models.join(file))).unwrap();
    }
    if let Some(dir) = combo.kws {
        helper.load_kws(Some(&models.join(dir))).unwrap();
    }
    // 与桌面端一致：束搜索 + 行业热词
    helper
        .load_asr_with_options(
            &models.join(combo.asr.0),
            combo.asr.1,
            moss_voice_local_proto::AsrDecodeOptions {
                beam_search: true,
                hotwords: moss_voice::settings::effective_hotwords(&[], &[]),
            },
        )
        .unwrap();
    helper
        .load_tts(&models.join(combo.tts.0), combo.tts.1)
        .unwrap();
    let mut conversation = Conversation::new(
        Arc::new(LocalAsr {
            helper: helper.clone(),
        }),
        Arc::new(CloudLikeLlm { first: llm_first }),
        Arc::new(LocalTts {
            helper: helper.clone(),
            speaker: combo.tts.2,
            speed: 1.0,
        }),
        Arc::new(FakeTools::default()),
        ConversationSettings {
            // 与桌面端默认一致：开启内置同音纠错
            asr_corrections: moss_voice::settings::effective_asr_corrections(true, &[]),
            command_keywords: combo.kws.is_some(),
            ..ConversationSettings::default()
        },
    );
    if combo.asr.1.streaming() {
        conversation = conversation.with_streaming_asr(Arc::new(LocalStreamingAsr {
            helper: helper.clone(),
        }));
    }
    let turn_helper = combo.smart_turn.then(|| {
        TurnHelper::spawn(
            exe,
            Some(&models.join("smart-turn-v3/smart-turn-v3.2-cpu.onnx")),
        )
        .unwrap()
    });
    if let Some(worker) = &turn_helper {
        conversation = conversation.with_turn_predictor(Arc::new(SmartTurn(worker.clone())));
    }
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            vad_factory: combo.vad.map(|_| {
                Arc::new(crate::LocalVadFactory {
                    helper: helper.clone(),
                }) as Arc<dyn crate::VadFactory>
            }),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    server.prewarm_speech_cache();
    let connect = || {
        let device = chat_device(&server, &registry, 1);
        device
            .get_ref()
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        device
    };
    // 预热一段（模型首次推理、合成缓存），不计入结果；片段集只有噪声档时用第一段普通句子
    let mut device = connect();
    let warm = clips
        .iter()
        .find(|clip| clip["note"] == "" && clip["level"] == 0)
        .or_else(|| clips.iter().find(|clip| clip["note"] == ""))
        // 只有命令词片段时：用不会断开连接的停止词片段预热
        .or_else(|| clips.iter().find(|clip| clip["note"] != "exit"))
        .unwrap();
    let _ = drive_clip(
        &mut device,
        &read_clip(clips_dir, warm["file"].as_str().unwrap()),
        warm["on_ms"].as_u64().unwrap(),
        warm["off_ms"].as_u64().unwrap(),
    );
    let mut level = None;
    for clip in clips {
        if level != Some(clip["level"].clone()) {
            // 每个噪声档换一条新连接（像设备重新唤醒）
            level = Some(clip["level"].clone());
            device = connect();
        }
        let pcm = read_clip(clips_dir, clip["file"].as_str().unwrap());
        let clip_started = crate::unix_now_ms();
        let (mut record, closed) = drive_clip(
            &mut device,
            &pcm,
            clip["on_ms"].as_u64().unwrap(),
            clip["off_ms"].as_u64().unwrap(),
        );
        for key in ["name", "level", "ref", "note"] {
            record[key] = clip[key].clone();
        }
        record["combo"] = json!(combo.name);
        // 各回合的分段耗时（相对开始拾音，毫秒），看“说完到出声”花在哪
        record["timings"] = json!(
            server
                .status()
                .timings
                .iter()
                .filter(|timing| timing.at_ms >= clip_started)
                .map(|timing| &timing.stages)
                .collect::<Vec<_>>()
        );
        let heard: Vec<&str> = record["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event[1] == "stt")
            .filter_map(|event| event[2].as_str())
            .collect();
        println!(
            "[{}] {:>5} {:<12} 识别 {:?} 回合 {} 断开 {}",
            combo.name,
            clip["level"],
            clip["name"].as_str().unwrap(),
            heard,
            record["turns"].as_array().unwrap().len(),
            closed
        );
        if std::env::var_os("MOSS_CHAIN_EVENTS").is_some() {
            let events = server.status().events;
            for event in events.iter().rev().take(30).rev() {
                println!("    · {}", event.text);
            }
        }
        writeln!(out, "{record}").unwrap();
        if closed {
            device = connect();
        }
    }
    drop(device);
    drop(server);
    if let Some(worker) = turn_helper {
        worker.terminate();
    }
    helper.terminate();
}

#[test]
#[ignore]
fn real_local_chain_matrix() {
    let exe = std::path::PathBuf::from(std::env::var("MOSS_VOICE_LOCAL_EXE").unwrap());
    let models = std::path::PathBuf::from(std::env::var("MOSS_LOCAL_MODELS").unwrap());
    let clips_dir = std::path::PathBuf::from(std::env::var("MOSS_CHAIN_CLIPS").unwrap());
    let clips: Vec<Value> =
        serde_json::from_slice(&std::fs::read(clips_dir.join("meta.json")).unwrap()).unwrap();
    let only = std::env::var("MOSS_CHAIN_ONLY").ok();
    let llm_first = Duration::from_millis(
        std::env::var("MOSS_CHAIN_LLM_FIRST_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(500),
    );
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::var("MOSS_CHAIN_OUT").unwrap())
        .unwrap();
    for combo in combos() {
        if only
            .as_deref()
            .is_some_and(|only| !only.split(',').any(|name| name == combo.name))
        {
            continue;
        }
        run_combo(
            &combo, &exe, &models, &clips_dir, &clips, llm_first, &mut out,
        );
    }
}

/// 命令词检测调参（不在常规测试中运行）：不走整条链路，直接把片段按 60 ms 送进“判停 + 命令词”流，
/// 数命令词片段里认出了几个、普通说话片段里误报了几个（`MOSS_KWS_OPUS` 时先经上行 Opus 编解码）。`MOSS_KWS_SUFFIX` 追加到每行拼音后
/// （如 ` :2.0 #0.10`：加权与阈值），`MOSS_CHAIN_CLIPS` 可用逗号分隔多个片段目录。
/// ```text
/// MOSS_VOICE_LOCAL_EXE=… MOSS_LOCAL_MODELS=… MOSS_CHAIN_CLIPS=<命令词>,<普通> [MOSS_KWS_SUFFIX=…] \
/// cargo test -p moss-voice-server real_kws_tuning -- --ignored --nocapture
/// ```
#[test]
#[ignore]
fn real_kws_tuning() {
    let exe = std::path::PathBuf::from(std::env::var("MOSS_VOICE_LOCAL_EXE").unwrap());
    let models = std::path::PathBuf::from(std::env::var("MOSS_LOCAL_MODELS").unwrap());
    let suffix = std::env::var("MOSS_KWS_SUFFIX").unwrap_or_default();
    let helper = LocalHelper::spawn(&exe).unwrap();
    helper
        .load_vad(
            &models.join(SILERO6.unwrap().0),
            moss_voice_local_proto::VadParams::for_engine(VadEngine::Silero),
            VadEngine::Silero,
        )
        .unwrap();
    let keywords: String = moss_voice_local_proto::COMMAND_KEYWORDS
        .iter()
        .map(|(tokens, label, _)| format!("{tokens}{suffix} @{label}\n"))
        .collect();
    helper
        .load_kws_with(Some(&models.join(KWS)), &keywords)
        .unwrap();
    let (mut hit, mut commands, mut false_alarms, mut normal) = (0, 0, 0, 0);
    for dir in std::env::var("MOSS_CHAIN_CLIPS").unwrap().split(',') {
        let dir = std::path::PathBuf::from(dir);
        let clips: Vec<Value> =
            serde_json::from_slice(&std::fs::read(dir.join("meta.json")).unwrap()).unwrap();
        for (index, clip) in clips.iter().enumerate() {
            let mut pcm = read_clip(&dir, clip["file"].as_str().unwrap());
            if std::env::var_os("MOSS_KWS_OPUS").is_some() {
                // 与设备上行一致：先经参考 libopus 16 kHz VOIP 编解码
                let mut decoder = crate::audio::codec::OpusDecoder::new(16_000).unwrap();
                pcm = uplink(&pcm)
                    .iter()
                    .flat_map(|frame| decoder.decode(frame).unwrap())
                    .collect();
            }
            let stream = 900_000 + index as u64;
            helper.vad_open(stream).unwrap();
            let mut heard = Vec::new();
            for chunk in pcm.chunks(960) {
                heard.extend(helper.vad_feed_with_keywords(stream, chunk).unwrap().1);
            }
            helper.vad_close(stream);
            let note = clip["note"].as_str().unwrap_or("");
            let expected = match note {
                "stop" => Some(moss_voice_local_proto::CommandKeyword::Stop),
                "exit" => Some(moss_voice_local_proto::CommandKeyword::Exit),
                _ => None,
            };
            let kinds: Vec<_> = heard
                .iter()
                .filter_map(|label| moss_voice_local_proto::command_keyword_kind(label))
                .collect();
            match expected {
                Some(kind) => {
                    commands += 1;
                    if kinds.contains(&kind) {
                        hit += 1;
                    } else {
                        println!("没认出 {} {} {heard:?}", clip["level"], clip["name"]);
                    }
                }
                None => {
                    normal += 1;
                    if !kinds.is_empty() {
                        false_alarms += 1;
                        println!("误报 {} {} {heard:?}", clip["level"], clip["name"]);
                    }
                }
            }
        }
    }
    println!("KWS[{suffix}] 命令词认出 {hit}/{commands}，普通说话误报 {false_alarms}/{normal}");
}
