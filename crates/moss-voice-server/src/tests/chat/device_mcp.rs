//! 设备自带 MCP：视觉与相机能力、工具列举与转发、仅限用户的工具、唤醒词包推送。
use super::*;

/// NV12: advertise the actual 8003 HTTP port and a token only to allowed,
/// MCP-capable devices when the configured provider is available.
#[test]
fn vision_capability_is_conditional_and_uses_bound_http_port() {
    use crate::vision::{VisionFactory, VisionSession};
    use moss_voice_providers::vision::{VisionFailure, VisionProvider};
    use std::sync::atomic::AtomicBool;
    struct ReadyProvider;
    impl VisionProvider for ReadyProvider {
        fn explain<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: &'a [u8],
        ) -> BoxFuture<'a, Result<String, VisionFailure>> {
            Box::pin(async { Ok("图片解释".into()) })
        }
    }
    struct ReadyFactory {
        enabled: Arc<AtomicBool>,
    }
    impl VisionFactory for ReadyFactory {
        fn ready(&self) -> bool {
            self.enabled.load(std::sync::atomic::Ordering::Acquire)
        }
        fn open(&self) -> Option<VisionSession> {
            self.ready().then(|| VisionSession {
                provider: Arc::new(ReadyProvider),
                generation: 1,
            })
        }
        fn still_current(&self, _: u64) -> bool {
            self.ready()
        }
    }
    let on = Arc::new(AtomicBool::new(true));
    let factory = Arc::new(ReadyFactory { enabled: on });
    let (server, registry, _fakes) = start_chat_with(
        FakeLlm::default(),
        &[],
        Duration::from_secs(120),
        Some(factory),
    );
    let mut device = allowed_device_with(&server, &registry, 1, true);
    let initial = read_mcp(&mut device);
    assert_eq!(initial["method"], "initialize");
    let cap = &initial["params"]["capabilities"]["vision"];
    assert_eq!(
        cap["url"],
        format!("http://127.0.0.1:{}/mcp/vision/explain", server.http_port())
    );
    let token = cap["token"].as_str().unwrap();
    assert!(TokenSigner::new(SECRET.to_vec()).verify(token, CLIENT, DEVICE, unix_now()));
    let off = Arc::new(AtomicBool::new(false));
    let (disabled, registered, _) = start_chat_with(
        FakeLlm::default(),
        &[],
        Duration::from_secs(120),
        Some(Arc::new(ReadyFactory { enabled: off })),
    );
    let mut device_off = allowed_device_with(&disabled, &registered, 1, true);
    let init_off = read_mcp(&mut device_off);
    assert!(init_off["params"]["capabilities"]["vision"].is_null());
}

/// 桌面端「摄像头」标签页抓拍：即使未配置视觉大模型，点击抓拍时也会按需补发 `capabilities.vision`，
/// 固件经 `self.camera.take_photo` 上传 JPEG 后直接进入内存帧；设备拍照报错时立即写入错误态。
#[test]
fn desktop_camera_capture_works_without_vision_provider_and_surfaces_errors() {
    let wait_until = |cond: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !cond() {
            assert!(std::time::Instant::now() < deadline, "等待条件超时");
            std::thread::sleep(Duration::from_millis(15));
        }
    };
    let (server, registry, _fakes) =
        start_chat_with(FakeLlm::default(), &[], Duration::from_secs(120), None);
    let mut device = allowed_device_with(&server, &registry, 1, true);

    let init = read_mcp(&mut device);
    assert_eq!(init["method"], "initialize");
    assert!(
        init["params"]["capabilities"]["vision"].is_null(),
        "未配置视觉模型时初始握手不下发 vision"
    );
    reply_mcp(
        &mut device,
        &init["id"],
        serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}),
    );
    assert_eq!(read_mcp(&mut device)["method"], "notifications/initialized");
    let list = read_mcp(&mut device);
    assert_eq!(list["method"], "tools/list");
    reply_mcp(
        &mut device,
        &list["id"],
        serde_json::json!({
            "tools": [
                {"name": "self.camera.take_photo", "description": "拍照", "inputSchema": {"type": "object", "properties": {"question": {"type": "string"}}}}
            ]
        }),
    );
    wait_until(&|| {
        server
            .status()
            .sessions
            .iter()
            .any(|s| s.device_tools.iter().any(|t| t == "self.camera.take_photo"))
    });

    // 第一次抓拍：应先收到补发的 initialize(vision)，再收到 self.camera.take_photo
    server
        .request_camera_capture(DEVICE)
        .expect("在线摄像头设备应允许抓拍");
    let vision_init = read_mcp(&mut device);
    assert_eq!(vision_init["method"], "initialize");
    let explain_url = vision_init["params"]["capabilities"]["vision"]["url"]
        .as_str()
        .expect("补发握手须带 vision.url")
        .to_owned();
    let explain_token = vision_init["params"]["capabilities"]["vision"]["token"]
        .as_str()
        .expect("补发握手须带 vision.token")
        .to_owned();
    reply_mcp(
        &mut device,
        &vision_init["id"],
        serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {}}),
    );

    let take = read_mcp(&mut device);
    assert_eq!(take["method"], "tools/call");
    assert_eq!(take["params"]["name"], "self.camera.take_photo");
    let question = take["params"]["arguments"]["question"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(question.starts_with(crate::vision::CAMERA_PREVIEW_PREFIX));

    // 模拟固件 camera->Explain(question) 上传 JPEG
    let mut jpg = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(16, 12)
        .write_to(&mut jpg, image::ImageFormat::Jpeg)
        .unwrap();
    let jpg = jpg.into_inner();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--boundary\r\nContent-Disposition: form-data; name=\"question\"\r\n\r\n{question}\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        b"--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"camera.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n",
    );
    body.extend_from_slice(&jpg);
    body.extend_from_slice(b"\r\n--boundary--\r\n");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let upload = client
        .post(&explain_url)
        .header("Device-Id", DEVICE)
        .header("Client-Id", CLIENT)
        .header("Authorization", format!("Bearer {explain_token}"))
        .header("Content-Type", "multipart/form-data; boundary=boundary")
        .body(body)
        .send()
        .unwrap();
    assert_eq!(upload.status().as_u16(), 200);
    let upload_text = upload.text().unwrap();
    reply_mcp(
        &mut device,
        &take["id"],
        serde_json::json!({"content": [{"type": "text", "text": upload_text}], "isError": false}),
    );

    let view = server.camera_frame_view(DEVICE, None);
    assert_eq!(view["online"], true);
    assert_eq!(view["has_camera_tool"], true);
    assert_eq!(view["pending"], false);
    assert_eq!(view["frame"]["source"], "preview");
    assert_eq!(view["frame"]["width"], 16);
    assert_eq!(view["frame"]["height"], 12);

    // 第二次抓拍模拟传感器报错：不再重复补发 initialize，直接调用 take_photo 并写入中文报错提示
    server.request_camera_capture(DEVICE).unwrap();
    let take_fail = read_mcp(&mut device);
    assert_eq!(take_fail["method"], "tools/call");
    assert_eq!(take_fail["params"]["name"], "self.camera.take_photo");
    reply_mcp(
        &mut device,
        &take_fail["id"],
        serde_json::json!({"content": [{"type": "text", "text": "Failed to capture photo"}], "isError": true}),
    );
    wait_until(&|| server.camera_frame_view(DEVICE, None)["last_error"].is_string());
    let err_view = server.camera_frame_view(DEVICE, None);
    assert_eq!(err_view["pending"], false);
    assert!(
        err_view["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("摄像头抓拍失败")
    );
}

/// NV4 设备自带 MCP：握手、分页取工具、模型以清理后的名字调用，设备收到原名，结果回填给模型。
#[test]
fn device_mcp_tools_are_listed_and_relayed() {
    let call = Round::Reply {
        deltas: Vec::new(),
        calls: vec![ToolCall::new(
            "c1",
            "device__self_audio_speaker_set_volume",
            r#"{"volume":50}"#,
        )],
        delay_ms: 0,
    };
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([call, say(&["好了。"])])),
        ..FakeLlm::default()
    };
    let (server, registry, fakes) = start_chat(llm, &["音量调到一半"], Duration::from_secs(120));
    let mut device = allowed_device_with(&server, &registry, 1, true);

    let initialize = read_mcp(&mut device);
    assert_eq!(initialize["method"], "initialize");
    assert_eq!(initialize["params"]["clientInfo"]["name"], "MOSS");
    reply_mcp(
        &mut device,
        &initialize["id"],
        serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}),
    );
    assert_eq!(read_mcp(&mut device)["method"], "notifications/initialized");
    let page1 = read_mcp(&mut device);
    assert_eq!(page1["method"], "tools/list");
    reply_mcp(
        &mut device,
        &page1["id"],
        serde_json::json!({
            "tools": [{"name": "self.get_device_status", "description": "设备状态", "inputSchema": {"type": "object", "properties": {}}}],
            "nextCursor": "p2"
        }),
    );
    let page2 = read_mcp(&mut device);
    assert_eq!(page2["params"]["cursor"], "p2");
    reply_mcp(
        &mut device,
        &page2["id"],
        serde_json::json!({
            "tools": [{"name": "self.audio_speaker.set_volume", "description": "设置音量", "inputSchema": {"type": "object", "properties": {"volume": {"type": "integer"}}}}]
        }),
    );

    speak_manual(&mut device, 1, 600);
    assert_eq!(read_json(&mut device)["text"], "音量调到一半");
    let call = read_mcp(&mut device);
    assert_eq!(call["method"], "tools/call");
    assert_eq!(
        call["params"]["name"], "self.audio_speaker.set_volume",
        "设备收到原始工具名"
    );
    assert_eq!(call["params"]["arguments"]["volume"], 50);
    reply_mcp(
        &mut device,
        &call["id"],
        serde_json::json!({"content": [{"type": "text", "text": "音量已设为 50"}], "isError": false}),
    );
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["tts:start", "句:好了。", "tts:stop"]
    );

    let seen = fakes.llm.seen.lock().unwrap();
    assert_eq!(
        seen[0].1, 4,
        "模型看到 MOSS 工具 1 个 + 设备工具 2 个 + 宿主保留的 end_conversation"
    );
    let result = seen[1].0.last().unwrap();
    assert_eq!(
        (result.role, result.content.as_deref()),
        (Role::Tool, Some("音量已设为 50"))
    );
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.contains("设备提供 2 个自带工具"))
    );
}

#[test]
fn device_mcp_results_are_turned_into_text() {
    use crate::session::device_result_text;
    let error = serde_json::json!({"id": 3, "error": {"code": -32601, "message": "Unknown tool"}});
    assert_eq!(device_result_text(&error), "设备工具出错：Unknown tool");
    let failed = serde_json::json!({"id": 3, "result": {"content": [{"type": "text", "text": "摄像头不可用"}], "isError": true}});
    assert_eq!(
        device_result_text(&failed),
        "设备工具执行失败：摄像头不可用"
    );
    let raw = serde_json::json!({"id": 3, "result": {"battery": 80}});
    assert_eq!(device_result_text(&raw), r#"{"battery":80}"#);
}

#[test]
fn user_only_device_tools_are_hidden_from_llm_but_usable_by_host_and_preview_push() {
    use image::{ImageFormat, Rgba, RgbaImage};
    use std::io::Cursor;

    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&["收到。"])])),
        repeat: None,
        seen: Mutex::default(),
    };
    let (server, registry, fakes) = start_chat(llm, &["随便聊聊"], Duration::from_secs(120));
    let mut device = allowed_device_with(&server, &registry, 1, true);

    let initialize = read_mcp(&mut device);
    reply_mcp(
        &mut device,
        &initialize["id"],
        serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}),
    );
    assert_eq!(read_mcp(&mut device)["method"], "notifications/initialized");
    let list_req = read_mcp(&mut device);
    assert_eq!(list_req["method"], "tools/list");
    assert_eq!(
        list_req["params"]["withUserTools"], true,
        "握手必须请求发现上位机专属工具"
    );
    reply_mcp(
        &mut device,
        &list_req["id"],
        serde_json::json!({
            "tools": [
                {"name": "self.get_device_status", "description": "状态", "inputSchema": {"type": "object", "properties": {}}},
                {"name": "self.audio_speaker.set_volume", "description": "音量", "inputSchema": {"type": "object", "properties": {"volume": {"type": "integer"}}}},
                {"name": "self.screen.preview_image", "description": "屏幕预览", "inputSchema": {"type": "object", "properties": {"url": {"type": "string"}}}},
                {"name": "self.reboot", "description": "重启", "inputSchema": {"type": "object", "properties": {}}},
                {"name": "self.upgrade_firmware", "description": "升级", "inputSchema": {"type": "object", "properties": {"url": {"type": "string"}}}}
            ]
        }),
    );

    // 1. 对话中大模型只能看到非 user_only 工具（MOSS 1 个 + 设备模型可见工具 2 个 + 宿主保留的 end_conversation = 4 个）
    speak_manual(&mut device, 1, 600);
    let _ = collect_items(&mut device);
    {
        let seen = fakes.llm.seen.lock().unwrap();
        assert_eq!(seen[0].1, 4, "user_only 工具严禁暴露给大模型");
    }
    let session_tools = &server.status().sessions[0].device_tools;
    assert!(
        session_tools
            .iter()
            .any(|t| t == "self.screen.preview_image")
    );

    // 2. 上位机调节音量（含静音 0%）下发 set_volume 并自动跟进刷新状态
    let dev_id = server.status().sessions[0].device_id.clone();
    server
        .device_request(&dev_id, crate::DeviceRequest::SetVolume(0))
        .unwrap();
    let vol_call = read_mcp(&mut device);
    assert_eq!(vol_call["params"]["name"], "self.audio_speaker.set_volume");
    assert_eq!(vol_call["params"]["arguments"]["volume"], 0);
    let status_call = read_mcp(&mut device);
    assert_eq!(status_call["params"]["name"], "self.get_device_status");
    reply_mcp(
        &mut device,
        &vol_call["id"],
        serde_json::json!({"content": [{"type": "text", "text": "true"}]}),
    );
    reply_mcp(
        &mut device,
        &status_call["id"],
        serde_json::json!({"audio_speaker": {"volume": 0}}),
    );

    // 3. 推送激光雕刻预览图：生成紧凑 PNG、带 Content-Length 响应头供固件拉取，并下发 self.screen.preview_image
    let mut img = RgbaImage::new(320, 320);
    for p in img.pixels_mut() {
        *p = Rgba([0, 220, 140, 255]);
    }
    let mut png_bytes = Vec::new();
    img.write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png)
        .unwrap();
    let pushed = server.push_preview_png(&png_bytes, None).unwrap();
    assert_eq!(pushed, 1);
    let preview_call = read_mcp(&mut device);
    assert_eq!(preview_call["params"]["name"], "self.screen.preview_image");
    let url = preview_call["params"]["arguments"]["url"].as_str().unwrap();
    let resp = reqwest::blocking::get(url).unwrap();
    assert_eq!(resp.status(), 200);
    let content_len: usize = resp
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let body = resp.bytes().unwrap();
    assert_eq!(
        content_len,
        body.len(),
        "固件 GetBodyLength 依赖精确 Content-Length"
    );
    assert!(&body[..8] == b"\x89PNG\r\n\x1a\n");
}

#[test]
fn n61_wake_word_package_is_pushed_by_url_then_reboot_and_served_once_staged() {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&["收到。"])])),
        repeat: None,
        seen: Mutex::default(),
    };
    let (server, registry, fakes) = start_chat(llm, &["随便聊聊"], Duration::from_secs(120));
    let mut device = allowed_device_with(&server, &registry, 1, true);
    let initialize = read_mcp(&mut device);
    reply_mcp(
        &mut device,
        &initialize["id"],
        serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}),
    );
    assert_eq!(read_mcp(&mut device)["method"], "notifications/initialized");
    let list_req = read_mcp(&mut device);
    reply_mcp(
        &mut device,
        &list_req["id"],
        serde_json::json!({"tools": [
            {"name": "self.get_device_status", "description": "状态", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "self.reboot", "description": "重启", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "self.assets.set_download_url", "description": "资源", "inputSchema": {"type": "object", "properties": {"url": {"type": "string"}}}}
        ]}),
    );
    speak_manual(&mut device, 1, 600);
    let _ = collect_items(&mut device);
    assert_eq!(
        fakes.llm.seen.lock().unwrap()[0].1,
        3,
        "资源包工具是 user-only，模型看不到也调不了（余下含宿主保留的 end_conversation）"
    );

    let dev_id = server.status().sessions[0].device_id.clone();
    assert!(
        server.install_wake_assets("someone-else", vec![1]).is_err(),
        "不在线的设备不推送"
    );
    let package = crate::wake_assets::build_package(
        None,
        &crate::wake_assets::WakeTarget::Custom(
            crate::wake_assets::WakeWord::new("你好 MOSS", "ni hao mo si", 0.2).unwrap(),
        ),
    )
    .unwrap();
    server
        .install_wake_assets(&dev_id, package.clone())
        .unwrap();
    let set_url = read_mcp(&mut device);
    assert_eq!(set_url["params"]["name"], "self.assets.set_download_url");
    let url = set_url["params"]["arguments"]["url"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        url.starts_with("http://") && url.contains("/xiaozhi/assets/") && url.ends_with(".bin")
    );
    let reboot = read_mcp(&mut device);
    assert_eq!(
        reboot["params"]["name"], "self.reboot",
        "先记下载地址，再重启让设备自己下载"
    );
    assert!(
        registry.assets_downloaded.lock().unwrap().is_empty(),
        "只是推送了地址，设备还没下载"
    );

    let resp = reqwest::blocking::get(&url).unwrap();
    assert_eq!(resp.status(), 200);
    let content_len: usize = resp
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let body = resp.bytes().unwrap();
    assert_eq!(
        (content_len, body.as_ref()),
        (package.len(), package.as_slice()),
        "固件按 Content-Length 判断分区放不放得下"
    );
    // 2026-11-09：整包交出后才通知宿主“下载完了”，宿主据此把全屏画风记为已装
    let deadline = Instant::now() + Duration::from_secs(5);
    while registry.assets_downloaded.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(registry.assets_downloaded.lock().unwrap()[0], dev_id);
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|e| e.text.contains("已下载完")),
        "下载完记一条事件"
    );
    let wrong = url
        .replace("/xiaozhi/assets/", "/xiaozhi/assets/0")
        .replacen(".bin", "", 1);
    assert_eq!(reqwest::blocking::get(&wrong).unwrap().status(), 404);
}
