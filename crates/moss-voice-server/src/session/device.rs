//! 设备自带 MCP：初始化、工具列表与调用、桌面端即时请求。

use super::*;

impl<'a> Session<'a> {
    async fn send_mcp(
        &mut self,
        socket: &mut crate::link::Link,
        payload: Value,
    ) -> Result<(), String> {
        let message = ServerMessage::Mcp {
            session_id: self.session_id.into(),
            payload,
        };
        if send_json(socket, &message).await {
            Ok(())
        } else {
            Err("发送失败".into())
        }
    }

    /// 对话模式（或桌面端开启 `eager_device_mcp` 的回声模式）会话开始时向声明了 MCP 的设备发 `initialize`；
    /// 纯协议回声模式仍按需在首次桌面端请求时握手。
    pub(super) async fn start_device_mcp(
        &mut self,
        socket: &mut crate::link::Link,
    ) -> Result<(), String> {
        if self.conversation.is_none() && !self.shared.config.eager_device_mcp {
            return Ok(());
        }
        self.send_mcp_initialize(socket).await
    }

    async fn send_mcp_initialize(&mut self, socket: &mut crate::link::Link) -> Result<(), String> {
        let Some(device) = self.device_mcp.as_mut() else {
            return Ok(());
        };
        if device.init_sent {
            return Ok(());
        }
        device.init_sent = true;
        // Only an already-authorized session gets a currently configured vision capability.
        // Host comes from the validated websocket Host header; port is the ACTUAL bound HTTP port.
        let vision_ready = match &self.shared.config.vision {
            Some(factory) if self.permission.load(std::sync::atomic::Ordering::Acquire) => {
                let factory = factory.clone();
                tokio::task::spawn_blocking(move || factory.ready())
                    .await
                    .unwrap_or(false)
            }
            _ => false,
        };
        let Some(device) = self.device_mcp.as_mut() else {
            return Ok(());
        };
        let capability = match (&self.vision_host, &self.vision_token) {
            (Some(host), Some(token)) if vision_ready => {
                device.vision_url_sent = true;
                device.vision_token_issued_at = Some(Instant::now());
                json!({"vision":{"url":format!("http://{host}:{}/mcp/vision/explain",self.shared.http_port),"token":token}})
            }
            _ => json!({}),
        };
        let params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": capability,
            "clientInfo": {"name": "MOSS", "version": env!("CARGO_PKG_VERSION")},
        });
        let payload = device.request("initialize", params, McpPending::Initialize);
        self.send_mcp(socket, payload).await
    }

    /// 设备发来的 MCP 消息：只处理对我们请求的响应，设备主动发来的请求与通知忽略。
    pub(super) async fn on_device_mcp(
        &mut self,
        socket: &mut crate::link::Link,
        payload: Value,
    ) -> Result<(), String> {
        let Some(device) = self.device_mcp.as_mut() else {
            return Ok(());
        };
        let Some(id) = payload.get("id").and_then(Value::as_u64) else {
            return Ok(());
        };
        if payload.get("method").is_some() {
            return Ok(());
        }
        match device.pending.remove(&id).map(|(_, pending)| pending) {
            Some(McpPending::Initialize) => {
                device.initialized = true;
                let deferred = std::mem::take(&mut device.deferred_requests);
                let need_tools_list = (self.conversation.is_some()
                    || self.shared.config.eager_device_mcp
                    || deferred.iter().any(|req| {
                        matches!(
                            req,
                            crate::DeviceRequest::SyncTools
                                | crate::DeviceRequest::CapturePhoto { .. }
                        )
                    }))
                    && device.pages == 0;
                let list = need_tools_list.then(|| {
                    device.request(
                        "tools/list",
                        json!({"withUserTools": true}),
                        McpPending::ToolsList,
                    )
                });
                self.send_mcp(
                    socket,
                    json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                )
                .await?;
                if let Some(list) = list {
                    self.send_mcp(socket, list).await?;
                }
                for req in deferred {
                    self.dispatch_device_request(socket, req).await?;
                }
                Ok(())
            }
            Some(McpPending::ToolsList) => {
                if let Some(tools) = payload.pointer("/result/tools").and_then(Value::as_array) {
                    device.tools.extend(tools.iter().cloned());
                }
                let cursor = payload
                    .pointer("/result/nextCursor")
                    .and_then(Value::as_str)
                    .filter(|cursor| !cursor.is_empty());
                match cursor {
                    Some(cursor) if device.pages < MAX_TOOL_PAGES => {
                        let next = device.request(
                            "tools/list",
                            json!({"cursor": cursor, "withUserTools": true}),
                            McpPending::ToolsList,
                        );
                        self.send_mcp(socket, next).await
                    }
                    _ => {
                        device.ready = true;
                        let all_names = device.all_tool_names();
                        let count = device.model_tools().len();
                        self.shared
                            .state
                            .set_device_tools(self.session_id, all_names);
                        self.event(format!("设备提供 {count} 个自带工具"));
                        if self
                            .device_mcp
                            .as_ref()
                            .is_some_and(|d| d.has_tool("self.screen.get_info"))
                            && let Some(dev) = self.device_mcp.as_mut()
                        {
                            let req = dev.request(
                                "tools/call",
                                json!({ "name": "self.screen.get_info", "arguments": {} }),
                                McpPending::ScreenInfo,
                            );
                            self.send_mcp(socket, req).await?;
                        }
                        let prefs_registry = self.shared.registry.clone();
                        let prefs_device = self.device_id.to_owned();
                        let prefs = tokio::task::spawn_blocking(move || {
                            prefs_registry.device_hardware_prefs(&prefs_device)
                        })
                        .await
                        .ok()
                        .flatten();
                        if let Some(prefs) = prefs {
                            self.screen.screen_pack = prefs
                                .screen_pack
                                .as_deref()
                                .is_some_and(crate::screen_pack::marker_supports_scenes);
                            self.shared
                                .state
                                .set_screen_pack(self.session_id, self.screen.screen_pack);
                            let mut replayed = false;
                            if let Some(v) = prefs.volume
                                && self
                                    .device_mcp
                                    .as_ref()
                                    .is_some_and(|d| d.has_tool("self.audio_speaker.set_volume"))
                            {
                                self.send_quiet_device_call(
                                    socket,
                                    "self.audio_speaker.set_volume",
                                    json!({"volume": v.min(100)}),
                                )
                                .await?;
                                replayed = true;
                            }
                            if let Some(b) = prefs.brightness {
                                if self
                                    .device_mcp
                                    .as_ref()
                                    .is_some_and(|d| d.has_tool("self.screen.set_brightness"))
                                {
                                    self.send_quiet_device_call(
                                        socket,
                                        "self.screen.set_brightness",
                                        json!({"brightness": b.min(100)}),
                                    )
                                    .await?;
                                    replayed = true;
                                } else if self
                                    .device_mcp
                                    .as_ref()
                                    .is_some_and(|d| d.has_tool("self.disp.setbacklight"))
                                {
                                    let level = ((b.min(100) as u16) * 255) / 100;
                                    self.send_quiet_device_call(
                                        socket,
                                        "self.disp.setbacklight",
                                        json!({"level": level}),
                                    )
                                    .await?;
                                    replayed = true;
                                }
                            }
                            if let Some(th) = prefs.theme
                                && self
                                    .device_mcp
                                    .as_ref()
                                    .is_some_and(|d| d.has_tool("self.screen.set_theme"))
                            {
                                self.send_quiet_device_call(
                                    socket,
                                    "self.screen.set_theme",
                                    json!({"theme": th}),
                                )
                                .await?;
                                replayed = true;
                            }
                            if let Some(mode) = prefs.talk_mode
                                && self
                                    .device_mcp
                                    .as_ref()
                                    .is_some_and(|d| d.has_tool("self.set_press_to_talk"))
                            {
                                self.send_quiet_device_call(
                                    socket,
                                    "self.set_press_to_talk",
                                    json!({"mode": mode}),
                                )
                                .await?;
                                replayed = true;
                            }
                            if replayed {
                                self.dispatch_device_request(
                                    socket,
                                    crate::DeviceRequest::RefreshInfo,
                                )
                                .await?;
                            }
                        }
                        let ready_deferred = self
                            .device_mcp
                            .as_mut()
                            .map(|d| std::mem::take(&mut d.deferred_requests))
                            .unwrap_or_default();
                        for req in ready_deferred {
                            self.dispatch_device_request(socket, req).await?;
                        }
                        Ok(())
                    }
                }
            }
            Some(McpPending::Call(reply)) => {
                let _ = reply.send(device_result_text(&payload));
                Ok(())
            }
            Some(McpPending::Snapshot(kind)) => {
                let text = device_result_text(&payload);
                self.shared
                    .schedule_device_snapshot(self.device_id, kind, text);
                Ok(())
            }
            Some(McpPending::Quiet(tool)) => {
                if payload.get("error").is_some()
                    || payload
                        .pointer("/result/isError")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                {
                    let msg = device_result_text(&payload);
                    self.event(
                        EventRecord::warn(
                            DiagCategory::Device,
                            format!("桌面端请求 {tool} 未成功：设备回复了错误"),
                        )
                        .hint("设备固件可能不支持这个操作，或设备正忙：稍后再试，必要时升级固件")
                        .detail(&msg),
                    );
                }
                Ok(())
            }
            Some(McpPending::ScreenInfo) => {
                let text = device_result_text(&payload);
                if let Ok(info) = serde_json::from_str::<Value>(&text) {
                    let mono = info.get("monochrome").and_then(Value::as_bool);
                    let width = info
                        .get("width")
                        .and_then(Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok())
                        .filter(|v| (64..=1024).contains(v));
                    let height = info
                        .get("height")
                        .and_then(Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok())
                        .filter(|v| (32..=1024).contains(v));
                    if let Some(dev) = self.device_mcp.as_mut() {
                        if mono.is_some() {
                            dev.screen_monochrome = mono;
                        }
                        if let (Some(w), Some(h)) = (width, height) {
                            dev.screen_size = Some((w, h));
                        }
                    }
                    self.shared
                        .state
                        .set_screen_info(self.session_id, mono, width, height);
                }
                Ok(())
            }
            Some(McpPending::CameraCapture { ticket }) => {
                let is_rpc_error = payload.get("error").is_some();
                let is_tool_error = payload
                    .pointer("/result/isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let text = device_result_text(&payload);
                let json_fail_message = if !is_rpc_error && !is_tool_error {
                    serde_json::from_str::<Value>(&text)
                        .ok()
                        .filter(|v| v.get("success").and_then(Value::as_bool) == Some(false))
                        .map(|v| {
                            v.get("message")
                                .or_else(|| v.pointer("/error/message"))
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                                .unwrap_or_else(|| text.clone())
                        })
                } else {
                    None
                };
                if is_rpc_error || is_tool_error || json_fail_message.is_some() {
                    let detail = json_fail_message.unwrap_or(text);
                    let human = if detail.contains("Failed to connect to explain URL") {
                        format!(
                            "设备无法连接电脑视觉接口（{detail}），请检查电脑防火墙是否放行端口 {}",
                            self.shared.http_port
                        )
                    } else if detail.contains("Failed to capture photo") {
                        "设备摄像头抓拍失败（Failed to capture photo），请检查摄像头排线或供电"
                            .into()
                    } else {
                        detail
                    };
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        human.clone(),
                    );
                    self.event(EventRecord::warn(
                        DiagCategory::Device,
                        format!("摄像头抓拍未成功：{human}"),
                    ));
                }
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// 2026-11-02：记录设备实际使用的拾音模式（每次开始拾音写一次，条数有界）。
    /// 这是判断设备端 AEC 是否开启的行为证据：固件的 `GetDefaultListeningMode()` 在 AEC
    /// 开启时返回 `realtime`、关闭时返回 `auto`，所以 `mode` 比固件自称可靠。
    /// 只记录、不做任何门禁。
    pub(super) fn record_listen_mode(&mut self, manual: bool, realtime: bool) {
        let mode = if realtime {
            "realtime"
        } else if manual {
            "manual"
        } else {
            "auto"
        };
        let snapshot = json!({ "mode": mode }).to_string();
        self.shared
            .schedule_device_snapshot(self.device_id, crate::SNAPSHOT_LISTEN, snapshot);
    }

    /// 清理超过 `pending_ttl` 仍未应答的设备端 MCP 请求。
    /// `Initialize` / `ToolsList` 超时只重置在途状态并释放滞留抓拍请求，不把 `device_tools_ready` 标为完成，
    /// 以便后续 `SyncTools` 重试而不会把暂时无响应误报为“固件不支持”。
    pub(super) fn prune_stalled_device_mcp(&mut self) {
        let stalled_deferred = if let Some(device) = self.device_mcp.as_mut()
            && device.prune_pending(Instant::now())
        {
            Some(std::mem::take(&mut device.deferred_requests))
        } else {
            None
        };
        if let Some(deferred) = stalled_deferred {
            for req in deferred {
                if let crate::DeviceRequest::CapturePhoto { ticket } = req {
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        "设备端 MCP 握手或工具列表同步超时，请稍后再试".into(),
                    );
                }
            }
        }
    }

    /// 桌面端请求（2026-11-02）：刷新设备信息 / 重启设备。设备自带 MCP 未就绪时只记事件。
    pub(super) async fn on_device_request(
        &mut self,
        socket: &mut crate::link::Link,
        request: crate::DeviceRequest,
    ) -> Result<(), String> {
        self.prune_stalled_device_mcp();
        if let crate::DeviceRequest::InjectText {
            text,
            simulated,
            reply,
        } = request
        {
            return self
                .handle_inject_text(socket, text, simulated, reply)
                .await;
        }
        if let crate::DeviceRequest::InjectAudio {
            pcm,
            sample_rate,
            frame_ms,
            lead_silence_ms,
            tail_silence_ms,
            reply,
        } = request
        {
            return self
                .handle_inject_audio(
                    pcm,
                    sample_rate,
                    frame_ms,
                    lead_silence_ms,
                    tail_silence_ms,
                    reply,
                )
                .await;
        }
        if let crate::DeviceRequest::ShowScene { scene, style } = request {
            return self.show_scene_request(socket, scene, style).await;
        }
        if matches!(request, crate::DeviceRequest::PreviewScreen { .. })
            && self.device_mcp.is_none()
        {
            return self.dispatch_device_request(socket, request).await;
        }
        let should_send_init = match self.device_mcp.as_mut() {
            Some(device) if !device.initialized => {
                if matches!(request, crate::DeviceRequest::SyncTools)
                    && device
                        .deferred_requests
                        .iter()
                        .any(|r| matches!(r, crate::DeviceRequest::SyncTools))
                {
                    // 已有排队中的 SyncTools，保持幂等不挤占延迟队列
                } else if device.deferred_requests.len() < 16 {
                    device.deferred_requests.push(request);
                } else if let crate::DeviceRequest::CapturePhoto { ticket } = request {
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        "设备待处理请求过多，请稍后再试".into(),
                    );
                }
                !device.init_sent
            }
            Some(_) => {
                return self.dispatch_device_request(socket, request).await;
            }
            None => {
                if let crate::DeviceRequest::CapturePhoto { ticket } = request {
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        "该设备未启用设备端 MCP，不支持摄像头抓拍".into(),
                    );
                }
                self.event(
                    EventRecord::warn(
                        DiagCategory::Device,
                        "设备没有提供自带 MCP，桌面端请求已忽略",
                    )
                    .hint("这台设备的固件不支持从电脑控制：升级到支持设备端 MCP 的小智固件"),
                );
                return Ok(());
            }
        };
        if should_send_init {
            return self.send_mcp_initialize(socket).await;
        }
        Ok(())
    }

    pub(super) async fn send_quiet_device_call(
        &mut self,
        socket: &mut crate::link::Link,
        tool: &'static str,
        arguments: Value,
    ) -> Result<(), String> {
        let Some(device) = self.device_mcp.as_mut() else {
            return Ok(());
        };
        let payload = device.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
            McpPending::Quiet(tool),
        );
        self.send_mcp(socket, payload).await
    }

    /// n70 语音中心“推到小智试看”：已刷全屏画风包的设备直接切到该场景（按指定画风）。
    async fn show_scene_request(
        &mut self,
        socket: &mut crate::link::Link,
        scene: crate::BotPose,
        style: crate::ScreenCardStyle,
    ) -> Result<(), String> {
        if !self.pack_active() {
            return Ok(());
        }
        let name = crate::screen_pack::emotion_name(style, scene);
        let message = ServerMessage::Llm {
            session_id: self.session_id.into(),
            text: name.clone(),
            emotion: name.clone(),
        };
        if !send_json(socket, &message).await {
            return Err("发送失败".into());
        }
        self.screen.last_screen_emotion = Some(name);
        self.event(format!("小智全屏试看：{}", scene.label_zh()));
        Ok(())
    }

    /// VA1 真小智文字直通注入：在设备空闲（或静音聆听无人说话）时立即下发 `stt` 字幕并进入完整对话链路，
    /// 绝不打断正在进行的对话、拾音或音乐播放。
    async fn handle_inject_text(
        &mut self,
        socket: &mut crate::link::Link,
        text: String,
        simulated: bool,
        reply: tokio::sync::oneshot::Sender<Result<crate::InjectTurnAck, crate::InjectTurnError>>,
    ) -> Result<(), String> {
        let trimmed = text.trim();
        if trimmed.is_empty()
            || trimmed.chars().count() > 500
            || trimmed.chars().any(char::is_control)
        {
            let _ = reply.send(Err(crate::InjectTurnError::invalid(
                "注入文字须为 1~500 字且不含控制字符",
            )));
            return Ok(());
        }
        if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
            let _ = reply.send(Err(crate::InjectTurnError::forbidden(
                "设备授权已撤销，无法注入文字",
            )));
            return Ok(());
        }
        if self.conversation.is_none() {
            let _ = reply.send(Err(crate::InjectTurnError::conflict(
                "echo_mode",
                "内置语音当前处于回声模式（未配置识别/大模型/合成），无法执行对话注入",
            )));
            return Ok(());
        }
        if self.busy()
            || self.actively_speaking_user()
            || self.music.as_ref().is_some_and(|m| m.active())
        {
            let _ = reply.send(Err(crate::InjectTurnError::busy(self.phase.label())));
            return Ok(());
        }
        // 设备处于自动拾音静默等待时，清空空缓冲再进入思考，避免旧静音缓冲随后误判
        self.listen.hold_check = None;
        self.pending_listen_stop = false;
        if let Some(worker) = self.vad_worker.as_mut() {
            worker.reset();
        }
        self.last_activity = Instant::now();
        let injected = trimmed.to_owned();
        if !self.send_stt(socket, injected.clone()).await {
            let _ = reply.send(Err(crate::InjectTurnError::offline(
                "向设备下发 stt 字幕失败，会话可能已断开",
            )));
            return Err("发送失败".into());
        }
        self.event(format!(
            "文字直通注入{}：{} 字",
            if simulated { "（模拟）" } else { "" },
            injected.chars().count()
        ));
        self.start_turn(
            TurnInput::Text {
                text: injected,
                simulated,
            },
            TurnKind::Answer,
        );
        let ack = crate::InjectTurnAck {
            turn_id: format!("{}:{}", self.session_id, self.turn_number),
            device_id: self.device_id.into(),
            session_id: self.session_id.into(),
            turn: self.turn_number,
            simulated,
        };
        let _ = reply.send(Ok(ack));
        Ok(())
    }

    /// VC2 测试时间窗内的服务端数字音频对照注入：按设备上行帧节奏送入真实 VAD/ASR/对话链路，跳过麦克风。
    async fn handle_inject_audio(
        &mut self,
        pcm: Vec<i16>,
        sample_rate: u32,
        frame_ms: u32,
        lead_silence_ms: u32,
        tail_silence_ms: u32,
        reply: oneshot::Sender<Result<crate::InjectTurnAck, crate::InjectTurnError>>,
    ) -> Result<(), String> {
        if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
            let _ = reply.send(Err(crate::InjectTurnError::forbidden(
                "设备授权已撤销，无法注入数字音频",
            )));
            return Ok(());
        }
        if !self.shared.is_bench_window_active(self.device_id) {
            let _ = reply.send(Err(crate::InjectTurnError {
                code: "bench_window_required".into(),
                http_status: 403,
                message: "数字音频注入仅允许在已打开的测试时间窗内使用".into(),
            }));
            return Ok(());
        }
        if self.conversation.is_none() {
            let _ = reply.send(Err(crate::InjectTurnError::conflict(
                "echo_mode",
                "内置语音当前处于回声模式，无法执行数字音频对照注入",
            )));
            return Ok(());
        }
        if self.busy()
            || self.actively_speaking_user()
            || self.digital_feed.is_some()
            || self.music.as_ref().is_some_and(|m| m.active())
        {
            let _ = reply.send(Err(crate::InjectTurnError::busy(self.phase.label())));
            return Ok(());
        }
        let body_pcm = if sample_rate == self.uplink_rate {
            pcm
        } else {
            let mut resampler = Resampler::new(sample_rate, self.uplink_rate);
            resampler.process(&pcm)
        };
        if body_pcm.is_empty() {
            let _ = reply.send(Err(crate::InjectTurnError::invalid("数字音频不能为空")));
            return Ok(());
        }
        let lead_samples = self.uplink_rate as usize * (lead_silence_ms as usize) / 1000;
        let tail_samples = self.uplink_rate as usize * (tail_silence_ms as usize) / 1000;
        let mut full_pcm = Vec::with_capacity(lead_samples + body_pcm.len() + tail_samples);
        full_pcm.resize(lead_samples, 0i16);
        full_pcm.extend_from_slice(&body_pcm);
        full_pcm.resize(full_pcm.len() + tail_samples, 0i16);

        let frame_samples = (self.uplink_rate as usize * (frame_ms as usize) / 1000).max(1);
        let duration_secs = body_pcm.len() as f64 / f64::from(self.uplink_rate.max(1));
        self.start_listening(false);
        self.last_activity = Instant::now();
        self.next_turn_is_digital = true;
        self.pending_digital_ack = Some(reply);
        self.digital_feed = Some(DigitalAudioFeed {
            pcm: full_pcm,
            offset: 0,
            frame_samples,
            frame_duration: Duration::from_millis(u64::from(frame_ms)),
            next_at: Instant::now(),
        });
        self.event(format!(
            "测试时间窗数字音频对照注入：约 {duration_secs:.1} 秒（{} Hz，每帧 {frame_ms} ms）",
            self.uplink_rate
        ));
        Ok(())
    }

    pub(super) async fn tick_digital_feed(
        &mut self,
        socket: &mut crate::link::Link,
    ) -> Result<(), String> {
        if !self.shared.is_bench_window_active(self.device_id) {
            self.cancel_digital_feed("测试时间窗已关闭，数字音频注入中止");
            return Ok(());
        }
        if !matches!(self.phase, Phase::Listening(_)) {
            self.digital_feed = None;
            return Ok(());
        }
        let (chunk, finished) = {
            let Some(feed) = self.digital_feed.as_mut() else {
                return Ok(());
            };
            let end = (feed.offset + feed.frame_samples).min(feed.pcm.len());
            let chunk = feed.pcm[feed.offset..end].to_vec();
            feed.offset = end;
            feed.next_at = Instant::now() + feed.frame_duration;
            (chunk, feed.offset >= feed.pcm.len())
        };
        if finished {
            self.digital_feed = None;
        }
        if !chunk.is_empty() {
            self.record_uplink_level(crate::audio::pcm_rms_level(&chunk));
            if !self.listen.listen_manual
                && let Some(worker) = self.vad_worker.as_mut()
            {
                worker.submit(chunk, false)?;
                if finished {
                    self.pending_listen_stop = true;
                }
                return Ok(());
            }
            if let Some((utterance, end)) = self.hear(&chunk, None) {
                self.next_turn_is_digital = true;
                return self.finish_or_hold_utterance(socket, utterance, end).await;
            }
        }
        if finished && matches!(self.phase, Phase::Listening(_)) {
            self.next_turn_is_digital = true;
            self.finish_listen_stop(socket).await?;
        }
        Ok(())
    }

    async fn dispatch_device_request(
        &mut self,
        socket: &mut crate::link::Link,
        request: crate::DeviceRequest,
    ) -> Result<(), String> {
        match request {
            crate::DeviceRequest::InjectText {
                text,
                simulated,
                reply,
            } => {
                return self
                    .handle_inject_text(socket, text, simulated, reply)
                    .await;
            }
            crate::DeviceRequest::InjectAudio {
                pcm,
                sample_rate,
                frame_ms,
                lead_silence_ms,
                tail_silence_ms,
                reply,
            } => {
                return self
                    .handle_inject_audio(
                        pcm,
                        sample_rate,
                        frame_ms,
                        lead_silence_ms,
                        tail_silence_ms,
                        reply,
                    )
                    .await;
            }
            crate::DeviceRequest::SyncTools => {
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                if !device.ready && device.pages == 0 {
                    let list = device.request(
                        "tools/list",
                        json!({"withUserTools": true}),
                        McpPending::ToolsList,
                    );
                    return self.send_mcp(socket, list).await;
                }
                Ok(())
            }
            crate::DeviceRequest::ShowScene { scene, style } => {
                return self.show_scene_request(socket, scene, style).await;
            }
            crate::DeviceRequest::RefreshInfo => {
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                let payload = device.request(
                    "tools/call",
                    json!({ "name": "self.get_device_status", "arguments": {} }),
                    McpPending::Snapshot(crate::SNAPSHOT_STATUS),
                );
                self.event("桌面端请求：self.get_device_status");
                self.send_mcp(socket, payload).await
            }
            crate::DeviceRequest::Reboot => {
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                // 重启不会回响应：设备随后断开，等它重新连接上报新的 OTA；不登记等待，免得留在 pending
                let payload = device.request_untracked(
                    "tools/call",
                    json!({ "name": "self.reboot", "arguments": {} }),
                );
                self.event("桌面端请求：self.reboot");
                self.send_mcp(socket, payload).await
            }
            crate::DeviceRequest::InstallAssets { token } => {
                if !self
                    .device_mcp
                    .as_ref()
                    .is_some_and(|d| d.has_tool("self.assets.set_download_url"))
                {
                    self.event(
                        EventRecord::warn(DiagCategory::Firmware, "设备固件不支持网络更新资源包（没有 self.assets.set_download_url），唤醒词未推送")
                            .hint("需要小智固件 2.0.2 以上、v2 分区表"),
                    );
                    return Ok(());
                }
                let host = self.preview_http_host();
                let url = format!(
                    "http://{host}:{}/xiaozhi/assets/{token}.bin",
                    self.shared.http_port
                );
                self.event("桌面端推送唤醒词资源包：设备将重启并自行下载写入");
                // 固件把两次调用按顺序排进主循环：先记下下载地址，再在 1 秒后重启。
                self.send_quiet_device_call(
                    socket,
                    "self.assets.set_download_url",
                    json!({ "url": url }),
                )
                .await?;
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                let payload = device.request_untracked(
                    "tools/call",
                    json!({ "name": "self.reboot", "arguments": {} }),
                );
                self.send_mcp(socket, payload).await
            }
            crate::DeviceRequest::CapturePhoto { ticket } => {
                if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
                    self.shared.camera_preview.clear_device(self.device_id);
                    return Err("设备授权已撤销".into());
                }
                if self.device_mcp.as_ref().is_some_and(|d| !d.ready) {
                    let mut list_req = None;
                    if let Some(dev) = self.device_mcp.as_mut() {
                        if dev.pages == 0 {
                            list_req = Some(dev.request(
                                "tools/list",
                                json!({"withUserTools": true}),
                                McpPending::ToolsList,
                            ));
                        }
                        if dev.deferred_requests.len() < 16 {
                            dev.deferred_requests
                                .push(crate::DeviceRequest::CapturePhoto { ticket });
                        } else {
                            self.shared.camera_preview.record_error(
                                self.device_id,
                                Some(&ticket),
                                "设备工具清单同步繁忙，请稍后再试".into(),
                            );
                        }
                    }
                    if let Some(list) = list_req {
                        self.send_mcp(socket, list).await?;
                    }
                    return Ok(());
                }
                if self
                    .device_mcp
                    .as_ref()
                    .is_some_and(|d| d.ready && !d.has_tool("self.camera.take_photo"))
                {
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        "该小智固件未上报摄像头工具（self.camera.take_photo）".into(),
                    );
                    return Ok(());
                }
                if self.client_id.trim().is_empty() {
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        "设备连接未携带 Client-Id，无法建立摄像头画面上传凭证".into(),
                    );
                    return Ok(());
                }
                self.last_activity = Instant::now();
                let need_vision_init = self.device_mcp.as_ref().is_some_and(|d| {
                    !d.vision_url_sent
                        || d.vision_token_issued_at
                            .is_none_or(|at| at.elapsed() > Duration::from_secs(3600))
                });
                if need_vision_init {
                    let host = self.preview_http_host();
                    let token =
                        self.shared
                            .signer
                            .issue(self.client_id, self.device_id, crate::unix_now());
                    let params = json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {
                            "vision": {
                                "url": format!("http://{host}:{}/mcp/vision/explain", self.shared.http_port),
                                "token": token,
                            }
                        },
                        "clientInfo": {"name": "MOSS", "version": env!("CARGO_PKG_VERSION")},
                    });
                    if let Some(dev) = self.device_mcp.as_mut() {
                        let init_payload = dev.request(
                            "initialize",
                            params,
                            McpPending::Quiet("initialize_vision"),
                        );
                        dev.vision_url_sent = true;
                        dev.vision_token_issued_at = Some(Instant::now());
                        if let Err(err) = self.send_mcp(socket, init_payload).await {
                            self.shared.camera_preview.record_error(
                                self.device_id,
                                Some(&ticket),
                                format!("发送摄像头能力握手失败：{err}"),
                            );
                            return Err(err);
                        }
                    }
                }
                let Some(device) = self.device_mcp.as_mut() else {
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        "该设备未启用设备端 MCP，不支持摄像头抓拍".into(),
                    );
                    return Ok(());
                };
                let question = format!("{}{ticket}", crate::vision::CAMERA_PREVIEW_PREFIX);
                let payload = device.request(
                    "tools/call",
                    json!({
                        "name": "self.camera.take_photo",
                        "arguments": { "question": question },
                    }),
                    McpPending::CameraCapture {
                        ticket: ticket.clone(),
                    },
                );
                if let Err(err) = self.send_mcp(socket, payload).await {
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        format!("发送摄像头抓拍指令失败：{err}"),
                    );
                    return Err(err);
                }
                Ok(())
            }
            crate::DeviceRequest::SetVolume(volume) => {
                let v = volume.min(100);
                self.event(format!("桌面端调节设备音量：{v}%"));
                self.send_quiet_device_call(
                    socket,
                    "self.audio_speaker.set_volume",
                    json!({ "volume": v }),
                )
                .await?;
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                let refresh = device.request(
                    "tools/call",
                    json!({ "name": "self.get_device_status", "arguments": {} }),
                    McpPending::Snapshot(crate::SNAPSHOT_STATUS),
                );
                self.send_mcp(socket, refresh).await
            }
            crate::DeviceRequest::SetBrightness(brightness) => {
                let b = brightness.min(100);
                self.event(format!("桌面端调节屏幕亮度：{b}%"));
                let use_amoled = self.device_mcp.as_ref().is_some_and(|d| {
                    !d.has_tool("self.screen.set_brightness")
                        && d.has_tool("self.disp.setbacklight")
                });
                if use_amoled {
                    let level = ((b as u16) * 255) / 100;
                    self.send_quiet_device_call(
                        socket,
                        "self.disp.setbacklight",
                        json!({ "level": level }),
                    )
                    .await?;
                } else {
                    self.send_quiet_device_call(
                        socket,
                        "self.screen.set_brightness",
                        json!({ "brightness": b }),
                    )
                    .await?;
                }
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                let refresh = device.request(
                    "tools/call",
                    json!({ "name": "self.get_device_status", "arguments": {} }),
                    McpPending::Snapshot(crate::SNAPSHOT_STATUS),
                );
                self.send_mcp(socket, refresh).await
            }
            crate::DeviceRequest::SetTheme(theme) => {
                self.event(format!("桌面端切换屏幕主题：{theme}"));
                self.send_quiet_device_call(
                    socket,
                    "self.screen.set_theme",
                    json!({ "theme": theme }),
                )
                .await?;
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                let refresh = device.request(
                    "tools/call",
                    json!({ "name": "self.get_device_status", "arguments": {} }),
                    McpPending::Snapshot(crate::SNAPSHOT_STATUS),
                );
                self.send_mcp(socket, refresh).await
            }
            crate::DeviceRequest::SetTalkMode(mode) => {
                self.event(format!("桌面端切换按键对话模式：{mode}"));
                self.send_quiet_device_call(
                    socket,
                    "self.set_press_to_talk",
                    json!({ "mode": mode }),
                )
                .await?;
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                if device.has_tool("self.get_device_status") {
                    let refresh = device.request(
                        "tools/call",
                        json!({ "name": "self.get_device_status", "arguments": {} }),
                        McpPending::Snapshot(crate::SNAPSHOT_STATUS),
                    );
                    self.send_mcp(socket, refresh).await?;
                }
                Ok(())
            }
            crate::DeviceRequest::PreviewScreen { token, summary } => {
                let clean_card: String = summary
                    .replace(['\r', '\n'], " ")
                    .trim()
                    .chars()
                    .take(96)
                    .collect();
                let can_color_image = token.is_some()
                    && self.device_mcp.as_ref().is_some_and(|d| {
                        d.has_tool("self.screen.preview_image") && d.screen_monochrome != Some(true)
                    });
                if let (true, Some(tok)) = (can_color_image, token) {
                    let host = self.preview_http_host();
                    let url = format!(
                        "http://{host}:{}/mcp/preview/{tok}.png",
                        self.shared.http_port
                    );
                    let now = Instant::now();
                    self.screen.preview_hold = Some(PreviewHold {
                        url: url.clone(),
                        until: now + self.shared.config.preview_hold_duration,
                        next_refresh: now + self.shared.config.preview_hold_interval,
                    });
                    self.event("向小智彩屏推送激光预览缩略图");
                    self.send_quiet_device_call(
                        socket,
                        "self.screen.preview_image",
                        json!({ "url": url }),
                    )
                    .await?;
                    if !clean_card.is_empty() {
                        if self.busy() {
                            self.screen.pending_oled_preview = Some(clean_card);
                        } else if !self.send_stt(socket, format!("[预览] {clean_card}")).await {
                            return Err("发送失败".into());
                        }
                    }
                    return Ok(());
                }
                self.screen.preview_hold = None;
                // 普通单色 OLED（`monochrome: true`）或未启用彩屏图片工具的官方固件：自动降级为滚动文字预览卡
                let card = if clean_card.is_empty() {
                    "激光雕刻预览已就绪".to_owned()
                } else {
                    clean_card
                };
                if self.busy() {
                    self.screen.pending_oled_preview = Some(card);
                    self.event("已暂存小智 OLED 雕刻预览文字卡（本轮语音播完后自动上屏）");
                    Ok(())
                } else if matches!(self.phase, Phase::Listening(_)) {
                    self.event("向小智 OLED 屏幕静默刷新雕刻预览参数字幕");
                    if self.send_stt(socket, format!("[预览] {card}")).await {
                        Ok(())
                    } else {
                        Err("发送失败".into())
                    }
                } else {
                    self.event("向小智 OLED 屏幕推送雕刻预览文字卡");
                    let alert = ServerMessage::Alert {
                        status: "雕刻预览".into(),
                        message: card,
                        emotion: "cool".into(),
                    };
                    if send_json(socket, &alert).await {
                        Ok(())
                    } else {
                        Err("发送失败".into())
                    }
                }
            }
        }
    }

    /// 把对话任务的设备工具调用转发给设备。
    pub(super) async fn forward_device_call(
        &mut self,
        socket: &mut crate::link::Link,
        call: Option<DeviceCall>,
    ) -> Result<(), String> {
        if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
            self.last_verified_person = None;
            return Err("设备授权已撤销".into());
        }
        let (Some(call), Some(device)) = (call, self.device_mcp.as_mut()) else {
            return Ok(());
        };
        let params = json!({"name": call.name, "arguments": call.arguments});
        let payload = device.request("tools/call", params, McpPending::Call(call.reply));
        self.send_mcp(socket, payload).await
    }
}
