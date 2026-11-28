//! n71 常连模式：假 MQTT + UDP 设备（不连真实小智）走完 登录 → 不唤醒就在线并能调音量 → 唤醒对话 →
//! 空闲结束这一轮但保持在线 → 没在对话时加工结束改发提示 → 断开下线。
use super::*;
use crate::mqtt::codec;
use moss_voice_providers::BoxFuture;
use std::io::{Read, Write};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicUsize, Ordering};

struct JobTools {
    payload: Arc<Mutex<Value>>,
    sending_polls: Arc<AtomicUsize>,
}

impl ToolExecutor for JobTools {
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
        let payload = self.payload.clone();
        let sending_polls = self.sending_polls.clone();
        Box::pin(async move {
            let snap = payload.lock().unwrap().clone();
            if snap["workflows"]
                .as_array()
                .and_then(|items| items.first())
                .and_then(|w| w["status"].as_str())
                == Some("sending")
            {
                sending_polls.fetch_add(1, Ordering::SeqCst);
            }
            Some(snap)
        })
    }
}

/// 假设备的 MQTT 连接（阻塞读写）。
struct MqttDevice {
    stream: TcpStream,
}

impl MqttDevice {
    fn connect(port: u16, client: &str, user: &str, password: &str) -> (Self, u8) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(&codec::connect(client, user, password, 240))
            .unwrap();
        let mut device = Self { stream };
        let (header, body) = device.read_packet();
        assert_eq!(header, 0x20, "第一个回包是 CONNACK");
        (device, body[1])
    }

    fn read_packet(&mut self) -> (u8, Vec<u8>) {
        let mut first = [0u8; 1];
        self.stream.read_exact(&mut first).unwrap();
        let (mut len, mut shift) = (0usize, 0);
        loop {
            let mut byte = [0u8; 1];
            self.stream.read_exact(&mut byte).unwrap();
            len |= usize::from(byte[0] & 0x7F) << shift;
            if byte[0] & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        let mut body = vec![0u8; len];
        self.stream.read_exact(&mut body).unwrap();
        (first[0], body)
    }

    /// 读下一条下行 JSON（跳过 PUBACK 等）。
    fn read_json(&mut self) -> Value {
        loop {
            let (header, body) = self.read_packet();
            if header >> 4 == 3 {
                let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
                return serde_json::from_slice(&body[2 + topic_len..]).unwrap();
            }
        }
    }

    fn read_mcp(&mut self) -> Value {
        loop {
            let value = self.read_json();
            if value["type"] == "mcp" {
                return value["payload"].clone();
            }
        }
    }

    fn publish(&mut self, value: &Value) {
        self.stream
            .write_all(&codec::publish_qos1(
                crate::mqtt::PUBLISH_TOPIC,
                1,
                value.to_string().as_bytes(),
            ))
            .unwrap();
    }

    fn reply_mcp(&mut self, id: &Value, result: Value) {
        self.publish(&serde_json::json!({"type": "mcp", "payload": {"jsonrpc": "2.0", "id": id, "result": result}}));
    }
}

/// 假设备的 UDP 语音通道：按 hello 回复里的密钥 / nonce 收发，与固件同样的包格式。
struct UdpVoice {
    socket: UdpSocket,
    key: [u8; 16],
    nonce: [u8; 16],
    sequence: u32,
}

fn unhex(text: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}

impl UdpVoice {
    fn open(hello: &Value) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .connect(("127.0.0.1", hello["udp"]["port"].as_u64().unwrap() as u16))
            .unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self {
            socket,
            key: unhex(hello["udp"]["key"].as_str().unwrap()),
            nonce: unhex(hello["udp"]["nonce"].as_str().unwrap()),
            sequence: 0,
        }
    }

    fn send(&mut self, frames: &[Vec<u8>]) {
        for (index, opus) in frames.iter().enumerate() {
            self.sequence += 1;
            let packet = crate::mqtt::udp::seal(
                &self.key,
                &self.nonce,
                opus,
                index as u32 * 60,
                self.sequence,
            );
            self.socket.send(&packet).unwrap();
        }
    }

    /// 收一个下行包并解密，返回 (序号, Opus)；超时返回 None。
    fn recv(&self) -> Option<(u32, Vec<u8>)> {
        let mut buf = [0u8; 4096];
        let len = self.socket.recv(&mut buf).ok()?;
        let packet = &buf[..len];
        let (ssrc, sequence) =
            crate::mqtt::udp::parse_header(packet).expect("下行包格式与固件一致");
        assert_eq!(
            ssrc,
            [self.nonce[4], self.nonce[5], self.nonce[6], self.nonce[7]]
        );
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&packet[..16]);
        let mut opus = packet[16..].to_vec();
        crate::mqtt::udp::crypt(&self.key, &iv, &mut opus);
        Some((sequence, opus))
    }
}

#[test]
fn mqtt_device_stays_online_without_wake_and_talks_over_encrypted_udp() {
    let payload = Arc::new(Mutex::new(serde_json::json!({ "workflows": [] })));
    let sending_polls = Arc::new(AtomicUsize::new(0));
    let conversation = Conversation::new(
        Arc::new(FakeAsr {
            replies: Mutex::new(VecDeque::from([Ok("你好".to_owned())])),
            ..Default::default()
        }),
        Arc::new(FakeLlm {
            rounds: Mutex::new(VecDeque::from([say(&["你好，我在。"])])),
            ..Default::default()
        }),
        Arc::new(FakeTts::default()),
        Arc::new(JobTools {
            payload: payload.clone(),
            sending_polls: sending_polls.clone(),
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
            playback_start_ms: 80,
            idle_timeout: Duration::from_millis(700),
            job_poll_interval: Duration::from_millis(60),
            mqtt: Some(crate::MqttConfig {
                port: 0,
                udp_port: 0,
            }),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let ports = server.mqtt_ports().expect("常连模式已开启");
    assert_eq!(server.status().mqtt_port, Some(ports.port));

    // OTA：待允许的设备拿不到 mqtt 段；允许后拿到，用户名是设备编号、密码是 token
    let (_, pending) = ota(&server, Some(DEVICE));
    assert!(pending.get("mqtt").is_none(), "{pending}");
    registry.set(DEVICE, DeviceStatus::Allowed);
    let (_, body) = ota(&server, Some(DEVICE));
    let mqtt = &body["mqtt"];
    assert_eq!(mqtt["endpoint"], format!("127.0.0.1:{}", ports.port));
    assert_eq!(mqtt["username"], DEVICE);
    assert_eq!(mqtt["client_id"], CLIENT);
    assert_eq!(mqtt["publish_topic"], crate::mqtt::PUBLISH_TOPIC);
    assert_eq!(mqtt["password"], body["websocket"]["token"]);
    let token = mqtt["password"].as_str().unwrap().to_owned();

    // 密码不对：拒绝（CONNACK 5）
    let (_, refused) = MqttDevice::connect(ports.port, CLIENT, DEVICE, "bad-token");
    assert_eq!(refused, 5);

    // 登录：不唤醒就在线，MOSS 马上做设备 MCP 握手
    let (mut device, accepted) = MqttDevice::connect(ports.port, CLIENT, DEVICE, &token);
    assert_eq!(accepted, 0);
    let init = device.read_mcp();
    assert_eq!(init["method"], "initialize");
    device.reply_mcp(
        &init["id"],
        serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}),
    );
    assert_eq!(device.read_mcp()["method"], "notifications/initialized");
    let list = device.read_mcp();
    device.reply_mcp(
        &list["id"],
        serde_json::json!({"tools": [
            {"name": "self.audio_speaker.set_volume", "description": "音量", "inputSchema": {"type": "object", "properties": {"volume": {"type": "integer"}}}}
        ]}),
    );
    wait_until("设备常连在线", || {
        server
            .status()
            .sessions
            .iter()
            .any(|s| s.device_id == DEVICE && s.transport == "mqtt" && !s.device_tools.is_empty())
    });
    // 没唤醒也能调音量
    server
        .device_request(DEVICE, crate::DeviceRequest::SetVolume(40))
        .unwrap();
    let call = device.read_mcp();
    assert_eq!(call["method"], "tools/call");
    assert_eq!(call["params"]["name"], "self.audio_speaker.set_volume");
    assert_eq!(call["params"]["arguments"]["volume"], 40);
    device.reply_mcp(
        &call["id"],
        serde_json::json!({"content": [{"type": "text", "text": "true"}]}),
    );
    // 没在对话时空闲不会被断开
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(server.status().sessions.len(), 1, "常连模式不因空闲下线");

    // 唤醒：hello 申请语音通道，MOSS 回 UDP 地址与本轮密钥
    device.publish(&serde_json::json!({
        "type": "hello", "version": 3, "transport": "udp",
        "features": {"mcp": true},
        "audio_params": {"format": "opus", "sample_rate": 16000, "channels": 1, "frame_duration": 60}
    }));
    let hello = loop {
        let value = device.read_json();
        if value["type"] == "hello" {
            break value;
        }
    };
    assert_eq!(hello["transport"], "udp");
    assert_eq!(hello["audio_params"]["sample_rate"], DOWN);
    assert_eq!(hello["udp"]["key"].as_str().unwrap().len(), 32);
    let session_id = hello["session_id"].as_str().unwrap().to_owned();
    let mut voice = UdpVoice::open(&hello);
    let uplink_frames = uplink(&tone(UP, 300.0, 400, 8000.0));
    device.publish(&serde_json::json!({"session_id": session_id, "type": "listen", "state": "start", "mode": "manual"}));
    // MQTT 控制报文与 UDP 语音分走两个独立接收任务：先等会话真正进入手动拾音，再发 UDP 音频并确认已收音后发 stop
    wait_until("会话进入手动拾音状态", || {
        server
            .status()
            .sessions
            .iter()
            .any(|s| s.session_id == session_id && s.state == "listening")
    });
    voice.send(&uplink_frames);
    wait_until("UDP 上行语音已送达会话", || {
        server
            .status()
            .sessions
            .iter()
            .any(|s| s.session_id == session_id && s.level > 0)
    });
    device
        .publish(&serde_json::json!({"session_id": session_id, "type": "listen", "state": "stop"}));
    let mut saw = (false, false);
    loop {
        let value = device.read_json();
        match (value["type"].as_str(), value["state"].as_str()) {
            (Some("stt"), _) => {
                assert_eq!(value["text"], "你好");
                saw.0 = true;
            }
            (Some("tts"), Some("start")) => saw.1 = true,
            (Some("tts"), Some("stop")) => break,
            _ => {}
        }
    }
    assert!(saw.0 && saw.1, "识别结果与播报开始都经 MQTT 下发");
    // 回复音频经 UDP 加密下发，序号从 1 起递增，能解成 24 kHz 声音
    let mut frames = Vec::new();
    let mut last = 0;
    voice
        .socket
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    while let Some((sequence, opus)) = voice.recv() {
        assert!(sequence > last, "序号递增");
        last = sequence;
        frames.push(opus);
    }
    assert!(!frames.is_empty(), "收到回复音频");
    assert!(!downlink(&frames).is_empty());

    // 空闲：说再见后只关语音通道，设备照旧在线
    let goodbye = loop {
        let value = device.read_json();
        if value["type"] == "goodbye" {
            break value;
        }
    };
    assert_eq!(goodbye["session_id"], session_id.as_str());
    assert_eq!(server.status().sessions.len(), 1, "这一轮对话结束后仍在线");
    // 设备在收到 goodbye 前按固件逻辑（tts stop 后自动回聆听）发出的 listen start 迟到：
    // 不能把已结束的对话拉回聆听；紧跟同连接 ping/pong 确认迟到消息已被会话处理完毕
    device.publish(&serde_json::json!({"session_id": session_id, "type": "listen", "state": "start", "mode": "auto"}));
    device.publish(&serde_json::json!({"type": "ping"}));
    loop {
        if device.read_json()["type"] == "pong" {
            break;
        }
    }
    assert_eq!(
        server.status().sessions[0].state,
        "idle",
        "goodbye 之后迟到的 listen start 被忽略"
    );
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|e| e.text.contains("保持常连在线"))
    );

    // 没在对话时加工结束：不播语音，改发屏幕提示
    *payload.lock().unwrap() = serde_json::json!({
        "workflows": [{"workflow_id": "wf-mqtt-1", "status": "sending",
            "live_progress": {"lines_sent": 10, "total_lines": 100, "elapsed_ms": 1000}}]
    });
    wait_until("常连空闲轮询记录 sending 基线", || {
        sending_polls.load(Ordering::SeqCst) >= 2
    });
    *payload.lock().unwrap() = serde_json::json!({
        "workflows": [{"workflow_id": "wf-mqtt-1", "status": "completed",
            "send_result": {"lines_sent": 100, "total_lines": 100}}]
    });
    let alert = loop {
        let value = device.read_json();
        assert_ne!(value["type"], "tts", "语音通道关着时不能开始播报");
        if value["type"] == "alert" {
            break value;
        }
    };
    assert_eq!(alert["status"], "雕刻完成");
    assert!(alert["message"].as_str().unwrap().contains("100 行"));

    // 断开：下线
    drop(device);
    wait_until("设备下线", || server.status().sessions.is_empty());
}

#[test]
fn mqtt_port_8883_is_refused_and_websocket_keeps_working() {
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mqtt: Some(crate::MqttConfig {
                port: 8883,
                udp_port: 0,
            }),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    assert_eq!(server.mqtt_ports(), None, "8883 会让固件改走 TLS，不开常连");
    registry.set(DEVICE, DeviceStatus::Allowed);
    let (_, body) = ota(&server, Some(DEVICE));
    assert!(body.get("mqtt").is_none(), "常连没开成就只下发 WebSocket");
    assert!(
        body["websocket"]["token"]
            .as_str()
            .is_some_and(|t| !t.is_empty())
    );
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|e| e.text.contains("常连模式没能启动"))
    );
}

#[test]
fn mqtt_standby_restores_saved_text_font_snapshot_for_glyph_push_before_wake() {
    let registry = Arc::new(FakeRegistry::default());
    registry.set(DEVICE, DeviceStatus::Allowed);
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        Arc::new(FakeLlm::default()),
        Arc::new(FakeTts::default()),
        Arc::new(NoTools),
        ConversationSettings::default(),
    );
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            mqtt: Some(crate::MqttConfig {
                port: 0,
                udp_port: 0,
            }),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let ports = server.mqtt_ports().expect("常连模式已开启");
    let (_, body) = ota(&server, Some(DEVICE));
    let token = body["mqtt"]["password"].as_str().unwrap().to_owned();

    // 第 1 次连接并唤醒：上报支持 glyph_push 与 basic 字库，写入 SNAPSHOT_HELLO
    let (mut dev1, accepted) = MqttDevice::connect(ports.port, CLIENT, DEVICE, &token);
    assert_eq!(accepted, 0);
    let init = dev1.read_mcp();
    dev1.reply_mcp(
        &init["id"],
        serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}),
    );
    assert_eq!(dev1.read_mcp()["method"], "notifications/initialized");
    let list = dev1.read_mcp();
    dev1.reply_mcp(&list["id"], serde_json::json!({"tools": []}));
    dev1.publish(&serde_json::json!({
        "type": "hello",
        "version": 3,
        "transport": "udp",
        "features": { "mcp": true, "glyph_push": true },
        "text_font": { "bundle": "noto-v1", "charset": "basic", "size": 20, "bpp": 4 },
        "audio_params": { "format": "opus", "sample_rate": 16000, "channels": 1, "frame_duration": 60 }
    }));
    loop {
        if dev1.read_json()["type"] == "hello" {
            break;
        }
    }
    wait_until("hello 快照已落库", || {
        !registry.snapshots(crate::SNAPSHOT_HELLO).is_empty()
    });
    drop(dev1);
    wait_until("旧常连下线", || server.status().sessions.is_empty());

    // 第 2 次重连模拟设备上电常连待命（未唤醒、未发 hello）
    let (mut dev2, accepted2) = MqttDevice::connect(ports.port, CLIENT, DEVICE, &token);
    assert_eq!(accepted2, 0);
    let init2 = dev2.read_mcp();
    dev2.reply_mcp(
        &init2["id"],
        serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}),
    );
    assert_eq!(dev2.read_mcp()["method"], "notifications/initialized");
    let list2 = dev2.read_mcp();
    dev2.reply_mcp(
        &list2["id"],
        serde_json::json!({"tools": [
            {"name": "self.screen.preview_image", "description": "预览图", "inputSchema": {"type": "object"}}
        ]}),
    );
    wait_until("重连常连会话已恢复字库规格", || {
        server.active_device_sessions().iter().any(|s| {
            s.device_id == DEVICE && s.text_font_size == Some(20) && !s.device_tools.is_empty()
        })
    });

    // 未唤醒直接主动推彩屏预览字幕：应根据恢复的字库规格下发 glyph_push 位图
    server
        .device_request(
            DEVICE,
            crate::DeviceRequest::PreviewScreen {
                token: Some("tok-preview".into()),
                summary: "激光雕刻参数正常".into(),
            },
        )
        .unwrap();
    let stt = loop {
        let msg = dev2.read_json();
        if msg["type"] == "stt" {
            break msg;
        }
    };
    assert!(stt["text"].as_str().unwrap_or("").contains("雕刻"), "{stt}");
    assert!(
        stt["glyph_push"]["glyphs"]
            .as_array()
            .is_some_and(|g| !g.is_empty()),
        "常连开机未唤醒前主动推屏也应携带 glyph_push 点阵: {stt}"
    );
}

#[test]
fn echo_mode_with_eager_device_mcp_discovers_tools_and_screen_size_over_mqtt() {
    let registry = Arc::new(FakeRegistry::default());
    registry
        .devices
        .lock()
        .unwrap()
        .insert(DEVICE.into(), (CLIENT.into(), DeviceStatus::Allowed));
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Echo,
            conversation: None,
            eager_device_mcp: true,
            mqtt: Some(MqttConfig {
                port: 0,
                udp_port: 0,
            }),
            ..ServerConfig::default()
        },
        registry,
    )
    .unwrap();
    let ports = server.mqtt_ports().expect("MQTT 已监听");
    let (_, body) = ota(&server, Some(DEVICE));
    let token = body["mqtt"]["password"].as_str().unwrap().to_owned();

    // 未配置对话模型（Echo 模式）且设备仅通过 MQTT 待机在线（未唤醒）：
    // 仍应立即完成 initialize -> tools/list(withUserTools=true) -> self.screen.get_info，
    // 使桌面端能直接识别 self.assets.set_download_url 与彩屏分辨率并推送唤醒词/全屏画风。
    let (mut device, accepted) = MqttDevice::connect(ports.port, CLIENT, DEVICE, &token);
    assert_eq!(accepted, 0);
    let init = device.read_mcp();
    assert_eq!(init["method"], "initialize");
    device.reply_mcp(
        &init["id"],
        serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}),
    );
    assert_eq!(device.read_mcp()["method"], "notifications/initialized");

    let list = device.read_mcp();
    assert_eq!(list["method"], "tools/list");
    assert_eq!(list["params"]["withUserTools"], true);
    device.reply_mcp(
        &list["id"],
        serde_json::json!({"tools": [
            {"name": "self.assets.set_download_url", "description": "资源包下载", "inputSchema": {"type": "object"}},
            {"name": "self.screen.get_info", "description": "屏幕信息", "inputSchema": {"type": "object"}},
            {"name": "self.reboot", "description": "重启", "inputSchema": {"type": "object"}}
        ]}),
    );

    let screen_call = device.read_mcp();
    assert_eq!(screen_call["method"], "tools/call");
    assert_eq!(screen_call["params"]["name"], "self.screen.get_info");
    device.reply_mcp(
        &screen_call["id"],
        serde_json::json!({"content": [{"type": "text", "text": "{\"width\":320,\"height\":240,\"monochrome\":false}"}]}),
    );

    wait_until(
        "回声模式常连设备完成工具与彩屏分辨率发现",
        || {
            server.active_device_sessions().iter().any(|s| {
                s.device_id == DEVICE
                    && s.device_tools_ready
                    && s.device_tools
                        .iter()
                        .any(|t| t == "self.assets.set_download_url")
                    && s.screen_width == Some(320)
                    && s.screen_height == Some(240)
            })
        },
    );
    assert_eq!(server.screen_pack_size(DEVICE), Some((320, 240)));

    server
        .install_wake_assets(DEVICE, vec![0xAA; 16])
        .expect("回声模式下已发现 self.assets.set_download_url 的设备可推送资源包");
    let set_url = device.read_mcp();
    assert_eq!(set_url["method"], "tools/call");
    assert_eq!(set_url["params"]["name"], "self.assets.set_download_url");
    let reboot = device.read_mcp();
    assert_eq!(reboot["method"], "tools/call");
    assert_eq!(reboot["params"]["name"], "self.reboot");
}

#[test]
fn sync_tools_deduplicates_before_init_and_hello_without_mcp_clears_loading_state() {
    let registry = Arc::new(FakeRegistry::default());
    registry
        .devices
        .lock()
        .unwrap()
        .insert(DEVICE.into(), (CLIENT.into(), DeviceStatus::Allowed));
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Echo,
            conversation: None,
            mqtt: Some(MqttConfig {
                port: 0,
                udp_port: 0,
            }),
            ..ServerConfig::default()
        },
        registry,
    )
    .unwrap();
    let ports = server.mqtt_ports().expect("MQTT 已监听");
    let (_, body) = ota(&server, Some(DEVICE));
    let token = body["mqtt"]["password"].as_str().unwrap().to_owned();

    let (mut device, accepted) = MqttDevice::connect(ports.port, CLIENT, DEVICE, &token);
    assert_eq!(accepted, 0);
    wait_until("常连会话已建立且处于待同步状态", || {
        server
            .active_device_sessions()
            .iter()
            .any(|s| s.device_id == DEVICE && s.device_mcp_enabled && !s.device_tools_ready)
    });

    // 连续轮询 20 次 sync_device_tools：不得重复发 initialize，也不得撑满 16 条延迟队列挤掉后续请求
    for _ in 0..20 {
        server.sync_device_tools();
    }
    server
        .device_request(DEVICE, crate::DeviceRequest::RefreshInfo)
        .unwrap();
    let init = device.read_mcp();
    assert_eq!(init["method"], "initialize");
    device.reply_mcp(
        &init["id"],
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "serverInfo": { "name": "board", "version": "2.5.0" }
        }),
    );
    let initialized_notif = device.read_mcp();
    assert_eq!(initialized_notif["method"], "notifications/initialized");
    let mut saw_tools_list = None;
    let mut saw_status_call = false;
    for _ in 0..2 {
        let msg = device.read_mcp();
        assert_ne!(
            msg["method"], "initialize",
            "不得发出第二个 initialize：{msg}"
        );
        if msg["method"] == "tools/list" {
            assert!(
                saw_tools_list.replace(msg["id"].clone()).is_none(),
                "不得发出第二个 tools/list"
            );
        } else if msg["method"] == "tools/call" && msg["params"]["name"] == "self.get_device_status"
        {
            saw_status_call = true;
        } else {
            panic!("收到意外的 MCP 请求：{msg}");
        }
    }
    let list_id = saw_tools_list.expect("应发出恰好一个 tools/list");
    assert!(saw_status_call, "RefreshInfo 不应被 SyncTools 挤出延迟队列");
    device.reply_mcp(&list_id, serde_json::json!({ "tools": [] }));
    wait_until("空工具列表应答后就绪", || {
        server
            .active_device_sessions()
            .iter()
            .any(|s| s.device_id == DEVICE && s.device_tools_ready)
    });

    // 重连一台旧固件设备：若旧版固件不响应 initialize，而是在唤醒 hello 中上报 features.mcp = false，应立即收口加载态
    drop(device);
    let (mut legacy_device, accepted2) = MqttDevice::connect(ports.port, CLIENT, DEVICE, &token);
    assert_eq!(accepted2, 0);
    wait_until("重连后处于待同步状态", || {
        server
            .active_device_sessions()
            .iter()
            .any(|s| s.device_id == DEVICE && s.device_mcp_enabled && !s.device_tools_ready)
    });
    server.sync_device_tools();
    let init2 = legacy_device.read_mcp();
    assert_eq!(init2["method"], "initialize");
    legacy_device.publish(&serde_json::json!({
        "type": "hello", "version": 3, "transport": "udp",
        "features": {"mcp": false},
        "audio_params": {"format": "opus", "sample_rate": 16000, "channels": 1, "frame_duration": 60}
    }));
    wait_until("hello 上报无 MCP 后退出加载态", || {
        server
            .active_device_sessions()
            .iter()
            .any(|s| s.device_id == DEVICE && !s.device_mcp_enabled && s.device_tools_ready)
    });
}

#[test]
fn device_mcp_timeout_keeps_tools_unready_and_retries_on_sync_tools() {
    let registry = Arc::new(FakeRegistry::default());
    registry
        .devices
        .lock()
        .unwrap()
        .insert(DEVICE.into(), (CLIENT.into(), DeviceStatus::Allowed));
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Echo,
            conversation: None,
            eager_device_mcp: true,
            mcp_pending_ttl: Duration::from_millis(80),
            job_poll_interval: Duration::from_millis(40),
            mqtt: Some(MqttConfig {
                port: 0,
                udp_port: 0,
            }),
            ..ServerConfig::default()
        },
        registry,
    )
    .unwrap();
    let ports = server.mqtt_ports().expect("MQTT 已监听");
    let (_, body) = ota(&server, Some(DEVICE));
    let token = body["mqtt"]["password"].as_str().unwrap().to_owned();

    let (mut device, accepted) = MqttDevice::connect(ports.port, CLIENT, DEVICE, &token);
    assert_eq!(accepted, 0);

    // 初次握手发出 initialize，设备暂不应答使其超时
    let first_init = device.read_mcp();
    assert_eq!(first_init["method"], "initialize");
    std::thread::sleep(Duration::from_millis(160));

    let sessions = server.active_device_sessions();
    let view = sessions
        .iter()
        .find(|s| s.device_id == DEVICE)
        .expect("设备仍在线");
    assert!(
        !view.device_tools_ready,
        "握手超时后不得把 device_tools_ready 标为 true 误报固件不支持"
    );
    assert_eq!(
        server.camera_frame_view(DEVICE, None)["has_camera_tool"],
        serde_json::Value::Null
    );

    // 再次触发 SyncTools 应重新发出 initialize（同一时刻最多一轮在途），设备应答后工具列表正常就绪
    server.sync_device_tools();
    server.sync_device_tools();
    let retry_init = device.read_mcp();
    assert_eq!(retry_init["method"], "initialize");
    assert_ne!(retry_init["id"], first_init["id"]);
    device.reply_mcp(
        &retry_init["id"],
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "serverInfo": { "name": "board", "version": "2.5.0" }
        }),
    );
    let notif = device.read_mcp();
    assert_eq!(notif["method"], "notifications/initialized");
    let list_req = device.read_mcp();
    assert_eq!(list_req["method"], "tools/list");
    device.reply_mcp(
        &list_req["id"],
        serde_json::json!({
            "tools": [
                { "name": "self.assets.set_download_url", "description": "ota assets", "inputSchema": {"type": "object"} },
                { "name": "self.camera.take_photo", "description": "camera", "inputSchema": {"type": "object"} }
            ]
        }),
    );

    wait_until("超时重试后设备工具清单同步就绪", || {
        server.active_device_sessions().iter().any(|s| {
            s.device_id == DEVICE
                && s.device_tools_ready
                && s.device_tools
                    .iter()
                    .any(|t| t == "self.assets.set_download_url")
        })
    });
    assert_eq!(
        server.camera_frame_view(DEVICE, None)["has_camera_tool"],
        serde_json::json!(true)
    );
}

/// 常连待机态（MQTT 未唤醒开 UDP 音频通道）下做文字直通注入与“退下”：
/// 服务端照常跑通对话与合成，并把 `TurnDetail.audio_channel_open` 标为 `false`，
/// 绝不向待机设备发送 `stt` / `tts` / `goodbye`（避免把未开音频通道的设备硬推入“聆听中”）。
#[test]
fn mqtt_idle_text_inject_skips_device_tts_and_goodbye_so_device_stays_idle() {
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            say(&["好的，已记录。"]),
            say(&[
                "这是一段较长的回复，用来确保在测试里进入播放态后再被设备打断不会泄漏悬空的停止播放指令。",
            ]),
        ])),
        ..Default::default()
    });
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        llm,
        Arc::new(FakeTts::default()),
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
            playback_start_ms: 60,
            idle_timeout: Duration::from_secs(60),
            mqtt: Some(crate::MqttConfig {
                port: 0,
                udp_port: 0,
            }),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let ports = server.mqtt_ports().expect("常连模式已开启");
    registry.set(DEVICE, DeviceStatus::Allowed);
    let (_, body) = ota(&server, Some(DEVICE));
    let token = body["mqtt"]["password"].as_str().unwrap().to_owned();

    // 常连登录，但不发 hello（保持待机态，不打开 UDP 语音通道）
    let (mut device, accepted) = MqttDevice::connect(ports.port, CLIENT, DEVICE, &token);
    assert_eq!(accepted, 0);
    let init = device.read_mcp();
    assert_eq!(init["method"], "initialize");
    device.reply_mcp(
        &init["id"],
        serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}),
    );
    assert_eq!(device.read_mcp()["method"], "notifications/initialized");
    let list = device.read_mcp();
    device.reply_mcp(&list["id"], serde_json::json!({"tools": []}));
    wait_until("常连设备上线且处于待机态", || {
        server
            .active_device_sessions()
            .iter()
            .any(|s| s.device_id == DEVICE && s.state == "idle" && s.device_tools_ready)
    });

    // 1. 注入普通对话文字
    let ack1 = server.inject_text(DEVICE, "随便测试一句", true).unwrap();
    wait_until("待机文字直通第 1 轮完成", || {
        server
            .turn_detail(&ack1.turn_id)
            .is_some_and(|d| d.status == "done")
    });
    let d1 = server.turn_detail(&ack1.turn_id).unwrap();
    assert!(!d1.audio_channel_open, "未开 UDP 音频通道须标为 false");
    assert!(d1.reply.contains("已记录"));

    // 2. 注入噪声/标点触发 Ignored：不得切入 listening，必须保持 idle
    let ack_ign = server.inject_text(DEVICE, "。。。", true).unwrap();
    wait_until("待机文字直通 Ignored 轮完成", || {
        server
            .turn_detail(&ack_ign.turn_id)
            .is_some_and(|d| d.status == "ignored")
    });
    assert_eq!(server.status().sessions[0].state, "idle");

    // 3. 注入并中途发 abort 打断正在播放的 silent 轮次：不得泄漏悬空 tts stop
    let ack_abort = server.inject_text(DEVICE, "长回复测试", true).unwrap();
    wait_until("silent 轮次进入 speaking", || {
        server
            .active_device_sessions()
            .iter()
            .any(|s| s.device_id == DEVICE && s.state == "speaking")
    });
    device.publish(&serde_json::json!({"type": "abort", "reason": "wake_word_detected"}));
    wait_until("silent 轮次被打断并回到 idle", || {
        server
            .turn_detail(&ack_abort.turn_id)
            .is_some_and(|d| d.status == "interrupted")
            && server
                .active_device_sessions()
                .iter()
                .any(|s| s.device_id == DEVICE && s.state == "idle")
    });

    // 4. 注入退出词“退下”
    let ack2 = server.inject_text(DEVICE, "退下", true).unwrap();
    wait_until("待机文字直通退出轮完成", || {
        server
            .turn_detail(&ack2.turn_id)
            .is_some_and(|d| d.status == "done")
    });
    let d2 = server.turn_detail(&ack2.turn_id).unwrap();
    assert!(!d2.audio_channel_open);
    assert_eq!(d2.route, "fast_exit");

    // 5. 发一个 ping 并收 pong，确认期间服务端从未下发任何 stt / tts / goodbye
    device.publish(&serde_json::json!({"type": "ping"}));
    loop {
        let value = device.read_json();
        let ty = value["type"].as_str().unwrap_or("");
        assert!(
            !matches!(ty, "stt" | "tts" | "goodbye" | "llm"),
            "常连待机态文字直通绝不能向设备发 stt/tts/goodbye/llm，实际收到：{value}"
        );
        if ty == "pong" {
            break;
        }
    }
    assert_eq!(server.status().sessions.len(), 1, "设备保持常连在线");
    assert_eq!(server.status().sessions[0].state, "idle");
    server.stop();
}
