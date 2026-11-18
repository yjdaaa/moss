//! 屏幕预览：单色 OLED 与彩屏预览卡、实时 HUD、零擦写卡片与说话字幕推屏。
use super::*;

#[test]
fn monochrome_oled_official_firmware_auto_detects_screen_info_and_pushes_text_preview_card() {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&["预览已准备好，确认开始吗？"])])),
        repeat: None,
        seen: Mutex::default(),
    };
    let (server, registry, _fakes) = start_chat(llm, &["刻一个福字"], Duration::from_secs(120));
    let mut oled = allowed_device_with(&server, &registry, 1, true);

    let init = read_mcp(&mut oled);
    reply_mcp(
        &mut oled,
        &init["id"],
        serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}),
    );
    assert_eq!(read_mcp(&mut oled)["method"], "notifications/initialized");
    let list_req = read_mcp(&mut oled);
    assert_eq!(list_req["params"]["withUserTools"], true);
    // 模拟 ESP32-S3 单色 128x64 OLED 官方固件：同时注册了 screen.get_info 与空实现的 screen.preview_image
    reply_mcp(
        &mut oled,
        &list_req["id"],
        serde_json::json!({
            "tools": [
                {"name": "self.screen.get_info", "description": "屏幕信息", "inputSchema": {"type": "object", "properties": {}}},
                {"name": "self.screen.preview_image", "description": "图片预览", "inputSchema": {"type": "object", "properties": {"url": {"type": "string"}}}}
            ]
        }),
    );

    // MOSS 自动发送 self.screen.get_info 探测屏幕是否为单色 OLED
    let info_req = read_mcp(&mut oled);
    assert_eq!(info_req["params"]["name"], "self.screen.get_info");
    reply_mcp(
        &mut oled,
        &info_req["id"],
        serde_json::json!({
            "content": [{"type": "text", "text": "{\"width\":128,\"height\":64,\"monochrome\":true}"}],
            "isError": false
        }),
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while server.status().sessions[0].screen_monochrome != Some(true)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.status().sessions[0].screen_monochrome, Some(true));

    // 1. 空闲态推送预览：识别为单色 OLED 后不发无效 PNG，自动改为向 OLED 下发 Alert 滚动文字预览卡
    let summary = "3mm椴木雕刻 ·「福」· 40×40mm · S600 F1200 ×1遍";
    let pushed = server
        .push_preview_adaptive_with_ruler(None, summary, None, None)
        .unwrap();
    assert_eq!(pushed, 1);
    let alert = read_json(&mut oled);
    assert_eq!(alert["type"], "alert");
    assert_eq!(alert["status"], "雕刻预览");
    assert_eq!(alert["message"], summary);

    // 2. 对话轮次中生成预览：暂存 OLED 文字卡，等本轮语音 tts:stop 播完后无声刷到 OLED 字幕槽供用户核对
    speak_manual(&mut oled, 1, 600);
    assert_eq!(read_json(&mut oled)["text"], "刻一个福字");
    server
        .push_preview_adaptive_with_ruler(None, "3mm椴木雕刻 ·「福」· S600 F1200", None, None)
        .unwrap();
    let _ = collect_items(&mut oled);
    let post_tts_card = read_json(&mut oled);
    assert_eq!(post_tts_card["type"], "stt");
    assert_eq!(
        post_tts_card["text"],
        "[预览] 3mm椴木雕刻 ·「福」· S600 F1200"
    );
}

#[test]
fn lichuang_szpi_320x240_pushes_1to1_preview_holds_until_speech_streams_live_hud_and_switches_talk_mode()
 {
    use image::{ImageFormat, Rgba, RgbaImage};
    use moss_voice_providers::BoxFuture;
    use std::io::Cursor;

    struct LiveJobTools {
        workflows: Arc<Mutex<Value>>,
    }
    impl ToolExecutor for LiveJobTools {
        fn tools(&self) -> Vec<ToolSpec> {
            Vec::new()
        }
        fn call<'a>(
            &'a self,
            _: &'a ToolContext,
            _: &'a str,
            _: &'a str,
        ) -> BoxFuture<'a, ToolOutcome> {
            Box::pin(async { panic!("实时副屏与播报只走只读通道") })
        }
        fn recent_workflows<'a>(&'a self, _: &'a ToolContext) -> BoxFuture<'a, Option<Value>> {
            let wf = self.workflows.lock().unwrap().clone();
            Box::pin(async move { Some(serde_json::json!({ "workflows": wf })) })
        }
    }

    let workflows = Arc::new(Mutex::new(serde_json::json!([])));
    let tts = Arc::new(FakeTts::default());
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        Arc::new(FakeLlm::default()),
        tts.clone(),
        Arc::new(LiveJobTools {
            workflows: workflows.clone(),
        }),
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
            job_poll_interval: Duration::from_millis(60),
            preview_hold_interval: Duration::from_millis(70),
            preview_hold_duration: Duration::from_millis(600),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();

    let mut szpi = allowed_device_with(&server, &registry, 1, true);
    let init = read_mcp(&mut szpi);
    reply_mcp(
        &mut szpi,
        &init["id"],
        serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}),
    );
    assert_eq!(read_mcp(&mut szpi)["method"], "notifications/initialized");
    let list_req = read_mcp(&mut szpi);
    reply_mcp(
        &mut szpi,
        &list_req["id"],
        serde_json::json!({
            "tools": [
                {"name": "self.screen.get_info", "description": "屏幕信息", "inputSchema": {"type": "object", "properties": {}}},
                {"name": "self.screen.preview_image", "description": "图片预览", "inputSchema": {"type": "object", "properties": {"url": {"type": "string"}}}},
                {"name": "self.set_press_to_talk", "description": "长按说话切换", "inputSchema": {"type": "object", "properties": {"mode": {"type": "string"}}}},
                {"name": "self.get_device_status", "description": "设备状态", "inputSchema": {"type": "object", "properties": {}}}
            ]
        }),
    );

    let info_req = read_mcp(&mut szpi);
    assert_eq!(info_req["params"]["name"], "self.screen.get_info");
    reply_mcp(
        &mut szpi,
        &info_req["id"],
        serde_json::json!({
            "content": [{"type": "text", "text": "{\"width\":320,\"height\":240,\"monochrome\":false}"}],
            "isError": false
        }),
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while server.status().sessions[0].screen_width != Some(320)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let sess = &server.status().sessions[0];
    assert_eq!(
        (
            sess.screen_width,
            sess.screen_height,
            sess.screen_monochrome
        ),
        (Some(320), Some(240), Some(false))
    );

    // 优化 4：一键切换排风抗噪「长按说话」模式
    server
        .device_request(
            DEVICE,
            crate::DeviceRequest::SetTalkMode("press_to_talk".into()),
        )
        .unwrap();
    let ptt_call = read_mcp(&mut szpi);
    assert_eq!(ptt_call["params"]["name"], "self.set_press_to_talk");
    assert_eq!(ptt_call["params"]["arguments"]["mode"], "press_to_talk");
    let status_call = read_mcp(&mut szpi);
    assert_eq!(status_call["params"]["name"], "self.get_device_status");
    reply_mcp(
        &mut szpi,
        &ptt_call["id"],
        serde_json::json!({"content": [{"type": "text", "text": "true"}]}),
    );
    reply_mcp(
        &mut szpi,
        &status_call["id"],
        serde_json::json!({"audio_speaker": {"volume": 60}}),
    );

    // 优化 1 & 2：推送白底 1px 极细刀路原图 → 自动输出精确 160×120 PNG + 底部中文参数栏 + 无声续期保持
    let mut fine = RgbaImage::from_pixel(400, 300, Rgba([255, 255, 255, 255]));
    for x in 30..370 {
        fine.put_pixel(x, 150, Rgba([0, 0, 0, 255]));
    }
    let mut raw_png = Vec::new();
    fine.write_to(&mut Cursor::new(&mut raw_png), ImageFormat::Png)
        .unwrap();
    let summary = "3mm椴木雕刻 ·「MOSS」· 40×30mm · S600 F1200";
    let pushed = server
        .push_preview_adaptive_with_ruler(Some(&raw_png), summary, Some("40X30MM S600 F1200"), None)
        .unwrap();
    assert_eq!(pushed, 1);

    let preview_mcp = read_mcp(&mut szpi);
    assert_eq!(preview_mcp["params"]["name"], "self.screen.preview_image");
    let preview_url = preview_mcp["params"]["arguments"]["url"]
        .as_str()
        .unwrap()
        .to_owned();
    let png_resp = reqwest::blocking::get(&preview_url)
        .unwrap()
        .bytes()
        .unwrap();
    let decoded_preview = image::load_from_memory(&png_resp).unwrap();
    assert_eq!(
        (decoded_preview.width(), decoded_preview.height()),
        (160, 120),
        "立创·实战派 320×240 必须输出精确 160×120 1:1 点对点 PNG"
    );
    reply_mcp(
        &mut szpi,
        &preview_mcp["id"],
        serde_json::json!({"content": [{"type": "text", "text": "true"}]}),
    );

    // 图文同屏：底部状态栏同时收到中文参数摘要
    let subtitle_msg = read_json(&mut szpi);
    assert_eq!(subtitle_msg["type"], "stt");
    assert_eq!(subtitle_msg["text"], format!("[预览] {summary}"));

    // 确认窗口内自动无声续期（克服官方固件 5000ms 自动隐藏预览图计时器）
    let hold_refresh = read_mcp(&mut szpi);
    assert_eq!(hold_refresh["params"]["name"], "self.screen.preview_image");
    assert_eq!(hold_refresh["params"]["arguments"]["url"], preview_url);
    reply_mcp(
        &mut szpi,
        &hold_refresh["id"],
        serde_json::json!({"content": [{"type": "text", "text": "true"}]}),
    );

    // 用户按下按键开口或打断时，确认续期立即停止
    send_json(
        &mut szpi,
        r#"{"type":"listen","state":"start","mode":"manual"}"#,
    );
    send_json(&mut szpi, r#"{"type":"abort","reason":"user_button"}"#);

    // 优化 3：进入激光加工 sending 阶段 → 屏幕自动变身 MOSS 太空舱实时进度副屏 HUD，完成后语音播报
    *workflows.lock().unwrap() = serde_json::json!([{
        "workflow_id": "wf-szpi-hud",
        "status": "sending",
        "confirmation_snapshot": { "power": 600, "feed_rate": 1200 },
        "live_progress": { "lines_sent": 680, "total_lines": 1000, "elapsed_ms": 17_000 }
    }]);
    let hud_mcp = read_mcp(&mut szpi);
    assert_eq!(hud_mcp["params"]["name"], "self.screen.preview_image");
    let hud_url = hud_mcp["params"]["arguments"]["url"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(
        hud_url, preview_url,
        "实时加工副屏 HUD 应生成独立的进度图像令牌"
    );
    let hud_bytes = reqwest::blocking::get(&hud_url).unwrap().bytes().unwrap();
    assert!(hud_bytes.len() < 16 * 1024);
    let hud_img = image::load_from_memory(&hud_bytes).unwrap();
    assert_eq!((hud_img.width(), hud_img.height()), (160, 120));
    reply_mcp(
        &mut szpi,
        &hud_mcp["id"],
        serde_json::json!({"content": [{"type": "text", "text": "true"}]}),
    );

    *workflows.lock().unwrap() = serde_json::json!([{
        "workflow_id": "wf-szpi-hud",
        "status": "completed",
        "send_result": { "lines_sent": 1000, "total_lines": 1000 }
    }]);
    let (_, frames, _) = collect_playback(&mut szpi, 1);
    assert!(!frames.is_empty());
    assert!(
        tts.said
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.contains("雕刻任务已完成，共 1000 行"))
    );
}

#[test]
fn zero_flash_cards_approval_alert_terminal_badge_music_deck_style_switch_and_goggle_bot_gif_work()
{
    use crate::music::{MusicIndex, MusicTools};
    use moss_voice_providers::BoxFuture;
    use std::io::Write;

    struct SceneTools {
        payload: Arc<Mutex<Value>>,
    }
    impl ToolExecutor for SceneTools {
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
    }

    let music_dir = tempfile::tempdir().unwrap();
    {
        let rate = 24_000u32;
        let frames = rate; // 1s WAV
        let mut f = std::fs::File::create(music_dir.path().join("MOSS_GROOVE.wav")).unwrap();
        let mut header = Vec::new();
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&(36 + frames * 2).to_le_bytes());
        header.extend_from_slice(b"WAVEfmt ");
        header.extend_from_slice(&16u32.to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&rate.to_le_bytes());
        header.extend_from_slice(&(rate * 2).to_le_bytes());
        header.extend_from_slice(&2u16.to_le_bytes());
        header.extend_from_slice(&16u16.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&(frames * 2).to_le_bytes());
        f.write_all(&header).unwrap();
        for _ in 0..frames {
            f.write_all(&2200i16.to_le_bytes()).unwrap();
        }
    }
    let music_index = Arc::new(MusicIndex::scan(music_dir.path()).unwrap());
    let payload = Arc::new(Mutex::new(serde_json::json!({ "workflows": [] })));
    let tools = Arc::new(MusicTools::new(
        Arc::new(SceneTools {
            payload: payload.clone(),
        }),
        music_index.clone(),
    ));
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([Round::Reply {
            deltas: vec![],
            calls: vec![ToolCall::new(
                "music-card-1",
                "play_music",
                r#"{"query":"MOSS_GROOVE"}"#,
            )],
            delay_ms: 0,
        }])),
        ..Default::default()
    });
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("放首音乐".to_owned())])),
        ..Default::default()
    });
    let tts = Arc::new(FakeTts::default());
    let conversation = Conversation::new(
        asr,
        llm,
        tts.clone(),
        tools,
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
            music: Some(music_index),
            playback_start_ms: 80,
            job_poll_interval: Duration::from_millis(60),
            preview_hold_interval: Duration::from_millis(80),
            preview_hold_duration: Duration::from_millis(600),
            screen_card_style: crate::ScreenCardStyle::MossHud,
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();

    // 0. 服务端 GIF89a 端点验证（护目镜小机器人 4 帧循环动图，零 Flash 分区占用）
    let gif_url = format!(
        "http://127.0.0.1:{}/mcp/preview/goggle-bot/engrave.gif",
        server.http_port()
    );
    let gif_resp = reqwest::blocking::get(&gif_url).unwrap();
    assert_eq!(gif_resp.status().as_u16(), 200);
    assert_eq!(
        gif_resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("image/gif")
    );
    let gif_bytes = gif_resp.bytes().unwrap();
    assert!(gif_bytes.starts_with(b"GIF89a"));
    // 2026-11-05：路由改取进程级缓存后响应字节不变
    assert_eq!(
        gif_bytes.as_ref(),
        crate::goggle_bot::encode_goggle_bot_gif(crate::goggle_bot::BotPose::GogglesEngrave)
            .unwrap()
            .as_slice()
    );

    // 立创·实战派 320×240 彩屏接入
    let mut szpi = allowed_device_with(&server, &registry, 1, true);
    let init = read_mcp(&mut szpi);
    reply_mcp(
        &mut szpi,
        &init["id"],
        serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}),
    );
    assert_eq!(read_mcp(&mut szpi)["method"], "notifications/initialized");
    let list_req = read_mcp(&mut szpi);
    reply_mcp(
        &mut szpi,
        &list_req["id"],
        serde_json::json!({
            "tools": [
                {"name": "self.screen.get_info", "description": "屏幕信息", "inputSchema": {"type": "object", "properties": {}}},
                {"name": "self.screen.preview_image", "description": "图片预览", "inputSchema": {"type": "object", "properties": {"url": {"type": "string"}}}}
            ]
        }),
    );
    let info_req = read_mcp(&mut szpi);
    reply_mcp(
        &mut szpi,
        &info_req["id"],
        serde_json::json!({
            "content": [{"type": "text", "text": "{\"width\":320,\"height\":240,\"monochrome\":false}"}],
            "isError": false
        }),
    );
    let wait_deadline = std::time::Instant::now() + Duration::from_secs(2);
    while server.status().sessions[0].screen_width != Some(320)
        && std::time::Instant::now() < wait_deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }

    // 1. 热切换为「可爱像素小机器人」画风（不断开当前小智会话）
    assert_eq!(server.screen_card_style(), crate::ScreenCardStyle::MossHud);
    server.set_screen_card_style(crate::ScreenCardStyle::GoggleBot);
    assert_eq!(
        server.screen_card_style(),
        crate::ScreenCardStyle::GoggleBot
    );
    assert_eq!(server.status().sessions.len(), 1);

    // 2. 场景卡 ①：严格模式待电脑审批警戒卡（含倒计时条，不泄露审批 ID 或摘要哈希）
    *payload.lock().unwrap() = serde_json::json!({
        "workflows": [],
        "pending_approval": {
            "kind": "workflow_send",
            "summary": "激光任务开始发送（3mm椴木）",
            "remaining_secs": 110,
            "total_secs": 180
        }
    });
    let approval_mcp = read_mcp(&mut szpi);
    assert_eq!(approval_mcp["params"]["name"], "self.screen.preview_image");
    let approval_url = approval_mcp["params"]["arguments"]["url"]
        .as_str()
        .unwrap()
        .to_owned();
    let approval_png = reqwest::blocking::get(&approval_url)
        .unwrap()
        .bytes()
        .unwrap();
    let approval_img = image::load_from_memory(&approval_png).unwrap();
    assert_eq!((approval_img.width(), approval_img.height()), (160, 120));
    reply_mcp(
        &mut szpi,
        &approval_mcp["id"],
        serde_json::json!({"content": [{"type": "text", "text": "true"}]}),
    );
    let approval_stt = read_json(&mut szpi);
    assert_eq!(approval_stt["type"], "stt");
    assert_eq!(
        approval_stt["text"],
        "[待电脑批准] 激光任务开始发送（3mm椴木）"
    );

    // 3. 场景卡 ②：加工从 sending 进入 completed → 推送 5 秒完工庆祝徽章卡 + 播报结果
    *payload.lock().unwrap() = serde_json::json!({
        "workflows": [{
            "workflow_id": "wf-celebrate-1",
            "status": "sending",
            "confirmation_snapshot": { "power": 550, "feed_rate": 1000 },
            "live_progress": { "lines_sent": 500, "total_lines": 1000, "elapsed_ms": 10_000 }
        }]
    });
    let sending_mcp = read_mcp(&mut szpi);
    assert_eq!(sending_mcp["params"]["name"], "self.screen.preview_image");
    reply_mcp(
        &mut szpi,
        &sending_mcp["id"],
        serde_json::json!({"content": [{"type": "text", "text": "true"}]}),
    );

    *payload.lock().unwrap() = serde_json::json!({
        "workflows": [{
            "workflow_id": "wf-celebrate-1",
            "status": "completed",
            "send_result": { "lines_sent": 1000, "total_lines": 1000 }
        }]
    });
    let badge_mcp = read_mcp(&mut szpi);
    assert_eq!(badge_mcp["params"]["name"], "self.screen.preview_image");
    let badge_url = badge_mcp["params"]["arguments"]["url"]
        .as_str()
        .unwrap()
        .to_owned();
    let badge_png = reqwest::blocking::get(&badge_url).unwrap().bytes().unwrap();
    let badge_img = image::load_from_memory(&badge_png).unwrap();
    assert_eq!((badge_img.width(), badge_img.height()), (160, 120));
    reply_mcp(
        &mut szpi,
        &badge_mcp["id"],
        serde_json::json!({"content": [{"type": "text", "text": "true"}]}),
    );
    let (_, notice_frames, _) = collect_playback(&mut szpi, 1);
    assert!(!notice_frames.is_empty());

    // 4. 场景卡 ③：播放本地音乐 → 自动推送小机器人耳机听歌/像素唱机卡
    speak_manual(&mut szpi, 1, 260);
    let mut saw_music_card = false;
    loop {
        match szpi.read().unwrap() {
            tungstenite::Message::Text(raw) => {
                let v: Value = serde_json::from_str(&raw).unwrap();
                if v["type"] == "mcp"
                    && v["payload"]["params"]["name"] == "self.screen.preview_image"
                {
                    let url = v["payload"]["params"]["arguments"]["url"].as_str().unwrap();
                    let png = reqwest::blocking::get(url).unwrap().bytes().unwrap();
                    let img = image::load_from_memory(&png).unwrap();
                    assert_eq!((img.width(), img.height()), (160, 120));
                    saw_music_card = true;
                    reply_mcp(
                        &mut szpi,
                        &v["payload"]["id"],
                        serde_json::json!({"content": [{"type": "text", "text": "true"}]}),
                    );
                } else if v["type"] == "tts" && v["state"] == "stop" {
                    break;
                }
            }
            tungstenite::Message::Binary(_) => {}
            other => panic!("unexpected ws message: {other:?}"),
        }
    }
    assert!(
        saw_music_card,
        "本地音乐播放期间应向 320×240 彩屏推送听歌卡片"
    );

    // 5. 桌面语音中心一键推屏小机器人姿态预览
    let pushed = server
        .push_scene_card(
            crate::BotPose::GogglesEngrave,
            Some(crate::ScreenCardStyle::GoggleBot),
            Some(DEVICE),
        )
        .unwrap();
    assert_eq!(pushed, 1);
    let pose_mcp = read_mcp(&mut szpi);
    assert_eq!(pose_mcp["params"]["name"], "self.screen.preview_image");
    let pose_url = pose_mcp["params"]["arguments"]["url"].as_str().unwrap();
    let pose_png = reqwest::blocking::get(pose_url).unwrap().bytes().unwrap();
    let pose_img = image::load_from_memory(&pose_png).unwrap();
    assert_eq!((pose_img.width(), pose_img.height()), (160, 120));
    reply_mcp(
        &mut szpi,
        &pose_mcp["id"],
        serde_json::json!({"content": [{"type": "text", "text": "true"}]}),
    );
    let pose_stt = read_json(&mut szpi);
    assert_eq!(pose_stt["text"], "[预览] 小MOSS护目镜机器人 · 戴护目镜雕刻");
}

#[test]
fn interim_subtitle_pushes_to_screen_every_half_second_while_speaking_and_skips_unchanged_final_stt()
 {
    use crate::audio::{EnergyVad, Vad, VadConfig, VadEvent, VadFactory};

    // 严格复刻 OfflineAsrBuffer：未出非空文字前按 5120 步长试认（前导静音空串不退化步长），出字后切 8000 步长
    struct PeriodicOfflineLikeAsr {
        turn: std::sync::atomic::AtomicUsize,
    }
    impl StreamingAsr for PeriodicOfflineLikeAsr {
        fn start(&self) -> AsrStream {
            let turn_idx = self.turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let (interim_tx, interim_rx) = tokio::sync::watch::channel(String::new());
            let handle = tokio::spawn(async move {
                let mut pending = 0usize;
                let mut voiced = 0usize;
                let mut has_nonempty = false;
                let mut step = 0u8;
                while let Some(chunk) = rx.recv().await {
                    for sub in chunk.chunks(320) {
                        if sub.iter().any(|&s| s.unsigned_abs() > 800) {
                            voiced += sub.len();
                        }
                    }
                    pending += chunk.len();
                    let threshold = if has_nonempty { 8_000 } else { 5_120 };
                    if pending < threshold {
                        continue;
                    }
                    pending = 0;
                    if voiced < 5_120 {
                        // 模拟静音 preroll + 60ms 辅音头不足以认出文字（返回空串），不消耗 5120 首认步长
                        continue;
                    }
                    has_nonempty = true;
                    step += 1;
                    match (turn_idx, step) {
                        (0, 1) => {
                            interim_tx.send_replace("你好摩斯".into());
                            tokio::time::sleep(Duration::from_millis(40)).await;
                            // 400ms 冷却窗内紧跟跳字：不应连续刷两次屏幕
                            interim_tx.send_replace("你好摩斯早".into());
                        }
                        (0, _) => {
                            interim_tx.send_replace("你好摩斯助手".into());
                        }
                        (1, _) => {
                            interim_tx.send_replace("查看当前功率".into());
                        }
                        (2, _) => {
                            interim_tx.send_replace("自动拾音首屏".into());
                        }
                        _ => {
                            interim_tx.send_replace("延迟判停首屏".into());
                        }
                    }
                }
                let final_text = match turn_idx {
                    0 => "你好摩斯助手！",
                    1 => "查看当前功率",
                    2 => "自动拾音首屏完成",
                    _ => "延迟判停首屏完成",
                };
                let _ = done_tx.send(Ok(final_text.into()));
            });
            AsrStream::new(tx, done_rx, handle).with_interim(interim_rx)
        }
    }

    let asr = Arc::new(PeriodicOfflineLikeAsr {
        turn: std::sync::atomic::AtomicUsize::new(0),
    });
    let conversation = Arc::new(
        Conversation::new(
            Arc::new(FakeAsr::default()),
            Arc::new(FakeLlm::default()),
            Arc::new(FakeTts::default()),
            Arc::new(FakeTools::default()),
            ConversationSettings {
                asr_corrections: vec![("摩斯".into(), "MOSS".into())],
                ..ConversationSettings::default()
            },
        )
        .with_streaming_asr(asr.clone()),
    );
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(conversation.clone()),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);

    // 第 1 轮（手动模式）：满 320ms（360ms 音频）首次重认即上屏，400ms 内二次跳字被节流，
    // 约 460ms 后（模拟第二次推理略快于第一次的轻微抖动）新增满 500ms 音频正常触发第二次刷屏不吞拍
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"manual"}"#,
    );
    std::thread::sleep(Duration::from_millis(320));
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 360, 8000.0)));

    let first_live = read_json(&mut device);
    assert_eq!(first_live["type"], "stt");
    assert_eq!(
        first_live["text"], "你好MOSS",
        "满 320ms 首轮重认应在说话未结束前立即上屏（不被 500ms 门槛误丢）"
    );

    // 用 ping 验证 40ms 后的紧跟跳字在 400ms 冷却窗内未刷屏
    std::thread::sleep(Duration::from_millis(70));
    send_json(&mut device, r#"{"type":"ping"}"#);
    let pong = read_json(&mut device);
    assert_eq!(
        pong["type"], "pong",
        "冷却窗内不应抢发第二条中间 stt: {pong:?}"
    );

    // 再等 390ms（总间隔约 460ms < 500ms，验证推理轻微加速抖动不会吞掉 500ms 步长的第二次重认）
    std::thread::sleep(Duration::from_millis(390));
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 540, 8000.0)));
    let second_live = read_json(&mut device);
    assert_eq!(second_live["type"], "stt");
    assert_eq!(
        second_live["text"], "你好MOSS助手",
        "500ms 音频步长在 460ms 墙钟到达时不应被冷却窗误吞"
    );

    send_json(&mut device, r#"{"type":"listen","state":"stop"}"#);
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["听:你好MOSS助手！", "tts:start", "句:好的。", "tts:stop"]
    );

    // 第 2 轮：最终定稿与已上屏的实时字幕完全一致时，不再重复下发同一条 stt
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"manual"}"#,
    );
    std::thread::sleep(Duration::from_millis(320));
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 360, 8000.0)));
    let live_same = read_json(&mut device);
    assert_eq!(live_same["type"], "stt");
    assert_eq!(live_same["text"], "查看当前功率");

    send_json(&mut device, r#"{"type":"listen","state":"stop"}"#);
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["tts:start", "句:好的。", "tts:stop"],
        "最终文字与已显示的实时字幕一致时不应重复刷屏重置跑马灯"
    );

    // 第 3 轮（自动拾音 + EnergyVad）：420ms 房间静音 preroll 后开口，
    // 首帧 60ms 起音虽使 head >= 5120 但因空串不退化步长，再说 360ms（< 500ms）即出首屏字幕
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    send_audio(&mut device, 1, &uplink(&silence(UP, 420)));
    for _ in 0..7 {
        std::thread::sleep(Duration::from_millis(55));
        send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 60, 8000.0)));
    }
    let auto_live = read_json(&mut device);
    assert_eq!(auto_live["type"], "stt");
    assert_eq!(
        auto_live["text"], "自动拾音首屏",
        "自动拾音前导静音空串不应吞掉 320ms 首认窗口"
    );
    send_audio(&mut device, 1, &uplink(&silence(UP, 500)));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["听:自动拾音首屏完成", "tts:start", "句:好的。", "tts:stop"]
    );

    // 第 4 轮（模拟本地 Silero / TEN-VAD 攒满 360ms 人声才报 SpeechStart）：
    // trailing_voiced_samples 从 preroll 尾部识别真实起音跨度回推 speech_began_at，head 首轮重认即上屏
    struct DelayedStartVad {
        inner: EnergyVad,
        voiced_samples: usize,
        started: bool,
    }
    impl Vad for DelayedStartVad {
        fn accept(&mut self, pcm: &[i16]) -> Vec<VadEvent> {
            let mut out = Vec::new();
            let raw = self.inner.accept(pcm);
            if self.inner.in_speech() {
                self.voiced_samples += pcm.len();
            } else {
                self.voiced_samples = 0;
            }
            for ev in raw {
                match ev {
                    VadEvent::SpeechStart | VadEvent::Keyword(_) => {}
                    VadEvent::SpeechEnd => {
                        if self.started {
                            self.started = false;
                            out.push(VadEvent::SpeechEnd);
                        }
                    }
                }
            }
            if !self.started && self.voiced_samples >= 5_760 {
                self.started = true;
                out.push(VadEvent::SpeechStart);
            }
            out
        }
        fn reset(&mut self) {
            self.voiced_samples = 0;
            self.started = false;
            self.inner.reset();
        }
        fn in_speech(&self) -> bool {
            self.started
        }
    }
    struct DelayedStartVadFactory;
    impl VadFactory for DelayedStartVadFactory {
        fn create(&self, sample_rate: u32, config: &VadConfig) -> Option<Box<dyn Vad>> {
            Some(Box::new(DelayedStartVad {
                inner: EnergyVad::new(sample_rate, config.clone()),
                voiced_samples: 0,
                started: false,
            }))
        }
    }

    let delayed_registry = Arc::new(FakeRegistry::default());
    let delayed_server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(conversation),
            vad_factory: Some(Arc::new(DelayedStartVadFactory)),
            ..ServerConfig::default()
        },
        delayed_registry.clone(),
    )
    .unwrap();
    let mut delayed_dev = chat_device(&delayed_server, &delayed_registry, 1);
    send_json(
        &mut delayed_dev,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    send_audio(&mut delayed_dev, 1, &uplink(&silence(UP, 420)));
    for _ in 0..6 {
        std::thread::sleep(Duration::from_millis(55));
        send_audio(&mut delayed_dev, 1, &uplink(&tone(UP, 300.0, 60, 8000.0)));
    }
    let delayed_live = read_json(&mut delayed_dev);
    assert_eq!(delayed_live["type"], "stt");
    assert_eq!(
        delayed_live["text"], "延迟判停首屏",
        "本地 VAD 晚报 SpeechStart 时应按 preroll 尾部真实人声跨度回推起音时刻并立即上屏"
    );
    send_audio(&mut delayed_dev, 1, &uplink(&silence(UP, 500)));
    assert_eq!(
        texts(&collect_items(&mut delayed_dev)),
        vec!["听:延迟判停首屏完成", "tts:start", "句:好的。", "tts:stop"]
    );
}
