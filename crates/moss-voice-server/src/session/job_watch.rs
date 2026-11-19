//! 加工任务轮询：完工播报、终态徽章与实时进度副屏。

use super::*;

impl<'a> Session<'a> {
    async fn stage_terminal_badge_url(&mut self, job: &crate::jobs::FinishedJob) -> Option<String> {
        let screen_size = self.device_mcp.as_ref().and_then(|d| d.screen_size);
        let (cw, ch) = crate::preview_push::optimal_canvas_for_screen(screen_size);
        let completed = job.status == "completed";
        let lines = if completed {
            job.total_lines
        } else {
            job.lines_sent
        };
        let card_style = self.shared.card_style();
        let png = tokio::task::spawn_blocking(move || {
            crate::preview_push::make_terminal_badge_png(
                &crate::preview_push::TerminalBadgeSpec { completed, lines },
                card_style,
                cw,
                ch,
            )
        })
        .await
        .ok()?
        .ok()?;
        let tok = self.shared.preview_store.stage_prebuilt_png(png).ok()?;
        let host = self.preview_http_host();
        Some(format!(
            "http://{host}:{}/mcp/preview/{tok}.png",
            self.shared.http_port
        ))
    }

    /// 加工结束播报、终态 5 秒徽章卡、实时加工副屏 HUD 与待电脑审批警戒卡（全流程零 Flash 擦写）：
    /// - 只读 `laser_workflow_tool(action=recent)`（进程内合并 `stop-targets` 与无敏待审批状态）；
    /// - 任务从 `sending` 进入终态时：彩屏弹出 5 秒完工庆祝/急停断点徽章卡（自动淡出）并语音播报；
    /// - 任务处于 `sending` 时：彩屏刷新双画风实时进度副屏 HUD；
    /// - 严格模式有待电脑批准请求时：彩屏刷新待电脑审批警戒卡（含倒计时条）。
    pub(super) async fn poll_jobs(&mut self, socket: &mut crate::link::Link) -> Result<(), String> {
        self.notices.next_job_poll = Instant::now() + self.shared.config.job_poll_interval;
        if self.busy() || !self.permission.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        // n70 完工 / 急停画面到时间了：回到聆听或待命
        if self
            .screen
            .screen_scene
            .as_ref()
            .is_some_and(|scene| scene.until.is_some_and(|t| Instant::now() >= t))
            && let Some(scene) = self.screen.screen_scene.as_ref().map(|s| s.scene)
        {
            self.clear_sticky(socket, &[scene]).await?;
        }
        let Some(conversation) = self.conversation.clone() else {
            return Ok(());
        };
        let want_announce = conversation.settings.announce_job_done;
        let want_hud = conversation.settings.screen_preview_sync;
        if !want_announce && !want_hud {
            self.notices.pending_finished.clear();
            self.notices.pending_agent_notices.clear();
            return Ok(());
        }
        // 若用户正在说话或处于非实时拾音占线态，延后轮询以免丢失终态边沿或干扰拾音
        if self.listening_occupied() {
            return Ok(());
        }
        let context = ToolContext {
            device_id: self.device_id.to_owned(),
            session_id: self.session_id.to_owned(),
        };
        let snap = self
            .notices
            .jobs
            .poll_full_with(conversation.tools.as_ref(), &context, want_announce)
            .await;
        for job in snap.finished {
            enqueue_bounded_notice(&mut self.notices.pending_finished, job, |a, b| {
                a.workflow_id == b.workflow_id
            });
        }
        if want_announce {
            for notice in snap.agent_notices {
                enqueue_bounded_notice(&mut self.notices.pending_agent_notices, notice, |a, b| {
                    a.claim_key == b.claim_key
                });
            }
        } else {
            self.notices.pending_agent_notices.clear();
        }
        let can_color_hud = self.device_mcp.as_ref().is_some_and(|d| {
            d.has_tool("self.screen.preview_image") && d.screen_monochrome != Some(true)
        });
        let is_mono_oled = self
            .device_mcp
            .as_ref()
            .is_some_and(|d| d.screen_monochrome == Some(true));

        while let Some(job) = self.notices.pending_finished.pop_front() {
            if want_announce {
                if !self.shared.job_announcements.claim(&job.workflow_id) {
                    continue;
                }
                self.screen.preview_hold = None;
                self.screen.last_hud_workflow = None;
                self.screen.last_approval_summary = None;
                self.screen.engrave_preview = None;
                if want_hud && self.pack_active() {
                    self.show_finished_scene(socket, &job).await?;
                } else if want_hud
                    && can_color_hud
                    && let Some(badge_url) = self.stage_terminal_badge_url(&job).await
                {
                    self.screen.one_shot_badge_url = Some(badge_url.clone());
                    self.send_quiet_device_call(
                        socket,
                        "self.screen.preview_image",
                        json!({ "url": badge_url }),
                    )
                    .await?;
                }
                if self.mqtt && !socket.audio_open() {
                    // n71 常连模式没在对话：没有语音通道可播，改发提示（屏幕显示结果并响一声提示音）
                    let completed = job.status == "completed";
                    let emotion = if self.pack_active() {
                        crate::screen_pack::emotion_name(
                            self.shared.card_style(),
                            if completed {
                                crate::BotPose::CelebrateDone
                            } else {
                                crate::BotPose::HaltedShield
                            },
                        )
                    } else if completed {
                        "happy".into()
                    } else {
                        "sad".into()
                    };
                    let alert = ServerMessage::Alert {
                        status: if completed {
                            "雕刻完成"
                        } else {
                            "雕刻已停止"
                        }
                        .into(),
                        message: job.speech(),
                        emotion,
                    };
                    if !send_json(socket, &alert).await {
                        return Err("发送失败".into());
                    }
                    self.event("加工任务已结束：设备没在对话，已在屏幕上提示结果");
                    return Ok(());
                }
                self.event("加工任务已结束，播报结果");
                self.start_turn(
                    TurnInput::Say {
                        text: job.speech(),
                        close: false,
                    },
                    TurnKind::Notice,
                );
                return Ok(());
            } else if want_hud && self.pack_active() {
                self.screen.preview_hold = None;
                self.screen.last_hud_workflow = None;
                self.screen.engrave_preview = None;
                self.show_finished_scene(socket, &job).await?;
                return Ok(());
            } else if want_hud && can_color_hud {
                self.screen.preview_hold = None;
                self.screen.last_hud_workflow = None;
                if let Some(badge_url) = self.stage_terminal_badge_url(&job).await {
                    self.send_quiet_device_call(
                        socket,
                        "self.screen.preview_image",
                        json!({ "url": badge_url }),
                    )
                    .await?;
                }
                return Ok(());
            }
        }

        if want_announce {
            while let Some(notice) = self.notices.pending_agent_notices.pop_front() {
                if !self.shared.job_announcements.claim(&notice.claim_key) {
                    continue;
                }
                if self.mqtt && !socket.audio_open() {
                    let (status, emotion, pose) = match notice.status.as_str() {
                        "completed" => ("Agent 完成", "happy", crate::BotPose::CelebrateDone),
                        "failed" => ("Agent 失败", "sad", crate::BotPose::HaltedShield),
                        "cancelled" => ("Agent 已停止", "neutral", crate::BotPose::HaltedShield),
                        "stalled" => ("Agent 暂停输出", "thinking", crate::BotPose::Thinking),
                        _ => ("Agent 执行中", "thinking", crate::BotPose::Thinking),
                    };
                    let emotion = if self.pack_active() {
                        crate::screen_pack::emotion_name(self.shared.card_style(), pose)
                    } else {
                        emotion.into()
                    };
                    let alert = ServerMessage::Alert {
                        status: status.into(),
                        message: notice.speech,
                        emotion,
                    };
                    if !send_json(socket, &alert).await {
                        return Err("发送失败".into());
                    }
                    self.event("Agent 任务巡检：设备未开启语音通道，已发屏幕提示");
                    return Ok(());
                }
                self.event("Agent 任务巡检，语音播报进度");
                self.start_turn(
                    TurnInput::Say {
                        text: notice.speech,
                        close: false,
                    },
                    TurnKind::Notice,
                );
                return Ok(());
            }
        }

        if let Some(progress) = snap.active_job.filter(|_| want_hud) {
            self.screen.preview_hold = None;
            self.screen.last_approval_summary = None;
            let frame = self.screen.hud_frame;
            self.screen.hud_frame = self.screen.hud_frame.wrapping_add(1);
            // n70 开始雕刻后才取这个任务的刀路预览（每个任务只取一次）
            if self
                .screen
                .engrave_preview
                .as_ref()
                .is_none_or(|(id, _)| id != &progress.workflow_id)
            {
                let bytes = conversation
                    .tools
                    .workflow_preview(&context, &progress.workflow_id)
                    .await
                    .map(Arc::new);
                self.screen.engrave_preview = Some((progress.workflow_id.clone(), bytes));
            }
            let toolpath = self
                .screen
                .engrave_preview
                .as_ref()
                .and_then(|(_, b)| b.clone());
            if self.pack_active() {
                // 全屏画风：先把底层切到“雕刻”并叠一行进度字，再在中央盖刀路进度卡（有刀路时）
                self.set_sticky(
                    socket,
                    crate::BotPose::GogglesEngrave,
                    Some(format!("[雕刻中] {}", progress.subtitle())),
                    None,
                )
                .await?;
            }
            if can_color_hud && (toolpath.is_some() || !self.pack_active()) {
                let screen_size = self.device_mcp.as_ref().and_then(|d| d.screen_size);
                let (cw, ch) = crate::preview_push::optimal_canvas_for_screen(screen_size);
                let hud_spec = progress.to_hud_spec();
                let card_style = self.shared.card_style();
                let toolpath_for_render = toolpath.clone();
                let png =
                    tokio::task::spawn_blocking(move || match toolpath_for_render.as_deref() {
                        Some(source) => crate::preview_push::make_engrave_progress_png(
                            source, &hud_spec, card_style, frame, cw, ch,
                        )
                        .or_else(|_| {
                            crate::preview_push::make_job_hud_png_for_style(
                                &hud_spec, card_style, frame, cw, ch,
                            )
                        }),
                        None => crate::preview_push::make_job_hud_png_for_style(
                            &hud_spec, card_style, frame, cw, ch,
                        ),
                    })
                    .await
                    .map_err(|_| "渲染任务取消".to_owned())
                    .and_then(|r| r);
                if let Ok(png) = png
                    && let Ok(tok) = self.shared.preview_store.stage_prebuilt_png(png)
                {
                    let host = self.preview_http_host();
                    let url = format!(
                        "http://{host}:{}/mcp/preview/{tok}.png",
                        self.shared.http_port
                    );
                    let first_for_job = self
                        .screen
                        .last_hud_workflow
                        .as_ref()
                        .is_none_or(|(id, _)| id != &progress.workflow_id);
                    self.screen.last_hud_workflow =
                        Some((progress.workflow_id.clone(), progress.percent));
                    self.last_activity = Instant::now();
                    if first_for_job {
                        self.event("向小智彩屏开启实时激光加工进度副屏 HUD");
                    }
                    self.send_quiet_device_call(
                        socket,
                        "self.screen.preview_image",
                        json!({ "url": url }),
                    )
                    .await?;
                }
            } else if is_mono_oled {
                let should_refresh = match &self.screen.last_hud_workflow {
                    Some((id, prev_pct)) if id == &progress.workflow_id => {
                        progress.percent.abs_diff(*prev_pct) >= 10
                    }
                    _ => true,
                };
                if should_refresh {
                    self.screen.last_hud_workflow =
                        Some((progress.workflow_id.clone(), progress.percent));
                    self.last_activity = Instant::now();
                    let alert = ServerMessage::Alert {
                        status: "加工中".into(),
                        message: progress.subtitle(),
                        emotion: "cool".into(),
                    };
                    let _ = send_json(socket, &alert).await;
                }
            }
            return Ok(());
        }
        self.screen.last_hud_workflow = None;
        self.screen.engrave_preview = None;
        self.clear_sticky(socket, &[crate::BotPose::GogglesEngrave])
            .await?;

        if let Some(pending) = snap.pending_approval.filter(|_| want_hud) {
            self.screen.preview_hold = None;
            let frame = self.screen.hud_frame;
            self.screen.hud_frame = self.screen.hud_frame.wrapping_add(1);
            let first_for_approval =
                self.screen.last_approval_summary.as_deref() != Some(pending.summary.as_str());
            if self.pack_active() {
                // n70 全屏警戒画面一直显示，倒计时写在那行字里
                self.last_activity = Instant::now();
                if first_for_approval {
                    self.screen.last_approval_summary = Some(pending.summary.clone());
                    self.event("小智全屏切到待电脑批准画面");
                }
                self.set_sticky(
                    socket,
                    crate::BotPose::ApprovalWave,
                    Some(format!(
                        "[待电脑批准] {} · 剩 {} 秒",
                        pending.summary, pending.remaining_secs
                    )),
                    None,
                )
                .await?;
            } else if can_color_hud {
                let screen_size = self.device_mcp.as_ref().and_then(|d| d.screen_size);
                let (cw, ch) = crate::preview_push::optimal_canvas_for_screen(screen_size);
                let card_spec = pending.to_card_spec();
                let card_style = self.shared.card_style();
                let png = tokio::task::spawn_blocking(move || {
                    crate::preview_push::make_approval_card_png(
                        &card_spec, card_style, frame, cw, ch,
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
                    self.last_activity = Instant::now();
                    self.send_quiet_device_call(
                        socket,
                        "self.screen.preview_image",
                        json!({ "url": url }),
                    )
                    .await?;
                    if first_for_approval {
                        self.screen.last_approval_summary = Some(pending.summary.clone());
                        self.event("向小智彩屏推送待电脑审批警戒卡");
                        let _ = self
                            .send_stt(socket, format!("[待电脑批准] {}", pending.summary))
                            .await;
                    }
                }
            } else if is_mono_oled && first_for_approval {
                self.screen.last_approval_summary = Some(pending.summary.clone());
                self.last_activity = Instant::now();
                let _ = send_json(
                    socket,
                    &ServerMessage::Alert {
                        status: "待电脑批准".into(),
                        message: pending.summary,
                        emotion: "thinking".into(),
                    },
                )
                .await;
            }
            return Ok(());
        }
        self.screen.last_approval_summary = None;
        self.clear_sticky(socket, &[crate::BotPose::ApprovalWave])
            .await?;
        Ok(())
    }

    /// n70 加工结束：全屏切到完工 / 急停画面 10 秒，叠一行结果；之后回到聆听或待命。
    async fn show_finished_scene(
        &mut self,
        socket: &mut crate::link::Link,
        job: &crate::jobs::FinishedJob,
    ) -> Result<(), String> {
        let (scene, text) = if job.status == "completed" {
            (
                crate::BotPose::CelebrateDone,
                format!("[雕刻完成] 共 {} 行", job.total_lines),
            )
        } else {
            (
                crate::BotPose::HaltedShield,
                format!("[已停止] 停在第 {} 行", job.lines_sent),
            )
        };
        self.set_sticky(socket, scene, Some(text), Some(FINISHED_SCENE_HOLD))
            .await
    }
}
