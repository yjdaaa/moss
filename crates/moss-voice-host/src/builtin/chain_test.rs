//! 全链路五步连贯试跑。

use super::*;

impl BuiltinVoice {
    /// 整条语音链路端到端连贯试跑（2026-11-11）：
    /// `测试录音 → 1. 拾音判停 → 2. 语音识别（本地模型也真实推理） → 3. Jev 粗筛 → 4. 大模型回复 → 5. 语音合成`。
    ///
    /// - 若传入 `audio_wav_base64`（电脑麦克风录制的真声 WAV），直接用作第 0 棒输入录音；
    /// - 未传录音时，先用当前选定（或本机已安装的降级）语音合成将 `prompt_text` 合成出真实中文人声并前后补静音段送入判停；
    /// - 大模型对话阶段不挂载任何写操作或物理运动工具，保证测试零硬件风险。
    pub fn chain_test(&self, input: &Value) -> Result<Value, CenterError> {
        use base64::Engine as _;
        use moss_voice_providers::{ChatMessage, Llm as _};

        let Ok(_single_flight) = self.chain_test_lock.try_lock() else {
            return Err(CenterError::new(
                "chain_test_busy",
                409,
                "全链路连贯试跑正在进行中，请等待当前试跑完成",
            ));
        };

        const STEP_KEYS: [&str; 5] = ["vad", "asr", "jev", "llm", "tts"];
        const DEFAULT_PROMPT: &str = "你好，三毫米椴木刻字一般要注意什么？";
        const MAX_AUDIO_BASE64_LEN: usize = 6 * 1024 * 1024;
        /// 与真机会话 `moss_voice_server::session::PREROLL_MS`（1000 ms）一致：Silero / TEN-VAD 确认起音晚 0.4~0.9 秒，必须留足 1 秒前置音频才不会削掉句首字音。
        const PREROLL_SAMPLES_16K: usize = 16_000;
        const TAIL_PAD_SAMPLES_16K: usize = 3_200;

        let strip_leading_screen_emoji = |text: &str| -> String {
            let is_screen_emoji = |c: char| {
                matches!(
                    c as u32,
                    0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0xFE0F | 0x200D | 0x2B50 | 0x2B55
                )
            };
            text.trim_start_matches(|c: char| is_screen_emoji(c) || c.is_whitespace())
                .trim_end()
                .to_owned()
        };

        let truncate_chars = |text: &str, max_chars: usize| -> String {
            let clean = text.trim();
            if clean.chars().count() <= max_chars {
                clean.to_owned()
            } else {
                let prefix: String = clean.chars().take(max_chars).collect();
                format!("{prefix}…")
            }
        };

        let make_response = |ok: bool,
                             failed_step: Option<&str>,
                             total_ms: u64,
                             seed_source: &str,
                             input_audio: Option<String>,
                             recognized: Option<String>,
                             reply: Option<String>,
                             reply_audio: Option<String>,
                             steps: &serde_json::Map<String, Value>,
                             summary: String|
         -> Value {
            let mut full_steps = steps.clone();
            for key in STEP_KEYS {
                full_steps.entry(key.to_owned()).or_insert_with(|| {
                    serde_json::json!({
                        "ok": true,
                        "skipped": true,
                        "elapsed_ms": 0,
                        "detail": "未执行（前序环节未通过）",
                    })
                });
            }
            serde_json::json!({
                "section": "chain",
                "ok": ok,
                "failed_step": failed_step,
                "total_ms": total_ms,
                "seed_source": seed_source,
                "input_audio_data_url": input_audio,
                "recognized_text": recognized,
                "reply_text": reply,
                "reply_audio_data_url": reply_audio,
                "steps": full_steps,
                "detail": summary,
            })
        };

        let started_all = std::time::Instant::now();
        let mut steps = serde_json::Map::new();
        let settings = self.settings();
        let experience = &settings.builtin_local;
        let models = &settings.builtin_models;
        let colab = self.active_colab(&settings)?;

        // n111：选了但还没下载的本地模型，实际对话会退回能量判停 / 云端（见 `local_models_report`），试跑跟它一致，
        // 只在退回后的那一项也不可用时报错，并写明本地模型还没下载
        let missing_local = |on: bool, id: &str, kind: ModelKind| -> Option<String> {
            let spec = local_models::spec_of(id, kind)?;
            (on && self.models.path_if_installed(spec.id).is_none()).then(|| spec.label.to_owned())
        };
        let missing_vad = missing_local(experience.vad, &experience.vad_model, ModelKind::Vad);
        let missing_asr = missing_local(experience.asr, &experience.asr_model, ModelKind::Asr);
        let missing_tts = missing_local(experience.tts, &experience.tts_model, ModelKind::Tts);
        let fallback_note = |missing: &Option<String>| {
            missing
                .as_ref()
                .map(|label| format!("（本地“{label}”还没下载，和实际对话一样先用它）"))
                .unwrap_or_default()
        };

        // 内置服务已运行且加载了同款本地模型时直接复用，否则按需启动隔离的临时 LocalHelper（结束即自动回收）
        let (mut local_parts, _helper_guard) = match self
            .local_parts_for_chain_test(experience, colab.is_some())
        {
            Ok(pair) => pair,
            Err(err) => {
                let msg = err.message.clone();
                let installed = |kind: ModelKind| {
                    experience
                        .selected(kind)
                        .is_some_and(|spec| self.models.path_if_installed(spec.id).is_some())
                };
                let vad_uses_local = installed(ModelKind::Vad) || installed(ModelKind::Turn);
                let asr_uses_local = installed(ModelKind::Asr);
                let target = if msg.contains("本地判停")
                    || msg.contains("语义判停")
                    || msg.contains("本地降噪")
                    || msg.contains("命令词抗噪")
                {
                    "vad"
                } else if msg.contains("本地识别") {
                    "asr"
                } else if msg.contains("本地合成")
                    || msg.contains("本地语音合成")
                    || msg.contains("音色")
                    || msg.contains("speech")
                    || msg.contains("audiocpp")
                    || msg.contains("克隆")
                {
                    "tts"
                } else if vad_uses_local {
                    "vad"
                } else if asr_uses_local {
                    "asr"
                } else {
                    "tts"
                };
                steps.insert(
                        target.to_owned(),
                        serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": 0, "detail": msg }),
                    );
                return Ok(make_response(
                    false,
                    Some(target),
                    started_all.elapsed().as_millis() as u64,
                    "",
                    None,
                    None,
                    None,
                    None,
                    &steps,
                    format!("本地模型加载未通过：{msg}"),
                ));
            }
        };

        let secret_of = |sec: Section| -> Result<String, String> {
            self.secret(sec)
                .map(Option::unwrap_or_default)
                .map_err(|e| e.message)
        };

        // 提前构造主 TTS（第 5 棒必用；免麦模式下第 0 棒也可借它生成真实中文人声种子录音）
        let local_tts_taken = local_parts.tts.take();
        let build_main_tts = || -> Result<(Arc<dyn Tts>, String), String> {
            if let Some((c, c_key)) = colab.as_ref() {
                let tts = OpenAiTts::new(&c.url, c_key, &c.tts_model, &c.tts_voice, c.tts_speed)
                    .map_err(|e| e.to_string())?;
                return Ok((Arc::new(tts), format!("Colab {}", c.tts_model)));
            }
            if experience.tts && missing_tts.is_none() {
                let spec = local_models::spec_of(&experience.tts_model, ModelKind::Tts);
                let label = spec
                    .map(|s| s.label)
                    .unwrap_or(experience.tts_model.as_str());
                let Some(t) = local_tts_taken.as_ref() else {
                    return Err(format!(
                        "本地语音合成模型“{label}”尚未下载：请到下方“本地模型”下载，或在顶部切换为云端"
                    ));
                };
                return Ok((t.clone(), format!("本地 {label}")));
            }
            let Some(cloud) = models.tts.as_ref() else {
                if let Some(label) = &missing_tts {
                    return Err(format!(
                        "本地语音合成模型“{label}”尚未下载，云端语音合成也未配置：请到下方“本地模型”下载，或填写云端语音合成"
                    ));
                }
                return Err("云端语音合成尚未配置：请到下方“语音合成”卡片填写并保存，或在顶部切换为本地模型".into());
            };
            let key = secret_of(Section::Tts)?;
            if cloud.kind == ProviderKind::Doubao {
                let tts = DoubaoTts::new(&cloud.url, &key, &cloud.model, &cloud.voice, cloud.speed)
                    .map_err(|e| e.to_string())?;
                Ok((
                    Arc::new(tts),
                    format!("豆包 {}{}", cloud.model, fallback_note(&missing_tts)),
                ))
            } else if cloud.kind == ProviderKind::Dashscope {
                let tts =
                    DashscopeTts::new(&cloud.url, &key, &cloud.model, &cloud.voice, cloud.speed)
                        .map_err(|e| e.to_string())?;
                Ok((
                    Arc::new(tts),
                    format!("千问 {}{}", cloud.model, fallback_note(&missing_tts)),
                ))
            } else {
                let tts = OpenAiTts::new(&cloud.url, &key, &cloud.model, &cloud.voice, cloud.speed)
                    .map_err(|e| e.to_string())?;
                Ok((
                    Arc::new(tts),
                    format!("云端 {}{}", cloud.model, fallback_note(&missing_tts)),
                ))
            }
        };
        let main_tts = build_main_tts();
        let fallback_seed_tts = local_parts.fallback_tts.take();

        // ---------------- 第 0 棒：准备 16 kHz 单声道测试录音 ----------------
        let raw_b64 = input
            .get("audio_wav_base64")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let prompt_text = input
            .get("prompt_text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(80).collect::<String>())
            .unwrap_or_else(|| DEFAULT_PROMPT.to_owned());

        let (test_pcm_16k, seed_source, input_audio_url) = if let Some(b64_input) = raw_b64 {
            if b64_input.len() > MAX_AUDIO_BASE64_LEN {
                return Err(CenterError::new(
                    "model_input_invalid",
                    400,
                    "测试录音体积过大（请控制在 10 秒以内）",
                ));
            }
            let clean_b64 = b64_input
                .split_once(',')
                .map(|(_, body)| body)
                .unwrap_or(b64_input)
                .trim();
            let wav_bytes = base64::engine::general_purpose::STANDARD
                .decode(clean_b64)
                .map_err(|_| {
                    CenterError::new("model_input_invalid", 400, "测试录音 Base64 编码无效")
                })?;
            let decoded = moss_voice_providers::wav::decode_wav(&wav_bytes)
                .map_err(|e| CenterError::new("model_input_invalid", 400, e.to_string()))?;
            let mut mono_16k = if decoded.sample_rate == 16_000 {
                decoded.samples
            } else {
                moss_voice_providers::resample::Resampler::new(decoded.sample_rate, 16_000)
                    .process(&decoded.samples)
            };
            if mono_16k.len() > 16_000 * 15 {
                mono_16k.truncate(16_000 * 15);
            }
            if mono_16k.len() < 3_200 {
                return Err(CenterError::new(
                    "model_input_invalid",
                    400,
                    "录音时间太短（不足 0.2 秒），请录制一句完整的话",
                ));
            }
            let secs = mono_16k.len() as f64 / 16_000.0;
            let preview_wav = moss_voice_providers::wav::encode_wav(&mono_16k, 16_000);
            let data_url = moss_voice::models::audio_data_url(&preview_wav, "audio/wav");
            // 前后留足静音段（前 500ms、后 800ms）以便本地 Silero / TEN-VAD 建立底噪收口并留够命令词抗噪尾音窗口
            let mut framed = Vec::with_capacity(8_000 + mono_16k.len() + 12_800);
            framed.resize(8_000, 0i16);
            framed.extend_from_slice(&mono_16k);
            framed.resize(framed.len() + 12_800, 0i16);
            (
                framed,
                format!("麦克风真声录音（{secs:.1} 秒）"),
                Some(data_url),
            )
        } else {
            let seed_tts: Arc<dyn Tts> = match (&main_tts, &fallback_seed_tts) {
                (Ok((t, _)), _) => t.clone(),
                (Err(_), Some(fb)) => fb.clone(),
                (Err(err_msg), None) => {
                    steps.insert(
                        "tts".into(),
                        serde_json::json!({
                            "ok": false,
                            "skipped": false,
                            "elapsed_ms": 0,
                            "detail": err_msg,
                        }),
                    );
                    return Ok(make_response(
                        false,
                        Some("tts"),
                        started_all.elapsed().as_millis() as u64,
                        "",
                        None,
                        None,
                        None,
                        None,
                        &steps,
                        format!(
                            "无法生成免麦测试人声（{err_msg}）；请先配置好语音合成，或直接点旁边「录真话测」用麦克风测试前四步"
                        ),
                    ));
                }
            };
            let seed_pcm = match crate::block_on(seed_tts.synthesize(&prompt_text)) {
                Ok(pcm) if !pcm.samples.is_empty() && pcm.sample_rate > 0 => pcm,
                Ok(_) => {
                    let msg = "生成测试录音失败：语音合成返回了空音频".to_owned();
                    steps.insert(
                            "tts".into(),
                            serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": 0, "detail": msg }),
                        );
                    return Ok(make_response(
                        false,
                        Some("tts"),
                        started_all.elapsed().as_millis() as u64,
                        "",
                        None,
                        None,
                        None,
                        None,
                        &steps,
                        msg,
                    ));
                }
                Err(e) => {
                    let msg = format!("生成测试录音失败（语音合成不通）：{e}");
                    steps.insert(
                            "tts".into(),
                            serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": 0, "detail": e.to_string() }),
                        );
                    return Ok(make_response(
                        false,
                        Some("tts"),
                        started_all.elapsed().as_millis() as u64,
                        "",
                        None,
                        None,
                        None,
                        None,
                        &steps,
                        msg,
                    ));
                }
            };
            let mono_16k = if seed_pcm.sample_rate == 16_000 {
                seed_pcm.samples
            } else {
                moss_voice_providers::resample::Resampler::new(seed_pcm.sample_rate, 16_000)
                    .process(&seed_pcm.samples)
            };
            let preview_wav = moss_voice_providers::wav::encode_wav(&mono_16k, 16_000);
            let data_url = moss_voice::models::audio_data_url(&preview_wav, "audio/wav");
            let mut framed = Vec::with_capacity(8_000 + mono_16k.len() + 12_800);
            framed.resize(8_000, 0i16);
            framed.extend_from_slice(&mono_16k);
            framed.resize(framed.len() + 12_800, 0i16);
            (
                framed,
                format!("内置测试人声：“{}”", truncate_chars(&prompt_text, 24)),
                Some(data_url),
            )
        };

        // ---------------- 第 1 棒：1. 拾音判停 (VAD + 命令词抗噪 / 降噪 / 语义判停) ----------------
        let vad_started = std::time::Instant::now();
        let turn_predictor = local_parts.turn_predictor.take();
        let mut addon_notes = Vec::new();
        if local_parts.denoise_active.is_some() {
            addon_notes.push("降噪");
        }
        if local_parts.kws_active.is_some() {
            addon_notes.push("命令词抗噪");
        }
        if local_parts.turn_active.is_some() {
            addon_notes.push("语义判停");
        }
        let addon_suffix = if addon_notes.is_empty() {
            String::new()
        } else {
            format!("（+ {}）", addon_notes.join("、"))
        };
        let (mut vad, vad_label): (Box<dyn Vad>, String) = if experience.vad
            && missing_vad.is_none()
        {
            let spec = local_models::spec_of(&experience.vad_model, ModelKind::Vad);
            let label = spec
                .map(|s| s.label)
                .unwrap_or(experience.vad_model.as_str());
            let Some(factory) = local_parts.vad.as_ref() else {
                let msg = format!(
                    "本地判停模型“{label}”尚未下载：请到下方“本地模型”下载，或在顶部改回内置能量判停"
                );
                steps.insert(
                        "vad".into(),
                        serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": 0, "detail": msg }),
                    );
                return Ok(make_response(
                    false,
                    Some("vad"),
                    started_all.elapsed().as_millis() as u64,
                    &seed_source,
                    input_audio_url,
                    None,
                    None,
                    None,
                    &steps,
                    msg,
                ));
            };
            let Some(instance) = factory.create(16_000, &VadConfig::default()) else {
                let msg = format!("本地判停模型“{label}”初始化流失败");
                steps.insert(
                        "vad".into(),
                        serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": 0, "detail": msg }),
                    );
                return Ok(make_response(
                    false,
                    Some("vad"),
                    started_all.elapsed().as_millis() as u64,
                    &seed_source,
                    input_audio_url,
                    None,
                    None,
                    None,
                    &steps,
                    msg,
                ));
            };
            (instance, format!("本地 {label}{addon_suffix}"))
        } else {
            (
                Box::new(EnergyVad::new(16_000, VadConfig::default())),
                format!("内置能量判停{}{addon_suffix}", fallback_note(&missing_vad)),
            )
        };

        // 完整扫描整段音频并合并句中逗号停顿分割的多段语音（对齐真机 `Session` 续听合并，不在首个 250ms 逗号停顿处截断后半句）
        let mut speech_start_idx: Option<usize> = None;
        let mut speech_end_idx: Option<usize> = None;
        let mut in_speech = false;
        let mut segment_count = 0usize;
        let mut heard_keyword: Option<moss_voice_providers::local::CommandKeyword> = None;
        let mut vad_error_notice: Option<String> = None;
        for (chunk_i, frame) in test_pcm_16k.chunks(320).enumerate() {
            let sample_pos = chunk_i * 320;
            let events = vad.accept(frame);
            if let Some(notice) = vad.take_notice() {
                vad_error_notice = Some(notice);
                break;
            }
            for ev in events {
                match ev {
                    VadEvent::SpeechStart => {
                        if speech_start_idx.is_none() {
                            speech_start_idx = Some(sample_pos);
                        }
                        if !in_speech {
                            segment_count += 1;
                            in_speech = true;
                        }
                    }
                    VadEvent::SpeechEnd if speech_start_idx.is_some() => {
                        speech_end_idx = Some(sample_pos + frame.len());
                        in_speech = false;
                    }
                    VadEvent::Keyword(kind) => {
                        heard_keyword = Some(kind);
                    }
                    _ => {}
                }
            }
        }
        if in_speech {
            speech_end_idx = None;
        }
        let mut vad_ms = vad_started.elapsed().as_millis() as u64;
        if let Some(notice) = vad_error_notice {
            let msg = format!("本地判停推理异常：{notice}");
            steps.insert(
                    "vad".into(),
                    serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": vad_ms, "detail": msg }),
                );
            return Ok(make_response(
                false,
                Some("vad"),
                started_all.elapsed().as_millis() as u64,
                &seed_source,
                input_audio_url,
                None,
                None,
                None,
                &steps,
                msg,
            ));
        }
        let Some(start_idx) = speech_start_idx else {
            let msg = "判停未检测到有效人声（录音音量过低或全为静音）".to_owned();
            steps.insert(
                    "vad".into(),
                    serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": vad_ms, "detail": msg }),
                );
            return Ok(make_response(
                false,
                Some("vad"),
                started_all.elapsed().as_millis() as u64,
                &seed_source,
                input_audio_url,
                None,
                None,
                None,
                &steps,
                msg,
            ));
        };
        let raw_end_idx = speech_end_idx
            .unwrap_or(test_pcm_16k.len())
            .min(test_pcm_16k.len());
        let seg_from = start_idx.saturating_sub(PREROLL_SAMPLES_16K);
        let seg_to = raw_end_idx
            .saturating_add(TAIL_PAD_SAMPLES_16K)
            .min(test_pcm_16k.len());
        let speech_segment = &test_pcm_16k[seg_from..seg_to];
        let turn_note = if let Some(predictor) = turn_predictor.as_ref() {
            let prob = crate::block_on(predictor.predict_turn_end(speech_segment, 16_000));
            vad_ms = vad_started.elapsed().as_millis() as u64;
            prob.map(|p| format!(" · 语义判停 {:.0}%", p * 100.0))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let start_ms_audio = (start_idx * 1000) / 16_000;
        let end_ms_audio = (raw_end_idx * 1000) / 16_000;
        let merge_note = if segment_count > 1 {
            format!("（合并 {segment_count} 段气口）")
        } else if speech_end_idx.is_some() {
            " 并准确判停".to_owned()
        } else {
            "（至音频末尾）".to_owned()
        };
        steps.insert(
            "vad".into(),
            serde_json::json!({
                "ok": true,
                "skipped": false,
                "elapsed_ms": vad_ms,
                "detail": format!(
                    "{vad_label}：检出语音 {start_ms_audio}→{end_ms_audio} ms{merge_note}{turn_note}（耗时 {vad_ms} ms）"
                ),
            }),
        );

        // ---------------- 第 2 棒：2. 语音识别 (ASR：对齐真机优先流式会话识别，失败回退整段) ----------------
        let asr_started = std::time::Instant::now();
        let hotwords = self.hotwords(experience);
        let mut streaming_asr: Option<Arc<dyn StreamingAsr>> = None;
        let (asr_provider, asr_label): (Arc<dyn Asr>, String) = if experience.asr
            && missing_asr.is_none()
        {
            let spec = local_models::spec_of(&experience.asr_model, ModelKind::Asr);
            let label = spec
                .map(|s| s.label)
                .unwrap_or(experience.asr_model.as_str());
            let Some(local_asr) = local_parts.asr.take() else {
                let msg = format!(
                    "本地语音识别模型“{label}”尚未下载：请到下方“本地模型”下载，或在顶部切换为云端"
                );
                steps.insert(
                        "asr".into(),
                        serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": 0, "detail": msg }),
                    );
                return Ok(make_response(
                    false,
                    Some("asr"),
                    started_all.elapsed().as_millis() as u64,
                    &seed_source,
                    input_audio_url,
                    None,
                    None,
                    None,
                    &steps,
                    msg,
                ));
            };
            streaming_asr = local_parts.streaming_asr.take();
            (Arc::new(local_asr), format!("本地 {label}"))
        } else {
            let Some(cloud) = models.asr.as_ref() else {
                let msg = match &missing_asr {
                    Some(label) => format!(
                        "本地语音识别模型“{label}”尚未下载，云端语音识别也未配置：请到下方“本地模型”下载，或填写云端语音识别"
                    ),
                    None => "云端语音识别尚未配置：请到下方“语音识别”卡片填写并保存，或在顶部切换为本地模型".to_owned(),
                };
                steps.insert(
                        "asr".into(),
                        serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": 0, "detail": msg }),
                    );
                return Ok(make_response(
                    false,
                    Some("asr"),
                    started_all.elapsed().as_millis() as u64,
                    &seed_source,
                    input_audio_url,
                    None,
                    None,
                    None,
                    &steps,
                    msg,
                ));
            };
            let key = match secret_of(Section::Asr) {
                Ok(k) => k,
                Err(msg) => {
                    steps.insert(
                            "asr".into(),
                            serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": 0, "detail": msg }),
                        );
                    return Ok(make_response(
                        false,
                        Some("asr"),
                        started_all.elapsed().as_millis() as u64,
                        &seed_source,
                        input_audio_url,
                        None,
                        None,
                        None,
                        &steps,
                        msg,
                    ));
                }
            };
            let built: Result<(Arc<dyn Asr>, String), String> =
                if cloud.kind == ProviderKind::Doubao {
                    DoubaoAsr::new(&cloud.url, &key, &cloud.model)
                        .map(|d| {
                            let arc = Arc::new(d.with_hotwords(&hotwords));
                            streaming_asr = Some(arc.clone());
                            (
                                arc as Arc<dyn Asr>,
                                format!("豆包 {}{}", cloud.model, fallback_note(&missing_asr)),
                            )
                        })
                        .map_err(|e| e.to_string())
                } else {
                    OpenAiAsr::new(&cloud.url, &key, &cloud.model, Some("zh"))
                        .map(|o| {
                            (
                                Arc::new(o.with_hotwords(&hotwords)) as Arc<dyn Asr>,
                                format!("云端 {}{}", cloud.model, fallback_note(&missing_asr)),
                            )
                        })
                        .map_err(|e| e.to_string())
                };
            match built {
                Ok(pair) => pair,
                Err(msg) => {
                    steps.insert(
                            "asr".into(),
                            serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": 0, "detail": msg }),
                        );
                    return Ok(make_response(
                        false,
                        Some("asr"),
                        started_all.elapsed().as_millis() as u64,
                        &seed_source,
                        input_audio_url,
                        None,
                        None,
                        None,
                        &steps,
                        msg,
                    ));
                }
            }
        };

        let raw_heard = match crate::block_on(async {
            if let Some(streaming) = streaming_asr.as_ref() {
                let stream = streaming.start();
                for chunk in speech_segment.chunks(1_600) {
                    stream.feed(chunk);
                }
                match stream.finish().await {
                    Ok(text) if !text.trim().is_empty() => Ok(text),
                    _ => asr_provider.transcribe(speech_segment, 16_000).await,
                }
            } else {
                asr_provider.transcribe(speech_segment, 16_000).await
            }
        }) {
            Ok(text) => text,
            Err(err) => {
                let asr_ms = asr_started.elapsed().as_millis() as u64;
                let msg = err.to_string();
                steps.insert(
                        "asr".into(),
                        serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": asr_ms, "detail": msg }),
                    );
                return Ok(make_response(
                    false,
                    Some("asr"),
                    started_all.elapsed().as_millis() as u64,
                    &seed_source,
                    input_audio_url,
                    None,
                    None,
                    None,
                    &steps,
                    format!("语音识别未通过：{msg}"),
                ));
            }
        };
        let asr_ms = asr_started.elapsed().as_millis() as u64;
        let corrections = moss_voice::settings::effective_asr_corrections(
            experience.asr_homophone_fix,
            &experience.custom_asr_corrections,
        );
        let mut recognized_text =
            moss_voice_server::conversation::apply_asr_corrections(raw_heard.trim(), &corrections);
        let mut kws_fused_note = "";
        if let Some(kind) = heard_keyword
            && let Some(fused) =
                moss_voice_server::conversation::fuse_command_keyword_hint(kind, &recognized_text)
        {
            recognized_text = fused.to_owned();
            kws_fused_note = " · 命令词抗噪纠偏";
        }
        if !moss_voice_providers::text::speakable(&recognized_text)
            || moss_voice_server::fast::is_noise_filler(&recognized_text)
        {
            let msg = "识别结果为空或仅为语气杂音（未能从该段语音识别出有效文字）".to_owned();
            steps.insert(
                    "asr".into(),
                    serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": asr_ms, "detail": msg }),
                );
            return Ok(make_response(
                false,
                Some("asr"),
                started_all.elapsed().as_millis() as u64,
                &seed_source,
                input_audio_url,
                None,
                None,
                None,
                &steps,
                msg,
            ));
        }
        steps.insert(
            "asr".into(),
            serde_json::json!({
                "ok": true,
                "skipped": false,
                "elapsed_ms": asr_ms,
                "detail": format!(
                    "{asr_label} → “{}”{kws_fused_note}（{asr_ms} ms）",
                    truncate_chars(&recognized_text, 22)
                ),
            }),
        );

        // ---------------- 第 3 棒：3. Jev 前置粗筛 ----------------
        let jev_model = moss_voice::settings::validate_jev_model(&experience.jev_model)
            .unwrap_or_else(|_| moss_voice::settings::default_jev_model());
        let local_jev_id = moss_voice::settings::canonical_local_jev_model(&jev_model);
        let local_budget = local_jev_id
            .and_then(|id| moss_voice::onejev::local_jev_budget(id, self.onejev_uses_gpu(id)));
        let jev_options = JevFilterOptions {
            enabled: experience.jev_filter_enabled,
            unfinished_hold: experience.jev_unfinished_hold
                && !local_budget.is_some_and(|b| b.hold_ms.is_none()),
            fast_route: experience.read_only_shortcuts && experience.jev_fast_route,
            tool_prune: experience.jev_tool_prune,
            memory_filter: experience.jev_memory_filter,
        };
        if !jev_options.active() {
            steps.insert(
                "jev".into(),
                serde_json::json!({
                    "ok": true,
                    "skipped": true,
                    "elapsed_ms": 0,
                    "detail": "已关闭，识别文本直通大模型",
                }),
            );
        } else if moss_voice_server::fast::is_stop(&recognized_text) {
            steps.insert(
                "jev".into(),
                serde_json::json!({
                    "ok": true,
                    "skipped": true,
                    "elapsed_ms": 0,
                    "detail": "命中急停短令，按真机快路径免 Jev 直通",
                }),
            );
        } else if moss_voice_server::conversation::is_default_exit_phrase(&recognized_text) {
            steps.insert(
                "jev".into(),
                serde_json::json!({
                    "ok": true,
                    "skipped": true,
                    "elapsed_ms": 0,
                    "detail": "命中退出短令，按真机快路径免 Jev 直通",
                }),
            );
        } else if experience.read_only_shortcuts
            && moss_voice_server::fast::read_intent(&recognized_text).is_some()
        {
            steps.insert(
                "jev".into(),
                serde_json::json!({
                    "ok": true,
                    "skipped": true,
                    "elapsed_ms": 0,
                    "detail": "命中本地只读快答，按真机快路径免 Jev 直通",
                }),
            );
        } else {
            let jev_started = std::time::Instant::now();
            let filter_res: Result<VoiceJevFilter, String> = if let Some(local_id) = local_jev_id {
                if self.models.path_if_installed(local_id).is_none() {
                    Err(format!("本地决策模型“{local_id}”尚未下载"))
                } else if let Err(msg) = self.ensure_local_jev_running_for(local_id, true) {
                    Err(msg)
                } else {
                    let reader: moss_voice_server::jev::SecretReader =
                        Arc::new(|| Some("moss-local-jev".to_owned()));
                    let upstream_model =
                        moss_voice::settings::local_jev_upstream_model_name(local_id);
                    let client = Arc::new(OpenRouterDecisionClient::with_endpoint(
                        moss_voice::models::DEFAULT_LOCAL_LAYA_DECISIONS_URL,
                        reader,
                        upstream_model,
                    ));
                    Ok(match local_budget {
                        Some(budget) => VoiceJevFilter::new(client, jev_options).with_timeouts(
                            std::time::Duration::from_millis(budget.turn_ms),
                            std::time::Duration::from_millis(budget.hold_ms.unwrap_or(0)),
                        ),
                        None => VoiceJevFilter::new(client, jev_options),
                    })
                }
            } else {
                match self.jev_effective_secret()? {
                    None => Err(
                        "尚未配置 OpenRouter API Key：请到下方 Jev 卡片填写或在顶部关闭 Jev".into(),
                    ),
                    Some(secret) => {
                        let reader: moss_voice_server::jev::SecretReader =
                            Arc::new(move || Some(secret.clone()));
                        let client =
                            Arc::new(OpenRouterDecisionClient::with_model(reader, &jev_model));
                        Ok(VoiceJevFilter::new(client, jev_options))
                    }
                }
            };
            let filter = match filter_res {
                Ok(f) => f,
                Err(msg) => {
                    let jev_ms = jev_started.elapsed().as_millis() as u64;
                    steps.insert(
                            "jev".into(),
                            serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": jev_ms, "detail": msg }),
                        );
                    return Ok(make_response(
                        false,
                        Some("jev"),
                        started_all.elapsed().as_millis() as u64,
                        &seed_source,
                        input_audio_url,
                        Some(recognized_text),
                        None,
                        None,
                        &steps,
                        format!("Jev 前置粗筛未通过：{msg}"),
                    ));
                }
            };
            let specs = self.jev_tool_specs(&settings.builtin_music)?;
            let total_specs = specs.len();
            let run_unfinished_hold_probe =
                |steps: &mut serde_json::Map<String, Value>| -> Result<(), String> {
                    if moss_voice_server::fast::normalized(&recognized_text)
                        .chars()
                        .count()
                        < 2
                    {
                        steps.insert(
                            "jev".into(),
                            serde_json::json!({
                                "ok": true,
                                "skipped": true,
                                "elapsed_ms": 0,
                                "detail": "单字短句按真机规则免查尾音续听",
                            }),
                        );
                        return Ok(());
                    }
                    let hold = crate::block_on(filter.check_unfinished_hold(
                        &recognized_text,
                        &[],
                        &specs,
                    ));
                    let jev_ms = jev_started.elapsed().as_millis() as u64;
                    if !hold.applied {
                        let msg = format!(
                            "Jev（{jev_model}）请求超时或未返回有效判断，请检查密钥、余额或本地进程状态"
                        );
                        steps.insert(
                            "jev".into(),
                            serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": jev_ms, "detail": msg }),
                        );
                        return Err(msg);
                    }
                    let state_label = if hold.unfinished {
                        "未说完续听"
                    } else {
                        "已说完放行"
                    };
                    steps.insert(
                        "jev".into(),
                        serde_json::json!({
                            "ok": true,
                            "skipped": false,
                            "elapsed_ms": jev_ms,
                            "detail": format!("尾音续听检测：{state_label}（{jev_ms} ms）"),
                        }),
                    );
                    Ok(())
                };
            if !jev_options.tool_prune && !jev_options.fast_route && !jev_options.memory_filter {
                if let Err(msg) = run_unfinished_hold_probe(&mut steps) {
                    return Ok(make_response(
                        false,
                        Some("jev"),
                        started_all.elapsed().as_millis() as u64,
                        &seed_source,
                        input_audio_url,
                        Some(recognized_text),
                        None,
                        None,
                        &steps,
                        msg,
                    ));
                }
            } else {
                let sample_memories = if jev_options.memory_filter {
                    vec!["常用材料：3mm 东北椴木，刻字功率 35%、速度 1800 mm/min".to_owned()]
                } else {
                    Vec::new()
                };
                let decision = crate::block_on(filter.evaluate_turn(
                    &recognized_text,
                    &[],
                    &specs,
                    &sample_memories,
                ));
                let jev_ms = jev_started.elapsed().as_millis() as u64;
                if decision.local_bypass {
                    if jev_options.unfinished_hold {
                        if let Err(msg) = run_unfinished_hold_probe(&mut steps) {
                            return Ok(make_response(
                                false,
                                Some("jev"),
                                started_all.elapsed().as_millis() as u64,
                                &seed_source,
                                input_audio_url,
                                Some(recognized_text),
                                None,
                                None,
                                &steps,
                                msg,
                            ));
                        }
                    } else {
                        steps.insert(
                            "jev".into(),
                            serde_json::json!({
                                "ok": true,
                                "skipped": true,
                                "elapsed_ms": jev_ms,
                                "detail": "当前句式按真机规则本地直通大模型",
                            }),
                        );
                    }
                } else if !decision.applied {
                    let msg = format!(
                        "Jev（{jev_model}）请求超时或未返回有效判断，请检查密钥、余额或本地进程状态"
                    );
                    steps.insert(
                            "jev".into(),
                            serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": jev_ms, "detail": msg }),
                        );
                    return Ok(make_response(
                        false,
                        Some("jev"),
                        started_all.elapsed().as_millis() as u64,
                        &seed_source,
                        input_audio_url,
                        Some(recognized_text),
                        None,
                        None,
                        &steps,
                        msg,
                    ));
                } else {
                    let kept_specs = decision.filter_tools(&specs, &recognized_text, &[]).len();
                    let route_note = if decision.fast_read.is_some() {
                        "命中只读状态快答"
                    } else {
                        "常规对话路由"
                    };
                    steps.insert(
                            "jev".into(),
                            serde_json::json!({
                                "ok": true,
                                "skipped": false,
                                "elapsed_ms": jev_ms,
                                "detail": format!("{route_note} · 工具保留 {kept_specs}/{total_specs}（{jev_ms} ms）"),
                            }),
                        );
                }
            }
        }

        // ---------------- 第 4 棒：4. 大模型回复 (LLM) ----------------
        let llm_started = std::time::Instant::now();
        let (llm_url, llm_key, llm_model, llm_label) = match colab.as_ref() {
            Some((c, c_key)) => (
                c.url.clone(),
                c_key.clone(),
                c.llm_model.clone(),
                format!("Colab {}", c.llm_model),
            ),
            None => {
                let Some(cloud) = models.llm.as_ref() else {
                    let msg = "云端大模型尚未配置：请到下方“大模型”卡片填写并保存".to_owned();
                    steps.insert(
                            "llm".into(),
                            serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": 0, "detail": msg }),
                        );
                    return Ok(make_response(
                        false,
                        Some("llm"),
                        started_all.elapsed().as_millis() as u64,
                        &seed_source,
                        input_audio_url,
                        Some(recognized_text),
                        None,
                        None,
                        &steps,
                        msg,
                    ));
                };
                let key = match secret_of(Section::Llm) {
                    Ok(k) => k,
                    Err(msg) => {
                        steps.insert(
                                "llm".into(),
                                serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": 0, "detail": msg }),
                            );
                        return Ok(make_response(
                            false,
                            Some("llm"),
                            started_all.elapsed().as_millis() as u64,
                            &seed_source,
                            input_audio_url,
                            Some(recognized_text),
                            None,
                            None,
                            &steps,
                            msg,
                        ));
                    }
                };
                (
                    cloud.url.clone(),
                    key,
                    cloud.model.clone(),
                    format!("云端 {}", cloud.model),
                )
            }
        };
        let llm_client = match OpenAiLlm::new(&llm_url, &llm_key, &llm_model) {
            Ok(c) => c.with_fast_reply(experience.llm_fast_reply),
            Err(e) => {
                let msg = e.to_string();
                steps.insert(
                        "llm".into(),
                        serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": 0, "detail": msg }),
                    );
                return Ok(make_response(
                    false,
                    Some("llm"),
                    started_all.elapsed().as_millis() as u64,
                    &seed_source,
                    input_audio_url,
                    Some(recognized_text),
                    None,
                    None,
                    &steps,
                    format!("大模型初始化失败：{msg}"),
                ));
            }
        };
        self.persona.configure(persona_config(experience));
        let sys_prompt = format!(
            "{}\n（当前未连接硬件执行器，直接按角色自然口语作答即可。）",
            moss_voice_server::conversation::build_turn_system_prompt(
                &self.persona.system_prompt(),
                true,
                false,
            )
        );
        let messages = vec![
            ChatMessage::system(sys_prompt),
            ChatMessage::user(recognized_text.clone()),
        ];
        let (llm_outcome, first_delta_ms, segmented_sentences) = crate::block_on(async {
            use moss_voice_providers::text::{Segmenter, ThinkFilter};
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let t0 = std::time::Instant::now();
            let first_slot = Arc::new(Mutex::new(None::<u64>));
            let first_clone = first_slot.clone();
            let drain = tokio::spawn(async move {
                let mut think = ThinkFilter::default();
                let mut segmenter = Segmenter::new();
                let mut sentences = Vec::new();
                while let Some(chunk) = rx.recv().await {
                    let clean_delta = think.push(&chunk);
                    if !clean_delta.is_empty() {
                        if let Ok(mut guard) = first_clone.lock()
                            && guard.is_none()
                        {
                            *guard = Some(t0.elapsed().as_millis() as u64);
                        }
                        sentences.extend(segmenter.push(&clean_delta, std::time::Instant::now()));
                    }
                }
                let tail = think.finish();
                if !tail.is_empty() {
                    sentences.extend(segmenter.push(&tail, std::time::Instant::now()));
                }
                if let Some(rest) = segmenter.finish() {
                    sentences.push(rest);
                }
                sentences
            });
            let reply = llm_client.chat(&messages, &[], tx).await;
            let sentences = drain.await.unwrap_or_default();
            let first_ms = first_slot.lock().ok().and_then(|g| *g);
            (reply, first_ms, sentences)
        });
        let llm_ms = llm_started.elapsed().as_millis() as u64;
        let raw_reply_text = match llm_outcome {
            Ok(rep) => moss_voice_providers::text::strip_think(&rep.text)
                .trim()
                .to_owned(),
            Err(e) => {
                let msg = e.to_string();
                steps.insert(
                        "llm".into(),
                        serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": llm_ms, "detail": msg }),
                    );
                return Ok(make_response(
                    false,
                    Some("llm"),
                    started_all.elapsed().as_millis() as u64,
                    &seed_source,
                    input_audio_url,
                    Some(recognized_text),
                    None,
                    None,
                    &steps,
                    format!("大模型请求未通过：{msg}"),
                ));
            }
        };
        let reply_text = strip_leading_screen_emoji(&raw_reply_text);
        if reply_text.is_empty() {
            let msg = "大模型未返回可读回复内容".to_owned();
            steps.insert(
                    "llm".into(),
                    serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": llm_ms, "detail": msg }),
                );
            return Ok(make_response(
                false,
                Some("llm"),
                started_all.elapsed().as_millis() as u64,
                &seed_source,
                input_audio_url,
                Some(recognized_text),
                None,
                None,
                &steps,
                msg,
            ));
        }
        let first_note = first_delta_ms
            .map(|ms| format!("首字 {ms} ms · "))
            .unwrap_or_default();
        steps.insert(
            "llm".into(),
            serde_json::json!({
                "ok": true,
                "skipped": false,
                "elapsed_ms": llm_ms,
                "detail": format!(
                    "{llm_label} → “{}”（{first_note}共 {llm_ms} ms）",
                    truncate_chars(&reply_text, 22)
                ),
            }),
        );

        // ---------------- 第 5 棒：5. 语音合成 (TTS：对齐真机按 Segmenter 分句流式合成与边界淡入淡出) ----------------
        let tts_started = std::time::Instant::now();
        let (tts_provider, tts_label) = match main_tts {
            Ok(pair) => pair,
            Err(msg) => {
                steps.insert(
                        "tts".into(),
                        serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": 0, "detail": msg }),
                    );
                return Ok(make_response(
                    false,
                    Some("tts"),
                    started_all.elapsed().as_millis() as u64,
                    &seed_source,
                    input_audio_url,
                    Some(recognized_text),
                    Some(reply_text),
                    None,
                    &steps,
                    msg,
                ));
            }
        };
        let mut sentences_to_speak: Vec<String> = segmented_sentences
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .collect();
        if sentences_to_speak.is_empty() {
            let mut fallback = moss_voice_providers::text::clean_for_speech(&reply_text);
            if fallback.trim().is_empty() {
                fallback = "收到。".to_owned();
            }
            sentences_to_speak.push(fallback);
        }
        let sentence_count = sentences_to_speak.len();
        let stream_tts_res: Result<moss_voice_providers::Pcm, moss_voice_providers::ProviderError> =
            crate::block_on(async {
                use moss_voice_server::audio::{
                    BOUNDARY_FADE_MS, clause_silence_bounds_ms, fade_head_i16, fade_tail_i16,
                    trim_trailing_silence_i16,
                };
                let mut merged_samples: Vec<i16> = Vec::new();
                let mut out_rate: u32 = 0;
                for sentence in &sentences_to_speak {
                    let (chunks_tx, mut chunks_rx) =
                        tokio::sync::mpsc::unbounded_channel::<moss_voice_providers::Pcm>();
                    let synth_fut = tts_provider.synthesize_stream(sentence, chunks_tx);
                    let collect_fut = async {
                        let mut clause = Vec::new();
                        let mut clause_rate: u32 = 0;
                        while let Some(pcm) = chunks_rx.recv().await {
                            if pcm.samples.is_empty() || pcm.sample_rate == 0 {
                                continue;
                            }
                            if clause_rate == 0 {
                                clause_rate = pcm.sample_rate;
                            }
                            if pcm.sample_rate == clause_rate {
                                clause.extend_from_slice(&pcm.samples);
                            } else {
                                let resampled = moss_voice_providers::resample::Resampler::new(
                                    pcm.sample_rate,
                                    clause_rate,
                                )
                                .process(&pcm.samples);
                                clause.extend_from_slice(&resampled);
                            }
                        }
                        (clause, clause_rate)
                    };
                    let (synth_res, (mut clause_samples, clause_rate)) =
                        tokio::join!(synth_fut, collect_fut);
                    synth_res?;
                    if clause_samples.is_empty() || clause_rate == 0 {
                        continue;
                    }
                    if out_rate == 0 {
                        out_rate = clause_rate;
                    } else if clause_rate != out_rate {
                        clause_samples =
                            moss_voice_providers::resample::Resampler::new(clause_rate, out_rate)
                                .process(&clause_samples);
                    }
                    let (keep_ms, trigger_ms) = clause_silence_bounds_ms(sentence);
                    trim_trailing_silence_i16(&mut clause_samples, out_rate, keep_ms, trigger_ms);
                    fade_head_i16(&mut clause_samples, out_rate, BOUNDARY_FADE_MS);
                    fade_tail_i16(&mut clause_samples, out_rate, BOUNDARY_FADE_MS);
                    merged_samples.extend_from_slice(&clause_samples);
                }
                Ok(moss_voice_providers::Pcm {
                    samples: merged_samples,
                    sample_rate: out_rate,
                })
            });
        let reply_pcm = match stream_tts_res {
            Ok(pcm) if !pcm.samples.is_empty() && pcm.sample_rate > 0 => pcm,
            Ok(_) => {
                let tts_ms = tts_started.elapsed().as_millis() as u64;
                let msg = "语音合成返回了空音频".to_owned();
                steps.insert(
                        "tts".into(),
                        serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": tts_ms, "detail": msg }),
                    );
                return Ok(make_response(
                    false,
                    Some("tts"),
                    started_all.elapsed().as_millis() as u64,
                    &seed_source,
                    input_audio_url,
                    Some(recognized_text),
                    Some(reply_text),
                    None,
                    &steps,
                    msg,
                ));
            }
            Err(e) => {
                let tts_ms = tts_started.elapsed().as_millis() as u64;
                let msg = e.to_string();
                steps.insert(
                        "tts".into(),
                        serde_json::json!({ "ok": false, "skipped": false, "elapsed_ms": tts_ms, "detail": msg }),
                    );
                return Ok(make_response(
                    false,
                    Some("tts"),
                    started_all.elapsed().as_millis() as u64,
                    &seed_source,
                    input_audio_url,
                    Some(recognized_text),
                    Some(reply_text),
                    None,
                    &steps,
                    format!("语音合成未通过：{msg}"),
                ));
            }
        };
        let tts_ms = tts_started.elapsed().as_millis() as u64;
        let reply_secs = reply_pcm.samples.len() as f64 / reply_pcm.sample_rate as f64;
        let reply_wav =
            moss_voice_providers::wav::encode_wav(&reply_pcm.samples, reply_pcm.sample_rate);
        let reply_audio_url = moss_voice::models::audio_data_url(&reply_wav, "audio/wav");
        let clause_note = if sentence_count > 1 {
            format!("（分 {sentence_count} 句流式合成）")
        } else {
            String::new()
        };
        steps.insert(
            "tts".into(),
            serde_json::json!({
                "ok": true,
                "skipped": false,
                "elapsed_ms": tts_ms,
                "detail": format!("{tts_label} → 合成 {reply_secs:.1} 秒语音{clause_note}（{tts_ms} ms）"),
            }),
        );

        let total_ms = started_all.elapsed().as_millis() as u64;
        Ok(make_response(
            true,
            None,
            total_ms,
            &seed_source,
            input_audio_url,
            Some(recognized_text),
            Some(reply_text),
            Some(reply_audio_url),
            &steps,
            format!("五步全链路连贯试跑通过（总耗时 {total_ms} ms），可点击下方按钮试听合成回复"),
        ))
    }
}
