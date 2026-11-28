//! 对话轮次：发起、事件处理、结束放行、暂停续播与打断。

use super::*;

impl<'a> Session<'a> {
    pub(super) fn start_turn(&mut self, input: TurnInput, kind: TurnKind) {
        let Some(conversation) = self.conversation.clone() else {
            return;
        };
        self.screen.turn_scene = None;
        if matches!(kind, TurnKind::Answer | TurnKind::Goodbye) {
            self.screen.preview_hold = None;
            self.screen.pending_oled_preview = None;
            self.screen.one_shot_badge_url = None;
            self.screen.music_hud = None;
        }
        self.listen.barge.reset();
        self.listen.follow.reset();
        self.listen.last_loud = None;
        self.listen.playback_started = None;
        self.listen.barge_resumed = false;
        let is_fast_stop = matches!(&input, TurnInput::FastStop { .. });
        let is_text_inject = matches!(&input, TurnInput::Text { .. });
        let latched_bench_window = std::mem::take(&mut self.pending_bench_window);
        let in_bench_window = kind == TurnKind::Answer
            && (latched_bench_window
                || self
                    .shared
                    .is_bench_window_active_for_session(self.device_id, self.session_id));
        let digital_audio =
            kind == TurnKind::Answer && std::mem::take(&mut self.next_turn_is_digital);
        let simulated = in_bench_window
            || digital_audio
            || matches!(
                &input,
                TurnInput::Text {
                    simulated: true,
                    ..
                }
            );
        let simulated_label = if in_bench_window {
            Some("模拟（整体测试）".to_owned())
        } else if simulated {
            Some("模拟".to_owned())
        } else {
            None
        };
        // 对暂停中的回答的插话：先只做识别，确认不是附和后才请求大模型
        let hold_llm = self.listen.paused.is_some();
        let silence_ended = std::mem::take(&mut self.listen.gate_next_turn);
        let gated = (silence_ended || hold_llm)
            && kind == TurnKind::Answer
            && matches!(input, TurnInput::Utterance { .. });
        let fresh_history = std::sync::Arc::new(std::sync::Mutex::new(None));
        // 回答轮次都带闸门：抢跑轮次起始关闭；其余起始放行——插话暂停时会话能把它关上，
        // 让被暂停的回答在下一批工具调用前停住
        let gated_answer = kind == TurnKind::Answer && matches!(input, TurnInput::Utterance { .. });
        let tool_latch = std::sync::Arc::new(crate::conversation::ToolLatch::default());
        let command_hint =
            std::sync::Arc::new(std::sync::Mutex::new(self.listen.utterance_keyword.take()));
        let (release, gate) = if gated_answer {
            let (tx, rx) = tokio::sync::watch::channel(!gated);
            (
                Some(tx),
                Some(TurnGate {
                    release: rx,
                    tool_latch: tool_latch.clone(),
                    speculative: gated,
                    hold_llm: gated && hold_llm,
                    fresh_history: fresh_history.clone(),
                    allow_silent: !self.listen.listen_manual
                        && conversation.settings.ignore_bystanders,
                    // 模拟轮次与声纹按人记忆开启时不转后台查询，避免时间窗关闭后播报泄漏进长期记忆
                    allow_background: !simulated && conversation.personal_context.is_none(),
                    command_hint: command_hint.clone(),
                    command_hint_until: self.listen.kws_tail_until.map(Instant::into_std),
                }),
            )
        } else {
            (None, None)
        };
        let tool_latch = gated_answer.then_some(tool_latch);
        self.listen.last_utterance = match &input {
            TurnInput::Utterance { pcm, .. } if silence_ended => Some(pcm.clone()),
            _ => None,
        };
        self.listen.last_utterance_bench_window =
            self.listen.last_utterance.is_some() && in_bench_window;
        // 上一轮还没收尾就被这一轮接上：先把它已合成的语音与结果记下
        self.trace_turn_end("replaced");
        self.turn_number += 1;
        let turn_trace = conversation
            .process_trace
            .as_ref()
            .filter(|t| t.enabled())
            .map(laser_runtime::ProcessTrace::scoped);
        let (input_source, input_source_label) =
            trace::classify_turn_input_source(is_text_inject, digital_audio, in_bench_window);
        let trace_audio_file = self.trace_turn_input(
            &input,
            kind,
            in_bench_window,
            digital_audio,
            turn_trace.as_ref(),
        );
        let trace_tts = turn_trace.as_ref().map(|_| Vec::new());
        let recording_offset =
            if kind == TurnKind::Answer && !matches!(&input, TurnInput::Text { .. }) {
                self.recording_started.elapsed().as_millis() as u64
            } else {
                0
            };
        let injected_text = match &input {
            TurnInput::Text { text, .. } => Some(text.clone()),
            _ => None,
        };
        let corpus_enabled = self
            .shared
            .config
            .asr_corpus
            .as_ref()
            .is_some_and(|store| store.is_enabled());
        // VB1 + VC3 #7：仅真实麦克风上行语音（含整体测试时间窗内的外放录音）可进入 asr-corpus；
        // VA 文字直通与 VC2 服务端数字音频对照注入绝不进入 asr-corpus。
        let allow_corpus_capture =
            kind == TurnKind::Answer && corpus_enabled && !is_text_inject && !digital_audio;
        let corpus_interims = if allow_corpus_capture {
            std::mem::take(&mut self.pending_corpus_interims)
        } else {
            self.pending_corpus_interims.clear();
            Vec::new()
        };
        let fast_stop_pcm = self.pending_fast_stop_pcm.take();
        let corpus_audio = if allow_corpus_capture {
            match &input {
                TurnInput::Utterance {
                    pcm, sample_rate, ..
                } if !pcm.is_empty() => Some((pcm.clone(), *sample_rate)),
                TurnInput::FastStop { .. } => fast_stop_pcm,
                _ => None,
            }
        } else {
            None
        };
        let (speech_began_at_ms, speech_end_at_ms, peak_uplink_level) =
            if kind == TurnKind::Answer && !is_text_inject {
                (
                    self.pending_speech_began_at_ms.take(),
                    self.pending_speech_end_at_ms.take(),
                    self.pending_peak_uplink_level.take(),
                )
            } else {
                self.pending_speech_began_at_ms = None;
                self.pending_speech_end_at_ms = None;
                self.pending_peak_uplink_level = None;
                (None, None, None)
            };
        let started_at_ms = crate::unix_now_ms();
        let timing = if kind == TurnKind::Answer {
            let mut stages = if is_text_inject {
                self.recording_marks.clear();
                BTreeMap::new()
            } else {
                std::mem::take(&mut self.recording_marks)
            };
            let end = if is_fast_stop {
                "interim_stop"
            } else if is_text_inject {
                "text_injected"
            } else {
                "utterance_end"
            };
            stages.insert(end.into(), recording_offset);
            Some(crate::TurnTiming {
                session_id: self.session_id.into(),
                device_id: self.device_id.into(),
                at_ms: 0,
                turn: self.turn_number,
                models: conversation.settings.model_labels.clone(),
                stages,
                tools: Vec::new(),
                outcome: "running".into(),
            })
        } else {
            None
        };
        let context = ToolContext {
            device_id: self.device_id.to_owned(),
            session_id: self.session_id.to_owned(),
        };
        // 2026-10-16：视觉卡片打开“把照片留在对话上下文里”时，把这一张作为多模态用户消息
        // 接进历史；只在会话内存里，随历史截断丢弃，不落盘。
        let mut pending_photo = None;
        let history = if kind == TurnKind::Answer {
            if simulated {
                self.history.clone()
            } else if conversation.personal_context.is_some() {
                if is_fast_stop {
                    Vec::new()
                } else {
                    let photo_msg =
                        if let Some(photo) = self.shared.vision_photos.take(self.device_id) {
                            self.carried_photo = None;
                            Some(photo.into_message())
                        } else {
                            self.carried_photo.take()
                        };
                    if let Some(msg) = photo_msg {
                        pending_photo = Some(msg.clone());
                        vec![msg]
                    } else {
                        Vec::new()
                    }
                }
            } else {
                self.carried_photo = None;
                if let Some(photo) = self.shared.vision_photos.take(self.device_id) {
                    self.history.push(photo.into_message());
                    append_history(
                        &mut self.history,
                        Vec::new(),
                        conversation.settings.history_budget,
                    );
                }
                self.history.clone()
            }
        } else {
            Vec::new()
        };
        let person_hint = if kind == TurnKind::Answer && !simulated {
            if let Some(personal) = &conversation.personal_context {
                let valid = self.last_verified_person.as_ref().is_some_and(|hint| {
                    hint.verified_at.elapsed() <= SHORT_UTTERANCE_CONTINUITY_WINDOW
                        && hint.consecutive_short < MAX_CONSECUTIVE_SHORT_UTTERANCES
                        && personal.permitted(&context, Some(&hint.scope))
                });
                if !valid {
                    self.last_verified_person = None;
                }
            } else {
                self.last_verified_person = None;
            }
            self.last_verified_person
                .as_ref()
                .map(|hint| hint.scope.clone())
        } else {
            None
        };
        let is_report = matches!(input, TurnInput::Report { .. });
        let debug_capture_epoch = self.shared.state.debug_capture_epoch();
        if kind == TurnKind::Answer {
            self.shared
                .state
                .set_latest_turn(self.session_id, self.turn_number);
            let turn_id = format!("{}:{}", self.session_id, self.turn_number);
            self.shared.state.upsert_turn_detail(crate::TurnDetail {
                turn_id: turn_id.clone(),
                device_id: self.device_id.into(),
                session_id: self.session_id.into(),
                turn: self.turn_number,
                simulated,
                bench_window: in_bench_window,
                simulated_label,
                digital_audio,
                input_source: input_source.into(),
                input_source_label: input_source_label.into(),
                trace_audio_file: trace_audio_file.clone(),
                status: "running".into(),
                outcome: "running".into(),
                route: "running".into(),
                injected_text: injected_text.clone().unwrap_or_default(),
                corrected_text: String::new(),
                stages_ms: timing
                    .as_ref()
                    .map(|t| t.stages.clone())
                    .unwrap_or_default(),
                tools: Vec::new(),
                reply: String::new(),
                spoken: String::new(),
                playback: crate::PlaybackDetail::default(),
                stop_skipped_reason: None,
                error: None,
                started_at_ms,
                finished_at_ms: None,
                speech_began_at_ms,
                speech_end_at_ms,
                first_audio_frame_at_ms: None,
                peak_uplink_level,
                corpus_date: None,
                corpus_turn_id: None,
            });
            if digital_audio && let Some(ack_tx) = self.pending_digital_ack.take() {
                let _ = ack_tx.send(Ok(crate::InjectTurnAck {
                    turn_id,
                    device_id: self.device_id.into(),
                    session_id: self.session_id.into(),
                    turn: self.turn_number,
                    simulated,
                }));
            }
        }
        let device = self.device_mcp.as_ref().and_then(DeviceMcp::tool_set);
        let direct_trace = crate::conversation::TurnTrace {
            trace: turn_trace
                .clone()
                .or_else(|| conversation.process_trace.as_ref().map(|t| t.scoped())),
            flow: turn_flow(self.device_id, self.turn_number),
            turn: self.turn_number,
        };
        let live_bench_window: Option<Arc<dyn Fn() -> bool + Send + Sync>> =
            if kind == TurnKind::Answer {
                let gate = self.shared.debug_gate.clone();
                let windows = self.shared.bench_windows.clone();
                let dev_id = self.device_id.to_owned();
                let sid = self.session_id.to_owned();
                Some(Arc::new(move || {
                    gate.is_enabled_valid() && windows.active_for_session(&dev_id, &sid).is_some()
                }))
            } else {
                None
            };
        let (handle, events) = crate::conversation::spawn_turn_traced(
            conversation,
            context,
            input,
            history,
            device,
            person_hint,
            gate,
            Some(direct_trace),
            simulated,
            live_bench_window,
        );
        if let Some(mut replaced) = self.turn.replace(ActiveTurn {
            turn_number: self.turn_number,
            person_scope: None,
            person_verified: false,
            pending_photo,
            timing,
            recording_offset,
            kind,
            started: Instant::now(),
            handle,
            events,
            user: None,
            smart_turn_prob: None,
            spoken: Vec::new(),
            spoken_at: Vec::new(),
            frames_sent: 0,
            command_hint: gated_answer.then_some(command_hint),
            report: is_report,
            debug_capture_epoch,
            debug_raw_asr: None,
            debug_corrected_asr: None,
            debug_tools: Vec::new(),
            debug_text_reply: Vec::new(),
            trace: turn_trace,
            trace_tts,
            pending_messages: Vec::new(),
            resampler: None,
            audio_started: false,
            done: false,
            close: false,
            listen_after: None,
            tools_started: false,
            tts_started_at: None,
            tool_rounds: Vec::new(),
            tool_latch,
            speculative: gated,
            fast_stop: is_fast_stop,
            release,
            fresh_history,
            hold: None,
            endpoint_wait: None,
            released_at: None,
            music_error: None,
            simulated,
            bench_window: in_bench_window,
            digital_audio,
            input_source,
            input_source_label,
            trace_audio_file,
            started_at_ms,
            speech_began_at_ms,
            speech_end_at_ms,
            first_audio_frame_at_ms: None,
            peak_uplink_level,
            injected_text,
            route: None,
            detail_tools: Vec::new(),
            underrun_started_at: None,
            playback_frames: 0,
            playback_underruns: 0,
            playback_stall_ms: 0,
            stop_skipped_reason: None,
            last_error: None,
            corpus_audio,
            corpus_interims,
        }) && !replaced.close
        {
            self.stash_uncommitted_photo(&mut replaced);
        }
        self.phase = Phase::Thinking;
        self.set_state("thinking");
        self.sync_trace_turn();
    }

    /// 回声：把这段话重采样到下行采样率后重新编码播回，验证编解码、重采样与播放节奏。
    pub(super) async fn echo(
        &mut self,
        socket: &mut crate::link::Link,
        pcm: Vec<i16>,
    ) -> Result<(), String> {
        let seconds = pcm.len() as f64 / self.uplink_rate as f64;
        let text = if pcm.is_empty() {
            "回声测试：没有听到声音".to_owned()
        } else {
            format!("回声测试：约 {seconds:.1} 秒")
        };
        if !self.send_stt(socket, text).await {
            return Err("发送失败".into());
        }
        self.tts(socket, TtsState::Start, None).await?;
        if pcm.is_empty() {
            self.last_activity = Instant::now();
            return self.tts(socket, TtsState::Stop, None).await;
        }
        let mut resampler =
            Resampler::new(self.uplink_rate, self.shared.config.downlink_sample_rate);
        self.pump.begin();
        self.listen.playback_started = None;
        self.pump
            .push_marked(&resampler.process(&pcm), "（回声）".into());
        self.pump.finish();
        self.phase = Phase::speaking_after_tts_start();
        self.set_state("speaking");
        Ok(())
    }

    /// Music is a separate audio source, never a TTS sentence or assistant
    /// message in conversation history. Only the trusted catalog opens a song.
    /// 启动异常时返回一句简短口语原因，由会话在本轮收尾后播报给用户。
    async fn start_music(
        &mut self,
        socket: &mut crate::link::Link,
        track_id: String,
        title: String,
    ) -> Result<Option<&'static str>, String> {
        self.music_failed = false;
        if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
            self.event(EventRecord::warn(
                DiagCategory::Audio,
                "音乐请求被拒绝：设备权限已撤销",
            ));
            return Ok(None);
        }
        if self.shared.config.downlink_sample_rate != 24_000 {
            self.event(EventRecord::warn(
                DiagCategory::Audio,
                "音乐请求被拒绝：播放采样率不符合要求",
            ));
            return Ok(Some("当前音频参数不支持放歌"));
        }
        let Some(index) = self.shared.config.music.clone() else {
            self.event(
                EventRecord::warn(DiagCategory::Config, "音乐功能未启用")
                    .hint("在“能力”标签里打开本地音乐并选好音乐文件夹"),
            );
            return Ok(Some("本地音乐还没开启"));
        };
        // No pathname or file descriptor is supplied by the model.
        let opened = tokio::task::spawn_blocking(move || index.open(&track_id)).await;
        let (file, extension) = match opened {
            Ok(Ok(track)) => track,
            _ => {
                self.event(
                    EventRecord::warn(DiagCategory::Audio, "所选歌曲不存在、已变化或不允许读取")
                        .hint("音乐文件可能被移动或删除了：在“能力”标签重新选择音乐文件夹"),
                );
                return Ok(Some("找不到这首歌，请刷新音乐目录"));
            }
        };
        if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(None);
        }
        if self.music.is_none() {
            return Ok(Some("本地音乐还没开启"));
        }
        if matches!(self.phase, Phase::Speaking { .. }) {
            self.stop_speaking(socket, Some("开始新歌曲")).await?;
            if let Some(turn) = self.turn.as_mut() {
                turn.audio_started = false;
                turn.tts_started_at = None;
            }
        }
        let Some(music) = self.music.as_mut() else {
            return Ok(Some("本地音乐还没开启"));
        };
        if music.start(file, &extension, &mut self.pump).is_err() {
            if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(None);
            }
            self.event(EventRecord::error(
                DiagCategory::Audio,
                "本地音乐解码任务未能启动",
            ));
            return Ok(Some("这首歌打不开，换一首试试"));
        }
        self.tts(socket, TtsState::Start, None).await?;
        self.phase = Phase::speaking_after_tts_start();
        self.set_state("speaking");
        // Title is bounded by the filesystem scanner; no absolute path/URL.
        self.event(format!("开始播放本地歌曲：{title}"));
        self.listen.playback_started = None;
        self.listen.barge.reset();
        self.screen.preview_hold = None;
        let want_hud = self
            .conversation
            .as_ref()
            .is_none_or(|c| c.settings.screen_preview_sync);
        if want_hud && self.pack_active() {
            // n70 刷了画风包：全屏唱机 / 听歌小机器人一直显示到停播，不再每 4 秒推半屏卡
            let short_title: String = title.chars().take(48).collect();
            self.set_sticky(
                socket,
                crate::BotPose::MusicGroove,
                Some(format!("[音乐] {short_title}")),
                None,
            )
            .await?;
            return Ok(None);
        }
        let can_color_music = want_hud
            && self.device_mcp.as_ref().is_some_and(|d| {
                d.has_tool("self.screen.preview_image") && d.screen_monochrome != Some(true)
            });
        if can_color_music {
            let ascii_raw: String = title
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == ' ' || *c == '-')
                .take(14)
                .collect::<String>()
                .trim()
                .to_ascii_uppercase();
            let title_ascii = if ascii_raw.is_empty() {
                "LOCAL TRACK".to_owned()
            } else {
                ascii_raw
            };
            let screen_size = self.device_mcp.as_ref().and_then(|d| d.screen_size);
            let (cw, ch) = crate::preview_push::optimal_canvas_for_screen(screen_size);
            let render_title = title_ascii.clone();
            let card_style = self.shared.card_style();
            let png = tokio::task::spawn_blocking(move || {
                crate::preview_push::make_music_card_png(
                    &crate::preview_push::MusicCardSpec {
                        title_ascii: render_title,
                    },
                    card_style,
                    0,
                    cw,
                    ch,
                )
            })
            .await
            .ok()
            .and_then(Result::ok);
            if let Some(png) = png
                && let Ok(tok) = self.shared.preview_store.stage_prebuilt_png(png)
            {
                let host = self.preview_http_host();
                let url = format!(
                    "http://{host}:{}/mcp/preview/{tok}.png",
                    self.shared.http_port
                );
                self.screen.music_hud = Some(MusicHud {
                    title_ascii,
                    frame: 1,
                    next_refresh: Instant::now() + self.shared.config.preview_hold_interval,
                });
                self.send_quiet_device_call(
                    socket,
                    "self.screen.preview_image",
                    json!({ "url": url }),
                )
                .await?;
                let short_title: String = title.chars().take(48).collect();
                let _ = self.send_stt(socket, format!("[音乐] {short_title}")).await;
            }
        }
        Ok(None)
    }

    pub(super) async fn on_turn_event(
        &mut self,
        socket: &mut crate::link::Link,
        event: Option<TurnEvent>,
    ) -> Result<(), String> {
        if self.turn.is_none() {
            return Ok(());
        }
        if self.listen.paused.is_some() {
            // 插话那一轮的第一个结论决定被暂停的回答的去留
            match &event {
                Some(TurnEvent::Heard(text)) if self.is_backchannel_now(text) => {
                    self.resume_paused("只是附和");
                    return Ok(());
                }
                Some(TurnEvent::Ignored) | None => {
                    self.resume_paused("没听出内容");
                    return Ok(());
                }
                Some(TurnEvent::NotForMe) => {
                    self.resume_paused("不是对小智说的");
                    return Ok(());
                }
                // 已识别出文字、正等大模型判断时出错（大模型或合成失败）：用户确实说了话，
                // 不能让原回答接着说而把这句吞掉——走下面的作废原回答、照常报错
                Some(TurnEvent::Failed(error)) if !self.listen.paused_waits_for_model => {
                    self.event(EventRecord::from_provider(
                        EventLevel::Warn,
                        "插话识别出错，继续原回答：",
                        error,
                    ));
                    self.resume_paused("识别失败");
                    return Ok(());
                }
                Some(
                    TurnEvent::RawHeard { .. }
                    | TurnEvent::SmartTurnProb(_)
                    | TurnEvent::Timing { .. }
                    | TurnEvent::TimingOverwrite { .. }
                    | TurnEvent::Route(_)
                    | TurnEvent::StopSkipped(_)
                    | TurnEvent::PersonResolved { .. }
                    | TurnEvent::Trace { .. }
                    | TurnEvent::SpeechSoon
                    | TurnEvent::Warning(_),
                ) => {}
                Some(TurnEvent::Heard(text)) if self.can_ask_whether_addressed() => {
                    // 先不打断：大模型带着“原回答说到哪、被插话打断”的历史判断这句是不是对小智说的，
                    // 它开口（或调用工具）时才作废原回答；判断不是对小智说的就接着说（n100）
                    let chars = text.chars().count();
                    self.offer_provisional_history();
                    self.event(format!(
                        "插话：识别出 {chars} 字，先判断是不是对小智说的（原回答暂停）"
                    ));
                }
                Some(TurnEvent::Heard(text)) => {
                    let chars = text.chars().count();
                    self.abandon_paused_for_barge(socket).await?;
                    self.event(format!("自然插话：识别出 {chars} 字，打断原回答"));
                }
                Some(_) => {
                    let deferred = self.listen.paused_waits_for_model;
                    self.abandon_paused_for_barge(socket).await?;
                    if deferred {
                        self.event("自然插话：是对小智说的，打断原回答");
                    }
                }
            }
        }
        // 任务意外结束（没有发 Done）按“结束、无新消息”处理
        match event.unwrap_or(TurnEvent::Done {
            messages: Vec::new(),
            close: false,
        }) {
            TurnEvent::PersonResolved { scope, verified } => {
                match (&scope, verified) {
                    (Some(resolved), true) => {
                        self.last_verified_person = Some(VerifiedPersonHint {
                            scope: resolved.clone(),
                            verified_at: Instant::now(),
                            consecutive_short: 0,
                        });
                    }
                    (Some(inherited), false) => {
                        if let Some(prev) = self.last_verified_person.as_mut()
                            && prev.scope == *inherited
                        {
                            prev.consecutive_short = prev.consecutive_short.saturating_add(1);
                        } else {
                            self.last_verified_person = None;
                        }
                    }
                    (None, _) => {
                        self.last_verified_person = None;
                    }
                }
                if let Some(turn) = self.turn.as_mut() {
                    turn.person_scope = scope;
                    turn.person_verified = verified;
                }
            }
            TurnEvent::ToolsStarted(calls) => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.tools_started = true;
                    turn.tool_rounds.push((calls, Vec::new()));
                }
            }
            TurnEvent::SpeechSoon => {
                // 已放行（等待期间会话不取事件）、还在思考、没被插话暂停：先发 `tts start`（n115）
                let early = matches!(self.phase, Phase::Thinking)
                    && self.listen.paused.is_none()
                    && !self.music.as_ref().is_some_and(|music| music.active())
                    && self.turn.as_ref().is_some_and(|turn| {
                        !turn.audio_started && !turn.close && turn.tts_started_at.is_none()
                    });
                if early {
                    if let Some(turn) = self.turn.as_mut() {
                        turn.tts_started_at = Some(Instant::now());
                    }
                    self.tts(socket, TtsState::Start, None).await?;
                }
            }
            TurnEvent::ToolsFinished(results) => {
                if let Some((_, slot)) = self
                    .turn
                    .as_mut()
                    .and_then(|turn| turn.tool_rounds.last_mut())
                {
                    *slot = results;
                }
            }
            TurnEvent::Closing => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.close = true;
                }
                self.listen.barge.reset();
            }
            TurnEvent::Route(route) => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.route = Some(route);
                }
            }
            TurnEvent::StopSkipped(reason) => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.stop_skipped_reason = Some(reason.clone());
                }
                self.event(EventRecord::warn(DiagCategory::Device, reason));
            }
            TurnEvent::Timing { stage, ms } => {
                if let Some(turn) = self.turn.as_mut()
                    && let Some(timing) = turn.timing.as_mut()
                {
                    timing
                        .stages
                        .entry(stage.into())
                        .or_insert(turn.recording_offset.saturating_add(ms));
                }
            }
            TurnEvent::TimingOverwrite { stage, ms } => {
                if let Some(turn) = self.turn.as_mut()
                    && let Some(timing) = turn.timing.as_mut()
                {
                    timing
                        .stages
                        .insert(stage.into(), turn.recording_offset.saturating_add(ms));
                }
            }
            TurnEvent::ToolTiming { name, ms } => {
                self.shared
                    .state
                    .set_active_tool(self.session_id, Some(name.clone()));
                if let Some(turn) = self.turn.as_mut()
                    && let Some(timing) = turn.timing.as_mut()
                    && timing.tools.len() < 50
                {
                    // 只保留注册名称，不保存参数或工具结果。
                    timing.tools.push(crate::ToolTiming { name, ms });
                }
            }
            TurnEvent::Trace { kind, data } => self.trace_turn_event(kind, data),
            TurnEvent::SmartTurnProb(prob) => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.smart_turn_prob = Some(prob);
                }
            }
            TurnEvent::RawHeard {
                raw,
                homophone: _,
                corrected,
                keyword: _,
            } => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.debug_raw_asr = Some(raw);
                    turn.debug_corrected_asr = Some(corrected);
                }
            }
            TurnEvent::ToolDebug {
                name,
                arguments,
                result,
                ms,
                blocked,
                block_reason,
            } => {
                if let Some(turn) = self.turn.as_mut() {
                    if turn.detail_tools.len() < 16 {
                        turn.detail_tools.push(crate::TurnToolDetail {
                            name: name.clone(),
                            arguments: arguments.clone(),
                            result: result.clone(),
                            ms,
                            blocked,
                            block_reason: block_reason.clone(),
                        });
                    }
                    if self.shared.state.debug_enabled() && turn.debug_tools.len() < 16 {
                        turn.debug_tools.push(crate::DebugToolTrace {
                            name,
                            arguments,
                            result,
                            blocked,
                            block_reason,
                        });
                    }
                }
            }
            TurnEvent::TextReply(text) => {
                // 合成不可用仍给设备文字，不用静音音频冒充已播报。
                let (audio_started, early_start) =
                    self.turn.as_mut().map_or((false, false), |turn| {
                        turn.debug_text_reply.push(text.clone());
                        (turn.audio_started, turn.tts_started_at.is_some())
                    });
                if audio_started {
                    // 前面的句子还在播：这时发 tts stop 会让设备丢掉排着的音频（n114），只记事件
                    self.event(format!("有一句合成失败，没能播出：{text}"));
                    return Ok(());
                }
                if !early_start {
                    self.tts(socket, TtsState::Start, None).await?;
                }
                if let Some(turn) = self.turn.as_mut() {
                    turn.tts_started_at = None;
                }
                self.tts(socket, TtsState::SentenceStart, Some(text))
                    .await?;
                self.tts(socket, TtsState::Stop, None).await?;
            }
            TurnEvent::Heard(text) => {
                let chars = text.chars().count();
                self.shared
                    .state
                    .set_utterance_chars(self.session_id, u32::try_from(chars).unwrap_or(u32::MAX));
                self.event(format!("识别完成：{chars} 字"));
                let mut held_ms = None;
                let mut decision = None;
                if let Some(turn) = self.turn.as_mut() {
                    turn.user = Some(text.clone());
                    if turn.speculative && turn.hold.is_none() {
                        // 抢跑：任务在静音 BASE_SILENCE_MS 时就开始了；结合句尾词与尾音语义概率决定还要等多久才开口
                        let total =
                            crate::fast::endpoint_wait_ms_with_prosody(&text, turn.smart_turn_prob);
                        turn.endpoint_wait = Some((crate::fast::endpoint_wait_ms(&text), total));
                        let wait = total.saturating_sub(BASE_SILENCE_MS);
                        let now = Instant::now();
                        let until = turn.started + Duration::from_millis(wait);
                        if until > now {
                            turn.hold = Some((until, until + MAX_HOLD_EXTENSION));
                            held_ms = Some(wait);
                        }
                        // 两种结论都记下依据（句尾规则、语义判停概率、还要再等多久）
                        decision = Some(serde_json::json!({
                            "held": until > now,
                            "text_wait_ms": crate::fast::endpoint_wait_ms(&text),
                            "smart_turn_prob": turn.smart_turn_prob,
                            "total_wait_ms": total,
                            "base_silence_ms": BASE_SILENCE_MS,
                            "extra_wait_ms": until.saturating_duration_since(now).as_millis() as u64,
                        }));
                    }
                }
                if let Some(decision) = decision {
                    self.trace_record("hold_decision", decision);
                }
                match held_ms {
                    Some(ms) if ms >= 500 => self.event(format!(
                        "这句话像是还没说完，多等 {ms} ms 再回应（期间接着说会合并）"
                    )),
                    Some(_) => {}
                    None => self.release_turn(),
                }
                if self.screen.last_screen_stt.take().as_deref() != Some(text.as_str())
                    && !self.send_stt(socket, text).await
                {
                    return Err("发送失败".into());
                }
                // n70 听完开始想：全屏画风切到“思考”（插话还在判断、原回答只是暂停时不切）
                if self.listen.paused.is_none() && self.sticky_while_talking().is_none() {
                    self.show_screen(socket, crate::BotPose::Thinking, true)
                        .await?;
                }
            }
            TurnEvent::Background(query) => {
                if self.bg.background.len() >= MAX_BACKGROUND {
                    // 中止最早的那个（丢弃即中止）
                    self.bg.background.remove(0);
                }
                self.bg.background.push(query);
                self.event("慢查询转到后台继续，查好了再主动告诉用户");
            }
            TurnEvent::NotForMe if self.turn.as_ref().is_some_and(|turn| turn.audio_started) => {
                // 防御：已经出声的轮次不能悄悄丢掉（设备还在说话状态、等 tts stop），按说完处理
                if let Some(turn) = self.turn.as_mut() {
                    turn.done = true;
                }
                self.pump.finish();
                if self.listen.playback_started.is_none()
                    && self.pump.ready_to_start()
                    && let Phase::Speaking { next, .. } = &mut self.phase
                {
                    *next = Instant::now();
                }
            }
            TurnEvent::NotForMe => {
                self.finish_timing("not_for_me");
                if let Some(mut turn) = self.turn.take() {
                    self.stash_uncommitted_photo(&mut turn);
                }
                self.screen.last_screen_stt = None;
                self.event("这句不是对小智说的，没有接话，继续拾音");
                if self.listen.listen_manual {
                    self.phase = Phase::Idle;
                    self.set_state("idle");
                } else {
                    self.start_listening(false);
                }
            }
            TurnEvent::Ignored => {
                self.finish_timing("ignored");
                if let Some(mut turn) = self.turn.take() {
                    self.stash_uncommitted_photo(&mut turn);
                }
                self.screen.last_screen_stt = None;
                self.event("没有识别出文字，继续拾音");
                if self.listen.listen_manual {
                    self.phase = Phase::Idle;
                    self.set_state("idle");
                } else {
                    // 自动模式下设备仍在拾音（还没收到 tts start）
                    self.start_listening(false);
                }
            }
            TurnEvent::Emotion(emotion) if self.pack_active() => {
                // n70 刷了画风包：大模型的表情换成当前画风的对应全屏场景
                self.shared.state.set_emotion(self.session_id, emotion);
                let scene = crate::screen_pack::speaking_scene(emotion);
                self.screen.turn_scene = Some(scene);
                // 已经开口就马上换；还没开口的等发 tts start 时一起换
                if self.sticky_while_talking().is_none()
                    && self.turn.as_ref().is_some_and(|t| t.audio_started)
                {
                    self.show_screen(socket, scene, true).await?;
                }
            }
            TurnEvent::Emotion(emotion) => {
                self.shared.state.set_emotion(self.session_id, emotion);
                let message = ServerMessage::Llm {
                    session_id: self.session_id.into(),
                    text: emotion.into(),
                    emotion: emotion.into(),
                };
                if !send_json(socket, &message).await {
                    return Err("发送失败".into());
                }
                let restore_url = self
                    .screen
                    .preview_hold
                    .as_ref()
                    .map(|h| h.url.clone())
                    .or_else(|| self.screen.one_shot_badge_url.take());
                if let Some(url) = restore_url {
                    self.send_quiet_device_call(
                        socket,
                        "self.screen.preview_image",
                        json!({ "url": url }),
                    )
                    .await?;
                }
            }
            TurnEvent::PlayMusic { track_id, title } => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.debug_text_reply
                        .push(format!("[播放本地音乐] {title}"));
                }
                if let Some(reason) = self.start_music(socket, track_id, title).await?
                    && let Some(turn) = self.turn.as_mut()
                {
                    turn.music_error = Some(reason);
                }
            }
            TurnEvent::Speech { text, pcm } => {
                let Some(turn) = self.turn.as_mut() else {
                    return Ok(());
                };
                let first = !turn.audio_started;
                turn.audio_started = true;
                let early_start = turn.tts_started_at;
                if first
                    && self
                        .bg
                        .reporting
                        .as_ref()
                        .is_some_and(|(number, _)| *number == self.turn_number)
                {
                    // 播报已经开口：不再重来
                    self.bg.reporting = None;
                }
                if first && turn.kind == TurnKind::Answer {
                    let latency = turn.started.elapsed().as_millis();
                    self.event(format!("首句延迟：{latency} ms（说完到首句音频就绪）"));
                }
                if first {
                    self.listen.playback_started = None;
                    self.pump.begin();
                    self.phase = match early_start {
                        Some(at) => Phase::speaking_after_tts_start_at(at),
                        None => {
                            self.tts(socket, TtsState::Start, None).await?;
                            Phase::speaking_after_tts_start()
                        }
                    };
                    self.set_state("speaking");
                }
                let downlink = self.shared.config.downlink_sample_rate;
                let mut resampler = Resampler::new(pcm.sample_rate, downlink);
                let samples = resampler.process(&pcm.samples);
                self.trace_tts_push(&samples);
                // n145：整句随首帧下发，超过一行由固件底部单行跑马灯自行滚动（与官方链路一致）
                self.pump.push_marked(&samples, text);
                if let Some(turn) = self.turn.as_mut() {
                    turn.resampler = Some(resampler);
                }
                if self.listen.playback_started.is_none()
                    && self.pump.ready_to_start()
                    && let Phase::Speaking { next, .. } = &mut self.phase
                {
                    *next = Instant::now();
                }
            }
            TurnEvent::SpeechMore(pcm) => {
                let downlink = self.shared.config.downlink_sample_rate;
                let Some(turn) = self.turn.as_mut() else {
                    return Ok(());
                };
                if !turn.audio_started {
                    return Ok(());
                }
                let resampler = turn
                    .resampler
                    .get_or_insert_with(|| Resampler::new(pcm.sample_rate, downlink));
                let samples = resampler.process(&pcm.samples);
                self.pump.push(&samples);
                self.trace_tts_push(&samples);
                if self.listen.playback_started.is_none()
                    && self.pump.ready_to_start()
                    && let Phase::Speaking { next, .. } = &mut self.phase
                {
                    *next = Instant::now();
                }
            }
            TurnEvent::SentenceEnd => {
                if self.turn.as_ref().is_some_and(|t| t.audio_started) {
                    self.pump.mark_sentence_end();
                    if self.listen.playback_started.is_none()
                        && self.pump.ready_to_start()
                        && let Phase::Speaking { next, .. } = &mut self.phase
                    {
                        *next = Instant::now();
                    }
                }
            }
            TurnEvent::Failed(error) => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.last_error = Some(error.to_string());
                }
                self.event(EventRecord::from_provider(
                    EventLevel::Error,
                    "对话出错：",
                    &error,
                ));
            }
            TurnEvent::Warning(error) => {
                self.event(EventRecord::from_provider(EventLevel::Warn, "", &error))
            }
            TurnEvent::Done { messages, close } => {
                if self.music.as_ref().is_some_and(|music| music.active()) {
                    self.finish_timing("music_started");
                    if let Some(mut turn) = self.turn.take()
                        && !close
                        && !turn.close
                    {
                        self.stash_uncommitted_photo(&mut turn);
                    }
                    return Ok(());
                }
                let Some(turn) = self.turn.as_mut() else {
                    return Ok(());
                };
                turn.done = true;
                turn.close = close;
                turn.pending_messages = messages;
                if turn.audio_started {
                    // 播完剩余音频后由 tick 收尾；若此前仍在等待启动缓冲则立即唤醒首帧
                    self.pump.finish();
                    if self.listen.playback_started.is_none()
                        && self.pump.ready_to_start()
                        && let Phase::Speaking { next, .. } = &mut self.phase
                    {
                        *next = Instant::now();
                    }
                } else {
                    // 提前发了 `tts start` 却没出声（合成都失败了）：补发 `tts stop`，设备回到聆听（n115）
                    let early_start = turn.tts_started_at.is_some();
                    if early_start {
                        self.tts(socket, TtsState::Stop, None).await?;
                    }
                    return self.end_turn(early_start).await;
                }
            }
        }
        Ok(())
    }

    /// 一轮对话结束（音频已播完或没有音频）。返回 `Err` 表示结束会话。
    pub(super) async fn end_turn(&mut self, played: bool) -> Result<(), String> {
        self.finish_timing("done");
        let Some(mut turn) = self.turn.take() else {
            return Ok(());
        };
        if turn.kind == TurnKind::Answer
            && let Some(conversation) = &self.conversation
        {
            let messages = std::mem::take(&mut turn.pending_messages);
            if turn.simulated {
                // VA2 #3 模拟轮次仅保留在当前会话内存中支持多轮追问，绝不触发长期记忆、人物资料或用户档案提取。
                append_history(
                    &mut self.history,
                    messages,
                    conversation.settings.history_budget,
                );
            } else {
                let turn_ctx = ToolContext {
                    device_id: self.device_id.into(),
                    session_id: self.session_id.into(),
                };
                if let Some(personal) = &conversation.personal_context {
                    let commit_scope = turn.person_scope.as_ref().filter(|_| turn.person_verified);
                    if let (Some(extraction), Some(verified_person)) =
                        (&conversation.context_extraction, commit_scope)
                    {
                        extraction.turn_completed(
                            &turn_ctx,
                            Some(verified_person),
                            &messages,
                            trace::background_llm(conversation, self.device_id, "长期记忆提取"),
                        );
                    }
                    let photo_in_messages = turn
                        .pending_photo
                        .as_ref()
                        .is_none_or(|photo| messages.contains(photo));
                    let committed = personal.commit(
                        &turn_ctx,
                        commit_scope,
                        messages,
                        conversation.settings.history_budget,
                    );
                    if !(committed && photo_in_messages) && !turn.close {
                        self.stash_uncommitted_photo(&mut turn);
                    }
                } else {
                    if let Some(extraction) = &conversation.context_extraction {
                        extraction.turn_completed(
                            &turn_ctx,
                            None,
                            &messages,
                            trace::background_llm(conversation, self.device_id, "长期记忆提取"),
                        );
                    }
                    append_history(
                        &mut self.memory_history,
                        messages.clone(),
                        conversation.settings.history_budget,
                    );
                    append_history(
                        &mut self.history,
                        messages,
                        conversation.settings.history_budget,
                    );
                }
            }
        }
        self.listen.playback_started = None;
        self.phase = Phase::Idle;
        self.set_state("idle");
        self.last_activity = Instant::now();
        if turn.close {
            if played {
                // tick 已等到预计播完才发 tts stop，这里只留设备本地播放队列的排空余量
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            let reason = match turn.kind {
                TurnKind::Goodbye => format!(
                    "{} 秒没有对话，已说再见",
                    self.shared.config.idle_timeout.as_secs()
                ),
                _ => "用户说了退出词".into(),
            };
            if self.mqtt {
                self.pending_goodbye = Some(reason);
                return Ok(());
            }
            return Err(reason);
        }
        if let Some(manual) = turn.listen_after {
            self.start_listening(manual);
        } else if let Some(speech) = turn.music_error.take().filter(|_| {
            self.permission.load(std::sync::atomic::Ordering::Acquire)
                && self.conversation.is_some()
        }) {
            self.start_turn(
                TurnInput::Say {
                    text: speech.into(),
                    close: false,
                },
                TurnKind::Notice,
            );
        } else if self.listen.listen_realtime
            || (!played && turn.kind == TurnKind::Answer && !self.listen.listen_manual)
        {
            // realtime 固件一直上传声音，不保证再发 listen start；无音频的 auto 模式也仍在拾音
            self.start_listening(false);
        }
        Ok(())
    }

    /// 认定用户这句话说完了：放行对话任务的工具调用，并开始处理它的输出。
    fn release_turn(&mut self) {
        if let Some(turn) = self.turn.as_mut() {
            if turn.speculative {
                turn.released_at = Some(Instant::now());
            }
            turn.hold = None;
            turn.speculative = false;
            if let Some(release) = &turn.release {
                let _ = release.send(true);
            }
        }
    }

    /// 抢跑等待到点：用户此刻没在出声（或已到最晚时刻）就放行，否则稍后再看。
    pub(super) fn check_release(&mut self) {
        let now = Instant::now();
        let Some((until, latest)) = self.turn.as_ref().and_then(|turn| turn.hold) else {
            return;
        };
        if now < until {
            return;
        }
        let speaking = self
            .listen
            .last_loud
            .is_some_and(|at| now.duration_since(at) < Duration::from_millis(250));
        if speaking && now < latest {
            if let Some(turn) = self.turn.as_mut() {
                turn.hold = Some((now + Duration::from_millis(80), latest));
            }
            return;
        }
        self.release_turn();
    }

    /// 被打断的这一轮执行过的工具，按“调用 + 结果”补记进历史（n114）：工具在后台线程里跑、打断停不下它，
    /// 不记的话大模型不知道预览已生成、任务已提交，用户问“开始了吗”时可能再执行一遍。
    /// 还没拿到结果的写 [`INTERRUPTED_TOOL_RESULT`]；点歌不记（工具参数与路径不进历史，与正常结束一致）。
    fn interrupted_tool_messages(turn: &ActiveTurn) -> Vec<ChatMessage> {
        let mut messages = Vec::new();
        for (calls, results) in &turn.tool_rounds {
            if calls.iter().any(|call| call.function.name == "play_music") {
                continue;
            }
            messages.push(ChatMessage::assistant_tools(None, calls.clone()));
            for call in calls {
                let result = results.iter().find(|(id, _)| *id == call.id).map_or(
                    crate::conversation::INTERRUPTED_TOOL_RESULT,
                    |(_, result)| result.as_str(),
                );
                messages.push(ChatMessage::tool(call.id.clone(), result));
            }
        }
        messages
    }

    /// 这一轮回答里用户实际听到的部分（按发送时间轴估算，见 [`heard`]），供打断时记入历史。
    fn heard_text(&self, turn: &ActiveTurn) -> Option<String> {
        let unplayed = match &self.phase {
            Phase::Speaking { anchor, sent, .. } => {
                let until = *anchor + Duration::from_millis(FRAME_MS) * *sent;
                let ahead = until.saturating_duration_since(Instant::now()).as_millis() as u64;
                u32::try_from(ahead.div_ceil(FRAME_MS)).unwrap_or(u32::MAX)
            }
            // 暂停或思考中：设备早已把收到的帧播完
            _ => 0,
        };
        let (tail, complete, pending) = if turn.audio_started {
            (
                self.pump.frames_before_next_mark(),
                self.pump.current_sentence_complete() || turn.done,
                self.pump.pending_marks(),
            )
        } else {
            (0, true, Vec::new())
        };
        heard::history_text(&heard::heard_reply(
            &turn.spoken,
            &turn.spoken_at,
            turn.frames_sent,
            unplayed,
            tail,
            complete,
            &pending,
        ))
    }

    /// 插话识别出来的只是附和（且原回答还剩不少没说）：不打断。
    /// 原回答已经说到最后一秒时，“好的”“是的”更可能是在回答它的提问，按正常的一句话处理。
    fn is_backchannel_now(&self, text: &str) -> bool {
        crate::fast::is_backchannel(text)
            && !(self.listen.paused.as_ref().is_some_and(|turn| turn.done)
                && self.pump.buffered_samples() < self.shared.config.downlink_sample_rate as usize)
    }

    /// 后台查询查完了：结果排队等空闲播报（n100）。
    pub(super) fn background_done(
        &mut self,
        question: String,
        outcomes: &[(
            moss_voice_providers::ToolCall,
            crate::conversation::ToolOutcome,
        )],
    ) {
        let results = crate::conversation::background_results_text(outcomes);
        if results.trim().is_empty() {
            return;
        }
        self.event("后台查询完成，等空闲时播报");
        self.bg.background_ready.push_back(BackgroundReport {
            question,
            results,
            done_at: Instant::now(),
            attempts: 0,
        });
    }

    /// 等用户安静多久后再看能不能播报（没有待播结果、或正在对话时由对话事件唤醒，不需要定时）。
    pub(super) fn background_report_deadline(&self, socket: &crate::link::Link) -> Option<Instant> {
        if self.bg.background_ready.is_empty() || self.busy() || (self.mqtt && !socket.audio_open())
        {
            return None;
        }
        let quiet_from = self
            .bg
            .last_heard_loud
            .map_or(self.last_activity, |loud| loud.max(self.last_activity));
        Some((quiet_from + QUIET_BEFORE_REPORT).max(Instant::now() + Duration::from_millis(200)))
    }

    /// 有查完的后台结果、此刻没在对话、用户也安静了一会儿：主动开口播报（像加工完工播报一样插进来）。
    pub(super) fn deliver_background(&mut self, socket: &crate::link::Link) {
        // 上一次播报还没开口就被用户的话取代（或没说出来）：放回队列重来
        if self
            .bg
            .reporting
            .as_ref()
            .is_some_and(|(turn, _)| self.turn.is_none() || *turn != self.turn_number)
            && let Some((_, mut report)) = self.bg.reporting.take()
        {
            report.attempts += 1;
            if report.attempts < MAX_REPORT_ATTEMPTS {
                self.bg.background_ready.push_front(report);
            }
        }
        self.bg
            .background_ready
            .retain(|report| report.done_at.elapsed() < BACKGROUND_REPORT_TTL);
        let quiet_from = self
            .bg
            .last_heard_loud
            .map_or(self.last_activity, |loud| loud.max(self.last_activity));
        if self.bg.background_ready.is_empty()
            || self.busy()
            || self.actively_speaking_user()
            || quiet_from.elapsed() < QUIET_BEFORE_REPORT
            || !self.permission.load(std::sync::atomic::Ordering::Acquire)
            || (self.mqtt && !socket.audio_open())
        {
            return;
        }
        let Some(report) = self.bg.background_ready.pop_front() else {
            return;
        };
        self.event("播报后台查询结果");
        self.start_turn(
            TurnInput::Report {
                question: report.question.clone(),
                results: report.results.clone(),
            },
            TurnKind::Answer,
        );
        self.bg.reporting = Some((self.turn_number, report));
    }

    /// 插话识别出来后能否先让大模型判断是不是对小智说的（不能时立即打断原回答，与 n100 之前一致）：
    /// 自动 / 实时拾音才有“不是对我说的”选项；声纹按人记忆开启时历史按人提交，仍立即打断。
    fn can_ask_whether_addressed(&self) -> bool {
        !self.listen.listen_manual
            && self.conversation.as_ref().is_some_and(|conversation| {
                conversation.personal_context.is_none() && conversation.settings.ignore_bystanders
            })
    }

    /// 交给插话这一轮的临时历史：原回答按“用户实际听到的部分 + 没说出口的”记上，但还不写进会话历史——
    /// 大模型判断不是对小智说的、原回答接着说时，会话历史保持原样。
    fn offer_provisional_history(&mut self) {
        let Some(paused) = self.listen.paused.as_ref() else {
            return;
        };
        let mut history = self.history.clone();
        if paused.kind == TurnKind::Answer
            && let Some(user) = paused.user.clone()
        {
            history.push(ChatMessage::user(user));
            history.extend(Self::interrupted_tool_messages(paused));
            if let Some(heard) = self.heard_text(paused) {
                history.push(ChatMessage::assistant(heard));
            }
        }
        if let Some(turn) = self.turn.as_ref()
            && let Ok(mut slot) = turn.fresh_history.lock()
        {
            *slot = Some(history);
        }
        self.listen.paused_waits_for_model = true;
    }

    /// 插话确定是对小智说的：作废被暂停的回答（已播部分记入历史），把最新历史交给插话这一轮。
    async fn abandon_paused_for_barge(
        &mut self,
        socket: &mut crate::link::Link,
    ) -> Result<(), String> {
        let deferred = std::mem::take(&mut self.listen.paused_waits_for_model);
        self.abandon_paused(socket, "自然插话").await?;
        if !deferred {
            // 被打断的回答刚记入历史：把最新历史或尚未提交给任何人的照片交给等放行的这一轮
            self.sync_fresh_history_after_abandon_paused();
        }
        // 某些固件在 tts stop 后再发一次 listen start：不能把刚开始的这一轮打断
        self.listen.barge_resumed = true;
        Ok(())
    }

    /// 被暂停的回答接着说（插话只是附和、没听出内容，或不是对小智说的）。
    fn resume_paused(&mut self, why: &str) {
        self.listen.paused_waits_for_model = false;
        let Some(paused) = self.listen.paused.take() else {
            return;
        };
        self.finish_timing("ignored");
        if let Some(mut abandoned) = self.turn.replace(paused) {
            self.stash_uncommitted_photo(&mut abandoned);
        }
        self.listen.hold_check = None;
        self.pending_listen_stop = false;
        if let Some(worker) = self.vad_worker.as_mut() {
            worker.reset();
        }
        self.listen.barge.reset();
        self.listen.playback_started = None;
        if let Some(release) = self.turn.as_ref().and_then(|turn| turn.release.as_ref()) {
            let _ = release.send(true);
        }
        self.phase = Phase::speaking_resumed();
        self.set_state("speaking");
        self.sync_trace_turn();
        self.event(format!("插话{why}，接着说"));
    }

    /// 被暂停的回答确定作废：按打断处理（发 tts stop、丢弃未播音频、已播的句子记入历史），
    /// 当前这一轮与阶段保持不变。
    pub(super) async fn abandon_paused(
        &mut self,
        socket: &mut crate::link::Link,
        reason: &str,
    ) -> Result<(), String> {
        // 被暂停的回答一旦作废，“等大模型判断插话是不是对小智说的”也随之结束（审查修正：不留残值）
        self.listen.paused_waits_for_model = false;
        let Some(paused) = self.listen.paused.take() else {
            return Ok(());
        };
        let current = self.turn.replace(paused);
        let phase = std::mem::replace(&mut self.phase, Phase::speaking_resumed());
        let result = self.interrupt_current(socket, reason).await;
        self.turn = current;
        self.phase = phase;
        self.set_state(self.phase.label());
        result
    }

    /// 打断：中止对话任务、丢弃未播音频；已识别的原话与已播的句子记入历史。
    pub(super) async fn interrupt(
        &mut self,
        socket: &mut crate::link::Link,
        reason: &str,
    ) -> Result<(), String> {
        self.listen.barge_resumed = false;
        self.abandon_paused(socket, reason).await?;
        self.interrupt_current(socket, reason).await
    }

    async fn interrupt_current(
        &mut self,
        socket: &mut crate::link::Link,
        reason: &str,
    ) -> Result<(), String> {
        if self.turn.is_some() {
            self.trace_record("interrupt", serde_json::json!({"reason": reason}));
        }
        // 提前发了 `tts start`、还没出声就被打断：设备已切到说话状态，要补发 `tts stop`（n115）
        let early_start = self
            .turn
            .as_ref()
            .is_some_and(|turn| turn.tts_started_at.is_some() && !turn.audio_started);
        self.finish_timing("interrupted");
        self.screen.preview_hold = None;
        self.screen.pending_oled_preview = None;
        self.screen.one_shot_badge_url = None;
        self.screen.music_hud = None;
        if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
            self.carried_photo = None;
        } else if let Some(personal) = self
            .conversation
            .as_ref()
            .and_then(|c| c.personal_context.as_ref())
        {
            let ctx = ToolContext {
                device_id: self.device_id.to_owned(),
                session_id: self.session_id.to_owned(),
            };
            if self
                .last_verified_person
                .as_ref()
                .is_some_and(|hint| !personal.permitted(&ctx, Some(&hint.scope)))
            {
                self.last_verified_person = None;
            }
        }
        if let Some(music) = self.music.as_mut()
            && music.active()
        {
            music.cancel(&mut self.pump);
        }
        self.music_failed = false;
        if let Some(mut turn) = self.turn.take() {
            if turn.kind == TurnKind::Answer
                && let Some(user) = turn.user.clone()
                && let Some(conversation) = &self.conversation
            {
                let mut messages = Vec::with_capacity(3);
                if let Some(photo) = &turn.pending_photo {
                    messages.push(photo.clone());
                }
                messages.push(ChatMessage::user(user));
                messages.extend(Self::interrupted_tool_messages(&turn));
                // 只记用户实际听到的部分，没说出口的另外标出（n100）
                if let Some(heard) = self.heard_text(&turn) {
                    messages.push(ChatMessage::assistant(heard));
                }
                if turn.simulated {
                    // VA2 #3 被打断的模拟轮次同样不提交给按人记忆或长期记忆
                    append_history(
                        &mut self.history,
                        messages,
                        conversation.settings.history_budget,
                    );
                } else if let Some(personal) = &conversation.personal_context {
                    let commit_scope = turn.person_scope.as_ref().filter(|_| turn.person_verified);
                    let committed = personal.commit(
                        &ToolContext {
                            device_id: self.device_id.into(),
                            session_id: self.session_id.into(),
                        },
                        commit_scope,
                        messages,
                        conversation.settings.history_budget,
                    );
                    if committed {
                        turn.pending_photo = None;
                    } else if !turn.close {
                        self.stash_uncommitted_photo(&mut turn);
                    }
                } else {
                    append_history(
                        &mut self.memory_history,
                        messages.clone(),
                        conversation.settings.history_budget,
                    );
                    append_history(
                        &mut self.history,
                        messages,
                        conversation.settings.history_budget,
                    );
                }
            } else if turn.report
                && let Some(conversation) = &self.conversation
                && conversation.personal_context.is_none()
                && let Some(heard) = self.heard_text(&turn)
            {
                // 主动播报被打断：没有用户原话，只把播出来的部分（与没说出口的）记下，用户接着问时有上下文
                append_history(
                    &mut self.history,
                    vec![ChatMessage::assistant(heard)],
                    conversation.settings.history_budget,
                );
            } else if !turn.close {
                self.stash_uncommitted_photo(&mut turn);
            }
        }
        match self.phase {
            Phase::Speaking { .. } => self.stop_speaking(socket, Some(reason)).await,
            Phase::Thinking => {
                self.event(format!("对话被打断（{reason}）"));
                self.phase = Phase::Idle;
                self.set_state("idle");
                if early_start {
                    self.tts(socket, TtsState::Stop, None).await?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}
