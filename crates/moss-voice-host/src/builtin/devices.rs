//! 设备登记与授权（DbRegistry）及设备状态、改名与即时请求命令。

use super::*;

/// 设备登记与密钥：存 MOSS 的 SQLite。
pub(super) struct DbRegistry {
    pub(super) db: Arc<Mutex<laser_runtime::RuntimeDb>>,
    pub(super) host: Arc<laser_host::HostApplication>,
    pub(super) settings_file: std::path::PathBuf,
    pub(super) chat_log_enabled: Arc<std::sync::atomic::AtomicBool>,
    pub(super) chat_log_epoch: Arc<std::sync::atomic::AtomicU64>,
}

pub(super) fn ensure_device_token_secret(db: &laser_runtime::RuntimeDb) -> Result<Vec<u8>, String> {
    if let Some(secret) = db.get_secret(TOKEN_SECRET_NAME).map_err(db_error)?
        && secret.len() >= 32
    {
        return Ok(secret.into_bytes());
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| format!("生成设备密钥失败：{error}"))?;
    let secret: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    db.set_secret(TOKEN_SECRET_NAME, &secret, now())
        .map_err(db_error)?;
    Ok(secret.into_bytes())
}

/// 允许设备后的免 token 直连宽限期（7 天，单位秒）。
/// 设备在 `pending` 态跑过开机 OTA 未获发 token，用户在桌面端点击「允许」后无需按硬件重启键即可在宽限期内唤醒直连；
/// 一旦设备在 `decided_at` 之后完成首次已绑定的开机 OTA（`ota.rs` 写入 `VOICE_SNAPSHOT_INFO` 快照并签发 token）或距 `decided_at` 超过 7 天，
/// 宽限期自动关闭，防止无 token 连接长期绕过 HMAC token 与 30 天过期校验。
pub(super) const TOKENLESS_GRACE_SECS: f64 = 7.0 * 24.0 * 60.0 * 60.0;

impl DeviceRegistry for DbRegistry {
    fn seen(&self, visit: &DeviceVisit) -> Result<DeviceStatus, String> {
        let auto_allow = moss_voice::settings::load(&self.settings_file)
            .builtin_local
            .auto_allow_devices;
        let db = self.db.lock().map_err(|_| "数据库锁失败".to_owned())?;
        let is_new = db
            .voice_device(&visit.device_id)
            .map_err(db_error)?
            .is_none();
        let mut record = db
            .voice_device_seen(
                &visit.device_id,
                &visit.client_id,
                &visit.model,
                &visit.firmware_version,
                &visit.ip,
                now(),
            )
            .map_err(db_error)?;
        if is_new && record.status == laser_runtime::VOICE_DEVICE_PENDING && auto_allow {
            let ts = now();
            if db
                .set_voice_device_status(&visit.device_id, laser_runtime::VOICE_DEVICE_ALLOWED, ts)
                .map_err(db_error)?
            {
                record.status = laser_runtime::VOICE_DEVICE_ALLOWED.to_owned();
                record.decided_at = Some(ts);
            }
        }
        drop(db);
        if !record.is_allowed() {
            self.host
                .api_service
                .revoke_voice_sessions(&visit.device_id);
        }
        Ok(match record.status.as_str() {
            laser_runtime::VOICE_DEVICE_ALLOWED => DeviceStatus::Allowed,
            laser_runtime::VOICE_DEVICE_REVOKED => DeviceStatus::Revoked,
            _ => DeviceStatus::Pending,
        })
    }

    fn authorize_http(&self, device: &str, client: &str) -> Result<Option<u64>, String> {
        let db = self.db.lock().map_err(|_| "数据库锁失败".to_string())?;
        db.voice_device(device)
            .map(|row| {
                row.and_then(|row| {
                    (row.is_allowed() && row.client_id == client)
                        .then_some(row.decided_at?.to_bits())
                })
            })
            .map_err(db_error)
    }

    fn allow_tokenless_session(&self, device: &str, client: &str) -> bool {
        if client.trim().is_empty() {
            return false;
        }
        let Ok(db) = self.db.lock() else {
            return false;
        };
        let Ok(Some(row)) = db.voice_device(device) else {
            return false;
        };
        if !row.is_allowed() || row.client_id.trim().is_empty() || row.client_id != client {
            return false;
        }
        let Some(decided_at) = row.decided_at.filter(|ts| ts.is_finite() && *ts > 0.0) else {
            return false;
        };
        let current = now();
        if current < decided_at - 60.0 || current - decided_at > TOKENLESS_GRACE_SECS {
            return false;
        }
        row.info_at
            .is_none_or(|info_at| info_at.is_finite() && info_at < decided_at)
    }
    fn open_session(
        &self,
        device: &str,
        client: &str,
        session: &str,
    ) -> Result<Arc<std::sync::atomic::AtomicBool>, String> {
        self.host
            .api_service
            .register_voice_session(device, client, session)
    }
    fn close_session(&self, session: &str) {
        self.host.api_service.unregister_voice_session(session);
    }

    fn token_secret(&self) -> Result<Vec<u8>, String> {
        let db = self.db.lock().map_err(|_| "数据库锁失败".to_owned())?;
        ensure_device_token_secret(&db)
    }

    fn sessions_changed(&self, count: usize) {
        laser_host::set_voice_device_sessions(
            &self.host.api_service,
            Some(u32::try_from(count).unwrap_or(u32::MAX)),
        );
    }

    fn pulse_changed(&self, phase: Option<&str>, level: u8) {
        laser_host::set_voice_pulse(&self.host.api_service, phase, level);
    }
    /// 2026-11-02：设备快照（开机 OTA 上报 / hello / 实时状态）只存原始 JSON，
    /// 固件字段变化时不改表结构。
    fn device_snapshot(&self, device: &str, kind: &str, json: &str) {
        if let Ok(db) = self.db.lock() {
            let _ = db.voice_device_snapshot(device, kind, json, now());
        }
    }

    fn device_hardware_prefs(&self, device: &str) -> Option<moss_voice_server::HardwarePrefs> {
        let mut prefs = moss_voice::settings::load(&self.settings_file).builtin_device_prefs;
        let entry = prefs.remove(device)?;
        (!entry.is_empty()).then_some(moss_voice_server::HardwarePrefs {
            volume: entry.volume,
            brightness: entry.brightness,
            theme: entry.theme,
            talk_mode: entry.talk_mode,
            screen_pack: entry.screen_pack,
        })
    }

    fn device_text_font(&self, device: &str) -> Option<moss_voice_server::TextFontCapability> {
        let db = self.db.lock().ok()?;
        let row = db.voice_device(device).ok()??;
        let hello = row.hello_json.as_deref()?;
        moss_voice_server::glyph_font::text_font_from_hello_snapshot(hello)
    }

    /// 2026-11-09：设备整包下载完资源包，才把“等下载”的全屏画风转为已装（重启重连后按它切画面）。
    fn assets_downloaded(&self, device: &str) {
        let _ = moss_voice::settings::update_voice(&self.settings_file, |s| {
            if let Some(entry) = s.builtin_device_prefs.get_mut(device) {
                entry.confirm_assets_downloaded();
            }
            Ok(())
        });
    }

    /// n81：将已脱敏的语音对话轮次持久化写入本机 SQLite `voice_chat_logs`（上限 500 条）。
    fn record_chat_turn(&self, trace: &moss_voice_server::TurnDebugTrace) {
        let epoch = self
            .chat_log_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        self.record_chat_turn_at(trace, epoch);
    }

    fn record_chat_turn_at(&self, trace: &moss_voice_server::TurnDebugTrace, captured_epoch: u64) {
        if !self
            .chat_log_enabled
            .load(std::sync::atomic::Ordering::Acquire)
            || captured_epoch
                != self
                    .chat_log_epoch
                    .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }
        // 在拿数据库锁之前读盘，避免持 db.lock() 期间阻塞在文件系统 I/O 上
        if !moss_voice::settings::load(&self.settings_file)
            .builtin_local
            .chat_log_enabled
        {
            return;
        }
        let scrubbed_tools: Vec<moss_voice_server::DebugToolTrace> = trace
            .tools
            .iter()
            .map(|t| moss_voice_server::DebugToolTrace {
                name: t.name.clone(),
                arguments: moss_voice_server::scrub_debug_text(&t.arguments),
                result: moss_voice_server::scrub_debug_text(&t.result),
                blocked: t.blocked,
                block_reason: t
                    .block_reason
                    .as_deref()
                    .map(moss_voice_server::scrub_debug_text),
            })
            .collect();
        let tools_json = serde_json::to_string(&scrubbed_tools).unwrap_or_else(|_| "[]".to_owned());
        let record = laser_runtime::VoiceChatLogRecord {
            id: 0,
            at_ms: i64::try_from(trace.at_ms).unwrap_or(i64::MAX),
            device_id: trace.device_id.clone(),
            session_id: trace.session_id.clone(),
            turn: i64::try_from(trace.turn).unwrap_or(i64::MAX),
            raw_asr: moss_voice_server::scrub_debug_text(&trace.raw_asr),
            corrected_asr: moss_voice_server::scrub_debug_text(&trace.corrected_asr),
            reply: moss_voice_server::scrub_debug_text(&trace.reply),
            spoken: moss_voice_server::scrub_debug_text(&trace.spoken),
            tools_json,
            outcome: trace.outcome.clone(),
            truncated: trace.truncated,
            tools_dropped: i64::try_from(trace.tools_dropped).unwrap_or(0),
            simulated: trace.simulated,
            bench_window: trace.bench_window,
        };
        if let Ok(db) = self.db.lock() {
            // 拿锁后二次核验开关与代次，保证与清空/关闭时的 db.lock() 屏障严格串行
            if !self
                .chat_log_enabled
                .load(std::sync::atomic::Ordering::Acquire)
                || captured_epoch
                    != self
                        .chat_log_epoch
                        .load(std::sync::atomic::Ordering::Acquire)
            {
                return;
            }
            let _ = db.append_voice_chat_log(&record);
        }
    }
}

impl BuiltinVoice {
    pub(super) fn devices(&self) -> Value {
        self.host
            .db
            .lock()
            .ok()
            .and_then(|db| db.voice_devices().ok())
            .map(|devices| serde_json::to_value(devices).unwrap_or_default())
            .unwrap_or_else(|| Value::Array(Vec::new()))
    }
    fn is_device_allowed(&self, device_id: &str) -> bool {
        self.host
            .db
            .lock()
            .ok()
            .and_then(|db| db.voice_device(device_id).ok().flatten())
            .is_some_and(|d| d.is_allowed())
    }

    /// 2026-11-02：桌面端直接向一台已连接设备发请求（`refresh_info` / `reboot`），不经过模型。
    pub fn device_request(
        &self,
        device_id: &str,
        request: &str,
        value: Option<&Value>,
    ) -> Result<Value, CenterError> {
        let device_id = device_id.trim();
        let label = request.trim();
        let invalid = |msg: &str| CenterError::new("voice_device_invalid", 400, msg);
        if device_id.is_empty() || device_id.len() > 128 {
            return Err(invalid("设备编号不能为空"));
        }
        if label == "capture_photo" {
            if !self.is_device_allowed(device_id) {
                if let Ok(guard) = self.server.lock()
                    && let Some(server) = guard.as_ref()
                {
                    server.clear_camera_frame(device_id);
                }
                return Err(CenterError::new(
                    "voice_device_forbidden",
                    403,
                    "该设备尚未在语音中心允许连接或已被撤销",
                ));
            }
            let server = self
                .server
                .lock()
                .map_err(|_| internal("内置语音服务状态锁失败"))?;
            let server = server.as_ref().ok_or_else(|| {
                CenterError::new("voice_builtin_stopped", 409, "内置语音服务未运行")
            })?;
            let ticket_seq = server
                .request_camera_capture(device_id)
                .map_err(|error| CenterError::new("voice_device_offline", 409, error))?;
            return Ok(serde_json::json!({
                "requested": label,
                "ticket_seq": ticket_seq,
            }));
        }
        if label == "camera_frame" {
            let after_seq = value
                .and_then(|v| v.get("after_seq").or(Some(v)))
                .and_then(Value::as_u64);
            let allowed = self.is_device_allowed(device_id);
            let server = self
                .server
                .lock()
                .map_err(|_| internal("内置语音服务状态锁失败"))?;
            let Some(server) = server.as_ref() else {
                return Ok(serde_json::json!({
                    "running": false,
                    "online": false,
                    "session_state": null,
                    "has_camera_tool": null,
                    "unchanged": false,
                    "pending": false,
                    "last_error": null,
                    "error_seq": null,
                    "frame_seq": null,
                    "frame": null,
                }));
            };
            if !allowed {
                server.clear_camera_frame(device_id);
                return Ok(serde_json::json!({
                    "running": true,
                    "online": false,
                    "session_state": null,
                    "has_camera_tool": null,
                    "unchanged": false,
                    "pending": false,
                    "last_error": null,
                    "error_seq": null,
                    "frame_seq": null,
                    "frame": null,
                }));
            }
            return Ok(server.camera_frame_view(device_id, after_seq));
        }
        if label == "clear_camera_frame" {
            let server = self
                .server
                .lock()
                .map_err(|_| internal("内置语音服务状态锁失败"))?;
            if let Some(server) = server.as_ref() {
                server.clear_camera_frame(device_id);
            }
            return Ok(serde_json::json!({ "requested": label }));
        }
        if label == "push_preview" {
            let (bytes, summary, ruler) = extract_preview_bundle_for_host(&self.host, None)
                .map_err(|msg| CenterError::new("voice_preview_missing", 404, msg))?;
            let server = self
                .server
                .lock()
                .map_err(|_| internal("内置语音服务状态锁失败"))?;
            let server = server.as_ref().ok_or_else(|| {
                CenterError::new("voice_builtin_stopped", 409, "内置语音服务未运行")
            })?;
            server
                .push_preview_adaptive_with_ruler(
                    bytes.as_deref(),
                    &summary,
                    ruler.as_deref(),
                    Some(device_id),
                )
                .map_err(|error| CenterError::new("voice_device_offline", 409, error))?;
            return Ok(serde_json::json!({ "requested": label, "summary": summary }));
        }
        if label == "inject_text" {
            let text = value
                .and_then(|v| v.get("text").or(Some(v)))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| invalid("请先输入要发送的测试文字"))?;
            return self.inject_text_for_device(device_id, text);
        }
        if label == "close_bench_window" {
            let server = self
                .server
                .lock()
                .map_err(|_| internal("内置语音服务状态锁失败"))?;
            let closed = server.as_ref().is_some_and(|s| {
                s.close_bench_window(
                    device_id,
                    moss_voice_server::BenchWindowCloseReason::UserStopped,
                    "桌面手动结束",
                )
            });
            return Ok(serde_json::json!({
                "requested": label,
                "device_id": device_id,
                "closed": closed,
            }));
        }
        if label == "push_bot_pose" {
            let raw_pose = value
                .and_then(|v| v.get("pose").or(Some(v)))
                .and_then(Value::as_str)
                .unwrap_or("idle");
            let pose = moss_voice_server::BotPose::from_slug(raw_pose).ok_or_else(|| {
                invalid("小机器人动作只能是 idle、engrave、approval、music、celebrate 或 halted")
            })?;
            let style_override = value
                .and_then(|v| v.get("style"))
                .and_then(Value::as_str)
                .map(moss_voice_server::ScreenCardStyle::from_setting);
            let server = self
                .server
                .lock()
                .map_err(|_| internal("内置语音服务状态锁失败"))?;
            let server = server.as_ref().ok_or_else(|| {
                CenterError::new("voice_builtin_stopped", 409, "内置语音服务未运行")
            })?;
            let effective_style = style_override.unwrap_or_else(|| server.screen_card_style());
            server
                .push_scene_card(pose, Some(effective_style), Some(device_id))
                .map_err(|error| CenterError::new("voice_device_offline", 409, error))?;
            return Ok(serde_json::json!({
                "requested": label,
                "pose": pose.slug(),
                "style": effective_style.as_str(),
            }));
        }
        let parsed = match label {
            "refresh_info" => moss_voice_server::DeviceRequest::RefreshInfo,
            "reboot" => moss_voice_server::DeviceRequest::Reboot,
            "set_volume" => {
                let vol = value
                    .and_then(|v| v.get("volume").or(Some(v)))
                    .and_then(Value::as_u64)
                    .filter(|v| *v <= 100)
                    .ok_or_else(|| invalid("音量须为 0 ~ 100 的整数"))?
                    as u8;
                moss_voice::settings::update_voice(&self.settings_file(), |s| {
                    s.builtin_device_prefs
                        .entry(device_id.to_owned())
                        .or_default()
                        .volume = Some(vol);
                    Ok(())
                })
                .map_err(|msg| CenterError::new("voice_settings_write_failed", 500, msg))?;
                moss_voice_server::DeviceRequest::SetVolume(vol)
            }
            "set_brightness" => {
                let bri = value
                    .and_then(|v| v.get("brightness").or(Some(v)))
                    .and_then(Value::as_u64)
                    .filter(|v| *v <= 100)
                    .ok_or_else(|| invalid("屏幕亮度须为 0 ~ 100 的整数"))?
                    as u8;
                moss_voice::settings::update_voice(&self.settings_file(), |s| {
                    s.builtin_device_prefs
                        .entry(device_id.to_owned())
                        .or_default()
                        .brightness = Some(bri);
                    Ok(())
                })
                .map_err(|msg| CenterError::new("voice_settings_write_failed", 500, msg))?;
                moss_voice_server::DeviceRequest::SetBrightness(bri)
            }
            "set_theme" => {
                let raw_theme = value
                    .and_then(|v| v.get("theme").or(Some(v)))
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("屏幕主题须为 light 或 dark"))?;
                let theme = moss_voice::settings::validate_device_theme(raw_theme)
                    .map_err(|msg| invalid(&msg))?;
                moss_voice::settings::update_voice(&self.settings_file(), |s| {
                    s.builtin_device_prefs
                        .entry(device_id.to_owned())
                        .or_default()
                        .theme = Some(theme.clone());
                    Ok(())
                })
                .map_err(|msg| CenterError::new("voice_settings_write_failed", 500, msg))?;
                moss_voice_server::DeviceRequest::SetTheme(theme)
            }
            "set_talk_mode" => {
                let raw_mode = value
                    .and_then(|v| v.get("mode").or(Some(v)))
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("按键模式须为 click_to_talk 或 press_to_talk"))?;
                let mode = moss_voice::settings::validate_talk_mode(raw_mode)
                    .map_err(|msg| invalid(&msg))?;
                moss_voice::settings::update_voice(&self.settings_file(), |s| {
                    s.builtin_device_prefs
                        .entry(device_id.to_owned())
                        .or_default()
                        .talk_mode = Some(mode.clone());
                    Ok(())
                })
                .map_err(|msg| CenterError::new("voice_settings_write_failed", 500, msg))?;
                moss_voice_server::DeviceRequest::SetTalkMode(mode)
            }
            _ => {
                return Err(invalid(
                    "设备请求只能是 refresh_info、reboot、set_volume、set_brightness、set_theme、set_talk_mode、push_preview、push_bot_pose、inject_text、close_bench_window、capture_photo、camera_frame 或 clear_camera_frame",
                ));
            }
        };
        let server = self
            .server
            .lock()
            .map_err(|_| internal("内置语音服务状态锁失败"))?;
        let server = server
            .as_ref()
            .ok_or_else(|| CenterError::new("voice_builtin_stopped", 409, "内置语音服务未运行"))?;
        server
            .device_request(device_id, parsed)
            .map_err(|error| CenterError::new("voice_device_offline", 409, error))?;
        Ok(serde_json::json!({ "requested": label }))
    }

    pub fn set_device_status(&self, device_id: &str, status: &str) -> Result<Value, CenterError> {
        let device_id = device_id.trim();
        if device_id.is_empty() || device_id.len() > 128 {
            return Err(CenterError::new(
                "voice_device_invalid",
                400,
                "设备编号不能为空",
            ));
        }
        let status = match status.trim() {
            "allowed" => laser_runtime::VOICE_DEVICE_ALLOWED,
            "revoked" => laser_runtime::VOICE_DEVICE_REVOKED,
            _ => {
                return Err(CenterError::new(
                    "voice_device_invalid",
                    400,
                    "设备状态只能是 allowed 或 revoked",
                ));
            }
        };
        let found = self
            .personal
            .device_changed(device_id, || {
                self.memory.device_changed(device_id, || {
                    self.host
                        .api_service
                        .set_voice_device_status(device_id, status, now())
                })
            })
            .map_err(internal)?;
        self.context.device_changed(device_id);
        if status != laser_runtime::VOICE_DEVICE_ALLOWED
            && let Ok(guard) = self.server.lock()
            && let Some(server) = guard.as_ref()
        {
            server.clear_camera_frame(device_id);
        }
        if !found {
            return Err(CenterError::new(
                "voice_device_not_found",
                404,
                "找不到该设备",
            ));
        }
        Ok(self.status())
    }

    pub fn rename_device(&self, device_id: &str, name: &str) -> Result<Value, CenterError> {
        let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
        let found = db
            .rename_voice_device(device_id, name)
            .map_err(db_failure)?;
        drop(db);
        if !found {
            return Err(CenterError::new(
                "voice_device_not_found",
                404,
                "找不到该设备",
            ));
        }
        Ok(self.status())
    }

    /// n134：移除已允许（`allowed`）或待允许（`pending`）设备；已撤销（`revoked`）设备拒绝移除以免绕过撤销。
    pub fn remove_device(&self, device_id: &str) -> Result<Value, CenterError> {
        let device_id = device_id.trim();
        if device_id.is_empty() || device_id.len() > 128 {
            return Err(CenterError::new(
                "voice_device_invalid",
                400,
                "设备编号不能为空",
            ));
        }
        let outcome = self
            .personal
            .device_changed(device_id, || {
                self.memory.device_changed(device_id, || {
                    let db = self
                        .host
                        .db
                        .lock()
                        .map_err(|_| "语音设备数据库不可用".to_owned())?;
                    let outcome = db.delete_voice_device(device_id, now()).map_err(db_error)?;
                    if outcome == laser_runtime::DeleteVoiceDeviceOutcome::Deleted {
                        self.host.api_service.revoke_voice_sessions(device_id);
                    }
                    Ok(outcome)
                })
            })
            .map_err(internal)?;
        self.context.device_changed(device_id);
        match outcome {
            laser_runtime::DeleteVoiceDeviceOutcome::Revoked => {
                return Err(CenterError::new(
                    "voice_device_revoked",
                    409,
                    "已撤销的设备不能移除，以免绕过撤销限制；如需恢复连接请点“允许”",
                ));
            }
            laser_runtime::DeleteVoiceDeviceOutcome::NotFound => {
                return Err(CenterError::new(
                    "voice_device_not_found",
                    404,
                    "找不到该设备",
                ));
            }
            laser_runtime::DeleteVoiceDeviceOutcome::Deleted => {}
        }
        if let Ok(guard) = self.server.lock()
            && let Some(server) = guard.as_ref()
        {
            server.clear_device_state(device_id);
        }
        let settings_file = self.settings_file();
        if moss_voice::settings::load(&settings_file)
            .builtin_device_prefs
            .contains_key(device_id)
        {
            moss_voice::settings::update_voice(&settings_file, |s| {
                s.builtin_device_prefs.remove(device_id);
                Ok(())
            })
            .map_err(|msg| CenterError::new("voice_settings_write_failed", 500, msg))?;
        }
        Ok(self.status())
    }
}
