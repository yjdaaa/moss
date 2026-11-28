//! 设备屏幕：全屏画风常驻场景、预览续期与音乐 HUD。

use super::*;

impl<'a> Session<'a> {
    /// n70 这台设备是否按全屏画风包显示（单色屏不算）。
    pub(super) fn pack_active(&self) -> bool {
        self.screen.screen_pack
            && self
                .device_mcp
                .as_ref()
                .is_none_or(|d| d.screen_monochrome != Some(true))
    }

    /// n70 当前有效的常驻场景（到期的完工 / 急停徽章、已停播的音乐不算）。
    fn sticky_scene(&self) -> Option<&ScreenSceneState> {
        let scene = self.screen.screen_scene.as_ref()?;
        if scene.until.is_some_and(|t| Instant::now() >= t) {
            return None;
        }
        if scene.scene == crate::BotPose::MusicGroove
            && !self.music.as_ref().is_some_and(|m| m.active())
        {
            return None;
        }
        Some(scene)
    }

    /// n70 说话时也保持的常驻场景：听歌、完工、急停。
    pub(super) fn sticky_while_talking(&self) -> Option<crate::BotPose> {
        self.sticky_scene().map(|s| s.scene).filter(|scene| {
            matches!(
                scene,
                crate::BotPose::MusicGroove
                    | crate::BotPose::CelebrateDone
                    | crate::BotPose::HaltedShield
            )
        })
    }

    /// n70 让设备切到当前画风的某个全屏场景（发 `llm` 表情名）。没刷画风包的设备什么也不发。
    pub(super) async fn show_screen(
        &mut self,
        socket: &mut crate::link::Link,
        scene: crate::BotPose,
        force: bool,
    ) -> Result<(), String> {
        if !self.pack_active() || self.current_turn_silent() {
            return Ok(());
        }
        let name = crate::screen_pack::emotion_name(self.shared.card_style(), scene);
        if !force && self.screen.last_screen_emotion.as_deref() == Some(name.as_str()) {
            return Ok(());
        }
        let message = ServerMessage::Llm {
            session_id: self.session_id.into(),
            text: name.clone(),
            emotion: name.clone(),
        };
        if !send_json(socket, &message).await {
            return Err("发送失败".into());
        }
        self.screen.last_screen_emotion = Some(name);
        Ok(())
    }

    /// n70 在全屏画面上叠一行字（固件字幕位置）。
    async fn screen_text(
        &mut self,
        socket: &mut crate::link::Link,
        text: String,
    ) -> Result<(), String> {
        if self.send_stt(socket, text).await {
            Ok(())
        } else {
            Err("发送失败".into())
        }
    }

    /// n70 回到常驻场景（没有就显示 `fallback`）并重发那行字；设备状态切换（固件会重置成 `neutral`）后调用。
    pub(super) async fn show_ambient(
        &mut self,
        socket: &mut crate::link::Link,
        fallback: crate::BotPose,
    ) -> Result<(), String> {
        if !self.pack_active() {
            return Ok(());
        }
        let (scene, text) = match self.sticky_scene() {
            Some(sticky) => (sticky.scene, sticky.text.clone()),
            None => (fallback, None),
        };
        self.show_screen(socket, scene, true).await?;
        if let Some(text) = text {
            self.screen_text(socket, text).await?;
        }
        Ok(())
    }

    /// n70 设定常驻场景；画面或那行字变了才发给设备。
    pub(super) async fn set_sticky(
        &mut self,
        socket: &mut crate::link::Link,
        scene: crate::BotPose,
        text: Option<String>,
        ttl: Option<Duration>,
    ) -> Result<(), String> {
        let text_changed = self
            .screen
            .screen_scene
            .as_ref()
            .is_none_or(|old| old.scene != scene || old.text != text);
        self.screen.screen_scene = Some(ScreenSceneState {
            scene,
            text: text.clone(),
            until: ttl.map(|d| Instant::now() + d),
        });
        self.show_screen(socket, scene, false).await?;
        if text_changed && let Some(text) = text {
            self.screen_text(socket, text).await?;
        }
        Ok(())
    }

    /// n70 结束雕刻 / 待批准这类常驻场景，回到聆听或待命画面。
    pub(super) async fn clear_sticky(
        &mut self,
        socket: &mut crate::link::Link,
        scenes: &[crate::BotPose],
    ) -> Result<(), String> {
        if !self
            .screen
            .screen_scene
            .as_ref()
            .is_some_and(|s| scenes.contains(&s.scene))
        {
            return Ok(());
        }
        self.screen.screen_scene = None;
        if !self.busy() {
            let fallback = if matches!(self.phase, Phase::Listening(_)) {
                crate::BotPose::Listening
            } else {
                crate::BotPose::IdleBlink
            };
            self.show_screen(socket, fallback, false).await?;
        }
        Ok(())
    }

    pub(super) async fn send_frame(
        &mut self,
        socket: &mut crate::link::Link,
        payload: &[u8],
    ) -> Result<(), String> {
        if self.current_turn_silent() {
            return Ok(());
        }
        let frame = encode_frame(self.version, payload, self.timestamp)
            .map_err(|error| error.to_string())?;
        self.timestamp = self.timestamp.wrapping_add(FRAME_MS as u32);
        socket
            .send(Message::Binary(frame.into()))
            .await
            .map_err(|_| "发送失败".to_owned())
    }

    pub(super) fn preview_http_host(&self) -> String {
        self.vision_host
            .clone()
            .or_else(|| crate::ota::local_ip().map(|ip| ip.to_string()))
            .unwrap_or_else(|| "127.0.0.1".into())
    }

    /// 彩屏预览确认窗口内的 4 秒无声续期：重置官方固件 5000ms `preview_timer_`，用户一旦开口或到期立即停止。
    pub(super) async fn on_preview_hold_tick(
        &mut self,
        socket: &mut crate::link::Link,
    ) -> Result<(), String> {
        let now = Instant::now();
        let speaking = self.actively_speaking_user();
        let Some(hold) = self.screen.preview_hold.as_mut() else {
            return Ok(());
        };
        if now >= hold.until || speaking {
            self.screen.preview_hold = None;
            return Ok(());
        }
        let url = hold.url.clone();
        hold.next_refresh = now + self.shared.config.preview_hold_interval;
        if !self.busy() {
            self.send_quiet_device_call(socket, "self.screen.preview_image", json!({ "url": url }))
                .await?;
        }
        Ok(())
    }

    /// 本地音乐播放期间每 4 秒刷新一次黑胶旋转 / 护目镜小机器人听歌摇摆帧；停播自动停止。
    pub(super) async fn on_music_hud_tick(
        &mut self,
        socket: &mut crate::link::Link,
    ) -> Result<(), String> {
        let music_active = self.music.as_ref().is_some_and(|m| m.active());
        let can_color = self.device_mcp.as_ref().is_some_and(|d| {
            d.has_tool("self.screen.preview_image") && d.screen_monochrome != Some(true)
        });
        if !music_active || !can_color {
            self.screen.music_hud = None;
            return Ok(());
        }
        let Some((title_ascii, frame)) = self.screen.music_hud.as_mut().map(|hud| {
            let current = hud.frame;
            hud.frame = hud.frame.wrapping_add(1);
            hud.next_refresh = Instant::now() + self.shared.config.preview_hold_interval;
            (hud.title_ascii.clone(), current)
        }) else {
            return Ok(());
        };
        let screen_size = self.device_mcp.as_ref().and_then(|d| d.screen_size);
        let (cw, ch) = crate::preview_push::optimal_canvas_for_screen(screen_size);
        let card_style = self.shared.card_style();
        let png = tokio::task::spawn_blocking(move || {
            crate::preview_push::make_music_card_png(
                &crate::preview_push::MusicCardSpec { title_ascii },
                card_style,
                frame,
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
            self.send_quiet_device_call(socket, "self.screen.preview_image", json!({ "url": url }))
                .await?;
        }
        Ok(())
    }
}
