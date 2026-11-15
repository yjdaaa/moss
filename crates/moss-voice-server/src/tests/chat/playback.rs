//! 下行播放连续性：用“模拟设备”按官方固件的规则接收音频，量出用户真正会听到的停顿。
//!
//! 固件规则（`78/xiaozhi-esp32` `main/application.cc`、`main/audio/audio_service.*`）：
//! - 收到 `tts start` 后要经主任务调度才切到说话状态，此前到达的音频帧被丢弃；
//! - 解码队列最多 20 帧（1.2 秒），满了直接丢；
//! - 没有抖动缓冲：队列空了喇叭就断。
use super::*;

/// 像本地合成辅助程序那样合成：单线程串行，一句话按 `synthesis_segments` 分段，
/// 每段算完才整段送出；`rtf` 为实时率（1 秒语音要算多少秒），每个字按 200 ms 语音计。
struct LocalLikeTts {
    rtf: f64,
    worker: tokio::sync::Mutex<()>,
}

impl LocalLikeTts {
    fn new(rtf: f64) -> Arc<Self> {
        Arc::new(Self {
            rtf,
            worker: tokio::sync::Mutex::new(()),
        })
    }

    async fn segment(&self, text: &str) -> Pcm {
        let ms = text.chars().count() as u32 * 200;
        tokio::time::sleep(Duration::from_secs_f64(f64::from(ms) / 1000.0 * self.rtf)).await;
        Pcm {
            samples: tone(24_000, 300.0, ms, 6000.0),
            sample_rate: 24_000,
        }
    }
}

impl Tts for LocalLikeTts {
    fn synthesize<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
        Box::pin(async move {
            let _busy = self.worker.lock().await;
            Ok(self.segment(text).await)
        })
    }

    fn synthesize_stream<'a>(
        &'a self,
        text: &'a str,
        chunks: tokio::sync::mpsc::UnboundedSender<Pcm>,
    ) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            let _busy = self.worker.lock().await;
            for segment in moss_voice_local_proto::synthesis_segments(text) {
                let pcm = self.segment(&segment).await;
                if chunks.send(pcm).is_err() {
                    break;
                }
            }
            Ok(())
        })
    }
}

#[derive(Debug)]
struct Heard {
    /// 固件还没切到说话状态就到达、被丢弃的帧
    dropped: usize,
    /// 解码队列最深时有多少帧
    max_queue: usize,
    /// 开播之后喇叭断流的总时长
    stalled: Duration,
    /// 最长的一次断流
    longest_stall: Duration,
    /// `tts start` 到第一帧开始播放
    first_audio: Duration,
    frames: usize,
}

/// 收到 `tts stop` 为止，按固件规则回放一遍到达时刻。
fn listen_like_firmware(device: &mut Device) -> Heard {
    const FRAME: Duration = Duration::from_millis(60);
    // 固件从收到 start 到切换状态的调度延迟（保守取值）
    const STATE_SWITCH: Duration = Duration::from_millis(40);
    let mut start = None;
    let mut arrivals = Vec::new();
    loop {
        match device.read().unwrap() {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                if value["type"] == "tts" && value["state"] == "start" {
                    start = Some(Instant::now());
                }
                if value["type"] == "tts" && value["state"] == "stop" {
                    break;
                }
            }
            Message::Binary(_) => arrivals.push(Instant::now()),
            _ => {}
        }
    }
    let start = start.expect("tts start");
    let mut heard = Heard {
        dropped: 0,
        max_queue: 0,
        stalled: Duration::ZERO,
        longest_stall: Duration::ZERO,
        first_audio: Duration::ZERO,
        frames: 0,
    };
    // 每一帧开始播放的时刻
    let mut starts: Vec<Instant> = Vec::new();
    for at in arrivals {
        if at < start + STATE_SWITCH {
            heard.dropped += 1;
            continue;
        }
        let queued = starts.iter().filter(|begin| **begin > at).count();
        heard.max_queue = heard.max_queue.max(queued + 1);
        let begin = match starts.last() {
            None => {
                heard.first_audio = at - start;
                at
            }
            Some(previous) if *previous + FRAME >= at => *previous + FRAME,
            Some(previous) => {
                let gap = at - (*previous + FRAME);
                heard.stalled += gap;
                heard.longest_stall = heard.longest_stall.max(gap);
                at
            }
        };
        starts.push(begin);
        heard.frames += 1;
    }
    heard
}

/// 大模型流式吐出一段五句话的回答（约 60 字、12 秒语音），由 `tts` 合成，返回模拟设备听到的结果。
fn play_long_answer(tts: Arc<dyn Tts>) -> Heard {
    play_answer(
        tts,
        vec![
            "好的，",
            "椴木三毫米",
            "我建议用",
            "百分之八十功率。",
            "速度可以",
            "设在每分钟",
            "六百毫米，",
            "分两遍切透。",
            "开始之前",
            "记得把吹气打开，",
            "再检查一下焦距。",
            "都准备好了",
            "就告诉我。",
        ],
    )
}

fn play_answer(tts: Arc<dyn Tts>, deltas: Vec<&'static str>) -> Heard {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([Round::Reply {
            deltas,
            calls: Vec::new(),
            // 每 40 ms 吐一小段：比任何合成都快，瓶颈只在合成与播放
            delay_ms: 40,
        }])),
        ..FakeLlm::default()
    };
    let conversation = Conversation::new(
        Arc::new(FakeAsr {
            replies: Mutex::new(VecDeque::from([Ok("椴木怎么切".to_owned())])),
            heard: Mutex::default(),
        }),
        Arc::new(llm),
        tts,
        Arc::new(FakeTools::default()),
        ConversationSettings::default(),
    );
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            playback_start_ms: 300,
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let heard = listen_like_firmware(&mut device);
    drop(server);
    heard
}

/// 合成远快于实时（云端流式、Matcha）：设备听到的应当一帧不丢、全程不断。
#[test]
fn fast_synthesis_plays_without_a_single_gap_on_the_device() {
    let heard = play_long_answer(LocalLikeTts::new(0.05));
    println!("实时率 0.05：{heard:?}");
    assert_eq!(heard.dropped, 0, "{heard:?}");
    assert!(heard.max_queue <= 12, "不能逼近固件 20 帧上限：{heard:?}");
    assert!(heard.frames > 150, "{heard:?}");
    assert!(
        heard.longest_stall <= Duration::from_millis(40),
        "播放中不能有听得见的停顿：{heard:?}"
    );
}

/// 本地 Kokoro 的典型速度（1 秒语音算 0.55 秒，单线程串行）：句子之间也不能断。
#[test]
fn local_speed_synthesis_keeps_up_with_playback() {
    let heard = play_long_answer(LocalLikeTts::new(0.55));
    println!("实时率 0.55：{heard:?}");
    assert_eq!(heard.dropped, 0, "{heard:?}");
    assert!(heard.max_queue <= 12, "{heard:?}");
    assert!(
        heard.longest_stall <= Duration::from_millis(150),
        "合成跟得上时不该有明显停顿：{heard:?}"
    );
}

/// 合成贴着实时线甚至更慢时的表现（不做断言，调优时看数字）：
/// `cargo test -p moss-voice-server slow_synthesis_report -- --ignored --nocapture`
#[test]
#[ignore]
fn slow_synthesis_report() {
    for rtf in [0.3, 0.55, 0.7] {
        // 短分句之后接没有逗号的长句：最考验“下一段能不能在上一段播完前算完”
        let heard = play_answer(
            LocalLikeTts::new(rtf),
            vec![
                "好的。",
                "这块椴木板我建议你先用比较低的功率在边角上试刻一小块看看颜色深浅合不合适。",
                "如果颜色偏浅就把速度降下来而不是一味加大功率因为功率太高容易把边缘烧黑。",
                "都调好以后再正式开始。",
            ],
        );
        println!(
            "实时率 {rtf}：断流合计 {} ms，最长一次 {} ms，丢帧 {}，队列最深 {}",
            heard.stalled.as_millis(),
            heard.longest_stall.as_millis(),
            heard.dropped,
            heard.max_queue
        );
    }
}
