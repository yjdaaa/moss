//! 连接建立：WebSocket 升级、hello 握手、授权检查与会话登记收尾。

use super::*;

pub fn router(shared: Arc<Shared>) -> Router {
    Router::new().fallback(upgrade).with_state(shared)
}

struct Identity {
    device_id: Option<String>,
    client_id: Option<String>,
    authorization: Option<String>,
    protocol_version: Option<u32>,
    host: Option<String>,
    ip: String,
}

async fn upgrade(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    let pick = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                query
                    .get(name)
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
            })
    };
    let identity = Identity {
        device_id: pick("device-id"),
        client_id: pick("client-id"),
        authorization: pick("authorization"),
        protocol_version: pick("protocol-version").and_then(|value| value.parse().ok()),
        host: headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .and_then(crate::ota::host_without_port),
        ip: peer.ip().to_string(),
    };
    ws.max_message_size(64 * 1024)
        .on_upgrade(move |socket| run(shared, socket, identity))
}

pub(crate) fn random_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub(super) async fn send_json(socket: &mut crate::link::Link, message: &ServerMessage) -> bool {
    socket
        .send(Message::Text(message.to_text().into()))
        .await
        .is_ok()
}

async fn send_json_ws(socket: &mut WebSocket, message: &ServerMessage) -> bool {
    socket
        .send(Message::Text(message.to_text().into()))
        .await
        .is_ok()
}

/// 等设备 hello，返回其声明的协议版本、上行音频参数、是否支持设备 MCP / AEC 与字库能力。
async fn wait_hello(
    socket: &mut WebSocket,
) -> Option<(u32, AudioParams, bool, bool, Option<TextFontCapability>)> {
    let deadline = Instant::now() + HELLO_TIMEOUT;
    loop {
        let message = tokio::time::timeout_at(deadline, socket.recv())
            .await
            .ok()??
            .ok()?;
        match message {
            Message::Text(text) => {
                if let Ok(ClientMessage::Hello {
                    version,
                    audio_params,
                    features,
                    text_font,
                    ..
                }) = ClientMessage::parse(text.as_str())
                {
                    let font_cap = features.glyph_push.then_some(text_font).flatten();
                    return Some((version, audio_params, features.mcp, features.aec, font_cap));
                }
            }
            Message::Close(_) => return None,
            _ => {}
        }
    }
}

async fn run(shared: Arc<Shared>, mut socket: WebSocket, identity: Identity) {
    let Some(device_id) = identity.device_id.clone() else {
        shared
            .state
            .event(
                EventRecord::warn(DiagCategory::Device, format!("拒绝连接（{}）：设备没有带设备编号（Device-Id）", identity.ip))
                    .hint("这台设备的固件没按小智协议上报设备编号：换用标准小智固件，或检查自编固件的连接请求头"),
            );
        let _ = socket
            .send(Message::Text("缺少 Device-Id，无法识别设备".into()))
            .await;
        let _ = socket.send(Message::Close(None)).await;
        return;
    };
    let client_id = identity.client_id.clone().unwrap_or_default();
    let visit = DeviceVisit {
        device_id: device_id.clone(),
        client_id: client_id.clone(),
        model: String::new(),
        firmware_version: String::new(),
        ip: identity.ip.clone(),
    };
    // 登记走本地 SQLite（同步锁）：和 open_session 一样放到阻塞线程，不占 tokio 工作线程
    let registry = shared.registry.clone();
    let seen = tokio::task::spawn_blocking(move || registry.seen(&visit))
        .await
        .unwrap_or_else(|error| Err(format!("登记任务失败：{error}")));
    let status = match seen {
        Ok(status) => status,
        Err(error) => {
            shared.state.event(
                EventRecord::error(
                    DiagCategory::Internal,
                    format!("登记设备 {device_id} 失败：本机设备记录读写出错"),
                )
                .hint("重启 MOSS 再试；一直失败请导出诊断包反馈")
                .detail(&error),
            );
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    let token_ok = status == DeviceStatus::Allowed
        && identity
            .authorization
            .as_deref()
            .map(|token| {
                shared
                    .signer
                    .verify(token, &client_id, &device_id, unix_now())
            })
            .unwrap_or(false);
    let Some((hello_version, uplink, device_mcp, device_aec, text_font_cap)) =
        wait_hello(&mut socket).await
    else {
        shared.state.event(
            EventRecord::warn(
                DiagCategory::Device,
                format!("设备 {device_id} 连上后 10 秒内没有完成握手（hello），已断开"),
            )
            .hint("多半是设备网络太差或固件版本太旧：靠近路由器、重启设备，或升级小智固件"),
        );
        return;
    };
    let version = if matches!(hello_version, 1..=3) {
        hello_version
    } else {
        identity.protocol_version.unwrap_or(1)
    };

    // 2026-11-02：连接握手快照（协议版本、音频参数、是否带 MCP、设备端 AEC）。
    // 桌面端「设备信息」用它显示 AEC 是否真的开启，而不是按板型推断。
    // 2026-11-05：只有 token 校验通过且 open_session 按当前授权（已允许 + Client-Id 一致）登记成功后才写入，
    // 冒用已允许设备 Device-Id 的未认证连接不能改写设备信息。
    let hello_snapshot = json!({
        "protocol_version": version,
        "transport": "websocket",
        "audio": {
            "format": uplink.format,
            "sample_rate": uplink.sample_rate,
            "channels": uplink.channels,
            "frame_duration": uplink.frame_duration,
        },
        "mcp": device_mcp,
        "aec": device_aec,
        "glyph_push": text_font_cap.is_some(),
        "text_font": text_font_cap,
    });
    let Ok(session_id) = random_id() else {
        shared.state.event(
            EventRecord::error(
                DiagCategory::Internal,
                format!("设备 {device_id} 会话建立失败：操作系统安全随机源不可用"),
            )
            .hint("拒绝使用不安全的零会话编号，请重启 MOSS 或操作系统后重试"),
        );
        return;
    };
    let hello = ServerMessage::Hello {
        version,
        transport: "websocket".into(),
        session_id: session_id.clone(),
        audio_params: AudioParams::opus(shared.config.downlink_sample_rate, FRAME_MS as u32),
    };
    if !token_ok {
        // 未授权：回 hello 后提示原因并断开
        if !send_json_ws(&mut socket, &hello).await {
            return;
        }
        let (status_text, message, log) = match status {
            DeviceStatus::Pending => (
                "等待允许",
                "请在电脑 MOSS 语音中心允许此设备，然后重启设备",
                "待允许，已提示用户在电脑上允许",
            ),
            DeviceStatus::Revoked => (
                "已撤销",
                "此设备已被撤销，请在电脑 MOSS 语音中心重新允许",
                "已撤销，拒绝连接",
            ),
            DeviceStatus::Allowed => (
                "需要重新授权",
                "请重启设备，从 MOSS 重新获取授权",
                "已允许但 token 缺失或无效（允许后尚未重启设备？），已提示重启",
            ),
        };
        let record = if status == DeviceStatus::Pending {
            EventRecord::info(DiagCategory::Device, format!("设备 {device_id}：{log}"))
                .hint(message)
        } else {
            EventRecord::warn(DiagCategory::Device, format!("设备 {device_id}：{log}"))
                .hint(message)
        };
        shared.state.event(record);
        let alert = ServerMessage::Alert {
            status: status_text.into(),
            message: message.into(),
            emotion: "sad".into(),
        };
        send_json_ws(&mut socket, &alert).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = socket.send(Message::Close(None)).await;
        return;
    }
    let (registry, d, c, s) = (
        shared.registry.clone(),
        device_id.clone(),
        client_id.clone(),
        session_id.clone(),
    );
    let snapshot = hello_snapshot.to_string();
    let opened = tokio::task::spawn_blocking(move || {
        let allowed = registry.open_session(&d, &c, &s)?;
        registry.device_snapshot(&d, crate::SNAPSHOT_HELLO, &snapshot);
        Ok::<_, String>(allowed)
    });
    let lease = match opened.await {
        Ok(Ok(allowed)) => crate::SessionLease {
            allowed,
            registry: shared.registry.clone(),
            session: session_id.clone(),
        },
        _ => {
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    let uplink_rate = if uplink.format == "opus" && supported_rate(uplink.sample_rate) {
        uplink.sample_rate
    } else {
        shared.state.event(
            EventRecord::warn(
                DiagCategory::Audio,
                format!(
                    "设备 {device_id} 声明的上行音频为 {} {} Hz，不支持，按 Opus 16000 Hz 处理",
                    uplink.format, uplink.sample_rate
                ),
            )
            .hint("如果听不清或识别全错，换用标准小智固件"),
        );
        16_000
    };
    let vision_token = identity.authorization.clone().map(|value| {
        value
            .trim()
            .strip_prefix("Bearer ")
            .unwrap_or(value.trim())
            .to_owned()
    });
    serve_session(
        shared,
        crate::link::Link::Ws(socket),
        SessionStart {
            device_id,
            client_id,
            session_id,
            ip: identity.ip.clone(),
            version,
            uplink_rate,
            device_mcp,
            vision_host: identity.host.clone(),
            vision_token,
            transport: "websocket",
            lease,
            hello: Some(hello),
            text_font_cap,
        },
    )
    .await;
}

/// 建好会话所需的连接信息（WebSocket 在 hello 后、常连模式在 MQTT 登录后）。
pub(crate) struct SessionStart {
    pub device_id: String,
    pub client_id: String,
    pub session_id: String,
    pub ip: String,
    pub version: u32,
    pub uplink_rate: u32,
    pub device_mcp: bool,
    pub vision_host: Option<String>,
    pub vision_token: Option<String>,
    /// `websocket` / `mqtt`
    pub transport: &'static str,
    pub lease: crate::SessionLease,
    /// WebSocket：登记好会话后回给设备的 hello；常连模式在设备唤醒时由连接层回 hello
    pub hello: Option<ServerMessage>,
    /// n81 设备在 `hello` 中声明的字库与 `glyph_push` 能力
    pub text_font_cap: Option<TextFontCapability>,
}

/// 已授权设备的会话：登记、运行到断开，再做收尾（记忆摘要、清理登记）。
pub(crate) async fn serve_session(
    shared: Arc<Shared>,
    mut socket: crate::link::Link,
    start: SessionStart,
) {
    let SessionStart {
        device_id,
        client_id,
        session_id,
        ip,
        version,
        uplink_rate,
        device_mcp,
        vision_host,
        vision_token,
        transport,
        lease,
        hello,
        text_font_cap,
    } = start;
    let mqtt = socket.is_mqtt();
    // 已授权的设备先建好会话（音频编解码器）并登记，再回 hello：设备收到 hello 时语音中心已能看到它
    let (mut session, mut requests_rx) = match Session::new(
        &shared,
        &session_id,
        &device_id,
        &client_id,
        version,
        uplink_rate,
        lease.allowed.clone(),
    ) {
        Ok(mut session) => {
            session.mqtt = mqtt;
            let init_font_size = text_font_cap.as_ref().map(|f| f.size);
            let init_font_bpp = text_font_cap.as_ref().map(|f| f.bpp);
            session.screen.text_font_cap = text_font_cap;
            if device_mcp {
                session.device_mcp = Some(DeviceMcp::new());
                session.vision_host = vision_host;
                if session.conversation.is_some() {
                    session.vision_token = vision_token;
                }
            }
            let (requests_tx, requests_rx) = tokio::sync::mpsc::unbounded_channel();
            if let Ok(mut map) = shared.device_requests.lock() {
                map.insert(
                    device_id.clone(),
                    crate::DeviceRequestHandle {
                        session_id: session_id.clone(),
                        supports_mcp: session.device_mcp.is_some(),
                        sender: requests_tx,
                    },
                );
            }
            shared.state.open(SessionView {
                session_id: session_id.clone(),
                device_id: device_id.clone(),
                ip: ip.clone(),
                protocol_version: version,
                connected_at: unix_now(),
                state: "idle".into(),
                level: 0,
                emotion: String::new(),
                active_tool: None,
                utterance_chars: 0,
                spoken_sentences: 0,
                device_tools: Vec::new(),
                screen_monochrome: None,
                screen_width: None,
                screen_height: None,
                screen_pack: false,
                transport: transport.into(),
                text_font_size: init_font_size,
                text_font_bpp: init_font_bpp,
            });
            shared.state.event(if mqtt {
                format!("设备 {device_id} 已常连在线（MQTT，唤醒后语音走加密 UDP）")
            } else {
                format!(
                    "设备 {device_id} 已连接（帧协议 v{version}，上行 {uplink_rate} Hz，下行 {} Hz）",
                    shared.config.downlink_sample_rate
                )
            });
            (session, requests_rx)
        }
        Err(error) => {
            shared.state.event(
                EventRecord::error(
                    DiagCategory::Audio,
                    format!("设备 {device_id} 的音频初始化失败，已断开"),
                )
                .hint("设备声明的音频格式 MOSS 处理不了：换用标准小智固件后重试")
                .detail(&error),
            );
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    shared
        .registry
        .sessions_changed(shared.active_device_sessions().len());
    let (phase, level) = shared.active_aggregate_pulse();
    shared.registry.pulse_changed(phase.as_deref(), level);
    if let Some(hello) = hello
        && !send_json(&mut socket, &hello).await
    {
        if let Ok(mut map) = shared.device_requests.lock()
            && map
                .get(&device_id)
                .is_some_and(|handle| handle.session_id == session_id)
        {
            map.remove(&device_id);
        }
        shared.state.close(&session_id);
        shared
            .registry
            .sessions_changed(shared.active_device_sessions().len());
        let (phase, level) = shared.active_aggregate_pulse();
        shared.registry.pulse_changed(phase.as_deref(), level);
        return;
    }
    let permission = lease.allowed.clone();
    let reason = tokio::select! {
        biased;
        _ = async {
            while permission.load(std::sync::atomic::Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        } => "设备授权已撤销".to_owned(),
        reason = session.run(&mut socket, &mut requests_rx) => reason,
    };
    // NV10: background summary uses only committed, bounded history and never delays disconnect.
    if permission.load(std::sync::atomic::Ordering::Acquire) {
        session.conversation_closed();
    } else {
        shared.camera_preview.clear_device(&device_id);
        let _ = shared.vision_photos.take(&device_id);
    }
    let mut was_active_device_handle = false;
    if let Ok(mut map) = shared.device_requests.lock()
        && map
            .get(&device_id)
            .is_some_and(|handle| handle.session_id == session_id)
    {
        map.remove(&device_id);
        was_active_device_handle = true;
    }
    if was_active_device_handle && permission.load(std::sync::atomic::Ordering::Acquire) {
        shared
            .camera_preview
            .fail_if_pending(&device_id, "设备连接已断开，画面未回传".into());
    }
    session.finish_timing("disconnected");
    session.last_verified_person = None;
    session.carried_photo = None;
    session.listen.paused = None;
    session.turn = None;
    session.pump.clear();
    drop(lease);
    shared.state.close(&session_id);
    shared
        .registry
        .sessions_changed(shared.active_device_sessions().len());
    let (phase, level) = shared.active_aggregate_pulse();
    shared.registry.pulse_changed(phase.as_deref(), level);
    shared.state.event(disconnect_event(&device_id, &reason));
    let _ = socket.send(Message::Close(None)).await;
}

/// 断开原因 → 大白话事件：正常结束记为日常记录，异常断线记为警告并说明最可能的原因。
fn disconnect_event(device_id: &str, reason: &str) -> EventRecord {
    let text = format!("设备 {device_id} 已断开：{reason}");
    let likely = "最可能是 Wi-Fi 掉线、设备重启或断电";
    match reason {
        "连接出错" => EventRecord::warn(
            DiagCategory::Device,
            format!("设备 {device_id} 已断开：连接突然中断（{likely}）"),
        )
        .hint("检查设备供电和 Wi-Fi 信号；设备会自动重连，频繁出现就把设备挪近路由器")
        .detail(reason),
        "发送失败" => EventRecord::warn(
            DiagCategory::Device,
            format!("设备 {device_id} 已断开：向设备发数据失败，连接已经断了（{likely}）"),
        )
        .hint("检查设备供电和 Wi-Fi 信号；设备会自动重连，频繁出现就把设备挪近路由器")
        .detail(reason),
        "设备授权已撤销" => EventRecord::warn(DiagCategory::Device, text)
            .hint("在设备列表里重新允许这台设备后，重启设备"),
        _ if reason.contains("失败") || reason.contains("出错") => {
            EventRecord::warn(DiagCategory::Device, text)
        }
        _ => EventRecord::info(DiagCategory::Device, text),
    }
}
