//! 设备上行消息分发：音频帧、文本消息、接管与空闲超时。

use super::*;

impl<'a> Session<'a> {
    /// 思考期间是否继续听“用户接着说”：还没开口、没开始执行工具的普通回答轮次。
    pub(super) fn may_follow_up(&self) -> bool {
        matches!(self.phase, Phase::Thinking)
            && !self.listen.listen_manual
            && self.listen.paused.is_none()
            // 主动播报轮次没有前半句，但播报还没开口时用户说话也要接住（自动模式下设备此刻仍在上传）
            && (self.listen.last_utterance.is_some() || self.turn.as_ref().is_some_and(|turn| turn.report))
            && self.turn.as_ref().is_some_and(|turn| {
                turn.kind == TurnKind::Answer
                    && !turn.close
                    && !turn.fast_stop
                    && !turn.audio_started
                    && !turn.tools_started
                    && !turn
                        .tool_latch
                        .as_ref()
                        .is_some_and(|latch| latch.tools_started())
            })
    }

    pub(super) async fn on_binary(
        &mut self,
        socket: &mut crate::link::Link,
        data: &[u8],
    ) -> Result<(), String> {
        let listening = matches!(self.phase, Phase::Listening(_));
        let may_interrupt = self.shared.config.barge_in
            && self.listen.listen_realtime
            && matches!(self.phase, Phase::Thinking | Phase::Speaking { .. })
            && !self.turn.as_ref().is_some_and(|turn| turn.fast_stop)
            && (self.turn.as_ref().is_some_and(|turn| {
                matches!(turn.kind, TurnKind::Answer | TurnKind::Notice) && !turn.close
            }) || self.music.as_ref().is_some_and(|music| music.active()));
        // 官方服务端在“思考”期间照常收音，新的话会取代还没开口的旧回答。自动 / 实时拾音下设备此时
        // 仍在上传（收到 tts start 才停），这里同样继续听：话说一半停顿被判停后接着说的后半句不再丢。
        // 还没开播所以没有回声问题，不依赖“自然插话”开关；已开始执行工具的轮次除外。
        let may_follow_up = self.may_follow_up();
        if !may_follow_up {
            self.listen.follow_probe = None;
        }
        // 工具已经开跑、这时接着说不能再整轮重来，又没开实时插话：上行音频照样听着，只认“停”（n114）。
        // 原先这段时间的音频全部丢掉，工具没跑完前说“停”没有反应
        let stop_watch = !listening && !may_interrupt && !may_follow_up && self.watches_for_stop();
        if !stop_watch {
            self.listen.stop_watch = None;
        }
        // 一段话刚说完：尾音继续送进判停流收命令词结果（确认窗口在用判停流时让给它）
        let kws_tail = !listening
            && self.listen.follow_probe.is_none()
            && self.vad_worker.is_some()
            && self
                .listen
                .kws_tail_until
                .is_some_and(|until| Instant::now() < until);
        if !listening && !may_interrupt && !may_follow_up && !kws_tail && !stop_watch {
            return Ok(());
        }
        let decoded = decode_frame(self.version, data).map_err(|error| error.to_string());
        let pcm = match decoded.and_then(|frame| self.decoder.decode(&frame.payload)) {
            Ok(pcm) => pcm,
            Err(error) => {
                self.decode_errors += 1;
                if self.decode_errors == 1 {
                    self.event(format!(
                        "上行音频无法解码（已忽略，之后同类错误不再记录）：{error}"
                    ));
                }
                return Ok(());
            }
        };
        if listening && frame_rms(&pcm) >= self.shared.config.vad.start_rms {
            self.bg.last_heard_loud = Some(Instant::now());
        }
        if kws_tail && let Some(worker) = self.vad_worker.as_mut() {
            worker.submit(pcm.clone(), false)?;
            if !may_interrupt && !may_follow_up && !stop_watch {
                return Ok(());
            }
        }
        if stop_watch {
            if let Some((head, remaining)) = self.listen.stop_watch.as_mut() {
                head.extend_from_slice(&pcm);
                *remaining = remaining.saturating_sub(pcm.len());
                if *remaining == 0
                    && let Some((head, _)) = self.listen.stop_watch.take()
                {
                    self.listen.follow.reset();
                    self.check_for_stop_word(head);
                }
            } else if let Some(head) = self.listen.follow.accept(&pcm, false) {
                self.event("工具执行中听到人声：听听是不是“停”".to_owned());
                let wanted = self.uplink_rate as usize * FOLLOW_CONFIRM.as_millis() as usize / 1000;
                self.listen.stop_watch = Some((head, wanted));
            }
            return Ok(());
        }
        if !listening {
            let guarded = matches!(self.phase, Phase::Speaking { .. })
                && self
                    .listen
                    .playback_started
                    .is_none_or(|at| at.elapsed() < Duration::from_millis(300));
            let detected = if may_follow_up {
                if frame_rms(&pcm) >= self.shared.config.vad.start_rms {
                    self.listen.last_loud = Some(Instant::now());
                }
                if let Some(probe) = self.listen.follow_probe.as_mut() {
                    // 确认中：后续帧接着交给判停模型，结果在 on_vad_result 里看
                    probe.head.extend_from_slice(&pcm);
                    let expired = Instant::now() >= probe.until;
                    if let Some(worker) = self.vad_worker.as_mut() {
                        worker.submit(pcm, true)?;
                    }
                    if expired && let Some(probe) = self.listen.follow_probe.take() {
                        self.listen.follow.reset();
                        self.trace_record(
                            "follow_probe",
                            serde_json::json!({"phase": "not_voice"}),
                        );
                        self.event("思考中听到的声响，判停模型认为不是人声：照常回答".to_owned());
                        self.check_for_stop_word(probe.head);
                    }
                    return Ok(());
                }
                let detected = self.listen.follow.accept(&pcm, false);
                if let (Some(head), Some(worker)) = (detected.as_ref(), self.vad_worker.as_mut()) {
                    // 用户像是又开口了：照常重置判停流再补送开头（判停模型才能报出“开始说话”确认接着说），
                    // 刚说完那句的尾音也不再送（新的话会走新的判停与命令词检测）
                    worker.reset();
                    self.listen.kws_tail_until = None;
                    worker.submit(head.clone(), true)?;
                    self.listen.follow_probe = Some(FollowProbe {
                        head: head.clone(),
                        until: Instant::now() + FOLLOW_CONFIRM,
                    });
                    self.trace_record(
                        "follow_probe",
                        serde_json::json!({
                            "phase": "start",
                            "head_ms": head.len() as u64 * 1000 / u64::from(self.uplink_rate.max(1)),
                            "confirm_ms": FOLLOW_CONFIRM.as_millis() as u64,
                        }),
                    );
                    return Ok(());
                }
                detected
            } else {
                self.listen.barge.accept(&pcm, guarded)
            };
            let Some(head) = detected else {
                return Ok(());
            };
            return self.take_over(socket, head, may_follow_up).await;
        }
        self.set_level(crate::audio::pcm_rms_level(&pcm));
        if !self.listen.listen_manual
            && let Some(worker) = self.vad_worker.as_mut()
        {
            worker.submit(pcm, false)?;
            return Ok(());
        }
        match self.hear(&pcm, None) {
            Some((utterance, end)) => self.finish_or_hold_utterance(socket, utterance, end).await,
            None => Ok(()),
        }
    }

    /// 判停模型没认出人声的声音仍识别一遍：短促的“停”可能短于模型判开口所需的最短语音（约 250 ms），
    /// 不能因此被吞掉、让准备好的回答（与它的工具）照常执行。只在识别出停止说法时才接管。
    /// 这一轮在执行工具、还没开口：设备自动拾音时仍在上传，这段时间要听着“停”（n114）
    fn watches_for_stop(&self) -> bool {
        matches!(self.phase, Phase::Thinking)
            && self.conversation.is_some()
            && self.turn.as_ref().is_some_and(|turn| {
                turn.kind == TurnKind::Answer
                    && !turn.close
                    && !turn.fast_stop
                    && !turn.audio_started
                    && (turn.tools_started
                        || turn
                            .tool_latch
                            .as_ref()
                            .is_some_and(|latch| latch.tools_started()))
            })
    }

    fn check_for_stop_word(&mut self, head: Vec<i16>) {
        let Some(conversation) = self.conversation.clone() else {
            return;
        };
        if let Some(task) = self.listen.stop_check.take() {
            task.abort();
        }
        let rate = self.uplink_rate;
        self.listen.stop_check = Some(tokio::spawn(async move {
            let text = conversation.asr.transcribe(&head, rate).await.ok()?;
            let text = crate::conversation::apply_asr_corrections(
                &text,
                &conversation.settings.asr_corrections,
            );
            crate::fast::is_stop(&text).then_some(text)
        }));
    }

    /// 停止说法识别完了：打断当前回答（无论是否已开口、已执行工具），按急停快路径执行（与流式中间结果命中“停”一致）。
    pub(super) async fn on_stop_check_done(
        &mut self,
        socket: &mut crate::link::Link,
        stop: Option<String>,
        source: crate::conversation::FastStopSource,
    ) -> Result<(), String> {
        let Some(text) = stop else {
            return Ok(());
        };
        // 停止说法绝不吞掉（AGENTS.md）：识别回来时回答可能已经开口或开始执行工具，照样打断它、走急停快路径；
        // 识别晚到时用户也确实说过“停”，宁可多停一次。只有正在执行的就是急停轮次时不重复。
        if self.turn.as_ref().is_some_and(|turn| turn.fast_stop) {
            return Ok(());
        }
        self.event("思考中听到“停”：打断当前回答，执行停止");
        if self.turn.is_some() || self.listen.paused.is_some() {
            self.interrupt(socket, "用户说停").await?;
        }
        self.start_turn(TurnInput::FastStop { text, source }, TurnKind::Answer);
        Ok(())
    }

    /// n112 判停复盘：上一句已被判停、还没开口或已经在回答时用户又出声了，记下当时的判停结论与停顿长短。
    /// 停顿 = 判停静音 [`BASE_SILENCE_MS`] + 抢跑开始到现在 − 开头里已经是人声的部分（开头带最多 1 秒的预录，
    /// 前面安静的那段不算），是估算值。
    fn trace_user_continued(
        &self,
        head: &[i16],
        thinking: bool,
        may_follow_up: bool,
        merged: bool,
        since_playback_ms: Option<u128>,
    ) {
        let Some(turn) = self
            .turn
            .as_ref()
            .filter(|turn| turn.kind == TurnKind::Answer && !turn.report)
        else {
            return;
        };
        if self.trace_handle().is_none() {
            return;
        }
        let rate = u64::from(self.uplink_rate.max(1));
        let head_ms = head.len() as u64 * 1000 / rate;
        let window = (self.uplink_rate as usize / 50).max(1);
        let start_rms = self.shared.config.vad.start_rms;
        let quiet = head
            .chunks(window)
            .take_while(|w| frame_rms(w) < start_rms)
            .map(<[i16]>::len)
            .sum::<usize>();
        let voiced_ms = head.len().saturating_sub(quiet) as u64 * 1000 / rate;
        let since_turn_ms = turn.started.elapsed().as_millis() as u64;
        let (text_wait_ms, total_wait_ms) = turn.endpoint_wait.unzip();
        self.trace_record(
            "user_continued",
            serde_json::json!({
                "phase": if thinking { "thinking" } else { "speaking" },
                "merged": merged,
                "too_late": may_follow_up && !merged,
                "text": turn.user,
                "text_wait_ms": text_wait_ms,
                "total_wait_ms": total_wait_ms,
                "released": turn.released_at.is_some(),
                "since_release_ms": turn.released_at.map(|at| at.elapsed().as_millis() as u64),
                "since_playback_ms": since_playback_ms.map(|ms| ms as u64),
                "head_ms": head_ms,
                "voiced_head_ms": voiced_ms,
                "pause_estimate_ms": (BASE_SILENCE_MS + since_turn_ms).saturating_sub(voiced_ms),
            }),
        );
    }

    /// 播放中插话或思考中接着说：接管这一轮，带着开头（思考中还拼上前半句）回到聆听。
    pub(super) async fn take_over(
        &mut self,
        socket: &mut crate::link::Link,
        head: Vec<i16>,
        may_follow_up: bool,
    ) -> Result<(), String> {
        let elapsed = self
            .listen
            .playback_started
            .map(|at| at.elapsed().as_millis());
        let thinking = matches!(self.phase, Phase::Thinking);
        // 思考中接着说：前半句不单独记入历史，与后半句拼成一段重新识别
        // 作废前先抢工具执行闩：任务刚好已经开始执行工具时不能重来（否则同一个工具会执行两遍），
        // 这段话按普通的后续语音处理
        let abandoned = may_follow_up
            && self
                .turn
                .as_ref()
                .and_then(|turn| turn.tool_latch.as_ref())
                .is_none_or(|latch| latch.try_abandon());
        self.trace_user_continued(&head, thinking, may_follow_up, abandoned, elapsed);
        if may_follow_up && !abandoned {
            return Ok(());
        }
        let earlier = if abandoned {
            if let Some(turn) = self.turn.as_mut() {
                turn.user = None;
            }
            self.listen.last_utterance.take()
        } else {
            None
        };
        let follow_up = earlier.is_some();
        // 回答正在播：先暂停，等插话识别出来再定——真有话才打断，一声“嗯”或噪声就接着说
        let pause = !follow_up
            && matches!(self.phase, Phase::Speaking { .. })
            && self.listen.paused.is_none()
            && !self.music.as_ref().is_some_and(|music| music.active())
            && self.turn.is_some();
        if pause {
            self.listen.paused = self.turn.take();
            // 关上被暂停回答的闸门：插话识别出来之前它不能再发起新的工具调用
            if let Some(release) = self
                .listen
                .paused
                .as_ref()
                .and_then(|turn| turn.release.as_ref())
            {
                let _ = release.send(false);
            }
            self.pump.pause();
            self.listen.playback_started = None;
        } else {
            self.interrupt(
                socket,
                if follow_up {
                    "思考中用户接着说"
                } else {
                    "自然插话"
                },
            )
            .await?;
            if thinking && !follow_up {
                self.tts(socket, TtsState::Stop, None).await?;
            }
        }
        self.start_listening(false);
        // 暂停与思考中接着说都没有发 tts stop，设备不会重发 listen start
        self.listen.barge_resumed = !follow_up && !pause;
        // 判停器只需要后半句的开头来进入“说话中”；交给识别的是前半句 + 后半句
        let vad_head = head.clone();
        let head = match earlier {
            Some(mut earlier) => {
                earlier.extend_from_slice(&head);
                earlier
            }
            None => head,
        };
        // 补送一次完整开头，并让普通判停器获得同样的语音状态。
        let head_ms = head.len() as u64 * 1000 / u64::from(self.uplink_rate);
        self.recording_started = Instant::now() - Duration::from_millis(head_ms);
        self.recording_marks.insert("barge_in".into(), head_ms);
        if let Some(worker) = self.vad_worker.as_mut() {
            worker.submit(vad_head, true)?;
        } else {
            let _ = self.vad.accept(&vad_head);
        }
        let stream = self.open_stream();
        let head_level = crate::audio::pcm_rms_level(&head);
        if let Phase::Listening(recording) = &mut self.phase {
            recording.speaking = true;
            recording.speech_began_at = Some(self.recording_started);
            recording.voiced_samples = head.len();
            recording.stream = stream;
            recording.feed(&head);
            recording.utterance = head;
        }
        self.set_level(head_level);
        self.last_activity = Instant::now();
        if follow_up {
            self.event(format!(
                "思考中用户接着说：放弃还没开口的回答，与前半句合并重新识别（已有 {head_ms} ms）"
            ));
            return Ok(());
        }
        if pause {
            self.event(format!(
                "听到插话，先暂停播放等识别结果（距开播 {}，保留开头 {head_ms} ms）",
                elapsed
                    .map(|ms| format!("{ms} ms"))
                    .unwrap_or_else(|| "尚未开播".into())
            ));
            return Ok(());
        }
        self.event(format!(
            "自然插话：连续语音至少 {} ms，距开播 {}，保留开头 {head_ms} ms",
            self.shared.config.barge_in_ms,
            elapsed
                .map(|ms| format!("{ms} ms"))
                .unwrap_or_else(|| "尚未开播".into())
        ));
        Ok(())
    }

    pub(super) fn busy(&self) -> bool {
        self.turn.is_some()
            || self.listen.paused.is_some()
            || matches!(self.phase, Phase::Speaking { .. } | Phase::Thinking)
    }

    pub(super) async fn on_text(
        &mut self,
        socket: &mut crate::link::Link,
        text: &str,
    ) -> Result<(), String> {
        self.last_activity = Instant::now();
        match ClientMessage::parse(text) {
            Ok(ClientMessage::Listen {
                state: ListenState::Start,
                mode,
                ..
            }) => {
                if self.mqtt && !socket.audio_open() {
                    // 常连模式：这一轮已结束（goodbye 已发）。设备在收到 goodbye 前按固件逻辑
                    // 发出的 listen start 迟到了——不能再进聆听，更不能把“聆听”画面推回已待命的屏幕
                    return Ok(());
                }
                let manual = mode.as_deref() == Some("manual");
                let realtime = mode.as_deref() == Some("realtime");
                if manual {
                    self.screen.preview_hold = None;
                }
                self.record_listen_mode(manual, realtime);
                // 插话后某些固件会补发一次 listen start：只在还没开口（聆听 / 思考）时吞掉这一次；
                // 新回答已经在播时的 listen start 是用户主动按键或唤醒，必须照常打断
                if realtime
                    && self.listen.listen_realtime
                    && self.listen.barge_resumed
                    && matches!(self.phase, Phase::Listening(_) | Phase::Thinking)
                {
                    self.listen.barge_resumed = false;
                    return Ok(());
                }
                self.listen.barge_resumed = false;
                self.listen.listen_realtime = realtime;
                self.trace_record(
                    "listen_start",
                    serde_json::json!({
                        "mode": if manual { "按住说话" } else if realtime { "实时" } else { "自动" },
                    }),
                );
                // 唤醒后设备紧接着发 listen start：不打断唤醒回应，播完再拾音
                if let Some(turn) = self.turn.as_mut()
                    && turn.kind == TurnKind::Greeting
                {
                    turn.listen_after = Some(manual);
                    self.listen.listen_manual = manual;
                    return Ok(());
                }
                if self.busy() {
                    self.interrupt(socket, "重新开始拾音").await?;
                }
                self.start_listening(manual);
                // n70 固件进入聆听时把表情重置成 neutral：换回常驻场景或“聆听”
                self.show_ambient(socket, crate::BotPose::Listening).await?;
            }
            Ok(ClientMessage::Listen {
                state: ListenState::Stop,
                ..
            }) => {
                self.trace_record("listen_stop", serde_json::json!({}));
                self.finish_listen_stop(socket).await?;
            }
            Ok(ClientMessage::Listen {
                state: ListenState::Detect,
                text,
                ..
            }) => {
                self.event(format!("唤醒：{}", text.unwrap_or_default()));
                let reply = self
                    .conversation
                    .as_ref()
                    .and_then(|conversation| conversation.settings.wake_reply.clone());
                if let Some(reply) = reply
                    && !self.busy()
                {
                    self.start_turn(
                        TurnInput::Say {
                            text: reply,
                            close: false,
                        },
                        TurnKind::Greeting,
                    );
                }
            }
            Ok(ClientMessage::Abort { reason }) => {
                let reason = reason.unwrap_or_else(|| "设备打断".into());
                self.screen.preview_hold = None;
                self.screen.pending_oled_preview = None;
                self.screen.one_shot_badge_url = None;
                self.screen.music_hud = None;
                let was_busy = self.busy();
                if was_busy {
                    self.interrupt(socket, &reason).await?;
                }
                if matches!(self.phase, Phase::Listening(_)) {
                    self.pending_listen_stop = false;
                    self.listen.hold_check = None;
                    if let Some(worker) = self.vad_worker.as_mut() {
                        worker.reset();
                    }
                    self.phase = Phase::Idle;
                    self.set_state("idle");
                    if !was_busy {
                        self.event(format!("拾音已取消（{reason}）"));
                    }
                }
            }
            Ok(ClientMessage::Ping) => {
                if !send_json(socket, &ServerMessage::Pong).await {
                    return Err("发送失败".into());
                }
            }
            Ok(ClientMessage::Goodbye) if self.mqtt => {
                // 常连模式：设备关了语音通道（按键结束、语音通道超时），控制通道还在
                self.end_conversation(socket, "设备结束了这一轮对话", false)
                    .await?;
            }
            Ok(ClientMessage::Goodbye) => return Err("设备结束会话".into()),
            Ok(ClientMessage::Mcp { payload }) => self.on_device_mcp(socket, payload).await?,
            Ok(ClientMessage::Hello {
                features,
                text_font,
                ..
            }) => {
                let cap = features.glyph_push.then_some(text_font).flatten();
                self.shared.state.set_text_font_spec(
                    self.session_id,
                    cap.as_ref().map(|f| f.size),
                    cap.as_ref().map(|f| f.bpp),
                );
                self.screen.text_font_cap = cap;
                if !features.mcp
                    && self.device_mcp.as_ref().is_some_and(|d| !d.initialized)
                    && let Some(mut dev) = self.device_mcp.take()
                {
                    for req in std::mem::take(&mut dev.deferred_requests) {
                        if let crate::DeviceRequest::CapturePhoto { ticket } = req {
                            self.shared.camera_preview.record_error(
                                self.device_id,
                                Some(&ticket),
                                "该设备未启用设备端 MCP，不支持摄像头抓拍".into(),
                            );
                        }
                    }
                    self.shared.state.disable_device_mcp(self.session_id);
                    if let Ok(mut map) = self.shared.device_requests.lock()
                        && let Some(handle) = map.get_mut(self.device_id)
                        && handle.session_id == self.session_id
                    {
                        handle.supports_mcp = false;
                    }
                }
            }
            Ok(_) => {}
            Err(error) => self.event(
                EventRecord::warn(
                    DiagCategory::Device,
                    "收到设备发来的一条看不懂的消息，已忽略",
                )
                .hint("通常是设备固件和 MOSS 版本不匹配；偶尔一次不影响使用，频繁出现请升级固件")
                .detail(&error),
            ),
        }
        Ok(())
    }

    /// 空闲到期：对话模式先说再见（说完断开），回声模式直接断开。
    pub(super) async fn on_idle(&mut self, socket: &mut crate::link::Link) -> Result<(), String> {
        if self.listen.paused.is_some() {
            self.abandon_paused(socket, "空闲超时").await?;
        }
        let idle = self.shared.config.idle_timeout.as_secs();
        let goodbye = self
            .conversation
            .as_ref()
            .map(|conversation| conversation.settings.goodbye.clone());
        match goodbye {
            Some(text) if self.turn.is_none() => {
                self.event(format!("{idle} 秒没有对话，说再见后断开"));
                self.last_activity = Instant::now();
                self.start_turn(TurnInput::Say { text, close: true }, TurnKind::Goodbye);
                Ok(())
            }
            _ if self.mqtt => {
                self.pending_goodbye = Some(format!("{idle} 秒无语音或消息"));
                Ok(())
            }
            _ => Err(format!("{idle} 秒无语音或消息")),
        }
    }
}
