//! 下行播放节拍与停止播报。

use super::*;

impl<'a> Session<'a> {
    /// 播放节奏的一拍：开头连发预缓冲帧，之后每拍一帧；按绝对时钟推进，不受发送耗时影响。
    pub(super) async fn tick(&mut self, socket: &mut crate::link::Link) -> Result<(), String> {
        let was_music = self.music.as_ref().is_some_and(|music| music.active());
        if was_music && let Some(music) = self.music.as_mut() {
            match music.refill(&mut self.pump) {
                MusicProgress::Failed(reason) => {
                    self.music_failed = true;
                    self.event(
                        EventRecord::error(DiagCategory::Audio, "音乐解码或缓冲失败，已终止播放")
                            .hint("这首歌的文件可能损坏或格式不支持：换一首试试")
                            .detail(format!("{reason:?}")),
                    );
                }
                MusicProgress::Cancelled => self.event(EventRecord::warn(
                    DiagCategory::Device,
                    "设备权限撤销，音乐已停止",
                )),
                _ => {}
            }
        }
        let step = Duration::from_millis(FRAME_MS);
        if let Phase::Speaking {
            next, not_before, ..
        } = &mut self.phase
            && Instant::now() < *not_before
        {
            // 设备还没切到说话状态：此刻发出的帧会被固件丢弃
            *next = *not_before;
            return Ok(());
        }
        loop {
            let now = Instant::now();
            if let Phase::Speaking { anchor, sent, .. } = &mut self.phase {
                let played_until = *anchor + step * *sent;
                if played_until <= now {
                    // 设备端已播空（刚起播，或上游中断久于余量）：时间轴从此刻重新对齐
                    *anchor = now;
                    *sent = 0;
                } else if played_until - now > step * (LEAD_FRAMES - 1) {
                    break;
                }
            }
            match self.pump.next_frame()? {
                PumpTick::Frame { data, marks } => {
                    // Check permission immediately before every outgoing frame,
                    // including any catch-up burst.
                    if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
                        self.last_verified_person = None;
                        if let Some(music) = self.music.as_mut() {
                            music.cancel(&mut self.pump);
                        }
                        self.music_failed = false;
                        return Err("设备权限已撤销".into());
                    }
                    for text in marks {
                        let index = self.turn.as_ref().map_or(0, |turn| turn.spoken.len()) + 1;
                        self.trace_record(
                            "sentence_play",
                            serde_json::json!({"index": index, "text": text}),
                        );
                        self.tts(socket, TtsState::SentenceStart, Some(text.clone()))
                            .await?;
                        self.shared.state.inc_spoken_sentences(self.session_id);
                        if let Some(turn) = self.turn.as_mut() {
                            turn.spoken.push(text);
                            turn.spoken_at.push(turn.frames_sent);
                        }
                    }
                    if let Some(turn) = self.turn.as_mut()
                        && let Some(since) = turn.underrun_started_at.take()
                    {
                        turn.playback_stall_ms = turn
                            .playback_stall_ms
                            .saturating_add(since.elapsed().as_millis() as u64);
                    }
                    self.send_frame(socket, &data).await?;
                    if let Some(turn) = self.turn.as_mut() {
                        turn.frames_sent += 1;
                    }
                    self.set_level(self.pump.last_level());
                    if self.listen.playback_started.is_none() {
                        self.listen.playback_started = Some(Instant::now());
                        self.listen.barge.reset();
                    }
                    if let Some(turn) = self.turn.as_mut() {
                        turn.first_audio_frame_at_ms
                            .get_or_insert_with(crate::unix_now_ms);
                        if let Some(timing) = turn.timing.as_mut() {
                            timing.stages.entry("first_audio_frame".into()).or_insert(
                                turn.recording_offset
                                    .saturating_add(turn.started.elapsed().as_millis() as u64),
                            );
                        }
                    }
                    if let Phase::Speaking { sent, .. } = &mut self.phase {
                        *sent += 1;
                    }
                }
                PumpTick::Waiting => {
                    // 还在攒启动缓冲或上游暂时供不上：不发帧，稍后重检（SpeechMore/Done 也会唤醒）
                    if self.listen.playback_started.is_some()
                        && let Some(turn) = self.turn.as_mut()
                        && turn.underrun_started_at.is_none()
                    {
                        turn.underrun_started_at = Some(Instant::now());
                    }
                    if let Phase::Speaking { next, .. } = &mut self.phase {
                        *next = Instant::now() + STARVED_POLL;
                    }
                    return Ok(());
                }
                PumpTick::Done => {
                    if let Some(turn) = self.turn.as_mut()
                        && let Some(since) = turn.underrun_started_at.take()
                    {
                        turn.playback_stall_ms = turn
                            .playback_stall_ms
                            .saturating_add(since.elapsed().as_millis() as u64);
                    }
                    // 等设备把已发出的帧播完再发 tts stop，避免尾字被吞
                    if let Phase::Speaking {
                        next,
                        anchor,
                        sent,
                        stop_at,
                        ..
                    } = &mut self.phase
                        && self.listen.playback_started.is_some()
                    {
                        let at = *stop_at.get_or_insert(*anchor + step * *sent + DRAIN_MARGIN);
                        if Instant::now() < at {
                            *next = at;
                            return Ok(());
                        }
                    }
                    let music_failed = was_music && std::mem::take(&mut self.music_failed);
                    if was_music {
                        self.screen.music_hud = None;
                        if let Some(music) = self.music.as_mut() {
                            music.finish_after_pump_done();
                        }
                    }
                    self.stop_speaking(socket, None, false).await?;
                    return if was_music {
                        if music_failed
                            && self.permission.load(std::sync::atomic::Ordering::Acquire)
                            && self.conversation.is_some()
                        {
                            self.start_turn(
                                TurnInput::Say {
                                    text: "这首歌播放出错，换一首试试".into(),
                                    close: false,
                                },
                                TurnKind::Notice,
                            );
                        } else if self.listen.listen_realtime {
                            self.start_listening(false);
                        }
                        Ok(())
                    } else {
                        self.end_turn(true).await
                    };
                }
            }
        }
        if let Phase::Speaking {
            next, anchor, sent, ..
        } = &mut self.phase
        {
            // 余量降到目标以下的时刻再发下一帧：稳态即绝对时间轴上每 60 ms 一帧，不随每拍调度误差漂移
            *next = *anchor + step * sent.saturating_sub(LEAD_FRAMES - 1);
        }
        Ok(())
    }

    /// 结束播放。`interrupted` 为打断原因时立即丢弃未播内容；`skip_device_tts` 用于打断前已取出 `device_silent` 轮次的场景。
    pub(super) async fn stop_speaking(
        &mut self,
        socket: &mut crate::link::Link,
        interrupted: Option<&str>,
        skip_device_tts: bool,
    ) -> Result<(), String> {
        let stats = self.pump.stats();
        let skip_tts = skip_device_tts || self.current_turn_silent();
        if let Some(turn) = self.turn.as_mut() {
            if let Some(since) = turn.underrun_started_at.take() {
                turn.playback_stall_ms = turn
                    .playback_stall_ms
                    .saturating_add(since.elapsed().as_millis() as u64);
            }
            turn.playback_frames = stats.frames;
            turn.playback_underruns = stats.underruns;
        }
        if let Some(reason) = interrupted {
            self.screen.pending_oled_preview = None;
            self.screen.preview_hold = None;
            self.pump.clear();
            self.event(format!("播放被打断（{reason}）：已播 {} 帧", stats.frames));
        } else {
            self.event(format!(
                "播放完成：{} 帧，供流中断 {} 次",
                stats.frames, stats.underruns
            ));
        }
        self.listen.playback_started = None;
        self.phase = Phase::Idle;
        self.set_state("idle");
        self.last_activity = Instant::now();
        if !skip_tts {
            self.tts(socket, TtsState::Stop, None).await?;
        }
        if interrupted.is_none() {
            if let Some(hold_url) = self.screen.preview_hold.as_mut().map(|hold| {
                let now = Instant::now();
                hold.until = now + self.shared.config.preview_hold_duration;
                hold.next_refresh = now + self.shared.config.preview_hold_interval;
                hold.url.clone()
            }) {
                self.send_quiet_device_call(
                    socket,
                    "self.screen.preview_image",
                    json!({ "url": hold_url }),
                )
                .await?;
            }
            if let Some(card) = self.screen.pending_oled_preview.take()
                && !self.send_stt(socket, format!("[预览] {card}")).await
            {
                return Err("发送失败".into());
            }
        }
        Ok(())
    }
}
