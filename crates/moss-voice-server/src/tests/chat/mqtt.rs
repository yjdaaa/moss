//! n71 常连模式：假 MQTT + UDP 设备（不连真实小智）走完 登录 → 不唤醒就在线并能调音量 → 唤醒对话 →
//! 空闲结束这一轮但保持在线 → 没在对话时加工结束改发提示 → 断开下线。
use super::*;
use crate::mqtt::codec;
use moss_voice_providers::BoxFuture;
use std::io::{Read, Write};
use std::net::UdpSocket;

struct JobTools {
    payload: Arc<Mutex<Value>>,
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
        let snap = self.payload.lock().unwrap().clone();
        Box::pin(async move { Some(snap) })
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

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !ok() {
        assert!(std::time::Instant::now() < deadline, "等待超时：{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn mqtt_device_stays_online_without_wake_and_talks_over_encrypted_udp() {
    let payload = Arc::new(Mutex::new(serde_json::json!({ "workflows": [] })));
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
    device.publish(&serde_json::json!({"session_id": session_id, "type": "listen", "state": "start", "mode": "manual"}));
    voice.send(&uplink(&tone(UP, 300.0, 400, 8000.0)));
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
    std::thread::sleep(Duration::from_millis(200));
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
