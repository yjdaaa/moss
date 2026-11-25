//! 服务状态汇总、聊天记录读取与诊断包生成（含脱敏）。

use super::*;

#[derive(Clone)]
pub(super) struct StatusProbeCache {
    captured_at: std::time::Instant,
    onejev_probe_id: &'static str,
    onejev_env: Value,
    speech_env: Value,
    audiocpp_env: Value,
    laya_env: Value,
    local_ip: Option<String>,
    local_ips: Vec<String>,
    helper_found: bool,
}

/// 诊断包里的审批记录条数（与 `/v1/approvals` 列表上限一致）。
pub(super) const APPROVALS_IN_BUNDLE: usize = 50;
/// 诊断包自带的“包含 / 不包含”说明，拿到文件的人不用翻文档也知道里面有什么。
pub(super) const BUNDLE_INCLUDED: &[&str] = &[
    "MOSS 版本、操作系统与 CPU 架构",
    "语音链路、服务是否运行、端口",
    "云端模型的服务类型、地址（去掉账号密码与查询参数，IP 换成 <ip>）、模型名、音色，以及是否填过密钥（只有是/否）",
    "本地模型安装与运行状态、Jev 决策服务状态",
    "全部保留的服务事件（日常记录最多 300 条、错误与警告最多 200 条）及被覆盖条数",
    "最近 50 轮对话各阶段耗时（含会话号与设备编号）",
    "对话记录开关打开时最近 20 轮的识别原话、工具参数与回复（关闭时为空）",
    "在线会话与已登记设备（设备编号、名称、型号、固件版本、授权状态；IP 换成 <ip>）",
    "最近 50 条审批记录及执行结果说明与错误码",
    "最近一次启动失败原因与当前降级提示",
];
pub(super) const BUNDLE_EXCLUDED: &[&str] = &[
    "任何密钥、令牌、密码（只记录是否填过）",
    "录音和合成音频",
    "排障开关关闭时的对话文字",
    "对话记忆、长期记忆、资料库内容与声纹",
    "家居地址与令牌、固件文件",
    "设备上报的原始状态快照（可能含 Wi-Fi 名称等）",
];

/// 诊断包整体脱敏：每个字符串先过 `moss_voice::redact::redact_line`（`api_key: …`、`Authorization: …` 一类），
/// 再过 [`moss_voice_providers::scrub_diagnostic_text`]（疑似密钥、带凭据或查询参数的地址、IPv4 地址）。
pub(super) fn scrub_bundle(value: &mut Value) {
    match value {
        Value::String(text) => {
            *text =
                moss_voice_providers::scrub_diagnostic_text(&moss_voice::redact::redact_line(text));
        }
        Value::Array(items) => items.iter_mut().for_each(scrub_bundle),
        Value::Object(map) => map.values_mut().for_each(scrub_bundle),
        _ => {}
    }
}

impl BuiltinVoice {
    /// 判断给定语音设置在当前宿主（含历史模型回退目录、外置模型覆盖与 Colab 密钥）下是否满足对话模式条件。
    pub fn chat_ready(&self, settings: &moss_voice::VoiceSettings) -> bool {
        let colab_key_configured = self
            .host
            .db
            .lock()
            .ok()
            .and_then(|db| {
                db.get_secret(moss_voice::colab::COLAB_SECRET_NAME)
                    .ok()
                    .flatten()
            })
            .is_some_and(|value| !value.is_empty());
        settings.builtin_chat_ready_in(&self.models, colab_key_configured)
    }

    pub fn status(&self) -> Value {
        self.status_with_options(false)
    }

    pub fn status_with_options(&self, lite: bool) -> Value {
        let guard = self
            .server
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (view, active_sessions) = match guard.as_ref() {
            Some(server) => (Some(server.status()), server.active_device_sessions()),
            None => (None, Vec::new()),
        };
        drop(guard);
        let db = self.host.db.lock().ok();
        let settings = self.settings();
        let jev_feedback_enabled =
            moss_voice::settings::jev_feedback_enabled(&self.settings_file());
        let key_set = |name: &str| {
            db.as_ref()
                .and_then(|db| db.get_secret(name).ok().flatten())
                .is_some_and(|value| !value.is_empty())
        };
        let jev_key_configured = key_set("jev_api_key");
        let colab_key_configured = key_set(moss_voice::colab::COLAB_SECRET_NAME);
        let colab_active = settings.builtin_colab.active(colab_key_configured);
        let openrouter_llm_key_set = settings.builtin_models.llm.as_ref().is_some_and(|p| {
            p.kind == ProviderKind::Openai && moss_voice::models::is_openrouter_https_url(&p.url)
        }) && key_set(builtin_secret_name(Section::Llm));
        let models = builtin::status_view(&settings.builtin_models, key_set);
        drop(db);
        let (ws_port, http_port) = view
            .as_ref()
            .map(|view| (view.ws_port, view.http_port))
            .unwrap_or((settings.builtin_ws_port, settings.builtin_http_port));
        // 运行中显示实际模式；未运行时显示下次启动的模式
        let mode = match &view {
            Some(view) => serde_json::to_value(view.mode).unwrap_or_default(),
            None => Value::String(
                if settings.builtin_chat_ready_in(&self.models, colab_key_configured) {
                    "chat"
                } else {
                    "echo"
                }
                .into(),
            ),
        };
        let active_local_jev_id =
            moss_voice::settings::canonical_local_jev_model(&settings.builtin_local.jev_model);
        let any_local_jev_installed = moss_voice::settings::LOCAL_JEV_MODEL_IDS
            .iter()
            .any(|id| self.models.path_if_installed(id).is_some());
        let active_local_jev_installed =
            active_local_jev_id.is_some_and(|id| self.models.path_if_installed(id).is_some());
        let local_jev_installed = if active_local_jev_id.is_some() {
            active_local_jev_installed
        } else {
            any_local_jev_installed
        };
        let is_local_jev = active_local_jev_id.is_some();
        let models_dir_ref = active_local_jev_id.unwrap_or("laya_multilingual");
        let onejev_probe_id = active_local_jev_id
            .filter(|id| moss_voice::settings::is_local_onejev_model(id))
            .or_else(|| {
                local_models::CATALOG
                    .iter()
                    .filter(|spec| spec.engine.uses_llama_runtime())
                    .map(|spec| spec.id)
                    .find(|id| self.models.path_if_installed(id).is_some())
            })
            .unwrap_or(moss_voice::settings::LOCAL_ONEJEV_0_8B_MODEL);
        let cached_probes = if lite {
            self.status_probe_cache
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
                .filter(|entry| {
                    entry.onejev_probe_id == onejev_probe_id
                        && entry.captured_at.elapsed() < std::time::Duration::from_secs(4)
                })
        } else {
            None
        };
        let (
            onejev_env,
            speech_env,
            audiocpp_env,
            laya_env,
            detected_ip,
            detected_ips,
            helper_found,
        ) = if let Some(entry) = cached_probes {
            (
                entry.onejev_env,
                entry.speech_env,
                entry.audiocpp_env,
                entry.laya_env,
                entry.local_ip,
                entry.local_ips,
                entry.helper_found,
            )
        } else {
            let onejev_env = serde_json::to_value(moss_voice::onejev::detect_onejev_env(
                self.models.root_for(onejev_probe_id),
                false,
            ))
            .unwrap_or(Value::Null);
            let speech_env =
                moss_voice::speech_cpp::detect_speech_env(self.models.speech_runtime_root());
            let audiocpp_env =
                moss_voice::audio_cpp::detect_audiocpp_env(self.models.audiocpp_runtime_root());
            let laya_env = serde_json::to_value(Self::detect_laya_env_with_probe(false))
                .unwrap_or(Value::Null);
            let mut detected_ips: Vec<String> = moss_voice_server::lan_ipv4_addrs()
                .into_iter()
                .map(|ip| ip.to_string())
                .collect();
            let detected_ip = local_ip().or_else(|| detected_ips.first().cloned());
            if let Some(ip) = detected_ip.as_ref()
                && !detected_ips.contains(ip)
            {
                detected_ips.insert(0, ip.clone());
            }
            let helper_found = helper_path().is_some_and(|path| path.is_file());
            if let Ok(mut slot) = self.status_probe_cache.lock() {
                *slot = Some(StatusProbeCache {
                    captured_at: std::time::Instant::now(),
                    onejev_probe_id,
                    onejev_env: onejev_env.clone(),
                    speech_env: speech_env.clone(),
                    audiocpp_env: audiocpp_env.clone(),
                    laya_env: laya_env.clone(),
                    local_ip: detected_ip.clone(),
                    local_ips: detected_ips.clone(),
                    helper_found,
                });
            }
            (
                onejev_env,
                speech_env,
                audiocpp_env,
                laya_env,
                detected_ip,
                detected_ips,
                helper_found,
            )
        };
        let mut root = serde_json::json!({
            "running": view.is_some(),
            "mode": mode,
            "link_mode": settings.mode.as_str(),
            "ws_port": ws_port,
            "http_port": http_port,
            "mqtt_port": view.as_ref().and_then(|v| v.mqtt_port),
            "udp_port": view.as_ref().and_then(|v| v.udp_port),
            "ota_url": format!("http://{}:{http_port}/xiaozhi/ota/", moss_voice_server::MDNS_HOST_LABEL),
            "ota_ip_url": detected_ip.map(|ip| format!("http://{ip}:{http_port}/xiaozhi/ota/")),
            "ota_ip_urls": detected_ips
                .into_iter()
                .map(|ip| format!("http://{ip}:{http_port}/xiaozhi/ota/"))
                .collect::<Vec<_>>(),
            "models": models,
            "jev": {
                "endpoint": if is_local_jev {
                    moss_voice::models::DEFAULT_LOCAL_LAYA_DECISIONS_URL
                } else {
                    moss_voice::models::DEFAULT_JEV_DECISIONS_URL
                },
                "model": settings.builtin_local.jev_model,
                "key_set": jev_key_configured,
                "openrouter_llm_key_set": openrouter_llm_key_set,
                "local_installed": local_jev_installed,
                "voice_enabled": settings.builtin_local.jev_filter_enabled,
                "feedback_enabled": jev_feedback_enabled,
            },
            "last_error": self.last_error.lock().ok().and_then(|last| last.clone()),
            "local": {
                "settings": settings.builtin_local,
                "colab": {
                    "enabled": settings.builtin_colab.enabled,
                    "url": settings.builtin_colab.url,
                    "llm_model": settings.builtin_colab.llm_model,
                    "tts_model": settings.builtin_colab.tts_model,
                    "tts_voice": settings.builtin_colab.tts_voice,
                    "tts_speed": settings.builtin_colab.tts_speed,
                    "key_configured": colab_key_configured,
                    "active": colab_active,
                    "cli": self.colab_cli_status(),
                },
                "jev_key_configured": jev_key_configured || openrouter_llm_key_set || active_local_jev_installed,
                "default_mirror": local_models::DEFAULT_BASE,
                "models_dir": self.models.write_root_for(models_dir_ref).to_string_lossy(),
                "onejev_env": onejev_env,
                "speech_env": speech_env,
                "audiocpp_env": audiocpp_env,
                "tts_clone_ref": moss_voice::audio_cpp::clone_reference_status_json(&self.local_clone_root()),
                "jev_runtime": self.local_jev_runtime_status(&settings, jev_feedback_enabled),
                "laya_env": laya_env,
                "laya_setup": self.laya_setup.status(),
                "helper_found": helper_found,
                "helper_running": self.helper.lock().ok().and_then(|helper| helper.as_ref().map(LocalHelper::is_alive)).unwrap_or(false),
                "models": self.models.status(),
                "ready": {
                    "llm": settings.builtin_section_ready_in(Section::Llm, &self.models, colab_key_configured),
                    "asr": settings.builtin_section_ready_in(Section::Asr, &self.models, colab_key_configured),
                    "tts": settings.builtin_section_ready_in(Section::Tts, &self.models, colab_key_configured),
                },
            },
            "sessions": if view.is_some() {
                serde_json::to_value(&active_sessions).unwrap_or_else(|_| Value::Array(Vec::new()))
            } else {
                Value::Array(Vec::new())
            },
            // n68：事件与耗时来自桌面端持有的诊断记录本，服务停止后仍可查看
            "events": self.diagnostics.events(),
            "events_dropped": self.diagnostics.drops(),
            "timings": self.diagnostics.timings(),
            "warnings": self.warnings(&settings),
            "debug_enabled": settings.builtin_local.chat_log_enabled,
            "debug_traces": self.persisted_chat_traces(view.as_ref()),
            "device_prefs": settings.builtin_device_prefs,
            "memory_enabled": self.memory.enabled(),
            "devices": self.devices(),
        });
        if !lite && let Some(map) = root.as_object_mut() {
            map.insert(
                "goggle_bot_gifs".into(),
                serde_json::to_value(moss_voice_server::cached_goggle_bot_gifs())
                    .unwrap_or(Value::Null),
            );
            map.insert(
                "screen_style_gifs".into(),
                serde_json::to_value(moss_voice_server::cached_style_scene_gifs())
                    .unwrap_or(Value::Null),
            );
        }
        root
    }

    /// n81 读取持久化的语音对话记录（上限 200 条供界面查阅与搜索；数据库暂不可读时回退内存缓存）。
    fn persisted_chat_traces(&self, view: Option<&moss_voice_server::ServerStatusView>) -> Value {
        if let Ok(db) = self.host.db.lock()
            && let Ok(rows) = db.list_voice_chat_logs(200)
        {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    let tools: Vec<moss_voice_server::DebugToolTrace> =
                        serde_json::from_str(&r.tools_json).unwrap_or_default();
                    let at_ms = u64::try_from(r.at_ms.max(0)).unwrap_or(0);
                    serde_json::json!({
                        "id": r.id,
                        "at_ms": at_ms,
                        "at": at_ms / 1000,
                        "device_id": r.device_id,
                        "session_id": r.session_id,
                        "turn": u64::try_from(r.turn.max(0)).unwrap_or(0),
                        "raw_asr": r.raw_asr,
                        "corrected_asr": r.corrected_asr,
                        "tools": tools,
                        "reply": r.reply,
                        "spoken": r.spoken,
                        "outcome": r.outcome,
                        "truncated": r.truncated,
                        "tools_dropped": usize::try_from(r.tools_dropped.max(0)).unwrap_or(0),
                    })
                })
                .collect();
            return Value::Array(items);
        }
        view.map(|v| serde_json::to_value(&v.debug_traces).unwrap_or_default())
            .unwrap_or_else(|| Value::Array(Vec::new()))
    }

    /// 启动（或重启）失败都写进诊断记录，界面上的报错弹窗关掉后仍能查到原因。
    /// 当前生效的降级与迁移提示（本地音乐目录不可用、声纹按人记忆未生效、`Windows.old` 旧配置模型未迁回）。
    fn warnings(&self, settings: &moss_voice::VoiceSettings) -> Vec<String> {
        let mut out: Vec<String> = [&self.music_warning, &self.speaker_warning]
            .into_iter()
            .filter_map(|slot| slot.lock().ok().and_then(|warning| warning.clone()))
            .collect();
        if let Some(notice) = &self.windows_old_notice
            && let Some(warning) = notice.warning_text(&self.models, settings)
        {
            out.push(warning);
        }
        out
    }

    /// 诊断包（2026-11-07 n68，`docs/developer/api-contracts.md`“诊断包”一节）：版本、系统、链路、模型名、
    /// 全部保留的事件与覆盖计数、耗时、开启时的排障记录、最近 50 条审批及其结果、最近错误与降级提示、
    /// 本地模型状态。不含密钥、令牌、音频、对话记忆与资料内容；所有文字统一经 [`scrub_bundle`] 脱敏。
    pub fn diagnostics_bundle(&self) -> Value {
        let status = self.status();
        let settings = self.settings();
        let (approvals, approvals_error) =
            match laser_api::approval::ApprovalService::new(self.host.db.clone())
                .list(None, APPROVALS_IN_BUNDLE)
            {
                Ok(rows) => (serde_json::to_value(rows).unwrap_or_default(), Value::Null),
                Err(error) => (
                    Value::Array(Vec::new()),
                    Value::String(format!("审批记录读取失败：{error:?}")),
                ),
            };
        let devices: Vec<Value> = status["devices"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        serde_json::json!({
                            "device_id": row["device_id"],
                            "name": row["name"],
                            "model": row["model"],
                            "firmware_version": row["firmware_version"],
                            "status": row["status"],
                            "last_seen": row["last_seen"],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut bundle = serde_json::json!({
            "format": "moss-voice-diagnostics",
            "format_version": 1,
            "generated_at_ms": (now() * 1000.0) as u64,
            "app": {
                "name": "MOSS",
                "version": env!("CARGO_PKG_VERSION"),
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
            },
            "link_mode": status["link_mode"],
            "running": status["running"],
            "mode": status["mode"],
            "ports": { "ws": status["ws_port"], "http": status["http_port"] },
            "models": status["models"],
            "jev": status["jev"],
            "local": {
                "ready": status["local"]["ready"],
                "helper_found": status["local"]["helper_found"],
                "helper_running": status["local"]["helper_running"],
                "models": status["local"]["models"],
                "jev_runtime": status["local"]["jev_runtime"],
                "onejev_env": status["local"]["onejev_env"],
                "speech_env": status["local"]["speech_env"],
                "audiocpp_env": status["local"]["audiocpp_env"],
                "tts_clone_configured": status["local"]["tts_clone_ref"]["configured"],
                "laya_env": status["local"]["laya_env"],
                "laya_setup": status["local"]["laya_setup"],
                "asr_model": settings.builtin_local.asr_model,
                "tts_model": settings.builtin_local.tts_model,
                "vad_model": settings.builtin_local.vad_model,
                "denoise": settings.builtin_local.denoise,
                "denoise_model": settings.builtin_local.denoise_model,
                "kws": settings.builtin_local.kws,
                "kws_model": settings.builtin_local.kws_model,
                "turn": settings.builtin_local.turn,
                "turn_model": settings.builtin_local.turn_model,
            },
            "last_error": status["last_error"],
            "warnings": status["warnings"],
            "events": status["events"],
            "events_dropped": status["events_dropped"],
            "timings": status["timings"],
            "debug_enabled": status["debug_enabled"],
            "debug_traces": if status["debug_enabled"].as_bool() == Some(true) {
                status["debug_traces"]
                    .as_array()
                    .map(|items| Value::Array(items.iter().take(20).cloned().collect()))
                    .unwrap_or_else(|| Value::Array(Vec::new()))
            } else {
                Value::Array(Vec::new())
            },
            "sessions": status["sessions"],
            "devices": devices,
            "approvals": {
                "limit": APPROVALS_IN_BUNDLE,
                "items": approvals,
                "error": approvals_error,
            },
            "included": BUNDLE_INCLUDED,
            "excluded": BUNDLE_EXCLUDED,
        });
        scrub_bundle(&mut bundle);
        bundle
    }
}
