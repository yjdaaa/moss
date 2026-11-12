//! n71 常连模式（2026-11-08 用户决定：“那你先用这个模式做吧”）：MOSS 内置的精简 MQTT 服务端 + UDP 语音。
//!
//! 官方固件（`78/xiaozhi-esp32`）收到的 OTA 回复里有 `mqtt` 段时改用 MQTT：开机联网就连上控制通道并自动重连，
//! 不说话时也处理服务端发来的 MCP、表情、提示等消息；唤醒后经 MQTT 发 hello 申请 UDP 语音通道，
//! MOSS 回 UDP 地址与本轮随机 AES 密钥 / nonce。设备不订阅任何主题，MOSS 直接把下行消息写到它的连接上。
//! 认证沿用 WebSocket 的设备 token：OTA 只给已允许的设备下发 MQTT 用户名（设备编号）与密码（token）。

pub(crate) mod codec;
pub(crate) mod udp;

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::state::{DiagCategory, EventRecord};
use crate::{DeviceStatus, DeviceVisit, Shared, unix_now};
use codec::Packet;
use udp::{UdpChannel, UdpHub};

/// 设备上行 JSON 的发布主题（OTA 下发给设备，MOSS 不校验主题名）。
pub const PUBLISH_TOPIC: &str = "device-server";
/// 设备 MQTT 心跳间隔（秒），与官方默认一致。
pub const KEEPALIVE_SECS: u16 = 240;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 连接层交给会话的一条设备消息。
pub(crate) enum LinkEvent {
    Text(String),
    Audio(Vec<u8>),
}

/// 会话看到的常连模式设备连接。
pub(crate) struct MqttLink {
    incoming: UnboundedReceiver<LinkEvent>,
    outgoing: UnboundedSender<Vec<u8>>,
    audio: Arc<Mutex<Option<Arc<UdpChannel>>>>,
    hub: Arc<UdpHub>,
    topic: String,
}

impl MqttLink {
    pub(crate) async fn recv(&mut self) -> Option<axum::extract::ws::Message> {
        use axum::extract::ws::Message;
        self.incoming.recv().await.map(|event| match event {
            LinkEvent::Text(text) => Message::Text(text.into()),
            LinkEvent::Audio(opus) => Message::Binary(opus.into()),
        })
    }

    pub(crate) fn send_text(&self, text: &str) -> bool {
        self.outgoing
            .send(codec::publish(&self.topic, text.as_bytes()))
            .is_ok()
    }

    pub(crate) async fn send_audio(&self, opus: &[u8]) {
        let channel = self.audio.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(channel) = channel {
            self.hub.send(&channel, opus).await;
        }
    }

    pub(crate) fn audio_open(&self) -> bool {
        self.audio
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }

    pub(crate) fn close_audio(&self, session_id: &str, notify: bool) {
        let channel = self.audio.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(channel) = channel {
            self.hub.close(&channel);
        }
        if notify {
            let goodbye = json!({ "type": "goodbye", "session_id": session_id });
            self.send_text(&goodbye.to_string());
        }
    }
}

/// 接受设备的 MQTT 连接。
pub(crate) async fn serve(
    shared: Arc<Shared>,
    listener: TcpListener,
    hub: Arc<UdpHub>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        let accepted = tokio::select! {
            _ = shutdown.wait_for(|stop| *stop) => return,
            accepted = listener.accept() => accepted,
        };
        let Ok((stream, peer)) = accepted else {
            continue;
        };
        let _ = stream.set_nodelay(true);
        tokio::spawn(handle(shared.clone(), stream, peer, hub.clone()));
    }
}

fn reject_event(shared: &Shared, peer: SocketAddr, text: String, hint: &str) {
    shared.state.event(
        EventRecord::warn(
            DiagCategory::Device,
            format!("常连模式拒绝连接（{}）：{text}", peer.ip()),
        )
        .hint(hint),
    );
}

async fn handle(shared: Arc<Shared>, stream: TcpStream, peer: SocketAddr, hub: Arc<UdpHub>) {
    let local_ip = stream
        .local_addr()
        .map(|a| a.ip())
        .unwrap_or(IpAddr::from([127, 0, 0, 1]));
    let (mut reader, mut writer) = stream.into_split();
    let connect = match tokio::time::timeout(CONNECT_TIMEOUT, codec::read_packet(&mut reader)).await
    {
        Ok(Ok(Some(Packet::Connect(connect)))) => connect,
        _ => return,
    };
    let device_id = connect.username.clone().unwrap_or_default();
    let client_id = connect.client_id.clone();
    let token = connect.password.clone().unwrap_or_default();
    if device_id.is_empty() {
        let _ = writer.write_all(&codec::connack(4)).await;
        reject_event(
            &shared,
            peer,
            "没有带设备编号".into(),
            "设备的常连设置不完整：重启设备，让它重新从 MOSS 获取设置",
        );
        return;
    }
    let visit = DeviceVisit {
        device_id: device_id.clone(),
        client_id: client_id.clone(),
        model: String::new(),
        firmware_version: String::new(),
        ip: peer.ip().to_string(),
    };
    let registry = shared.registry.clone();
    let status = tokio::task::spawn_blocking(move || registry.seen(&visit))
        .await
        .ok()
        .and_then(Result::ok);
    let token_ok = status == Some(DeviceStatus::Allowed)
        && shared
            .signer
            .verify(&token, &client_id, &device_id, unix_now());
    if !token_ok {
        let _ = writer.write_all(&codec::connack(5)).await;
        reject_event(
            &shared,
            peer,
            format!("设备 {device_id} 未允许或授权已失效"),
            "在语音中心允许这台设备后重启它；授权超过 30 天也要重启一次，让它从 MOSS 拿新的授权",
        );
        return;
    }
    let Ok(session_id) = crate::session::random_id() else {
        let _ = writer.write_all(&codec::connack(3)).await;
        reject_event(
            &shared,
            peer,
            format!("设备 {device_id} 会话建立失败：操作系统安全随机源不可用"),
            "重启 MOSS 或操作系统后重试",
        );
        return;
    };
    let (registry, d, c, s) = (
        shared.registry.clone(),
        device_id.clone(),
        client_id.clone(),
        session_id.clone(),
    );
    let lease = match tokio::task::spawn_blocking(move || registry.open_session(&d, &c, &s)).await {
        Ok(Ok(allowed)) => crate::SessionLease {
            allowed,
            registry: shared.registry.clone(),
            session: session_id.clone(),
        },
        _ => {
            let _ = writer.write_all(&codec::connack(5)).await;
            return;
        }
    };
    if writer.write_all(&codec::connack(0)).await.is_err() {
        return;
    }

    let (out_tx, mut out_rx) = unbounded_channel::<Vec<u8>>();
    let writer_task = tokio::spawn(async move {
        while let Some(bytes) = out_rx.recv().await {
            if writer.write_all(&bytes).await.is_err() {
                break;
            }
        }
        let _ = writer.shutdown().await;
    });
    let (in_tx, in_rx) = unbounded_channel::<LinkEvent>();
    let audio: Arc<Mutex<Option<Arc<UdpChannel>>>> = Arc::new(Mutex::new(None));
    let reader_task = tokio::spawn(read_loop(ReaderCtx {
        shared: shared.clone(),
        hub: hub.clone(),
        audio: audio.clone(),
        incoming: in_tx,
        outgoing: out_tx.clone(),
        device_id: device_id.clone(),
        session_id: session_id.clone(),
        topic: format!("devices/p2p/{client_id}"),
        device_ip: peer.ip(),
        local_ip,
        keep_alive: connect.keep_alive,
        reader,
    }));
    let link = MqttLink {
        incoming: in_rx,
        outgoing: out_tx,
        audio: audio.clone(),
        hub: hub.clone(),
        topic: format!("devices/p2p/{client_id}"),
    };
    crate::session::serve_session(
        shared.clone(),
        crate::link::Link::Mqtt(link),
        crate::session::SessionStart {
            device_id,
            client_id,
            session_id,
            ip: peer.ip().to_string(),
            version: 1,
            uplink_rate: 16_000,
            device_mcp: true,
            vision_host: Some(local_ip.to_string()),
            vision_token: Some(token),
            transport: "mqtt",
            lease,
            hello: None,
            text_font_cap: None,
        },
    )
    .await;
    reader_task.abort();
    if let Some(channel) = audio.lock().unwrap_or_else(|p| p.into_inner()).take() {
        hub.close(&channel);
    }
    writer_task.abort();
}

struct ReaderCtx {
    shared: Arc<Shared>,
    hub: Arc<UdpHub>,
    audio: Arc<Mutex<Option<Arc<UdpChannel>>>>,
    incoming: UnboundedSender<LinkEvent>,
    outgoing: UnboundedSender<Vec<u8>>,
    device_id: String,
    session_id: String,
    topic: String,
    device_ip: IpAddr,
    local_ip: IpAddr,
    keep_alive: u16,
    reader: tokio::net::tcp::OwnedReadHalf,
}

/// 设备 → MOSS：应答 QoS、心跳与订阅；hello 时开 UDP 通道并回地址与密钥；其余 JSON 交给会话。
async fn read_loop(mut ctx: ReaderCtx) {
    // 规范要求 1.5 倍心跳内没有任何报文即可断开
    let idle = (ctx.keep_alive > 0).then(|| Duration::from_secs(u64::from(ctx.keep_alive) * 3 / 2));
    loop {
        let next = match idle {
            Some(limit) => {
                match tokio::time::timeout(limit, codec::read_packet(&mut ctx.reader)).await {
                    Ok(next) => next,
                    Err(_) => break,
                }
            }
            None => codec::read_packet(&mut ctx.reader).await,
        };
        let packet = match next {
            Ok(Some(packet)) => packet,
            _ => break,
        };
        match packet {
            Packet::Publish {
                qos,
                packet_id,
                payload,
                ..
            } => {
                match (qos, packet_id) {
                    (1, Some(id)) => {
                        let _ = ctx.outgoing.send(codec::puback(id));
                    }
                    (2, Some(id)) => {
                        let _ = ctx.outgoing.send(codec::pubrec(id));
                    }
                    _ => {}
                }
                let Ok(text) = String::from_utf8(payload) else {
                    continue;
                };
                let kind = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| v["type"].as_str().map(str::to_owned));
                match kind.as_deref() {
                    Some("hello") => on_hello(&ctx, &text),
                    Some("goodbye") => {
                        if let Some(channel) =
                            ctx.audio.lock().unwrap_or_else(|p| p.into_inner()).take()
                        {
                            ctx.hub.close(&channel);
                        }
                    }
                    _ => {}
                }
                if ctx.incoming.send(LinkEvent::Text(text)).is_err() {
                    break;
                }
            }
            Packet::PubRel(id) => {
                let _ = ctx.outgoing.send(codec::pubcomp(id));
            }
            Packet::Subscribe { packet_id, topics } => {
                let _ = ctx.outgoing.send(codec::suback(packet_id, topics));
            }
            Packet::Unsubscribe(id) => {
                let _ = ctx.outgoing.send(codec::unsuback(id));
            }
            Packet::PingReq => {
                let _ = ctx.outgoing.send(codec::pingresp());
            }
            Packet::Disconnect | Packet::Connect(_) => break,
            Packet::Other(_) => {}
        }
    }
    if let Some(channel) = ctx.audio.lock().unwrap_or_else(|p| p.into_inner()).take() {
        ctx.hub.close(&channel);
    }
}

/// 设备唤醒后申请语音通道：新开 UDP 通道（旧的作废），回 UDP 地址与本轮随机密钥。
fn on_hello(ctx: &ReaderCtx, text: &str) {
    let hello: Value = serde_json::from_str(text).unwrap_or_default();
    let channel = match ctx.hub.open(ctx.device_ip, ctx.incoming.clone()) {
        Ok(channel) => channel,
        Err(reason) => {
            if let Some(previous) = ctx.audio.lock().unwrap_or_else(|p| p.into_inner()).take() {
                ctx.hub.close(&previous);
            }
            ctx.shared.state.event(
                EventRecord::error(
                    DiagCategory::Audio,
                    format!("设备 {} 打开 UDP 语音通道失败：{reason}", ctx.device_id),
                )
                .hint("拒绝下发全零或重复密钥；请重试唤醒或重启 MOSS"),
            );
            return;
        }
    };
    let previous = ctx
        .audio
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .replace(channel.clone());
    if let Some(previous) = previous {
        ctx.hub.close(&previous);
    }
    let reply = json!({
        "type": "hello",
        "transport": "udp",
        "session_id": ctx.session_id,
        "audio_params": {
            "format": "opus",
            "sample_rate": ctx.shared.config.downlink_sample_rate,
            "channels": 1,
            "frame_duration": 60,
        },
        "udp": {
            "server": ctx.local_ip.to_string(),
            "port": ctx.hub.local_port(),
            "key": channel.key_hex(),
            "nonce": channel.nonce_hex(),
        },
    });
    let _ = ctx
        .outgoing
        .send(codec::publish(&ctx.topic, reply.to_string().as_bytes()));
    // 握手快照：桌面端「设备信息」据此显示连接方式与设备端 AEC
    let snapshot = json!({
        "protocol_version": hello["version"],
        "transport": "mqtt+udp",
        "audio": hello["audio_params"],
        "mcp": hello["features"]["mcp"].as_bool().unwrap_or(true),
        "aec": hello["features"]["aec"].as_bool().unwrap_or(false),
        "glyph_push": hello["features"]["glyph_push"].as_bool().unwrap_or(false),
        "text_font": hello.get("text_font").cloned().unwrap_or(Value::Null),
    })
    .to_string();
    let (registry, device) = (ctx.shared.registry.clone(), ctx.device_id.clone());
    tokio::task::spawn_blocking(move || {
        registry.device_snapshot(&device, crate::SNAPSHOT_HELLO, &snapshot);
    });
}
