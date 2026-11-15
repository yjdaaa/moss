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

    /// 对话模式会话开始时向声明了 MCP 的设备发 `initialize`（回声模式按需在首次桌面端请求时握手）。
    pub(super) async fn start_device_mcp(
        &mut self,
        socket: &mut crate::link::Link,
    ) -> Result<(), String> {
        if self.conversation.is_none() {
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
        let capability = match (
            &self.vision_host,
            &self.vision_token,
            &self.shared.config.vision,
        ) {
            (Some(host), Some(token), Some(factory))
                if self.permission.load(std::sync::atomic::Ordering::Acquire)
                    && factory.ready() =>
            {
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
                let list = self.conversation.is_some().then(|| {
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
                        if let Some(prefs) =
                            self.shared.registry.device_hardware_prefs(self.device_id)
                        {
                            self.screen_pack = prefs
                                .screen_pack
                                .as_deref()
                                .is_some_and(crate::screen_pack::marker_supports_scenes);
                            self.shared
                                .state
                                .set_screen_pack(self.session_id, self.screen_pack);
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
                    .registry
                    .device_snapshot(self.device_id, kind, &text);
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
            .registry
            .device_snapshot(self.device_id, crate::SNAPSHOT_LISTEN, &snapshot);
    }

    /// 桌面端请求（2026-11-02）：刷新设备信息 / 重启设备。设备自带 MCP 未就绪时只记事件。
    pub(super) async fn on_device_request(
        &mut self,
        socket: &mut crate::link::Link,
        request: crate::DeviceRequest,
    ) -> Result<(), String> {
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
                if device.deferred_requests.len() < 16 {
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
        self.last_screen_emotion = Some(name);
        self.event(format!("小智全屏试看：{}", scene.label_zh()));
        Ok(())
    }

    async fn dispatch_device_request(
        &mut self,
        socket: &mut crate::link::Link,
        request: crate::DeviceRequest,
    ) -> Result<(), String> {
        match request {
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
                if self.conversation.is_some() && self.device_mcp.as_ref().is_some_and(|d| !d.ready)
                {
                    if let Some(dev) = self.device_mcp.as_mut() {
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
                    self.preview_hold = Some(PreviewHold {
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
                            self.pending_oled_preview = Some(clean_card);
                        } else if !self.send_stt(socket, format!("[预览] {clean_card}")).await {
                            return Err("发送失败".into());
                        }
                    }
                    return Ok(());
                }
                self.preview_hold = None;
                // 普通单色 OLED（`monochrome: true`）或未启用彩屏图片工具的官方固件：自动降级为滚动文字预览卡
                let card = if clean_card.is_empty() {
                    "激光雕刻预览已就绪".to_owned()
                } else {
                    clean_card
                };
                if self.busy() {
                    self.pending_oled_preview = Some(card);
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
