//! 用进程内的假辅助程序（本机 TCP 连接充当标准输入输出）测试客户端；
//! 另有一项 `#[ignore]` 测试调用真实辅助程序与模型（见文末说明）。

use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::*;

#[test]
fn asr_decode_options_cross_helper_protocol() {
    let helper = fake(|request, _| {
        let Op::LoadAsr {
            engine, options, ..
        } = request.op
        else {
            panic!("unexpected operation")
        };
        assert_eq!(engine, AsrEngine::StreamingTransducer);
        assert!(!options.beam_search);
        assert_eq!(options.hotwords, vec!["椴木".to_owned()]);
        Some((
            Reply::Loaded {
                sample_rate: None,
                speakers: None,
            },
            vec![],
        ))
    });
    helper
        .load_asr_with_options(
            Path::new("fake"),
            AsrEngine::StreamingTransducer,
            AsrDecodeOptions {
                beam_search: false,
                hotwords: vec!["椴木".into()],
            },
        )
        .unwrap();
}

#[tokio::test]
async fn synthesis_stream_keeps_pending_request_until_done_and_starts_early() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    let worker = std::thread::spawn(move || {
        let (request, _) = read_frame::<_, Request>(&mut server).unwrap().unwrap();
        assert!(matches!(request.op, Op::SynthesizeStream { .. }));
        for value in [1, 2, 3] {
            if value > 1 {
                std::thread::sleep(Duration::from_millis(70));
            }
            write_frame(
                &mut server,
                &Response {
                    id: request.id,
                    reply: Reply::AudioChunk { sample_rate: 24000 },
                },
                &pcm_to_bytes(&[value; 240]),
            )
            .unwrap();
        }
        write_frame(
            &mut server,
            &Response {
                id: request.id,
                reply: Reply::Done,
            },
            &[],
        )
        .unwrap();
    });
    let helper = LocalHelper::from_streams(Box::new(client.try_clone().unwrap()), Box::new(client));
    let tts = LocalTts {
        helper: helper.clone(),
        speaker: 0,
        speed: 1.0,
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move { tts.synthesize_stream("长句测试", tx).await });
    let first = rx.recv().await.unwrap();
    assert_eq!(first.samples, vec![1; 240]);
    assert!(!task.is_finished(), "首段到达时整句尚未完成");
    assert_eq!(rx.recv().await.unwrap().samples, vec![2; 240]);
    assert_eq!(rx.recv().await.unwrap().samples, vec![3; 240]);
    task.await.unwrap().unwrap();
    worker.join().unwrap();
    assert!(helper.inner.pending.lock().unwrap().is_empty());
}

/// 流式合成的接收方没了（对话被打断或合并重来）：客户端通知辅助程序放弃这一句。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abandoned_stream_synthesis_is_cancelled_in_the_helper() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    let worker = std::thread::spawn(move || {
        let (request, _) = read_frame::<_, Request>(&mut server).unwrap().unwrap();
        assert!(matches!(request.op, Op::SynthesizeStream { .. }));
        write_frame(
            &mut server,
            &Response {
                id: request.id,
                reply: Reply::AudioChunk { sample_rate: 24000 },
            },
            &pcm_to_bytes(&[1; 240]),
        )
        .unwrap();
        // 这一句还没合成完（不发 Done）：等客户端的取消
        let (cancel, _) = read_frame::<_, Request>(&mut server).unwrap().unwrap();
        (request.id, cancel.op)
    });
    let helper = LocalHelper::from_streams(Box::new(client.try_clone().unwrap()), Box::new(client));
    let tts = LocalTts {
        helper: helper.clone(),
        speaker: 0,
        speed: 1.0,
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move { tts.synthesize_stream("很长的一句话", tx).await });
    assert_eq!(rx.recv().await.unwrap().samples, vec![1; 240]);
    drop(rx);
    task.await.unwrap().unwrap();
    let (synth_id, cancel) = worker.join().unwrap();
    assert_eq!(cancel, Op::Cancel { target: synth_id });
    assert!(helper.inner.pending.lock().unwrap().is_empty());
}

/// 真实辅助程序与 Kokoro 的合成速度（本机调优用）：
/// `MOSS_VOICE_LOCAL_EXE=<moss-voice-local> MOSS_LOCAL_TTS_DIR=<Kokoro 模型目录> [MOSS_VOICE_THREADS=n]
///  cargo test -p moss-voice-providers real_local_tts_latency -- --ignored --nocapture`
/// 打印加载耗时、每句“首段音频延迟 / 总耗时 / 音频时长 / 实时率”，以及取消后下一句的等待时间。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn real_local_tts_latency() {
    let exe = std::env::var("MOSS_VOICE_LOCAL_EXE").expect("MOSS_VOICE_LOCAL_EXE");
    let dir =
        std::path::PathBuf::from(std::env::var("MOSS_LOCAL_TTS_DIR").expect("MOSS_LOCAL_TTS_DIR"));
    let helper = LocalHelper::spawn(Path::new(&exe)).unwrap();
    let started = Instant::now();
    let info = helper.load_tts(&dir, TtsEngine::Kokoro).unwrap();
    println!(
        "加载（含预热）{:?}，{} Hz",
        started.elapsed(),
        info.sample_rate
    );
    let tts = Arc::new(LocalTts {
        helper: helper.clone(),
        speaker: 3,
        speed: 1.0,
    });
    let speak = |text: &'static str| {
        let tts = tts.clone();
        async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let started = Instant::now();
            let task = tokio::spawn(async move { tts.synthesize_stream(text, tx).await });
            let (mut first, mut samples) = (None, 0usize);
            // 从第一段到达起按实时播放：后面的段到得比播放进度晚多少，就是听得见的停顿
            let (mut play_until, mut stalled, mut chunks) = (None::<Instant>, Duration::ZERO, 0);
            while let Some(chunk) = rx.recv().await {
                first.get_or_insert_with(|| started.elapsed());
                samples += chunk.samples.len();
                chunks += 1;
                let now = Instant::now();
                let begin = match play_until {
                    Some(until) if until >= now => until,
                    Some(until) => {
                        stalled += now - until;
                        now
                    }
                    None => now,
                };
                play_until = Some(
                    begin
                        + Duration::from_secs_f64(
                            chunk.samples.len() as f64 / f64::from(chunk.sample_rate),
                        ),
                );
            }
            task.await.unwrap().unwrap();
            let total = started.elapsed();
            let audio = samples as f64 / f64::from(info.sample_rate);
            println!(
                "{:>2} 字｜{chunks} 段｜首段 {:>5} ms｜播放中停顿 {:>4} ms｜总 {:>5} ms｜音频 {:.2} s｜实时率 {:.2}｜{text}",
                text.chars().count(),
                first.unwrap_or_default().as_millis(),
                stalled.as_millis(),
                total.as_millis(),
                audio,
                total.as_secs_f64() / audio.max(0.001),
            );
        }
    };
    for text in [
        "好的。",
        "现在是下午三点。",
        "激光机已经连接好了，",
        "椴木三毫米建议用百分之八十功率，速度每分钟六百毫米。",
        "我先把材料参数调出来，然后生成一个预览给你看看，确认没问题再开始雕刻。",
        "先把材料放平整，再把防护盖轻轻关好，然后在电脑上确认一下功率和速度这两个参数，没有问题的话就可以按下开始键让机器动起来了。",
    ] {
        speak(text).await;
    }
    // 取消：一句长话刚开始合成就放弃，紧接着的短句应当几乎不用等
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let long = tts.clone();
    let abandoned = tokio::spawn(async move {
        long.synthesize_stream(
            "这是一段会被放弃的很长的话，它有很多很多字，如果不取消的话，后面那一句就要等它全部合成完才能开始，听起来就像卡住了一样。",
            tx,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    rx.close();
    drop(rx);
    let started = Instant::now();
    speak("好的。").await;
    println!("放弃长句后，下一句从请求到合成完 {:?}", started.elapsed());
    let _ = abandoned.await;
}

/// 假辅助程序：按脚本回应请求。返回客户端。
fn fake(
    respond: impl Fn(Request, Vec<u8>) -> Option<(Reply, Vec<u8>)> + Send + 'static,
) -> LocalHelper {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    std::thread::spawn(move || {
        let mut reader = server.try_clone().unwrap();
        let mut writer = server;
        while let Ok(Some((request, payload))) = read_frame::<_, Request>(&mut reader) {
            let id = request.id;
            if let Some((reply, out)) = respond(request, payload)
                && write_frame(&mut writer, &Response { id, reply }, &out).is_err()
            {
                return;
            }
        }
    });
    LocalHelper::from_streams(Box::new(client.try_clone().unwrap()), Box::new(client))
}

#[test]
fn requests_and_replies_are_matched_by_id() {
    let helper = fake(|request, payload| match request.op {
        Op::Hello => Some((
            Reply::Hello {
                protocol: PROTOCOL_VERSION,
                version: "t".into(),
                vad: false,
                asr: false,
                tts: false,
                denoiser: false,
                smart_turn: false,
            },
            vec![],
        )),
        Op::Transcribe { sample_rate } => {
            let samples = bytes_to_pcm(&payload).len();
            Some((
                Reply::Text {
                    text: format!("{sample_rate}:{samples}"),
                },
                vec![],
            ))
        }
        Op::Synthesize { text, speaker, .. } => Some((
            Reply::Audio {
                sample_rate: 24_000,
            },
            pcm_to_bytes(&vec![speaker as i16; text.chars().count()]),
        )),
        Op::LoadTts { .. } => Some((
            Reply::Loaded {
                sample_rate: Some(24_000),
                speakers: Some(53),
            },
            vec![],
        )),
        Op::VadFeed { .. } => Some((
            Reply::VadEvents {
                events: vec![VadEventKind::SpeechStart],
            },
            vec![],
        )),
        Op::LoadAsr { .. } => Some((
            Reply::Error {
                message: "模型文件不存在：x".into(),
            },
            vec![],
        )),
        _ => Some((Reply::Done, vec![])),
    });
    helper.hello().unwrap();
    assert_eq!(
        helper.transcribe_blocking(&[1, 2, 3], 16_000).unwrap(),
        "16000:3"
    );
    let pcm = helper.synthesize_blocking("你好吗", 50, 1.0).unwrap();
    assert_eq!(
        pcm,
        Pcm {
            samples: vec![50; 3],
            sample_rate: 24_000
        }
    );
    assert_eq!(
        helper.load_tts(Path::new("k"), TtsEngine::Kokoro).unwrap(),
        TtsInfo {
            sample_rate: 24_000,
            speakers: 53
        }
    );
    assert_eq!(
        helper.vad_feed(1, &[0; 960]).unwrap(),
        vec![VadEventKind::SpeechStart]
    );
    assert_eq!(
        helper
            .load_asr(Path::new("x"), AsrEngine::SenseVoice)
            .unwrap_err()
            .message,
        "模型文件不存在：x",
        "辅助程序的错误原样返回"
    );
    // 并发：多个线程同时请求，各自拿到自己的结果
    let handles: Vec<_> = (0..8)
        .map(|index| {
            let helper = helper.clone();
            std::thread::spawn(move || helper.transcribe_blocking(&vec![0; index], 8_000).unwrap())
        })
        .collect();
    for (index, handle) in handles.into_iter().enumerate() {
        assert_eq!(handle.join().unwrap(), format!("8000:{index}"));
    }
}

#[test]
fn protocol_mismatch_is_reported() {
    let helper = fake(|_, _| {
        Some((
            Reply::Hello {
                protocol: 99,
                version: "x".into(),
                vad: false,
                asr: false,
                tts: false,
                denoiser: false,
                smart_turn: false,
            },
            vec![],
        ))
    });
    assert!(
        helper
            .hello()
            .unwrap_err()
            .to_string()
            .contains("版本不匹配")
    );
}

#[test]
fn speaker_response_validates_identity_generation_dimension_and_f32_payload() {
    use moss_voice_local_proto::{
        SPEAKER_DIMENSION, SPEAKER_MODEL_ID, SPEAKER_SAMPLE_RATE, embedding_to_bytes,
    };
    for bad in 0..8 {
        let helper = fake(move |request, payload| {
            let Op::SpeakerEmbed {
                model_id,
                sample_rate,
                utterance_generation,
            } = request.op
            else {
                panic!("unexpected request")
            };
            assert_eq!(model_id, SPEAKER_MODEL_ID);
            assert_eq!(sample_rate, 16000);
            assert_eq!(payload.len(), 64000);
            let mut bytes = embedding_to_bytes(&vec![1.; 192]).unwrap();
            if bad == 6 {
                bytes.pop();
            }
            if bad == 7 {
                bytes[..4].copy_from_slice(&f32::NAN.to_le_bytes());
            }
            Some((
                Reply::Embedding {
                    model_id: if bad == 1 { "wrong".into() } else { model_id },
                    sample_rate: if bad == 2 { 24000 } else { SPEAKER_SAMPLE_RATE },
                    dimension: if bad == 3 { 64 } else { SPEAKER_DIMENSION },
                    utterance_generation: if bad == 4 {
                        utterance_generation + 1
                    } else {
                        utterance_generation
                    },
                    format: if bad == 5 {
                        "i16le".into()
                    } else {
                        "f32le".into()
                    },
                },
                bytes,
            ))
        });
        let result = helper.speaker_embedding_blocking(&vec![100; 32000], 16000, 7);
        if bad == 0 {
            assert_eq!(result.unwrap().len(), 192);
        } else {
            assert!(result.is_err(), "invalid reply {bad} accepted");
        }
        assert!(
            helper
                .speaker_embedding_blocking(&[1; 20], 16000, 7)
                .is_err()
        );
        assert!(
            helper
                .speaker_embedding_blocking(&vec![1; 32000], 24000, 7)
                .is_err()
        );
    }
}

#[test]
fn helper_exit_fails_waiting_and_later_requests_immediately() {
    // 收到识别请求后直接退出（不回应）
    let helper = fake(|request, _| match request.op {
        Op::Transcribe { .. } => std::panic::panic_any("退出"),
        _ => Some((Reply::Done, vec![])),
    });
    let started = Instant::now();
    assert_eq!(
        helper.transcribe_blocking(&[0; 10], 16_000).unwrap_err(),
        dead()
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "不等到 30 秒超时"
    );
    assert!(!helper.is_alive());
    assert_eq!(helper.vad_feed(1, &[0; 10]).unwrap_err(), dead());
}

#[test]
fn speaker_timeout_does_not_invalidate_independent_voice_helper() {
    let voice = fake(|request, _| match request.op {
        Op::VadFeed { .. } => Some((
            Reply::VadEvents {
                events: vec![VadEventKind::SpeechStart],
            },
            vec![],
        )),
        _ => Some((Reply::Done, vec![])),
    });
    let speaker = SpeakerHelper {
        helper: fake(|_, _| None),
    };
    assert!(
        speaker
            .embedding_blocking(&vec![100; 32000], 16000, 8)
            .is_err()
    );
    assert!(!speaker.is_alive());
    assert!(voice.is_alive());
    assert_eq!(
        voice.vad_feed(1, &[0; 960]).unwrap(),
        vec![VadEventKind::SpeechStart]
    );
}

#[test]
fn slow_vad_times_out() {
    let helper = fake(|request, _| {
        if let Op::VadFeed { .. } = request.op {
            std::thread::sleep(Duration::from_millis(900));
        }
        Some((Reply::VadEvents { events: vec![] }, vec![]))
    });
    let error = helper.vad_feed(1, &[0; 10]).unwrap_err();
    assert!(error.message.contains("超时"), "{error}");
    assert!(
        !helper.is_alive(),
        "等待超时可能是管道阻塞，旧辅助程序不可继续使用"
    );
    assert_eq!(helper.vad_feed(1, &[0; 10]).unwrap_err(), dead());
}

#[test]
fn missing_executable_is_reported() {
    let error = LocalHelper::spawn(Path::new("Z:/no/such/moss-voice-local.exe"))
        .err()
        .unwrap();
    assert!(error.message.contains("未找到本地模型程序"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_wrappers_use_blocking_threads() {
    let helper = fake(|request, _| match request.op {
        Op::Transcribe { .. } => Some((
            Reply::Text {
                text: "打开".into(),
            },
            vec![],
        )),
        Op::Synthesize { .. } => Some((
            Reply::Audio {
                sample_rate: 24_000,
            },
            pcm_to_bytes(&[7, 7]),
        )),
        _ => Some((Reply::Done, vec![])),
    });
    let asr = LocalAsr {
        helper: helper.clone(),
    };
    assert_eq!(asr.transcribe(&[0; 160], 16_000).await.unwrap(), "打开");
    let tts = LocalTts {
        helper,
        speaker: 3,
        speed: 1.0,
    };
    assert_eq!(tts.synthesize("嗯").await.unwrap().samples, vec![7, 7]);
}

/// 假辅助程序的流式识别：记录每路收到的请求，结束时回送该路收到的样本总数。
fn fake_streaming() -> (LocalHelper, Arc<Mutex<Vec<String>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let record = log.clone();
    let totals = Arc::new(Mutex::new(HashMap::<u64, usize>::new()));
    let helper = fake(move |request, payload| {
        let entry = match request.op {
            Op::AsrOpen { stream } => {
                totals.lock().unwrap().insert(stream, 0);
                format!("open {stream}")
            }
            Op::AsrFeed { stream } => {
                *totals.lock().unwrap().get_mut(&stream).unwrap() += bytes_to_pcm(&payload).len();
                format!("feed {stream}")
            }
            Op::AsrFinish { stream } => {
                let total = totals.lock().unwrap().remove(&stream).unwrap();
                record.lock().unwrap().push(format!("finish {stream}"));
                return Some((
                    Reply::Text {
                        text: format!("收到{total}"),
                    },
                    vec![],
                ));
            }
            Op::AsrClose { stream } => format!("close {stream}"),
            _ => String::new(),
        };
        record.lock().unwrap().push(entry);
        Some((Reply::Done, vec![]))
    });
    (helper, log)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_streaming_asr_feeds_while_speaking_and_returns_final_text() {
    let (helper, log) = fake_streaming();
    let asr = LocalStreamingAsr { helper };
    let stream = asr.start();
    for _ in 0..5 {
        stream.feed(&[1; 960]);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        stream.finish().await.unwrap(),
        "收到4800",
        "全部音频都送到了同一路"
    );
    let log = log.lock().unwrap().clone();
    assert_eq!(log.first().map(String::as_str), Some("open 1"));
    assert_eq!(log.last().map(String::as_str), Some("finish 1"));
    assert!(log.iter().any(|entry| entry.starts_with("feed")), "{log:?}");
    assert!(
        !log.iter().any(|entry| entry.starts_with("close")),
        "正常结束不再另发关闭"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_local_stream_is_closed_in_helper() {
    let (helper, log) = fake_streaming();
    let asr = LocalStreamingAsr { helper };
    let stream = asr.start();
    stream.feed(&[1; 960]);
    tokio::time::sleep(Duration::from_millis(100)).await;
    // 打断：丢弃识别流
    drop(stream);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !log.lock().unwrap().iter().any(|entry| entry == "close 1") {
        assert!(
            Instant::now() < deadline,
            "辅助程序里的这一路没有关闭：{:?}",
            log.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // 下一路用新编号
    let stream = asr.start();
    stream.feed(&[1; 320]);
    assert_eq!(stream.finish().await.unwrap(), "收到320");
    assert!(log.lock().unwrap().iter().any(|entry| entry == "open 2"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_stream_reports_helper_errors_on_finish() {
    let helper = fake(|request, _| match request.op {
        Op::AsrOpen { .. } => Some((
            Reply::Error {
                message: "识别模型未加载".into(),
            },
            vec![],
        )),
        _ => Some((Reply::Done, vec![])),
    });
    let stream = LocalStreamingAsr { helper }.start();
    stream.feed(&[0; 160]);
    assert_eq!(stream.finish().await.unwrap_err().message, "识别模型未加载");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_streaming_asr_surfaces_periodic_offline_interims_while_ignoring_done_replies() {
    let totals = Arc::new(Mutex::new(HashMap::<u64, usize>::new()));
    let helper = fake(move |request, payload| match request.op {
        Op::AsrOpen { stream } => {
            totals.lock().unwrap().insert(stream, 0);
            Some((Reply::Done, vec![]))
        }
        Op::AsrFeed { stream } => {
            let mut map = totals.lock().unwrap();
            let total = map.get_mut(&stream).unwrap();
            *total += bytes_to_pcm(&payload).len();
            if *total >= 13_120 {
                Some((
                    Reply::Text {
                        text: "今天天气怎么样".into(),
                    },
                    vec![],
                ))
            } else if *total >= 5_120 {
                Some((
                    Reply::Text {
                        text: "今天天气".into(),
                    },
                    vec![],
                ))
            } else {
                Some((Reply::Done, vec![]))
            }
        }
        Op::AsrFinish { stream } => {
            let _ = totals.lock().unwrap().remove(&stream);
            Some((
                Reply::Text {
                    text: "今天天气怎么样？".into(),
                },
                vec![],
            ))
        }
        _ => Some((Reply::Done, vec![])),
    });
    let mut stream = LocalStreamingAsr { helper }.start();
    for _ in 0..4 {
        stream.feed(&[1; 960]);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(stream.latest_interim(), None);

    for _ in 0..2 {
        stream.feed(&[1; 960]);
    }
    let first = tokio::time::timeout(Duration::from_secs(2), stream.next_interim())
        .await
        .expect("first periodic interim");
    assert_eq!(first, "今天天气");

    for _ in 0..9 {
        stream.feed(&[1; 960]);
    }
    let second = tokio::time::timeout(Duration::from_secs(2), stream.next_interim())
        .await
        .expect("second periodic interim");
    assert_eq!(second, "今天天气怎么样");

    assert_eq!(stream.finish().await.unwrap(), "今天天气怎么样？");
}

/// 真实辅助程序 + 模型的冒烟测试（不在常规测试中运行）：
/// `MOSS_VOICE_LOCAL_EXE=<moss-voice-local.exe> MOSS_LOCAL_MODELS=<含 silero_vad.onnx、SenseVoice 与 Kokoro 目录的文件夹>
///  cargo test -p moss-voice-providers real_helper -- --ignored --nocapture`
#[test]
#[ignore]
fn real_helper_smoke() {
    let exe = std::env::var("MOSS_VOICE_LOCAL_EXE").expect("MOSS_VOICE_LOCAL_EXE");
    let models =
        std::path::PathBuf::from(std::env::var("MOSS_LOCAL_MODELS").expect("MOSS_LOCAL_MODELS"));
    let helper = LocalHelper::spawn(Path::new(&exe)).unwrap();
    let started = Instant::now();
    helper
        .load_vad(
            &models.join("silero_vad.onnx"),
            VadParams::default(),
            VadEngine::Silero,
        )
        .unwrap();
    helper
        .load_asr(
            &models.join("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17"),
            AsrEngine::SenseVoice,
        )
        .unwrap();
    let info = helper
        .load_tts(&models.join("kokoro-multi-lang-v1_0"), TtsEngine::Kokoro)
        .unwrap();
    println!(
        "加载三个模型 {:?}，合成 {} Hz、{} 个音色",
        started.elapsed(),
        info.sample_rate,
        info.speakers
    );

    let started = Instant::now();
    let speech = helper
        .synthesize_blocking("现在激光机处于空闲状态。", 50, 1.0)
        .unwrap();
    println!(
        "合成 {:.2} 秒音频用 {:?}",
        speech.samples.len() as f64 / speech.sample_rate as f64,
        started.elapsed()
    );
    assert!(speech.samples.len() > speech.sample_rate as usize / 2);

    // 合成结果重采样到 16 kHz 后识别，再送判停
    let step = speech.sample_rate as f64 / 16_000.0;
    let pcm16: Vec<i16> = (0..(speech.samples.len() as f64 / step) as usize)
        .map(|i| speech.samples[(i as f64 * step) as usize])
        .collect();
    let started = Instant::now();
    let text = helper.transcribe_blocking(&pcm16, 16_000).unwrap();
    println!("识别用 {:?}：{text}", started.elapsed());
    assert!(text.contains("激光") || text.contains("空闲"), "{text}");

    helper.vad_open(1).unwrap();
    let mut padded = vec![0i16; 8_000];
    padded.extend_from_slice(&pcm16);
    padded.extend(vec![0i16; 16_000]);
    let mut events = Vec::new();
    for chunk in padded.chunks(960) {
        events.extend(helper.vad_feed(1, chunk).unwrap());
    }
    println!("判停事件：{events:?}");
    assert_eq!(events.first(), Some(&VadEventKind::SpeechStart));
    assert_eq!(events.last(), Some(&VadEventKind::SpeechEnd));
}

fn to_16k(pcm: &Pcm) -> Vec<i16> {
    let step = pcm.sample_rate as f64 / 16_000.0;
    (0..(pcm.samples.len() as f64 / step) as usize)
        .map(|i| pcm.samples[(i as f64 * step) as usize])
        .collect()
}

/// 粗略的基频（自相关，取有声帧的中位数），用来区分男声 / 女声。
fn median_pitch(pcm: &Pcm) -> f64 {
    let rate = pcm.sample_rate as usize;
    let frame = rate / 25;
    let (min_lag, max_lag) = (rate / 400, rate / 70);
    let mut pitches: Vec<f64> = pcm
        .samples
        .chunks(frame)
        .filter(|chunk| chunk.len() == frame)
        .filter_map(|chunk| {
            let x: Vec<f64> = chunk.iter().map(|&v| v as f64).collect();
            let energy: f64 = x.iter().map(|v| v * v).sum();
            if (energy / frame as f64).sqrt() < 1000.0 {
                return None;
            }
            let (lag, score) = (min_lag..max_lag)
                .map(|lag| {
                    (
                        lag,
                        x.iter().zip(&x[lag..]).map(|(a, b)| a * b).sum::<f64>() / energy,
                    )
                })
                .fold(
                    (0, f64::MIN),
                    |best, item| if item.1 > best.1 { item } else { best },
                );
            (score > 0.4).then(|| rate as f64 / lag as f64)
        })
        .collect();
    pitches.sort_by(|a, b| a.partial_cmp(b).unwrap());
    pitches.get(pitches.len() / 2).copied().unwrap_or(0.0)
}

/// 可选模型（TEN-VAD、SenseVoice 2025、X-ASR 流式、Kokoro v1.1 int8、Matcha 中英）的真实冒烟测试：
/// `MOSS_VOICE_LOCAL_EXE=<moss-voice-local.exe> MOSS_LOCAL_MODELS=<模型目录>
///  cargo test -p moss-voice-providers real_optional_models -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn real_optional_models() {
    let exe = std::env::var("MOSS_VOICE_LOCAL_EXE").expect("MOSS_VOICE_LOCAL_EXE");
    let models =
        std::path::PathBuf::from(std::env::var("MOSS_LOCAL_MODELS").expect("MOSS_LOCAL_MODELS"));
    let helper = LocalHelper::spawn(Path::new(&exe)).unwrap();
    let sentence = "激光机已经回到原点，现在可以开始雕刻。";
    let sid = std::env::var("MOSS_KOKORO11_SID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);

    // Kokoro v1.1 int8：音色范围与男女
    let started = Instant::now();
    let info = helper
        .load_tts(
            &models.join("kokoro-int8-multi-lang-v1_1"),
            TtsEngine::Kokoro,
        )
        .unwrap();
    println!(
        "Kokoro v1.1 加载 {:?}：{} Hz、{} 个音色",
        started.elapsed(),
        info.sample_rate,
        info.speakers
    );
    for probe in [0, 3, 30, 57, 58, 80, 102] {
        let started = Instant::now();
        let pcm = helper
            .synthesize_blocking("你好，我是小智。", probe, 1.0)
            .unwrap();
        println!(
            "  音色 {probe:>3}：基频约 {:.0} Hz，合成用 {:?}",
            median_pitch(&pcm),
            started.elapsed()
        );
    }
    let started = Instant::now();
    let kokoro = helper.synthesize_blocking(sentence, sid, 1.0).unwrap();
    println!(
        "Kokoro v1.1 音色 {sid} 合成 {:.2} 秒音频用 {:?}",
        kokoro.samples.len() as f64 / kokoro.sample_rate as f64,
        started.elapsed()
    );

    // Matcha 中英：单音色，忽略音色编号
    let started = Instant::now();
    let info = helper
        .load_tts(&models.join("matcha-icefall-zh-en"), TtsEngine::Matcha)
        .unwrap();
    println!(
        "Matcha 加载 {:?}：{} Hz、{} 个音色",
        started.elapsed(),
        info.sample_rate,
        info.speakers
    );
    let started = Instant::now();
    let matcha = helper.synthesize_blocking(sentence, 50, 1.0).unwrap();
    println!(
        "Matcha 合成 {:.2} 秒音频用 {:?}，基频约 {:.0} Hz",
        matcha.samples.len() as f64 / matcha.sample_rate as f64,
        started.elapsed(),
        median_pitch(&matcha)
    );
    assert!(matcha.samples.len() > matcha.sample_rate as usize);

    // SenseVoice 2025 整段识别与说话期半秒增量重认两种合成结果
    helper
        .load_asr(
            &models.join("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09"),
            AsrEngine::SenseVoice,
        )
        .unwrap();
    for (name, pcm) in [("Kokoro v1.1", &kokoro), ("Matcha", &matcha)] {
        let started = Instant::now();
        let text = helper.transcribe_blocking(&to_16k(pcm), 16_000).unwrap();
        println!(
            "SenseVoice 2025 识别 {name} 用 {:?}：{text}",
            started.elapsed()
        );
        assert!(text.contains("激光") || text.contains("雕刻"), "{text}");
    }
    let pcm_16k = to_16k(&matcha);
    let stream = helper.next_stream();
    helper.asr_open(stream).unwrap();
    let mut saw_interim = false;
    for chunk in pcm_16k.chunks(960) {
        if let Some(interim) = helper.asr_feed_text(stream, chunk).unwrap()
            && !interim.is_empty()
        {
            saw_interim = true;
        }
    }
    let streamed_text = helper.asr_finish(stream).unwrap();
    assert!(
        saw_interim,
        "SenseVoice 2025 说话期间应产出周期重认中间文字"
    );
    assert!(
        streamed_text.contains("激光") || streamed_text.contains("雕刻"),
        "{streamed_text}"
    );

    // X-ASR 流式：按 60 ms 一段边送边解码，量说完到出字的时间
    let started = Instant::now();
    helper
        .load_asr(
            &models.join("sherpa-onnx-x-asr-480ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05"),
            AsrEngine::StreamingTransducer,
        )
        .unwrap();
    println!("X-ASR 流式加载 {:?}", started.elapsed());
    for (name, pcm) in [("Kokoro v1.1", &kokoro), ("Matcha", &matcha)] {
        let pcm16 = to_16k(pcm);
        let asr = LocalStreamingAsr {
            helper: helper.clone(),
        };
        let stream = asr.start();
        for chunk in pcm16.chunks(960) {
            stream.feed(chunk);
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        let started = Instant::now();
        let text = stream.finish().await.unwrap();
        println!(
            "X-ASR 流式识别 {name}：说完到出字 {:?}：{text}",
            started.elapsed()
        );
        assert!(text.contains("激光") || text.contains("雕刻"), "{text}");
        let started = Instant::now();
        let whole = helper.transcribe_blocking(&pcm16, 16_000).unwrap();
        println!("X-ASR 整段识别 {name} 用 {:?}：{whole}", started.elapsed());
    }

    // TEN-VAD 判停
    helper
        .load_vad(
            &models.join("ten-vad.onnx"),
            VadParams::for_engine(VadEngine::Ten),
            VadEngine::Ten,
        )
        .unwrap();
    let speech = to_16k(&kokoro);
    let mut padded = vec![0i16; 8_000];
    padded.extend_from_slice(&speech);
    padded.extend(vec![0i16; 16_000]);
    helper.vad_open(7).unwrap();
    let mut events = Vec::new();
    for (index, chunk) in padded.chunks(960).enumerate() {
        for event in helper.vad_feed(7, chunk).unwrap() {
            events.push((event, (index + 1) * 60));
        }
    }
    println!(
        "TEN-VAD 事件（送完第几毫秒的音频时报出）：{events:?}，语音在 {} ms 处结束",
        (8_000 + speech.len()) / 16
    );
    assert_eq!(
        events.first().map(|event| event.0),
        Some(VadEventKind::SpeechStart)
    );
    assert_eq!(
        events.last().map(|event| event.0),
        Some(VadEventKind::SpeechEnd)
    );
}

/// 真实 Kokoro 整句合成里各处停顿的实际长度（调停顿常量前先量）：
/// `MOSS_VOICE_LOCAL_EXE=… MOSS_LOCAL_TTS_DIR=… cargo test -p moss-voice-providers real_local_tts_pauses -- --ignored --nocapture`
#[test]
#[ignore]
fn real_local_tts_pauses() {
    let exe = std::env::var("MOSS_VOICE_LOCAL_EXE").expect("MOSS_VOICE_LOCAL_EXE");
    let dir =
        std::path::PathBuf::from(std::env::var("MOSS_LOCAL_TTS_DIR").expect("MOSS_LOCAL_TTS_DIR"));
    let helper = LocalHelper::spawn(Path::new(&exe)).unwrap();
    helper.load_tts(&dir, TtsEngine::Kokoro).unwrap();
    for text in [
        "好的，我先看一下机器的状态。",
        "椴木三毫米建议用百分之八十功率，速度每分钟六百毫米，分两遍切透。",
        "先把材料放平。再关上防护盖！都好了吗？",
    ] {
        let pcm = helper.synthesize_blocking(text, 3, 1.0).unwrap();
        let rate = pcm.sample_rate as usize;
        // 幅度很小且持续 40 ms 以上的一段算一次停顿：（起点 ms，时长 ms）
        let (mut runs, mut begin) = (Vec::new(), None);
        for (index, sample) in pcm.samples.iter().enumerate() {
            match (sample.unsigned_abs() <= 200, begin) {
                (true, None) => begin = Some(index),
                (false, Some(start)) => {
                    if index - start >= rate * 40 / 1000 {
                        runs.push((start * 1000 / rate, (index - start) * 1000 / rate));
                    }
                    begin = None;
                }
                _ => {}
            }
        }
        if let Some(start) = begin {
            runs.push((
                start * 1000 / rate,
                (pcm.samples.len() - start) * 1000 / rate,
            ));
        }
        println!(
            "{:.2} s｜停顿（起点→时长 ms）{:?}｜{text}",
            pcm.samples.len() as f64 / rate as f64,
            runs
        );
    }
}

/// 第一段能切多长由机器速度和播放队列里的存货决定：刚开口、机器慢时保守，机器快或存货多时放宽到整句。
#[test]
fn pacing_relaxes_segmentation_only_when_synthesis_can_keep_up() {
    let now = Instant::now();
    let mut slow = Pacing::default();
    assert_eq!(
        slow.first_max_chars(now),
        0,
        "没量过、没有存货：用最保守的阶梯"
    );
    // 慢机器（1 秒语音算 0.8 秒）刚开口：仍然保守
    for _ in 0..6 {
        slow.delivered(now, Some(Duration::from_millis(1600)), 2.0);
    }
    assert!((0.75..0.85).contains(&slow.rtf), "{}", slow.rtf);
    let mut cold = slow;
    cold.play_until = None;
    assert_eq!(cold.first_max_chars(now), 0);
    // 同一台慢机器，队列里已有 12 秒存货：下一句可以长一些
    assert!(
        slow.first_max_chars(now) >= 40,
        "{}",
        slow.first_max_chars(now)
    );
    // 快机器（Matcha 一类）：刚开口第一段就能放到二十多个字，绝大多数句子不用切
    let mut fast = Pacing::default();
    for _ in 0..8 {
        fast.delivered(now, Some(Duration::from_millis(100)), 2.0);
    }
    fast.play_until = None;
    assert!(
        fast.first_max_chars(now) >= 24,
        "{}",
        fast.first_max_chars(now)
    );
    // 排队等待的时间不计入速度估计
    let before = fast.rtf;
    fast.delivered(now, None, 2.0);
    assert_eq!(fast.rtf, before);
}

/// 真实 Kokoro 连续说一整段回答（像对话里那样一句接一句预取）：
/// `MOSS_VOICE_LOCAL_EXE=… MOSS_LOCAL_TTS_DIR=… cargo test -p moss-voice-providers real_local_tts_turn -- --ignored --nocapture`
/// 打印每句切成几段、整段回答的首声延迟与播放中停顿。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn real_local_tts_turn() {
    let exe = std::env::var("MOSS_VOICE_LOCAL_EXE").expect("MOSS_VOICE_LOCAL_EXE");
    let dir =
        std::path::PathBuf::from(std::env::var("MOSS_LOCAL_TTS_DIR").expect("MOSS_LOCAL_TTS_DIR"));
    let helper = LocalHelper::spawn(Path::new(&exe)).unwrap();
    helper.load_tts(&dir, TtsEngine::Kokoro).unwrap();
    let tts = LocalTts {
        helper: helper.clone(),
        speaker: 3,
        speed: 1.0,
    };
    let turns: [&[&str]; 3] = [
        &[
            "好的，",
            "椴木三毫米我建议用百分之八十功率，速度每分钟六百毫米，",
            "分两遍切透。",
            "开始之前记得把吹气打开，再检查一下焦距，",
            "都准备好了就告诉我。",
        ],
        &[
            "可以的。",
            "先把材料放平整，再把防护盖轻轻关好，然后在电脑上确认一下功率和速度这两个参数，没有问题的话就可以按下开始键让机器动起来了。",
        ],
        &[
            "嗯，",
            "这块板子颜色偏浅的话，可以把速度降下来一点，",
            "不要一味加大功率，功率太高容易把边缘烧黑。",
        ],
    ];
    for (number, sentences) in turns.iter().enumerate() {
        let started = Instant::now();
        let (mut first, mut play_until, mut stalled, mut longest) =
            (None, None::<Instant>, Duration::ZERO, Duration::ZERO);
        let mut shape = Vec::new();
        // 每次停顿发生在哪：（第几句，这一句的第几段，停了多少 ms）
        let mut stalls = Vec::new();
        for (index, sentence) in sentences.iter().enumerate() {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let text = (*sentence).to_owned();
            let job = tts.synthesize_stream(&text, tx);
            tokio::pin!(job);
            let mut chunks = 0;
            let mut done = false;
            loop {
                tokio::select! {
                    result = &mut job, if !done => { result.unwrap(); done = true; }
                    chunk = rx.recv() => {
                        let Some(chunk) = chunk else { break };
                        chunks += 1;
                        first.get_or_insert_with(|| started.elapsed());
                        let now = Instant::now();
                        let begin = match play_until {
                            Some(until) if until >= now => until,
                            Some(until) => {
                                stalled += now - until;
                                longest = longest.max(now - until);
                                stalls.push((index + 1, chunks, (now - until).as_millis()));
                                now
                            }
                            None => now,
                        };
                        play_until = Some(begin + Duration::from_secs_f64(
                            chunk.samples.len() as f64 / f64::from(chunk.sample_rate),
                        ));
                    }
                }
            }
            shape.push(format!("{}字→{chunks}段", sentence.chars().count()));
        }
        println!(
            "第 {} 段回答｜首声 {} ms｜播放中停顿合计 {} ms（最长 {} ms）｜{}｜停顿位置（句，段，ms）{stalls:?}",
            number + 1,
            first.unwrap_or_default().as_millis(),
            stalled.as_millis(),
            longest.as_millis(),
            shape.join("，")
        );
        // 等这一段播完再开始下一段回答
        if let Some(until) = play_until {
            tokio::time::sleep(until.saturating_duration_since(Instant::now())).await;
        }
    }
}

/// 把真实本地合成的原始输出（16-bit 单声道）存成文件，供音量 / 失真 / 编码质量分析：
/// `MOSS_VOICE_LOCAL_EXE=… MOSS_LOCAL_TTS_DIR=… MOSS_TTS_DUMP_DIR=<目录> [MOSS_TTS_DUMP_SID=3]
///  cargo test -p moss-voice-providers real_local_tts_dump -- --ignored --nocapture`
#[test]
#[ignore]
fn real_local_tts_dump() {
    let exe = std::env::var("MOSS_VOICE_LOCAL_EXE").expect("MOSS_VOICE_LOCAL_EXE");
    let dir =
        std::path::PathBuf::from(std::env::var("MOSS_LOCAL_TTS_DIR").expect("MOSS_LOCAL_TTS_DIR"));
    let out =
        std::path::PathBuf::from(std::env::var("MOSS_TTS_DUMP_DIR").expect("MOSS_TTS_DUMP_DIR"));
    let sid = std::env::var("MOSS_TTS_DUMP_SID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);
    let engine = if dir.join("voices.bin").exists() {
        TtsEngine::Kokoro
    } else {
        TtsEngine::Matcha
    };
    std::fs::create_dir_all(&out).unwrap();
    let helper = LocalHelper::spawn(Path::new(&exe)).unwrap();
    helper.load_tts(&dir, engine).unwrap();
    for (index, text) in [
        "好的，我先看一下机器的状态。",
        "椴木三毫米建议用百分之八十功率，速度每分钟六百毫米，分两遍切透。",
        "啊！不行不行，快停下来！",
        "开始之前记得把吹气打开，再检查一下焦距，都准备好了就告诉我。",
        "现在是下午三点二十，激光机已经连接好了，随时可以开始雕刻。",
    ]
    .iter()
    .enumerate()
    {
        let pcm = helper.synthesize_blocking(text, sid, 1.0).unwrap();
        let peak = pcm
            .samples
            .iter()
            .map(|s| s.unsigned_abs())
            .max()
            .unwrap_or(0);
        let rms = (pcm
            .samples
            .iter()
            .map(|s| f64::from(*s).powi(2))
            .sum::<f64>()
            / pcm.samples.len().max(1) as f64)
            .sqrt();
        let path = out.join(format!("sid{sid}-{index}-{}hz.s16", pcm.sample_rate));
        std::fs::write(&path, pcm_to_bytes(&pcm.samples)).unwrap();
        println!(
            "{} Hz｜峰值 {peak}（{:.1} dBFS）｜均方根 {:.0}（{:.1} dBFS）｜{text}",
            pcm.sample_rate,
            20.0 * (f64::from(peak) / 32768.0).log10(),
            rms,
            20.0 * (rms / 32768.0).log10(),
        );
    }
}

#[test]
fn denoiser_and_smart_turn_roundtrip_over_protocol() {
    let helper = fake(|request, payload| match request.op {
        Op::LoadDenoiser { model } => {
            assert_eq!(model, "gtcrn.onnx");
            Some((
                Reply::Loaded {
                    sample_rate: Some(16_000),
                    speakers: None,
                },
                vec![],
            ))
        }
        Op::LoadSmartTurn { model } => {
            assert!(model.is_empty());
            Some((
                Reply::Loaded {
                    sample_rate: Some(16_000),
                    speakers: None,
                },
                vec![],
            ))
        }
        Op::SmartTurnPredict { sample_rate } => {
            assert_eq!(sample_rate, 16_000);
            assert_eq!(bytes_to_pcm(&payload).len(), 320);
            Some((
                Reply::SmartTurn {
                    probability: 0.87,
                    complete: true,
                },
                vec![],
            ))
        }
        _ => Some((Reply::Done, vec![])),
    });
    helper.load_denoiser(Some(Path::new("gtcrn.onnx"))).unwrap();
    helper.load_smart_turn(None).unwrap();
    let (complete, prob) = helper
        .predict_smart_turn(16_000, &vec![42; 320], Duration::from_secs(1))
        .unwrap();
    assert!(complete);
    assert!((prob - 0.87).abs() < 1e-6);
}

#[test]
fn turn_helper_soft_timeout_keeps_worker_alive_until_consecutive_cap_and_isolates_main_helper() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    let server_thread = std::thread::spawn(move || {
        let mut idx = 0usize;
        while let Ok(Some((request, _))) = read_frame::<_, Request>(&mut server) {
            idx += 1;
            let slow = !matches!(idx, 4 | 8);
            if slow {
                std::thread::sleep(Duration::from_millis(60));
            }
            let reply = match request.op {
                Op::SmartTurnPredict { .. } => Reply::SmartTurn {
                    probability: 0.91,
                    complete: true,
                },
                _ => Reply::Done,
            };
            if write_frame(
                &mut server,
                &Response {
                    id: request.id,
                    reply,
                },
                &[],
            )
            .is_err()
            {
                break;
            }
        }
    });

    let raw_turn =
        LocalHelper::from_streams(Box::new(client.try_clone().unwrap()), Box::new(client));
    let turn = TurnHelper::from_loaded_helper(raw_turn, Some(Path::new("smart-turn.onnx")));
    assert_eq!(turn.model_path(), Some(Path::new("smart-turn.onnx")));

    let main_helper = fake(|request, _| match request.op {
        Op::Transcribe { .. } => Some((
            Reply::Text {
                text: "正常识别".into(),
            },
            vec![],
        )),
        _ => Some((Reply::Done, vec![])),
    });

    let pcm = vec![10i16; 320];
    // 连续 3 次超时后仍保持存活，之后 1 次成功将计数清零
    for _ in 0..3 {
        assert!(
            turn.predict_blocking(16_000, &pcm, Duration::from_millis(12))
                .is_err()
        );
        assert!(turn.is_alive());
        std::thread::sleep(Duration::from_millis(80));
    }
    let (complete, prob) = turn
        .predict_blocking(16_000, &pcm, Duration::from_secs(1))
        .unwrap();
    assert!(complete);
    assert!((prob - 0.91).abs() < 1e-6);
    assert!(turn.is_alive());

    // 计数已清零：再连续 3 次超时依然存活
    for _ in 0..3 {
        assert!(
            turn.predict_blocking(16_000, &pcm, Duration::from_millis(12))
                .is_err()
        );
        assert!(turn.is_alive());
        std::thread::sleep(Duration::from_millis(80));
    }
    let _ = turn
        .predict_blocking(16_000, &pcm, Duration::from_secs(1))
        .unwrap();

    // 真正连续 4 次超时才自我终止；主 helper 全程不受影响
    for round in 0..4 {
        assert!(
            turn.predict_blocking(16_000, &pcm, Duration::from_millis(12))
                .is_err()
        );
        if round < 3 {
            assert!(turn.is_alive(), "第 {} 次超时不应提前杀死进程", round + 1);
            std::thread::sleep(Duration::from_millis(80));
        }
    }
    assert!(!turn.is_alive());
    assert!(main_helper.is_alive());
    assert_eq!(
        main_helper.transcribe_blocking(&pcm, 16_000).unwrap(),
        "正常识别"
    );
    drop(turn);
    let _ = server_thread.join();
}
