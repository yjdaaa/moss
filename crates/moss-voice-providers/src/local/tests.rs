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
        let Op::LoadAsr { engine, options, .. } = request.op else { panic!("unexpected operation") };
        assert_eq!(engine, AsrEngine::StreamingTransducer);
        assert!(!options.beam_search);
        assert_eq!(options.hotwords, vec!["椴木".to_owned()]);
        Some((Reply::Loaded { sample_rate: None, speakers: None }, vec![]))
    });
    helper.load_asr_with_options(Path::new("fake"), AsrEngine::StreamingTransducer, AsrDecodeOptions { beam_search:false, hotwords:vec!["椴木".into()] }).unwrap();
}

#[tokio::test]
async fn synthesis_stream_keeps_pending_request_until_done_and_starts_early() {
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();
    let client=TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut server,_)=listener.accept().unwrap();
    let worker=std::thread::spawn(move || {
        let (request,_)=read_frame::<_,Request>(&mut server).unwrap().unwrap();
        assert!(matches!(request.op, Op::SynthesizeStream { .. }));
        for value in [1,2,3] {
            if value > 1 { std::thread::sleep(Duration::from_millis(70)); }
            write_frame(&mut server,&Response{id:request.id,reply:Reply::AudioChunk{sample_rate:24000}},&pcm_to_bytes(&[value;240])).unwrap();
        }
        write_frame(&mut server,&Response{id:request.id,reply:Reply::Done},&[]).unwrap();
    });
    let helper=LocalHelper::from_streams(Box::new(client.try_clone().unwrap()),Box::new(client));
    let tts=LocalTts{helper:helper.clone(),speaker:0,speed:1.0};
    let (tx,mut rx)=tokio::sync::mpsc::unbounded_channel();
    let task=tokio::spawn(async move { tts.synthesize_stream("长句测试",tx).await });
    let first=rx.recv().await.unwrap(); assert_eq!(first.samples,vec![1;240]);
    assert!(!task.is_finished(),"首段到达时整句尚未完成");
    assert_eq!(rx.recv().await.unwrap().samples,vec![2;240]);
    assert_eq!(rx.recv().await.unwrap().samples,vec![3;240]);
    task.await.unwrap().unwrap(); worker.join().unwrap();
    assert!(helper.inner.pending.lock().unwrap().is_empty());
}

/// 假辅助程序：按脚本回应请求。返回客户端。
fn fake(respond: impl Fn(Request, Vec<u8>) -> Option<(Reply, Vec<u8>)> + Send + 'static) -> LocalHelper {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    std::thread::spawn(move || {
        let mut reader = server.try_clone().unwrap();
        let mut writer = server;
        while let Ok(Some((request, payload))) = read_frame::<_, Request>(&mut reader) {
            let id = request.id;
            if let Some((reply, out)) = respond(request, payload) {
                if write_frame(&mut writer, &Response { id, reply }, &out).is_err() {
                    return;
                }
            }
        }
    });
    LocalHelper::from_streams(Box::new(client.try_clone().unwrap()), Box::new(client))
}

#[test]
fn requests_and_replies_are_matched_by_id() {
    let helper = fake(|request, payload| match request.op {
        Op::Hello => Some((Reply::Hello { protocol: PROTOCOL_VERSION, version: "t".into(), vad: false, asr: false, tts: false }, vec![])),
        Op::Transcribe { sample_rate } => {
            let samples = bytes_to_pcm(&payload).len();
            Some((Reply::Text { text: format!("{sample_rate}:{samples}") }, vec![]))
        }
        Op::Synthesize { text, speaker, .. } => {
            Some((Reply::Audio { sample_rate: 24_000 }, pcm_to_bytes(&vec![speaker as i16; text.chars().count()])))
        }
        Op::LoadTts { .. } => Some((Reply::Loaded { sample_rate: Some(24_000), speakers: Some(53) }, vec![])),
        Op::VadFeed { .. } => Some((Reply::VadEvents { events: vec![VadEventKind::SpeechStart] }, vec![])),
        Op::LoadAsr { .. } => Some((Reply::Error { message: "模型文件不存在：x".into() }, vec![])),
        _ => Some((Reply::Done, vec![])),
    });
    helper.hello().unwrap();
    assert_eq!(helper.transcribe_blocking(&[1, 2, 3], 16_000).unwrap(), "16000:3");
    let pcm = helper.synthesize_blocking("你好吗", 50, 1.0).unwrap();
    assert_eq!(pcm, Pcm { samples: vec![50; 3], sample_rate: 24_000 });
    assert_eq!(helper.load_tts(Path::new("k"), TtsEngine::Kokoro).unwrap(), TtsInfo { sample_rate: 24_000, speakers: 53 });
    assert_eq!(helper.vad_feed(1, &[0; 960]).unwrap(), vec![VadEventKind::SpeechStart]);
    assert_eq!(helper.load_asr(Path::new("x"), AsrEngine::SenseVoice).unwrap_err().message, "模型文件不存在：x", "辅助程序的错误原样返回");
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
    let helper = fake(|_, _| Some((Reply::Hello { protocol: 99, version: "x".into(), vad: false, asr: false, tts: false }, vec![])));
    assert!(helper.hello().unwrap_err().to_string().contains("版本不匹配"));
}

#[test]
fn speaker_response_validates_identity_generation_dimension_and_f32_payload(){
    use moss_voice_local_proto::{SPEAKER_MODEL_ID,SPEAKER_SAMPLE_RATE,SPEAKER_DIMENSION,embedding_to_bytes};
    for bad in 0..8 {
        let helper=fake(move|request,payload|{
            let Op::SpeakerEmbed{model_id,sample_rate,utterance_generation}=request.op else{panic!("unexpected request")};
            assert_eq!(model_id,SPEAKER_MODEL_ID);assert_eq!(sample_rate,16000);assert_eq!(payload.len(),64000);
            let mut bytes=embedding_to_bytes(&vec![1.;192]).unwrap();
            if bad==6{bytes.pop();}if bad==7{bytes[..4].copy_from_slice(&f32::NAN.to_le_bytes());}
            Some((Reply::Embedding{model_id:if bad==1{"wrong".into()}else{model_id},sample_rate:if bad==2{24000}else{SPEAKER_SAMPLE_RATE},dimension:if bad==3{64}else{SPEAKER_DIMENSION},utterance_generation:if bad==4{utterance_generation+1}else{utterance_generation},format:if bad==5{"i16le".into()}else{"f32le".into()}},bytes))
        });
        let result=helper.speaker_embedding_blocking(&vec![100;32000],16000,7);
        if bad==0{assert_eq!(result.unwrap().len(),192);}else{assert!(result.is_err(),"invalid reply {bad} accepted");}
        assert!(helper.speaker_embedding_blocking(&[1;20],16000,7).is_err());
        assert!(helper.speaker_embedding_blocking(&vec![1;32000],24000,7).is_err());
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
    assert_eq!(helper.transcribe_blocking(&[0; 10], 16_000).unwrap_err(), dead());
    assert!(started.elapsed() < Duration::from_secs(5), "不等到 30 秒超时");
    assert!(!helper.is_alive());
    assert_eq!(helper.vad_feed(1, &[0; 10]).unwrap_err(), dead());
}

#[test]
fn speaker_timeout_does_not_invalidate_independent_voice_helper() {
    let voice=fake(|request,_| match request.op {
        Op::VadFeed{..}=>Some((Reply::VadEvents{events:vec![VadEventKind::SpeechStart]},vec![])),
        _=>Some((Reply::Done,vec![])),
    });
    let speaker=SpeakerHelper{helper:fake(|_,_|None)};
    assert!(speaker.embedding_blocking(&vec![100;32000],16000,8).is_err());
    assert!(!speaker.is_alive());
    assert!(voice.is_alive());
    assert_eq!(voice.vad_feed(1,&[0;960]).unwrap(),vec![VadEventKind::SpeechStart]);
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
    assert!(!helper.is_alive(), "等待超时可能是管道阻塞，旧辅助程序不可继续使用");
    assert_eq!(helper.vad_feed(1, &[0; 10]).unwrap_err(), dead());
}

#[test]
fn missing_executable_is_reported() {
    let error = LocalHelper::spawn(Path::new("Z:/no/such/moss-voice-local.exe")).err().unwrap();
    assert!(error.message.contains("未找到本地模型程序"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_wrappers_use_blocking_threads() {
    let helper = fake(|request, _| match request.op {
        Op::Transcribe { .. } => Some((Reply::Text { text: "打开".into() }, vec![])),
        Op::Synthesize { .. } => Some((Reply::Audio { sample_rate: 24_000 }, pcm_to_bytes(&[7, 7]))),
        _ => Some((Reply::Done, vec![])),
    });
    let asr = LocalAsr { helper: helper.clone() };
    assert_eq!(asr.transcribe(&[0; 160], 16_000).await.unwrap(), "打开");
    let tts = LocalTts { helper, speaker: 3, speed: 1.0 };
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
                return Some((Reply::Text { text: format!("收到{total}") }, vec![]));
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
    assert_eq!(stream.finish().await.unwrap(), "收到4800", "全部音频都送到了同一路");
    let log = log.lock().unwrap().clone();
    assert_eq!(log.first().map(String::as_str), Some("open 1"));
    assert_eq!(log.last().map(String::as_str), Some("finish 1"));
    assert!(log.iter().any(|entry| entry.starts_with("feed")), "{log:?}");
    assert!(!log.iter().any(|entry| entry.starts_with("close")), "正常结束不再另发关闭");
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
        assert!(Instant::now() < deadline, "辅助程序里的这一路没有关闭：{:?}", log.lock().unwrap());
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
        Op::AsrOpen { .. } => Some((Reply::Error { message: "当前识别模型不支持流式识别".into() }, vec![])),
        _ => Some((Reply::Done, vec![])),
    });
    let stream = LocalStreamingAsr { helper }.start();
    stream.feed(&[0; 160]);
    assert_eq!(stream.finish().await.unwrap_err().message, "当前识别模型不支持流式识别");
}

/// 真实辅助程序 + 模型的冒烟测试（不在常规测试中运行）：
/// `MOSS_VOICE_LOCAL_EXE=<moss-voice-local.exe> MOSS_LOCAL_MODELS=<含 silero_vad.onnx、SenseVoice 与 Kokoro 目录的文件夹>
///  cargo test -p moss-voice-providers real_helper -- --ignored --nocapture`
#[test]
#[ignore]
fn real_helper_smoke() {
    let exe = std::env::var("MOSS_VOICE_LOCAL_EXE").expect("MOSS_VOICE_LOCAL_EXE");
    let models = std::path::PathBuf::from(std::env::var("MOSS_LOCAL_MODELS").expect("MOSS_LOCAL_MODELS"));
    let helper = LocalHelper::spawn(Path::new(&exe)).unwrap();
    let started = Instant::now();
    helper.load_vad(&models.join("silero_vad.onnx"), VadParams::default(), VadEngine::Silero).unwrap();
    helper.load_asr(&models.join("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17"), AsrEngine::SenseVoice).unwrap();
    let info = helper.load_tts(&models.join("kokoro-multi-lang-v1_0"), TtsEngine::Kokoro).unwrap();
    println!("加载三个模型 {:?}，合成 {} Hz、{} 个音色", started.elapsed(), info.sample_rate, info.speakers);

    let started = Instant::now();
    let speech = helper.synthesize_blocking("现在激光机处于空闲状态。", 50, 1.0).unwrap();
    println!("合成 {:.2} 秒音频用 {:?}", speech.samples.len() as f64 / speech.sample_rate as f64, started.elapsed());
    assert!(speech.samples.len() > speech.sample_rate as usize / 2);

    // 合成结果重采样到 16 kHz 后识别，再送判停
    let step = speech.sample_rate as f64 / 16_000.0;
    let pcm16: Vec<i16> = (0..(speech.samples.len() as f64 / step) as usize).map(|i| speech.samples[(i as f64 * step) as usize]).collect();
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
    (0..(pcm.samples.len() as f64 / step) as usize).map(|i| pcm.samples[(i as f64 * step) as usize]).collect()
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
                .map(|lag| (lag, x.iter().zip(&x[lag..]).map(|(a, b)| a * b).sum::<f64>() / energy))
                .fold((0, f64::MIN), |best, item| if item.1 > best.1 { item } else { best });
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
    let models = std::path::PathBuf::from(std::env::var("MOSS_LOCAL_MODELS").expect("MOSS_LOCAL_MODELS"));
    let helper = LocalHelper::spawn(Path::new(&exe)).unwrap();
    let sentence = "激光机已经回到原点，现在可以开始雕刻。";
    let sid = std::env::var("MOSS_KOKORO11_SID").ok().and_then(|value| value.parse().ok()).unwrap_or(3);

    // Kokoro v1.1 int8：音色范围与男女
    let started = Instant::now();
    let info = helper.load_tts(&models.join("kokoro-int8-multi-lang-v1_1"), TtsEngine::Kokoro).unwrap();
    println!("Kokoro v1.1 加载 {:?}：{} Hz、{} 个音色", started.elapsed(), info.sample_rate, info.speakers);
    for probe in [0, 3, 30, 57, 58, 80, 102] {
        let started = Instant::now();
        let pcm = helper.synthesize_blocking("你好，我是小智。", probe, 1.0).unwrap();
        println!("  音色 {probe:>3}：基频约 {:.0} Hz，合成用 {:?}", median_pitch(&pcm), started.elapsed());
    }
    let started = Instant::now();
    let kokoro = helper.synthesize_blocking(sentence, sid, 1.0).unwrap();
    println!("Kokoro v1.1 音色 {sid} 合成 {:.2} 秒音频用 {:?}", kokoro.samples.len() as f64 / kokoro.sample_rate as f64, started.elapsed());

    // Matcha 中英：单音色，忽略音色编号
    let started = Instant::now();
    let info = helper.load_tts(&models.join("matcha-icefall-zh-en"), TtsEngine::Matcha).unwrap();
    println!("Matcha 加载 {:?}：{} Hz、{} 个音色", started.elapsed(), info.sample_rate, info.speakers);
    let started = Instant::now();
    let matcha = helper.synthesize_blocking(sentence, 50, 1.0).unwrap();
    println!(
        "Matcha 合成 {:.2} 秒音频用 {:?}，基频约 {:.0} Hz",
        matcha.samples.len() as f64 / matcha.sample_rate as f64,
        started.elapsed(),
        median_pitch(&matcha)
    );
    assert!(matcha.samples.len() > matcha.sample_rate as usize);

    // SenseVoice 2025 整段识别两种合成结果
    helper.load_asr(&models.join("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09"), AsrEngine::SenseVoice).unwrap();
    for (name, pcm) in [("Kokoro v1.1", &kokoro), ("Matcha", &matcha)] {
        let started = Instant::now();
        let text = helper.transcribe_blocking(&to_16k(pcm), 16_000).unwrap();
        println!("SenseVoice 2025 识别 {name} 用 {:?}：{text}", started.elapsed());
        assert!(text.contains("激光") || text.contains("雕刻"), "{text}");
    }
    let stream = helper.next_stream();
    assert!(helper.asr_open(stream).unwrap_err().to_string().contains("不支持流式"), "整段模型拒绝流式");

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
        let asr = LocalStreamingAsr { helper: helper.clone() };
        let stream = asr.start();
        for chunk in pcm16.chunks(960) {
            stream.feed(chunk);
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        let started = Instant::now();
        let text = stream.finish().await.unwrap();
        println!("X-ASR 流式识别 {name}：说完到出字 {:?}：{text}", started.elapsed());
        assert!(text.contains("激光") || text.contains("雕刻"), "{text}");
        let started = Instant::now();
        let whole = helper.transcribe_blocking(&pcm16, 16_000).unwrap();
        println!("X-ASR 整段识别 {name} 用 {:?}：{whole}", started.elapsed());
    }

    // TEN-VAD 判停
    helper.load_vad(&models.join("ten-vad.onnx"), VadParams::for_engine(VadEngine::Ten), VadEngine::Ten).unwrap();
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
    println!("TEN-VAD 事件（送完第几毫秒的音频时报出）：{events:?}，语音在 {} ms 处结束", (8_000 + speech.len()) / 16);
    assert_eq!(events.first().map(|event| event.0), Some(VadEventKind::SpeechStart));
    assert_eq!(events.last().map(|event| event.0), Some(VadEventKind::SpeechEnd));
}
