//! 云端模型配置的测试、列表与保存，本地模型配置保存与下载、取消、删除。

use super::*;

/// 测试豆包识别或合成（NV7）：识别送 1 秒低音量正弦音，合成一句短话。结果与错误都不含密钥。
fn test_doubao(resolved: &moss_voice::models::ResolvedSection) -> moss_voice::TestResult {
    let started = std::time::Instant::now();
    let outcome: Result<(String, Option<String>), moss_voice_providers::ProviderError> =
        crate::block_on(async {
            match resolved.section {
                Section::Asr => {
                    let asr = DoubaoAsr::new(&resolved.url, &resolved.api_key, &resolved.model)?;
                    let tone: Vec<i16> = (0..3_200)
                        .map(|index| {
                            ((index as f64 / 16_000.0 * 440.0 * std::f64::consts::TAU).sin()
                                * 1200.0) as i16
                        })
                        .collect();
                    asr.transcribe(&tone, 16_000)
                        .await
                        .map(|_| ("连接成功，识别接口已返回结果（流式）".to_owned(), None))
                }
                _ => {
                    let tts = DoubaoTts::new(
                        &resolved.url,
                        &resolved.api_key,
                        &resolved.model,
                        &resolved.voice,
                        resolved.speed as f32,
                    )?;
                    let pcm = tts.synthesize("你好。").await?;
                    let seconds = pcm.samples.len() as f64 / pcm.sample_rate as f64;
                    let wav = moss_voice_providers::wav::encode_wav(&pcm.samples, pcm.sample_rate);
                    let data_url = moss_voice::models::audio_data_url(&wav, "audio/wav");
                    Ok((
                        format!("连接成功，合成了 {seconds:.1} 秒音频（流式）"),
                        Some(data_url),
                    ))
                }
            }
        });
    let (ok, detail, audio_data_url) = match outcome {
        Ok((detail, audio_data_url)) => (true, detail, audio_data_url),
        Err(error) => (false, error.to_string(), None),
    };
    moss_voice::TestResult {
        section: resolved.section,
        ok,
        elapsed_ms: started.elapsed().as_millis() as u64,
        http_status: None,
        final_url: moss_voice::redact::redact_line(&resolved.url),
        detail,
        audio_data_url,
        thinking_off: None,
    }
}

/// 测试千问 / 百炼原生流式语音合成（n123）：合成短句并返回 WAV Data URL 供页面直接试听，不含密钥。
fn test_dashscope(resolved: &moss_voice::models::ResolvedSection) -> moss_voice::TestResult {
    let started = std::time::Instant::now();
    let outcome: Result<(String, Option<String>), moss_voice_providers::ProviderError> =
        crate::block_on(async {
            let tts = DashscopeTts::new(
                &resolved.url,
                &resolved.api_key,
                &resolved.model,
                &resolved.voice,
                resolved.speed as f32,
            )?;
            let pcm = tts.synthesize("你好，我是 MOSS。").await?;
            let seconds = pcm.samples.len() as f64 / pcm.sample_rate as f64;
            let wav = moss_voice_providers::wav::encode_wav(&pcm.samples, pcm.sample_rate);
            let data_url = moss_voice::models::audio_data_url(&wav, "audio/wav");
            Ok((
                format!("连接成功，合成了 {seconds:.1} 秒音频（流式）"),
                Some(data_url),
            ))
        });
    let (ok, detail, audio_data_url) = match outcome {
        Ok((detail, audio_data_url)) => (true, detail, audio_data_url),
        Err(error) => (false, error.to_string(), None),
    };
    moss_voice::TestResult {
        section: resolved.section,
        ok,
        elapsed_ms: started.elapsed().as_millis() as u64,
        http_status: None,
        final_url: moss_voice::redact::redact_line(&resolved.url),
        detail,
        audio_data_url,
        thinking_off: None,
    }
}

impl BuiltinVoice {
    /// 保存本地模型开关、音色、语速与镜像；运行中则重启服务以生效。
    pub fn local_save(&self, patch: &Value) -> Result<Value, CenterError> {
        let invalid = |message: &str| CenterError::new("voice_settings_invalid", 400, message);
        if let Some(cli_action) = patch.get("colab_cli") {
            self.handle_colab_cli_action(cli_action)?;
            let only_cli = patch
                .as_object()
                .is_some_and(|obj| obj.keys().all(|k| k == "colab_cli"));
            if only_cli {
                let mut status = self.status();
                status["restarted"] = Value::Bool(false);
                status["hot_applied"] = Value::Bool(false);
                return Ok(status);
            }
        }
        let debug_enable_patch = match patch
            .get("chat_log_enabled")
            .or_else(|| patch.get("debug_trace_enabled"))
        {
            Some(v) => Some(
                v.as_bool()
                    .ok_or_else(|| invalid("对话记录开关取值不正确"))?,
            ),
            None => None,
        };
        let clear_debug_patch = match patch.get("clear_debug_traces") {
            Some(v) => Some(
                v.as_bool()
                    .ok_or_else(|| invalid("清空对话记录取值不正确"))?,
            ),
            None => None,
        };
        if debug_enable_patch.is_some() || clear_debug_patch.is_some() {
            let server_guard = self
                .server
                .lock()
                .map_err(|_| internal("内置语音服务状态锁失败"))?;
            if let Some(enabled) = debug_enable_patch {
                moss_voice::settings::update_voice(&self.settings_file(), |s| {
                    s.builtin_local.chat_log_enabled = enabled;
                    Ok(())
                })
                .map_err(|msg| CenterError::new("voice_settings_write_failed", 500, msg))?;
                self.debug_trace_enabled
                    .store(enabled, std::sync::atomic::Ordering::Release);
                if !enabled {
                    self.chat_log_epoch
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                if let Some(server) = server_guard.as_ref() {
                    server.set_debug_enabled(enabled);
                }
            }
            if clear_debug_patch == Some(true) {
                let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
                self.chat_log_epoch
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                db.clear_voice_chat_logs().map_err(db_failure)?;
                if let Some(server) = server_guard.as_ref() {
                    server.clear_debug_traces();
                }
            } else if debug_enable_patch == Some(false) {
                // 等待正在落库的在途 record_chat_turn_at 退出或观测到新开关/代次后再返回
                let _db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            }
            drop(server_guard);
            let only_debug = patch.as_object().is_some_and(|obj| {
                obj.keys().all(|k| {
                    matches!(
                        k.as_str(),
                        "debug_trace_enabled" | "chat_log_enabled" | "clear_debug_traces"
                    )
                })
            });
            if only_debug {
                let mut status = self.status();
                status["restarted"] = Value::Bool(false);
                status["hot_applied"] = Value::Bool(true);
                return Ok(status);
            }
        }
        let touched_jev = patch.get("jev_model").is_some()
            || patch.get("jev_filter_enabled").is_some()
            || patch.get("jev_feedback_enabled").is_some();
        let mut bool_patches: Vec<(&'static str, bool)> = Vec::new();
        for key in [
            "asr",
            "tts",
            "vad",
            "denoise",
            "kws",
            "turn",
            "read_only_shortcuts",
            "asr_beam_search",
            "barge_in",
            "job_done_announce",
            "ignore_bystanders",
            "llm_fast_reply",
            "jev_filter_enabled",
            "jev_unfinished_hold",
            "jev_fast_route",
            "jev_tool_prune",
            "jev_memory_filter",
            "asr_homophone_fix",
            "screen_preview_sync",
            "persona_risk_notes",
            "screen_pack_auto_keep",
            "auto_allow_devices",
        ] {
            if let Some(value) = patch.get(key) {
                bool_patches.push((
                    key,
                    value.as_bool().ok_or_else(|| invalid("开关取值不正确"))?,
                ));
            }
        }
        let jev_model_patch = match patch.get("jev_model") {
            Some(value) => {
                let raw = value
                    .as_str()
                    .ok_or_else(|| invalid("Jev 模型名称须为字符串"))?;
                Some(
                    moss_voice::settings::validate_jev_model(raw)
                        .map_err(|message| invalid(&message))?,
                )
            }
            None => None,
        };
        let jev_feedback_patch = match patch.get("jev_feedback_enabled") {
            Some(v) => Some(
                v.as_bool()
                    .ok_or_else(|| invalid("Jev 加工反馈开关取值不正确"))?,
            ),
            None => None,
        };
        let barge_in_ms_patch = match patch.get("barge_in_ms") {
            Some(value) => {
                let ms = value
                    .as_u64()
                    .ok_or_else(|| invalid("插话时长须为 300 ~ 600 毫秒的整数"))?;
                Some(
                    moss_voice::settings::validate_barge_in_ms(ms)
                        .map_err(|message| invalid(&message))?,
                )
            }
            None => None,
        };
        let custom_hotwords_patch = match patch.get("custom_hotwords") {
            Some(value) => {
                let words: Vec<String> = serde_json::from_value(value.clone())
                    .map_err(|_| invalid("热词须为文字数组"))?;
                Some(
                    moss_voice::settings::normalize_hotwords(&words)
                        .map_err(|message| invalid(&message))?,
                )
            }
            None => None,
        };
        let custom_corrections_patch = match patch.get("custom_asr_corrections") {
            Some(value) => {
                let entries: Vec<String> = serde_json::from_value(value.clone())
                    .map_err(|_| invalid("同音纠错规则须为文字数组"))?;
                Some(
                    moss_voice::settings::normalize_asr_corrections(&entries)
                        .map_err(|message| invalid(&message))?,
                )
            }
            None => None,
        };
        let custom_personas_patch = match patch.get("custom_personas") {
            Some(value) => {
                let roles: Vec<moss_voice::settings::CustomPersona> =
                    serde_json::from_value(value.clone())
                        .map_err(|_| invalid("自建角色格式无效"))?;
                Some(
                    moss_voice::settings::normalize_custom_personas(&roles)
                        .map_err(|message| invalid(&message))?,
                )
            }
            None => None,
        };
        // The active role is checked against the custom list inside the settings lock below.
        let persona_style_patch = match patch.get("persona_style") {
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| invalid("说话角色须为字符串"))?
                    .trim()
                    .to_owned(),
            ),
            None => None,
        };
        let persona_prompt_patch = match patch.get("persona_prompt") {
            Some(value) => {
                let raw = value
                    .as_str()
                    .ok_or_else(|| invalid("补充说明须为字符串"))?;
                Some(
                    moss_voice::settings::normalize_persona_prompt(raw)
                        .map_err(|message| invalid(&message))?,
                )
            }
            None => None,
        };
        let screen_card_style_patch = match patch.get("screen_card_style") {
            Some(value) => {
                let raw = value
                    .as_str()
                    .ok_or_else(|| invalid("屏幕卡片画风须为字符串"))?;
                Some(
                    moss_voice::settings::validate_screen_card_style(raw)
                        .map_err(|message| invalid(&message))?,
                )
            }
            None => None,
        };
        let mut model_patches = Vec::new();
        for (key, kind) in [
            ("vad_model", ModelKind::Vad),
            ("asr_model", ModelKind::Asr),
            ("tts_model", ModelKind::Tts),
            ("denoise_model", ModelKind::Denoise),
            ("kws_model", ModelKind::Kws),
            ("turn_model", ModelKind::Turn),
        ] {
            if let Some(value) = patch.get(key) {
                let id = value
                    .as_str()
                    .filter(|id| local_models::spec_of(id, kind).is_some());
                model_patches.push((
                    key,
                    id.ok_or_else(|| invalid("选择的本地模型不存在"))?
                        .to_owned(),
                ));
            }
        }
        let tts_speaker_patch = match patch.get("tts_speaker") {
            Some(value) => Some(
                value
                    .as_i64()
                    .filter(|value| (0..=999).contains(value))
                    .ok_or_else(|| invalid("音色编号须在 0 ~ 999 之间"))? as i32,
            ),
            None => None,
        };
        let tts_speed_patch = match patch.get("tts_speed") {
            Some(value) => Some(
                value
                    .as_f64()
                    .filter(|value| (0.5..=2.0).contains(value))
                    .ok_or_else(|| invalid("本地合成语速须在 0.5 ~ 2.0 之间"))?
                    as f32,
            ),
            None => None,
        };
        let mirror_patch: Option<Option<String>> = match patch.get("mirror") {
            Some(value) => {
                let mirror = value.as_str().map(str::trim).unwrap_or_default();
                if mirror.is_empty() {
                    Some(None)
                } else if (mirror.starts_with("https://") || mirror.starts_with("http://"))
                    && !mirror.contains(char::is_whitespace)
                {
                    Some(Some(mirror.trim_end_matches('/').to_owned()))
                } else {
                    return Err(invalid("下载地址必须以 http:// 或 https:// 开头"));
                }
            }
            None => None,
        };
        let old_card_style = self.settings().builtin_local.screen_card_style;
        let persona_touched = persona_style_patch.is_some()
            || custom_personas_patch.is_some()
            || persona_prompt_patch.is_some()
            || patch.get("persona_risk_notes").is_some();
        let colab_raw = patch.get("colab");
        let mut should_auto_start_colab_cli = false;
        let mut should_cancel_colab_cli = false;
        let (is_local_laya, laya_needed) = {
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            let current_settings = self.settings();
            let mut colab_patch: Option<moss_voice::colab::BuiltinColab> = None;
            let mut colab_secret_changes: Vec<(String, Option<String>)> = Vec::new();
            if let Some(raw_colab) = colab_raw {
                let obj = raw_colab
                    .as_object()
                    .ok_or_else(|| invalid("Colab 联动配置须为对象"))?;
                let mut next_colab = current_settings.builtin_colab.clone();
                if let Some(v) = obj.get("enabled") {
                    next_colab.enabled = v
                        .as_bool()
                        .ok_or_else(|| invalid("Colab 联动开关取值不正确"))?;
                }
                if let Some(v) = obj.get("url") {
                    let raw_url = v
                        .as_str()
                        .ok_or_else(|| invalid("Colab 隧道地址须为字符串"))?
                        .trim();
                    if raw_url.is_empty() {
                        next_colab.url = String::new();
                    } else {
                        next_colab.url = moss_voice::colab::normalize_colab_url(raw_url)
                            .map_err(|message| invalid(&message))?;
                    }
                } else if next_colab.enabled && !next_colab.url.trim().is_empty() {
                    next_colab.url = moss_voice::colab::normalize_colab_url(&next_colab.url)
                        .map_err(|message| invalid(&message))?;
                }
                if let Some(v) = obj.get("llm_model") {
                    let raw_m = v
                        .as_str()
                        .ok_or_else(|| invalid("Colab 大模型名须为字符串"))?;
                    next_colab.llm_model = moss_voice::colab::validate_colab_token(
                        "Colab 大模型名",
                        raw_m,
                        moss_voice::colab::DEFAULT_COLAB_LLM_MODEL,
                    )
                    .map_err(|message| invalid(&message))?;
                }
                if let Some(v) = obj.get("tts_model") {
                    let raw_m = v
                        .as_str()
                        .ok_or_else(|| invalid("Colab 合成模型名须为字符串"))?;
                    next_colab.tts_model = moss_voice::colab::validate_colab_token(
                        "Colab 合成模型名",
                        raw_m,
                        moss_voice::colab::DEFAULT_COLAB_TTS_MODEL,
                    )
                    .map_err(|message| invalid(&message))?;
                }
                if let Some(v) = obj.get("tts_voice") {
                    let raw_v = v
                        .as_str()
                        .ok_or_else(|| invalid("Colab 合成音色须为字符串"))?;
                    next_colab.tts_voice = moss_voice::colab::validate_colab_token(
                        "Colab 合成音色",
                        raw_v,
                        moss_voice::colab::DEFAULT_COLAB_TTS_VOICE,
                    )
                    .map_err(|message| invalid(&message))?;
                }
                if let Some(v) = obj.get("tts_speed") {
                    let raw_s = v
                        .as_f64()
                        .ok_or_else(|| invalid("Colab 合成语速须为数字"))?;
                    next_colab.tts_speed = moss_voice::colab::validate_colab_speed(raw_s)
                        .map_err(|message| invalid(&message))?;
                }
                let existing_key = db
                    .get_secret(moss_voice::colab::COLAB_SECRET_NAME)
                    .map_err(db_failure)?
                    .is_some_and(|s| !s.trim().is_empty());
                let clear_key = obj
                    .get("clear_key")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let new_key = obj
                    .get("api_key")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                if let Some(secret) = new_key {
                    if secret.len() > 8192 || secret.chars().any(char::is_control) {
                        return Err(invalid("Colab API 密钥格式不正确或过长"));
                    }
                    colab_secret_changes.push((
                        moss_voice::colab::COLAB_SECRET_NAME.to_owned(),
                        Some(secret.to_owned()),
                    ));
                } else if clear_key {
                    colab_secret_changes
                        .push((moss_voice::colab::COLAB_SECRET_NAME.to_owned(), None));
                }
                let has_key = if new_key.is_some() {
                    true
                } else if clear_key {
                    false
                } else {
                    existing_key
                };
                let cli_logged_in = self.colab_cli_logged_in();
                if next_colab.enabled {
                    if next_colab.url.trim().is_empty() || !has_key {
                        if !cli_logged_in {
                            return Err(invalid(
                                "尚未登录 Colab CLI：请先点击「登录 Colab CLI」完成授权（或手动填写隧道地址与 API 密钥）",
                            ));
                        }
                        should_auto_start_colab_cli = true;
                    } else if cli_logged_in
                        && (obj.get("auto_start").and_then(Value::as_bool) == Some(true)
                            || (!cfg!(test) && !current_settings.builtin_colab.enabled))
                    {
                        should_auto_start_colab_cli = true;
                    }
                } else if current_settings.builtin_colab.enabled {
                    should_cancel_colab_cli = true;
                }
                colab_patch = Some(next_colab);
            }
            let mut saved_pair = (false, false);
            let write_res = commit_secrets_then_settings(&db, &colab_secret_changes, || {
                let saved = moss_voice::settings::update_voice_with_jev_feedback(
                    &self.settings_file(),
                    |settings| {
                        if let Some(next_colab) = &colab_patch {
                            settings.builtin_colab = next_colab.clone();
                        }
                        let local = &mut settings.builtin_local;
                        for (key, val) in &bool_patches {
                            match *key {
                                "asr" => local.asr = *val,
                                "tts" => local.tts = *val,
                                "vad" => local.vad = *val,
                                "denoise" => local.denoise = *val,
                                "kws" => local.kws = *val,
                                "turn" => local.turn = *val,
                                "read_only_shortcuts" => local.read_only_shortcuts = *val,
                                "asr_beam_search" => local.asr_beam_search = *val,
                                "barge_in" => local.barge_in = *val,
                                "job_done_announce" => local.job_done_announce = *val,
                                "ignore_bystanders" => local.ignore_bystanders = *val,
                                "llm_fast_reply" => local.llm_fast_reply = *val,
                                "jev_filter_enabled" => local.jev_filter_enabled = *val,
                                "jev_unfinished_hold" => local.jev_unfinished_hold = *val,
                                "jev_fast_route" => local.jev_fast_route = *val,
                                "jev_tool_prune" => local.jev_tool_prune = *val,
                                "jev_memory_filter" => local.jev_memory_filter = *val,
                                "asr_homophone_fix" => local.asr_homophone_fix = *val,
                                "screen_preview_sync" => local.screen_preview_sync = *val,
                                "persona_risk_notes" => local.persona_risk_notes = *val,
                                "screen_pack_auto_keep" => local.screen_pack_auto_keep = *val,
                                "auto_allow_devices" => local.auto_allow_devices = *val,
                                _ => {}
                            }
                        }
                        if let Some(model) = &jev_model_patch {
                            local.jev_model = model.clone();
                        }
                        if let Some(ms) = barge_in_ms_patch {
                            local.barge_in_ms = ms;
                        }
                        if let Some(words) = &custom_hotwords_patch {
                            local.custom_hotwords = words.clone();
                        }
                        if let Some(rules) = &custom_corrections_patch {
                            local.custom_asr_corrections = rules.clone();
                        }
                        if let Some(roles) = &custom_personas_patch {
                            local.custom_personas = roles.clone();
                        }
                        match &persona_style_patch {
                            Some(style) => {
                                local.persona_style =
                                    moss_voice::settings::validate_active_persona(
                                        style,
                                        &local.custom_personas,
                                    )?;
                            }
                            // Deleting the active custom role falls back to the default role.
                            None => {
                                if moss_voice::settings::validate_active_persona(
                                    &local.persona_style,
                                    &local.custom_personas,
                                )
                                .is_err()
                                {
                                    local.persona_style =
                                        moss_voice::settings::DEFAULT_PERSONA_STYLE.into();
                                }
                            }
                        }
                        if let Some(prompt) = &persona_prompt_patch {
                            local.persona_prompt = prompt.clone();
                        }
                        if let Some(card_style) = &screen_card_style_patch {
                            local.screen_card_style = card_style.clone();
                        }
                        if persona_touched {
                            // n59: update the running role inside the settings lock, in the same order as
                            // spoken switches (DesktopPersonaStore), so the two never disagree.
                            self.persona.configure(persona_config(local));
                        }
                        for (key, id) in &model_patches {
                            match *key {
                                "vad_model" => local.vad_model = id.clone(),
                                "asr_model" => local.asr_model = id.clone(),
                                "denoise_model" => local.denoise_model = id.clone(),
                                "kws_model" => local.kws_model = id.clone(),
                                "turn_model" => local.turn_model = id.clone(),
                                "tts_model" => {
                                    if local.tts_model != *id
                                        && tts_speaker_patch.is_none()
                                        && let Some(spec) =
                                            local_models::spec_of(id, ModelKind::Tts)
                                    {
                                        local.tts_speaker =
                                            spec.voices.map(|v| v.default).unwrap_or(0);
                                    }
                                    local.tts_model = id.clone();
                                }
                                _ => {}
                            }
                        }
                        if let Some(speaker) = tts_speaker_patch {
                            local.tts_speaker = speaker;
                        }
                        if let Some(speed) = tts_speed_patch {
                            local.tts_speed = speed;
                        }
                        if let Some(mirror) = &mirror_patch {
                            local.mirror = mirror.clone();
                        }
                        let is_local = moss_voice::settings::is_local_jev_model(&local.jev_model);
                        Ok(((is_local, local.jev_filter_enabled), jev_feedback_patch))
                    },
                );
                match saved {
                    Ok(pair) => {
                        saved_pair = pair;
                        Ok(())
                    }
                    Err(message) => Err(CenterError::new(
                        "voice_settings_write_failed",
                        500,
                        message,
                    )),
                }
            });
            if let Err(err) = write_res {
                if persona_touched {
                    // A rejected or failed save must leave the running role equal to the file.
                    self.persona
                        .configure(persona_config(&self.settings().builtin_local));
                }
                return Err(err);
            }
            let (is_local_jev, filter_on) = saved_pair;
            let jev_feedback_on = moss_voice::settings::jev_feedback_enabled(&self.settings_file());
            (is_local_jev, filter_on || jev_feedback_on)
        };
        if colab_raw.is_some() {
            if should_cancel_colab_cli {
                self.cancel_colab_cli_runner(None);
            }
            if should_auto_start_colab_cli {
                self.spawn_colab_cli_runner();
            }
            self.sync_local_vision(true);
        }
        if is_local_laya && laya_needed {
            if touched_jev {
                self.ensure_laya_running(false)
                    .map_err(|msg| CenterError::new("laya_env_missing", 412, msg))?;
            } else if let Err(err) = self.ensure_laya_running(false) {
                eprintln!(
                    "[moss-desktop] 本地 Jev 决策服务未就绪（不影响其余本地设置保存）：{err}"
                );
                self.diagnostics.push_from(
                    moss_voice_server::DiagSource::Moss,
                    local_jev_failure(
                        "保存本地设置后，本地 Jev 决策服务没有就绪（其余设置已保存）",
                        &err,
                    ),
                );
            }
        } else if !is_local_laya || !laya_needed {
            self.stop_laya();
        }
        // 2026-11-09 用户决定：换画风后画风包要重刷（开机待命画面才跟着换）。清掉各台“一天内已自动推过”的记录，
        // 开着“自动保持全屏画风”时马上按新画风自动重刷
        if let Some(card_style) = &screen_card_style_patch
            && *card_style != old_card_style
        {
            self.reset_screen_pack_auto_throttle();
        }
        if let Some(card_style) = &screen_card_style_patch
            && let Ok(guard) = self.server.lock()
            && let Some(server) = guard.as_ref()
        {
            server.set_screen_card_style(moss_voice_server::ScreenCardStyle::from_setting(
                card_style,
            ));
        }
        let hot_only = patch.as_object().is_some_and(|obj| {
            !obj.is_empty()
                && obj.keys().all(|k| {
                    matches!(
                        k.as_str(),
                        "screen_card_style"
                            | "debug_trace_enabled"
                            | "chat_log_enabled"
                            | "clear_debug_traces"
                            | "persona_style"
                            | "persona_prompt"
                            | "custom_personas"
                            | "persona_risk_notes"
                            | "screen_pack_auto_keep"
                            | "auto_allow_devices"
                    )
                })
        });
        let restarted = if hot_only {
            false
        } else {
            self.restart_if_running()?
        };
        let mut status = self.status();
        status["restarted"] = Value::Bool(restarted);
        status["hot_applied"] = Value::Bool(hot_only);
        Ok(status)
    }

    pub fn model_download(&self, raw_id: &str) -> Result<Value, CenterError> {
        let (id, force) = match raw_id.strip_suffix(":force") {
            Some(base) => (base, true),
            None => (raw_id, false),
        };
        let mut checked_env = None;
        if id == "laya_multilingual" {
            let env_info = Self::detect_laya_env();
            let ready = env_info.get("ready").and_then(Value::as_bool) == Some(true);
            if !ready && !force {
                // 2026-11-07：运行环境没准备好时，先确认本机有 Python，再在后台自动创建虚拟环境，与权重下载同时进行；
                // 没有 Python 时返回 412 说明原因（前端仍可选择只下载权重）
                self.start_laya_setup()?;
            }
            checked_env = Some(env_info);
        }
        let mirror = self.settings().builtin_local.mirror;
        let default_base = if id == local_models::XIAOZHI_NOTO_V1_FULL_ID {
            local_models::DEFAULT_XIAOZHI_FONTS_BASE
        } else {
            local_models::DEFAULT_BASE
        };
        self.models
            .download(id, mirror.as_deref().unwrap_or(default_base))
            .map_err(|message| CenterError::new("local_model_download_failed", 409, message))?;
        let mut status = self.status();
        if let Some(env_info) = checked_env {
            status["local"]["laya_env"] = env_info;
        }
        Ok(status)
    }

    pub fn model_cancel(&self, id: &str) -> Value {
        if id == local_models::XIAOZHI_NOTO_V1_FULL_ID {
            self.sync_running_full_font_dir();
        }
        self.models.cancel(id);
        self.status()
    }

    pub fn model_delete(&self, id: &str) -> Result<Value, CenterError> {
        let _chain_test_guard = self.chain_test_lock.try_lock().map_err(|_| {
            CenterError::new(
                "local_model_in_use",
                409,
                "全链路连贯试跑正在运行，请等待试跑结束再删除模型",
            )
        })?;
        // 锁序：chain_test_lock -> lifecycle，避免服务启动尚未写完 worker 槽位时并发删掉加载中的模型/引擎。
        let _lifecycle_guard = self
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // n74：删正在用的本地视觉模型前先停掉它的推理进程（Windows 上运行中的文件删不掉）
        if self
            .local_vision
            .launch()
            .is_some_and(|launch| launch.model == id)
        {
            self.local_vision.configure(None);
        }
        if id == "campplus_sv_zh_cn_16k_common"
            && self
                .speaker_backend
                .helper
                .lock()
                .map(|h| h.is_some())
                .unwrap_or(true)
        {
            return Err(CenterError::new(
                "local_model_in_use",
                409,
                "声纹模型已加载，请先停止内置服务",
            ));
        }
        let is_speech_cpp = local_models::spec_of(id, ModelKind::Tts)
            .is_some_and(|spec| spec.engine.uses_speech_runtime());
        let is_audiocpp = local_models::spec_of(id, ModelKind::Tts)
            .is_some_and(|spec| spec.engine.uses_audiocpp_runtime());
        let is_turn_model = local_models::spec_of(id, ModelKind::Turn).is_some();
        let used_by_helper = id != local_models::XIAOZHI_NOTO_V1_FULL_ID
            && local_models::spec_of(id, ModelKind::Vision).is_none()
            && !is_speech_cpp
            && !is_audiocpp
            && !is_turn_model;
        if moss_voice::settings::is_local_jev_model(id) {
            let configured_id = moss_voice::settings::canonical_local_jev_model(
                &self.settings().builtin_local.jev_model,
            );
            let (stop_runner, wait_cancelling) = match self.laya.lock() {
                Ok(r) => (
                    configured_id == Some(id)
                        || r.active_model.as_deref() == Some(id)
                        || r.starting_model.as_deref() == Some(id),
                    r.cancelling_model.as_deref() == Some(id),
                ),
                Err(_) => (true, false),
            };
            if stop_runner {
                self.stop_laya_and_wait();
            } else if wait_cancelling
                && let Ok(_guard) = self.jev_start_lock.lock()
                && let Ok(mut r) = self.laya.lock()
                && r.cancelling_model.as_deref() == Some(id)
            {
                r.cancelling_model = None;
            }
        } else if is_speech_cpp {
            if self.running()
                && self
                    .speech_worker
                    .lock()
                    .map(|w| w.as_ref().is_some_and(SpeechCppWorker::is_alive))
                    .unwrap_or(false)
            {
                return Err(CenterError::new(
                    "local_model_in_use",
                    409,
                    "内置服务正在使用 Qwen3-TTS 模型，请先停止内置服务再删除",
                ));
            }
            if let Ok(mut worker) = self.speech_worker.lock()
                && let Some(w) = worker.take()
            {
                w.terminate();
            }
        } else if is_audiocpp {
            if self.running()
                && self
                    .audiocpp_worker
                    .lock()
                    .map(|w| w.as_ref().is_some_and(AudioCppWorker::is_alive))
                    .unwrap_or(false)
            {
                return Err(CenterError::new(
                    "local_model_in_use",
                    409,
                    "内置服务正在使用 Qwen3-TTS 声音克隆模型，请先停止内置服务再删除",
                ));
            }
            if let Ok(mut worker) = self.audiocpp_worker.lock()
                && let Some(w) = worker.take()
            {
                w.terminate();
            }
        } else if is_turn_model {
            if self.running()
                && self
                    .turn_worker
                    .lock()
                    .map(|w| w.as_ref().is_some_and(TurnHelper::is_alive))
                    .unwrap_or(false)
            {
                return Err(CenterError::new(
                    "local_model_in_use",
                    409,
                    "内置服务正在使用尾音语义判停模型，请先停止内置服务再删除",
                ));
            }
            if let Ok(mut worker) = self.turn_worker.lock()
                && let Some(w) = worker.take()
            {
                w.terminate();
            }
        } else if used_by_helper
            && self.running()
            && self
                .helper
                .lock()
                .map(|helper| helper.is_some())
                .unwrap_or(false)
        {
            return Err(CenterError::new(
                "local_model_in_use",
                409,
                "内置服务正在使用本地模型，请先停止内置服务再删除",
            ));
        }
        let _fallback_delete_guard = self.begin_fallback_tts_delete(id)?;
        // n82：删本地合成模型期间由 RAII 守卫全程置 deleting 抑制并发轮询复入队，并主动结束预热/试听子进程等文件锁释放后再删目录
        let _tts_delete_guard = self.begin_tts_model_delete(id);
        self.models
            .delete(id)
            .map_err(|message| CenterError::new("local_model_delete_failed", 500, message))?;
        if id == local_models::XIAOZHI_NOTO_V1_FULL_ID {
            self.sync_running_full_font_dir();
        }
        if id == moss_voice::settings::LOCAL_LAYA_JEV_MODEL {
            // 2026-11-07：连同 MOSS 自动创建的 Python 虚拟环境一起删（用户自建、未带标记的环境不动）
            self.laya_setup.cancel_and_wait();
            if let Some(root) = self.laya_env_root() {
                moss_voice::laya_env::remove_managed_env(&root).map_err(|message| {
                    CenterError::new("local_model_delete_failed", 500, message)
                })?;
            }
        }
        Ok(self.status())
    }

    /// 测试一个分区的连接：密钥留空时用已保存的密钥。结果不含密钥。
    /// 读取当前分区密钥；若当前分区尚未保存密钥且填写的地址与另一个已保存分区严格同源（OpenAI 兼容同基础 `/models` 地址，
    /// 或千问 / 百炼语音合成与大模型共享同一 HTTPS 主机端口），自动沿用同源分区的本机密钥。
    pub(super) fn secret_or_same_origin(
        &self,
        section: Section,
        input: &SectionInput,
    ) -> Result<Option<String>, CenterError> {
        let settings = self.settings();
        if let Some(own) = self.secret(section)?.filter(|value| !value.is_empty()) {
            let own_usable = match settings.builtin_models.get(section) {
                Some(saved) => {
                    moss_voice::models::saved_provider_matches_input(section, saved, input)
                }
                None => true,
            };
            if own_usable
                && !(input.kind == ProviderKind::Dashscope
                    && (own.trim().starts_with("sk-sp-")
                        || moss_voice::models::is_alibaba_token_plan_url(&input.url)
                        || moss_voice::models::chat_only_gateway_name(&input.url).is_some()))
            {
                return Ok(Some(own));
            }
        }
        if input.kind == ProviderKind::Dashscope {
            if section != Section::Tts {
                return Ok(None);
            }
            let Ok(target_url) =
                moss_voice::models::validate_dashscope_gateway_and_key(&input.url, "")
            else {
                return Ok(None);
            };
            for other in Section::ALL {
                if other == section {
                    continue;
                }
                let Some(provider) = settings.builtin_models.get(other) else {
                    continue;
                };
                if provider.kind == ProviderKind::Doubao
                    || moss_voice::models::is_alibaba_token_plan_url(&provider.url)
                    || moss_voice::models::chat_only_gateway_name(&provider.url).is_some()
                {
                    continue;
                }
                if moss_voice::models::same_cloud_origin_key(&target_url, &provider.url)
                    && let Some(peer_key) = self.secret(other)?.filter(|value| !value.is_empty())
                    && !peer_key.trim().starts_with("sk-sp-")
                {
                    return Ok(Some(peer_key));
                }
            }
            return Ok(None);
        }
        if input.kind != ProviderKind::Openai {
            return Ok(None);
        }
        if section != Section::Llm
            && moss_voice::models::chat_only_gateway_name(&input.url).is_some()
        {
            return Ok(None);
        }
        let target_list_url = moss_voice::models::models_list_url(&input.url);
        if target_list_url.is_empty() {
            return Ok(None);
        }
        for other in Section::ALL {
            if other == section {
                continue;
            }
            let Some(provider) = settings.builtin_models.get(other) else {
                continue;
            };
            if provider.kind == ProviderKind::Openai
                && moss_voice::models::models_list_url(&provider.url) == target_list_url
                && let Some(peer_key) = self.secret(other)?.filter(|value| !value.is_empty())
            {
                return Ok(Some(peer_key));
            }
            if provider.kind == ProviderKind::Dashscope
                && moss_voice::models::same_cloud_origin_key(&target_list_url, &provider.url)
                && let Some(peer_key) = self.secret(other)?.filter(|value| !value.is_empty())
            {
                return Ok(Some(peer_key));
            }
        }
        Ok(None)
    }

    /// 测试一个分区的连接：密钥留空时用已保存的密钥（或同基础地址分区的已存密钥）。结果不含密钥。
    pub fn models_test(
        &self,
        section: Section,
        input: &SectionInput,
    ) -> Result<Value, CenterError> {
        let stored = self.secret_or_same_origin(section, input)?;
        let resolved = builtin::resolve_for_test(section, input, stored)
            .map_err(|message| CenterError::new("model_input_invalid", 400, message))?;
        let result = match resolved.kind {
            ProviderKind::Openai => {
                // 快速回答开启时，测试与真实对话带同样的关闭思考参数，测出来的就是实际对话的速度
                let thinking_off = (section == Section::Llm
                    && self.settings().builtin_local.llm_fast_reply)
                    .then(|| {
                        moss_voice_providers::openai::fast_reply_params(
                            &resolved.url,
                            &resolved.model,
                        )
                    })
                    .flatten();
                let result =
                    moss_voice::models::test_section_with(&resolved, thinking_off.as_ref());
                if result.thinking_off.as_deref() == Some("unsupported") {
                    moss_voice_providers::openai::mark_thinking_off_unsupported(
                        &resolved.url,
                        &resolved.model,
                    );
                }
                result
            }
            ProviderKind::Doubao => test_doubao(&resolved),
            ProviderKind::Dashscope => test_dashscope(&resolved),
        };
        serde_json::to_value(result).map_err(|error| internal(error.to_string()))
    }

    /// 千问 / 百炼上传本机参考录音创建复刻音色，就绪后自动合成一句试听语音返回（结果不含密钥）。
    pub fn tts_voice_clone(&self, input: &Value) -> Result<Value, CenterError> {
        let started = std::time::Instant::now();
        let raw_url = input
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let typed_key = input
            .get("api_key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let raw_model = input
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let speed = input.get("speed").and_then(Value::as_f64).unwrap_or(1.0);
        if !(0.5..=2.0).contains(&speed) {
            return Err(CenterError::new(
                "model_input_invalid",
                400,
                "千问 / 百炼语速须在 0.5 ~ 2.0 之间",
            ));
        }
        let file_name = input
            .get("file_name")
            .and_then(Value::as_str)
            .unwrap_or("moss.wav");
        let prefix = input
            .get("prefix")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(file_name);
        let audio_b64 = input
            .get("audio_base64")
            .and_then(Value::as_str)
            .unwrap_or_default();

        let final_url =
            moss_voice::models::validate_dashscope_gateway_and_key(&raw_url, &typed_key)
                .map_err(|msg| CenterError::new("model_input_invalid", 400, msg))?;
        let target_model = moss_voice_providers::dashscope::validate_clone_target_model(&raw_model)
            .map_err(|e| CenterError::new("model_input_invalid", 400, e.to_string()))?;
        let audio_uri =
            moss_voice_providers::dashscope::prepare_clone_audio_data_uri(audio_b64, file_name)
                .map_err(|e| CenterError::new("model_input_invalid", 400, e.to_string()))?;
        let redacted_final_url = moss_voice::redact::redact_line(&final_url);

        let probe_input = SectionInput {
            kind: ProviderKind::Dashscope,
            url: raw_url.clone(),
            api_key: typed_key.clone(),
            model: target_model.clone(),
            voice: "placeholder".into(),
            speed: Some(speed),
        };
        let api_key = if !typed_key.is_empty() {
            typed_key
        } else {
            self.secret_or_same_origin(Section::Tts, &probe_input)?
                .unwrap_or_default()
        };
        if api_key.trim().is_empty() {
            return Err(CenterError::new(
                "model_input_invalid",
                400,
                "请先填写千问 / 百炼 API Key（或先在大模型卡片保存同域名的按量密钥）",
            ));
        }

        let outcome = crate::block_on(async {
            let voice_id = moss_voice_providers::dashscope::enroll_voice(
                &raw_url,
                &api_key,
                &target_model,
                prefix,
                &audio_uri,
            )
            .await?;
            let preview_outcome = match DashscopeTts::new(
                &final_url,
                &api_key,
                &target_model,
                &voice_id,
                speed as f32,
            ) {
                Ok(tts) => match tts.synthesize("你好，我是 MOSS。").await {
                    Ok(pcm) if !pcm.samples.is_empty() && pcm.sample_rate > 0 => {
                        let secs = pcm.samples.len() as f64 / pcm.sample_rate as f64;
                        let wav =
                            moss_voice_providers::wav::encode_wav(&pcm.samples, pcm.sample_rate);
                        Ok((secs, moss_voice::models::audio_data_url(&wav, "audio/wav")))
                    }
                    Ok(_) => Err("试听合成未返回有效音频".to_owned()),
                    Err(e) => Err(e.to_string()),
                },
                Err(e) => Err(e.to_string()),
            };
            Ok::<_, moss_voice_providers::ProviderError>((voice_id, preview_outcome))
        });

        match outcome {
            Ok((voice_id, preview)) => {
                let (preview_ok, detail, audio_data_url) = match preview {
                    Ok((secs, url)) => (
                        true,
                        format!("音色克隆成功并已生成 {secs:.1} 秒试听语音，可直接点保存生效"),
                        Some(url),
                    ),
                    Err(reason) => (
                        false,
                        format!(
                            "音色已克隆（{voice_id}），但自动试听合成未通过（{reason}）；请点「测试连接」通过后再保存"
                        ),
                        None,
                    ),
                };
                Ok(serde_json::json!({
                    "section": "tts",
                    "ok": true,
                    "preview_ok": preview_ok,
                    "voice_id": voice_id,
                    "model": target_model,
                    "final_url": redacted_final_url,
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                    "detail": detail,
                    "audio_data_url": audio_data_url,
                }))
            }
            Err(err) => Ok(serde_json::json!({
                "section": "tts",
                "ok": false,
                "final_url": redacted_final_url,
                "elapsed_ms": started.elapsed().as_millis() as u64,
                "detail": err.to_string(),
            })),
        }
    }

    /// 查询千问 / 百炼账号下已创建的复刻音色列表（结果不含密钥）。
    pub fn tts_cloned_voices_list(&self, input: &Value) -> Result<Value, CenterError> {
        let raw_url = input
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let typed_key = input
            .get("api_key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let model = input
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if let Err(detail) =
            moss_voice::models::validate_dashscope_gateway_and_key(&raw_url, &typed_key)
        {
            return Ok(serde_json::json!({
                "ok": false,
                "voices": [],
                "detail": detail,
            }));
        }
        let probe_input = SectionInput {
            kind: ProviderKind::Dashscope,
            url: raw_url.clone(),
            api_key: typed_key.clone(),
            model: model.clone(),
            voice: "placeholder".into(),
            speed: Some(1.0),
        };
        let api_key = if !typed_key.is_empty() {
            typed_key
        } else {
            self.secret_or_same_origin(Section::Tts, &probe_input)?
                .unwrap_or_default()
        };
        if api_key.trim().is_empty() {
            return Ok(serde_json::json!({
                "ok": false,
                "voices": [],
                "detail": "填写密钥后可读取账号下已克隆的音色",
            }));
        }
        let outcome = crate::block_on(moss_voice_providers::dashscope::list_cloned_voices(
            &raw_url,
            &api_key,
            (!model.is_empty()).then_some(model.as_str()),
        ));
        match outcome {
            Ok(voices) => Ok(serde_json::json!({
                "ok": true,
                "voices": voices,
            })),
            Err(err) => Ok(serde_json::json!({
                "ok": false,
                "voices": [],
                "detail": err.to_string(),
            })),
        }
    }

    /// 读取一个分区服务商支持的全部模型列表：密钥留空时用已保存的密钥（或同基础地址分区的已存密钥）。结果不含密钥。
    pub fn models_list(
        &self,
        section: Section,
        input: &SectionInput,
    ) -> Result<Value, CenterError> {
        let stored = self.secret_or_same_origin(section, input)?;
        let resolved = builtin::resolve_for_list(section, input, stored)
            .map_err(|message| CenterError::new("model_input_invalid", 400, message))?;
        let result = moss_voice::models::list_models(&resolved);
        serde_json::to_value(result).map_err(|error| internal(error.to_string()))
    }

    /// 保存模型设置（地址等写 settings.json，密钥写 app_secrets）；运行中则重启服务以生效。
    pub fn models_save(
        &self,
        input: &ModelsInput,
        clear: &[Section],
    ) -> Result<Value, CenterError> {
        let settings = self.settings();
        let plan = builtin::plan_save(&settings.builtin_models, input, clear)
            .map_err(|message| CenterError::new("model_input_invalid", 400, message))?;
        let mut changes: Vec<(String, Option<String>)> = Vec::new();
        for (section, change) in &plan.keys {
            let name = builtin_secret_name(*section).to_owned();
            match change {
                KeyChange::Keep => {
                    if let Some(raw_input) = input.section(*section) {
                        let own_existing = self.secret(*section)?.filter(|v| !v.is_empty());
                        let saved_matches =
                            settings.builtin_models.get(*section).is_none_or(|saved| {
                                moss_voice::models::saved_provider_matches_input(
                                    *section, saved, raw_input,
                                )
                            }) && !own_existing.as_ref().is_some_and(|own| {
                                raw_input.kind == ProviderKind::Dashscope
                                    && own.trim().starts_with("sk-sp-")
                            });
                        if !saved_matches {
                            match self.secret_or_same_origin(*section, raw_input)? {
                                Some(peer_key) => changes.push((name, Some(peer_key))),
                                None => changes.push((name, None)),
                            }
                        } else if own_existing.is_none()
                            && let Some(peer_key) =
                                self.secret_or_same_origin(*section, raw_input)?
                        {
                            changes.push((name, Some(peer_key)));
                        }
                    }
                }
                KeyChange::Set(value) => changes.push((name, Some(value.clone()))),
                KeyChange::Clear => changes.push((name, None)),
            }
        }
        {
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            let next_models = plan.models;
            commit_secrets_then_settings(&db, &changes, || {
                moss_voice::settings::update_voice(&self.settings_file(), |latest| {
                    latest.builtin_models = next_models.clone();
                    Ok(())
                })
                .map_err(|message| CenterError::new("voice_settings_write_failed", 500, message))
            })?;
        }
        let restarted = self.restart_if_running()?;
        let mut status = self.status();
        status["restarted"] = Value::Bool(restarted);
        Ok(status)
    }

    /// 上传本地参考录音时自动尝试用本机已安装的本地语音识别提取参考台词；
    /// 绝不将本地克隆录音发往云端（严格遵守「音频只留在本机电脑」隐私边界），识别不可用或失败时静默返回 `None`。
    fn auto_transcribe_clone_wav(&self, mono_samples: &[i16], sr: u32) -> Option<String> {
        let samples_16k = if sr == 16_000 {
            mono_samples.to_vec()
        } else {
            moss_voice_providers::resample::Resampler::new(sr, 16_000).process(mono_samples)
        };
        if samples_16k.len() < 3_200 {
            return None;
        }
        // 1. 若常驻 LocalHelper 已加载识别模型，先在窄锁作用域内克隆句柄再释放锁执行 IPC
        let reusable_helper = {
            let h_guard = self.helper.lock().ok();
            let r_guard = self.running_local.lock().ok();
            if let (Some(Some(helper)), Some(Some(spec))) = (h_guard.as_deref(), r_guard.as_deref())
                && helper.is_alive()
                && spec.asr.is_some()
            {
                Some(helper.clone())
            } else {
                None
            }
        };
        if let Some(helper) = reusable_helper {
            let asr = LocalAsr { helper };
            if let Ok(Ok(text)) = crate::block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    asr.transcribe(&samples_16k, 16_000),
                )
                .await
            }) {
                let cleaned = moss_voice::audio_cpp::sanitize_clone_transcript(&text).ok()?;
                if !cleaned.is_empty() {
                    return Some(cleaned);
                }
            }
        }
        // 2. 查找本机已安装的本地识别模型（优先当前选定项，否则顺延已安装项），拉起短生命周期本地进程识别
        let settings = self.settings();
        let experience = &settings.builtin_local;
        let preferred_spec = local_models::spec_of(&experience.asr_model, ModelKind::Asr)
            .and_then(|s| self.models.path_if_installed(s.id).map(|dir| (s, dir)));
        let (spec, dir) = preferred_spec.or_else(|| {
            local_models::CATALOG
                .iter()
                .filter(|s| s.kind == ModelKind::Asr)
                .find_map(|s| self.models.path_if_installed(s.id).map(|dir| (s, dir)))
        })?;
        let exe = helper_path().filter(|p| p.is_file())?;
        let helper = LocalHelper::spawn_as(&exe, "克隆参考台词识别程序").ok()?;
        struct Guard(LocalHelper);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.terminate_with_reason("MOSS 不再使用它，已结束");
            }
        }
        let guard = Guard(helper);
        let hotwords = self.hotwords(experience);
        guard
            .0
            .load_asr_with_options(
                &dir,
                super::conversation::helper_asr_engine(spec.engine),
                moss_voice_providers::local::AsrDecodeOptions {
                    beam_search: experience.asr_beam_search,
                    hotwords,
                },
            )
            .ok()?;
        let asr = LocalAsr {
            helper: guard.0.clone(),
        };
        let text = crate::block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(12),
                asr.transcribe(&samples_16k, 16_000),
            )
            .await
        })
        .ok()?
        .ok()?;
        let cleaned = moss_voice::audio_cpp::sanitize_clone_transcript(&text).ok()?;
        (!cleaned.is_empty()).then_some(cleaned)
    }

    fn sync_running_clone_caches(&self, rewarm_fixed_phrases: bool) {
        if !rewarm_fixed_phrases
            && let Ok(fb_guard) = self.fallback_tts.lock()
            && let Some(fb) = fb_guard.as_ref()
        {
            fb.invalidate_audiocpp();
        }
        let local = self.settings().builtin_local;
        if local.tts
            && local.tts_model == local_models::QWEN3_TTS_1_7B_CLONE_ID
            && let Ok(srv_guard) = self.server.lock()
            && let Some(server) = srv_guard.as_ref()
        {
            server.invalidate_speech_cache(rewarm_fixed_phrases);
        }
    }

    /// 管理本地零样本声音克隆（`qwen3_tts_1_7b_clone`）的参考录音与参考台词：
    /// `action = "upload" | "update_transcript" | "clear"`（n131，不返回任何本地路径）。
    pub fn local_tts_clone_manage(&self, input: &Value) -> Result<Value, CenterError> {
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("upload");
        let clone_root = self.local_clone_root();
        match action {
            "upload" => {
                let audio_b64 = input
                    .get("audio_base64")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let file_name = input
                    .get("file_name")
                    .and_then(Value::as_str)
                    .unwrap_or("reference.wav");
                let raw_transcript = input
                    .get("transcript")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let validated = moss_voice::audio_cpp::parse_clone_wav_upload(audio_b64, file_name)
                    .map_err(|msg| CenterError::new("model_input_invalid", 400, msg))?;
                let cleaned_input =
                    moss_voice::audio_cpp::sanitize_clone_transcript(raw_transcript)
                        .map_err(|msg| CenterError::new("model_input_invalid", 400, msg))?;
                let (final_transcript, auto_transcribed) = if cleaned_input.is_empty() {
                    match self
                        .auto_transcribe_clone_wav(&validated.mono_samples, validated.sample_rate)
                    {
                        Some(recognized) => (recognized, true),
                        None => (String::new(), false),
                    }
                } else {
                    (cleaned_input, false)
                };
                let meta = {
                    let _io = self
                        .local_clone_io
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    moss_voice::audio_cpp::store_clone_reference(
                        &clone_root,
                        &validated,
                        &final_transcript,
                    )
                    .map_err(|msg| CenterError::new("voice_settings_write_failed", 500, msg))?
                };
                self.sync_running_clone_caches(true);
                let detail = if auto_transcribed {
                    format!(
                        "已保存参考录音 {}（{:.1} 秒），并自动识别填入参考台词，可核对修改或直接点下方试听",
                        meta.file_name, meta.duration_secs
                    )
                } else if meta.transcript.is_empty() {
                    format!(
                        "已保存参考录音 {}（{:.1} 秒，当前按声纹模式克隆；补填录音原话可进一步提高相似度）",
                        meta.file_name, meta.duration_secs
                    )
                } else {
                    format!(
                        "已保存参考录音 {}（{:.1} 秒）与参考台词，可直接试听或对话",
                        meta.file_name, meta.duration_secs
                    )
                };
                let status = self.status();
                Ok(serde_json::json!({
                    "section": "local_tts_clone",
                    "ok": true,
                    "action": "upload",
                    "auto_transcribed": auto_transcribed,
                    "tts_clone_ref": status["local"]["tts_clone_ref"],
                    "detail": detail,
                    "status": status,
                }))
            }
            "update_transcript" => {
                let raw_transcript = input
                    .get("transcript")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let meta = {
                    let _io = self
                        .local_clone_io
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    moss_voice::audio_cpp::update_clone_transcript(&clone_root, raw_transcript)
                        .map_err(|err| match err {
                            moss_voice::audio_cpp::CloneManageError::InvalidInput(msg) => {
                                CenterError::new("model_input_invalid", 400, msg)
                            }
                            moss_voice::audio_cpp::CloneManageError::Io(msg) => {
                                CenterError::new("voice_settings_write_failed", 500, msg)
                            }
                        })?
                };
                self.sync_running_clone_caches(true);
                let detail = if meta.transcript.is_empty() {
                    "已清空参考台词，切换为声纹克隆模式".to_owned()
                } else {
                    "已更新参考录音台词，下一句合成起立即生效".to_owned()
                };
                let status = self.status();
                Ok(serde_json::json!({
                    "section": "local_tts_clone",
                    "ok": true,
                    "action": "update_transcript",
                    "tts_clone_ref": status["local"]["tts_clone_ref"],
                    "detail": detail,
                    "status": status,
                }))
            }
            "clear" => {
                {
                    let _io = self
                        .local_clone_io
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    moss_voice::audio_cpp::clear_clone_reference(&clone_root)
                        .map_err(|msg| CenterError::new("voice_settings_write_failed", 500, msg))?;
                }
                self.sync_running_clone_caches(false);
                let status = self.status();
                Ok(serde_json::json!({
                    "section": "local_tts_clone",
                    "ok": true,
                    "action": "clear",
                    "tts_clone_ref": status["local"]["tts_clone_ref"],
                    "detail": "已移除本地克隆参考录音",
                    "status": status,
                }))
            }
            _ => Err(CenterError::new(
                "model_input_invalid",
                400,
                "不支持的本地克隆操作",
            )),
        }
    }
}
