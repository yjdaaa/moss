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
            let stages_snapshot = timing.stages.clone();
            self.shared
                .state
                .finish_timing_with_trace(timing, turn.trace.as_ref());

            if turn.kind == TurnKind::Answer {
                let raw_text = turn
                    .debug_raw_asr
                    .clone()
                    .or_else(|| turn.injected_text.clone())
                    .unwrap_or_default();
                let corrected_text = turn
                    .debug_corrected_asr
                    .clone()
                    .unwrap_or_else(|| raw_text.clone());
                // VB1 + VC3 #7：采集真实麦克风上行语音（16 kHz 单声道 WAV + 元数据），包含整体测试时间窗内的外放录音；
                // VA 文字直通注入与 VC2 数字音频对照注入绝不落盘，且绝不进诊断包、过程记录或声纹。
                let mut corpus_date = None;
                let mut corpus_turn_id = None;
                let allow_corpus_save = turn.injected_text.is_none() && !turn.digital_audio;
                if allow_corpus_save
                    && let Some(corpus) = self
                        .shared
                        .config
                        .asr_corpus
                        .clone()
                        .filter(|store| store.is_enabled())
                    && let Some((pcm, sample_rate)) = turn.corpus_audio.take()
                {
                    let date_str = crate::asr_corpus::local_date_from_ms(at_ms);
                    let turn_key = crate::asr_corpus::sanitize_turn_id(self.session_id, turn_no);
                    corpus_date = Some(date_str);
                    corpus_turn_id = Some(turn_key);
                    let asr_model = self
                        .conversation
                        .as_ref()
                        .and_then(|c| {
                            c.settings
                                .model_labels
                                .iter()
                                .find_map(|s| s.strip_prefix("ASR:"))
                        })
                        .unwrap_or_default()
                        .to_owned();
                    let endpoint = crate::asr_corpus::EndpointTiming {
                        vad_start_ms: stages_snapshot.get("vad_start").copied(),
                        vad_end_ms: stages_snapshot.get("vad_end").copied(),
                        utterance_end_ms: stages_snapshot
                            .get("utterance_end")
                            .or_else(|| stages_snapshot.get("interim_stop"))
                            .copied(),
                        final_asr_ms: stages_snapshot.get("final_asr").copied(),
                        endpoint_wait_ms: turn.endpoint_wait.map(|(_, total)| total),
                    };
                    let utterance = crate::asr_corpus::CapturedUtterance {
                        session_id: self.session_id.to_owned(),
                        device_id: self.device_id.to_owned(),
                        turn: turn_no,
                        captured_at_ms: at_ms,
                        simulated: false,
                        bench_window: turn.bench_window,
                        pcm,
                        sample_rate,
                        raw_text: raw_text.clone(),
                        corrected_text: corrected_text.clone(),
                        interims: std::mem::take(&mut turn.corpus_interims),
                        asr_model,
                        vad_engine: self.vad_engine.clone(),
                        endpoint,
                        outcome: outcome.to_owned(),
                    };
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        handle.spawn_blocking(move || {
                            let _ = corpus.record(utterance);
                        });
                    } else {
                        let _ = corpus.record(utterance);
                    }
                } else {
                    turn.corpus_audio = None;
                    turn.corpus_interims.clear();
                }
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
                let stats = self.pump.stats();
                let frames = turn.playback_frames.max(stats.frames);
                let underruns = turn.playback_underruns.max(stats.underruns);
                let stall_ms = turn.playback_stall_ms.saturating_add(
                    turn.underrun_started_at
                        .take()
                        .map_or(0, |since| since.elapsed().as_millis() as u64),
                );
                let route = turn.route.unwrap_or(match outcome {
                    "ignored" => "ignored",
                    "not_for_me" => "not_for_me",
                    _ => "unknown",
                });

                // 先落入 debug_traces，再把 turn_detail 置为终态，避免轮询方看到 turn_detail.status == "done" 时读到尚未写入的对话记录。
                if self.shared.state.debug_enabled()
                    && let Some(captured_epoch) = turn.debug_capture_epoch
                    && let Some(raw_asr) = turn
                        .debug_raw_asr
                        .take()
                        .or_else(|| turn.injected_text.clone())
                {
                    let corrected_asr = turn
                        .debug_corrected_asr
                        .take()
                        .unwrap_or_else(|| raw_asr.clone());
                    if let Some((scrubbed, epoch)) =
                        self.shared.state.finish_debug_trace_with_epoch(
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
                                reply: reply.clone(),
                                spoken: spoken.clone(),
                                outcome: outcome.into(),
                                truncated: false,
                                tools_dropped: 0,
                                simulated: turn.simulated,
                                bench_window: turn.bench_window,
                                input_source: turn.input_source.into(),
                                trace_audio_file: turn.trace_audio_file.clone(),
                            },
                            captured_epoch,
                        )
                    {
                        let registry = self.shared.registry.clone();
                        if let Ok(handle) = tokio::runtime::Handle::try_current() {
                            handle.spawn_blocking(move || {
                                registry.record_chat_turn_at(&scrubbed, epoch)
                            });
                        } else {
                            registry.record_chat_turn_at(&scrubbed, epoch);
                        }
                    }
                }

                let simulated_label = if turn.bench_window {
                    Some("模拟（整体测试）".to_owned())
                } else if turn.simulated {
                    Some("模拟".to_owned())
                } else {
                    None
                };
                self.shared.state.upsert_turn_detail(crate::TurnDetail {
                    turn_id: format!("{}:{turn_no}", self.session_id),
                    device_id: self.device_id.into(),
                    session_id: self.session_id.into(),
                    turn: turn_no,
                    simulated: turn.simulated,
                    audio_channel_open: !turn.device_silent,
                    bench_window: turn.bench_window,
                    simulated_label,
                    digital_audio: turn.digital_audio,
                    input_source: turn.input_source.into(),
                    input_source_label: turn.input_source_label.into(),
                    trace_audio_file: turn.trace_audio_file.clone(),
                    status: outcome.into(),
                    outcome: outcome.into(),
                    route: route.into(),
                    injected_text: raw_text,
                    corrected_text,
                    stages_ms: stages_snapshot,
                    tools: std::mem::take(&mut turn.detail_tools),
                    reply,
                    spoken,
                    playback: crate::PlaybackDetail {
                        frames,
                        underruns,
                        stall_ms,
                    },
                    stop_skipped_reason: turn.stop_skipped_reason.clone(),
                    error: turn.last_error.clone(),
                    started_at_ms: turn.started_at_ms,
                    finished_at_ms: Some(at_ms),
                    speech_began_at_ms: turn.speech_began_at_ms,
                    speech_end_at_ms: turn.speech_end_at_ms,
                    first_audio_frame_at_ms: turn.first_audio_frame_at_ms,
                    peak_uplink_level: turn.peak_uplink_level,
                    corpus_date,
                    corpus_turn_id,
                });
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
        let vad_engine = match shared.config.vad_factory.as_ref() {
            Some(factory) => factory.label(),
            None => crate::audio::ENERGY_VAD_LABEL.to_owned(),
        };
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
            vad_engine,
            vad_fallback_reason: None,
            vad_worker_reported: false,
            vad_engine_traced: false,
            synced_turn: 0,
            pending_listen_stop: false,
            music: shared
                .config
                .music
                .as_ref()
                .map(|_| MusicController::new(permission.clone())),
            music_failed: false,
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
                stop_watch: None,
                gate_next_turn: false,
                paused: None,
                paused_waits_for_model: false,
                last_loud: None,
                last_utterance: None,
                last_utterance_bench_window: false,
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
            memory_history: Vec::new(),
            turn: None,
            carried_photo: None,
            last_verified_person: None,
            vision_host: None,
            vision_token: None,
            device_mcp: None,
            mqtt: false,
            pending_goodbye: None,
            pending_corpus_interims: Vec::new(),
            pending_fast_stop_pcm: None,
            recent_uplink_levels: VecDeque::new(),
            pending_speech_began_at_ms: None,
            pending_speech_end_at_ms: None,
            pending_peak_uplink_level: None,
            digital_feed: None,
            pending_digital_ack: None,
            next_turn_is_digital: false,
            pending_bench_window: false,
            pending_timers: VecDeque::new(),
        })
    }

    /// VC2/VC3 记录上行音频帧音量（0..=100）：同时维护最近 3 秒滑动峰值与本轮拾音峰值。
    pub(super) fn record_uplink_level(&mut self, raw_level: u8) {
        let level = raw_level.min(100);
        let now = Instant::now();
        self.recent_uplink_levels.push_back((now, level));
        while self
            .recent_uplink_levels
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > Duration::from_secs(3))
        {
            self.recent_uplink_levels.pop_front();
        }
        let peak = self
            .recent_uplink_levels
            .iter()
            .map(|(_, v)| *v)
            .max()
            .unwrap_or(level);
        if let Phase::Listening(listening) = &mut self.phase {
            listening.peak_uplink_level = listening.peak_uplink_level.max(level);
        }
        self.shared
            .state
            .set_recent_peak_level(self.session_id, peak);
        self.set_level(level);
    }

    pub(super) fn cancel_digital_feed(&mut self, reason: &str) {
        let had_digital = self.digital_feed.is_some()
            || self.next_turn_is_digital
            || self.pending_digital_ack.is_some();
        self.digital_feed = None;
        self.next_turn_is_digital = false;
        if let Some(tx) = self.pending_digital_ack.take() {
            let _ = tx.send(Err(crate::InjectTurnError::conflict(
                "inject_audio_cancelled",
                reason,
            )));
        }
        if had_digital {
            // 完整拆除数字注入留下的聆听缓冲、流式 ASR 与 VAD 队列，防止残余数字音频在关窗后降级成无门禁真实轮次
            self.pending_listen_stop = false;
            self.listen.hold_check = None;
            self.pending_corpus_interims.clear();
            self.pending_fast_stop_pcm = None;
            self.pending_speech_began_at_ms = None;
            self.pending_speech_end_at_ms = None;
            self.pending_peak_uplink_level = None;
            self.pending_bench_window = false;
            self.vad.reset();
            if let Some(worker) = self.vad_worker.as_mut() {
                worker.reset();
            }
            if matches!(self.phase, Phase::Listening(_)) {
                self.phase = Phase::Idle;
                self.set_state("idle");
            }
        }
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

    pub(super) fn current_turn_silent(&self) -> bool {
        self.turn.as_ref().is_some_and(|turn| turn.device_silent)
    }

    pub(super) async fn send_stt(&mut self, socket: &mut crate::link::Link, text: String) -> bool {
        if self.current_turn_silent() {
            return true;
        }
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
        if self.current_turn_silent() {
            return Ok(());
        }
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
            if let Some(memory) = conversation.memory.as_ref().filter(|_| !extracting)
                && !self.memory_history.is_empty()
            {
                memory.queue_summary(
                    self.device_id,
                    &self.memory_history,
                    trace::background_llm(conversation, self.device_id, "对话记忆摘要"),
                );
            }
            // NV19-03: independent of the NV10 summary; off unless the user enabled it.
            if let Some(extraction) = &conversation.context_extraction
                && !self.memory_history.is_empty()
            {
                extraction.session_closed(
                    &end_ctx,
                    &self.memory_history,
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
        self.cancel_digital_feed("会话结束，数字音频注入中止");
        if let Some(music) = self.music.as_mut() {
            music.cancel(&mut self.pump);
        }
        self.music_failed = false;
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
        self.shared
            .state
            .set_audio_channel_open(self.session_id, socket.audio_open());
        if self.permission.load(std::sync::atomic::Ordering::Acquire) {
            self.conversation_closed();
        }
        self.history.clear();
        self.memory_history.clear();
        self.last_verified_person = None;
        self.event(format!("这一轮对话结束（{reason}），设备保持常连在线"));
        Ok(())
    }

    /// r77 / n144 到点计时提醒播报：设备空闲且未在拾音/放歌时才播，绝不打断正在进行的对话。
    pub(super) async fn deliver_due_timers(
        &mut self,
        socket: &mut crate::link::Link,
    ) -> Result<(), String> {
        if self.pending_timers.is_empty() {
            return Ok(());
        }
        if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
            self.flush_unplayed_timers("设备授权已撤销");
            return Ok(());
        }
        if self.busy()
            || self.listening_occupied()
            || self.music.as_ref().is_some_and(|m| m.active())
        {
            return Ok(());
        }
        let Some((_id, speech)) = self.pending_timers.pop_front() else {
            return Ok(());
        };
        if (self.mqtt && !socket.audio_open()) || self.conversation.is_none() {
            let alert = ServerMessage::Alert {
                status: "计时提醒".into(),
                message: speech.clone(),
                emotion: "happy".into(),
            };
            if !send_json(socket, &alert).await {
                self.shared.state.event_in_flow(
                    EventRecord::warn(
                        DiagCategory::Device,
                        format!(
                            "计时提醒已到期（设备 {} 发送失败，未播报）：{speech}",
                            self.device_id
                        ),
                    )
                    .source(crate::DiagSource::Xiaozhi),
                    device_flow(self.device_id),
                );
                return Err("发送失败".into());
            }
            self.shared.state.event_in_flow(
                EventRecord::info(
                    DiagCategory::Device,
                    format!("计时提醒已到期（已在屏幕提示）：{speech}"),
                )
                .source(crate::DiagSource::Xiaozhi),
                device_flow(self.device_id),
            );
            return Ok(());
        }
        self.shared.state.event_in_flow(
            EventRecord::info(
                DiagCategory::Device,
                format!("计时提醒已到期，正在播报：{speech}"),
            )
            .source(crate::DiagSource::Xiaozhi),
            device_flow(self.device_id),
        );
        self.start_turn(
            TurnInput::Say {
                text: speech,
                close: false,
            },
            TurnKind::Notice,
        );
        Ok(())
    }

    /// 会话断开或授权撤销时，把排队尚未播报的到点提醒写入「事件记录」（来源小智），不静默丢失。
    pub(super) fn flush_unplayed_timers(&mut self, reason: &str) {
        while let Some((_id, speech)) = self.pending_timers.pop_front() {
            self.shared.state.event_in_flow(
                EventRecord::warn(
                    DiagCategory::Device,
                    format!(
                        "计时提醒已到期（设备 {} {reason}，未播报）：{speech}",
                        self.device_id
                    ),
                )
                .source(crate::DiagSource::Xiaozhi),
                device_flow(self.device_id),
            );
        }
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
            self.prune_stalled_device_mcp();
            let mcp_pending_deadline = self.device_mcp.as_ref().and_then(DeviceMcp::next_expiry);
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
            let bench_window_deadline = self
                .shared
                .bench_window_deadline_for_session(self.device_id, self.session_id);
            let digital_feed_deadline = self.digital_feed.as_ref().map(|f| f.next_at);
            // 按顺序检查（biased）：对话任务的事件先于它发出的设备工具调用处理，设备看到的顺序与任务一致
            // （先 stt 再 tools/call）；设备消息先于播放定时器，打断不被积压的播放拍延迟。
            let result = tokio::select! {
                biased;
                _ = shutdown.changed() => return "内置语音服务已停止".into(),
                _ = self.shared.bench_window_changed.notified() => {
                    self.shared.sync_bench_windows();
                    if (self.digital_feed.is_some()
                        || self.next_turn_is_digital
                        || self.pending_digital_ack.is_some())
                        && !self
                            .shared
                            .is_bench_window_active_for_session(self.device_id, self.session_id)
                    {
                        self.cancel_digital_feed("测试时间窗已关闭，数字音频注入中止");
                    }
                    Ok(())
                },
                _ = async {
                    match bench_window_deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.shared.sync_bench_windows();
                    if (self.digital_feed.is_some()
                        || self.next_turn_is_digital
                        || self.pending_digital_ack.is_some())
                        && !self
                            .shared
                            .is_bench_window_active_for_session(self.device_id, self.session_id)
                    {
                        self.cancel_digital_feed("测试时间窗已到期关闭，数字音频注入中止");
                    }
                    Ok(())
                },
                frame = async {
                    match self.vad_worker.as_mut() {
                        Some(worker) => worker.next().await,
                        None => std::future::pending().await,
                    }
                } => self.on_vad_result(socket, frame).await,
                _ = async {
                    match digital_feed_deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => self.tick_digital_feed(socket).await,
                event = next_turn_event(&mut self.turn) => self.on_turn_event(socket, event).await,
                interim = next_listening_interim(&mut self.phase) => self.on_interim(socket, interim).await,
                incoming = socket.recv() => match incoming {
                    Some(Ok(Message::Binary(data))) => self.on_binary(socket, &data).await,
                    Some(Ok(Message::Text(text))) => self.on_text(socket, text.as_str()).await,
                    Some(Ok(Message::Close(_))) | None => return "设备关闭了连接".into(),
                    Some(Ok(_)) => Ok(()),
                    Some(Err(_)) => return "连接出错".into(),
                },
                (paused_at, outcome) = next_listening_hold_check(&mut self.listen.hold_check) => self.on_hold_check_done(socket, paused_at, outcome).await,
                (stop, stop_bench) = next_stop_check(&mut self.listen.stop_check) => self.on_stop_check_done(socket, stop, crate::conversation::FastStopSource::Recheck, stop_bench).await,
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
                    match mcp_pending_deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.prune_stalled_device_mcp();
                    Ok(())
                },
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
            self.sync_trace_turn();
            // n71 / n146 常连模式：先处理退出收尾（发 goodbye、关语音通道），绝不让后台播报或计时提醒
            // 抢在 goodbye 前开启新语音轮次或把到点提醒吞掉；通道关闭后再让计时提醒按待机屏幕 Alert 投递。
            if let Some(reason) = self.pending_goodbye.take()
                && let Err(reason) = self.end_conversation(socket, &reason, true).await
            {
                return reason;
            }
            self.deliver_background(socket);
            if let Err(reason) = self.deliver_due_timers(socket).await {
                return reason;
            }
        }
    }
}
