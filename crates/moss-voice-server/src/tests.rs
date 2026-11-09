//! 端到端测试：真实绑定本机端口，用 reqwest 走 OTA、用 tungstenite 扮演设备（不需要真实小智设备）。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use moss_xiaozhi_proto::{TokenSigner, encode_frame};

use crate::audio::test_signal::{rms, silence, tone};
use crate::audio::{OpusDecoder, OpusEncoder};
use serde_json::Value;
use tungstenite::Message;
use tungstenite::client::IntoClientRequest;

use super::*;

const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
const DEVICE: &str = "aa:bb:cc:dd:ee:ff";
const CLIENT: &str = "uuid-device-1";

#[derive(Default)]
struct FakeRegistry {
    devices: Mutex<HashMap<String, (String, DeviceStatus)>>,
    leases: Mutex<HashMap<String, (String, Arc<std::sync::atomic::AtomicBool>)>>,
    /// 写入过的设备快照（设备、种类、JSON）
    snapshots: Mutex<Vec<(String, String, String)>>,
    /// 按真实数据库语义登记：同 Device-Id、不同 Client-Id 的来访不改绑定，返回已绑定记录的状态
    /// （`laser_runtime::RuntimeDb::voice_device_seen`）。
    keep_binding: std::sync::atomic::AtomicBool,
    /// 上位机为设备记忆的硬件偏好（n70 测试全屏画风包标记）
    hardware_prefs: Mutex<Option<HardwarePrefs>>,
    /// 整包下载完资源包的设备（2026-11-09）
    assets_downloaded: Mutex<Vec<String>>,
}

impl FakeRegistry {
    fn set(&self, device_id: &str, status: DeviceStatus) {
        let mut devices = self.devices.lock().unwrap();
        let entry = devices
            .entry(device_id.to_owned())
            .or_insert_with(|| (CLIENT.to_owned(), status));
        entry.1 = status;
        if status != DeviceStatus::Allowed {
            for (d, flag) in self.leases.lock().unwrap().values() {
                if d == device_id {
                    flag.store(false, std::sync::atomic::Ordering::Release);
                }
            }
        }
    }

    fn status(&self, device_id: &str) -> Option<DeviceStatus> {
        self.devices
            .lock()
            .unwrap()
            .get(device_id)
            .map(|entry| entry.1)
    }

    fn snapshots(&self, kind: &str) -> Vec<String> {
        self.snapshots
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, k, _)| k == kind)
            .map(|(_, _, json)| json.clone())
            .collect()
    }
}

impl DeviceRegistry for FakeRegistry {
    fn device_hardware_prefs(&self, _device: &str) -> Option<HardwarePrefs> {
        self.hardware_prefs.lock().unwrap().clone()
    }
    fn assets_downloaded(&self, device: &str) {
        self.assets_downloaded
            .lock()
            .unwrap()
            .push(device.to_owned());
    }
    fn seen(&self, visit: &DeviceVisit) -> Result<DeviceStatus, String> {
        let mut devices = self.devices.lock().unwrap();
        let entry = devices
            .entry(visit.device_id.clone())
            .or_insert_with(|| (visit.client_id.clone(), DeviceStatus::Pending));
        if entry.0 != visit.client_id
            && self.keep_binding.load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(entry.1);
        }
        if entry.0 != visit.client_id && entry.1 == DeviceStatus::Allowed {
            entry.1 = DeviceStatus::Pending;
        }
        entry.0 = visit.client_id.clone();
        Ok(entry.1)
    }

    fn authorize_http(&self, device: &str, client: &str) -> Result<Option<u64>, String> {
        Ok(self
            .devices
            .lock()
            .map_err(|_| "设备锁不可用".to_owned())?
            .get(device)
            .and_then(|(registered, status)| {
                (registered == client && *status == DeviceStatus::Allowed).then_some(1)
            }))
    }
    fn open_session(
        &self,
        device: &str,
        client: &str,
        session: &str,
    ) -> Result<Arc<std::sync::atomic::AtomicBool>, String> {
        let devices = self.devices.lock().unwrap();
        if !devices
            .get(device)
            .is_some_and(|(c, s)| c == client && *s == DeviceStatus::Allowed)
        {
            return Err("revoked".into());
        }
        let allowed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        self.leases
            .lock()
            .unwrap()
            .insert(session.into(), (device.into(), allowed.clone()));
        Ok(allowed)
    }
    fn close_session(&self, session: &str) {
        self.leases.lock().unwrap().remove(session);
    }

    fn token_secret(&self) -> Result<Vec<u8>, String> {
        Ok(SECRET.to_vec())
    }

    fn device_snapshot(&self, device: &str, kind: &str, json: &str) {
        self.snapshots
            .lock()
            .unwrap()
            .push((device.into(), kind.into(), json.into()));
    }
}

fn start() -> (VoiceServer, Arc<FakeRegistry>) {
    let registry = Arc::new(FakeRegistry::default());
    let config = ServerConfig {
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        ws_port: 0,
        http_port: 0,
        ..ServerConfig::default()
    };
    let server = VoiceServer::start(config, registry.clone()).unwrap();
    (server, registry)
}

fn ota(server: &VoiceServer, device: Option<&str>) -> (u16, Value) {
    let client = reqwest::blocking::Client::new();
    let mut request = client
        .post(format!(
            "http://127.0.0.1:{}/xiaozhi/ota/",
            server.http_port()
        ))
        .header("Client-Id", CLIENT)
        .header("Content-Type", "application/json")
        .body(r#"{"application":{"version":"1.9.2"},"board":{"type":"lichuang-dev"}}"#);
    if let Some(device) = device {
        request = request.header("Device-Id", device);
    }
    let response = request.send().unwrap();
    let status = response.status().as_u16();
    let text = response.text().unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

type Device = tungstenite::WebSocket<TcpStream>;

fn connect(server: &VoiceServer, headers: &[(&'static str, String)]) -> Device {
    let stream = TcpStream::connect(("127.0.0.1", server.ws_port())).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = format!("ws://127.0.0.1:{}/xiaozhi/v1/", server.ws_port())
        .into_client_request()
        .unwrap();
    for (name, value) in headers {
        request.headers_mut().insert(*name, value.parse().unwrap());
    }
    tungstenite::client(request, stream).unwrap().0
}

fn device_headers(token: Option<&str>, version: u32) -> Vec<(&'static str, String)> {
    let mut headers = vec![
        ("device-id", DEVICE.to_owned()),
        ("client-id", CLIENT.to_owned()),
        ("protocol-version", version.to_string()),
    ];
    if let Some(token) = token {
        headers.push(("authorization", format!("Bearer {token}")));
    }
    headers
}

fn send_json(device: &mut Device, text: &str) {
    device.send(Message::text(text.to_owned())).unwrap();
}

fn hello(device: &mut Device, version: u32) -> Value {
    hello_with(device, version, true)
}

fn hello_with(device: &mut Device, version: u32, mcp: bool) -> Value {
    send_json(
        device,
        &format!(
            r#"{{"type":"hello","version":{version},"features":{{"mcp":{mcp}}},"transport":"websocket","audio_params":{{"format":"opus","sample_rate":16000,"channels":1,"frame_duration":60}}}}"#
        ),
    );
    read_json(device)
}

fn read_json(device: &mut Device) -> Value {
    loop {
        match device.read().unwrap() {
            Message::Text(text) => return serde_json::from_str(text.as_str()).unwrap(),
            Message::Binary(_) => panic!("期望文本消息，收到音频帧"),
            _ => {}
        }
    }
}

fn expect_closed(device: &mut Device) {
    loop {
        match device.read() {
            Ok(Message::Close(_)) | Err(_) => return,
            Ok(Message::Text(text)) => panic!("期望断开，又收到 {text}"),
            Ok(_) => {}
        }
    }
}

/// 收集一次回放：返回（文本消息、音频负载、从 tts start 到 tts stop 的耗时）。
fn collect_playback(device: &mut Device, version: u32) -> (Vec<Value>, Vec<Vec<u8>>, Duration) {
    let (mut texts, mut frames) = (Vec::new(), Vec::new());
    let mut started: Option<Instant> = None;
    loop {
        match device.read().unwrap() {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                if value["type"] == "tts" && value["state"] == "start" {
                    started = Some(Instant::now());
                }
                let stop = value["type"] == "tts" && value["state"] == "stop";
                texts.push(value);
                if stop {
                    return (
                        texts,
                        frames,
                        started.map(|at| at.elapsed()).unwrap_or_default(),
                    );
                }
            }
            Message::Binary(data) => {
                frames.push(
                    moss_xiaozhi_proto::decode_frame(version, &data)
                        .unwrap()
                        .payload,
                );
            }
            _ => {}
        }
    }
}

const UP: u32 = 16_000;
const DOWN: u32 = 24_000;

/// 把 PCM 编码成设备上行的 60 ms Opus 帧。
fn uplink(pcm: &[i16]) -> Vec<Vec<u8>> {
    let mut encoder = OpusEncoder::new(UP, 60).unwrap();
    pcm.chunks(encoder.frame())
        .map(|chunk| encoder.encode(chunk).unwrap())
        .collect()
}

fn send_audio(device: &mut Device, version: u32, frames: &[Vec<u8>]) {
    for (index, frame) in frames.iter().enumerate() {
        device
            .send(Message::binary(
                encode_frame(version, frame, index as u32 * 60).unwrap(),
            ))
            .unwrap();
    }
}

/// 用设备的 24 kHz 解码器解出下行音频。
fn downlink(frames: &[Vec<u8>]) -> Vec<i16> {
    let mut decoder = OpusDecoder::new(DOWN).unwrap();
    frames
        .iter()
        .flat_map(|frame| decoder.decode(frame).unwrap())
        .collect()
}

/// 从回声的 stt 文本“回声测试：约 1.2 秒”中取出秒数。
fn stt_seconds(texts: &[Value]) -> f64 {
    let text = texts[0]["text"].as_str().unwrap();
    text.split("约 ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(-1.0)
}

#[test]
fn stalled_local_vad_does_not_block_two_devices_ping_or_revoke() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Gate {
        entered: AtomicUsize,
        open: Mutex<bool>,
        cv: std::sync::Condvar,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            *self.0.open.lock().unwrap() = true;
            self.0.cv.notify_all();
        }
    }
    struct Guard(Arc<Gate>);
    struct Factory(Arc<Gate>);
    struct Detector(Arc<Gate>);
    impl crate::audio::VadFactory for Factory {
        fn create(
            &self,
            _: u32,
            _: &crate::audio::VadConfig,
        ) -> Option<Box<dyn crate::audio::Vad>> {
            Some(Box::new(Detector(self.0.clone())))
        }
    }
    impl crate::audio::Vad for Detector {
        fn reset(&mut self) {}
        fn in_speech(&self) -> bool {
            false
        }
        fn accept(&mut self, _: &[i16]) -> Vec<crate::audio::VadEvent> {
            self.0.entered.fetch_add(1, Ordering::Release);
            let lock = self.0.open.lock().unwrap();
            let _guard = self
                .0
                .cv
                .wait_timeout_while(lock, Duration::from_secs(3), |open| !*open)
                .unwrap();
            Vec::new()
        }
    }
    let gate = Arc::new(Gate {
        entered: AtomicUsize::new(0),
        open: Mutex::new(false),
        cv: std::sync::Condvar::new(),
    });
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            vad_factory: Some(Arc::new(Factory(gate.clone()))),
            ..Default::default()
        },
        registry.clone(),
    )
    .unwrap();
    let _release = Guard(gate.clone());
    let mut a = allowed_device_with(&server, &registry, 1, false);
    let mut b = allowed_device_with(&server, &registry, 1, false);
    for device in [&mut a, &mut b] {
        send_json(device, r#"{"type":"listen","state":"start","mode":"auto"}"#);
        send_audio(device, 1, &uplink(&tone(UP, 300.0, 60, 8000.0)));
        device
            .get_mut()
            .set_read_timeout(Some(Duration::from_millis(700)))
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    while gate.entered.load(Ordering::Acquire) < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(gate.entered.load(Ordering::Acquire), 2);
    for device in [&mut a, &mut b] {
        send_json(device, r#"{"type":"ping"}"#);
        assert_eq!(
            read_json(device)["type"],
            "pong",
            "both slow model calls must leave socket loop runnable"
        );
    }
    registry.set(DEVICE, DeviceStatus::Revoked);
    for device in [&mut a, &mut b] {
        assert!(matches!(device.read(), Ok(Message::Close(_))));
    }
}

#[test]
fn revoke_disconnects_all_live_sessions_without_restarting_server() {
    let (server, registry) = start();
    registry.set(DEVICE, DeviceStatus::Allowed);
    let (_, body) = ota(&server, Some(DEVICE));
    let token = body["websocket"]["token"].as_str().unwrap();
    let mut first = connect(&server, &device_headers(Some(token), 1));
    let mut second = connect(&server, &device_headers(Some(token), 1));
    hello_with(&mut first, 1, false);
    hello_with(&mut second, 1, false);
    assert_eq!(server.status().sessions.len(), 2);
    registry.set(DEVICE, DeviceStatus::Revoked);
    // Re-allowing immediately must NOT reactivate leases issued before revoke.
    registry.set(DEVICE, DeviceStatus::Allowed);
    for device in [&mut first, &mut second] {
        device
            .get_mut()
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        assert!(
            matches!(device.read(), Ok(Message::Close(_))),
            "expected actual close frame, not timeout"
        );
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while !server.status().sessions.is_empty() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(server.status().running);
    assert!(server.status().sessions.is_empty());
}

#[test]
fn ota_registers_pending_device_and_issues_token_after_allow() {
    let (server, registry) = start();
    let (status, body) = ota(&server, Some(DEVICE));
    assert_eq!(status, 200);
    assert_eq!(
        body["websocket"]["url"],
        format!("ws://127.0.0.1:{}/xiaozhi/v1/", server.ws_port())
    );
    assert_eq!(body["websocket"]["token"], "");
    assert_eq!(body["websocket"]["version"], 1);
    assert_eq!(body["firmware"]["version"], "1.9.2");
    assert_eq!(
        body["firmware"]["url"], "",
        "self-reported OTA identity cannot authorize firmware download"
    );
    assert!(body.get("mqtt").is_none() && body.get("activation").is_none());
    assert_eq!(registry.status(DEVICE), Some(DeviceStatus::Pending));

    registry.set(DEVICE, DeviceStatus::Allowed);
    let (_, body) = ota(&server, Some(DEVICE));
    assert_eq!(
        body["firmware"]["url"], "",
        "allowed WS device still has no trusted firmware offer"
    );
    let token = body["websocket"]["token"].as_str().unwrap().to_owned();
    assert!(TokenSigner::new(SECRET.to_vec()).verify(&token, CLIENT, DEVICE, unix_now()));

    let (status, _) = ota(&server, None);
    assert_eq!(status, 400, "缺少 Device-Id 的 OTA 请求被拒绝");
    let text = reqwest::blocking::get(format!(
        "http://127.0.0.1:{}/xiaozhi/ota/",
        server.http_port()
    ))
    .unwrap()
    .text()
    .unwrap();
    assert!(text.contains("OTA"));
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.contains("待允许"))
    );
}

#[test]
fn unauthorized_devices_get_alert_then_disconnect() {
    let (server, registry) = start();
    // 未登记（首次连接即登记为待允许）
    let mut device = connect(&server, &device_headers(None, 1));
    let hello_reply = hello(&mut device, 1);
    assert_eq!(hello_reply["type"], "hello");
    let alert = read_json(&mut device);
    assert_eq!(alert["type"], "alert");
    assert!(alert["message"].as_str().unwrap().contains("允许"));
    expect_closed(&mut device);
    assert_eq!(registry.status(DEVICE), Some(DeviceStatus::Pending));

    // 已允许但 token 错误
    registry.set(DEVICE, DeviceStatus::Allowed);
    let mut device = connect(&server, &device_headers(Some("bogus.1"), 1));
    hello(&mut device, 1);
    let alert = read_json(&mut device);
    assert_eq!(alert["status"], "需要重新授权");
    expect_closed(&mut device);

    // 已撤销：即使 token 正确也拒绝
    let token = TokenSigner::new(SECRET.to_vec()).issue(CLIENT, DEVICE, unix_now());
    registry.set(DEVICE, DeviceStatus::Revoked);
    let mut device = connect(&server, &device_headers(Some(&token), 1));
    hello(&mut device, 1);
    assert_eq!(read_json(&mut device)["status"], "已撤销");
    expect_closed(&mut device);
    assert!(server.status().sessions.is_empty());

    // 缺少 Device-Id
    let mut device = connect(&server, &[("client-id", CLIENT.to_owned())]);
    match device.read().unwrap() {
        Message::Text(text) => assert!(text.as_str().contains("Device-Id")),
        other => panic!("{other:?}"),
    }
}

/// 2026-11-05：未认证的 hello（缺 token、token 错、未允许）不写握手快照；认证通过后才写。
#[test]
fn unauthenticated_hello_does_not_write_device_snapshot() {
    let (server, registry) = start();
    // 待允许设备（首次来访即登记）
    let mut device = connect(&server, &device_headers(None, 1));
    hello(&mut device, 1);
    assert_eq!(read_json(&mut device)["type"], "alert");
    expect_closed(&mut device);
    // 冒用已允许设备的 Device-Id，但拿不出有效 token
    registry.set(DEVICE, DeviceStatus::Allowed);
    for token in [None, Some("bogus.1")] {
        let mut device = connect(&server, &device_headers(token, 1));
        hello(&mut device, 1);
        assert_eq!(read_json(&mut device)["type"], "alert");
        expect_closed(&mut device);
    }
    assert!(
        registry.snapshots(SNAPSHOT_HELLO).is_empty(),
        "未认证连接不能改写设备信息"
    );

    // 认证通过的连接照常写入
    let _device = allowed_device_with(&server, &registry, 1, false);
    let hello = registry.snapshots(SNAPSHOT_HELLO);
    assert_eq!(hello.len(), 1);
    let hello: Value = serde_json::from_str(&hello[0]).unwrap();
    assert_eq!(hello["protocol_version"], 1);
    assert_eq!(hello["mcp"], false);
}

/// 2026-11-05：OTA 不带 token；同 Device-Id、不同 Client-Id 的来访（真实数据库仍返回已允许）
/// 不写设备上报快照，Client-Id 一致的已允许设备才写。
#[test]
fn ota_from_spoofed_client_does_not_write_device_snapshot() {
    let (server, registry) = start();
    registry
        .keep_binding
        .store(true, std::sync::atomic::Ordering::Release);
    // 待允许设备的上报不写
    ota(&server, Some(DEVICE));
    assert!(registry.snapshots(SNAPSHOT_INFO).is_empty());
    registry.set(DEVICE, DeviceStatus::Allowed);
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(format!(
            "http://127.0.0.1:{}/xiaozhi/ota/",
            server.http_port()
        ))
        .header("Device-Id", DEVICE)
        .header("Client-Id", "uuid-spoofer")
        .header("Content-Type", "application/json")
        .body(r#"{"application":{"version":"6.6.6"},"board":{"type":"spoofed"}}"#)
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        registry.status(DEVICE),
        Some(DeviceStatus::Allowed),
        "绑定未被替换"
    );
    assert!(
        registry.snapshots(SNAPSHOT_INFO).is_empty(),
        "冒用 Device-Id 的上报不能改写设备信息"
    );

    ota(&server, Some(DEVICE));
    let info = registry.snapshots(SNAPSHOT_INFO);
    assert_eq!(info.len(), 1);
    assert!(info[0].contains("1.9.2") && !info[0].contains("6.6.6"));
}

fn allowed_device(server: &VoiceServer, registry: &FakeRegistry, version: u32) -> Device {
    allowed_device_with(server, registry, version, true)
}

/// `mcp` 为设备 hello 中是否声明自带 MCP 工具。
fn allowed_device_with(
    server: &VoiceServer,
    registry: &FakeRegistry,
    version: u32,
    mcp: bool,
) -> Device {
    registry.set(DEVICE, DeviceStatus::Allowed);
    let token = TokenSigner::new(SECRET.to_vec()).issue(CLIENT, DEVICE, unix_now());
    let mut device = connect(server, &device_headers(Some(&token), version));
    let reply = hello_with(&mut device, version, mcp);
    assert_eq!(reply["type"], "hello");
    assert_eq!(reply["version"], version);
    assert_eq!(reply["transport"], "websocket");
    assert_eq!(
        reply["audio_params"]["sample_rate"], DOWN,
        "下行声明 24 kHz"
    );
    assert!(reply["session_id"].as_str().unwrap().len() >= 8);
    device
}

#[test]
fn manual_mode_echo_is_decoded_resampled_and_paced() {
    let (server, registry) = start();
    let mut device = allowed_device(&server, &registry, 1);
    assert_eq!(server.status().sessions.len(), 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"manual"}"#,
    );
    // 无法解码的帧被忽略并记录一次，不影响后续音频
    device.send(Message::binary(vec![0xFF])).unwrap();
    let speech = tone(UP, 300.0, 1200, 8000.0);
    send_audio(&mut device, 1, &uplink(&speech));
    send_json(&mut device, r#"{"type":"listen","state":"stop"}"#);
    let (texts, frames, elapsed) = collect_playback(&mut device, 1);
    assert_eq!(texts[0]["type"], "stt");
    assert!((stt_seconds(&texts) - 1.2).abs() < 0.05, "{:?}", texts[0]);
    assert_eq!(texts[1]["state"], "start");
    assert_eq!(texts[2]["state"], "sentence_start");
    // 1.2 秒在 24 kHz 下正好 20 帧（重采样尾部少 1 个样本，补零凑满）
    assert_eq!(frames.len(), 20);
    let played = downlink(&frames);
    assert_eq!(played.len(), 20 * 1440);
    let ratio = rms(&played[2880..26_000]) / rms(&speech[1920..17_000]);
    assert!((0.6..1.4).contains(&ratio), "回放音量与原声相近：{ratio}");
    // 3 帧预缓冲后其余 17 帧按 60 ms 间隔发送
    assert!(
        elapsed >= Duration::from_millis(950),
        "回放过快：{elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(3), "回放过慢：{elapsed:?}");
    send_json(&mut device, r#"{"type":"ping"}"#);
    assert_eq!(read_json(&mut device)["type"], "pong");
    let events = server.status().events;
    assert!(events.iter().any(|event| event.text.contains("无法解码")));
    assert!(
        events
            .iter()
            .any(|event| event.text.contains("播放完成：20 帧，补静音 0 帧")),
        "{events:?}"
    );
}

#[test]
fn auto_mode_vad_ends_utterance_on_silence_with_v2_frames() {
    let (server, registry) = start();
    let mut device = allowed_device(&server, &registry, 2);
    // 自动模式：设备不发 stop，持续上传；说话后静音 200 ms 判停并回放
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut pcm = silence(UP, 1500);
    pcm.extend(tone(UP, 300.0, 900, 8000.0));
    pcm.extend(silence(UP, 900));
    send_audio(&mut device, 2, &uplink(&pcm));
    let (texts, frames, _) = collect_playback(&mut device, 2);
    // 语音前保留 1000 ms（前面的 1500 ms 静音只留最后 1000 ms）+ 900 ms 语音 + 约 200–300 ms 判停静音
    let seconds = stt_seconds(&texts);
    assert!((1.95..=2.35).contains(&seconds), "{:?}", texts[0]);
    let played = downlink(&frames);
    assert!(rms(&played) > 2000.0, "回放有声音");
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.contains("静音判停"))
    );

    // 只有静音时设备停止拾音：没有声音，不播放
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    send_audio(&mut device, 2, &uplink(&silence(UP, 600)));
    send_json(&mut device, r#"{"type":"listen","state":"stop"}"#);
    let (texts, frames, _) = collect_playback(&mut device, 2);
    assert!(texts[0]["text"].as_str().unwrap().contains("没有听到声音"));
    assert!(frames.is_empty());
}

#[test]
fn abort_stops_playback_immediately() {
    let (server, registry) = start();
    let mut device = allowed_device(&server, &registry, 3);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"manual"}"#,
    );
    send_audio(&mut device, 3, &uplink(&tone(UP, 300.0, 3000, 8000.0)));
    send_json(&mut device, r#"{"type":"listen","state":"stop"}"#);
    let mut received = 0;
    loop {
        match device.read().unwrap() {
            Message::Binary(_) => {
                received += 1;
                if received == 5 {
                    send_json(
                        &mut device,
                        r#"{"type":"abort","reason":"wake_word_detected"}"#,
                    );
                }
            }
            Message::Text(text) => {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                if value["type"] == "tts" && value["state"] == "stop" {
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(
        received < 15,
        "打断后应立即停止（3 秒共 50 帧），实际收到 {received} 帧"
    );
    // tts stop 之后不再有音频帧
    device
        .get_mut()
        .set_read_timeout(Some(Duration::from_millis(400)))
        .unwrap();
    match device.read() {
        Ok(Message::Binary(_)) => panic!("打断后仍在发送音频"),
        Ok(other) => panic!("意外消息 {other:?}"),
        Err(_) => {}
    }
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.contains("播放被打断（wake_word_detected）"))
    );
}

#[test]
fn new_listen_start_interrupts_playback_and_records_again() {
    let (server, registry) = start();
    let mut device = allowed_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"manual"}"#,
    );
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 3000, 8000.0)));
    send_json(&mut device, r#"{"type":"listen","state":"stop"}"#);
    // 收到第一帧后再次按下说话键：立即收到 tts stop
    loop {
        if let Message::Binary(_) = device.read().unwrap() {
            break;
        }
    }
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"manual"}"#,
    );
    loop {
        if let Message::Text(text) = device.read().unwrap()
            && text.as_str().contains(r#""state":"stop""#)
        {
            break;
        }
    }
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 600, 8000.0)));
    send_json(&mut device, r#"{"type":"listen","state":"stop"}"#);
    // 跳过打断前已在路上的音频帧，从新一轮的 stt 开始收
    let (texts, frames, _) = loop {
        let (texts, frames, elapsed) = collect_playback(&mut device, 1);
        if texts
            .first()
            .map(|text| text["type"] == "stt")
            .unwrap_or(false)
        {
            break (texts, frames, elapsed);
        }
    };
    assert!((stt_seconds(&texts) - 0.6).abs() < 0.05, "{texts:?}");
    assert_eq!(frames.len(), 10);
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.contains("重新开始拾音"))
    );
}

#[test]
fn stop_closes_sessions_and_frees_ports() {
    let (server, registry) = start();
    let ws_port = server.ws_port();
    let mut device = allowed_device(&server, &registry, 1);
    server.stop();
    expect_closed(&mut device);
    // 监听已关闭：新的连接被拒绝
    let refused = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], ws_port)),
        Duration::from_secs(2),
    );
    assert!(refused.is_err(), "停止后不应再接受连接");
}

#[test]
fn port_in_use_is_reported() {
    let busy = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = busy.local_addr().unwrap().port();
    let config = ServerConfig {
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        ws_port: port,
        http_port: 0,
        ..ServerConfig::default()
    };
    let error = VoiceServer::start(config, Arc::new(FakeRegistry::default()))
        .err()
        .unwrap();
    assert!(error.contains("已被占用"), "{error}");
}

mod chat;

/// NV17 固件 OTA（2026-10-15）：只给已允许的设备下发用户推送的固件地址；令牌错误下载 404；设备上报版本交给宿主。
#[test]
fn firmware_offer_is_sent_only_to_allowed_device_and_download_uses_token() {
    struct FakeFirmware {
        reported: std::sync::Mutex<Vec<(String, String)>>,
        fetched: std::sync::Mutex<Vec<String>>,
    }
    impl crate::firmware::FirmwareHost for FakeFirmware {
        fn offer(&self, device: &str, current: &str) -> Option<(String, String)> {
            (device == DEVICE && crate::firmware::is_newer("1.9.3", current))
                .then(|| ("1.9.3".into(), "a".repeat(32)))
        }
        fn fetch(&self, token: &str, peer: &str) -> Option<Vec<u8>> {
            self.fetched.lock().unwrap().push(peer.into());
            (token == "a".repeat(32)).then(|| vec![0xE9; 2048])
        }
        fn reported(&self, device: &str, version: &str) {
            self.reported
                .lock()
                .unwrap()
                .push((device.into(), version.into()));
        }
    }
    let host = Arc::new(FakeFirmware {
        reported: Default::default(),
        fetched: Default::default(),
    });
    let registry = Arc::new(FakeRegistry::default());
    let config = ServerConfig {
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        ws_port: 0,
        http_port: 0,
        firmware: Some(host.clone()),
        ..ServerConfig::default()
    };
    let server = VoiceServer::start(config, registry.clone()).unwrap();

    let (_, body) = ota(&server, Some(DEVICE));
    assert_eq!(body["firmware"]["url"], "", "未允许的设备拿不到固件");
    assert!(
        host.reported.lock().unwrap().is_empty(),
        "未允许的设备上报版本不记录"
    );

    registry.set(DEVICE, DeviceStatus::Allowed);
    let (_, body) = ota(&server, Some(DEVICE));
    assert_eq!(body["firmware"]["version"], "1.9.3");
    let url = body["firmware"]["url"].as_str().unwrap().to_owned();
    assert_eq!(
        url,
        format!(
            "http://127.0.0.1:{}/xiaozhi/firmware/{}.bin",
            server.http_port(),
            "a".repeat(32)
        )
    );
    assert_eq!(
        *host.reported.lock().unwrap(),
        vec![(DEVICE.to_owned(), "1.9.2".to_owned())]
    );

    let response = reqwest::blocking::get(&url).unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.bytes().unwrap().len(), 2048);
    assert_eq!(
        host.fetched.lock().unwrap()[0],
        "127.0.0.1",
        "宿主拿到下载方地址用于核对设备"
    );
    let wrong = reqwest::blocking::get(format!(
        "http://127.0.0.1:{}/xiaozhi/firmware/{}.bin",
        server.http_port(),
        "b".repeat(32)
    ))
    .unwrap();
    assert_eq!(wrong.status().as_u16(), 404);
    let bad = reqwest::blocking::get(format!(
        "http://127.0.0.1:{}/xiaozhi/firmware/..%2Fsecret",
        server.http_port()
    ))
    .unwrap();
    assert_eq!(bad.status().as_u16(), 404);
}

#[test]
fn device_request_preserves_reconnected_session_rejects_non_mcp_and_works_in_echo_mode() {
    let (server, registry) = start();

    // 1. 设备未声明 MCP 时，device_request 同步报错，绝不返回假成功
    let mut no_mcp = allowed_device_with(&server, &registry, 1, false);
    let err = server
        .device_request(DEVICE, crate::DeviceRequest::RefreshInfo)
        .unwrap_err();
    assert!(err.contains("未启用设备端 MCP"), "{err}");
    let _ = no_mcp.close(None);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !server.status().sessions.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }

    // 2. 同一设备重叠重连：旧连接断开时绝不抹掉新连接的请求句柄；回声模式也支持按需握手刷新状态
    let mut old_dev = allowed_device_with(&server, &registry, 1, true);
    let mut new_dev = allowed_device_with(&server, &registry, 1, true);
    let _ = old_dev.close(None);
    while server.status().sessions.len() > 1 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    server
        .device_request(DEVICE, crate::DeviceRequest::RefreshInfo)
        .unwrap();
    // 连续第 2 次请求在设备回复 initialize 前到达：仍须进入延迟队列，不得抢跑发 tools/call 或重复发 initialize
    server
        .device_request(DEVICE, crate::DeviceRequest::RefreshInfo)
        .unwrap();
    let init = read_json(&mut new_dev);
    assert_eq!(init["type"], "mcp");
    assert_eq!(init["payload"]["method"], "initialize");
    let init_id = init["payload"]["id"].clone();
    std::thread::sleep(Duration::from_millis(50));
    send_json(
        &mut new_dev,
        &serde_json::json!({
            "type": "mcp",
            "payload": {"jsonrpc": "2.0", "id": init_id, "result": {"protocolVersion": "2024-11-05", "capabilities": {}}}
        })
        .to_string(),
    );
    let initialized = read_json(&mut new_dev);
    assert_eq!(
        initialized["payload"]["method"],
        "notifications/initialized"
    );
    let call_1 = read_json(&mut new_dev);
    assert_eq!(call_1["payload"]["method"], "tools/call");
    assert_eq!(
        call_1["payload"]["params"]["name"],
        "self.get_device_status"
    );
    let call_2 = read_json(&mut new_dev);
    assert_eq!(call_2["payload"]["method"], "tools/call");
    assert_eq!(
        call_2["payload"]["params"]["name"],
        "self.get_device_status"
    );
}

/// n68：宿主持有的诊断记录本在服务停止、再次启动后仍保留上次的事件（含错误），并显示被覆盖计数。
#[test]
fn diagnostics_survive_server_stop_and_restart() {
    let registry = Arc::new(FakeRegistry::default());
    let log = crate::DiagnosticsLog::new();
    let config = || ServerConfig {
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        ws_port: 0,
        http_port: 0,
        diagnostics: log.clone(),
        ..ServerConfig::default()
    };
    let server = VoiceServer::start(config(), registry.clone()).unwrap();
    let (status, _) = ota(&server, None);
    assert_eq!(status, 400);
    let first = server.status().events;
    let rejected = first
        .iter()
        .find(|event| event.text.contains("OTA 请求被拒绝"))
        .expect("OTA 拒绝记为警告");
    assert_eq!(rejected.level, crate::EventLevel::Warn);
    assert_eq!(rejected.category, crate::DiagCategory::Firmware);
    assert!(rejected.hint.is_some());
    server.stop();
    assert!(
        log.events()
            .iter()
            .any(|event| event.text.contains("OTA 请求被拒绝")),
        "停止后记录仍在"
    );
    let restarted = VoiceServer::start(config(), registry).unwrap();
    let events = restarted.status().events;
    assert!(
        events
            .iter()
            .any(|event| event.text.contains("OTA 请求被拒绝")),
        "重启后仍能看到上次的问题"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.text.starts_with("内置语音服务已启动"))
            .count(),
        2
    );
    assert_eq!(restarted.status().events_dropped.info, 0);
    restarted.stop();
}

/// 2026-11-08：桌面端应用内更新在 tokio 异步任务里收尾并停止内置语音服务。此前 `shutdown_timeout`
/// 在异步上下文直接 panic，更新任务被吞掉、界面卡在“安装完成，MOSS 正在重启…”。
#[test]
fn server_stops_cleanly_from_async_task() {
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            ..ServerConfig::default()
        },
        Arc::new(FakeRegistry::default()),
    )
    .unwrap();
    let outer = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    outer
        .block_on(outer.spawn(async move { server.stop() }))
        .expect("在异步任务里停止语音服务不应 panic");
}
