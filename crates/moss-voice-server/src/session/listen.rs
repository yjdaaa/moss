//! 拾音：开始聆听、逐帧判停、流式识别抢跑与一段话的结束判定。

use super::*;

impl<'a> Session<'a> {
    fn samples(&self, ms: usize) -> usize {
        self.uplink_rate as usize * ms / 1000
    }

    /// 开始一段流式识别（没有配置流式识别时为空）。
    pub(super) fn open_stream(&self) -> Option<(AsrStream, Option<Resampler>)> {
        let streaming = self.conversation.as_ref()?.streaming_asr.as_ref()?;
        let resampler = (self.uplink_rate != STREAM_ASR_RATE)
            .then(|| Resampler::new(self.uplink_rate, STREAM_ASR_RATE));
        Some((streaming.start(), resampler))
    }

    pub(super) fn start_listening(&mut self, manual: bool) {
        self.listen.barge_resumed = false;
        self.recording_started = Instant::now();
        self.recording_marks = BTreeMap::from([("recording_start".into(), 0)]);
        self.vad.reset();
        if let Some(worker) = self.vad_worker.as_mut() {
            worker.reset();
        }
        self.pending_listen_stop = false;
        self.listen.hold_check = None;
        self.screen.last_screen_stt = None;
        self.listen.utterance_keyword = None;
        self.listen.kws_tail_until = None;
        self.listen.listen_manual = manual;
        // 自动拾音（非按键说话）且本次连接尚未写入过 `vad_engine`（纯能量判停，或本地判停已上报后中途刚开启过程记录）：补写一次实际判停方式
        if !manual
            && !self.vad_engine_traced
            && (self.vad_worker.is_none() || self.vad_worker_reported)
            && self.trace_handle().is_some()
        {
            self.vad_engine_traced = true;
            let engine = if self.vad_worker.is_some() {
                self.vad_engine.clone()
            } else {
                self.vad.engine()
            };
            let mut payload = serde_json::json!({"engine": engine});
            if let Some(reason) = &self.vad_fallback_reason {
                payload["fallback_reason"] = serde_json::json!(reason);
            }
            self.trace_record("vad_engine", payload);
        }
        // 手动模式按住即说：马上建立识别连接；自动模式等 VAD 确认开始说话（避免为环境声建连接）
        let stream = if manual { self.open_stream() } else { None };
        self.phase = Phase::Listening(Listening {
            manual,
            speaking: false,
            speech_began_at: manual.then(Instant::now),
            voiced_samples: 0,
            last_interim_stt_at: None,
            utterance: Vec::new(),
            preroll: VecDeque::new(),
            stream,
            last_interim: String::new(),
            prewarm_task: None,
            hold_used: false,
            hold_deadline: None,
            finalize_after_vad_seq: None,
        });
        self.set_state("listening");
    }

    /// 处理一帧上行 PCM；一段话结束时返回它。
    pub(super) fn hear(
        &mut self,
        pcm: &[i16],
        detected: Option<(Vec<VadEvent>, Option<String>)>,
    ) -> Option<(Vec<i16>, UtteranceEnd)> {
        let max = self.samples(MAX_UTTERANCE_MS);
        let preroll_max = self.samples(PREROLL_MS);
        let min_speech_samples = self.samples(self.shared.config.vad.min_speech_ms as usize);
        let Phase::Listening(listening) = &mut self.phase else {
            return None;
        };
        let mut ended = false;
        let mut began = false;
        let mut resumed = false;
        let mut runtime_notice = None;
        let mut heard_keywords = Vec::new();
        if listening.manual {
            listening.utterance.extend_from_slice(pcm);
            listening.voiced_samples = listening.utterance.len();
            listening.feed(pcm);
            self.last_activity = Instant::now();
        } else {
            let was_speaking = listening.speaking;
            let (events, notice) = match detected {
                Some(value) => value,
                None => {
                    let events = self.vad.accept(pcm);
                    (events, self.vad.take_notice())
                }
            };
            // 判停出了问题的提示：属于这台设备这一轮，记进它的流程（下面借用结束后再写）
            runtime_notice = notice;
            for event in events {
                match event {
                    VadEvent::SpeechStart => {
                        listening.speaking = true;
                        self.recording_marks
                            .entry("vad_start".into())
                            .or_insert(self.recording_started.elapsed().as_millis() as u64);
                    }
                    VadEvent::SpeechEnd => {
                        ended = true;
                        self.recording_marks.insert(
                            "vad_end".into(),
                            self.recording_started.elapsed().as_millis() as u64,
                        );
                    }
                    VadEvent::Keyword(kind) => {
                        self.listen.utterance_keyword = Some(kind);
                        heard_keywords.push(kind);
                    }
                }
            }
            if listening.speaking && !was_speaking {
                if self.listen.hold_check.take().is_some()
                    || listening.hold_deadline.is_some()
                    || listening.finalize_after_vad_seq.is_some()
                {
                    listening.hold_used = true;
                }
                listening.hold_deadline = None;
                listening.finalize_after_vad_seq = None;
                if listening.utterance.is_empty() {
                    let mut merged: Vec<i16> = listening.preroll.drain(..).collect();
                    merged.extend_from_slice(pcm);
                    let voiced_span =
                        trailing_voiced_samples(&merged, self.uplink_rate, &self.shared.config.vad)
                            .max(min_speech_samples.min(merged.len()))
                            .min(merged.len());
                    let voiced_ms = voiced_span as u64 * 1000 / u64::from(self.uplink_rate.max(1));
                    let approx_began = Instant::now()
                        .checked_sub(Duration::from_millis(voiced_ms))
                        .map_or(self.recording_started, |t| t.max(self.recording_started));
                    listening.speech_began_at = Some(approx_began);
                    listening.voiced_samples = voiced_span;
                    listening.utterance = merged;
                    began = true;
                } else {
                    // 半截话续听后再次开口：将中间短暂过渡音频无缝拼入同一段 utterance 与流式识别
                    let bridge: Vec<i16> = listening.preroll.drain(..).collect();
                    listening.voiced_samples = listening
                        .voiced_samples
                        .saturating_add(bridge.len())
                        .saturating_add(pcm.len());
                    listening.feed(&bridge);
                    listening.utterance.extend_from_slice(&bridge);
                    listening.utterance.extend_from_slice(pcm);
                    listening.feed(pcm);
                    resumed = true;
                }
                self.last_activity = Instant::now();
            } else if listening.speaking {
                listening.utterance.extend_from_slice(pcm);
                listening.voiced_samples = listening.voiced_samples.saturating_add(pcm.len());
                listening.feed(pcm);
                self.last_activity = Instant::now();
            } else {
                listening.preroll.extend(pcm.iter().copied());
                let excess = listening.preroll.len().saturating_sub(preroll_max);
                listening.preroll.drain(..excess);
            }
        }
        if let Some(notice) = runtime_notice {
            self.event(
                EventRecord::warn(DiagCategory::Audio, notice)
                    .hint("本地判停模型出了问题，这次连接先用音量判断你是否说完；重启内置服务会重新尝试本地模型"),
            );
        }
        for kind in heard_keywords {
            self.event(EventRecord::info(
                DiagCategory::Audio,
                format!("命令词模型听到{}命令", keyword_label(kind)),
            ));
        }
        if began || resumed {
            self.trace_record(
                "speech_began",
                serde_json::json!({"engine": self.vad_engine, "resumed": resumed}),
            );
        }
        if ended {
            self.trace_record("speech_end", serde_json::json!({"engine": self.vad_engine}));
        }
        if began {
            self.screen.preview_hold = None;
            // VAD 刚确认开始说话：建立流式识别，先补送保留的开头与这一帧
            let stream = self.open_stream();
            if let Phase::Listening(listening) = &mut self.phase {
                listening.stream = stream;
                let head = listening.utterance.clone();
                listening.feed(&head);
            }
        }
        let Phase::Listening(listening) = &mut self.phase else {
            return None;
        };
        if ended && self.listen.hold_check.is_none() {
            return Some((
                std::mem::take(&mut listening.utterance),
                UtteranceEnd::Silence,
            ));
        }
        if listening.utterance.len() >= max {
            return Some((
                std::mem::take(&mut listening.utterance),
                UtteranceEnd::TooLong,
            ));
        }
        None
    }

    /// NV8a-1 / n87：中间识别经同音纠错后整句命中停止词时立刻发起急停轮次；
    /// 说话满约半秒后按 500ms 节流把实时识别文字推上小智屏幕；开启 Jev 时按 120ms 防抖预热未说完判定。
    pub(super) async fn on_interim(
        &mut self,
        socket: &mut crate::link::Link,
        interim: String,
    ) -> Result<(), String> {
        let Some(conversation) = self.conversation.clone() else {
            return Ok(());
        };
        if !matches!(self.phase, Phase::Listening(_)) {
            return Ok(());
        }
        let corrected = crate::conversation::apply_asr_corrections(
            &interim,
            &conversation.settings.asr_corrections,
        );
        if self.trace_handle().is_some() {
            // 识别流的降噪情况（辅助程序打开流时报告）在第一个中间结果时记下，按真实先后排在中间文字之前
            let denoise = match &self.phase {
                Phase::Listening(listening) => listening
                    .stream
                    .as_ref()
                    .and_then(|(stream, _)| stream.take_denoise()),
                _ => None,
            };
            if let Some(report) = denoise {
                self.trace_record("denoise", crate::audio::denoise_trace("识别", &report));
            }
            if !interim.trim().is_empty() {
                self.trace_record(
                    "interim",
                    serde_json::json!({"text": interim, "corrected": corrected}),
                );
            }
        }
        if crate::fast::is_stop(&corrected) {
            self.pending_listen_stop = false;
            self.listen.hold_check = None;
            if let Some(worker) = self.vad_worker.as_mut() {
                worker.reset();
            }
            // 替换 Listening 会丢弃旧 ASR 接收器与预热任务；本次语音只开始一个 FastStop 轮次，
            // 后续最终结果和设备 listen stop 不会再次触发。
            self.phase = Phase::Idle;
            self.event(format!(
                "流式中间结果整句命中急停词（{}）：立即请求急停",
                crate::fast::normalized(&corrected)
            ));
            self.start_turn(
                TurnInput::FastStop {
                    text: interim,
                    source: crate::conversation::FastStopSource::Interim,
                },
                TurnKind::Answer,
            );
            return Ok(());
        }
        let device_specs: Vec<_> = self
            .device_mcp
            .as_ref()
            .and_then(DeviceMcp::tool_set)
            .map(|device| {
                device
                    .specs()
                    .iter()
                    .filter(|spec| spec.name != "play_music")
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let min_audio_samples = self.samples(INTERIM_STT_MIN_AUDIO_MS);
        let can_push_screen_stt = self.listen.paused.is_none() && self.listen.hold_check.is_none();
        let mut push_screen_stt = false;
        {
            let Phase::Listening(listening) = &mut self.phase else {
                return Ok(());
            };
            if listening.last_interim != corrected {
                listening.last_interim.clone_from(&corrected);
                if let Some(prev) = listening.prewarm_task.take() {
                    prev.abort();
                }
                if let Some(jev) = conversation
                    .jev_filter
                    .clone()
                    .filter(|f| f.options().enabled && f.options().unfinished_hold)
                    && crate::fast::normalized(&corrected).chars().count() >= 2
                {
                    let history = Self::jev_prewarm_history(
                        &self.shared.vision_photos,
                        self.device_id,
                        &self.history,
                        &conversation,
                    );
                    let prewarm_text = corrected.clone();
                    listening.prewarm_task = Some(tokio::spawn(async move {
                        tokio::time::sleep(INTERIM_PREWARM_DEBOUNCE).await;
                        let tool_executor = conversation.tools.clone();
                        let mut tools = tokio::task::spawn_blocking(move || tool_executor.tools())
                            .await
                            .unwrap_or_default();
                        tools.extend(device_specs);
                        let _ = jev
                            .should_hold_unfinished(&prewarm_text, &history, &tools)
                            .await;
                    }));
                }
            }
            let now = Instant::now();
            if can_push_screen_stt
                && listening.voiced_samples >= min_audio_samples
                && listening
                    .speech_began_at
                    .is_some_and(|at| now.duration_since(at) >= INTERIM_STT_FIRST_ELAPSED)
                && listening
                    .last_interim_stt_at
                    .is_none_or(|at| now.duration_since(at) >= INTERIM_STT_COOLDOWN)
                && moss_voice_providers::text::speakable(&corrected)
                && !crate::fast::is_noise_filler(&corrected)
                && crate::fast::normalized(&corrected).chars().count() >= 2
                && self.screen.last_screen_stt.as_deref() != Some(corrected.as_str())
            {
                listening.last_interim_stt_at = Some(now);
                push_screen_stt = true;
            }
        }
        if push_screen_stt && !self.send_stt(socket, corrected).await {
            return Err("发送失败".into());
        }
        Ok(())
    }

    fn jev_prewarm_history(
        vision_photos: &crate::vision::VisionPhotos,
        device_id: &str,
        base_history: &[ChatMessage],
        conversation: &Conversation,
    ) -> Vec<ChatMessage> {
        // 声纹个人模式下本轮说话人尚未在拾音阶段完成验声，严禁把上一位说话人的私密历史送往预热端点。
        if conversation.personal_context.is_some() {
            return Vec::new();
        }
        let mut history = base_history.to_vec();
        if let Some(photo_msg) = vision_photos.peek_message(device_id) {
            history.push(photo_msg);
        }
        history
    }

    /// VAD 静音判停时：若开启了 Jev 未说完防抢话，启动非阻塞判定任务挂在 `Session::hold_check` 上，
    /// 会话循环继续实时响应 `Abort`、`ListenStop`、`Ping`、停止词与提前续说音频。
    pub(super) async fn finish_or_hold_utterance(
        &mut self,
        socket: &mut crate::link::Link,
        pcm: Vec<i16>,
        end: UtteranceEnd,
    ) -> Result<(), String> {
        if matches!(end, UtteranceEnd::Silence)
            && !self.pending_listen_stop
            && self.listen.hold_check.is_none()
            && let Some(conversation) = self.conversation.clone()
            && let Some(jev) = conversation
                .jev_filter
                .clone()
                .filter(|f| f.options().enabled && f.options().unfinished_hold)
        {
            let device_specs: Vec<_> = self
                .device_mcp
                .as_ref()
                .and_then(DeviceMcp::tool_set)
                .map(|device| {
                    device
                        .specs()
                        .iter()
                        .filter(|spec| spec.name != "play_music")
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if let Phase::Listening(listening) = &mut self.phase
                && !listening.manual
                && !listening.hold_used
            {
                if let Some(task) = listening.prewarm_task.take()
                    && !task.is_finished()
                {
                    task.abort();
                }
                let latest = listening
                    .stream
                    .as_ref()
                    .and_then(|(s, _)| s.latest_interim())
                    .map(|raw| {
                        crate::conversation::apply_asr_corrections(
                            &raw,
                            &conversation.settings.asr_corrections,
                        )
                    })
                    .unwrap_or_else(|| listening.last_interim.clone());
                listening.last_interim = latest.clone();
                if !latest.is_empty() {
                    let paused_at = Instant::now();
                    let checking = latest.clone();
                    let history = Self::jev_prewarm_history(
                        &self.shared.vision_photos,
                        self.device_id,
                        &self.history,
                        &conversation,
                    );
                    let tool_executor = conversation.tools.clone();
                    // 暂存前半句录音、保持 Phase::Listening 不中断，交由 select! 异步接收结果；
                    // `tools()` 放到后台任务内的 spawn_blocking，既不阻塞会话循环也不占 2-worker Tokio 线程。
                    listening.utterance = pcm;
                    listening.speaking = false;
                    self.listen.hold_check = Some(PendingHoldCheck {
                        paused_at,
                        task: tokio::spawn(async move {
                            tokio::task::yield_now().await;
                            let mut tools =
                                tokio::task::spawn_blocking(move || tool_executor.tools())
                                    .await
                                    .unwrap_or_default();
                            tools.extend(device_specs);
                            jev.should_hold_unfinished(&latest, &history, &tools).await
                        }),
                    });
                    self.set_level(0);
                    self.trace_record("jev_hold_check", serde_json::json!({"text": checking}));
                    return Ok(());
                }
            }
        }
        self.finish_utterance(socket, pcm, end).await
    }

    pub(super) async fn on_hold_check_done(
        &mut self,
        socket: &mut crate::link::Link,
        paused_at: Instant,
        held: bool,
    ) -> Result<(), String> {
        if self.pending_listen_stop {
            return self.finish_listen_stop(socket).await;
        }
        let deadline = paused_at + UNFINISHED_HOLD_WINDOW;
        let in_time = Instant::now() < deadline;
        if held
            && in_time
            && let Phase::Listening(listening) = &mut self.phase
        {
            listening.hold_used = true;
            listening.hold_deadline = Some(deadline);
            listening.finalize_after_vad_seq = None;
            self.set_level(0);
            self.trace_record(
                "jev_hold",
                serde_json::json!({
                    "decision": "extend",
                    "waited_ms": paused_at.elapsed().as_millis() as u64,
                    "window_ms": UNFINISHED_HOLD_WINDOW.as_millis() as u64,
                }),
            );
            self.event("检测到语句尚未说完，延长拾音等待续说");
            return Ok(());
        }
        self.trace_record(
            "jev_hold",
            serde_json::json!({
                "decision": if held && !in_time { "late" } else { "finished" },
                "held": held,
                "waited_ms": paused_at.elapsed().as_millis() as u64,
            }),
        );
        self.finalize_held_utterance(socket).await
    }

    pub(super) async fn on_hold_expired(
        &mut self,
        socket: &mut crate::link::Link,
    ) -> Result<(), String> {
        let active = matches!(&self.phase, Phase::Listening(l) if l.hold_deadline.is_some());
        if !active {
            return Ok(());
        }
        self.trace_record("jev_hold", serde_json::json!({"decision": "expired"}));
        self.finalize_held_utterance(socket).await
    }

    /// 结束挂起中的半句录音：若此刻 `VadWorker` 仍有已接收但尚未消费的在途帧，
    /// 先记录帧序号屏障，待这些在途帧经 `hear` 确认无续说后再提交，防止 `worker.reset()` 丢弃已入队续说语音。
    async fn finalize_held_utterance(
        &mut self,
        socket: &mut crate::link::Link,
    ) -> Result<(), String> {
        if !self.listen.listen_manual
            && let Some(target) = self
                .vad_worker
                .as_ref()
                .and_then(crate::audio::worker::VadWorker::pending_target)
            && let Phase::Listening(listening) = &mut self.phase
        {
            listening.hold_deadline = None;
            listening.finalize_after_vad_seq = Some(target);
            return Ok(());
        }
        let pcm = match &mut self.phase {
            Phase::Listening(listening) => {
                listening.hold_deadline = None;
                listening.finalize_after_vad_seq = None;
                std::mem::take(&mut listening.utterance)
            }
            _ => return Ok(()),
        };
        self.finish_utterance(socket, pcm, UtteranceEnd::Silence)
            .await
    }

    /// 一段话结束：记录并交给应答。
    async fn finish_utterance(
        &mut self,
        socket: &mut crate::link::Link,
        pcm: Vec<i16>,
        end: UtteranceEnd,
    ) -> Result<(), String> {
        self.pending_listen_stop = false;
        self.listen.hold_check = None;
        // 开了命令词检测：说完先不重置判停流，尾音接着送进同一路，命令词模型才等得到词后那一小段、报出结果
        // （下次开始拾音时照常重置；这期间判停事件在思考阶段本就不处理，只收命令词）
        let keep_for_keywords = !self.listen.listen_manual
            && self
                .conversation
                .as_ref()
                .is_some_and(|conversation| conversation.settings.command_keywords);
        if !keep_for_keywords && let Some(worker) = self.vad_worker.as_mut() {
            worker.reset();
        }
        let stream = match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Listening(mut listening) => listening.stream.take().map(|(stream, _)| stream),
            _ => None,
        };
        self.set_state("idle");
        let seconds = pcm.len() as f64 / self.uplink_rate as f64;
        self.event(format!("收到一段语音：{seconds:.1} 秒（{}）", end.label()));
        if self.conversation.is_none() {
            return self.echo(socket, pcm).await;
        }
        if pcm.is_empty() {
            // 手动模式松开但没说话：设备已回到空闲
            return Ok(());
        }
        let sample_rate = self.uplink_rate;
        if let Some(enrollment) = self
            .conversation
            .as_ref()
            .and_then(|c| c.speaker_enrollment.as_ref())
        {
            enrollment.submit(self.device_id, self.session_id, &pcm, sample_rate);
        }
        self.listen.gate_next_turn =
            matches!(end, UtteranceEnd::Silence) && !self.listen.listen_manual;
        if self
            .conversation
            .as_ref()
            .is_some_and(|conversation| conversation.settings.command_keywords)
            && !self.listen.listen_manual
        {
            self.listen.kws_tail_until = Some(Instant::now() + KWS_TAIL);
        }
        self.start_turn(
            TurnInput::Utterance {
                pcm,
                sample_rate,
                stream,
            },
            TurnKind::Answer,
        );
        Ok(())
    }

    pub(super) async fn finish_listen_stop(
        &mut self,
        socket: &mut crate::link::Link,
    ) -> Result<(), String> {
        if !self.listen.listen_manual
            && self
                .vad_worker
                .as_ref()
                .is_some_and(|worker| worker.pending())
        {
            self.pending_listen_stop = true;
            return Ok(());
        }
        if let Phase::Listening(listening) = &mut self.phase {
            let utterance =
                if listening.manual || listening.speaking || !listening.utterance.is_empty() {
                    std::mem::take(&mut listening.utterance)
                } else {
                    Vec::new()
                };
            self.finish_utterance(socket, utterance, UtteranceEnd::ListenStop)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn on_vad_result(
        &mut self,
        socket: &mut crate::link::Link,
        frame: Result<crate::audio::worker::VadFrame, String>,
    ) -> Result<(), String> {
        let mut frame = frame?;
        // 判停线程交来的事实（判停方式、跳过的帧、降噪情况）与出错提示：不论此刻处于哪个阶段都如实记下
        let notes = std::mem::take(&mut frame.trace);
        if !notes.is_empty() {
            self.trace_vad_notes(notes);
        }
        if let Some(notice) = frame.notice.take() {
            self.event(
                EventRecord::warn(DiagCategory::Audio, notice)
                    .hint("本地判停模型出了问题，这次连接先用音量判断你是否说完；重启内置服务会重新尝试本地模型"),
            );
        }
        // 命令词模型的每一次命中都记下（后面各分支决定怎么用它，用法另有事件说明）
        if self.trace_handle().is_some() {
            let when = match (&self.phase, frame.prime) {
                (Phase::Listening(_), false) => "用户说话中",
                (Phase::Listening(_), true) => "补送的开头里",
                (Phase::Thinking, false) => "刚说完那句的尾音里",
                (Phase::Thinking, true) => "思考中听到的新声音里",
                (Phase::Speaking { .. }, _) => "播放回答时",
                (Phase::Idle, _) => "空闲时",
            };
            for event in &frame.events {
                if let VadEvent::Keyword(kind) = event {
                    self.trace_keyword(*kind, when);
                }
            }
        }
        // 思考中新的一阵声音里命令词模型听到了“停”：不用等识别，直接打断、执行停止（n101；停止说法绝不吞掉）。
        // 刚说完那句的尾音里听到的不在这里急停（n114）：“别停”“不要停止”“加工怎么暂停”的尾音里也有“停”，
        // 它补写给这一轮，由识别出的文字决定要不要按停止处理（`command_hint_fits`，长句与否定说法不算）
        if frame.prime
            && matches!(self.phase, Phase::Thinking)
            && frame.events.contains(&VadEvent::Keyword(
                moss_voice_providers::local::CommandKeyword::Stop,
            ))
        {
            self.listen.follow_probe = None;
            self.listen.follow.reset();
            self.event("思考中命令词模型听到“停”");
            return self
                .on_stop_check_done(
                    socket,
                    Some("停".into()),
                    crate::conversation::FastStopSource::Keyword,
                )
                .await;
        }
        // 这段话的判停结果晚到（这一轮已经开始，甚至是抢跑开始的）：里面的命令词补写给这一轮
        // （思考中补送的确认窗口是新的一阵声音，不属于这段话，不算）
        if !frame.prime
            && matches!(self.phase, Phase::Thinking)
            && let Some(slot) = self
                .turn
                .as_ref()
                .filter(|turn| !turn.audio_started)
                .and_then(|turn| turn.command_hint.as_ref())
        {
            for event in &frame.events {
                if let VadEvent::Keyword(kind) = event
                    && let Ok(mut hint) = slot.lock()
                {
                    *hint = Some(*kind);
                    self.shared.state.event_in_flow(
                        EventRecord::info(
                            DiagCategory::Audio,
                            format!("命令词模型听到{}命令（说完后补到）", keyword_label(*kind)),
                        ),
                        self.trace_flow(),
                    );
                }
            }
        }
        // 补送的开头里命令词模型听到的命令词也记给这一段话
        if frame.prime && matches!(self.phase, Phase::Listening(_)) {
            for event in &frame.events {
                if let VadEvent::Keyword(kind) = event {
                    self.listen.utterance_keyword = Some(*kind);
                }
            }
        }
        // 思考中“接着说”候选的确认结果：判停模型也听到人声才放弃还没开口的回答
        if frame.prime && matches!(self.phase, Phase::Thinking) {
            if self.listen.follow_probe.is_some()
                && frame.events.contains(&VadEvent::SpeechStart)
                && self.may_follow_up()
                && let Some(probe) = self.listen.follow_probe.take()
            {
                self.listen.follow.reset();
                self.trace_record("follow_probe", serde_json::json!({"phase": "confirmed"}));
                return self.take_over(socket, probe.head, true).await;
            }
            return Ok(());
        }
        // 插话 / 接着说补送开头后判停模型的结论：此刻不在说话（只是噪声，或一句短话已说完）就立刻收尾识别，
        // 不再等一个永远不会来的“说完”（此前在突发噪声下会一直等到 30 秒上限）
        if frame.prime && matches!(self.phase, Phase::Listening(_)) && !self.listen.listen_manual {
            let in_speech = still_speaking(&frame.events);
            if !in_speech
                && let Some((pcm, end)) = self.hear(&[], Some((vec![VadEvent::SpeechEnd], None)))
            {
                self.event("补送的开头里判停模型没听到还在继续的人声：直接收尾识别".to_owned());
                return self.finish_or_hold_utterance(socket, pcm, end).await;
            }
        }
        if !frame.prime
            && matches!(self.phase, Phase::Listening(_))
            && !self.listen.listen_manual
            && let Some((pcm, end)) = self.hear(&frame.samples, Some((frame.events, None)))
        {
            self.finish_or_hold_utterance(socket, pcm, end).await?;
        }
        if self.pending_listen_stop {
            self.finish_listen_stop(socket).await?;
            return Ok(());
        }
        let reached_barrier = self
            .vad_worker
            .as_ref()
            .zip(match &self.phase {
                Phase::Listening(listening) => listening.finalize_after_vad_seq,
                _ => None,
            })
            .is_some_and(|(worker, target)| worker.completed_seq() >= target);
        if reached_barrier {
            let pcm = match &mut self.phase {
                Phase::Listening(listening) if !listening.speaking => {
                    listening.finalize_after_vad_seq = None;
                    std::mem::take(&mut listening.utterance)
                }
                _ => Vec::new(),
            };
            if !pcm.is_empty() {
                self.finish_utterance(socket, pcm, UtteranceEnd::Silence)
                    .await?;
            }
        }
        Ok(())
    }

    /// 正在拾音或存在尚未提交的半句录音/续听判定时，延后加工完成播报，待本轮对话安全结束后再播。
    pub(super) fn listening_occupied(&self) -> bool {
        if self.listen.hold_check.is_some() || self.pending_listen_stop {
            return true;
        }
        match &self.phase {
            Phase::Listening(listening) => {
                listening.manual
                    || !self.listen.listen_realtime
                    || listening.speaking
                    || !listening.utterance.is_empty()
                    || listening.hold_deadline.is_some()
                    || listening.finalize_after_vad_seq.is_some()
                    || self
                        .vad_worker
                        .as_ref()
                        .is_some_and(crate::audio::worker::VadWorker::pending)
            }
            _ => false,
        }
    }

    pub(super) fn actively_speaking_user(&self) -> bool {
        if self.listen.hold_check.is_some() || self.pending_listen_stop {
            return true;
        }
        match &self.phase {
            Phase::Listening(listening) => {
                listening.manual
                    || listening.speaking
                    || !listening.utterance.is_empty()
                    || listening.hold_deadline.is_some()
                    || listening.finalize_after_vad_seq.is_some()
            }
            _ => false,
        }
    }
}

fn keyword_label(kind: moss_voice_providers::local::CommandKeyword) -> &'static str {
    match kind {
        moss_voice_providers::local::CommandKeyword::Stop => "停止",
        moss_voice_providers::local::CommandKeyword::Exit => "退出",
    }
}

/// 这批判停事件之后用户还在不在说：只看最后一个开始 / 结束事件。命令词事件排在它们后面，
/// 不能把“还在说”算成“说完了”（n114：原先只看最后一个事件，插话“停下来，先把……”会被截成两段）。
pub(crate) fn still_speaking(events: &[VadEvent]) -> bool {
    events
        .iter()
        .rev()
        .find(|event| matches!(event, VadEvent::SpeechStart | VadEvent::SpeechEnd))
        .is_some_and(|event| matches!(event, VadEvent::SpeechStart))
}
