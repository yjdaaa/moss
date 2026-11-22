//! 会话构造、状态上报、合成与对话收尾，以及会话主循环。

use super::*;

impl<'a> Session<'a> {
    pub(super) fn finish_timing(&mut self, outcome: &str) {
        self.trace_turn_end(outcome);
        if let Some(turn) = self.turn.as_mut()
            && let Some(mut timing) = turn.timing.take()
        {
            timing.stages.insert(
                "turn_end".into(),
                turn.recording_offset
                    .saturating_add(turn.started.elapsed().as_millis() as u64),
            );
            let at_ms = crate::unix_now_ms();
            timing.outcome = outcome.into();
            timing.at_ms = at_ms;
            let turn_no = timing.turn;
            self.shared.state.finish_timing(timing);
            if turn.kind == TurnKind::Answer
                && self.shared.state.debug_enabled()
                && let Some(captured_epoch) = turn.debug_capture_epoch
                && let Some(raw_asr) = turn.debug_raw_asr.take()
            {
                let corrected_asr = turn
                    .debug_corrected_asr
                    .take()
                    .unwrap_or_else(|| raw_asr.clone());
                let spoken = turn.spoken.join("");
                let full_reply = turn
                    .pending_messages
                    .iter()
                    .rev()
                    .find(|m| m.role == moss_voice_providers::Role::Assistant)
                    .and_then(|m| m.content.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                let reply = if let Some(full) = full_reply {
                    full
                } else if !spoken.is_empty() {
                    spoken.clone()
                } else {
                    turn.debug_text_reply.join("")
                };
                if let Some((scrubbed, epoch)) = self.shared.state.finish_debug_trace_with_epoch(
                    crate::TurnDebugTrace {
                        id: 0,
                        at_ms,
                        device_id: self.device_id.into(),
                        session_id: self.session_id.into(),
                        turn: turn_no,
                        at: at_ms / 1000,
                        raw_asr,
                        corrected_asr,
                        tools: std::mem::take(&mut turn.debug_tools),
                        reply,
                        spoken,
                        outcome: outcome.into(),
                        truncated: false,
                        tools_dropped: 0,
                    },
                    captured_epoch,
                ) {
                    let registry = self.shared.registry.clone();
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        handle
                            .spawn_blocking(move || registry.record_chat_turn_at(&scrubbed, epoch));
                    } else {
                        registry.record_chat_turn_at(&scrubbed, epoch);
                    }
                }
            }
        }
    }

    pub(super) fn new(
        shared: &'a Shared,
        session_id: &'a str,
        device_id: &'a str,
        client_id: &'a str,
        version: u32,
        uplink_rate: u32,
        permission: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Self, String> {
        let downlink = shared.config.downlink_sample_rate;
        Ok(Self {
            recording_started: Instant::now(),
            permission: permission.clone(),
            recording_marks: BTreeMap::new(),
            turn_number: 0,
            shared,
            session_id,
            device_id,
            client_id,
            version,
            timestamp: 0,
            uplink_rate,
            decoder: OpusDecoder::new(uplink_rate)?,
            vad: Box::new(EnergyVad::new(uplink_rate, shared.config.vad.clone())),
            vad_worker: shared
                .config
                .vad_factory
                .as_ref()
                .map(|factory| {
                    crate::audio::worker::VadWorker::spawn(
                        factory.clone(),
                        uplink_rate,
                        shared.config.vad.clone(),
                    )
                })
                .transpose()?,
            pending_listen_stop: false,
            music: shared
                .config
                .music
                .as_ref()
                .map(|_| MusicController::new(permission.clone())),
            pump: PlaybackPump::new(
                OpusEncoder::new(downlink, FRAME_MS as u32)?,
                downlink,
                shared.config.playback_start_ms,
            ),
            decode_errors: 0,
            phase: Phase::Idle,
            last_activity: Instant::now(),
            listen: ListenAndBargeState {
                hold_check: None,
                stop_check: None,
                utterance_keyword: None,
                kws_tail_until: None,
                listen_manual: false,
                listen_realtime: false,
                barge: crate::barge::BargeIn::new(
                    uplink_rate,
                    shared.config.barge_in_ms,
                    &shared.config.vad,
                ),
                playback_started: None,
                barge_resumed: false,
                follow: crate::barge::BargeIn::follow_up(uplink_rate, &shared.config.vad),
                follow_probe: None,
                gate_next_turn: false,
                paused: None,
                paused_waits_for_model: false,
                last_loud: None,
                last_utterance: None,
            },
            bg: BackgroundQueryState::default(),
            screen: SessionScreenState::default(),
            notices: JobNoticeState {
                jobs: crate::jobs::JobWatcher::new(),
                next_job_poll: Instant::now() + shared.config.job_poll_interval,
                pending_finished: std::collections::VecDeque::new(),
                pending_agent_notices: std::collections::VecDeque::new(),
            },
            conversation: match shared.config.mode {
                ServerMode::Echo => None,
                ServerMode::Chat => shared.config.conversation.clone(),
            },
            history: Vec::new(),
            turn: None,
            carried_photo: None,
            last_verified_person: None,
            vision_host: None,
            vision_token: None,
            device_mcp: None,
            mqtt: false,
            pending_goodbye: None,
        })
    }

    fn sync_pulse(&self) {
        let (phase, level) = self.shared.active_aggregate_pulse();
        self.shared.registry.pulse_changed(phase.as_deref(), level);
    }

    pub(super) fn set_state(&self, state: &str) {
        self.shared.state.set_state(self.session_id, state);
        self.sync_pulse();
    }

    pub(super) fn set_level(&self, level: u8) {
        self.shared.state.set_level(self.session_id, level);
        self.sync_pulse();
    }

    pub(super) fn event(&self, record: impl Into<EventRecord>) {
        self.shared.state.event_in_flow(record, self.trace_flow());
    }

    pub(super) fn stash_uncommitted_photo(&mut self, turn: &mut ActiveTurn) {
        if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
            self.carried_photo = None;
            return;
        }
        if let Some(photo) = turn.pending_photo.take() {
            self.carried_photo.get_or_insert(photo);
        }
    }

    /// 暂停中的回答确定被打断并作废后，把更新后的历史（或尚未提交给任何说话人的视觉照片）交给等放行的插话轮。
    pub(super) fn sync_fresh_history_after_abandon_paused(&mut self) {
        let Some(conversation) = &self.conversation else {
            return;
        };
        if conversation.personal_context.is_none() {
            if let Some(turn) = self.turn.as_ref()
                && let Ok(mut slot) = turn.fresh_history.lock()
            {
                *slot = Some(self.history.clone());
            }
            return;
        }
        if let Some(turn) = self.turn.as_mut()
            && turn.kind == TurnKind::Answer
        {
            if turn.pending_photo.is_some() {
                self.carried_photo = None;
            } else if !turn.fast_stop
                && let Some(photo) = self.carried_photo.take()
            {
                if let Ok(mut slot) = turn.fresh_history.lock() {
                    *slot = Some(vec![photo.clone()]);
                }
                turn.pending_photo = Some(photo);
            }
        }
    }

    /// 字幕里有设备显示不出、MOSS 也补不上的字时记一条诊断（只记字数，不把原话/回复字符写入公共诊断事件，防泄露用户文本）。
    fn emit_glyph_gap_event(&mut self, undisplayable: Vec<char>, full_pack_ready: bool) {
        let fresh: Vec<char> = undisplayable
            .into_iter()
            .filter(|ch| self.screen.glyph_gaps_reported.insert(*ch))
            .collect();
        if fresh.is_empty() {
            return;
        }
        let count = fresh.len();
        let record = if full_pack_ready {
            EventRecord::warn(
                DiagCategory::Device,
                format!("小智屏幕字幕缺字：有 {count} 个生僻字符不在设备字库与全量补字库里，这些字不会显示"),
            )
            .hint("可能是表情符号或超大字符集罕见字，小智屏幕字库暂未收录")
        } else {
            EventRecord::warn(
                DiagCategory::Device,
                format!("小智屏幕字幕缺字：有 {count} 个生僻字不在设备字库里，全量补字库也没就位，这些字不会显示"),
            )
            .hint("在语音中心下载全量补字库后，生僻字会随字幕自动补给设备")
        };
        self.event(record);
    }

    /// 在阻塞线程池准备字幕缺字诊断与动态字模（纯基础字走内存快路径，不占 2-worker Tokio 音频线程）。
    async fn prepare_glyphs_async(
        &mut self,
        text: &str,
    ) -> Option<moss_xiaozhi_proto::GlyphPushPayload> {
        if !crate::glyph_font::GlyphFontService::needs_lookup(
            self.screen.text_font_cap.as_ref(),
            text,
        ) {
            return None;
        }
        let glyph_fonts = self.shared.glyph_fonts.clone();
        let cap = self.screen.text_font_cap.clone();
        let text_owned = text.to_owned();
        let (undisplayable, full_ready, glyph_push) = tokio::task::spawn_blocking(move || {
            glyph_fonts.prepare_text_glyphs(cap.as_ref(), &text_owned)
        })
        .await
        .unwrap_or_default();
        self.emit_glyph_gap_event(undisplayable, full_ready);
        glyph_push
    }

    pub(super) async fn send_stt(&mut self, socket: &mut crate::link::Link, text: String) -> bool {
        let glyph_push = self.prepare_glyphs_async(&text).await;
        let sent = send_json(
            socket,
            &ServerMessage::Stt {
                session_id: self.session_id.into(),
                text: text.clone(),
                glyph_push,
            },
        )
        .await;
        if sent {
            self.screen.last_screen_stt = Some(text);
        }
        sent
    }

    pub(super) async fn tts(
        &mut self,
        socket: &mut crate::link::Link,
        state: TtsState,
        text: Option<String>,
    ) -> Result<(), String> {
        let starting = matches!(state, TtsState::Start);
        let glyph_push = match (&state, text.as_deref()) {
            (TtsState::SentenceStart, Some(s)) => {
                self.screen.last_screen_stt = None;
                self.prepare_glyphs_async(s).await
            }
            _ => None,
        };
        let message = ServerMessage::Tts {
            session_id: self.session_id.into(),
            state,
            text,
            glyph_push,
        };
        if !send_json(socket, &message).await {
            return Err("发送失败".into());
        }
        if starting {
            // n70 开口说话：全屏画风切到“说话”（听歌、完工、急停画面说话时保持）
            let scene = self
                .sticky_while_talking()
                .or(self.screen.turn_scene)
                .unwrap_or(crate::BotPose::Talking);
            self.show_screen(socket, scene, true).await?;
        }
        Ok(())
    }

    /// 一轮对话（WebSocket 一次连接 / 常连模式一次唤醒）结束：按设置排队记忆摘要与结构化提取。
    pub(super) fn conversation_closed(&self) {
        let Some(conversation) = &self.conversation else {
            return;
        };
        let end_ctx = ToolContext {
            device_id: self.device_id.to_owned(),
            session_id: self.session_id.to_owned(),
        };
        if let Some(personal) = &conversation.personal_context {
            let person_histories = personal.session_closed(
                &end_ctx,
                trace::background_llm(conversation, self.device_id, "按人记忆整理"),
            );
            if let Some(extraction) = &conversation.context_extraction {
                for (scope, person_history) in &person_histories {
                    extraction.session_closed_for_person(
                        &end_ctx,
                        Some(scope),
                        person_history,
                        trace::background_llm(conversation, self.device_id, "长期记忆提取"),
                    );
                }
            }
        } else {
            // n57: structured extraction replaces the NV10 whole-summary rewrite once it is on.
            let extracting = conversation
                .context_extraction
                .as_ref()
                .is_some_and(|e| e.extractor.enabled());
            if let Some(memory) = conversation.memory.as_ref().filter(|_| !extracting) {
                memory.queue_summary(
                    self.device_id,
                    &self.history,
                    trace::background_llm(conversation, self.device_id, "对话记忆摘要"),
                );
            }
            // NV19-03: independent of the NV10 summary; off unless the user enabled it.
            if let Some(extraction) = &conversation.context_extraction {
                extraction.session_closed(
                    &end_ctx,
                    &self.history,
                    trace::background_llm(conversation, self.device_id, "长期记忆提取"),
                );
            }
        }
    }

    /// n71 常连模式结束这一轮对话：停下正在说的话与拾音，关语音通道（`notify` 时让设备也关），
    /// 做与 WebSocket 断开相同的记忆收尾，控制通道与设备在线状态保持不变。
    pub(super) async fn end_conversation(
        &mut self,
        socket: &mut crate::link::Link,
        reason: &str,
        notify: bool,
    ) -> Result<(), String> {
        if let Some(music) = self.music.as_mut() {
            music.cancel(&mut self.pump);
        }
        // 先收尾在途轮次的完整过程记录与耗时，再清空 self.turn，避免常连结束时漏记 turn_end 与合成语音
        self.finish_timing("disconnected");
        self.turn = None;
        self.listen.paused = None;
        self.listen.paused_waits_for_model = false;
        if let Some(task) = self.listen.stop_check.take() {
            task.abort();
        }
        // 后台查询属于这一场对话：中止（丢弃即中止）并清掉待播结果，不带进下一场
        self.bg.background.clear();
        self.bg.background_ready.clear();
        self.bg.reporting = None;
        self.carried_photo = None;
        self.pump.clear();
        self.listen.hold_check = None;
        self.pending_listen_stop = false;
        if let Some(worker) = self.vad_worker.as_mut() {
            worker.reset();
        }
        self.listen.playback_started = None;
        self.phase = Phase::Idle;
        self.set_state("idle");
        socket.end_audio(self.session_id, notify);
        if self.permission.load(std::sync::atomic::Ordering::Acquire) {
            self.conversation_closed();
        }
        self.history.clear();
        self.last_verified_person = None;
        self.event(format!("这一轮对话结束（{reason}），设备保持常连在线"));
        Ok(())
    }

    /// 会话循环；返回断开原因。
    pub(super) async fn run(
        &mut self,
        socket: &mut crate::link::Link,
        requests: &mut tokio::sync::mpsc::UnboundedReceiver<crate::DeviceRequest>,
    ) -> String {
        let mut shutdown = self.shared.shutdown.clone();
        let idle_timeout = self.shared.config.idle_timeout;
        let mut requests_closed = false;
        if let Err(reason) = self.start_device_mcp(socket).await {
            return reason;
        }
        loop {
            if *shutdown.borrow() {
                return "内置语音服务已停止".into();
            }
            if let Some(device) = self.device_mcp.as_mut() {
                device.prune_pending(Instant::now());
            }
            let deadline = match &self.phase {
                Phase::Speaking { next, .. } => *next,
                // 对话任务有自己的超时（识别 / 合成 30 秒，大模型 120 秒），等待期间不算空闲
                Phase::Thinking => self
                    .turn
                    .as_ref()
                    .and_then(|turn| turn.hold)
                    .map_or_else(|| Instant::now() + Duration::from_secs(3600), |(at, _)| at),
                // n71 常连模式没在对话（语音通道关着）时不算空闲，设备一直在线
                _ if !socket.audio_open()
                    && self.turn.is_none()
                    && self.listen.paused.is_none() =>
                {
                    Instant::now() + Duration::from_secs(3600)
                }
                _ => self.last_activity + idle_timeout,
            };
            let hold_deadline = match &self.phase {
                Phase::Listening(listening) => listening.hold_deadline,
                _ => None,
            };
            let preview_hold_deadline = self.screen.preview_hold.as_ref().map(|h| h.next_refresh);
            let music_hud_deadline = self.screen.music_hud.as_ref().map(|m| m.next_refresh);
            // 加工结束播报与副屏 HUD 用固定节拍（不随消息重置），否则 realtime 一直上传音频时永远轮不到
            let job_deadline = self.notices.next_job_poll;
            let report_deadline = self.background_report_deadline(socket);
            // 按顺序检查（biased）：对话任务的事件先于它发出的设备工具调用处理，设备看到的顺序与任务一致
            // （先 stt 再 tools/call）；设备消息先于播放定时器，打断不被积压的播放拍延迟。
            let result = tokio::select! {
                biased;
                _ = shutdown.changed() => return "内置语音服务已停止".into(),
                frame = async {
                    match self.vad_worker.as_mut() {
                        Some(worker) => worker.next().await,
                        None => std::future::pending().await,
                    }
                } => self.on_vad_result(socket, frame).await,
                event = next_turn_event(&mut self.turn) => self.on_turn_event(socket, event).await,
                interim = next_listening_interim(&mut self.phase) => self.on_interim(socket, interim).await,
                incoming = socket.recv() => match incoming {
                    Some(Ok(Message::Binary(data))) => self.on_binary(socket, &data).await,
                    Some(Ok(Message::Text(text))) => self.on_text(socket, text.as_str()).await,
                    Some(Ok(Message::Close(_))) | None => return "设备关闭了连接".into(),
                    Some(Ok(_)) => Ok(()),
                    Some(Err(_)) => return "连接出错".into(),
                },
                (paused_at, held) = next_listening_hold_check(&mut self.listen.hold_check) => self.on_hold_check_done(socket, paused_at, held).await,
                stop = next_stop_check(&mut self.listen.stop_check) => self.on_stop_check_done(socket, stop).await,
                call = next_device_call(&mut self.device_mcp) => self.forward_device_call(socket, call).await,
                request = async {
                    if requests_closed {
                        std::future::pending().await
                    } else {
                        requests.recv().await
                    }
                } => match request {
                    Some(request) => self.on_device_request(socket, request).await,
                    None => {
                        requests_closed = true;
                        Ok(())
                    }
                },
                _ = async {
                    match hold_deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => self.on_hold_expired(socket).await,
                _ = async {
                    match preview_hold_deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => self.on_preview_hold_tick(socket).await,
                _ = async {
                    match music_hud_deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => self.on_music_hud_tick(socket).await,
                _ = tokio::time::sleep_until(deadline) => {
                    match self.phase {
                        Phase::Speaking { .. } => self.tick(socket).await,
                        Phase::Thinking => {
                            self.check_release();
                            Ok(())
                        }
                        _ => self.on_idle(socket).await,
                    }
                }
                _ = tokio::time::sleep_until(job_deadline) => self.poll_jobs(socket).await,
                _ = async {
                    match report_deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => Ok(()),
                (question, outcomes) = next_background_done(&mut self.bg.background) => {
                    self.background_done(question, &outcomes);
                    Ok(())
                }
            };
            if let Err(reason) = result {
                return reason;
            }
            self.deliver_background(socket);
            // n71 常连模式：这一轮对话该结束了（再见说完、用户说退出、空闲到期）
            if let Some(reason) = self.pending_goodbye.take()
                && let Err(reason) = self.end_conversation(socket, &reason, true).await
            {
                return reason;
            }
        }
    }
}
