//! n70 小智全屏画风：假设备（不连真实小智），验证服务端按场景发表情名、开始雕刻才推刀路。
use super::*;
use moss_voice_providers::BoxFuture;

struct ScreenTools {
    payload: Arc<Mutex<Value>>,
    preview_reads: Arc<Mutex<Vec<String>>>,
}

impl ToolExecutor for ScreenTools {
    fn tools(&self) -> Vec<ToolSpec> {
        Vec::new()
    }
    fn call<'a>(
        &'a self,
        _: &'a ToolContext,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async { ToolOutcome::Reply("{}".into()) })
    }
    fn recent_workflows<'a>(&'a self, _: &'a ToolContext) -> BoxFuture<'a, Option<Value>> {
        let snap = self.payload.lock().unwrap().clone();
        Box::pin(async move { Some(snap) })
    }
    fn workflow_preview<'a>(
        &'a self,
        _: &'a ToolContext,
        workflow_id: &'a str,
    ) -> BoxFuture<'a, Option<Vec<u8>>> {
        self.preview_reads
            .lock()
            .unwrap()
            .push(workflow_id.to_owned());
        // 白底黑线的刀路预览图
        let mut img = image::RgbImage::from_pixel(400, 300, image::Rgb([255, 255, 255]));
        for x in 50..350 {
            img.put_pixel(x, 150, image::Rgb([0, 0, 0]));
        }
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        Box::pin(async move { Some(png) })
    }
}

struct Rig {
    server: VoiceServer,
    registry: Arc<FakeRegistry>,
    payload: Arc<Mutex<Value>>,
    preview_reads: Arc<Mutex<Vec<String>>>,
}

fn rig(pack: bool) -> Rig {
    let payload = Arc::new(Mutex::new(serde_json::json!({ "workflows": [] })));
    let preview_reads = Arc::new(Mutex::new(Vec::new()));
    let tools = Arc::new(ScreenTools {
        payload: payload.clone(),
        preview_reads: preview_reads.clone(),
    });
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&["😊好的，马上开始。"])])),
        ..Default::default()
    });
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("开始吧".to_owned())])),
        ..Default::default()
    });
    let conversation = Conversation::new(
        asr,
        llm,
        Arc::new(FakeTts::default()),
        tools,
        ConversationSettings::default(),
    );
    let registry = Arc::new(FakeRegistry::default());
    if pack {
        *registry.hardware_prefs.lock().unwrap() = Some(HardwarePrefs {
            screen_pack: Some(crate::screen_pack::pack_marker(
                crate::ScreenCardStyle::MossHud,
            )),
            ..HardwarePrefs::default()
        });
    }
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            playback_start_ms: 80,
            job_poll_interval: Duration::from_millis(60),
            screen_card_style: crate::ScreenCardStyle::MossHud,
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    Rig {
        server,
        registry,
        payload,
        preview_reads,
    }
}

/// 立创·实战派 320×240 彩屏握手（工具清单 + 屏幕信息）。
fn color_device(rig: &Rig) -> Device {
    let mut device = allowed_device_with(&rig.server, &rig.registry, 1, true);
    let init = read_mcp(&mut device);
    reply_mcp(
        &mut device,
        &init["id"],
        serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}),
    );
    assert_eq!(read_mcp(&mut device)["method"], "notifications/initialized");
    let list = read_mcp(&mut device);
    reply_mcp(
        &mut device,
        &list["id"],
        serde_json::json!({"tools": [
            {"name": "self.screen.get_info", "description": "屏幕信息", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "self.screen.preview_image", "description": "图片预览", "inputSchema": {"type": "object", "properties": {"url": {"type": "string"}}}}
        ]}),
    );
    let info = read_mcp(&mut device);
    reply_mcp(
        &mut device,
        &info["id"],
        serde_json::json!({"content": [{"type": "text", "text": "{\"width\":320,\"height\":240,\"monochrome\":false}"}], "isError": false}),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while rig.server.status().sessions[0].screen_width != Some(320) {
        assert!(std::time::Instant::now() < deadline, "等屏幕信息超时");
        std::thread::sleep(Duration::from_millis(10));
    }
    device
}

/// 读到 `tts stop` 为止，返回途中设备收到的表情名（跳过音频）。
fn emotions_until_tts_stop(device: &mut Device) -> Vec<String> {
    let mut emotions = Vec::new();
    loop {
        if let Message::Text(text) = device.read().unwrap() {
            let v: Value = serde_json::from_str(text.as_str()).unwrap();
            if v["type"] == "llm" {
                emotions.push(v["emotion"].as_str().unwrap().to_owned());
            }
            if v["type"] == "tts" && v["state"] == "stop" {
                return emotions;
            }
        }
    }
}

/// 读文本消息直到满足条件；途中的设备工具调用一律回成功（模拟固件拉图）。
fn read_until(device: &mut Device, mut done: impl FnMut(&Value) -> bool) -> Vec<Value> {
    let mut seen = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "等待超时，已收到 {seen:?}"
        );
        if let Message::Text(text) = device.read().unwrap() {
            let v: Value = serde_json::from_str(text.as_str()).unwrap();
            if v["type"] == "mcp" && v["payload"]["id"].is_number() {
                reply_mcp(
                    device,
                    &v["payload"]["id"],
                    serde_json::json!({"content": [{"type": "text", "text": "true"}]}),
                );
            }
            let stop = done(&v);
            seen.push(v);
            if stop {
                return seen;
            }
        }
    }
}

#[test]
fn screen_pack_device_switches_full_screen_scenes_for_conversation_and_engraving() {
    let rig = rig(true);
    let mut device = color_device(&rig);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !rig.server.status().sessions[0].screen_pack {
        assert!(
            std::time::Instant::now() < deadline,
            "设备偏好里的画风包标记要进会话视图"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // 对话：聆听 → 思考 → 说话 → 回复带 😊 换成“开心”，全是当前画风（太空舱）的全屏场景名
    speak_manual(&mut device, 1, 260);
    let emotions = emotions_until_tts_stop(&mut device);
    assert_eq!(
        emotions,
        ["hud_listen", "hud_think", "hud_happy"],
        "刷了画风包的设备不再收到固件标准表情名"
    );

    // 开始雕刻：底层切“雕刻”画面 + 一行进度字，中央叠这次任务的刀路进度卡（只取一次刀路）
    *rig.payload.lock().unwrap() = serde_json::json!({
        "workflows": [{
            "workflow_id": "wf-pack-1",
            "status": "sending",
            "confirmation_snapshot": { "power": 600, "feed_rate": 1200 },
            "live_progress": { "lines_sent": 500, "total_lines": 1000, "elapsed_ms": 10_000 }
        }]
    });
    let seen = read_until(&mut device, |v| {
        v["type"] == "mcp" && v["payload"]["params"]["name"] == "self.screen.preview_image"
    });
    let llm: Vec<_> = seen.iter().filter(|v| v["type"] == "llm").collect();
    assert_eq!(llm.last().unwrap()["emotion"], "hud_engrave");
    let progress = seen
        .iter()
        .find(|v| v["type"] == "stt")
        .expect("雕刻进度写在字幕里");
    assert!(
        progress["text"]
            .as_str()
            .unwrap()
            .starts_with("[雕刻中] 50%"),
        "{progress}"
    );
    let url = seen.last().unwrap()["payload"]["params"]["arguments"]["url"]
        .as_str()
        .unwrap()
        .to_owned();
    let png = reqwest::blocking::get(&url).unwrap().bytes().unwrap();
    let img = image::load_from_memory(&png).unwrap();
    assert_eq!((img.width(), img.height()), (160, 120), "中央半屏 1:1");
    // 下一拍：表情和字幕没变就不重发，只续推刀路卡
    let next = read_until(&mut device, |v| {
        v["type"] == "mcp" && v["payload"]["params"]["name"] == "self.screen.preview_image"
    });
    assert!(
        next.iter()
            .all(|v| v["type"] != "llm" && v["type"] != "stt"),
        "{next:?}"
    );
    assert_eq!(rig.preview_reads.lock().unwrap().as_slice(), ["wf-pack-1"]);

    // 雕完：完工画面 + 结果字幕，播报期间保持完工画面
    *rig.payload.lock().unwrap() = serde_json::json!({
        "workflows": [{
            "workflow_id": "wf-pack-1",
            "status": "completed",
            "send_result": { "lines_sent": 1000, "total_lines": 1000 }
        }]
    });
    let seen = read_until(&mut device, |v| v["type"] == "tts" && v["state"] == "start");
    assert!(
        seen.iter()
            .any(|v| v["type"] == "llm" && v["emotion"] == "hud_celebrate")
    );
    assert!(
        seen.iter()
            .any(|v| v["type"] == "stt" && v["text"] == "[雕刻完成] 共 1000 行")
    );
    assert!(
        !seen
            .iter()
            .any(|v| v["type"] == "mcp"
                && v["payload"]["params"]["name"] == "self.screen.preview_image"),
        "刷了画风包的设备不再推半屏完工徽章"
    );
    let during_notice = emotions_until_tts_stop(&mut device);
    assert_eq!(
        during_notice.first().map(String::as_str),
        Some("hud_celebrate")
    );

    // 语音中心试看：直接切全屏场景，不推图
    rig.server
        .push_scene_card(
            crate::BotPose::Listening,
            Some(crate::ScreenCardStyle::GoggleBot),
            Some(DEVICE),
        )
        .unwrap();
    let seen = read_until(&mut device, |v| v["type"] == "llm");
    assert_eq!(seen.last().unwrap()["emotion"], "bot_listen");
    assert!(seen.iter().all(|v| v["type"] != "mcp"));
}

#[test]
fn device_without_screen_pack_keeps_official_emotions() {
    let rig = rig(false);
    let mut device = color_device(&rig);
    speak_manual(&mut device, 1, 260);
    let emotions = emotions_until_tts_stop(&mut device);
    assert_eq!(
        emotions,
        ["happy"],
        "没刷画风包：只转发大模型表情，不发画风场景名"
    );
    assert!(!rig.server.status().sessions[0].screen_pack);
}
