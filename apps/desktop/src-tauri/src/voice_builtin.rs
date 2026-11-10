//! 内置语音服务的 Tauri 命令（内置语音技术书 NV1 起）：启动 / 停止、状态、设备允许与撤销、模型配置（NV5）。
//!
//! 与 `voice.rs` 一样以 Tauri 命令提供，只有 MOSS 桌面窗口能调用；Android 不编译服务本身，命令直接返回错误。
//! 设备登记存 SQLite `voice_devices`（schema v6），设备 token 的签名密钥与模型密钥存 `app_secrets`
//! （`voice.device_token_secret`、`voice.llm.main` 等，任何命令都不返回原文）。
//!
//! 识别、大模型、合成三项都可用时以对话模式启动，否则以回声模式启动；对话中调用 MOSS 工具时身份为
//! `CallerContext::Voice`（运动与出光跟随严格模式开关）。语音链路为“内置服务”时随 MOSS 启动。
//!
//! 本地模型（NV6）：识别（SenseVoice）、合成（Kokoro）、判停（Silero）可改用本机模型，由与 MOSS 同目录的
//! 辅助程序 `moss-voice-local`（GPL-3.0）运行；模型按需下载到运行目录 `models/` 并校验 SHA-256。
//!
//! 豆包流式（NV7）：识别与合成可选豆包。识别在用户开始说话时就建立连接、边说边传；合成每句的第一段音频
//! 一到就播放（播放启动缓冲相应从 600 ms 降到 300 ms）。

use serde_json::Value;

type CmdResult = Result<Value, Value>;

/// 敏感语音设置窗口：只接受经 [`crate::window_is_trusted_main`] 确认的 MOSS 主窗口。
///
/// 2026-10-18 用户决定（技术书 n26）：记忆、信息服务、声纹、家居四项在主语音页就地展开；
/// 2026-11-07 用户决定（n67）：取消独立的本机安全设置窗口，唤醒词与固件升级也放进语音中心，
/// 推送前逐台确认不变。Tauri ACL（`voice-private-main.json`）是第一层，这里固定来源为本进程后端。
#[cfg(desktop)]
pub(crate) fn require_private_settings_window(window: &tauri::WebviewWindow) -> Result<(), Value> {
    if crate::window_is_trusted_main(window) {
        return Ok(());
    }
    Err(serde_json::json!({
        "code": "voice_acl_denied",
        "message": "这项语音设置只能在 MOSS 主窗口里修改"
    }))
}

/// 主窗口专用命令：只接受后端地址上的 `main` 窗口（见 [`crate::is_trusted_main`]）。
#[cfg(desktop)]
fn require_main_window(window: &tauri::WebviewWindow) -> Result<(), Value> {
    if crate::window_is_trusted_main(window) {
        return Ok(());
    }
    Err(serde_json::json!({
        "code": "voice_acl_denied",
        "message": "只能在 MOSS 主窗口修改内置语音设置"
    }))
}

/// 读取内置语音状态的窗口：经 [`crate::window_is_trusted_main`] 确认的主窗口。
/// 判定与 [`require_private_settings_window`] 相同，只换成“查看状态”的提示。
#[cfg(desktop)]
fn require_status_window(window: &tauri::WebviewWindow) -> Result<(), Value> {
    require_private_settings_window(window).map_err(|_| {
        serde_json::json!({
            "code": "voice_acl_denied",
            "message": "内置语音状态只能在 MOSS 主窗口查看"
        })
    })
}

/// NV19-08: entry points for `voice_context.rs` commands (the backend type stays private).
#[cfg(desktop)]
pub(crate) async fn context_manage(app: tauri::AppHandle, input: Value) -> CmdResult {
    imp::run(app, move |voice| voice.context_manage(&input)).await
}

#[cfg(desktop)]
pub(crate) async fn context_add_folder(
    app: tauri::AppHandle,
    path: std::path::PathBuf,
) -> CmdResult {
    imp::run(app, move |voice| voice.context_add_folder(&path)).await
}

#[cfg(mobile)]
fn desktop_only<T>(_: T) -> CmdResult {
    Err(
        serde_json::json!({"code": "voice_desktop_only", "message": "内置语音服务仅在 MOSS 桌面端可用"}),
    )
}

#[cfg(desktop)]
mod imp {
    use std::sync::{Arc, Mutex};

    use moss_voice::builtin::{self, KeyChange};
    use moss_voice::local_models::{self, Engine, ModelKind, ModelManager};
    use moss_voice::settings::{BuiltinLocal, builtin_secret_name};
    use moss_voice::{CenterError, ModelsInput, ProviderKind, Section, SectionInput, VoiceMode};
    use moss_voice_providers::doubao::{DoubaoAsr, DoubaoTts};
    use moss_voice_providers::local::{
        AsrEngine, HELPER_FILE, LocalAsr, LocalHelper, LocalStreamingAsr, LocalTts, TtsEngine,
        VadEngine, VadParams,
    };
    use moss_voice_providers::openai::{OpenAiAsr, OpenAiLlm, OpenAiTts};
    use moss_voice_providers::{Asr, StreamingAsr, Tts};
    use moss_voice_server::{
        Conversation, ConversationSettings, DeviceRegistry, DeviceStatus, DeviceVisit,
        DiagCategory, EventRecord, HostTools, JevFilterOptions, LocalVadFactory,
        OpenRouterDecisionClient, ServerConfig, ServerMode, ToolContext, ToolHost, ToolHostResult,
        VadFactory, VoiceJevFilter, VoiceServer,
    };
    use serde_json::Value;
    use tauri::Manager;

    use super::CmdResult;

    const TOKEN_SECRET_NAME: &str = "voice.device_token_secret";
    /// 流式合成时播放前攒的音频（批量合成沿用默认 600 ms）
    const STREAMING_PLAYBACK_START_MS: u32 = 300;

    fn now() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }

    /// 字符串版本（设备登记、记忆等内部接口用）：大白话在前，脱敏后的原始错误在括号里。
    fn db_error(error: impl std::fmt::Debug) -> String {
        format!(
            "MOSS 本机设置数据库读写失败（原始信息：{}）",
            moss_voice_providers::scrub_diagnostic_text(&format!("{error:?}"))
        )
    }

    /// 本机数据库失败：界面只说大白话，原始错误（Debug 文本，已脱敏）放进 `detail`（2026-11-07 n68）。
    fn db_failure(error: impl std::fmt::Debug) -> CenterError {
        internal("MOSS 本机设置数据库读写失败，请重启 MOSS 再试；一直失败请导出诊断包反馈")
            .with_detail(moss_voice_providers::scrub_diagnostic_text(&format!(
                "{error:?}"
            )))
    }

    fn internal(message: impl Into<String>) -> CenterError {
        CenterError::new("voice_internal", 500, message)
    }

    /// 密钥存 `app_secrets`、其余配置存 `settings.json`，两处无法共用一个事务：先在事务内写密钥，
    /// 再写设置；设置写入失败时按原值还原密钥，不留下“已开启但缺密钥”或“密钥已换、设置未变”的
    /// 半写状态（2026-10-13 审查，NV11-4）。调用方须持有数据库锁，避免校验与写入之间被并发保存改动。
    pub(super) fn commit_secrets_then_settings(
        db: &laser_runtime::db::RuntimeDb,
        changes: &[(String, Option<String>)],
        save_settings: impl FnOnce() -> Result<(), CenterError>,
    ) -> Result<(), CenterError> {
        let previous = db
            .apply_secret_changes(changes, now())
            .map_err(db_failure)?;
        let Err(error) = save_settings() else {
            return Ok(());
        };
        if let Err(restore) = db.restore_secrets(&previous, now()) {
            return Err(
                internal(format!("{}；密钥还原也失败，请重新保存", error.message)).with_detail(
                    moss_voice_providers::scrub_diagnostic_text(&format!("{restore:?}")),
                ),
            );
        }
        Err(error)
    }

    /// 在已持有 `RuntimeDb` 锁的同一临界区内读取当前配置与凭据：
    /// 优先返回独立 `jev_api_key`；未设时仅当**同一代次配置**的大模型为 OpenAI 兼容且严格指向
    /// `https://openrouter.ai:443` 时才允许回退读取 `llm` 密钥，杜绝跨配置代次把其他服务商密钥发往 OpenRouter。
    pub(super) fn resolve_openrouter_jev_secret_locked(
        db: &laser_runtime::db::RuntimeDb,
        settings_file: &std::path::Path,
    ) -> Result<(Option<String>, bool), CenterError> {
        if let Some(own) = db
            .get_secret("jev_api_key")
            .map_err(db_failure)?
            .filter(|v| !v.trim().is_empty())
        {
            return Ok((Some(own), true));
        }
        let settings = moss_voice::settings::load(settings_file);
        let openrouter_llm = settings.builtin_models.llm.as_ref().is_some_and(|p| {
            p.kind == ProviderKind::Openai && moss_voice::models::is_openrouter_https_url(&p.url)
        });
        if openrouter_llm {
            let peer = db
                .get_secret(builtin_secret_name(Section::Llm))
                .map_err(db_failure)?
                .filter(|v| !v.trim().is_empty());
            return Ok((peer, false));
        }
        Ok((None, false))
    }

    #[cfg(test)]
    mod commit_tests {
        use super::{CenterError, commit_secrets_then_settings};
        use laser_runtime::db::RuntimeDb;

        #[test]
        fn settings_failure_restores_secrets() {
            let db = RuntimeDb::open_memory().unwrap();
            db.set_secret("voice.plugin.weather", "old", 1.).unwrap();
            let changes = vec![
                ("voice.plugin.weather".to_owned(), None),
                ("voice.plugin.rag".to_owned(), Some("new-rag".to_owned())),
            ];
            let error = commit_secrets_then_settings(&db, &changes, || {
                Err(CenterError::new(
                    "plugin_settings_failed",
                    500,
                    "插件配置保存失败",
                ))
            })
            .unwrap_err();
            assert_eq!(error.code, "plugin_settings_failed");
            assert_eq!(
                db.get_secret("voice.plugin.weather").unwrap().as_deref(),
                Some("old")
            );
            assert_eq!(db.get_secret("voice.plugin.rag").unwrap(), None);
        }

        #[test]
        fn settings_success_keeps_new_secrets() {
            let db = RuntimeDb::open_memory().unwrap();
            let changes = vec![("voice.vision.main".to_owned(), Some("key".to_owned()))];
            let mut saved = false;
            commit_secrets_then_settings(&db, &changes, || {
                saved = true;
                Ok(())
            })
            .unwrap();
            assert!(saved);
            assert_eq!(
                db.get_secret("voice.vision.main").unwrap().as_deref(),
                Some("key")
            );
        }

        #[test]
        fn switching_llm_away_from_openrouter_never_exposes_new_provider_key_to_jev_reader() {
            use super::{builtin_secret_name, resolve_openrouter_jev_secret_locked};
            use moss_voice::models::{ProviderKind, Section};
            use moss_voice::settings::{BuiltinProvider, VoiceSettings};
            use std::sync::{Arc, Mutex};

            let dir = tempfile::tempdir().unwrap();
            let settings_file = dir.path().join("settings.json");
            let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));

            let mut initial = VoiceSettings::default();
            initial.builtin_models.llm = Some(BuiltinProvider {
                kind: ProviderKind::Openai,
                url: "https://openrouter.ai/api/v1".into(),
                model: "openai/gpt-4o-mini".into(),
                voice: String::new(),
                speed: 1.0,
            });
            {
                let guard = db.lock().unwrap();
                commit_secrets_then_settings(
                    &guard,
                    &[(
                        builtin_secret_name(Section::Llm).to_owned(),
                        Some("sk-or-v1-initial".into()),
                    )],
                    || {
                        moss_voice::settings::save(&settings_file, &initial)
                            .map_err(|m| CenterError::new("err", 500, m))
                    },
                )
                .unwrap();
            }

            // 模拟运行中语音服务持有的动态 SecretReader（不缓存旧布尔值）
            let reader_db = db.clone();
            let reader_file = settings_file.clone();
            let reader = move || {
                let guard = reader_db.lock().ok()?;
                resolve_openrouter_jev_secret_locked(&guard, &reader_file)
                    .ok()
                    .and_then(|(secret, _)| secret)
            };
            assert_eq!(reader().as_deref(), Some("sk-or-v1-initial"));

            // 用户将大模型切到其他服务商并换上新密钥：旧服务尚未停机期间调用 reader 也绝不读出新服务商密钥
            let mut switched = initial.clone();
            switched.builtin_models.llm = Some(BuiltinProvider {
                kind: ProviderKind::Openai,
                url: "https://api.deepseek.com/v1".into(),
                model: "deepseek-chat".into(),
                voice: String::new(),
                speed: 1.0,
            });
            {
                let guard = db.lock().unwrap();
                commit_secrets_then_settings(
                    &guard,
                    &[(
                        builtin_secret_name(Section::Llm).to_owned(),
                        Some("sk-deepseek-private".into()),
                    )],
                    || {
                        moss_voice::settings::save(&settings_file, &switched)
                            .map_err(|m| CenterError::new("err", 500, m))
                    },
                )
                .unwrap();
            }
            assert_eq!(
                reader(),
                None,
                "切换非 OpenRouter 大模型后绝不能把新服务商密钥暴露给 Jev"
            );
        }
    }

    #[cfg(test)]
    mod vision_credentials_tests {
        use super::vision_credentials;
        use moss_voice::models::ProviderKind;
        use moss_voice::settings::{BuiltinProvider, BuiltinVision};

        fn provider(kind: ProviderKind) -> BuiltinProvider {
            BuiltinProvider {
                kind,
                url: "https://llm.example.test/v1".into(),
                model: "main-model".into(),
                voice: String::new(),
                speed: 1.0,
            }
        }

        #[test]
        fn independent_vision_needs_its_own_key() {
            let setting = BuiltinVision {
                enabled: true,
                url: "https://vision.example.test/v1".into(),
                model: "vl-1".into(),
                use_dialog_llm: false,
                keep_in_history: false,
                local_model: String::new(),
            };
            assert!(
                vision_credentials(&setting, None, None, Some("llm-key")).is_none(),
                "独立的视觉模型没有密钥就不能用"
            );
            assert_eq!(
                vision_credentials(&setting, None, Some("vision-key"), Some("llm-key")),
                Some((
                    "https://vision.example.test/v1".into(),
                    "vl-1".into(),
                    "vision-key".into()
                ))
            );
        }

        #[test]
        fn dialog_llm_mode_uses_the_conversation_model_and_key() {
            let setting = BuiltinVision {
                enabled: true,
                url: String::new(),
                model: String::new(),
                use_dialog_llm: true,
                keep_in_history: false,
                local_model: String::new(),
            };
            let openai = provider(ProviderKind::Openai);
            assert_eq!(
                vision_credentials(&setting, Some(&openai), Some("vision-key"), Some("llm-key")),
                Some((
                    "https://llm.example.test/v1".into(),
                    "main-model".into(),
                    "llm-key".into()
                )),
                "多模态模式用对话模型的地址 / 模型名 / 密钥，视觉专用密钥与地址都不参与"
            );
            assert!(
                vision_credentials(&setting, Some(&openai), Some("vision-key"), None).is_none(),
                "对话模型没有密钥就不能用"
            );
            assert!(
                vision_credentials(&setting, None, Some("vision-key"), Some("llm-key")).is_none(),
                "没有配置对话模型就不能用"
            );
        }

        #[test]
        fn dialog_llm_mode_refuses_non_openai_and_disabled() {
            let mut setting = BuiltinVision {
                enabled: true,
                url: String::new(),
                model: String::new(),
                use_dialog_llm: true,
                keep_in_history: false,
                local_model: String::new(),
            };
            let doubao = provider(ProviderKind::Doubao);
            assert!(
                vision_credentials(&setting, Some(&doubao), None, Some("llm-key")).is_none(),
                "非 OpenAI 兼容的对话模型不能看图"
            );
            setting.enabled = false;
            let openai = provider(ProviderKind::Openai);
            assert!(
                vision_credentials(&setting, Some(&openai), Some("k"), Some("k")).is_none(),
                "总开关关着就不提供"
            );
        }
    }

    #[cfg(test)]
    mod local_fallback_tests {
        use super::ordered_local_tts_candidates;
        use std::path::PathBuf;

        #[test]
        fn user_selected_fallback_model_is_prioritized_then_other_installed_models_follow() {
            let both_installed = |id: &str| match id {
                "kokoro" | "matcha_zh_en" => Some(PathBuf::from(format!("/models/{id}"))),
                _ => None,
            };
            let matcha_first = ordered_local_tts_candidates("matcha_zh_en", 0, both_installed);
            assert_eq!(
                matcha_first.iter().map(|c| c.id).collect::<Vec<_>>(),
                vec!["matcha_zh_en", "kokoro"],
                "用户指定 Matcha 兜底时应先试 Matcha，若失败再顺延 Kokoro"
            );

            let kokoro_first = ordered_local_tts_candidates("kokoro", 50, both_installed);
            assert_eq!(
                kokoro_first.iter().map(|c| c.id).collect::<Vec<_>>(),
                vec!["kokoro", "matcha_zh_en"],
                "用户指定 Kokoro 兜底时应先试 Kokoro，若失败再顺延 Matcha"
            );

            let only_kokoro = ordered_local_tts_candidates("matcha_zh_en", 0, |id| {
                (id == "kokoro").then(|| PathBuf::from("/models/kokoro"))
            });
            assert_eq!(
                only_kokoro.iter().map(|c| c.id).collect::<Vec<_>>(),
                vec!["kokoro"],
                "用户所选未安装时自动顺延本机已安装的另一个模型"
            );
        }
    }

    use moss_voice_server::memory::{MemoryRecord, MemoryService, VoiceMemoryStore};

    struct DbMemoryStore {
        db: Arc<Mutex<laser_runtime::RuntimeDb>>,
    }
    /// `tests`：简易标定时每人另录的测试音向量，只在内存里，算完阈值或删除声纹即丢弃。
    struct DesktopSpeakerBackend {
        db: Arc<Mutex<laser_runtime::RuntimeDb>>,
        helper: Mutex<Option<moss_voice_providers::local::SpeakerHelper>>,
        personal: Mutex<Option<std::sync::Weak<moss_voice_server::person_context::PersonContext>>>,
        tests: Mutex<std::collections::HashMap<String, Vec<Vec<f32>>>>,
    }
    impl moss_voice_server::speaker_enrollment::EnrollmentBackend for DesktopSpeakerBackend {
        fn save_test(&self, speaker_id: &str, clips: &[Vec<f32>]) -> Result<(), String> {
            let known = self
                .db
                .lock()
                .map_err(|_| "数据库不可用")?
                .list_voice_speakers()
                .map_err(|_| "读取声纹失败")?
                .iter()
                .any(|r| r.id == speaker_id);
            if !known {
                return Err("这个人的声纹已删除".into());
            }
            self.tests
                .lock()
                .map_err(|_| "标定数据不可用")?
                .insert(speaker_id.into(), clips.to_vec());
            Ok(())
        }
        fn embedding(&self, pcm: &[i16], generation: u64) -> Result<Vec<f32>, String> {
            let helper = self
                .helper
                .lock()
                .map_err(|_| "声纹辅助程序不可用")?
                .clone()
                .ok_or("声纹辅助程序未准备")?;
            helper
                .embedding_blocking(pcm, 16000, generation)
                .map_err(|_| "声纹计算失败".into())
        }
        fn save(&self, name: &str, clips: &[Vec<f32>]) -> Result<(), String> {
            let mut bytes = [0u8; 16];
            getrandom::fill(&mut bytes).map_err(|_| "声纹编号生成失败")?;
            let id = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
            self.db
                .lock()
                .map_err(|_| "数据库不可用")?
                .enroll_voice_speaker(&id, name, "campplus_sv_zh_cn_16k_common", clips, now())
                .map_err(|_| "声纹登记失败")?;
            if let Some(personal) = self
                .personal
                .lock()
                .ok()
                .and_then(|slot| slot.as_ref().and_then(std::sync::Weak::upgrade))
            {
                personal.directory_changed();
            }
            Ok(())
        }
    }
    use moss_voice_server::person_context::{
        CalibrationReport, CalibrationSample, IdentityRequest, IdentityResolver, PersonContext,
        PersonMemoryStore, PersonScope, PersonSummary, SpeakerTemplate, match_embedding,
    };
    const SPEAKER_CALIBRATION_KEY: &str = "voice.speaker.calibration";
    const SPEAKER_MODEL: &str = "campplus_sv_zh_cn_16k_common";
    /// 简易标定结果（2026-10-14 用户决定，取代“源码写死 None”）：没有标定就不识别身份。
    /// 只对标定时的登记名单（编号 + 版本）有效；增删或重新登记任何人都会让它失效，需要重新标定。
    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    struct Calibration {
        report: CalibrationReport,
        directory: String,
        calibrated_at: f64,
    }
    /// 登记名单的比较键：CampPlus 192 维声纹的“编号:版本”，排序后拼接。
    fn speaker_directory<'a>(items: impl Iterator<Item = (&'a str, i64)>) -> String {
        let mut keys = items
            .map(|(id, revision)| format!("{id}:{revision}"))
            .collect::<Vec<_>>();
        keys.sort();
        keys.join(",")
    }
    #[cfg(test)]
    mod speaker_directory_tests {
        /// 标定只对当时的登记名单有效：顺序无关，增删人或重新登记（版本变）都会得到不同的键。
        #[test]
        fn directory_key_changes_when_anyone_is_added_removed_or_re_enrolled() {
            let key = |items: &[(&str, i64)]| super::speaker_directory(items.iter().copied());
            assert_eq!(key(&[("a", 1), ("b", 1)]), key(&[("b", 1), ("a", 1)]));
            assert_ne!(key(&[("a", 1), ("b", 1)]), key(&[("a", 1)]));
            assert_ne!(
                key(&[("a", 1), ("b", 1)]),
                key(&[("a", 1), ("b", 1), ("c", 1)])
            );
            assert_ne!(key(&[("a", 1), ("b", 1)]), key(&[("a", 2), ("b", 1)]));
        }
    }
    struct DesktopIdentity {
        host: std::sync::Weak<laser_host::HostApplication>,
        backend: Arc<DesktopSpeakerBackend>,
        calibration: Mutex<Option<Calibration>>,
        workers: Arc<tokio::sync::Semaphore>,
    }
    impl DesktopIdentity {
        /// 有标定且登记名单没变时返回标定结果。
        fn valid_calibration(&self) -> Option<Calibration> {
            let calibration = self.calibration.lock().ok()?.clone()?;
            let rows = self.backend.db.lock().ok()?.list_voice_speakers().ok()?;
            let current = speaker_directory(
                rows.iter()
                    .filter(|r| r.model_id == SPEAKER_MODEL && r.dimension == 192)
                    .map(|r| (r.id.as_str(), r.revision)),
            );
            (current == calibration.directory).then_some(calibration)
        }
    }
    impl IdentityResolver for DesktopIdentity {
        fn identify<'a>(
            &'a self,
            _: &'a ToolContext,
            pcm: &'a [i16],
            rate: u32,
            request: IdentityRequest,
        ) -> moss_voice_providers::BoxFuture<'a, Option<PersonScope>> {
            Box::pin(async move {
                let calibration = self.calibration.lock().ok()?.clone()?;
                let (threshold, margin, directory) = (
                    calibration.report.threshold,
                    calibration.report.margin,
                    calibration.directory,
                );
                // Keep the physical worker slot until a cancelled blocking
                // helper operation actually finishes; no unbounded queue.
                let worker = self.workers.clone().try_acquire_owned().ok()?;
                let backend = self.backend.clone();
                let pcm = pcm.to_vec();
                tauri::async_runtime::spawn_blocking(move || {
                    let _worker = worker;
                    let pcm = if rate == 16000 {
                        pcm
                    } else {
                        moss_voice_providers::resample::Resampler::new(rate, 16000).process(&pcm)
                    };
                    let helper = backend.helper.lock().ok()?.clone()?;
                    let embedding = helper
                        .embedding_blocking(&pcm, 16000, request.utterance_generation)
                        .ok()?;
                    let templates = backend
                        .db
                        .lock()
                        .ok()?
                        .native_voice_speaker_templates()
                        .ok()?
                        .into_iter()
                        .map(|t| SpeakerTemplate {
                            id: t.id,
                            revision: t.revision,
                            embedding: t.embedding,
                        })
                        .collect::<Vec<_>>();
                    // 登记名单在标定后变过：阈值不再可信，不识别
                    if speaker_directory(templates.iter().map(|t| (t.id.as_str(), t.revision)))
                        != directory
                    {
                        return None;
                    }
                    match_embedding(
                        &embedding,
                        &templates,
                        request.directory_generation,
                        threshold,
                        margin,
                    )
                })
                .await
                .ok()
                .flatten()
            })
        }
        fn permitted(&self, ctx: &ToolContext) -> bool {
            self.host.upgrade().is_some_and(|h| {
                h.api_service
                    .voice_session_allowed(&ctx.device_id, &ctx.session_id)
            })
        }
        fn device_allowed(&self, device: &str) -> bool {
            self.device_generation(device).is_some()
        }
        fn device_generation(&self, device: &str) -> Option<u64> {
            let host = self.host.upgrade()?;
            let db = host.db.lock().ok()?;
            let row = db.voice_device(device).ok()??;
            if !row.is_allowed() {
                return None;
            }
            Some(row.decided_at?.to_bits())
        }
        fn current(&self, scope: &PersonScope) -> bool {
            self.backend
                .db
                .lock()
                .ok()
                .and_then(|db| db.list_voice_speakers().ok())
                .is_some_and(|rows| {
                    rows.iter().any(|r| {
                        r.id == scope.id()
                            && r.revision == scope.revision()
                            && r.model_id == "campplus_sv_zh_cn_16k_common"
                            && r.dimension == 192
                    })
                })
        }
    }
    struct DbPersonStore {
        db: Arc<Mutex<laser_runtime::RuntimeDb>>,
    }
    impl PersonMemoryStore for DbPersonStore {
        fn load(&self, scope: &PersonScope) -> Result<Option<PersonSummary>, String> {
            let db = self.db.lock().map_err(|_| "数据库不可用")?;
            if !db
                .list_voice_speakers()
                .map_err(db_error)?
                .iter()
                .any(|r| r.id == scope.id() && r.revision == scope.revision())
            {
                return Ok(None);
            }
            db.voice_person_summary(scope.id())
                .map(|r| r.map(|(text, revision)| PersonSummary { text, revision }))
                .map_err(db_error)
        }
        fn update(&self, scope: &PersonScope, revision: i64, text: &str) -> Result<bool, String> {
            let db = self.db.lock().map_err(|_| "数据库不可用")?;
            if !db
                .list_voice_speakers()
                .map_err(db_error)?
                .iter()
                .any(|r| r.id == scope.id() && r.revision == scope.revision())
            {
                return Ok(false);
            }
            db.update_voice_person_summary_if_revision(scope.id(), revision, text, now())
                .map_err(db_error)
        }
    }
    impl VoiceMemoryStore for DbMemoryStore {
        fn allowed(&self, id: &str) -> Result<bool, String> {
            self.db
                .lock()
                .map_err(|_| "数据库锁失败".to_owned())?
                .voice_device(id)
                .map(|item| item.is_some_and(|d| d.is_allowed()))
                .map_err(db_error)
        }
        fn load(&self, id: &str) -> Result<Option<MemoryRecord>, String> {
            self.db
                .lock()
                .map_err(|_| "数据库锁失败".to_owned())?
                .voice_memory(id)
                .map(|r| r.map(convert_memory))
                .map_err(db_error)
        }
        fn ensure(&self, id: &str) -> Result<MemoryRecord, String> {
            self.db
                .lock()
                .map_err(|_| "数据库锁失败".to_owned())?
                .ensure_voice_memory(id, now())
                .map(convert_memory)
                .map_err(db_error)
        }
        fn update(
            &self,
            id: &str,
            revision: i64,
            summary: &str,
        ) -> Result<Option<MemoryRecord>, String> {
            self.db
                .lock()
                .map_err(|_| "数据库锁失败".to_owned())?
                .update_voice_memory_if_revision(id, revision, summary, now())
                .map(|r| r.map(convert_memory))
                .map_err(db_error)
        }
        fn list(&self) -> Result<Vec<MemoryRecord>, String> {
            self.db
                .lock()
                .map_err(|_| "数据库锁失败".to_owned())?
                .voice_memories()
                .map(|rows| rows.into_iter().map(convert_memory).collect())
                .map_err(db_error)
        }
    }
    fn convert_memory(value: laser_runtime::VoiceMemoryRecord) -> MemoryRecord {
        MemoryRecord {
            device_id: value.device_id,
            summary: value.summary,
            revision: value.revision,
            updated_at: value.updated_at,
        }
    }

    use moss_voice_server::ToolExecutor;
    use moss_voice_server::home_tools::{HomeAvailability, HomeHost, HomeTools};
    use moss_voice_server::plugins::runtime::{CombinedTools, InfoTools, PluginHost};

    use moss_voice_server::persona::{
        CustomRole, PersonaConfig, PersonaStore, PersonaSwitch, PersonaTools,
    };

    /// n59 当前说话角色的组成：内置 / 自建角色、共用的工坊补充背景。
    fn persona_config(local: &BuiltinLocal) -> PersonaConfig {
        PersonaConfig {
            active: local.persona_style.clone(),
            background: local.persona_prompt.clone(),
            custom: local
                .custom_personas
                .iter()
                .map(|c| CustomRole {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    prompt: c.prompt.clone(),
                })
                .collect(),
            risk_notes: local.persona_risk_notes,
        }
    }

    /// 语音切换角色时写入设置（只改 `persona_style`，不重启服务）。核对与更新运行中的角色都在设置写入锁内，
    /// 与界面保存（`local_save`）排成同一顺序。
    struct DesktopPersonaStore {
        settings_file: std::path::PathBuf,
    }
    impl PersonaStore for DesktopPersonaStore {
        fn save_active(
            &self,
            id: &str,
            apply: &mut dyn FnMut(PersonaConfig),
        ) -> Result<(), String> {
            let mut touched = false;
            let result = moss_voice::settings::update_voice(&self.settings_file, |settings| {
                let local = &mut settings.builtin_local;
                touched = true;
                match moss_voice::settings::validate_active_persona(id, &local.custom_personas) {
                    Ok(active) => {
                        local.persona_style = active;
                        apply(persona_config(local));
                        Ok(())
                    }
                    Err(message) => {
                        // The role is gone from the file (e.g. just deleted); re-sync the running role to it.
                        apply(persona_config(local));
                        Err(message)
                    }
                }
            });
            if result.is_err() && touched {
                // The file write failed after the running role changed; follow the file again.
                apply(persona_config(
                    &moss_voice::settings::load(&self.settings_file).builtin_local,
                ));
            }
            result
        }
    }

    struct DesktopInfoHost {
        host: Arc<laser_host::HostApplication>,
    }
    impl PluginHost for DesktopInfoHost {
        fn config(&self) -> Value {
            let path = self.host.runtime_root.path.join("settings.json");
            serde_json::to_value(moss_voice::settings::load(&path).builtin_plugins)
                .unwrap_or(Value::Null)
        }
        fn secret(&self, kind: &str) -> Option<String> {
            let name = match kind {
                "weather" => "voice.plugin.weather",
                "search" => "voice.plugin.search",
                "metaso" => "voice.plugin.metaso",
                "rag" => "voice.plugin.rag",
                _ => return None,
            };
            self.host
                .db
                .lock()
                .ok()
                .and_then(|db| db.get_secret(name).ok().flatten())
                .filter(|s| !s.is_empty())
        }
        fn permitted(&self, ctx: &ToolContext) -> bool {
            self.host
                .api_service
                .voice_session_allowed(&ctx.device_id, &ctx.session_id)
        }
    }

    // NV15 opt-in *read-only* model tool. Never expose the token or POST.
    // A saved grant binds the exact allowlist revision and token revision,
    // so editing either disables the tool until the desktop user re-consents.
    const HOME_VOICE_GRANT: &str = "voice.homeassistant.voice_read_consent";
    struct DesktopHomeHost {
        host: Arc<laser_host::HostApplication>,
        control: Arc<laser_host::home_control::HomeControlCoordinator>,
    }
    impl DesktopHomeHost {
        fn current(
            &self,
        ) -> Option<(moss_voice::settings::BuiltinHomeAssistant, u64, u64, String)> {
            let (settings, policy) = moss_voice::settings::read_home_offline(
                &self.host.runtime_root.path.join("settings.json"),
            )
            .ok()?;
            settings.validate_offline().ok()?;
            if policy == 0
                || settings.base_url.is_empty()
                || !settings.entities.iter().any(|e| e.enabled)
            {
                return None;
            }
            let db = self.host.db.lock().ok()?;
            let (has_token, revision) = db.home_token_status().ok()?;
            if !has_token || revision == 0 {
                return None;
            }
            let grant = db.get_secret(HOME_VOICE_GRANT).ok()??;
            if grant != format!("{policy}:{revision}") {
                return None;
            }
            Some((settings, policy, revision, grant))
        }
    }
    impl HomeHost for DesktopHomeHost {
        fn availability(&self) -> HomeAvailability {
            HomeAvailability {
                read: self.current().is_some(),
                control: self.control.enabled(),
            }
        }
        fn permitted(&self, context: &ToolContext) -> bool {
            self.host
                .api_service
                .voice_session_allowed(&context.device_id, &context.session_id)
        }
        fn invoke(&self, context: &ToolContext, name: &str, arguments: &Value) -> Value {
            let deny = |code: &str| serde_json::json!({"ok":false,"error":{"code":code,"retryable":false}});
            if !self.permitted(context) {
                return deny("home_disabled");
            }
            if name != "hass_get_state" {
                return self
                    .control
                    .prepare(&context.device_id, &context.session_id, name, arguments)
                    .unwrap_or_else(&deny);
            }
            let Some((config, policy, revision, _)) = self.current() else {
                return deny("home_disabled");
            };
            let target = arguments["entity"].as_str().unwrap_or("");
            if !config
                .entities
                .iter()
                .any(|e| e.enabled && e.entity_id == target)
            {
                return deny("home_unknown_entity");
            }
            let token = {
                let Ok(db) = self.host.db.lock() else {
                    return deny("home_unavailable");
                };
                match db.home_token_for_connection() {
                    Ok(Some(token)) => token,
                    _ => return deny("home_unavailable"),
                }
            };
            let discovered = moss_voice::home_readonly::discover_guarded(
                &config,
                &token,
                policy,
                revision,
                || {
                    self.permitted(context)
                        && self.current().is_some_and(|(latest, p, r, _)| {
                            latest == config && p == policy && r == revision
                        })
                },
            );
            if !self.permitted(context)
                || self
                    .current()
                    .is_none_or(|(new, p, r, _)| new != config || p != policy || r != revision)
            {
                return deny("home_stale_permission");
            }
            match discovered {
                Ok(output) => match output.entities.iter().find(|e| e.entity_id == target) {
                    Some(entity) => serde_json::json!({"ok":true,"entity_id":entity.entity_id,
                        "state":entity.state,"last_updated":entity.last_updated,
                        "attributes":entity.attributes,"actions_available":entity.actions_available}),
                    None => deny("home_unknown_entity"),
                },
                Err(moss_voice::home_readonly::DiscoveryError::Unauthorized) => {
                    deny("home_unauthorized")
                }
                Err(_) => deny("home_unavailable"),
            }
        }
    }

    /// NV12: fresh signed-in device permission and fresh user configuration are
    /// consulted for every image; snapshots are never logged or stored as conversation history.
    /// 2026-10-16：`use_dialog_llm` 打开时改用当前对话大模型的地址 / 模型名与 `voice.llm.main` 密钥，
    /// 仍然只发这一次请求、不进对话历史；对话模型必须 OpenAI 兼容且支持看图。
    fn vision_credentials(
        setting: &moss_voice::settings::BuiltinVision,
        llm: Option<&moss_voice::settings::BuiltinProvider>,
        vision_key: Option<&str>,
        llm_key: Option<&str>,
    ) -> Option<(String, String, String)> {
        if !setting.enabled {
            return None;
        }
        let usable = |url: &str, model: &str, key: &str| {
            !model.trim().is_empty()
                && !key.trim().is_empty()
                && moss_voice_providers::vision::endpoint(url).is_ok()
        };
        if setting.use_dialog_llm {
            let llm = llm.filter(|llm| llm.kind.is_openai())?;
            let key = llm_key?;
            return usable(&llm.url, &llm.model, key)
                .then(|| (llm.url.clone(), llm.model.clone(), key.to_owned()));
        }
        let key = vision_key?;
        usable(&setting.url, &setting.model, key)
            .then(|| (setting.url.clone(), setting.model.clone(), key.to_owned()))
    }
    /// 本地视觉模型要怎么启动（由 `BuiltinVoice::sync_local_vision` 按设置与安装情况给出）。
    #[derive(Clone, PartialEq, Eq)]
    struct LocalVisionLaunch {
        model: String,
        dir: std::path::PathBuf,
        root: std::path::PathBuf,
        mirror: Option<String>,
    }

    #[derive(Default)]
    struct LocalVisionState {
        launch: Option<LocalVisionLaunch>,
        server: Option<moss_voice::local_vision::LocalVisionServer>,
        starting: Option<Arc<std::sync::atomic::AtomicBool>>,
        failure: Option<String>,
        generation: u64,
    }

    /// n74 本地视觉模型（Qwen3-VL）运行管理：后台预加载，就绪后视觉理解直接调用本机地址；
    /// 停用、换模型或服务停止时结束推理进程释放内存。与本地 Jev 分开运行，互不排队。
    #[derive(Default)]
    pub(crate) struct LocalVision {
        state: Mutex<LocalVisionState>,
        /// 串行化启动，避免预加载与设备请求同时各起一个进程
        start_lock: Mutex<()>,
    }

    impl LocalVision {
        fn lock(&self) -> std::sync::MutexGuard<'_, LocalVisionState> {
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        /// 设置要用的模型；为 `None` 时停掉推理进程。模型或路径变了也会先停掉旧的。
        fn configure(&self, launch: Option<LocalVisionLaunch>) {
            let mut state = self.lock();
            if state.launch == launch {
                return;
            }
            state.generation += 1;
            if let Some(cancel) = state.starting.take() {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            state.server = None;
            state.failure = None;
            state.launch = launch;
        }

        fn launch(&self) -> Option<LocalVisionLaunch> {
            self.lock().launch.clone()
        }

        fn ready_url(&self) -> Option<String> {
            let mut state = self.lock();
            let alive = state.server.as_mut()?.check_alive();
            match alive {
                Ok(()) => state.server.as_ref().map(|server| server.base_url()),
                Err(error) => {
                    state.server = None;
                    state.failure = Some(error.clone());
                    record_moss_event(
                        EventRecord::error(DiagCategory::Model, "本地视觉模型的推理进程意外退出")
                            .hint(
                                "下次看图时会自动重新加载；反复出现时到“本地模型”重新下载视觉模型",
                            )
                            .detail(error),
                    );
                    None
                }
            }
        }

        /// 确保推理进程在跑，返回本机地址；没在跑就启动并等它就绪（会阻塞，只在后台线程调用）。
        fn ensure(&self) -> Result<String, String> {
            if let Some(url) = self.ready_url() {
                return Ok(url);
            }
            let _start = self
                .start_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(url) = self.ready_url() {
                return Ok(url);
            }
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (launch, generation) = {
                let mut state = self.lock();
                let launch = state
                    .launch
                    .clone()
                    .ok_or_else(|| "没有选用已下载的本地视觉模型".to_owned())?;
                state.starting = Some(cancel.clone());
                state.failure = None;
                (launch, state.generation)
            };
            let result = moss_voice::local_vision::LocalVisionServer::start(
                &launch.model,
                &launch.dir,
                &launch.root,
                launch.mirror.as_deref(),
                &cancel,
            );
            let mut state = self.lock();
            if state.generation != generation {
                return Err("本地视觉模型设置已变化，请重试".into());
            }
            state.starting = None;
            match result {
                Ok(server) => {
                    let url = server.base_url();
                    state.server = Some(server);
                    Ok(url)
                }
                Err(error) => {
                    state.failure = Some(error.clone());
                    record_moss_event(
                        EventRecord::error(
                            DiagCategory::Model,
                            format!("本地视觉模型（{}）启动失败", launch.model),
                        )
                        .hint("确认模型已完整下载、磁盘与内存够用；看技术细节里推理进程的输出")
                        .detail(&error),
                    );
                    Err(error)
                }
            }
        }

        /// 后台预加载（服务启动、保存设置后调用），不阻塞调用方。
        fn preload(self: &Arc<Self>) {
            if self.launch().is_none()
                || self.ready_url().is_some()
                || self.lock().starting.is_some()
            {
                return;
            }
            let this = self.clone();
            let _ = std::thread::Builder::new()
                .name("moss-local-vision".into())
                .spawn(move || {
                    let _ = this.ensure();
                });
        }

        /// 给语音中心卡片显示：`idle` / `starting` / `ready` / `failed`。
        fn status(&self) -> Value {
            let ready = self.ready_url().is_some();
            let state = self.lock();
            let phase = if ready {
                "ready"
            } else if state.starting.is_some() {
                "starting"
            } else if state.failure.is_some() {
                "failed"
            } else {
                "idle"
            };
            serde_json::json!({
                "state": phase,
                "model": state.launch.as_ref().map(|launch| launch.model.clone()),
                "accelerator": state.server.as_ref().map(|server| server.accelerator_text()),
                "error": state.failure,
            })
        }
    }

    struct DesktopVisionFactory {
        host: Arc<laser_host::HostApplication>,
        local: Arc<LocalVision>,
    }
    impl DesktopVisionFactory {
        fn vision_setting(&self) -> moss_voice::settings::BuiltinVision {
            moss_voice::settings::load(&self.host.runtime_root.path.join("settings.json"))
                .builtin_vision
        }
        /// 返回（地址、模型、密钥、配置修订号）。
        fn profile(&self) -> Option<(String, String, String, u64)> {
            use std::hash::{Hash, Hasher};
            let settings =
                moss_voice::settings::load(&self.host.runtime_root.path.join("settings.json"));
            // n80：Colab T4 联动开启且密钥就绪时，临时接管摄像头看图（关闭开关即刻恢复下方原视觉配置）
            if settings.builtin_colab.enabled
                && let Some(key) = self
                    .host
                    .db
                    .lock()
                    .ok()
                    .and_then(|db| {
                        db.get_secret(moss_voice::colab::COLAB_SECRET_NAME)
                            .ok()
                            .flatten()
                    })
                    .filter(|s| !s.trim().is_empty())
                && settings.builtin_colab.active(true)
                && let Ok(url) = moss_voice::colab::normalize_colab_url(&settings.builtin_colab.url)
                && moss_voice_providers::vision::endpoint(&url).is_ok()
            {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                "colab".hash(&mut hasher);
                url.hash(&mut hasher);
                settings.builtin_colab.llm_model.hash(&mut hasher);
                key.hash(&mut hasher);
                return Some((url, settings.builtin_colab.llm_model, key, hasher.finish()));
            }
            let setting = settings.builtin_vision;
            if setting.enabled && !setting.use_dialog_llm && !setting.local_model.is_empty() {
                // 本地视觉：地址在打开会话时才确定（推理进程按需启动），这里只看是否已选好并下载
                let launch = self
                    .local
                    .launch()
                    .filter(|launch| launch.model == setting.local_model)?;
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                "local".hash(&mut hasher);
                launch.model.hash(&mut hasher);
                launch.dir.hash(&mut hasher);
                return Some((String::new(), launch.model, String::new(), hasher.finish()));
            }
            let (vision_key, llm_key) = {
                let db = self.host.db.lock().ok()?;
                (
                    db.get_secret("voice.vision.main").ok().flatten(),
                    db.get_secret("voice.llm.main").ok().flatten(),
                )
            };
            let (url, model, key) = vision_credentials(
                &setting,
                settings.builtin_models.llm.as_ref(),
                vision_key.as_deref(),
                llm_key.as_deref(),
            )?;
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            setting.enabled.hash(&mut hasher);
            setting.use_dialog_llm.hash(&mut hasher);
            url.hash(&mut hasher);
            model.hash(&mut hasher);
            key.hash(&mut hasher);
            Some((url, model, key, hasher.finish()))
        }
    }
    impl moss_voice_server::vision::VisionFactory for DesktopVisionFactory {
        fn ready(&self) -> bool {
            self.profile().is_some()
        }
        fn open(&self) -> Option<moss_voice_server::vision::VisionSession> {
            let (url, model, key, generation) = self.profile()?;
            let (url, model) = if url.is_empty() {
                // 本地视觉：在后台线程里调用，推理进程没在跑就等它加载好（服务启动时已预加载，通常立即可用）
                (
                    self.local.ensure().ok()?,
                    moss_voice::local_vision::LOCAL_VISION_MODEL_NAME.to_owned(),
                )
            } else {
                (url, model)
            };
            let provider =
                moss_voice_providers::vision::OpenAiVision::new(&url, &model, &key).ok()?;
            Some(moss_voice_server::vision::VisionSession {
                provider: Arc::new(provider),
                generation,
            })
        }
        fn still_current(&self, generation: u64) -> bool {
            self.profile()
                .is_some_and(|(_, _, _, revision)| revision == generation)
        }
        /// 2026-10-16：视觉卡片打开“把照片留在对话上下文里”时，服务端才会把这张照片交给会话。
        fn keep_in_history(&self) -> bool {
            let setting = self.vision_setting();
            setting.enabled && setting.keep_in_history
        }
    }

    /// NV17 固件 OTA（2026-10-15）：推送记录在 SQLite，固件文件在运行目录 `firmware/<sha256>.bin`。
    struct DesktopFirmwareHost {
        db: Arc<Mutex<laser_runtime::RuntimeDb>>,
        dir: std::path::PathBuf,
    }
    /// 推送有效期：设备在这段时间内开机检查更新才会拿到。
    const FIRMWARE_OFFER_SECONDS: f64 = 24.0 * 3600.0;
    use moss_voice_server::firmware::sha256_hex;
    fn random_hex(len: usize) -> Result<String, CenterError> {
        let mut bytes = vec![0u8; len / 2];
        getrandom::fill(&mut bytes).map_err(|_| internal("随机数生成失败"))?;
        Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }
    impl moss_voice_server::firmware::FirmwareHost for DesktopFirmwareHost {
        fn offer(&self, device_id: &str, current_version: &str) -> Option<(String, String)> {
            let (version, token) = self
                .db
                .lock()
                .ok()?
                .active_firmware_offer(device_id, now())
                .ok()??;
            moss_voice_server::firmware::is_newer(&version, current_version)
                .then_some((version, token))
        }
        fn fetch(&self, token: &str, peer: &str) -> Option<Vec<u8>> {
            let db = self.db.lock().ok()?;
            let offer = db.firmware_offer_by_token(token, now()).ok()??;
            // 令牌只给这台设备：下载方必须是这台已允许设备最近一次请求 OTA 的地址
            let device = db.voice_device(&offer.device_id).ok()??;
            if !device.is_allowed() || device.last_ip != peer {
                return None;
            }
            drop(db);
            let bytes =
                std::fs::read(self.dir.join(format!("{}.bin", offer.artifact_sha256))).ok()?;
            if sha256_hex(&bytes) != offer.artifact_sha256 {
                return None;
            }
            let db = self.db.lock().ok()?;
            if offer.download_status == "pending" {
                let _ = db.update_firmware_download_if_revision(
                    &offer.delivery_id,
                    offer.revision,
                    "downloading",
                    now(),
                );
            }
            Some(bytes)
        }
        fn reported(&self, device_id: &str, version: &str) {
            if let Ok(db) = self.db.lock() {
                let _ = db.firmware_device_reported(device_id, version, now());
            }
        }
    }

    #[cfg(test)]
    mod firmware_host_tests {
        use super::*;
        use moss_voice_server::firmware::FirmwareHost;
        /// 下载必须来自这台已允许设备最近请求 OTA 的地址，文件按 SHA-256 核对，第一次下载记为“已开始下载”。
        #[test]
        fn download_is_bound_to_device_ip_and_verified_by_hash() {
            let dir = tempfile::tempdir().unwrap();
            let db = Arc::new(Mutex::new(laser_runtime::RuntimeDb::open_memory().unwrap()));
            let bytes = vec![0xE9u8; 2048];
            let sha = sha256_hex(&bytes);
            std::fs::write(dir.path().join(format!("{sha}.bin")), &bytes).unwrap();
            {
                let db = db.lock().unwrap();
                db.voice_device_seen("dev", "client", "board", "1.0.0", "192.168.1.9", now())
                    .unwrap();
                db.set_voice_device_status("dev", laser_runtime::VOICE_DEVICE_ALLOWED, now())
                    .unwrap();
                db.add_firmware_artifact(&sha, "1.1.0", "", 2048, now())
                    .unwrap();
                db.create_firmware_offer(
                    "fw-1",
                    "dev",
                    &sha,
                    "1.1.0",
                    &"a".repeat(32),
                    now() + 60.0,
                    now(),
                )
                .unwrap();
            }
            let host = DesktopFirmwareHost {
                db: db.clone(),
                dir: dir.path().to_path_buf(),
            };
            assert_eq!(
                host.offer("dev", "1.0.0"),
                Some(("1.1.0".into(), "a".repeat(32)))
            );
            assert_eq!(host.offer("dev", "1.1.0"), None, "不比设备新就不下发");
            assert!(
                host.fetch(&"a".repeat(32), "192.168.1.50").is_none(),
                "别的地址拿不到"
            );
            assert_eq!(
                host.fetch(&"a".repeat(32), "192.168.1.9").unwrap().len(),
                2048
            );
            assert_eq!(
                db.lock().unwrap().list_firmware_deliveries().unwrap()[0].download_status,
                "downloading"
            );
            std::fs::write(dir.path().join(format!("{sha}.bin")), b"tampered").unwrap();
            assert!(
                host.fetch(&"a".repeat(32), "192.168.1.9").is_none(),
                "文件被改动就不发"
            );
            host.reported("dev", "1.1.0");
            assert_eq!(
                db.lock().unwrap().list_firmware_deliveries().unwrap()[0].device_report_status,
                "reported"
            );
            assert!(host.offer("dev", "1.0.0").is_none(), "升级完成后不再下发");
        }
    }

    /// 设备登记与密钥：存 MOSS 的 SQLite。
    struct DbRegistry {
        db: Arc<Mutex<laser_runtime::RuntimeDb>>,
        host: Arc<laser_host::HostApplication>,
        settings_file: std::path::PathBuf,
    }

    impl DeviceRegistry for DbRegistry {
        fn seen(&self, visit: &DeviceVisit) -> Result<DeviceStatus, String> {
            let db = self.db.lock().map_err(|_| "数据库锁失败".to_owned())?;
            let record = db
                .voice_device_seen(
                    &visit.device_id,
                    &visit.client_id,
                    &visit.model,
                    &visit.firmware_version,
                    &visit.ip,
                    now(),
                )
                .map_err(db_error)?;
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

        fn sessions_changed(&self, count: usize) {
            laser_host::set_voice_device_sessions(Some(u32::try_from(count).unwrap_or(u32::MAX)));
        }

        fn pulse_changed(&self, phase: Option<&str>, level: u8) {
            laser_host::set_voice_pulse(phase, level);
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

        /// 2026-11-09：设备整包下载完资源包，才把“等下载”的全屏画风转为已装（重启重连后按它切画面）。
        fn assets_downloaded(&self, device: &str) {
            let _ = moss_voice::settings::update_voice(&self.settings_file, |s| {
                if let Some(entry) = s.builtin_device_prefs.get_mut(device) {
                    entry.confirm_assets_downloaded();
                }
                Ok(())
            });
        }
    }

    /// 对话中调用 MOSS 工具（NV4）：身份 `CallerContext::Voice`；运动与出光跟随严格模式开关，桌面控制类工具始终拒绝。
    fn preview_candidate_from_json(val: &Value) -> Option<&str> {
        [
            val.pointer("/workflow/artifacts/path_preview_file"),
            val.pointer("/workflow/artifacts/preview_file"),
            val.pointer("/workflow/artifacts/image_file"),
            val.pointer("/artifacts/path_preview_file"),
            val.pointer("/artifacts/preview_file"),
            val.pointer("/artifacts/image_file"),
            val.get("path_preview_file"),
            val.get("preview_file"),
            val.get("image_file"),
        ]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|s| !s.is_empty())
    }

    fn format_workflow_oled_summary(val: &Value) -> Option<String> {
        let wf = val.get("workflow").unwrap_or(val);
        if !wf.is_object() {
            return None;
        }
        let input = wf.get("input").unwrap_or(wf);
        let snap = wf.get("confirmation_snapshot").unwrap_or(wf);
        let artifacts = wf.get("artifacts").unwrap_or(wf);
        let estimate = wf
            .get("estimate")
            .or_else(|| artifacts.get("estimate"))
            .unwrap_or(wf);

        let mut parts: Vec<String> = Vec::new();
        let mat = input
            .get("material")
            .or_else(|| wf.get("material"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let thick = input
            .get("thickness_mm")
            .or_else(|| wf.get("thickness_mm"))
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v > 0.0);
        let mode_label = match input
            .get("laser_mode")
            .or_else(|| wf.get("laser_mode"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
        {
            "cut" => "切割",
            "engrave" => "雕刻",
            _ => "",
        };
        match (thick, mat) {
            (Some(t), Some(m)) => {
                let t_str = if (t.fract()).abs() < 1e-3 {
                    format!("{}mm", t.round() as i64)
                } else {
                    format!("{t:.1}mm")
                };
                parts.push(format!("{t_str}{m}{mode_label}"));
            }
            (None, Some(m)) => parts.push(format!("{m}{mode_label}")),
            (Some(t), None) if !mode_label.is_empty() => {
                parts.push(format!("{t:.1}mm{mode_label}"))
            }
            _ if !mode_label.is_empty() => parts.push(mode_label.to_owned()),
            _ => {}
        }

        if let Some(txt) = input
            .get("text")
            .or_else(|| wf.get("text"))
            .and_then(Value::as_str)
            .map(|s| s.replace(['\r', '\n'], " ").trim().to_owned())
            .filter(|s| !s.is_empty())
        {
            let short: String = txt.chars().take(10).collect();
            let ell = if txt.chars().count() > 10 { "…" } else { "" };
            parts.push(format!("「{short}{ell}」"));
        }

        let w_mm = estimate
            .get("width_mm")
            .or_else(|| artifacts.get("width_mm"))
            .or_else(|| input.get("width_mm"))
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v > 0.0);
        let h_mm = estimate
            .get("height_mm")
            .or_else(|| artifacts.get("height_mm"))
            .or_else(|| input.get("height_mm"))
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v > 0.0);
        if let (Some(w), Some(h)) = (w_mm, h_mm) {
            parts.push(format!("{}×{}mm", w.round() as i64, h.round() as i64));
        }

        let power = snap
            .get("power")
            .or_else(|| input.get("power"))
            .and_then(Value::as_i64);
        let feed = snap
            .get("feed_rate")
            .or_else(|| input.get("feed_rate"))
            .and_then(Value::as_i64);
        let passes = snap
            .get("passes")
            .or_else(|| input.get("passes"))
            .and_then(Value::as_i64);
        let mut p_tokens = Vec::new();
        if let Some(p) = power {
            p_tokens.push(format!("S{p}"));
        }
        if let Some(f) = feed {
            p_tokens.push(format!("F{f}"));
        }
        if let Some(ps) = passes {
            p_tokens.push(format!("×{ps}遍"));
        }
        if !p_tokens.is_empty() {
            parts.push(p_tokens.join(" "));
        }

        if let Some(secs) = estimate
            .get("estimated_seconds")
            .or_else(|| wf.get("estimated_seconds"))
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v >= 1.0)
        {
            let total = secs.round() as u64;
            if total >= 60 {
                parts.push(format!("约{}分{}秒", total / 60, total % 60));
            } else {
                parts.push(format!("约{total}秒"));
            }
        }

        if parts.is_empty() {
            wf.get("workflow_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|id| format!("预览就绪 · {id}"))
        } else {
            Some(parts.join(" · "))
        }
    }

    /// 提取用于彩屏 PNG 底部 5×7 像素字模的精简 ASCII 标尺（如 `40X30MM S600 F1200`）。
    fn format_workflow_ruler_ascii(val: &Value) -> Option<String> {
        let wf = val.get("workflow").unwrap_or(val);
        if !wf.is_object() {
            return None;
        }
        let input = wf.get("input").unwrap_or(wf);
        let snap = wf.get("confirmation_snapshot").unwrap_or(wf);
        let artifacts = wf.get("artifacts").unwrap_or(wf);
        let estimate = wf
            .get("estimate")
            .or_else(|| artifacts.get("estimate"))
            .unwrap_or(wf);
        let mut tokens: Vec<String> = Vec::new();
        let w_mm = estimate
            .get("width_mm")
            .or_else(|| artifacts.get("width_mm"))
            .or_else(|| input.get("width_mm"))
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v > 0.0);
        let h_mm = estimate
            .get("height_mm")
            .or_else(|| artifacts.get("height_mm"))
            .or_else(|| input.get("height_mm"))
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v > 0.0);
        if let (Some(w), Some(h)) = (w_mm, h_mm) {
            tokens.push(format!("{}X{}MM", w.round() as i64, h.round() as i64));
        }
        if let Some(p) = snap
            .get("power")
            .or_else(|| input.get("power"))
            .and_then(Value::as_i64)
        {
            tokens.push(format!("S{p}"));
        }
        if let Some(f) = snap
            .get("feed_rate")
            .or_else(|| input.get("feed_rate"))
            .and_then(Value::as_i64)
        {
            tokens.push(format!("F{f}"));
        }
        if let Some(ps) = snap
            .get("passes")
            .or_else(|| input.get("passes"))
            .and_then(Value::as_i64)
            .filter(|v| *v > 1)
        {
            tokens.push(format!("P{ps}"));
        }
        (!tokens.is_empty()).then(|| tokens.join(" "))
    }

    fn extract_preview_bundle_for_host(
        host: &laser_host::HostApplication,
        tool_detail: Option<&str>,
    ) -> Result<(Option<Vec<u8>>, String, Option<String>), String> {
        use laser_runtime::RuntimeStore;
        let roots = host.api_service.preview_roots();
        let try_read = |rel_or_abs: &str| -> Option<Vec<u8>> {
            let resolved =
                laser_api::resolve_preview_file(std::path::Path::new(rel_or_abs), roots).ok()?;
            std::fs::read(resolved).ok().filter(|b| !b.is_empty())
        };
        if let Some(detail) = tool_detail
            && let Ok(parsed) = serde_json::from_str::<Value>(detail)
        {
            let bytes = preview_candidate_from_json(&parsed).and_then(try_read);
            let summary = format_workflow_oled_summary(&parsed);
            let ruler = format_workflow_ruler_ascii(&parsed);
            if bytes.is_some() || summary.is_some() {
                return Ok((
                    bytes,
                    summary.unwrap_or_else(|| "激光雕刻预览已就绪".into()),
                    ruler,
                ));
            }
        }
        let recent = host
            .db
            .list_recent(laser_runtime::RecordKind::Workflow, 10)
            .map_err(|_| "查询最近加工预览失败".to_owned())?;
        for record in recent {
            let bytes = preview_candidate_from_json(&record.payload).and_then(try_read);
            let summary = format_workflow_oled_summary(&record.payload);
            let ruler = format_workflow_ruler_ascii(&record.payload);
            if bytes.is_some() || summary.is_some() {
                return Ok((
                    bytes,
                    summary.unwrap_or_else(|| "激光雕刻预览已就绪".into()),
                    ruler,
                ));
            }
        }
        Err("当前还没有可推送的激光加工预览（请先在工作台、画板或语音生成一次预览）".into())
    }
    /// 对话中调用 MOSS 工具（NV4）：身份 `CallerContext::Voice`；运动与出光跟随严格模式开关，桌面控制类工具始终拒绝。
    struct DesktopToolHost {
        host: Arc<laser_host::HostApplication>,
    }
    impl ToolHost for DesktopToolHost {
        fn list(&self) -> Value {
            let mut base = laser_host::voice_tools_list(&self.host.api_service);
            let extra = self.host.api_service.agent_tools_list();
            if let (Some(base_tools), Some(agent_tools)) =
                (base["tools"].as_array_mut(), extra["tools"].as_array())
            {
                base_tools.extend(agent_tools.iter().cloned());
            }
            base
        }

        fn call(&self, context: &ToolContext, name: &str, arguments: &Value) -> ToolHostResult {
            let result = if matches!(name, "agent_codex" | "agent_claude_code") {
                let caller = laser_contracts::CallerContext::Voice {
                    device_id: context.device_id.clone(),
                    session_id: context.session_id.clone(),
                };
                self.host
                    .api_service
                    .call_agent_tool(&caller, name, arguments)
            } else {
                laser_host::voice_tool_call(
                    &self.host.api_service,
                    &context.device_id,
                    &context.session_id,
                    name,
                    arguments,
                )
            };
            // n70（2026-11-08 用户决定）：生成预览时不再自动推到小智屏幕，开始雕刻后才显示刀路（见 `workflow_preview`）
            ToolHostResult {
                success: result.success,
                result: result.result,
                detail: result.detail,
            }
        }

        /// 加工结束播报的只读轮询：走不计入调用统计的进程内只读通道。
        fn recent_workflows(&self, device_id: &str, session_id: &str) -> Option<Value> {
            laser_host::voice_recent_workflows(&self.host.api_service, device_id, session_id)
        }

        /// n70 正在雕刻的任务的刀路预览原图：只读本机任务记录里的预览文件（限预览目录内），不走工具分发。
        fn workflow_preview(
            &self,
            device_id: &str,
            session_id: &str,
            workflow_id: &str,
        ) -> Option<Vec<u8>> {
            use laser_runtime::RuntimeStore;
            if !self
                .host
                .api_service
                .voice_session_allowed(device_id, session_id)
            {
                return None;
            }
            let record = self
                .host
                .db
                .get(laser_runtime::RecordKind::Workflow, workflow_id)
                .ok()
                .flatten()?;
            let rel = preview_candidate_from_json(&record.payload)?;
            let resolved = laser_api::resolve_preview_file(
                std::path::Path::new(rel),
                self.host.api_service.preview_roots(),
            )
            .ok()?;
            std::fs::read(resolved).ok().filter(|b| {
                !b.is_empty() && b.len() <= moss_voice_server::preview_push::MAX_SOURCE_IMAGE_BYTES
            })
        }
    }

    fn probe_local_jev_health_model(
        addr: std::net::SocketAddr,
        connect_timeout: std::time::Duration,
    ) -> Option<String> {
        use std::io::{Read, Write};
        let Ok(mut stream) = std::net::TcpStream::connect_timeout(&addr, connect_timeout) else {
            return None;
        };
        let io_timeout = Some(std::time::Duration::from_millis(300));
        let _ = stream.set_write_timeout(io_timeout);
        let _ = stream.set_read_timeout(io_timeout);
        if stream
            .write_all(
                b"GET /health HTTP/1.1\r\nHost: 127.0.0.1:18090\r\nConnection: close\r\n\r\n",
            )
            .is_err()
        {
            return None;
        }
        let mut buf = [0u8; 1024];
        let mut read_total = 0;
        while read_total < buf.len() {
            match stream.read(&mut buf[read_total..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => read_total += n,
            }
        }
        let raw = &buf[..read_total];
        let sep = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&raw[..sep]);
        let status_line = head.lines().next()?;
        let is_200 = status_line == "HTTP/1.1 200"
            || status_line.starts_with("HTTP/1.1 200 ")
            || status_line == "HTTP/1.0 200"
            || status_line.starts_with("HTTP/1.0 200 ");
        if !is_200 {
            return None;
        }
        let body = String::from_utf8_lossy(&raw[sep + 4..]);
        let trimmed = body.trim();
        if trimmed.eq_ignore_ascii_case("ok") {
            return Some(moss_voice::settings::LOCAL_LAYA_JEV_MODEL.to_owned());
        }
        let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            return None;
        };
        let healthy = json.get("ok").and_then(serde_json::Value::as_bool) == Some(true)
            || json
                .get("status")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|s| {
                    matches!(
                        s.trim().to_ascii_lowercase().as_str(),
                        "ok" | "healthy" | "ready"
                    )
                });
        if !healthy {
            return None;
        }
        let is_onejev_like = json.get("engine").and_then(serde_json::Value::as_str)
            == Some("onejev")
            || json.get("fork_mode").is_some()
            || json.get("prompt_version").is_some();
        if let Some(raw_model) = json
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if let Some(canonical) = moss_voice::settings::canonical_local_jev_model(raw_model) {
                return Some(canonical.to_owned());
            }
            if is_onejev_like {
                return Some(format!("external_onejev:{raw_model}"));
            }
        }
        if is_onejev_like {
            return Some("external_onejev".to_owned());
        }
        Some(moss_voice::settings::LOCAL_LAYA_JEV_MODEL.to_owned())
    }

    struct LocalLayaRunner {
        active_model: Option<String>,
        starting_model: Option<String>,
        starting_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
        cancelling_model: Option<String>,
        child: Option<std::process::Child>,
        job: Option<moss_voice::supervisor::JobHandle>,
        onejev: Option<moss_voice::onejev::OneJevGateway>,
        generation: u64,
        /// 最近一次启动失败（模型编号、原因），供状态接口说明；开始新的启动或启动成功时清除（2026-11-07）。
        last_failure: Option<(String, String)>,
    }

    impl LocalLayaRunner {
        fn is_alive_for(&mut self, canonical_model: &str) -> bool {
            if self.active_model.as_deref() != Some(canonical_model) {
                return false;
            }
            if canonical_model == moss_voice::settings::LOCAL_LAYA_JEV_MODEL {
                self.child
                    .as_mut()
                    .is_some_and(|c| matches!(c.try_wait(), Ok(None)))
            } else {
                self.onejev
                    .as_ref()
                    .is_some_and(moss_voice::onejev::OneJevGateway::is_alive)
            }
        }

        fn is_starting_for(&self, canonical_model: &str) -> bool {
            self.starting_model.as_deref() == Some(canonical_model)
                && self
                    .starting_cancel
                    .as_ref()
                    .is_some_and(|c| !c.load(std::sync::atomic::Ordering::SeqCst))
        }

        fn cancel_in_flight(&mut self) {
            if let Some(model) = self.starting_model.take() {
                self.cancelling_model = Some(model);
            }
            if let Some(cancel) = self.starting_cancel.take() {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        fn stop(&mut self) {
            self.generation = self.generation.wrapping_add(1);
            self.cancel_in_flight();
            self.kill_reaped();
        }

        fn kill_reaped(&mut self) {
            self.active_model = None;
            if let Some(mut gw) = self.onejev.take() {
                gw.stop();
            }
            let job = self.job.take();
            if let Some(mut child) = self.child.take() {
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    let pid = child.id();
                    let _ = std::process::Command::new("taskkill")
                        .args(["/PID", &pid.to_string(), "/T", "/F"])
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .creation_flags(0x0800_0000)
                        .status();
                }
                if let Some(job) = job.as_ref() {
                    job.terminate();
                }
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    impl Drop for LocalLayaRunner {
        fn drop(&mut self) {
            self.stop();
        }
    }

    #[derive(Default)]
    struct ColabCliState {
        account_hint: String,
        expires_in: String,
        whoami_probed: bool,
        whoami_probing: bool,
        auth_url: Option<String>,
        runner_state: String,
        runner_step: String,
        runner_error: Option<String>,
        generation: u64,
    }

    pub struct BuiltinVoice {
        #[cfg_attr(test, allow(dead_code))]
        self_weak: std::sync::Weak<BuiltinVoice>,
        host: Arc<laser_host::HostApplication>,
        /// 串行化启停与重启编排，避免加载本地模型/扫描音乐时长时间阻塞 `self.server` 状态轮询。
        lifecycle: Mutex<()>,
        server: Arc<Mutex<Option<VoiceServer>>>,
        /// 仅内存、重启即清的实时对话调试流总开关（默认 false，不落盘）
        debug_trace_enabled: std::sync::atomic::AtomicBool,
        memory: Arc<MemoryService>,
        speaker: Arc<moss_voice_server::speaker_enrollment::SpeakerEnrollment>,
        speaker_backend: Arc<DesktopSpeakerBackend>,
        personal: Arc<PersonContext>,
        identity: Arc<DesktopIdentity>,
        home_control: Arc<laser_host::home_control::HomeControlCoordinator>,
        /// 最近一次自动启动失败的原因（语音链路为内置服务时随 MOSS 启动）
        last_error: Mutex<Option<String>>,
        /// 诊断记录本（2026-11-07 n68）：服务事件、对话耗时与桌面端后台失败（本地模型、Laya、固件记录等），
        /// 传给每次启动的语音服务，停止、重启、崩溃重启都不清空；导出诊断包时整份写出。
        diagnostics: Arc<moss_voice_server::DiagnosticsLog>,
        /// 本地音乐目录换机或拔盘不可用时的降级提示（不阻断内置语音主服务启动）
        music_warning: Mutex<Option<String>>,
        /// 声纹模型或辅助程序未就绪时的降级提示（保持个人上下文隔离，不回退共享设备记忆）
        speaker_warning: Mutex<Option<String>>,
        /// 本地模型下载与安装（默认运行目录 `models/`，可通过 `MOSS_LAYA_ROOT` 指定外置 `<path>/models`）
        models: ModelManager,
        /// Laya 的 Python 运行环境自动准备（2026-11-07：点下载 Laya 时在软件目录创建虚拟环境并安装 laya[serve]）
        laya_setup: moss_voice::laya_env::LayaSetup,
        /// 测试用 Laya 根目录（测试中不碰真实用户目录下的虚拟环境）
        #[cfg(test)]
        laya_root_for_test: Mutex<Option<std::path::PathBuf>>,
        /// 运行中的本地模型辅助程序（随内置服务启停）
        helper: Mutex<Option<LocalHelper>>,
        /// 串行化本地 Jev 决策服务（OneJev / Laya）启动与模型切换，不阻塞 `stop_laya`
        jev_start_lock: Arc<Mutex<()>>,
        /// n74 本地视觉模型运行管理
        local_vision: Arc<LocalVision>,
        /// n80 Colab CLI 登录检测与后台云端 T4 会话托管
        colab_cli: Arc<Mutex<ColabCliState>>,
        #[cfg(test)]
        colab_cli_installed_for_test: std::sync::atomic::AtomicBool,
        /// 运行中的本地 Jev `/v1/systemone` 决策服务（OneJev `llama-server` 网关或 Laya 子进程，随本地 Jev 开关启停）
        laya: Arc<Mutex<LocalLayaRunner>>,
        /// NV19 长期记忆、资料与恢复摘要；检索与提取默认关闭
        context: Arc<crate::voice_context::host::ContextHost>,
        /// n59 当前说话角色（全部设备共用；语音与界面切换都即时生效，不重启服务）
        persona: Arc<PersonaSwitch>,
    }

    /// 本次启动用到的本地模型。
    #[derive(Default)]
    struct LocalParts {
        asr: Option<LocalAsr>,
        /// 选用流式识别模型时：边说边识别（整段识别 `asr` 作为它失败时的退路）
        streaming_asr: Option<Arc<dyn StreamingAsr>>,
        tts: Option<LocalTts>,
        /// 主用云端 TTS 时，若本机已安装本地合成模型（Kokoro / Matcha），挂作失败时的按需自动降级兜底。
        fallback_tts: Option<Arc<dyn Tts>>,
        vad: Option<Arc<dyn VadFactory>>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct LocalFallbackCandidate {
        id: &'static str,
        dir: std::path::PathBuf,
        engine: TtsEngine,
        preferred_speaker: i32,
        default_speaker: i32,
    }

    /// 按「用户指定模型优先 → 其余已安装本地 TTS 模型顺延」顺序生成候选列表。
    fn ordered_local_tts_candidates(
        preferred_id: &str,
        preferred_speaker: i32,
        mut path_if_installed: impl FnMut(&str) -> Option<std::path::PathBuf>,
    ) -> Vec<LocalFallbackCandidate> {
        let mut candidates = Vec::new();
        let to_candidate = |spec: &'static moss_voice::local_models::ModelSpec,
                            dir: std::path::PathBuf| {
            let engine = if spec.engine == Engine::Matcha {
                TtsEngine::Matcha
            } else {
                TtsEngine::Kokoro
            };
            let default_speaker = spec.voices.map(|v| v.default).unwrap_or(0);
            LocalFallbackCandidate {
                id: spec.id,
                dir,
                engine,
                preferred_speaker,
                default_speaker,
            }
        };
        if let Some(spec) = moss_voice::local_models::spec_of(preferred_id, ModelKind::Tts)
            && let Some(dir) = path_if_installed(spec.id)
        {
            candidates.push(to_candidate(spec, dir));
        }
        for spec in moss_voice::local_models::CATALOG
            .iter()
            .filter(|spec| spec.kind == ModelKind::Tts)
        {
            if candidates.iter().any(|c| c.id == spec.id) {
                continue;
            }
            if let Some(dir) = path_if_installed(spec.id) {
                candidates.push(to_candidate(spec, dir));
            }
        }
        candidates
    }

    /// 云端 TTS 失败且当句尚未产出音频时，按需加载本机已安装的本地 TTS（优先用户选定模型，坏了自动顺延本机其他已安装模型）。
    struct LazyLocalFallbackTts {
        exe: std::path::PathBuf,
        candidates: Vec<LocalFallbackCandidate>,
        speed: f32,
        shared_helper: Option<LocalHelper>,
        loaded: Arc<Mutex<Option<(usize, LocalTts)>>>,
    }

    impl LazyLocalFallbackTts {
        async fn get_or_init_from(
            &self,
            start_index: usize,
        ) -> Result<(usize, LocalTts), moss_voice_providers::ProviderError> {
            if start_index >= self.candidates.len() {
                return Err(moss_voice_providers::ProviderError::new(
                    "没有可用的本地降级语音合成模型",
                ));
            }
            if let Ok(guard) = self.loaded.lock()
                && let Some((idx, existing)) = guard.as_ref()
                && *idx >= start_index
                && existing.helper.is_alive()
            {
                return Ok((
                    *idx,
                    LocalTts {
                        helper: existing.helper.clone(),
                        speaker: existing.speaker,
                        speed: existing.speed,
                    },
                ));
            }
            let exe = self.exe.clone();
            let candidates = self.candidates.clone();
            let speed = self.speed.clamp(0.5, 2.0);
            let shared_helper = self.shared_helper.clone();
            let loaded_slot = self.loaded.clone();

            tokio::task::spawn_blocking(move || {
                let mut guard = loaded_slot
                    .lock()
                    .map_err(|_| moss_voice_providers::ProviderError::new("本地降级合成锁失败"))?;
                if let Some((idx, existing)) = guard.as_ref()
                    && *idx >= start_index
                    && existing.helper.is_alive()
                {
                    return Ok((
                        *idx,
                        LocalTts {
                            helper: existing.helper.clone(),
                            speaker: existing.speaker,
                            speed: existing.speed,
                        },
                    ));
                }
                let reuse_helper = guard
                    .as_ref()
                    .and_then(|(_, existing)| {
                        existing.helper.is_alive().then(|| existing.helper.clone())
                    })
                    .or_else(|| shared_helper.filter(|h| h.is_alive()));
                let helper = match reuse_helper {
                    Some(h) => h,
                    None => LocalHelper::spawn(&exe)?,
                };
                let mut last_err =
                    moss_voice_providers::ProviderError::new("没有可用的本地降级语音合成模型");
                for (idx, cand) in candidates.iter().enumerate().skip(start_index) {
                    match helper.load_tts(&cand.dir, cand.engine) {
                        Ok(info) => {
                            let speaker = if info.speakers > 1 {
                                if cand.preferred_speaker >= 0
                                    && (cand.preferred_speaker as u32) < info.speakers
                                {
                                    cand.preferred_speaker
                                } else {
                                    cand.default_speaker.clamp(0, (info.speakers as i32) - 1)
                                }
                            } else {
                                0
                            };
                            let tts = LocalTts {
                                helper: helper.clone(),
                                speaker,
                                speed,
                            };
                            *guard = Some((
                                idx,
                                LocalTts {
                                    helper,
                                    speaker,
                                    speed,
                                },
                            ));
                            return Ok((idx, tts));
                        }
                        Err(err) => {
                            last_err = err;
                        }
                    }
                }
                Err(last_err)
            })
            .await
            .map_err(|_| moss_voice_providers::ProviderError::new("本地降级合成线程异常退出"))?
        }

        fn invalidate_index(&self, failed_idx: usize) {
            if let Ok(mut guard) = self.loaded.lock()
                && guard.as_ref().is_some_and(|(idx, _)| *idx == failed_idx)
            {
                *guard = None;
            }
        }
    }

    impl Tts for LazyLocalFallbackTts {
        fn synthesize<'a>(
            &'a self,
            text: &'a str,
        ) -> moss_voice_providers::BoxFuture<
            'a,
            Result<moss_voice_providers::Pcm, moss_voice_providers::ProviderError>,
        > {
            Box::pin(async move {
                let mut start_idx = 0;
                let mut last_err =
                    moss_voice_providers::ProviderError::new("没有可用的本地降级语音合成模型");
                while start_idx < self.candidates.len() {
                    let (idx, tts) = self.get_or_init_from(start_idx).await?;
                    match tts.synthesize(text).await {
                        Ok(pcm) => return Ok(pcm),
                        Err(err) => {
                            self.invalidate_index(idx);
                            last_err = err;
                            start_idx = idx + 1;
                        }
                    }
                }
                Err(last_err)
            })
        }

        fn synthesize_stream<'a>(
            &'a self,
            text: &'a str,
            chunks: tokio::sync::mpsc::UnboundedSender<moss_voice_providers::Pcm>,
        ) -> moss_voice_providers::BoxFuture<'a, Result<(), moss_voice_providers::ProviderError>>
        {
            Box::pin(async move {
                let mut start_idx = 0;
                let mut last_err =
                    moss_voice_providers::ProviderError::new("没有可用的本地降级语音合成模型");
                while start_idx < self.candidates.len() {
                    let (idx, tts) = self.get_or_init_from(start_idx).await?;
                    let (stage_tx, mut stage_rx) = tokio::sync::mpsc::unbounded_channel();
                    let res = tts.synthesize_stream(text, stage_tx).await;
                    let mut emitted = false;
                    while let Ok(pcm) = stage_rx.try_recv() {
                        emitted = true;
                        let _ = chunks.send(pcm);
                    }
                    match res {
                        Ok(()) => return Ok(()),
                        Err(err) if !emitted && idx + 1 < self.candidates.len() => {
                            self.invalidate_index(idx);
                            last_err = err;
                            start_idx = idx + 1;
                        }
                        Err(err) => return Err(err),
                    }
                }
                Err(last_err)
            })
        }
    }
    /// 辅助程序与 MOSS 放在同一目录。
    fn helper_path() -> Option<std::path::PathBuf> {
        std::env::current_exe()
            .ok()?
            .parent()
            .map(|dir| dir.join(HELPER_FILE))
    }

    /// 若 MOSS 运行在用户自定义独立目录（排除 Cargo `target/` 与 macOS `.app` 签名包目录且当前用户可写），返回该安装目录。
    fn writable_install_dir() -> Option<std::path::PathBuf> {
        if cfg!(test) {
            return None;
        }
        let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        let norm = exe_dir
            .to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase();
        if norm.ends_with("\\target\\release")
            || norm.ends_with("\\target\\debug")
            || norm.contains("\\target\\")
            || norm.contains(".app\\contents\\macos")
        {
            return None;
        }
        let models_dir = exe_dir.join("models");
        let probe = models_dir.join(".moss-write-probe");
        if std::fs::create_dir_all(&models_dir).is_ok() && std::fs::write(&probe, b"ok").is_ok() {
            let _ = std::fs::remove_file(&probe);
            return Some(exe_dir);
        }
        None
    }

    fn start_error(message: impl Into<String>) -> CenterError {
        CenterError::new("builtin_start_failed", 409, message)
    }

    fn parse_abs_env_path(raw: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
        let raw = raw?;
        if raw.is_empty() {
            return None;
        }
        let path = match raw.to_str() {
            Some(text) => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return None;
                }
                std::path::PathBuf::from(trimmed)
            }
            None => std::path::PathBuf::from(raw),
        };
        path.is_absolute().then_some(path)
    }

    fn parse_non_empty_env_path(raw: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
        let raw = raw?;
        if raw.is_empty() {
            return None;
        }
        match raw.to_str() {
            Some(text) => {
                let trimmed = text.trim();
                (!trimmed.is_empty()).then(|| std::path::PathBuf::from(trimmed))
            }
            None => Some(std::path::PathBuf::from(raw)),
        }
    }

    fn moss_laya_root_env() -> Option<std::path::PathBuf> {
        parse_abs_env_path(std::env::var_os("MOSS_LAYA_ROOT").as_deref())
    }

    fn moss_laya_python_env() -> Option<std::path::PathBuf> {
        parse_non_empty_env_path(std::env::var_os("MOSS_LAYA_PYTHON").as_deref())
    }

    /// Tauri 托管状态：后端未启动时为空。
    pub struct BuiltinState(pub Option<Arc<BuiltinVoice>>);

    /// 宿主持有的诊断记录本（内置语音初始化时登记），供更新模块、本地模型进程等写“MOSS 软件”事件。
    static HOST_LOG: std::sync::OnceLock<Arc<moss_voice_server::DiagnosticsLog>> =
        std::sync::OnceLock::new();

    /// 自动保持全屏画风：这台该不该自动推送——没装好（含包版本过旧）、没有在等下载、一天内没自动推过。
    /// `current` 为按当前画风应装的标记（[`current_screen_pack_marker`]）：画风换了也要重刷，开机待命画面才跟着换。
    fn screen_pack_auto_due(
        pref: Option<&moss_voice::settings::DeviceHardwarePrefs>,
        current: &str,
        now: u64,
    ) -> bool {
        const DAY: u64 = 24 * 60 * 60;
        let Some(pref) = pref else {
            return true;
        };
        if pref.screen_pack.as_deref() == Some(current) {
            return false;
        }
        if pref.screen_pack_pending.is_some()
            && now.saturating_sub(pref.screen_pack_pending_at.unwrap_or(0))
                <= moss_voice_server::wake_assets::STAGED_TTL.as_secs()
        {
            return false;
        }
        pref.screen_pack_auto_at
            .is_none_or(|at| now.saturating_sub(at) >= DAY)
    }

    /// 按当前选的画风应装的画风包标记（`moss-screen-1:moss_hud`）。
    fn current_screen_pack_marker(settings: &moss_voice::VoiceSettings) -> String {
        moss_voice_server::screen_pack::pack_marker(
            moss_voice_server::ScreenCardStyle::from_setting(
                &settings.builtin_local.screen_card_style,
            ),
        )
    }

    /// 写一条“MOSS 软件”事件到语音中心“记录与诊断”（2026-11-09）；后端没起来时静默丢弃。
    pub fn record_moss_event(record: EventRecord) {
        if let Some(log) = HOST_LOG.get() {
            log.push_from(moss_voice_server::DiagSource::Moss, record);
        }
    }

    pub fn init(host: Option<Arc<laser_host::HostApplication>>) -> BuiltinState {
        BuiltinState(host.map(|host| {
            let diagnostics = moss_voice_server::DiagnosticsLog::new();
            let _ = HOST_LOG.set(diagnostics.clone());
            // 2026-11-09：激光雕刻机相关事件也写进同一本记录，标为“激光雕刻”（语音中心“记录与诊断”）
            {
                let laser_log = diagnostics.clone();
                host.api_service
                    .set_event_sink(Arc::new(move |event: laser_api::LaserEvent| {
                        let level = match event.level {
                            laser_api::LaserEventLevel::Info => moss_voice_server::EventLevel::Info,
                            laser_api::LaserEventLevel::Warn => moss_voice_server::EventLevel::Warn,
                            laser_api::LaserEventLevel::Error => {
                                moss_voice_server::EventLevel::Error
                            }
                        };
                        let mut record = EventRecord::new(level, DiagCategory::Device, event.text);
                        if let Some(hint) = event.hint {
                            record = record.hint(hint);
                        }
                        if let Some(detail) = event.detail {
                            record = record.detail(detail);
                        }
                        laser_log.push_from(moss_voice_server::DiagSource::Laser, record);
                    }));
            }
            let setting_path = host.runtime_root.path.join("settings.json");
            if let Err(error) = laser_host::voice_agent::AgentCoordinator::attach(
                &host.api_service,
                host.db.clone(),
                setting_path.clone(),
                vec![host.runtime_root.path.clone()],
            ) {
                eprintln!("[moss-desktop] AgentRun 只读工具不可用：{error}");
                diagnostics.push_from(
                    moss_voice_server::DiagSource::Moss,
                    EventRecord::warn(
                        DiagCategory::Internal,
                        "本机 Agent 的只读工具没能挂上，语音里暂时用不了 Agent 查询",
                    )
                    .hint("重启 MOSS 再试；不用 Agent 功能可以忽略")
                    .detail(&error),
                );
            }
            let default_models_dir = writable_install_dir()
                .map(|dir| dir.join("models"))
                .unwrap_or_else(|| host.runtime_root.path.join("models"));
            let mut models = ModelManager::new(default_models_dir.clone());
            if !cfg!(test)
                && let Some(env_root) = moss_laya_root_env()
            {
                let e_models = env_root.join("models");
                if std::fs::create_dir_all(&e_models).is_ok() {
                    for id in moss_voice::settings::LOCAL_JEV_MODEL_IDS {
                        let installed_in_default = local_models::spec(id)
                            .is_some_and(|spec| local_models::installed(&default_models_dir, spec));
                        if !installed_in_default {
                            models = models.with_root_override(id, e_models.clone());
                        }
                    }
                }
            }
            // 2026-11-07：清掉上次中途退出（强关、崩溃、断电）留下的半截下载与解压目录
            let cleaned = models.clean_leftovers();
            if cleaned > 0 {
                eprintln!("[moss-desktop] 已清理 {cleaned} 个本地模型下载残留");
                diagnostics.push_from(
                    moss_voice_server::DiagSource::Moss,
                    EventRecord::info(
                        DiagCategory::Model,
                        format!(
                            "已清理 {cleaned} 个本地模型下载残留（上次下载中途退出留下的半截文件）"
                        ),
                    ),
                );
            }
            // 2026-11-07：macOS / Linux 上 MOSS 崩溃或被强制结束后，OneJev 推理进程会被系统收养继续占着内存；
            // 启动时只结束 MOSS 模型目录里 OneJev 权重对应、父进程已不在的那些（Windows 由 Job Object 自动处理）
            if !cfg!(test) {
                let mut roots: Vec<std::path::PathBuf> = moss_voice::settings::LOCAL_JEV_MODEL_IDS
                    .iter()
                    .map(|id| models.root_for(id).to_path_buf())
                    .collect();
                roots.sort();
                roots.dedup();
                let reaped = moss_voice::onejev::reap_orphaned_llama_servers(&roots);
                if reaped > 0 {
                    eprintln!(
                        "[moss-desktop] 已结束 {reaped} 个上次异常退出遗留的本地 OneJev 推理进程"
                    );
                    diagnostics.push_from(
                        moss_voice_server::DiagSource::Moss,
                        EventRecord::warn(
                            DiagCategory::Model,
                            format!("已结束 {reaped} 个上次异常退出遗留的本地 OneJev 推理进程"),
                        )
                        .hint(
                            "说明 MOSS 上次没有正常退出（崩溃、强制结束或断电）；偶尔一次不用处理",
                        ),
                    );
                }
            }
            let memory = MemoryService::new(Arc::new(DbMemoryStore {
                db: host.db.clone(),
            }));
            let enabled = moss_voice::settings::load(&host.runtime_root.path.join("settings.json"))
                .builtin_memory_enabled;
            let _ = memory.configure(enabled, || Ok(()));
            let home_control = laser_host::home_control::HomeControlCoordinator::attach(&host);
            let speaker_backend = Arc::new(DesktopSpeakerBackend {
                db: host.db.clone(),
                helper: Mutex::new(None),
                personal: Mutex::new(None),
                tests: Mutex::new(Default::default()),
            });
            let speaker = moss_voice_server::speaker_enrollment::SpeakerEnrollment::new(
                speaker_backend.clone(),
            );
            let saved_calibration = host
                .db
                .lock()
                .ok()
                .and_then(|db| db.get_secret(SPEAKER_CALIBRATION_KEY).ok().flatten())
                .and_then(|text| serde_json::from_str::<Calibration>(&text).ok());
            let identity = Arc::new(DesktopIdentity {
                host: Arc::downgrade(&host),
                backend: speaker_backend.clone(),
                calibration: Mutex::new(saved_calibration),
                workers: Arc::new(tokio::sync::Semaphore::new(2)),
            });
            let personal = PersonContext::new(
                identity.clone(),
                Arc::new(DbPersonStore {
                    db: host.db.clone(),
                }),
            );
            if let Ok(mut slot) = speaker_backend.personal.lock() {
                *slot = Some(Arc::downgrade(&personal));
            }
            if let Ok(db) = host.db.lock()
                && let Err(error) = db.recover_firmware_downloads(now())
            {
                eprintln!("[moss-desktop] 固件下载记录恢复失败，固件传输仍禁用");
                diagnostics.push_from(
                    moss_voice_server::DiagSource::Moss,
                    EventRecord::error(
                        DiagCategory::Firmware,
                        "固件下载记录恢复失败，这次运行不能给设备推送固件",
                    )
                    .hint("重启 MOSS 再试；一直失败请导出诊断包反馈")
                    .detail(format!("{error:?}")),
                );
            }
            let identity_for_context = identity.clone();
            let db_for_context = host.db.clone();
            let calibrated_speakers: crate::voice_context::host::CalibratedSpeakersFn =
                Arc::new(move || {
                    if identity_for_context.valid_calibration().is_none() {
                        return Vec::new();
                    }
                    db_for_context
                        .lock()
                        .ok()
                        .and_then(|db| db.list_voice_speakers().ok())
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|r| {
                            r.model_id == SPEAKER_MODEL && r.dimension == 192 && r.revision > 0
                        })
                        .map(|r| (r.id, r.name, r.revision))
                        .collect()
                });
            let context = crate::voice_context::host::ContextHost::with_speakers(
                host.db.clone(),
                setting_path.clone(),
                calibrated_speakers,
            );
            let persona = Arc::new(PersonaSwitch::new(
                persona_config(&moss_voice::settings::load(&setting_path).builtin_local),
                Some(Arc::new(DesktopPersonaStore {
                    settings_file: setting_path.clone(),
                })),
            ));
            Arc::new_cyclic(|weak| BuiltinVoice {
                self_weak: weak.clone(),
                host,
                memory,
                speaker,
                speaker_backend,
                personal,
                identity,
                home_control,
                lifecycle: Mutex::new(()),
                server: Arc::new(Mutex::new(None)),
                debug_trace_enabled: std::sync::atomic::AtomicBool::new(false),
                last_error: Mutex::new(None),
                diagnostics,
                music_warning: Mutex::new(None),
                speaker_warning: Mutex::new(None),
                models,
                laya_setup: moss_voice::laya_env::LayaSetup::new(Arc::new(
                    moss_voice::laya_env::SystemHost,
                )),
                #[cfg(test)]
                laya_root_for_test: Mutex::new(None),
                helper: Mutex::new(None),
                jev_start_lock: Arc::new(Mutex::new(())),
                local_vision: Arc::new(LocalVision::default()),
                colab_cli: Arc::new(Mutex::new(ColabCliState::default())),
                #[cfg(test)]
                colab_cli_installed_for_test: std::sync::atomic::AtomicBool::new(true),
                laya: Arc::new(Mutex::new(LocalLayaRunner {
                    active_model: None,
                    starting_model: None,
                    starting_cancel: None,
                    cancelling_model: None,
                    child: None,
                    job: None,
                    onejev: None,
                    generation: 0,
                    last_failure: None,
                })),
                context,
                persona,
            })
        }))
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
        "排障开关打开时最近 10 轮的识别原话、工具参数与回复（关闭时为空）",
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
                *text = moss_voice_providers::scrub_diagnostic_text(
                    &moss_voice::redact::redact_line(text),
                );
            }
            Value::Array(items) => items.iter_mut().for_each(scrub_bundle),
            Value::Object(map) => map.values_mut().for_each(scrub_bundle),
            _ => {}
        }
    }

    /// 本地 Jev（OneJev / Laya）启动失败 → 诊断事件。
    fn local_jev_failure(what: &str, err: &str) -> EventRecord {
        EventRecord::warn(DiagCategory::Model, what)
            .hint("Jev 只做信息筛选，失败时语音照常可用；到“服务 → 本地模型”检查模型是否下载完整、运行环境是否就绪")
            .detail(err)
    }

    /// 内置语音服务启动失败 → 诊断事件（端口占用归网络类，其余按配置类）。
    fn start_failure_event(what: &str, error: &CenterError) -> EventRecord {
        let category = if error.message.contains("端口") {
            DiagCategory::Network
        } else {
            DiagCategory::Config
        };
        EventRecord::error(category, format!("{what}：{}", error.message))
            .hint("按提示改好后，在“服务”标签点“启动”重试")
            .detail(format!("错误码 {}", error.code))
    }

    /// 语音链路为“内置服务”时，在后台启动内置语音服务；若启用了本地 Laya 加工反馈，即使当前非内置语音模式也在后台拉起本地 Laya。
    pub fn autostart(state: &BuiltinState) {
        let Some(builtin) = state.0.clone() else {
            return;
        };
        // 2026-11-09 自动保持全屏画风：开关默认关，开着时每 20 秒看一眼在线小智（只持弱引用，随 MOSS 退出）
        let weak = Arc::downgrade(&builtin);
        let _ = std::thread::Builder::new()
            .name("voice-screen-pack-keeper".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(20));
                    let Some(builtin) = weak.upgrade() else {
                        break;
                    };
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    builtin.screen_pack_auto_tick(now);
                }
            });
        let start_voice = builtin.settings().mode == VoiceMode::Builtin;
        let start_feedback_laya = builtin.should_run_local_laya_for_feedback();
        if !start_voice && !start_feedback_laya {
            return;
        }
        let _ = std::thread::Builder::new()
            .name("voice-builtin-autostart".into())
            .spawn(move || {
                if start_feedback_laya && let Err(err) = builtin.ensure_laya_running(false) {
                    eprintln!("[moss-desktop] 本地 Laya 决策服务自动启动失败：{err}");
                    builtin.diagnostics.push_from(
                        moss_voice_server::DiagSource::Moss,
                        local_jev_failure("MOSS 启动时自动拉起本地 Jev 决策服务失败", &err),
                    );
                }
                if start_voice && let Err(error) = builtin.start() {
                    eprintln!("[moss-desktop] 内置语音服务自动启动失败：{}", error.message);
                    if let Ok(mut last) = builtin.last_error.lock() {
                        *last = Some(error.message);
                    }
                }
            });
    }

    /// 本机真实局域网地址（跳过 Clash TUN 198.18.0.0/15、Tailscale 与 VMware 等虚拟网卡）。
    fn local_ip() -> Option<String> {
        moss_voice_server::primary_lan_ip().map(|ip| ip.to_string())
    }

    impl BuiltinVoice {
        fn prepare_speaker_helper(&self) -> Result<(), CenterError> {
            let mut helper = self
                .speaker_backend
                .helper
                .lock()
                .map_err(|_| internal("声纹辅助程序锁失败"))?;
            if helper.as_ref().is_none_or(|h| !h.is_alive()) {
                let exe = helper_path().ok_or_else(|| {
                    CenterError::new("speaker_helper_missing", 409, "请先准备声纹辅助程序")
                })?;
                *helper = Some(
                    moss_voice_providers::local::SpeakerHelper::spawn(&exe, self.models.root())
                        .map_err(|_| {
                            CenterError::new(
                                "speaker_model_missing",
                                409,
                                "声纹模型未安装或校验失败",
                            )
                        })?,
                );
            }
            Ok(())
        }
        fn hotwords(&self, local: &BuiltinLocal) -> Vec<String> {
            let materials: Vec<String> = self
                .host
                .db
                .lock()
                .ok()
                .and_then(|db| db.export_material_library().ok())
                .map(|library| library.materials.into_keys().collect())
                .unwrap_or_default();
            moss_voice::settings::effective_hotwords(&local.custom_hotwords, &materials)
        }
        fn settings_file(&self) -> std::path::PathBuf {
            self.host.runtime_root.path.join("settings.json")
        }

        fn settings(&self) -> moss_voice::VoiceSettings {
            moss_voice::settings::load(&self.settings_file())
        }

        fn secret(&self, section: Section) -> Result<Option<String>, CenterError> {
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            db.get_secret(builtin_secret_name(section))
                .map_err(db_failure)
        }

        fn colab_secret(&self) -> Result<Option<String>, CenterError> {
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            Ok(db
                .get_secret(moss_voice::colab::COLAB_SECRET_NAME)
                .map_err(db_failure)?
                .filter(|s| !s.trim().is_empty()))
        }

        /// n80：Colab T4 专属联动已开启、地址合法且密钥已保存时返回规范化后的配置与密钥；未开启时零开销返回 `None`。
        fn active_colab(
            &self,
            settings: &moss_voice::VoiceSettings,
        ) -> Result<Option<(moss_voice::colab::BuiltinColab, String)>, CenterError> {
            if !settings.builtin_colab.enabled {
                return Ok(None);
            }
            let Some(key) = self.colab_secret()? else {
                return Ok(None);
            };
            if !settings.builtin_colab.active(true) {
                return Ok(None);
            }
            let Ok(url) = moss_voice::colab::normalize_colab_url(&settings.builtin_colab.url)
            else {
                return Ok(None);
            };
            let mut normalized = settings.builtin_colab.clone();
            normalized.url = url;
            Ok(Some((normalized, key)))
        }

        fn find_colab_cli_binary(&self) -> Option<std::path::PathBuf> {
            #[cfg(test)]
            {
                if self
                    .colab_cli_installed_for_test
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    Some(std::path::PathBuf::from("colab"))
                } else {
                    None
                }
            }
            #[cfg(not(test))]
            {
                let bin_name = if cfg!(windows) { "colab.exe" } else { "colab" };
                if let Some(path_os) = std::env::var_os("PATH") {
                    for dir in std::env::split_paths(&path_os) {
                        let cand = dir.join(bin_name);
                        if cand.is_file() {
                            return Some(cand);
                        }
                    }
                }
                let home_opt = std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .map(std::path::PathBuf::from);
                if let Some(ref home) = home_opt {
                    let local_bin = home.join(".local").join("bin").join(bin_name);
                    if local_bin.is_file() {
                        return Some(local_bin);
                    }
                    #[cfg(target_os = "macos")]
                    if let Ok(entries) = std::fs::read_dir(home.join("Library").join("Python")) {
                        for entry in entries.flatten() {
                            let cand = entry.path().join("bin").join(bin_name);
                            if cand.is_file() {
                                return Some(cand);
                            }
                        }
                    }
                }
                if let Some(data) = std::env::var_os("APPDATA").map(std::path::PathBuf::from) {
                    let uv_bin = if cfg!(windows) {
                        data.join("uv")
                            .join("tools")
                            .join("google-colab-cli")
                            .join("Scripts")
                            .join("colab.exe")
                    } else {
                        data.join("uv")
                            .join("tools")
                            .join("google-colab-cli")
                            .join("bin")
                            .join("colab")
                    };
                    if uv_bin.is_file() {
                        return Some(uv_bin);
                    }
                    #[cfg(windows)]
                    if let Ok(entries) = std::fs::read_dir(data.join("Python")) {
                        for entry in entries.flatten() {
                            let cand = entry.path().join("Scripts").join("colab.exe");
                            if cand.is_file() {
                                return Some(cand);
                            }
                        }
                    }
                }
                None
            }
        }

        fn colab_ref_audio_path(&self) -> std::path::PathBuf {
            self.host
                .runtime_root
                .path
                .join("colab-cli")
                .join("moss_ref.wav")
        }

        fn colab_ref_audio_meta_path(&self) -> std::path::PathBuf {
            self.host
                .runtime_root
                .path
                .join("colab-cli")
                .join("moss_ref.meta")
        }

        fn colab_cli_token_path(&self) -> Option<std::path::PathBuf> {
            #[cfg(test)]
            {
                Some(
                    self.host
                        .runtime_root
                        .path
                        .join("colab-cli")
                        .join("token.json"),
                )
            }
            #[cfg(not(test))]
            {
                std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .map(std::path::PathBuf::from)
                    .map(|h| h.join(".config").join("colab-cli").join("token.json"))
            }
        }

        fn colab_token_file_valid(path: &std::path::Path) -> bool {
            let Ok(raw) = std::fs::read_to_string(path) else {
                return false;
            };
            let Ok(val) = serde_json::from_str::<Value>(&raw) else {
                return false;
            };
            val.get("refresh_token")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty())
                || val
                    .get("token")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty())
        }

        fn colab_cli_logged_in(&self) -> bool {
            self.find_colab_cli_binary().is_some()
                && self
                    .colab_cli_token_path()
                    .as_deref()
                    .is_some_and(Self::colab_token_file_valid)
        }

        #[cfg(not(test))]
        fn colab_cli_command(bin: &std::path::Path) -> std::process::Command {
            let mut cmd = std::process::Command::new(bin);
            cmd.env("PYTHONUTF8", "1")
                .env("PYTHONIOENCODING", "utf-8")
                .env("COLAB_CLI_REQUEST_TIMEOUT", "25")
                .env("COLAB_CLI_RETRY_BACKOFF", "2")
                .env("COLAB_CLI_ASSIGN_RETRIES", "5")
                .arg("--auth=oauth2");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000);
            }
            cmd
        }

        #[cfg(not(test))]
        fn kill_colab_pid(pid: u32) {
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .creation_flags(0x0800_0000)
                    .status();
            }
            #[cfg(unix)]
            {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }

        #[cfg(not(test))]
        fn open_google_oauth_url(url: &str) {
            if !url.starts_with("https://accounts.google.com/") {
                return;
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("rundll32")
                    .args(["url.dll,FileProtocolHandler", url])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .creation_flags(0x0800_0000)
                    .spawn();
            }
            #[cfg(target_os = "macos")]
            {
                let _ = std::process::Command::new("open")
                    .arg(url)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                let _ = std::process::Command::new("xdg-open")
                    .arg(url)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
        }

        fn colab_cli_status(&self) -> Value {
            let installed = self.find_colab_cli_binary().is_some();
            let logged_in = self.colab_cli_logged_in();
            let mut guard = self
                .colab_cli
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !logged_in {
                guard.account_hint.clear();
                guard.expires_in.clear();
                guard.whoami_probed = false;
                guard.whoami_probing = false;
            } else if !guard.whoami_probed && !guard.whoami_probing {
                #[cfg(test)]
                {
                    guard.account_hint = "t***r@gmail.com".into();
                    guard.expires_in = "59m".into();
                    guard.whoami_probed = true;
                }
                #[cfg(not(test))]
                if let Some(bin) = self.find_colab_cli_binary() {
                    guard.whoami_probing = true;
                    let slot = self.colab_cli.clone();
                    std::thread::spawn(move || {
                        let out = Self::colab_cli_command(&bin)
                            .arg("whoami")
                            .stdin(std::process::Stdio::null())
                            .output();
                        if let Ok(mut st) = slot.lock() {
                            st.whoami_probing = false;
                            st.whoami_probed = true;
                            if let Ok(out) = out
                                && out.status.success()
                            {
                                let text = String::from_utf8_lossy(&out.stdout);
                                if let Some((masked, exp)) =
                                    moss_voice::colab::parse_whoami_output(&text)
                                {
                                    st.account_hint = masked;
                                    st.expires_in = exp;
                                }
                            }
                        }
                    });
                }
            }
            let runner_state = if guard.runner_state.is_empty() {
                "idle"
            } else {
                guard.runner_state.as_str()
            };
            let account_hint = if logged_in {
                if guard.account_hint.is_empty() {
                    "已授权账号".to_owned()
                } else {
                    guard.account_hint.clone()
                }
            } else {
                String::new()
            };
            let ref_audio_path = self.colab_ref_audio_path();
            let ref_audio_configured = ref_audio_path.is_file();
            let ref_audio_name = if ref_audio_configured {
                std::fs::read_to_string(self.colab_ref_audio_meta_path())
                    .ok()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .or_else(|| Some("moss_ref.wav".to_owned()))
            } else {
                None
            };
            serde_json::json!({
                "installed": installed,
                "logged_in": logged_in,
                "account_hint": account_hint,
                "expires_in": guard.expires_in,
                "auth_url": guard.auth_url,
                "runner_state": runner_state,
                "runner_step": guard.runner_step,
                "runner_error": guard.runner_error,
                "ref_audio_configured": ref_audio_configured,
                "ref_audio_name": ref_audio_name,
            })
        }

        fn handle_colab_cli_action(&self, input: &Value) -> Result<(), CenterError> {
            let invalid = |message: &str| CenterError::new("voice_settings_invalid", 400, message);
            let action = input
                .get("action")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            match action {
                "refresh" => {
                    #[cfg(not(test))]
                    if let Some(bin) = self.find_colab_cli_binary()
                        && self.colab_cli_logged_in()
                        && let Ok(out) = Self::colab_cli_command(&bin)
                            .arg("whoami")
                            .stdin(std::process::Stdio::null())
                            .output()
                        && let Ok(mut st) = self.colab_cli.lock()
                    {
                        st.whoami_probed = true;
                        st.whoami_probing = false;
                        if out.status.success() {
                            let text = String::from_utf8_lossy(&out.stdout);
                            if let Some((masked, exp)) =
                                moss_voice::colab::parse_whoami_output(&text)
                            {
                                st.account_hint = masked;
                                st.expires_in = exp;
                            }
                        }
                    }
                    Ok(())
                }
                "install" => {
                    if self.find_colab_cli_binary().is_some() {
                        return Ok(());
                    }
                    #[cfg(test)]
                    {
                        self.colab_cli_installed_for_test
                            .store(true, std::sync::atomic::Ordering::Release);
                        if let Ok(mut st) = self.colab_cli.lock() {
                            st.runner_error = None;
                            st.runner_step =
                                "已安装 Colab CLI，请点击「登录 Colab CLI」完成 Google 授权".into();
                        }
                        Ok(())
                    }
                    #[cfg(not(test))]
                    {
                        let mut installed_ok = false;
                        let uv_name = if cfg!(windows) { "uv.exe" } else { "uv" };
                        let mut uv_bin: Option<std::path::PathBuf> = None;
                        if let Some(path_os) = std::env::var_os("PATH") {
                            for dir in std::env::split_paths(&path_os) {
                                let cand = dir.join(uv_name);
                                if cand.is_file() {
                                    uv_bin = Some(cand);
                                    break;
                                }
                            }
                        }
                        if uv_bin.is_none()
                            && let Some(home) = std::env::var_os("USERPROFILE")
                                .or_else(|| std::env::var_os("HOME"))
                                .map(std::path::PathBuf::from)
                        {
                            let cand = home.join(".local").join("bin").join(uv_name);
                            if cand.is_file() {
                                uv_bin = Some(cand);
                            }
                        }
                        if let Some(uv) = uv_bin {
                            let mut cmd = std::process::Command::new(uv);
                            cmd.args(["tool", "install", "google-colab-cli"])
                                .stdin(std::process::Stdio::null());
                            #[cfg(windows)]
                            {
                                use std::os::windows::process::CommandExt;
                                cmd.creation_flags(0x0800_0000);
                            }
                            if let Ok(out) = cmd.output()
                                && out.status.success()
                            {
                                installed_ok = self.find_colab_cli_binary().is_some();
                            }
                        }
                        if !installed_ok {
                            let py = self.laya_setup.find_python().map_err(|msg| {
                                CenterError::new(
                                    "colab_cli_install_failed",
                                    412,
                                    format!("自动安装 Colab CLI 需要本机有 uv 或 Python 3.10+：{msg}（你也可以点右上角导出 .ipynb 笔记本去浏览器手动运行）"),
                                )
                            })?;
                            let mut cmd = std::process::Command::new(&py.executable);
                            cmd.args(["-m", "pip", "install", "--user", "-U", "google-colab-cli"])
                                .stdin(std::process::Stdio::null());
                            #[cfg(windows)]
                            {
                                use std::os::windows::process::CommandExt;
                                cmd.creation_flags(0x0800_0000);
                            }
                            let out = cmd.output().map_err(|e| {
                                internal(format!("调用 pip 安装 google-colab-cli 失败：{e}"))
                            })?;
                            if !out.status.success() || self.find_colab_cli_binary().is_none() {
                                let err_tail = String::from_utf8_lossy(&out.stderr);
                                return Err(CenterError::new(
                                    "colab_cli_install_failed",
                                    500,
                                    format!(
                                        "自动安装 google-colab-cli 未成功：{}（也可在终端手动运行 uv tool install google-colab-cli）",
                                        err_tail.lines().last().unwrap_or("安装失败")
                                    ),
                                ));
                            }
                        }
                        if let Ok(mut st) = self.colab_cli.lock() {
                            st.runner_error = None;
                            st.runner_step =
                                "已安装 Colab CLI，请点击「登录 Colab CLI」完成 Google 授权".into();
                        }
                        Ok(())
                    }
                }
                "set_ref_audio" => {
                    use base64::Engine as _;
                    let raw_b64 = input
                        .get("wav_base64")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .unwrap_or("");
                    if raw_b64.is_empty() {
                        return Err(invalid("请选择有效的 WAV 参考音频文件"));
                    }
                    let clean_b64 = raw_b64
                        .split_once(";base64,")
                        .map(|(_, b)| b)
                        .unwrap_or(raw_b64)
                        .trim();
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(clean_b64)
                        .map_err(|_| invalid("参考音频 Base64 解码失败"))?;
                    moss_voice::colab::validate_ref_wav(&bytes).map_err(|msg| invalid(&msg))?;
                    let raw_name = input
                        .get("file_name")
                        .and_then(Value::as_str)
                        .unwrap_or("moss_ref.wav");
                    let safe_name: String = std::path::Path::new(raw_name)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("moss_ref.wav")
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(80)
                        .collect();
                    let wav_path = self.colab_ref_audio_path();
                    if let Some(parent) = wav_path.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| internal(e.to_string()))?;
                    }
                    std::fs::write(&wav_path, &bytes)
                        .map_err(|e| internal(format!("保存参考音频失败：{e}")))?;
                    let _ = std::fs::write(
                        self.colab_ref_audio_meta_path(),
                        if safe_name.is_empty() {
                            "moss_ref.wav"
                        } else {
                            &safe_name
                        },
                    );
                    #[cfg(not(test))]
                    if let Some(bin) = self.find_colab_cli_binary()
                        && self.colab_cli_logged_in()
                    {
                        let slot = self.colab_cli.clone();
                        std::thread::spawn(move || {
                            let out = Self::colab_cli_command(&bin)
                                .args([
                                    "upload",
                                    "-s",
                                    moss_voice::colab::COLAB_CLI_SESSION_NAME,
                                    &wav_path.to_string_lossy(),
                                    "/content/moss_ref.wav",
                                ])
                                .stdin(std::process::Stdio::null())
                                .output();
                            if let Ok(out) = out
                                && out.status.success()
                                && let Ok(mut st) = slot.lock()
                            {
                                st.runner_step =
                                    "已将 550W 参考音频同步到云端 Colab T4（下一句合成即刻生效）"
                                        .into();
                            }
                        });
                    }
                    Ok(())
                }
                "clear_ref_audio" => {
                    let _ = std::fs::remove_file(self.colab_ref_audio_path());
                    let _ = std::fs::remove_file(self.colab_ref_audio_meta_path());
                    Ok(())
                }
                "login_start" => {
                    let Some(_bin) = self.find_colab_cli_binary() else {
                        return Err(CenterError::new(
                            "colab_cli_missing",
                            412,
                            "本机尚未安装 Colab CLI，请先在终端执行：uv tool install google-colab-cli",
                        ));
                    };
                    if let Some(token_path) = self.colab_cli_token_path()
                        && !Self::colab_token_file_valid(&token_path)
                    {
                        let _ = std::fs::remove_file(&token_path);
                    }
                    #[cfg(test)]
                    let url = "https://accounts.google.com/o/oauth2/auth?response_type=code&client_id=mock".to_owned();
                    #[cfg(not(test))]
                    let url = {
                        if let Some(token_path) = self.colab_cli_token_path() {
                            let _ = std::fs::remove_file(&token_path);
                        }
                        let out = Self::colab_cli_command(&_bin)
                            .arg("whoami")
                            .stdin(std::process::Stdio::null())
                            .output()
                            .map_err(|e| internal(format!("启动 Colab CLI 授权失败：{e}")))?;
                        let combined = format!(
                            "{}\n{}",
                            String::from_utf8_lossy(&out.stdout),
                            String::from_utf8_lossy(&out.stderr)
                        );
                        let extracted = moss_voice::colab::extract_oauth_url(&combined).ok_or_else(|| {
                            internal("未能从 Colab CLI 获取 Google 授权链接，请确认 google-colab-cli 版本最新")
                        })?;
                        Self::open_google_oauth_url(&extracted);
                        extracted
                    };
                    if let Ok(mut st) = self.colab_cli.lock() {
                        st.auth_url = Some(url);
                        st.runner_error = None;
                        st.runner_step =
                            "已在浏览器打开 Google 授权页，请复制授权码粘贴到下方完成登录".into();
                    }
                    Ok(())
                }
                "login_complete" => {
                    let Some(_bin) = self.find_colab_cli_binary() else {
                        return Err(CenterError::new(
                            "colab_cli_missing",
                            412,
                            "本机尚未安装 Colab CLI，请先在终端执行：uv tool install google-colab-cli",
                        ));
                    };
                    let code = input
                        .get("code")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .unwrap_or("");
                    if code.is_empty()
                        || code.len() > 512
                        || code.chars().any(|c| c.is_whitespace() || c.is_control())
                    {
                        return Err(invalid("请粘贴有效的 Google 授权码（不能含空格）"));
                    }
                    #[cfg(test)]
                    {
                        if let Some(path) = self.colab_cli_token_path() {
                            if let Some(parent) = path.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            std::fs::write(
                                &path,
                                r#"{"token":"mock-access","refresh_token":"mock-refresh"}"#,
                            )
                            .map_err(|e| internal(e.to_string()))?;
                        }
                        if let Ok(mut st) = self.colab_cli.lock() {
                            st.auth_url = None;
                            st.account_hint = "t***r@gmail.com".into();
                            st.expires_in = "59m".into();
                            st.whoami_probed = true;
                            st.runner_error = None;
                            st.runner_step = "Colab CLI 登录成功".into();
                        }
                    }
                    #[cfg(not(test))]
                    {
                        use std::io::Write;
                        let mut child = Self::colab_cli_command(&_bin)
                            .arg("whoami")
                            .stdin(std::process::Stdio::piped())
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .spawn()
                            .map_err(|e| internal(format!("执行 Colab CLI 登录失败：{e}")))?;
                        if let Some(mut stdin) = child.stdin.take() {
                            let _ = stdin.write_all(format!("{code}\n").as_bytes());
                        }
                        let out = child
                            .wait_with_output()
                            .map_err(|e| internal(format!("等待 Colab CLI 登录结果失败：{e}")))?;
                        if !self.colab_cli_logged_in() {
                            let err_txt = String::from_utf8_lossy(&out.stderr);
                            return Err(invalid(&format!(
                                "Google 授权码验证未通过，请重新获取授权码再试（{}）",
                                err_txt.lines().last().unwrap_or("验证失败")
                            )));
                        }
                        let stdout_txt = String::from_utf8_lossy(&out.stdout);
                        if let Ok(mut st) = self.colab_cli.lock() {
                            st.auth_url = None;
                            st.whoami_probed = true;
                            st.runner_error = None;
                            st.runner_step = "Colab CLI 登录成功".into();
                            if let Some((masked, exp)) =
                                moss_voice::colab::parse_whoami_output(&stdout_txt)
                            {
                                st.account_hint = masked;
                                st.expires_in = exp;
                            }
                        }
                    }
                    if self.settings().builtin_colab.enabled {
                        self.spawn_colab_cli_runner();
                    }
                    Ok(())
                }
                "logout" => {
                    if let Some(path) = self.colab_cli_token_path() {
                        let _ = std::fs::remove_file(path);
                    }
                    if let Ok(mut st) = self.colab_cli.lock() {
                        st.generation = st.generation.wrapping_add(1);
                        st.account_hint.clear();
                        st.expires_in.clear();
                        st.auth_url = None;
                        st.whoami_probed = false;
                        st.whoami_probing = false;
                        st.runner_state = "idle".into();
                        st.runner_step = "已退出 Colab CLI 登录".into();
                        st.runner_error = None;
                    }
                    Ok(())
                }
                "start_session" => {
                    if !self.colab_cli_logged_in() {
                        return Err(invalid(
                            "尚未登录 Colab CLI：请先点击「登录 Colab CLI」完成授权",
                        ));
                    }
                    let _ = moss_voice::settings::update_voice(&self.settings_file(), |s| {
                        s.builtin_colab.enabled = true;
                        Ok(())
                    });
                    self.spawn_colab_cli_runner();
                    Ok(())
                }
                "stop_session" => {
                    if let Ok(mut st) = self.colab_cli.lock() {
                        st.generation = st.generation.wrapping_add(1);
                        st.runner_state = "idle".into();
                        st.runner_step = "正在释放云端 Colab T4 会话...".into();
                        st.runner_error = None;
                    }
                    #[cfg(not(test))]
                    if let Some(bin) = self.find_colab_cli_binary() {
                        let slot = self.colab_cli.clone();
                        std::thread::spawn(move || {
                            let _ = Self::colab_cli_command(&bin)
                                .args(["stop", "-s", moss_voice::colab::COLAB_CLI_SESSION_NAME])
                                .stdin(std::process::Stdio::null())
                                .output();
                            if let Ok(mut st) = slot.lock() {
                                st.runner_step = "云端 Colab T4 会话已释放".into();
                            }
                        });
                    }
                    #[cfg(test)]
                    if let Ok(mut st) = self.colab_cli.lock() {
                        st.runner_step = "云端 Colab T4 会话已释放".into();
                    }
                    Ok(())
                }
                _ => Err(invalid("未知的 colab_cli 操作")),
            }
        }

        fn spawn_colab_cli_runner(&self) {
            if !self.colab_cli_logged_in() {
                return;
            }
            #[cfg_attr(test, allow(unused_variables))]
            let generation = {
                let Ok(mut st) = self.colab_cli.lock() else {
                    return;
                };
                if st.runner_state == "starting" {
                    return;
                }
                st.generation = st.generation.wrapping_add(1);
                st.runner_state = "starting".into();
                st.runner_step = "正在通过 Colab CLI 自动拉起云端 T4...".into();
                st.runner_error = None;
                st.generation
            };
            #[cfg(not(test))]
            {
                let Some(bin) = self.find_colab_cli_binary() else {
                    return;
                };
                let Some(voice) = self.self_weak.upgrade() else {
                    return;
                };
                std::thread::Builder::new()
                    .name("moss-colab-cli-runner".into())
                    .spawn(move || {
                        let is_current = |slot: &Mutex<ColabCliState>| -> bool {
                            slot.lock().is_ok_and(|st| st.generation == generation)
                        };
                        let set_step = |slot: &Mutex<ColabCliState>, step: &str| {
                            if let Ok(mut st) = slot.lock()
                                && st.generation == generation
                            {
                                st.runner_step = step.to_owned();
                            }
                        };
                        let fail = |slot: &Mutex<ColabCliState>, msg: String| {
                            if let Ok(mut st) = slot.lock()
                                && st.generation == generation
                            {
                                st.runner_state = "error".into();
                                st.runner_step = "云端 Colab T4 自动拉起失败".into();
                                st.runner_error = Some(msg.clone());
                            }
                            voice.diagnostics.push_from(
                                moss_voice_server::DiagSource::Moss,
                                EventRecord::warn(
                                    DiagCategory::Model,
                                    format!("Colab CLI 自动拉起云端 T4 失败：{msg}"),
                                )
                                .hint("可在“本地模型 → Colab T4 专属联动”查看进度或点“一键拉起云端 T4”重试"),
                            );
                        };

                        // 1. 若已有保存的隧道地址且 /healthz 存活，直接复用
                        let current_settings = voice.settings();
                        if let Ok(Some((active_cfg, _))) = voice.active_colab(&current_settings) {
                            let health_url = format!(
                                "{}/healthz",
                                active_cfg.url.trim_end_matches('/').trim_end_matches("/v1")
                            );
                            let alive = reqwest::blocking::Client::builder()
                                .timeout(std::time::Duration::from_secs(3))
                                .build()
                                .ok()
                                .and_then(|c| c.get(&health_url).send().ok())
                                .is_some_and(|r| r.status().is_success());
                            if alive {
                                if let Ok(mut st) = voice.colab_cli.lock()
                                    && st.generation == generation
                                {
                                    st.runner_state = "running".into();
                                    st.runner_step =
                                        "云端 Colab T4 隧道在线并已接管（Qwen3-VL-8B + 550W 克隆）"
                                            .into();
                                    st.runner_error = None;
                                }
                                return;
                            }
                        }

                        if !is_current(&voice.colab_cli) {
                            return;
                        }
                        let session_name = moss_voice::colab::COLAB_CLI_SESSION_NAME;
                        set_step(
                            &voice.colab_cli,
                            &format!("正在检查 Colab 云端会话 ({session_name})..."),
                        );
                        let status_out = Self::colab_cli_command(&bin)
                            .args(["status", "-s", session_name])
                            .stdin(std::process::Stdio::null())
                            .output();
                        let session_exists = match status_out {
                            Ok(out) => {
                                let combined = format!(
                                    "{}\n{}",
                                    String::from_utf8_lossy(&out.stdout),
                                    String::from_utf8_lossy(&out.stderr)
                                );
                                out.status.success()
                                    && !combined.contains("not found")
                                    && !combined.contains("No active sessions")
                            }
                            Err(_) => false,
                        };

                        if !session_exists {
                            if !is_current(&voice.colab_cli) {
                                return;
                            }
                            set_step(
                                &voice.colab_cli,
                                "正在向 Google 申请免费 T4 显卡 (colab new -s moss-t4 --gpu T4)...",
                            );
                            let new_out = Self::colab_cli_command(&bin)
                                .args(["new", "-s", session_name, "--gpu", "T4"])
                                .stdin(std::process::Stdio::null())
                                .output();
                            match new_out {
                                Ok(out) if out.status.success() => {}
                                Ok(out) => {
                                    let detail = format!(
                                        "{} {}",
                                        String::from_utf8_lossy(&out.stderr).trim(),
                                        String::from_utf8_lossy(&out.stdout).trim()
                                    );
                                    fail(
                                        &voice.colab_cli,
                                        format!("分配 T4 显卡未通过：{}", detail.trim()),
                                    );
                                    return;
                                }
                                Err(e) => {
                                    fail(&voice.colab_cli, format!("调用 colab new 失败：{e}"));
                                    return;
                                }
                            }
                        }

                        if !is_current(&voice.colab_cli) {
                            return;
                        }
                        let ref_audio = voice.colab_ref_audio_path();
                        if ref_audio.is_file() {
                            set_step(
                                &voice.colab_cli,
                                "正在将本机保存的 550W 参考音频同步到云端 T4...",
                            );
                            let _ = Self::colab_cli_command(&bin)
                                .args([
                                    "upload",
                                    "-s",
                                    session_name,
                                    &ref_audio.to_string_lossy(),
                                    "/content/moss_ref.wav",
                                ])
                                .stdin(std::process::Stdio::null())
                                .output();
                        }

                        if !is_current(&voice.colab_cli) {
                            return;
                        }
                        let bootstrap_path = voice
                            .host
                            .runtime_root
                            .path
                            .join("moss_colab_bootstrap.py");
                        if let Err(e) = std::fs::write(
                            &bootstrap_path,
                            moss_voice::colab::cli_bootstrap_script(),
                        ) {
                            fail(&voice.colab_cli, format!("写入启动脚本失败：{e}"));
                            return;
                        }

                        set_step(
                            &voice.colab_cli,
                            "正在云端 T4 极速并发部署 Qwen3-VL-8B 与 550W 克隆服务（约 1.5~2 分钟）...",
                        );

                        let mut creds_found: Option<(String, String)> = None;
                        let mut last_combined = String::new();
                        let mut last_exit_note = String::new();

                        for attempt in 0..4 {
                            if !is_current(&voice.colab_cli) {
                                return;
                            }
                            if attempt > 0 {
                                std::thread::sleep(std::time::Duration::from_secs(2));
                            }
                            let mut child = match Self::colab_cli_command(&bin)
                                .args([
                                    "exec",
                                    "-s",
                                    session_name,
                                    "--timeout",
                                    "1200",
                                    "-f",
                                    &bootstrap_path.to_string_lossy(),
                                ])
                                .stdin(std::process::Stdio::null())
                                .stdout(std::process::Stdio::piped())
                                .stderr(std::process::Stdio::piped())
                                .spawn()
                            {
                                Ok(c) => c,
                                Err(e) => {
                                    fail(&voice.colab_cli, format!("调用 colab exec 失败：{e}"));
                                    return;
                                }
                            };
                            let child_pid = child.id();

                            let stderr_handle = child.stderr.take().map(|err_pipe| {
                                std::thread::spawn(move || {
                                    use std::io::BufRead;
                                    let mut buf = String::new();
                                    let r = std::io::BufReader::new(err_pipe);
                                    for line in r.lines().map_while(Result::ok) {
                                        buf.push_str(&line);
                                        buf.push('\n');
                                        if line.contains("RuntimeError: Connection was lost.")
                                            || line.contains("TimeoutError:")
                                        {
                                            Self::kill_colab_pid(child_pid);
                                            break;
                                        }
                                    }
                                    buf
                                })
                            });

                            let mut collected_stdout = String::new();
                            if let Some(out_pipe) = child.stdout.take() {
                                use std::io::BufRead;
                                let reader = std::io::BufReader::new(out_pipe);
                                for line in reader.lines().map_while(Result::ok) {
                                    if !is_current(&voice.colab_cli) {
                                        Self::kill_colab_pid(child_pid);
                                        return;
                                    }
                                    if let Some(rest) = line.trim().strip_prefix("[MOSS-COLAB]") {
                                        let clean = rest.trim();
                                        if !clean.is_empty() {
                                            set_step(&voice.colab_cli, clean);
                                        }
                                    }
                                    collected_stdout.push_str(&line);
                                    collected_stdout.push('\n');
                                    if moss_voice::colab::extract_tunnel_credentials(&collected_stdout)
                                        .is_some()
                                    {
                                        Self::kill_colab_pid(child_pid);
                                        break;
                                    }
                                }
                            }

                            let status = child.wait();
                            Self::kill_colab_pid(child_pid);
                            let stderr_txt = stderr_handle
                                .and_then(|h| h.join().ok())
                                .unwrap_or_default();
                            last_combined = format!("{collected_stdout}\n{stderr_txt}");
                            last_exit_note = match status {
                                Ok(s) => format!("exit {}", s.code().unwrap_or(-1)),
                                Err(e) => e.to_string(),
                            };

                            if let Some(pair) =
                                moss_voice::colab::extract_tunnel_credentials(&last_combined)
                            {
                                creds_found = Some(pair);
                                break;
                            }
                            if !last_combined.contains("Connection was lost")
                                && !last_combined.contains("TimeoutError")
                            {
                                break;
                            }
                        }

                        if let Some((url, token)) = creds_found {
                            if let Ok(db) = voice.host.db.lock() {
                                let ops = vec![(
                                    moss_voice::colab::COLAB_SECRET_NAME.to_owned(),
                                    Some(token),
                                )];
                                let _ = commit_secrets_then_settings(&db, &ops, || {
                                    moss_voice::settings::update_voice(
                                        &voice.settings_file(),
                                        |s| {
                                            s.builtin_colab.url = url.clone();
                                            Ok(())
                                        },
                                    )
                                    .map_err(|msg| {
                                        CenterError::new("voice_settings_write_failed", 500, msg)
                                    })
                                });
                            }
                            voice.sync_local_vision(true);
                            let _ = voice.restart_if_running();
                            if let Ok(mut st) = voice.colab_cli.lock()
                                && st.generation == generation
                            {
                                st.runner_state = "running".into();
                                st.runner_step =
                                    "云端 Colab T4 已就绪并自动接管（Qwen3-VL-8B + 550W 克隆）"
                                        .into();
                                st.runner_error = None;
                            }
                        } else {
                            let last_line = last_combined
                                .lines()
                                .rev()
                                .find(|l| !l.trim().is_empty())
                                .unwrap_or("未返回隧道地址");
                            fail(
                                &voice.colab_cli,
                                format!("云端脚本未返回有效隧道（{last_exit_note}）：{last_line}"),
                            );
                        }
                    })
                    .ok();
            }
        }

        fn devices(&self) -> Value {
            self.host
                .db
                .lock()
                .ok()
                .and_then(|db| db.voice_devices().ok())
                .map(|devices| serde_json::to_value(devices).unwrap_or_default())
                .unwrap_or_else(|| Value::Array(Vec::new()))
        }

        /// 查找本机已安装的本地 TTS 模型作为主用云端 TTS 失败时的自动降级兜底（优先使用用户选中的 `tts_model`，坏了自动顺延其他已安装模型）。
        fn local_fallback_tts(
            &self,
            local: &BuiltinLocal,
            shared_helper: Option<LocalHelper>,
            colab_tts_override: bool,
        ) -> Option<Arc<dyn Tts>> {
            if local.tts && !colab_tts_override {
                return None;
            }
            let exe = helper_path().filter(|path| path.is_file())?;
            let candidates =
                ordered_local_tts_candidates(&local.tts_model, local.tts_speaker, |id| {
                    self.models.path_if_installed(id)
                });
            if candidates.is_empty() {
                return None;
            }
            Some(Arc::new(LazyLocalFallbackTts {
                exe,
                candidates,
                speed: local.tts_speed,
                shared_helper,
                loaded: Arc::new(Mutex::new(None)),
            }))
        }

        /// 按开关启动辅助程序并加载选用且已安装的本地模型。没有打开任何本地开关（或模型未安装）时不启动辅助程序。
        fn local_parts(
            &self,
            local: &BuiltinLocal,
            colab_tts_override: bool,
        ) -> Result<(LocalParts, Option<LocalHelper>), CenterError> {
            let pick = |kind| {
                local.selected(kind).and_then(|spec| {
                    self.models
                        .path_if_installed(spec.id)
                        .map(|path| (spec, path))
                })
            };
            let (vad, asr, tts) = (
                pick(ModelKind::Vad),
                pick(ModelKind::Asr),
                if colab_tts_override {
                    None
                } else {
                    pick(ModelKind::Tts)
                },
            );
            if vad.is_none() && asr.is_none() && tts.is_none() {
                return Ok((
                    LocalParts {
                        fallback_tts: self.local_fallback_tts(local, None, colab_tts_override),
                        ..LocalParts::default()
                    },
                    None,
                ));
            }
            let exe = helper_path().ok_or_else(|| start_error("无法确定 MOSS 程序所在目录"))?;
            let helper =
                LocalHelper::spawn(&exe).map_err(|error| start_error(error.to_string()))?;
            let mut parts = LocalParts::default();
            if let Some((spec, model)) = vad {
                let engine = if spec.engine == Engine::TenVad {
                    VadEngine::Ten
                } else {
                    VadEngine::Silero
                };
                helper
                    .load_vad(&model, VadParams::for_engine(engine), engine)
                    .map_err(|error| {
                        start_error(format!("本地判停（{}）：{}", spec.label, error))
                    })?;
                parts.vad = Some(Arc::new(LocalVadFactory {
                    helper: helper.clone(),
                }));
            }
            if let Some((spec, dir)) = asr {
                let engine = if spec.engine == Engine::StreamingTransducer {
                    AsrEngine::StreamingTransducer
                } else {
                    AsrEngine::SenseVoice
                };
                helper
                    .load_asr_with_options(
                        &dir,
                        engine,
                        moss_voice_providers::local::AsrDecodeOptions {
                            beam_search: local.asr_beam_search,
                            hotwords: self.hotwords(local),
                        },
                    )
                    .map_err(|error| {
                        start_error(format!("本地识别（{}）：{}", spec.label, error))
                    })?;
                parts.asr = Some(LocalAsr {
                    helper: helper.clone(),
                });
                if engine.streaming() {
                    parts.streaming_asr = Some(Arc::new(LocalStreamingAsr {
                        helper: helper.clone(),
                    }));
                }
            }
            if let Some((spec, dir)) = tts {
                let engine = if spec.engine == Engine::Matcha {
                    TtsEngine::Matcha
                } else {
                    TtsEngine::Kokoro
                };
                let info = helper.load_tts(&dir, engine).map_err(|error| {
                    start_error(format!("本地合成（{}）：{}", spec.label, error))
                })?;
                // 单音色模型忽略音色编号
                let speaker = if info.speakers > 1 {
                    if local.tts_speaker < 0 || local.tts_speaker as u32 >= info.speakers {
                        return Err(start_error(format!(
                            "{} 的音色编号须在 0 ~ {} 之间",
                            spec.label,
                            info.speakers - 1
                        )));
                    }
                    local.tts_speaker
                } else {
                    0
                };
                parts.tts = Some(LocalTts {
                    helper: helper.clone(),
                    speaker,
                    speed: local.tts_speed,
                });
            } else {
                parts.fallback_tts =
                    self.local_fallback_tts(local, Some(helper.clone()), colab_tts_override);
            }
            Ok((parts, Some(helper)))
        }

        /// 三个分区都可用时构造对话（本地识别 / 合成优先于云端；开启 Colab T4 联动时临时接管大模型与合成）；否则返回 `None`（回声模式）。
        /// 第二项为合成是否流式（豆包），用于缩短播放启动缓冲。
        fn conversation(
            &self,
            models: &moss_voice::settings::BuiltinModels,
            local: &mut LocalParts,
            experience: &BuiltinLocal,
            colab: Option<&(moss_voice::colab::BuiltinColab, String)>,
        ) -> Result<Option<(Conversation, bool)>, CenterError> {
            let key = |section| self.secret(section).map(Option::unwrap_or_default);
            let invalid = |error: moss_voice_providers::ProviderError| {
                CenterError::new("builtin_models_invalid", 400, error.to_string())
            };
            let (llm_url, llm_key, llm_model, llm_label) = match colab {
                Some((c, c_key)) => (
                    c.url.as_str(),
                    c_key.clone(),
                    c.llm_model.as_str(),
                    format!("LLM:colab/{}", c.llm_model),
                ),
                None => {
                    let Some(llm) = &models.llm else {
                        return Ok(None);
                    };
                    (
                        llm.url.as_str(),
                        key(Section::Llm)?,
                        llm.model.as_str(),
                        format!("LLM:{}", llm.model),
                    )
                }
            };
            let hotwords = self.hotwords(experience);
            let mut streaming_asr: Option<Arc<dyn StreamingAsr>> = None;
            let asr: Arc<dyn Asr> = match (local.asr.take(), &models.asr) {
                (Some(local_asr), _) => {
                    streaming_asr = local.streaming_asr.take();
                    Arc::new(local_asr)
                }
                (None, Some(cloud)) if cloud.kind == ProviderKind::Doubao => {
                    let doubao = Arc::new(
                        DoubaoAsr::new(&cloud.url, &key(Section::Asr)?, &cloud.model)
                            .map_err(invalid)?
                            .with_hotwords(&hotwords),
                    );
                    streaming_asr = Some(doubao.clone());
                    doubao
                }
                (None, Some(cloud)) => Arc::new(
                    OpenAiAsr::new(&cloud.url, &key(Section::Asr)?, &cloud.model, None)
                        .map_err(invalid)?
                        .with_hotwords(&hotwords),
                ),
                (None, None) => return Ok(None),
            };
            let (tts, streaming_tts, tts_label): (Arc<dyn Tts>, bool, String) =
                match (colab, local.tts.take(), &models.tts) {
                    (Some((c, c_key)), _, _) => {
                        let openai =
                            OpenAiTts::new(&c.url, c_key, &c.tts_model, &c.tts_voice, c.tts_speed);
                        (
                            Arc::new(openai.map_err(invalid)?),
                            false,
                            format!("TTS:colab/{}", c.tts_model),
                        )
                    }
                    (None, Some(local), _) => (
                        Arc::new(local),
                        true,
                        format!("TTS:{}", experience.tts_model.as_str()),
                    ),
                    (None, None, Some(cloud)) if cloud.kind == ProviderKind::Doubao => {
                        let doubao = DoubaoTts::new(
                            &cloud.url,
                            &key(Section::Tts)?,
                            &cloud.model,
                            &cloud.voice,
                            cloud.speed,
                        );
                        (
                            Arc::new(doubao.map_err(invalid)?),
                            true,
                            format!("TTS:{}", cloud.model),
                        )
                    }
                    (None, None, Some(cloud)) => {
                        let openai = OpenAiTts::new(
                            &cloud.url,
                            &key(Section::Tts)?,
                            &cloud.model,
                            &cloud.voice,
                            cloud.speed,
                        );
                        (
                            Arc::new(openai.map_err(invalid)?),
                            false,
                            format!("TTS:{}", cloud.model),
                        )
                    }
                    (None, None, None) => return Ok(None),
                };
            let mut jev_options = JevFilterOptions {
                enabled: experience.jev_filter_enabled,
                unfinished_hold: experience.jev_unfinished_hold,
                fast_route: experience.read_only_shortcuts && experience.jev_fast_route,
                tool_prune: experience.jev_tool_prune,
                memory_filter: experience.jev_memory_filter,
            };
            let jev_model = moss_voice::settings::validate_jev_model(&experience.jev_model)
                .unwrap_or_else(|_| moss_voice::settings::default_jev_model());
            let local_jev_id = moss_voice::settings::canonical_local_jev_model(&jev_model);
            // 2026-11-07：本地 OneJev 按档位与是否有显卡加速决定等待预算；来不及时关闭“没说完”判断（界面另有说明）
            let local_budget = local_jev_id
                .and_then(|id| moss_voice::onejev::local_jev_budget(id, self.onejev_uses_gpu(id)));
            if local_budget.is_some_and(|budget| budget.hold_ms.is_none()) {
                jev_options.unfinished_hold = false;
            }
            let jev_ready = match local_jev_id {
                Some(id) => self.models.path_if_installed(id).is_some(),
                None => self.jev_effective_secret()?.is_some(),
            };
            let mut labels = vec![
                llm_label,
                format!(
                    "ASR:{}",
                    if experience.asr {
                        experience.asr_model.as_str()
                    } else {
                        models
                            .asr
                            .as_ref()
                            .map(|v| v.model.as_str())
                            .unwrap_or("unknown")
                    }
                ),
                tts_label,
            ];
            if jev_options.active() && jev_ready {
                labels.push(format!("Jev:{jev_model}"));
            }
            let llm = OpenAiLlm::new(llm_url, &llm_key, llm_model)
                .map_err(invalid)?
                .with_fast_reply(experience.llm_fast_reply);

            let original: Arc<dyn ToolExecutor> =
                Arc::new(HostTools::new(Arc::new(DesktopToolHost {
                    host: self.host.clone(),
                })));
            let plugin_host = Arc::new(DesktopInfoHost {
                host: self.host.clone(),
            });
            let info = InfoTools::new(plugin_host)
                .map_err(|_| internal("信息插件 HTTP 客户端初始化失败"))?;
            let combined: Arc<dyn ToolExecutor> = Arc::new(CombinedTools {
                original,
                info: Arc::new(info),
            });
            let tools: Arc<dyn ToolExecutor> = Arc::new(HomeTools {
                original: combined,
                home: Arc::new(DesktopHomeHost {
                    host: self.host.clone(),
                    control: self.home_control.clone(),
                }),
            });
            self.persona.configure(persona_config(experience));
            let tools: Arc<dyn ToolExecutor> = Arc::new(PersonaTools {
                original: tools,
                persona: self.persona.clone(),
            });
            let system_prompt = self.persona.system_prompt();
            let asr_corrections = moss_voice::settings::effective_asr_corrections(
                experience.asr_homophone_fix,
                &experience.custom_asr_corrections,
            );
            let mut conversation = Conversation::new(
                asr,
                Arc::new(llm),
                tts,
                tools,
                ConversationSettings {
                    system_prompt,
                    asr_corrections,
                    read_only_shortcuts: experience.read_only_shortcuts,
                    announce_job_done: experience.job_done_announce,
                    screen_preview_sync: experience.screen_preview_sync,
                    model_labels: labels,
                    ..Default::default()
                },
            )
            .with_memory(self.memory.clone())
            .with_speaker_enrollment(self.speaker.clone())
            .with_persona(self.persona.clone());
            if let Some(fallback_tts) = local.fallback_tts.take() {
                conversation = conversation.with_fallback_tts(fallback_tts);
            }
            if jev_options.active() {
                let filter = if let Some(local_id) = local_jev_id {
                    let reader: moss_voice_server::jev::SecretReader =
                        Arc::new(|| Some("moss-local-jev".to_owned()));
                    let upstream_model =
                        moss_voice::settings::local_jev_upstream_model_name(local_id);
                    let client = Arc::new(OpenRouterDecisionClient::with_endpoint(
                        moss_voice::models::DEFAULT_LOCAL_LAYA_DECISIONS_URL,
                        reader,
                        upstream_model,
                    ));
                    match local_budget {
                        Some(budget) => VoiceJevFilter::new(client, jev_options).with_timeouts(
                            std::time::Duration::from_millis(budget.turn_ms),
                            std::time::Duration::from_millis(budget.hold_ms.unwrap_or(0)),
                        ),
                        None => VoiceJevFilter::new(client, jev_options),
                    }
                } else {
                    let db = self.host.db.clone();
                    let settings_file = self.settings_file();
                    let reader = Arc::new(move || {
                        let guard = db.lock().ok()?;
                        resolve_openrouter_jev_secret_locked(&guard, &settings_file)
                            .ok()
                            .and_then(|(secret, _)| secret)
                    });
                    let client = Arc::new(OpenRouterDecisionClient::with_model(reader, jev_model));
                    VoiceJevFilter::new(client, jev_options)
                };
                conversation = conversation.with_jev_filter(Arc::new(filter));
            }
            // NV19: both hooks stay inert until the user switches them on in the private window.
            let (retrieval, extraction) = self.context.hooks();
            conversation = conversation
                .with_context_retrieval(retrieval)
                .with_context_extraction(extraction);
            if self.identity.valid_calibration().is_some()
                && self.settings().builtin_person_memory_enabled
            {
                // 换设备后若尚未下载声纹模型，仍必须挂上个人上下文：未知身份绝不回退共享设备记忆，同时不阻断语音服务启动
                match self.prepare_speaker_helper() {
                    Ok(()) => {
                        if let Ok(mut slot) = self.speaker_warning.lock() {
                            *slot = None;
                        }
                    }
                    Err(error) => {
                        let warning = format!(
                            "声纹按人记忆暂未生效（{}）：当前按未知身份处理、不会回退共享设备记忆，请下载声纹模型后重启内置服务",
                            error.message
                        );
                        eprintln!("[moss-desktop] {warning}");
                        self.diagnostics.push_from(
                            moss_voice_server::DiagSource::Moss,
                            EventRecord::warn(DiagCategory::Model, warning.clone())
                                .hint("到“隐私与家居 → 声纹”下载声纹模型，再重启内置服务"),
                        );
                        if let Ok(mut slot) = self.speaker_warning.lock() {
                            *slot = Some(warning);
                        }
                    }
                }
                conversation = conversation.with_personal_context(self.personal.clone());
            } else if let Ok(mut slot) = self.speaker_warning.lock() {
                *slot = None;
            }
            if let Some(streaming) = streaming_asr {
                conversation = conversation.with_streaming_asr(streaming);
            }
            Ok(Some((conversation, streaming_tts)))
        }

        pub fn status(&self) -> Value {
            let guard = self
                .server
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let view = guard.as_ref().map(VoiceServer::status);
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
                p.kind == ProviderKind::Openai
                    && moss_voice::models::is_openrouter_https_url(&p.url)
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
                    if settings
                        .builtin_chat_ready_with_colab(self.models.root(), colab_key_configured)
                    {
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
                    [
                        moss_voice::settings::LOCAL_ONEJEV_0_8B_MODEL,
                        moss_voice::settings::LOCAL_ONEJEV_4B_MODEL,
                        moss_voice::settings::LOCAL_ONEJEV_9B_MODEL,
                    ]
                    .into_iter()
                    .find(|id| self.models.path_if_installed(id).is_some())
                })
                .unwrap_or(moss_voice::settings::LOCAL_ONEJEV_0_8B_MODEL);
            let onejev_env =
                moss_voice::onejev::detect_onejev_env(self.models.root_for(onejev_probe_id), false);
            let laya_env = Self::detect_laya_env_with_probe(false);
            serde_json::json!({
                "running": view.is_some(),
                "mode": mode,
                "link_mode": settings.mode.as_str(),
                "ws_port": ws_port,
                "http_port": http_port,
                "mqtt_port": view.as_ref().and_then(|v| v.mqtt_port),
                "udp_port": view.as_ref().and_then(|v| v.udp_port),
                "ota_url": format!("http://{}:{http_port}/xiaozhi/ota/", moss_voice_server::MDNS_HOST_LABEL),
                "ota_ip_url": local_ip().map(|ip| format!("http://{ip}:{http_port}/xiaozhi/ota/")),
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
                    "models_dir": self.models.root_for(models_dir_ref).to_string_lossy(),
                    "onejev_env": onejev_env,
                    "jev_runtime": self.local_jev_runtime_status(&settings, jev_feedback_enabled),
                    "laya_env": laya_env,
                    "laya_setup": self.laya_setup.status(),
                    "helper_found": helper_path().is_some_and(|path| path.is_file()),
                    "helper_running": self.helper.lock().ok().and_then(|helper| helper.as_ref().map(LocalHelper::is_alive)).unwrap_or(false),
                    "models": self.models.status(),
                    "ready": {
                        "llm": settings.builtin_section_ready_with_colab(Section::Llm, self.models.root(), colab_key_configured),
                        "asr": settings.builtin_section_ready_with_colab(Section::Asr, self.models.root(), colab_key_configured),
                        "tts": settings.builtin_section_ready_with_colab(Section::Tts, self.models.root(), colab_key_configured),
                    },
                },
                "sessions": view.as_ref().map(|view| serde_json::to_value(&view.sessions).unwrap_or_default()).unwrap_or_else(|| Value::Array(Vec::new())),
                // n68：事件与耗时来自桌面端持有的诊断记录本，服务停止后仍可查看
                "events": self.diagnostics.events(),
                "events_dropped": self.diagnostics.drops(),
                "timings": self.diagnostics.timings(),
                "warnings": self.warnings(),
                "debug_enabled": view.as_ref().map(|v| v.debug_enabled).unwrap_or_else(|| self.debug_trace_enabled.load(std::sync::atomic::Ordering::Acquire)),
                "debug_traces": view.as_ref().map(|v| serde_json::to_value(&v.debug_traces).unwrap_or_default()).unwrap_or_else(|| Value::Array(Vec::new())),
                "device_prefs": settings.builtin_device_prefs,
                "goggle_bot_gifs": moss_voice_server::cached_goggle_bot_gifs(),
                "screen_style_gifs": moss_voice_server::cached_style_scene_gifs(),
                "memory_enabled": self.memory.enabled(),
                "devices": self.devices(),
            })
        }

        /// 启动（或重启）失败都写进诊断记录，界面上的报错弹窗关掉后仍能查到原因。
        /// 当前生效的降级提示（本地音乐目录不可用、声纹按人记忆未生效）。
        fn warnings(&self) -> Vec<String> {
            [&self.music_warning, &self.speaker_warning]
                .into_iter()
                .filter_map(|slot| slot.lock().ok().and_then(|warning| warning.clone()))
                .collect()
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
                    "laya_env": status["local"]["laya_env"],
                    "laya_setup": status["local"]["laya_setup"],
                    "asr_model": settings.builtin_local.asr_model,
                    "tts_model": settings.builtin_local.tts_model,
                    "vad_model": settings.builtin_local.vad_model,
                },
                "last_error": status["last_error"],
                "warnings": status["warnings"],
                "events": status["events"],
                "events_dropped": status["events_dropped"],
                "timings": status["timings"],
                "debug_enabled": status["debug_enabled"],
                "debug_traces": status["debug_traces"],
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

        fn start_locked(&self) -> Result<(), CenterError> {
            let result = self.start_locked_inner();
            if let Err(error) = &result {
                self.diagnostics.push_from(
                    moss_voice_server::DiagSource::Moss,
                    start_failure_event("内置语音服务启动失败", error),
                );
            } else {
                // n74：选了本地视觉模型时在后台预加载，设备第一次看图不用等
                self.sync_local_vision(true);
            }
            result
        }

        fn start_locked_inner(&self) -> Result<(), CenterError> {
            if !self.running() {
                let settings = self.settings();
                let colab = self.active_colab(&settings)?;
                let (mut local, helper) =
                    self.local_parts(&settings.builtin_local, colab.is_some())?;
                let mut conversation = self.conversation(
                    &settings.builtin_models,
                    &mut local,
                    &settings.builtin_local,
                    colab.as_ref(),
                )?;
                // No music scanning while disabled or chat unavailable;换机或拔盘目录不可用时跳过音乐索引，不阻断内置语音主服务启动。
                let (music, next_music_warning) =
                    scan_music_for_start(&settings.builtin_music, conversation.is_some());
                if let (Some(index), Some((chat, _))) = (&music, conversation.as_mut()) {
                    chat.tools = Arc::new(moss_voice_server::music::MusicTools::new(
                        chat.tools.clone(),
                        index.clone(),
                    ));
                }
                let defaults = ServerConfig::default();
                let playback_start_ms = match &conversation {
                    Some((_, true)) => STREAMING_PLAYBACK_START_MS,
                    _ => defaults.playback_start_ms,
                };
                let config = ServerConfig {
                    ws_port: settings.builtin_ws_port,
                    http_port: settings.builtin_http_port,
                    mode: if conversation.is_some() {
                        ServerMode::Chat
                    } else {
                        ServerMode::Echo
                    },
                    conversation: conversation.map(|(conversation, _)| Arc::new(conversation)),
                    music,
                    vision: Some(Arc::new(DesktopVisionFactory {
                        host: self.host.clone(),
                        local: self.local_vision.clone(),
                    })),
                    firmware: Some(Arc::new(DesktopFirmwareHost {
                        db: self.host.db.clone(),
                        dir: self.firmware_dir(),
                    })),
                    vad_factory: local.vad.take(),
                    barge_in: settings.builtin_local.barge_in,
                    barge_in_ms: settings.builtin_local.barge_in_ms,
                    playback_start_ms,
                    screen_card_style: moss_voice_server::ScreenCardStyle::from_setting(
                        &settings.builtin_local.screen_card_style,
                    ),
                    diagnostics: self.diagnostics.clone(),
                    // n71 常连模式（默认关）
                    mqtt: settings
                        .builtin_mqtt_enabled
                        .then_some(moss_voice_server::MqttConfig {
                            port: settings.builtin_mqtt_port,
                            udp_port: settings.builtin_udp_port,
                        }),
                    ..defaults
                };
                let registry = Arc::new(DbRegistry {
                    db: self.host.db.clone(),
                    host: self.host.clone(),
                    settings_file: self.settings_file(),
                });
                let server = VoiceServer::start(config, registry)
                    .map_err(|message| CenterError::new("builtin_start_failed", 409, message))?;
                server.set_debug_enabled(
                    self.debug_trace_enabled
                        .load(std::sync::atomic::Ordering::Acquire),
                );
                server.prewarm_speech_cache();
                {
                    let mut guard = self
                        .server
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    *guard = Some(server);
                }
                if let Ok(mut slot) = self.helper.lock() {
                    *slot = helper;
                }
                if let Some(warning) = &next_music_warning {
                    self.diagnostics.push_from(
                        moss_voice_server::DiagSource::Moss,
                        EventRecord::warn(DiagCategory::Config, warning.clone())
                            .hint("在“能力 → 本地音乐”里重新选择音乐文件夹"),
                    );
                }
                if let Ok(mut slot) = self.music_warning.lock() {
                    *slot = next_music_warning;
                }
                // 顶栏：服务已运行、还没有设备连接
                laser_host::set_voice_device_sessions(Some(0));
                laser_host::set_voice_pulse(None, 0);
                if let Ok(mut last) = self.last_error.lock() {
                    *last = None;
                }
            }
            if self.should_run_local_laya()
                && let Err(err) = self.ensure_laya_running(false)
            {
                eprintln!("[moss-desktop] 本地 Laya 决策服务启动失败：{err}");
                self.diagnostics.push_from(
                    moss_voice_server::DiagSource::Moss,
                    local_jev_failure("开启内置语音时拉起本地 Jev 决策服务失败", &err),
                );
            }
            Ok(())
        }

        fn stop_locked(&self) {
            self.stop_locked_inner(false);
            // n74：手动停止服务与退出时结束本地视觉推理进程；设置变更触发的重启不经过这里，模型保持加载
            self.local_vision.configure(None);
        }

        fn stop_locked_inner(&self, keep_local_jev: bool) {
            self.personal.directory_changed();
            let _ = self.speaker.cancel();
            if let Ok(mut helper) = self.speaker_backend.helper.lock() {
                *helper = None;
            }
            if let Ok(mut slot) = self.music_warning.lock() {
                *slot = None;
            }
            if let Ok(mut slot) = self.speaker_warning.lock() {
                *slot = None;
            }
            let server = self
                .server
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            if let Some(server) = server {
                server.stop();
                laser_host::set_voice_device_sessions(None);
                laser_host::set_voice_pulse(None, 0);
            }
            // 结束辅助程序（最后一个句柄丢弃时结束进程）
            if let Ok(mut helper) = self.helper.lock() {
                *helper = None;
            }
            if !keep_local_jev && !self.should_run_local_laya_for_feedback() {
                self.stop_laya();
            }
        }

        pub fn start(&self) -> Result<Value, CenterError> {
            {
                let _lifecycle = self
                    .lifecycle
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.start_locked()?;
            }
            Ok(self.status())
        }

        pub fn stop(&self) -> Value {
            {
                let _lifecycle = self
                    .lifecycle
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.stop_locked();
            }
            self.status()
        }

        fn restart_if_running(&self) -> Result<bool, CenterError> {
            let _lifecycle = self
                .lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !self.running() {
                return Ok(false);
            }
            let keep_local_jev = self.should_run_local_laya();
            self.stop_locked_inner(keep_local_jev);
            if let Err(err) = self.start_locked() {
                if !self.should_run_local_laya_for_feedback() {
                    self.stop_laya();
                }
                return Err(err);
            }
            Ok(true)
        }

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
                    return Ok(status);
                }
            }
            let debug_enable_patch = match patch.get("debug_trace_enabled") {
                Some(v) => Some(v.as_bool().ok_or_else(|| invalid("调试开关取值不正确"))?),
                None => None,
            };
            let clear_debug_patch = match patch.get("clear_debug_traces") {
                Some(v) => Some(
                    v.as_bool()
                        .ok_or_else(|| invalid("清空调试记录取值不正确"))?,
                ),
                None => None,
            };
            if debug_enable_patch.is_some() || clear_debug_patch.is_some() {
                if let Some(enabled) = debug_enable_patch {
                    self.debug_trace_enabled
                        .store(enabled, std::sync::atomic::Ordering::Release);
                }
                if let Ok(guard) = self.server.lock()
                    && let Some(server) = guard.as_ref()
                {
                    if let Some(enabled) = debug_enable_patch {
                        server.set_debug_enabled(enabled);
                    }
                    if clear_debug_patch == Some(true) {
                        server.clear_debug_traces();
                    }
                }
                let only_debug = patch.as_object().is_some_and(|obj| {
                    obj.keys()
                        .all(|k| k == "debug_trace_enabled" || k == "clear_debug_traces")
                });
                if only_debug {
                    let mut status = self.status();
                    status["restarted"] = Value::Bool(false);
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
                "read_only_shortcuts",
                "asr_beam_search",
                "barge_in",
                "job_done_announce",
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
                        .ok_or_else(|| invalid("音色编号须在 0 ~ 999 之间"))?
                        as i32,
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
                                    "read_only_shortcuts" => local.read_only_shortcuts = *val,
                                    "asr_beam_search" => local.asr_beam_search = *val,
                                    "barge_in" => local.barge_in = *val,
                                    "job_done_announce" => local.job_done_announce = *val,
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
                                    "tts_model" => local.tts_model = id.clone(),
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
                            let is_local =
                                moss_voice::settings::is_local_jev_model(&local.jev_model);
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
                let jev_feedback_on =
                    moss_voice::settings::jev_feedback_enabled(&self.settings_file());
                (is_local_jev, filter_on || jev_feedback_on)
            };
            if colab_raw.is_some() {
                if should_cancel_colab_cli && let Ok(mut st) = self.colab_cli.lock() {
                    st.generation = st.generation.wrapping_add(1);
                    if st.runner_state == "starting" {
                        st.runner_state = "idle".into();
                        st.runner_step = "已取消自动拉起云端 Colab T4".into();
                    }
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
                let _ = moss_voice::settings::update_voice(&self.settings_file(), |s| {
                    for entry in s.builtin_device_prefs.values_mut() {
                        entry.screen_pack_auto_at = None;
                    }
                    Ok(())
                });
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
                                | "clear_debug_traces"
                                | "persona_style"
                                | "persona_prompt"
                                | "custom_personas"
                                | "persona_risk_notes"
                                | "screen_pack_auto_keep"
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
            self.models
                .download(id, mirror.as_deref().unwrap_or(local_models::DEFAULT_BASE))
                .map_err(|message| CenterError::new("local_model_download_failed", 409, message))?;
            let mut status = self.status();
            if let Some(env_info) = checked_env {
                status["local"]["laya_env"] = env_info;
            }
            Ok(status)
        }

        pub fn model_cancel(&self, id: &str) -> Value {
            self.models.cancel(id);
            self.status()
        }

        pub fn model_delete(&self, id: &str) -> Result<Value, CenterError> {
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
            } else if self.running()
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
            self.models
                .delete(id)
                .map_err(|message| CenterError::new("local_model_delete_failed", 500, message))?;
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

        /// MOSS 可以自动管理的 Laya 根目录；用户用 `MOSS_LAYA_PYTHON` 指定了外部 Python 时返回 `None`。
        fn laya_env_root(&self) -> Option<std::path::PathBuf> {
            if moss_laya_python_env().is_some() {
                return None;
            }
            #[cfg(test)]
            {
                self.laya_root_for_test
                    .lock()
                    .ok()
                    .and_then(|root| root.clone())
            }
            #[cfg(not(test))]
            {
                Some(Self::resolve_laya_python().0).filter(|root| !root.as_os_str().is_empty())
            }
        }

        /// 开始自动准备 Laya 运行环境；本机没有可用 Python 时返回 412 并说明原因。
        fn start_laya_setup(&self) -> Result<(), CenterError> {
            if self.laya_setup.running() {
                return Ok(());
            }
            let root = self.laya_env_root().ok_or_else(|| {
                CenterError::new(
                    "laya_env_missing",
                    412,
                    "已通过 MOSS_LAYA_PYTHON 指定外部 Python，MOSS 不会改动该环境；请在其中自行执行 pip install \"laya[serve]\"",
                )
            })?;
            self.laya_setup
                .find_python()
                .map_err(|message| CenterError::new("laya_env_missing", 412, message))?;
            let mirror = self
                .settings()
                .builtin_local
                .mirror
                .unwrap_or_default()
                .to_ascii_lowercase();
            let prefer_mirror = [
                "mirror",
                "ghfast",
                "tuna",
                "aliyun",
                "ustc",
                "huaweicloud",
                "tencent",
            ]
            .iter()
            .any(|hint| mirror.contains(hint));
            self.laya_setup
                .start(moss_voice::laya_env::SetupRequest {
                    root,
                    prefer_mirror,
                })
                .map_err(|message| CenterError::new("laya_setup_failed", 500, message))
        }

        /// `voice_local_laya_setup`：`start` 开始准备、`cancel` 取消。
        pub fn laya_setup_action(&self, action: &str) -> Result<Value, CenterError> {
            match action {
                "start" => self.start_laya_setup()?,
                "cancel" => self.laya_setup.cancel(),
                _ => {
                    return Err(CenterError::new(
                        "voice_settings_invalid",
                        400,
                        "action 只能是 start 或 cancel",
                    ));
                }
            }
            Ok(self.status())
        }

        fn running(&self) -> bool {
            self.server
                .lock()
                .map(|guard| guard.is_some())
                .unwrap_or(false)
        }

        /// 测试一个分区的连接：密钥留空时用已保存的密钥。结果不含密钥。
        /// 读取当前分区密钥；若当前分区尚未保存密钥且填写的地址与另一个已保存 OpenAI 兼容分区同源（基础 `/models` 地址一致），自动沿用同源分区的本机密钥。
        fn secret_or_same_origin(
            &self,
            section: Section,
            input: &SectionInput,
        ) -> Result<Option<String>, CenterError> {
            if let Some(own) = self.secret(section)?.filter(|value| !value.is_empty()) {
                return Ok(Some(own));
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
            let settings = self.settings();
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
            };
            serde_json::to_value(result).map_err(|error| internal(error.to_string()))
        }

        /// n80：测试 Colab T4 专属联动（`target = "llm"` / `"tts"` 供顶部“测试全部环节”按环节探测；
        /// 默认 `"all"` 依次实测大模型工具调用、摄像头看图与 550W 克隆语音合成，并返回试听音频）。
        pub fn colab_test(&self, input: &Value) -> Result<Value, CenterError> {
            use moss_voice_providers::vision::VisionProvider as _;
            let invalid = |message: &str| CenterError::new("model_input_invalid", 400, message);
            let saved = self.settings().builtin_colab;
            let raw_url = input
                .get("url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(saved.url.as_str());
            let url = moss_voice::colab::normalize_colab_url(raw_url)
                .map_err(|message| invalid(&message))?;
            let raw_key = input
                .get("api_key")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let api_key = match raw_key {
                Some(k) => {
                    if k.len() > 8192 || k.chars().any(char::is_control) {
                        return Err(invalid("Colab API 密钥格式不正确"));
                    }
                    k
                }
                None => self
                    .colab_secret()?
                    .ok_or_else(|| invalid("请先填写 Colab API 密钥"))?,
            };
            let llm_model = moss_voice::colab::validate_colab_token(
                "Colab 大模型名",
                input
                    .get("llm_model")
                    .and_then(Value::as_str)
                    .unwrap_or(&saved.llm_model),
                moss_voice::colab::DEFAULT_COLAB_LLM_MODEL,
            )
            .map_err(|message| invalid(&message))?;
            let tts_model = moss_voice::colab::validate_colab_token(
                "Colab 合成模型名",
                input
                    .get("tts_model")
                    .and_then(Value::as_str)
                    .unwrap_or(&saved.tts_model),
                moss_voice::colab::DEFAULT_COLAB_TTS_MODEL,
            )
            .map_err(|message| invalid(&message))?;
            let tts_voice = moss_voice::colab::validate_colab_token(
                "Colab 合成音色",
                input
                    .get("tts_voice")
                    .and_then(Value::as_str)
                    .unwrap_or(&saved.tts_voice),
                moss_voice::colab::DEFAULT_COLAB_TTS_VOICE,
            )
            .map_err(|message| invalid(&message))?;
            let tts_speed = match input.get("tts_speed").and_then(Value::as_f64) {
                Some(v) => moss_voice::colab::validate_colab_speed(v)
                    .map_err(|message| invalid(&message))?,
                None => saved.tts_speed,
            };
            let target = input
                .get("target")
                .and_then(Value::as_str)
                .unwrap_or("all")
                .trim();
            let llm_resolved = moss_voice::models::ResolvedSection {
                section: Section::Llm,
                kind: ProviderKind::Openai,
                url: url.clone(),
                api_key: api_key.clone(),
                model: llm_model.clone(),
                voice: String::new(),
                speed: 1.0,
            };
            let tts_resolved = moss_voice::models::ResolvedSection {
                section: Section::Tts,
                kind: ProviderKind::Openai,
                url: moss_voice::models::tts_full_url(&url),
                api_key: api_key.clone(),
                model: tts_model,
                voice: tts_voice,
                speed: tts_speed as f64,
            };
            if target == "llm" {
                let res = moss_voice::models::test_section_with(&llm_resolved, None);
                return serde_json::to_value(res).map_err(|e| internal(e.to_string()));
            }
            if target == "tts" {
                let res = moss_voice::models::test_section_with(&tts_resolved, None);
                return serde_json::to_value(res).map_err(|e| internal(e.to_string()));
            }
            let llm_res = moss_voice::models::test_section_with(&llm_resolved, None);
            if !llm_res.ok {
                return Ok(serde_json::json!({
                    "section": "colab",
                    "ok": false,
                    "elapsed_ms": llm_res.elapsed_ms,
                    "detail": format!("大模型未通过：{}", llm_res.detail),
                }));
            }
            let mut png_buf = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(8, 8)
                .write_to(&mut png_buf, image::ImageFormat::Png)
                .map_err(|_| internal("生成内存测试图片失败"))?;
            let png = png_buf.into_inner();
            let vision_started = std::time::Instant::now();
            let vision_ok =
                moss_voice_providers::vision::OpenAiVision::new(&url, &llm_model, &api_key)
                    .ok()
                    .and_then(|provider| {
                        tauri::async_runtime::block_on(provider.explain(
                            "请仅用一句话描述此测试图像。",
                            "image/png",
                            &png,
                        ))
                        .ok()
                    })
                    .is_some_and(|text| !text.trim().is_empty());
            let vision_ms = vision_started.elapsed().as_millis() as u64;
            if !vision_ok {
                return Ok(serde_json::json!({
                    "section": "colab",
                    "ok": false,
                    "elapsed_ms": llm_res.elapsed_ms + vision_ms,
                    "detail": "大模型通过，但视觉看图未通过：请确认 Colab 上启动了带 --mmproj 的 Qwen3-VL 多模态模型",
                }));
            }
            let tts_res = moss_voice::models::test_section_with(&tts_resolved, None);
            let total_ms = llm_res.elapsed_ms + vision_ms + tts_res.elapsed_ms;
            if !tts_res.ok {
                return Ok(serde_json::json!({
                    "section": "colab",
                    "ok": false,
                    "elapsed_ms": total_ms,
                    "detail": format!("大模型与看图已通过，但 550W 克隆语音合成未通过：{}", tts_res.detail),
                }));
            }
            Ok(serde_json::json!({
                "section": "colab",
                "ok": true,
                "elapsed_ms": total_ms,
                "detail": format!(
                    "三项全部通过（大模型工具调用 {} ms · 摄像头看图 {} ms · 550W 克隆合成 {} ms）",
                    llm_res.elapsed_ms, vision_ms, tts_res.elapsed_ms
                ),
                "audio_data_url": tts_res.audio_data_url,
            }))
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
                        if let Some(raw_input) = input.section(*section)
                            && self.secret(*section)?.filter(|v| !v.is_empty()).is_none()
                            && let Some(peer_key) =
                                self.secret_or_same_origin(*section, raw_input)?
                        {
                            changes.push((name, Some(peer_key)));
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
                    .map_err(|message| {
                        CenterError::new("voice_settings_write_failed", 500, message)
                    })
                })?;
            }
            let restarted = self.restart_if_running()?;
            let mut status = self.status();
            status["restarted"] = Value::Bool(restarted);
            Ok(status)
        }

        /// 读取 Jev 可用密钥：优先本机 `jev_api_key`，未设时若大模型指向 `openrouter.ai` 则沿用大模型密钥。
        fn jev_effective_secret(&self) -> Result<Option<String>, CenterError> {
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            resolve_openrouter_jev_secret_locked(&db, &self.settings_file())
                .map(|(secret, _)| secret)
        }

        fn stop_laya(&self) {
            if let Ok(mut runner) = self.laya.lock() {
                runner.stop();
            }
        }

        fn stop_laya_and_wait(&self) {
            self.stop_laya();
            if let Ok(_guard) = self.jev_start_lock.lock() {
                self.stop_laya();
                if let Ok(mut r) = self.laya.lock() {
                    r.cancelling_model = None;
                }
            }
        }

        fn is_local_laya_configured_and_installed(
            &self,
            settings: &moss_voice::VoiceSettings,
        ) -> bool {
            let jev_model =
                moss_voice::settings::validate_jev_model(&settings.builtin_local.jev_model)
                    .unwrap_or_else(|_| moss_voice::settings::default_jev_model());
            moss_voice::settings::canonical_local_jev_model(&jev_model)
                .is_some_and(|id| self.models.path_if_installed(id).is_some())
        }

        fn should_run_local_laya_for_feedback(&self) -> bool {
            let settings = self.settings();
            self.is_local_laya_configured_and_installed(&settings)
                && moss_voice::settings::jev_feedback_enabled(&self.settings_file())
        }

        fn should_run_local_laya(&self) -> bool {
            let settings = self.settings();
            self.is_local_laya_configured_and_installed(&settings)
                && (settings.builtin_local.jev_filter_enabled
                    || moss_voice::settings::jev_feedback_enabled(&self.settings_file()))
        }

        fn select_laya_device(
            cuda_available: bool,
            mps_available: bool,
            env_override: Option<&str>,
        ) -> String {
            if let Some(raw) = env_override.map(str::trim).filter(|v| !v.is_empty()) {
                return raw.to_owned();
            }
            if cuda_available {
                "cuda:0".to_owned()
            } else if mps_available {
                "mps".to_owned()
            } else {
                "cpu".to_owned()
            }
        }

        fn probe_python_flag(py: &std::path::Path, snippet: &str) -> bool {
            if !py.is_file() {
                return false;
            }
            let mut cmd = std::process::Command::new(py);
            cmd.args(["-c", snippet])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
            let Ok(mut child) = cmd.spawn() else {
                return false;
            };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if !status.success() {
                            return false;
                        }
                        let mut out = String::new();
                        if let Some(mut stdout) = child.stdout.take() {
                            use std::io::Read;
                            let _ = stdout.read_to_string(&mut out);
                        }
                        return out.trim() == "1";
                    }
                    Ok(None) => {
                        if std::time::Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            return false;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(40));
                    }
                    Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return false;
                    }
                }
            }
        }

        fn probe_laya_cuda_available(py: &std::path::Path, cuda_lib_present: bool) -> bool {
            if !cuda_lib_present {
                return false;
            }
            Self::probe_python_flag(
                py,
                "import sys, torch; sys.stdout.write('1' if torch.cuda.is_available() else '0')",
            )
        }

        fn probe_laya_mps_available(py: &std::path::Path, is_apple_silicon: bool) -> bool {
            if !is_apple_silicon {
                return false;
            }
            Self::probe_python_flag(
                py,
                "import sys, torch; b = getattr(torch.backends, 'mps', None); sys.stdout.write('1' if b is not None and b.is_available() else '0')",
            )
        }

        fn laya_venv_python(root: &std::path::Path) -> std::path::PathBuf {
            #[cfg(windows)]
            {
                root.join(".venv").join("Scripts").join("python.exe")
            }
            #[cfg(not(windows))]
            {
                let py = root.join(".venv").join("bin").join("python");
                if py.is_file() {
                    py
                } else {
                    let py3 = root.join(".venv").join("bin").join("python3");
                    if py3.is_file() { py3 } else { py }
                }
            }
        }

        fn resolve_laya_python() -> (std::path::PathBuf, std::path::PathBuf) {
            let install_dir = writable_install_dir();
            let env_py = moss_laya_python_env();
            let laya_root = moss_laya_root_env()
                .or_else(|| {
                    install_dir.as_ref().and_then(|dir| {
                        [dir.join("laya"), dir.join("moss-laya")]
                            .into_iter()
                            .find(|cand| Self::laya_venv_python(cand).is_file())
                    })
                })
                .or_else(|| {
                    env_py
                        .as_ref()
                        .and_then(|p| p.parent())
                        .and_then(|bin| bin.parent())
                        .and_then(|venv| venv.parent().map(std::path::Path::to_path_buf))
                })
                .unwrap_or_else(|| {
                    if let Some(dir) = install_dir {
                        dir.join("laya")
                    } else {
                        let home = std::env::var_os("HOME")
                            .or_else(|| std::env::var_os("USERPROFILE"))
                            .map(std::path::PathBuf::from)
                            .filter(|p| p.is_absolute());
                        let local_appdata = std::env::var_os("LOCALAPPDATA")
                            .map(std::path::PathBuf::from)
                            .filter(|p| p.is_absolute());
                        let mut cands = Vec::new();
                        if let Some(h) = &home {
                            cands.push(h.join("moss-laya"));
                            cands.push(h.join(".moss").join("laya"));
                        }
                        if let Some(appdata) = &local_appdata {
                            cands.push(appdata.join("moss").join("laya"));
                        }
                        cands
                            .iter()
                            .find(|cand| Self::laya_venv_python(cand).is_file())
                            .cloned()
                            .or_else(|| home.map(|h| h.join("moss-laya")))
                            .or_else(|| local_appdata.map(|a| a.join("moss").join("laya")))
                            .unwrap_or_default()
                    }
                });
            let py = env_py.unwrap_or_else(|| {
                if laya_root.as_os_str().is_empty() {
                    std::path::PathBuf::new()
                } else {
                    Self::laya_venv_python(&laya_root)
                }
            });
            (laya_root, py)
        }

        fn detect_laya_env() -> Value {
            let skip_accel_probe = std::env::var("MOSS_LAYA_DEVICE")
                .or_else(|_| std::env::var("MOSS_JEV_DEVICE"))
                .ok()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("cpu"));
            Self::detect_laya_env_with_probe(!skip_accel_probe)
        }

        fn detect_laya_env_with_probe(probe_accel: bool) -> Value {
            let (_laya_root, py) = Self::resolve_laya_python();
            let force_cpu = std::env::var("MOSS_LAYA_DEVICE")
                .or_else(|_| std::env::var("MOSS_JEV_DEVICE"))
                .ok()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("cpu"));
            let python_found = py.is_file();
            let site_packages = py.parent().and_then(|s| s.parent()).and_then(|venv| {
                let win_sp = venv.join("Lib").join("site-packages");
                if win_sp.is_dir() {
                    return Some(win_sp);
                }
                let lib_dir = venv.join("lib");
                std::fs::read_dir(&lib_dir)
                    .ok()?
                    .flatten()
                    .find_map(|entry| {
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        name.starts_with("python")
                            .then(|| entry.path().join("site-packages"))
                            .filter(|p| p.is_dir())
                    })
            });
            let laya_serve_found = site_packages.as_ref().is_some_and(|sp| {
                sp.join("laya").join("serve.py").is_file() && sp.join("uvicorn").is_dir()
            });
            let torch_found = site_packages
                .as_ref()
                .is_some_and(|sp| sp.join("torch").is_dir());
            let cuda_lib_found = site_packages.as_ref().is_some_and(|sp| {
                let torch_lib = sp.join("torch").join("lib");
                torch_lib.join("torch_cuda.dll").is_file()
                    || torch_lib.join("libtorch_cuda.so").is_file()
            });
            let ready = python_found && laya_serve_found;
            // 虚拟环境由 MOSS 自动创建（删除 Laya 时一并删除）
            let venv_dir = py
                .parent()
                .and_then(|bin| bin.parent())
                .map(std::path::Path::to_path_buf);
            let managed = venv_dir
                .as_deref()
                .is_some_and(moss_voice::laya_env::is_managed);
            let is_apple_silicon = cfg!(all(target_os = "macos", target_arch = "aarch64"));
            let cuda_found = if !ready || force_cpu {
                false
            } else if probe_accel {
                Self::probe_laya_cuda_available(&py, cuda_lib_found)
            } else {
                cuda_lib_found
            };
            let mps_found = if !ready || force_cpu || cuda_found {
                false
            } else if probe_accel {
                Self::probe_laya_mps_available(&py, is_apple_silicon && torch_found)
            } else {
                is_apple_silicon && torch_found
            };
            let accelerator = if cuda_found {
                "cuda"
            } else if mps_found {
                "mps"
            } else {
                "cpu"
            };
            let summary = if ready && cuda_found {
                "运行环境检测通过（Python + laya[serve] + NVIDIA CUDA 显卡加速已就绪），可直接下载并启用。".to_owned()
            } else if ready && mps_found {
                "运行环境检测通过（Python + laya[serve] + Apple Metal MPS 加速已就绪），可直接下载并启用。".to_owned()
            } else if ready {
                "运行环境检测通过（Python + laya[serve] CPU 模式已就绪），可直接下载并启用。"
                    .to_owned()
            } else if !python_found {
                "尚未准备 Laya 运行环境：点“下载”或“准备运行环境”后，MOSS 会用本机 Python 3.10+ 自动创建虚拟环境并安装 laya[serve]（有 NVIDIA 显卡时装 CUDA 版 PyTorch）；也可直接选用上方免 Python 的 OneJev 1.6。".to_owned()
            } else {
                "已找到 Python 虚拟环境，但缺少 laya[serve] 依赖包：点“准备运行环境”自动补齐。"
                    .to_owned()
            };
            serde_json::json!({
                "ready": ready,
                "python_found": python_found,
                "laya_serve_found": laya_serve_found,
                "cuda_found": cuda_found,
                "mps_found": mps_found,
                "accelerator": accelerator,
                "managed": managed,
                "venv_dir": venv_dir.map(|dir| dir.to_string_lossy().into_owned()),
                "summary": summary,
            })
        }

        fn local_jev_missing_message(canonical_id: &str) -> String {
            let label = local_models::spec(canonical_id)
                .map(|s| s.label)
                .unwrap_or(canonical_id);
            format!(
                "本地决策模型“{label}”（{canonical_id}）尚未下载：请先在下方“本地模型（断网可用）”点击下载"
            )
        }

        fn ensure_laya_running(&self, wait_ready: bool) -> Result<(), String> {
            let settings = self.settings();
            let validated =
                moss_voice::settings::validate_jev_model(&settings.builtin_local.jev_model)
                    .unwrap_or_else(|_| moss_voice::settings::default_jev_model());
            let Some(canonical_id) = moss_voice::settings::canonical_local_jev_model(&validated)
            else {
                return Ok(());
            };
            let result = self.ensure_local_jev_running_for(canonical_id, wait_ready);
            // 启动失败的原因留给状态接口显示（打包后的程序看不到控制台输出）；主动取消不算失败
            if let Err(err) = &result
                && !err.contains("已取消")
                && let Ok(mut runner) = self.laya.lock()
            {
                runner.last_failure = Some((canonical_id.to_owned(), err.clone()));
            }
            result
        }

        /// 本地 OneJev 是否走显卡加速：推理进程已在运行时以实际探测为准，否则按推理引擎文件预估。
        fn onejev_uses_gpu(&self, canonical_id: &str) -> bool {
            if let Ok(runner) = self.laya.lock()
                && runner.active_model.as_deref() == Some(canonical_id)
                && let Some(gw) = runner.onejev.as_ref()
            {
                return gw.accelerator().kind.uses_gpu();
            }
            moss_voice::onejev::expected_gpu(self.models.root_for(canonical_id))
        }

        /// 当前所选本地 Jev 的实际运行情况（2026-11-07）：未下载 / 未启用 / 加载中 / 已就绪 / 启动失败 / 已停止。
        /// 后台启动失败或推理进程意外退出时带上原因，界面据此提示，不再“看着开着、实际每轮都跳过”。
        fn local_jev_runtime_status(
            &self,
            settings: &moss_voice::VoiceSettings,
            feedback_enabled: bool,
        ) -> Value {
            let validated =
                moss_voice::settings::validate_jev_model(&settings.builtin_local.jev_model)
                    .unwrap_or_else(|_| moss_voice::settings::default_jev_model());
            let Some(id) = moss_voice::settings::canonical_local_jev_model(&validated) else {
                return serde_json::json!({ "state": "cloud" });
            };
            let is_onejev = moss_voice::settings::is_local_onejev_model(id);
            let installed = self.models.path_if_installed(id).is_some();
            let wanted = settings.builtin_local.jev_filter_enabled || feedback_enabled;
            let (alive, starting, crashed, accelerator, crash_log, failure) = match self.laya.lock()
            {
                Ok(mut runner) => {
                    let alive = runner.is_alive_for(id);
                    let starting = runner.is_starting_for(id);
                    let crashed = !alive && runner.active_model.as_deref() == Some(id);
                    let gw = runner
                        .onejev
                        .as_ref()
                        .filter(|_| runner.active_model.as_deref() == Some(id));
                    let accelerator = gw.map(|gw| {
                        (
                            gw.accelerator().display_text(),
                            gw.accelerator().kind.uses_gpu(),
                        )
                    });
                    let crash_log = gw.map(|gw| gw.log_summary()).unwrap_or_default();
                    let failure = runner
                        .last_failure
                        .as_ref()
                        .filter(|(model, _)| model == id)
                        .map(|(_, message)| message.clone());
                    (alive, starting, crashed, accelerator, crash_log, failure)
                }
                Err(_) => (false, false, false, None, String::new(), None),
            };
            // Laya 子进程起来后还要加载权重，能答 /health 才算就绪
            let answering = alive
                && (is_onejev
                    || probe_local_jev_health_model(
                        std::net::SocketAddr::from(([127, 0, 0, 1], 18090)),
                        std::time::Duration::from_millis(120),
                    )
                    .as_deref()
                        == Some(id));
            let (state, error) = if !installed {
                ("not_installed", None)
            } else if answering {
                ("ready", None)
            } else if starting || alive {
                ("starting", None)
            } else if crashed {
                let hint = if crash_log.is_empty() {
                    String::new()
                } else {
                    format!("（{crash_log}）")
                };
                (
                    "failed",
                    Some(format!(
                        "本地推理进程已意外退出{hint}；关闭再打开 Jev 开关或重启语音服务即可重新拉起"
                    )),
                )
            } else if let Some(message) = failure {
                ("failed", Some(message))
            } else if !wanted {
                ("idle", None)
            } else {
                ("stopped", None)
            };
            let notice = if is_onejev {
                let gpu = accelerator
                    .as_ref()
                    .map(|(_, gpu)| *gpu)
                    .unwrap_or_else(|| moss_voice::onejev::expected_gpu(self.models.root_for(id)));
                moss_voice::onejev::local_jev_budget(id, gpu).and_then(|budget| budget.notice)
            } else {
                None
            };
            serde_json::json!({
                "model": id,
                "state": state,
                "accelerator": accelerator.map(|(label, _)| label),
                "error": error,
                "notice": notice,
            })
        }

        fn ensure_local_jev_running_for(
            &self,
            canonical_id: &str,
            wait_ready: bool,
        ) -> Result<(), String> {
            let Some(model_dir) = self.models.path_if_installed(canonical_id) else {
                return Err(Self::local_jev_missing_message(canonical_id));
            };
            if cfg!(test) {
                return Ok(());
            }
            {
                let mut runner = self
                    .laya
                    .lock()
                    .map_err(|_| "本地 Jev 进程锁失败".to_owned())?;
                if !wait_ready
                    && (runner.is_alive_for(canonical_id) || runner.is_starting_for(canonical_id))
                {
                    return Ok(());
                }
                if runner
                    .starting_model
                    .as_deref()
                    .is_some_and(|m| m != canonical_id)
                {
                    runner.cancel_in_flight();
                }
            }
            let start_guard = self
                .jev_start_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 18090));
            let (already_alive, start_generation) = {
                let mut runner = self
                    .laya
                    .lock()
                    .map_err(|_| "本地 Jev 进程锁失败".to_owned())?;
                let alive = runner.is_alive_for(canonical_id);
                if !alive {
                    runner.cancel_in_flight();
                    runner.kill_reaped();
                }
                (alive, runner.generation)
            };

            if let Some(serving_model) =
                probe_local_jev_health_model(addr, std::time::Duration::from_millis(120))
            {
                if serving_model == canonical_id {
                    return Ok(());
                }
                let release_deadline =
                    std::time::Instant::now() + std::time::Duration::from_millis(1500);
                let mut freed = false;
                while std::time::Instant::now() < release_deadline {
                    if probe_local_jev_health_model(addr, std::time::Duration::from_millis(100))
                        .is_none()
                    {
                        freed = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if !freed {
                    return Err(format!(
                        "本地端口 127.0.0.1:18090 已被外部 {serving_model} 服务占用，无法切换到 {canonical_id}；请先关闭外部 18090 进程"
                    ));
                }
            }

            if canonical_id == moss_voice::settings::LOCAL_LAYA_JEV_MODEL {
                if !already_alive {
                    let (laya_root, py) = Self::resolve_laya_python();
                    let env_device = std::env::var("MOSS_LAYA_DEVICE")
                        .or_else(|_| std::env::var("MOSS_JEV_DEVICE"))
                        .ok();
                    let has_override = env_device.as_deref().is_some_and(|v| !v.trim().is_empty());
                    let env_info = Self::detect_laya_env_with_probe(!has_override);
                    if env_info.get("ready").and_then(Value::as_bool) != Some(true) {
                        return Err(env_info
                            .get("summary")
                            .and_then(Value::as_str)
                            .unwrap_or("未找到本地 Laya 运行环境")
                            .to_owned());
                    }
                    let cuda_available =
                        env_info.get("cuda_found").and_then(Value::as_bool) == Some(true);
                    let mps_available =
                        env_info.get("mps_found").and_then(Value::as_bool) == Some(true);
                    let laya_device = Self::select_laya_device(
                        cuda_available,
                        mps_available,
                        env_device.as_deref(),
                    );
                    let tmp_dir = laya_root.join("tmp");
                    let tmp_existed = tmp_dir.exists();
                    let _ = std::fs::create_dir_all(&tmp_dir);
                    if !tmp_existed {
                        let _ =
                            std::fs::write(tmp_dir.join(moss_voice::laya_env::MANAGED_MARKER), b"");
                    }
                    let extra_models = serde_json::json!({
                        "multilingual": model_dir.to_string_lossy().replace('\\', "/")
                    })
                    .to_string();
                    let mut runner = self
                        .laya
                        .lock()
                        .map_err(|_| "本地 Jev 进程锁失败".to_owned())?;
                    if runner.generation != start_generation {
                        return Err("本地 Laya 启动已取消（开关已关闭或服务已停止）".into());
                    }
                    if !runner.is_alive_for(canonical_id)
                        && probe_local_jev_health_model(addr, std::time::Duration::from_millis(120))
                            .as_deref()
                            != Some(canonical_id)
                    {
                        let mut cmd = std::process::Command::new(&py);
                        cmd.args(["-m", "laya.serve"])
                            .env("USE_TF", "0")
                            .env("HF_HUB_OFFLINE", "1")
                            .env("TMP", &tmp_dir)
                            .env("TEMP", &tmp_dir)
                            .env("LAYA_HOST", "127.0.0.1")
                            .env("LAYA_PORT", "18090")
                            .env("LAYA_DEVICE", &laya_device)
                            .env("LAYA_PRELOAD", "1")
                            .env("LAYA_MODELS", "multilingual")
                            .env("LAYA_DEFAULT_MODEL", "multilingual")
                            .env("LAYA_MAX_LOADED", "1")
                            .env("LAYA_IDLE_UNLOAD_SECONDS", "0")
                            .env("LAYA_MAX_TOKEN_BUDGET", "4096")
                            .env("LAYA_EXTRA_MODELS", extra_models)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null());
                        #[cfg(windows)]
                        {
                            use std::os::windows::process::CommandExt;
                            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                            cmd.creation_flags(CREATE_NO_WINDOW);
                        }
                        let child = cmd
                            .spawn()
                            .map_err(|e| format!("启动本地 Laya 服务失败：{e}"))?;
                        runner.job = moss_voice::supervisor::JobHandle::for_child(&child);
                        runner.child = Some(child);
                        runner.active_model = Some(canonical_id.to_owned());
                        runner.last_failure = None;
                    }
                }
                if wait_ready {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(28);
                    while std::time::Instant::now() < deadline {
                        if self
                            .laya
                            .lock()
                            .map(|r| r.generation != start_generation)
                            .unwrap_or(false)
                        {
                            return Err("本地 Laya 启动已取消（开关已关闭或服务已停止）".into());
                        }
                        if probe_local_jev_health_model(addr, std::time::Duration::from_millis(200))
                            .as_deref()
                            == Some(canonical_id)
                        {
                            return Ok(());
                        }
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    }
                    return Err("本地 Laya 决策服务启动超时（正在加载模型，请稍后再试）".into());
                }
                return Ok(());
            }

            let models_root = self.models.root_for(canonical_id).to_path_buf();
            if moss_voice::onejev::resolve_llama_server(&models_root).is_none()
                && !moss_voice::onejev::has_platform_runtime_asset()
            {
                return Err(
                    "当前系统架构暂无内置预编译包，请安装 llama.cpp 并设置 MOSS_LLAMA_SERVER 指向 llama-server。".into(),
                );
            }

            if !already_alive {
                let mirror_hint = self.settings().builtin_local.mirror;
                let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
                {
                    let mut runner = self
                        .laya
                        .lock()
                        .map_err(|_| "本地 Jev 进程锁失败".to_owned())?;
                    if runner.generation != start_generation {
                        return Err("本地 OneJev 启动已取消（开关已关闭或服务已停止）".into());
                    }
                    runner.starting_model = Some(canonical_id.to_owned());
                    runner.starting_cancel = Some(cancel.clone());
                    runner.last_failure = None;
                }

                if !wait_ready {
                    let start_lock = self.jev_start_lock.clone();
                    let laya_slot = self.laya.clone();
                    let diagnostics = self.diagnostics.clone();
                    let id_owned = canonical_id.to_owned();
                    drop(start_guard);
                    let _ = std::thread::Builder::new()
                        .name(format!("onejev-start-{canonical_id}"))
                        .spawn(move || {
                            let _bg_guard = start_lock
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                                if let Ok(mut r) = laya_slot.lock() {
                                    if r.generation == start_generation {
                                        r.starting_model = None;
                                        r.starting_cancel = None;
                                    }
                                    if r.cancelling_model.as_deref() == Some(id_owned.as_str()) {
                                        r.cancelling_model = None;
                                    }
                                }
                                return;
                            }
                            match moss_voice::onejev::OneJevGateway::start_with_cancel(
                                &id_owned,
                                &model_dir,
                                &models_root,
                                addr,
                                mirror_hint.as_deref(),
                                &cancel,
                            ) {
                                Ok(mut gw) => {
                                    if let Ok(mut r) = laya_slot.lock() {
                                        if r.cancelling_model.as_deref() == Some(id_owned.as_str())
                                        {
                                            r.cancelling_model = None;
                                        }
                                        if r.generation != start_generation
                                            || cancel.load(std::sync::atomic::Ordering::SeqCst)
                                        {
                                            gw.stop();
                                        } else {
                                            r.starting_model = None;
                                            r.starting_cancel = None;
                                            r.onejev = Some(gw);
                                            r.active_model = Some(id_owned);
                                            r.last_failure = None;
                                        }
                                    } else {
                                        gw.stop();
                                    }
                                }
                                Err(err) => {
                                    if let Ok(mut r) = laya_slot.lock() {
                                        if r.generation == start_generation {
                                            r.starting_model = None;
                                            r.starting_cancel = None;
                                        }
                                        if r.cancelling_model.as_deref() == Some(id_owned.as_str())
                                        {
                                            r.cancelling_model = None;
                                        }
                                    }
                                    if !cancel.load(std::sync::atomic::Ordering::SeqCst) {
                                        eprintln!("[moss-desktop] 本地 OneJev ({id_owned}) 后台启动失败：{err}");
                                        diagnostics.push_from(moss_voice_server::DiagSource::Moss, local_jev_failure(&format!("本地 OneJev（{id_owned}）后台启动失败"), &err));
                                        if let Ok(mut r) = laya_slot.lock()
                                            && r.generation == start_generation
                                        {
                                            r.last_failure = Some((id_owned.clone(), err));
                                        }
                                    }
                                }
                            }
                        });
                    return Ok(());
                }

                let start_res = moss_voice::onejev::OneJevGateway::start_with_cancel(
                    canonical_id,
                    &model_dir,
                    &models_root,
                    addr,
                    mirror_hint.as_deref(),
                    &cancel,
                );
                let mut runner = self
                    .laya
                    .lock()
                    .map_err(|_| "本地 Jev 进程锁失败".to_owned())?;
                if runner.generation == start_generation {
                    runner.starting_model = None;
                    runner.starting_cancel = None;
                }
                if runner.cancelling_model.as_deref() == Some(canonical_id) {
                    runner.cancelling_model = None;
                }
                let mut gateway = start_res?;
                if runner.generation != start_generation
                    || cancel.load(std::sync::atomic::Ordering::SeqCst)
                {
                    gateway.stop();
                    return Err("本地 OneJev 启动已取消（开关已关闭或服务已停止）".into());
                }
                runner.onejev = Some(gateway);
                runner.active_model = Some(canonical_id.to_owned());
                runner.last_failure = None;
            }
            if wait_ready
                && probe_local_jev_health_model(addr, std::time::Duration::from_millis(300))
                    .as_deref()
                    != Some(canonical_id)
            {
                return Err("本地 OneJev 网关未就绪".into());
            }
            Ok(())
        }

        pub fn jev_test(&self, input: &Value) -> Result<Value, CenterError> {
            let current_model = self.settings().builtin_local.jev_model;
            let model = input
                .get("model")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .unwrap_or(current_model.as_str());
            let model = moss_voice::settings::validate_jev_model(model)
                .map_err(|msg| CenterError::new("model_input_invalid", 400, msg))?;
            if let Some(local_id) = moss_voice::settings::canonical_local_jev_model(&model) {
                self.ensure_local_jev_running_for(local_id, true)
                    .map_err(|msg| CenterError::new("model_input_invalid", 400, msg))?;
                return Ok(moss_voice::models::test_jev_decisions(
                    moss_voice::models::DEFAULT_LOCAL_LAYA_DECISIONS_URL,
                    "moss-local-jev",
                    local_id,
                ));
            }
            let typed_key = input
                .get("api_key")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            let api_key = if !typed_key.is_empty() {
                typed_key.to_owned()
            } else {
                self.jev_effective_secret()?.ok_or_else(|| {
                    CenterError::new("model_input_invalid", 400, "请填写 OpenRouter API Key")
                })?
            };
            Ok(moss_voice::models::test_jev_decisions(
                moss_voice::models::DEFAULT_JEV_DECISIONS_URL,
                &api_key,
                &model,
            ))
        }

        pub fn jev_models_list(&self, input: &Value) -> Result<Value, CenterError> {
            let typed_key = input
                .get("api_key")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(str::to_owned);
            let effective = match typed_key {
                Some(k) => Some(k),
                None => self.jev_effective_secret()?,
            };
            Ok(moss_voice::models::list_jev_models(
                moss_voice::models::DEFAULT_OPENROUTER_MODELS_URL,
                effective.as_deref(),
            ))
        }

        /// 组合保存 `models_save` + `jev_save` 前预校验 Jev 输入，防止普通模型落盘后因 Jev 参数格式错误造成半提交。
        pub fn precheck_jev_save_input(
            &self,
            input: Option<&Value>,
            clear: bool,
            pending_models: &ModelsInput,
            clearing_sections: &[Section],
        ) -> Result<(), CenterError> {
            if clear {
                return Ok(());
            }
            let invalid = |msg: &str| CenterError::new("model_input_invalid", 400, msg);
            let obj = input
                .and_then(Value::as_object)
                .ok_or_else(|| invalid("Jev 配置格式不正确"))?;
            let current_settings = self.settings();
            let model_raw = obj
                .get("model")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .unwrap_or(current_settings.builtin_local.jev_model.as_str());
            let model =
                moss_voice::settings::validate_jev_model(model_raw).map_err(|msg| invalid(&msg))?;
            let next_voice = match obj.get("voice_enabled") {
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| invalid("语音筛选开关取值不正确"))?,
                None => current_settings.builtin_local.jev_filter_enabled,
            };
            let effective_feedback = match obj.get("feedback_enabled") {
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| invalid("加工反馈开关取值不正确"))?,
                None => moss_voice::settings::jev_feedback_enabled(&self.settings_file()),
            };
            if let Some(local_id) = moss_voice::settings::canonical_local_jev_model(&model) {
                if (next_voice || effective_feedback)
                    && self.models.path_if_installed(local_id).is_none()
                {
                    return Err(invalid(&Self::local_jev_missing_message(local_id)));
                }
                return Ok(());
            }
            let typed_key = obj
                .get("api_key")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if !typed_key.is_empty() {
                return Ok(());
            }
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            if db
                .get_secret("jev_api_key")
                .map_err(db_failure)?
                .is_some_and(|v| !v.trim().is_empty())
            {
                return Ok(());
            }
            let plan = builtin::plan_save(
                &current_settings.builtin_models,
                pending_models,
                clearing_sections,
            )
            .map_err(|message| invalid(&message))?;
            let next_openrouter_llm = plan.models.llm.as_ref().is_some_and(|p| {
                p.kind == ProviderKind::Openai
                    && moss_voice::models::is_openrouter_https_url(&p.url)
            });
            let llm_has_key = plan
                .keys
                .iter()
                .find(|(sec, _)| *sec == Section::Llm)
                .map(|(_, ch)| match ch {
                    KeyChange::Set(v) => !v.trim().is_empty(),
                    KeyChange::Clear => false,
                    KeyChange::Keep => db
                        .get_secret(builtin_secret_name(Section::Llm))
                        .ok()
                        .flatten()
                        .is_some_and(|v| !v.trim().is_empty()),
                })
                .unwrap_or(false);
            if next_openrouter_llm && llm_has_key {
                Ok(())
            } else {
                Err(invalid("请填写 OpenRouter API Key"))
            }
        }

        pub fn jev_save(&self, input: Option<&Value>, clear: bool) -> Result<Value, CenterError> {
            let invalid = |msg: &str| CenterError::new("model_input_invalid", 400, msg);
            enum LayaAction {
                Start,
                Stop,
            }
            let laya_action = {
                let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
                let snapshot = self.settings();
                let mut changes: Vec<(String, Option<String>)> = Vec::new();
                let next_model: String;
                let next_voice: bool;
                let feedback_patch: Option<bool>;
                let action: LayaAction;
                if clear {
                    changes.push(("jev_api_key".into(), None));
                    next_voice = false;
                    next_model = moss_voice::settings::default_jev_model();
                    feedback_patch = Some(false);
                    action = LayaAction::Stop;
                } else {
                    let obj = input
                        .and_then(Value::as_object)
                        .ok_or_else(|| invalid("Jev 配置格式不正确"))?;
                    let model_raw = obj
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|m| !m.is_empty())
                        .unwrap_or(snapshot.builtin_local.jev_model.as_str())
                        .to_owned();
                    let model = moss_voice::settings::validate_jev_model(&model_raw)
                        .map_err(|msg| invalid(&msg))?;
                    let local_id = moss_voice::settings::canonical_local_jev_model(&model);
                    let typed_key = obj
                        .get("api_key")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .unwrap_or("");
                    next_voice = match obj.get("voice_enabled") {
                        Some(v) => v
                            .as_bool()
                            .ok_or_else(|| invalid("语音筛选开关取值不正确"))?,
                        None => snapshot.builtin_local.jev_filter_enabled,
                    };
                    feedback_patch = match obj.get("feedback_enabled") {
                        Some(v) => Some(
                            v.as_bool()
                                .ok_or_else(|| invalid("加工反馈开关取值不正确"))?,
                        ),
                        None => None,
                    };
                    let effective_feedback = feedback_patch.unwrap_or_else(|| {
                        moss_voice::settings::jev_feedback_enabled(&self.settings_file())
                    });
                    if let Some(lid) = local_id {
                        if (next_voice || effective_feedback)
                            && self.models.path_if_installed(lid).is_none()
                        {
                            return Err(invalid(&Self::local_jev_missing_message(lid)));
                        }
                        if !typed_key.is_empty() {
                            changes.push(("jev_api_key".into(), Some(typed_key.to_owned())));
                        }
                    } else if !typed_key.is_empty() {
                        changes.push(("jev_api_key".into(), Some(typed_key.to_owned())));
                    } else {
                        let (existing, has_own) =
                            resolve_openrouter_jev_secret_locked(&db, &self.settings_file())?;
                        let Some(resolved_key) = existing else {
                            return Err(invalid("请填写 OpenRouter API Key"));
                        };
                        if !has_own {
                            changes.push(("jev_api_key".into(), Some(resolved_key)));
                        }
                    }
                    next_model = model;
                    action = if local_id.is_some() && (next_voice || effective_feedback) {
                        LayaAction::Start
                    } else {
                        LayaAction::Stop
                    };
                }
                commit_secrets_then_settings(&db, &changes, || {
                    moss_voice::settings::update_voice_with_jev_feedback(
                        &self.settings_file(),
                        |settings| {
                            settings.builtin_local.jev_model = next_model.clone();
                            settings.builtin_local.jev_filter_enabled = next_voice;
                            Ok(((), feedback_patch))
                        },
                    )
                    .map_err(|message| {
                        CenterError::new("voice_settings_write_failed", 500, message)
                    })
                })?;
                action
            };
            match laya_action {
                LayaAction::Start => {
                    if let Err(err) = self.ensure_laya_running(false) {
                        eprintln!("[moss-desktop] 本地 Jev 决策服务启动失败：{err}");
                        self.diagnostics.push_from(
                            moss_voice_server::DiagSource::Moss,
                            local_jev_failure("切换 Jev 设置后本地 Jev 决策服务启动失败", &err),
                        );
                    }
                }
                LayaAction::Stop => self.stop_laya(),
            }
            let restarted = self.restart_if_running()?;
            let mut status = self.status();
            status["restarted"] = Value::Bool(restarted);
            Ok(status)
        }
        fn is_device_allowed(&self, device_id: &str) -> bool {
            self.host
                .db
                .lock()
                .ok()
                .and_then(|db| db.voice_devices().ok())
                .is_some_and(|list| {
                    list.into_iter()
                        .any(|d| d.device_id == device_id && d.is_allowed())
                })
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
            if label == "push_bot_pose" {
                let raw_pose = value
                    .and_then(|v| v.get("pose").or(Some(v)))
                    .and_then(Value::as_str)
                    .unwrap_or("idle");
                let pose = moss_voice_server::BotPose::from_slug(raw_pose).ok_or_else(|| {
                    invalid(
                        "小机器人动作只能是 idle、engrave、approval、music、celebrate 或 halted",
                    )
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
                        "设备请求只能是 refresh_info、reboot、set_volume、set_brightness、set_theme、set_talk_mode、push_preview、push_bot_pose、capture_photo、camera_frame 或 clear_camera_frame",
                    ));
                }
            };
            let server = self
                .server
                .lock()
                .map_err(|_| internal("内置语音服务状态锁失败"))?;
            let server = server.as_ref().ok_or_else(|| {
                CenterError::new("voice_builtin_stopped", 409, "内置语音服务未运行")
            })?;
            server
                .device_request(device_id, parsed)
                .map_err(|error| CenterError::new("voice_device_offline", 409, error))?;
            Ok(serde_json::json!({ "requested": label }))
        }

        pub fn set_device_status(
            &self,
            device_id: &str,
            status: &str,
        ) -> Result<Value, CenterError> {
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

        /// NV19 private-window management (`voice_context_manage`).
        pub fn context_manage(&self, input: &Value) -> Result<Value, CenterError> {
            let out = self.context.manage(input, || self.chat_llm())?;
            if input.get("action").and_then(Value::as_str) == Some("set_options")
                && input.get("sync_legacy_memory").and_then(Value::as_bool) == Some(true)
                && let Some(enabled) = out.get("retrieval").and_then(Value::as_bool)
            {
                let _ = self.memory_enable(enabled);
                if !enabled
                    && let Ok(db) = self.host.db.lock()
                    && let Ok(devices) = db.voice_devices()
                {
                    for d in devices.into_iter().filter(|d| d.is_allowed()) {
                        let _ = self
                            .memory
                            .device_changed(&d.device_id, || Ok::<_, String>(()));
                    }
                }
            }
            if input.get("action").and_then(Value::as_str) == Some("clear_all")
                && let Ok(list) = self.memory.list()
            {
                for item in list {
                    if !item.summary.is_empty() {
                        let _ = self.memory.edit(&item.device_id, item.revision, "");
                    }
                }
            }
            Ok(out)
        }

        pub fn context_add_folder(&self, path: &std::path::Path) -> Result<Value, CenterError> {
            self.context.add_folder(path)
        }

        /// The configured cloud LLM for NV19 deep search; `None` when not configured.
        fn chat_llm(&self) -> Option<Arc<dyn moss_voice_providers::Llm>> {
            let settings = self.settings();
            let (url, key, model) = match self.active_colab(&settings).ok().flatten() {
                Some((colab, key)) => (colab.url, key, colab.llm_model),
                None => {
                    let llm = settings.builtin_models.llm.as_ref()?;
                    let key = self.secret(Section::Llm).ok()?.unwrap_or_default();
                    (llm.url.clone(), key, llm.model.clone())
                }
            };
            OpenAiLlm::new(&url, &key, &model)
                .ok()
                .map(|llm| llm.with_fast_reply(settings.builtin_local.llm_fast_reply))
                .map(|llm| Arc::new(llm) as Arc<dyn moss_voice_providers::Llm>)
        }

        /// NV13 desktop-only inert configuration. Live execution remains locked
        /// until separately authorized real CLI sandbox-negative tests succeed.
        pub fn agent_stop(&self, run_id: &str) -> Value {
            let stopped = self.host.api_service.agent_stop(run_id);
            let mut status = self.agent_status();
            status["stopped"] = Value::Bool(stopped);
            status
        }
        pub fn agent_status(&self) -> Value {
            let mut status = self.host.api_service.agent_status();
            status["settings"] =
                serde_json::to_value(self.settings().builtin_agents).unwrap_or(Value::Null);
            status
        }
        /// NV13 Agent 配置（2026-10-15 起可开启；2026-10-25 支持留空自动读取本机 CLI 与换机自动适配）：
        /// 开启时必须有工作目录，且本机至少有一个手动填写或自动检测到的 CLI；每次运行仍要桌面批准并要求严格模式。
        pub fn agent_save(&self, input: &Value) -> Result<Value, CenterError> {
            use laser_host::voice_agent::detect_cli;
            use moss_voice::settings::BuiltinAgents;
            let invalid = |message: &str| CenterError::new("agent_config_invalid", 400, message);
            let raw: BuiltinAgents =
                serde_json::from_value(input.clone()).map_err(|_| invalid("Agent 配置格式无效"))?;
            let sensitive = [self.host.runtime_root.path.clone()];
            let mut validation_err: Option<CenterError> = None;
            moss_voice::settings::update_voice(&self.settings_file(), |settings| {
                match normalize_agent_save(raw, &settings.builtin_agents, &sensitive, detect_cli) {
                    Ok(config) => {
                        settings.builtin_agents = config;
                        Ok(())
                    }
                    Err(err) => {
                        validation_err = Some(err);
                        Err(String::new())
                    }
                }
            })
            .map_err(|_| {
                validation_err.unwrap_or_else(|| {
                    CenterError::new("agent_save_failed", 500, "Agent 配置保存失败")
                })
            })?;
            Ok(self.agent_status())
        }

        /// NV14: main-window-only local music settings. Reading status does not
        /// scan the user's filesystem; saving with enabled=true explicitly
        /// validates only the requested local folder.
        pub fn music_status(&self) -> Value {
            let settings = self.settings();
            let warning = if settings.builtin_music.enabled {
                self.music_warning.lock().ok().and_then(|slot| slot.clone())
            } else {
                None
            };
            serde_json::json!({
                "settings":settings.builtin_music,
                "running":self.running(),
                "requires_chat":true,
                "warning":warning
            })
        }
        pub fn music_save(&self, input: &Value) -> Result<Value, CenterError> {
            use moss_voice::settings::BuiltinMusic;
            let invalid = |message: &str| CenterError::new("music_invalid", 400, message);
            let config: BuiltinMusic =
                serde_json::from_value(input.get("settings").cloned().unwrap_or(Value::Null))
                    .map_err(|_| invalid("本地音乐配置格式无效"))?;
            if config.directory.len() > 1024 || config.directory.chars().any(char::is_control) {
                return Err(invalid("目录太长或含非法字符"));
            }
            let track_count = if config.enabled {
                if config.directory.trim().is_empty() {
                    return Err(invalid("先明确选择本地音乐目录"));
                }
                moss_voice_server::music::MusicIndex::scan(std::path::Path::new(&config.directory))
                    .map_err(|_| invalid("音乐目录无法读取、路径不安全或超过索引限制"))?
                    .len()
            } else {
                0
            };
            moss_voice::settings::update_voice(&self.settings_file(), |settings| {
                settings.builtin_music = config;
                Ok(())
            })
            .map_err(|_| CenterError::new("music_save_failed", 500, "音乐配置写入失败"))?;
            if let Ok(mut slot) = self.music_warning.lock() {
                *slot = None;
            }
            let restarted = self.restart_if_running()?;
            let mut result = self.music_status();
            result["restarted"] = Value::Bool(restarted);
            result["track_count"] = serde_json::json!(track_count);
            Ok(result)
        }

        /// NV15 inert Home Assistant configuration with optimistic revision.
        pub fn home_status(&self) -> Value {
            let credentials = self
                .host
                .db
                .lock()
                .ok()
                .and_then(|db| db.home_token_status().ok());
            let (key_configured, credential_revision) = match credentials {
                Some(status) => status,
                None => {
                    // A copied SQLite file may hold an un-decryptable DPAPI or Keychain
                    // envelope from another device or OS account. Permit ONLY private
                    // window import/recovery with revision CAS, not use of the
                    // foreign bearer nor any read/control authority.
                    let recover = self
                        .host
                        .db
                        .lock()
                        .ok()
                        .and_then(|db| db.home_token_recovery_revision().ok());
                    let profile =
                        moss_voice::settings::read_home_offline(&self.settings_file()).ok();
                    if let (Some(credential_revision), Some((settings, revision))) =
                        (recover, profile)
                    {
                        return serde_json::json!({
                            "settings":settings,"revision":revision,
                            "credential_revision":credential_revision,
                            "key_configured":false,"ready":false,"can_control":false,
                            "voice_read_enabled":false,"voice_control_enabled":false,
                            "recovery_only":true,
                            "reason":"旧凭据可能来自其他设备、其他系统账户或已损坏；只能导入安全配置并撤销旧密文，随后重新设置令牌。旧控制和只读授权不可用"
                        });
                    }
                    return serde_json::json!({
                        "settings":null,"revision":null,"credential_revision":null,
                        "key_configured":false,"ready":false,"can_control":false,
                        "reason":"Home Assistant 凭据或版本损坏，安全恢复不可用"
                    });
                }
            };
            match moss_voice::settings::read_home_offline(&self.settings_file()) {
                Ok((settings, revision)) => serde_json::json!({
                    "settings":settings, "revision":revision,"credential_revision":credential_revision,
                    "voice_read_enabled":DesktopHomeHost{host:self.host.clone(),control:self.home_control.clone()}.current().is_some(),
                    "voice_control_enabled":self.home_control.enabled(),
                    "ready":self.home_control.enabled(), "can_control":self.home_control.enabled(), "key_configured":key_configured,
                    "reason":"控制默认关闭；开启后语音只能创建家居审批，每次均须在电脑独立批准"
                }),
                Err(_) => serde_json::json!({
                    "settings":null, "revision":null,"credential_revision":null,
                    "ready":false, "can_control":false, "key_configured":false,
                    "reason":"Home Assistant 配置损坏或无法读取，禁止覆盖，请先修复设置文件"
                }),
            }
        }
        pub fn home_save(&self, input: &Value) -> Result<Value, CenterError> {
            use moss_voice::settings::{BuiltinHomeAssistant, HomeOfflineError};
            let invalid = |message: &str| CenterError::new("home_invalid", 400, message);
            let object = input
                .as_object()
                .ok_or_else(|| invalid("Home Assistant 配置必须是对象"))?;
            if object.len() != 2
                || !object.contains_key("settings")
                || !object.contains_key("expected_revision")
            {
                return Err(invalid(
                    "只接受 settings 和 expected_revision，拒绝令牌、密钥及控制字段",
                ));
            }
            let revision = object["expected_revision"]
                .as_u64()
                .filter(|value| *value <= 9_007_199_254_740_991)
                .ok_or_else(|| invalid("缺少有效的 Home Assistant 配置版本"))?;
            let next: BuiltinHomeAssistant = serde_json::from_value(object["settings"].clone())
                .map_err(|_| invalid("实体白名单格式无效或有禁止字段"))?;
            self.home_control
                .fence_sensitive_change(|| {
                    moss_voice::settings::save_home_offline(&self.settings_file(), &next, revision)
                })
                .map_err(|_| internal("无法安全撤销在途家居授权"))?
                .map_err(|error| match error {
                    HomeOfflineError::Invalid(message) => invalid(message),
                    HomeOfflineError::Conflict => CenterError::new(
                        "home_conflict",
                        409,
                        "白名单已在其他窗口修改，请重新加载后再保存",
                    ),
                    HomeOfflineError::Storage(_) => CenterError::new(
                        "home_save_failed",
                        500,
                        "Home Assistant 配置保存失败；原设置未被覆盖",
                    ),
                })?;
            Ok(self.home_status())
        }

        pub fn home_voice_consent(&self, input: &Value) -> Result<Value, CenterError> {
            let invalid = || CenterError::new("home_invalid", 400, "语音只读授权参数无效");
            let obj = input.as_object().ok_or_else(invalid)?;
            if obj.len() != 3
                || !obj.contains_key("enabled")
                || !obj.contains_key("policy_revision")
                || !obj.contains_key("credential_revision")
            {
                return Err(invalid());
            }
            let enabled = obj["enabled"].as_bool().ok_or_else(invalid)?;
            let expected_policy = obj["policy_revision"].as_u64().ok_or_else(invalid)?;
            let expected_credential = obj["credential_revision"].as_u64().ok_or_else(invalid)?;
            // The configuration read is protected by revision. A later change
            // automatically invalidates this exact grant.
            let (config, policy) = moss_voice::settings::read_home_offline(&self.settings_file())
                .map_err(|_| invalid())?;
            config.validate_offline().map_err(|_| invalid())?;
            if policy != expected_policy {
                return Err(CenterError::new(
                    "home_conflict",
                    409,
                    "家居白名单已改变，拒绝授权",
                ));
            }
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            let (present, credential) = db.home_token_status().map_err(|_| invalid())?;
            if credential != expected_credential
                || (enabled
                    && (!present
                        || config.base_url.is_empty()
                        || !config.entities.iter().any(|e| e.enabled)))
            {
                return Err(CenterError::new(
                    "home_conflict",
                    409,
                    "令牌或实体已更改，拒绝授权",
                ));
            }
            if enabled {
                db.set_secret(HOME_VOICE_GRANT, &format!("{policy}:{credential}"), now())
                    .map_err(|_| internal("语音只读授权未保存"))?;
            } else {
                db.delete_secret(HOME_VOICE_GRANT)
                    .map_err(|_| internal("撤销语音只读授权失败"))?;
            }
            drop(db);
            Ok(self.home_status())
        }
        pub fn home_control_consent(&self, input: &Value) -> Result<Value, CenterError> {
            let invalid = || CenterError::new("home_invalid", 400, "家居控制授权参数无效");
            let map = input.as_object().ok_or_else(invalid)?;
            if map.len() != 4
                || map.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "enabled" | "confirmed" | "policy_revision" | "credential_revision"
                    )
                })
            {
                return Err(invalid());
            }
            self.home_control
                .consent(
                    map["enabled"].as_bool().ok_or_else(invalid)?,
                    map["policy_revision"].as_u64().ok_or_else(invalid)?,
                    map["credential_revision"].as_u64().ok_or_else(invalid)?,
                    map["confirmed"].as_bool().ok_or_else(invalid)?,
                )
                .map_err(|code| {
                    CenterError::new(code, 409, "家居控制授权未生效，请核对白名单、凭据与确认")
                })?;
            Ok(self.home_status())
        }
        pub fn home_approval_approve(&self, input: &Value) -> Result<Value, CenterError> {
            let invalid = || CenterError::new("home_approval_invalid", 400, "家居批准参数无效");
            let map = input.as_object().ok_or_else(invalid)?;
            if map.len() != 3
                || map
                    .keys()
                    .any(|key| !matches!(key.as_str(), "approval_id" | "digest" | "confirmed"))
            {
                return Err(invalid());
            }
            let id = map["approval_id"]
                .as_str()
                .filter(|s| s.len() <= 128)
                .ok_or_else(invalid)?;
            let digest = map["digest"]
                .as_str()
                .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or_else(invalid)?;
            self.home_control
                .approve(id, digest, map["confirmed"].as_bool().ok_or_else(invalid)?)
                .map_err(|code| {
                    CenterError::new(
                        code,
                        409,
                        "家居批准或执行未确认；请查看持久结果，不自动重发",
                    )
                })
        }

        /// Main-window private-only redacted profile export. A portable
        /// profile NEVER contains the DPAPI ciphertext, bearer key, grants,
        /// approvals or the rest of settings.json.
        pub fn home_backup_export(&self) -> Result<Value, CenterError> {
            let profile = moss_voice::settings::export_home_portable(&self.settings_file())
                .map_err(|_| {
                    CenterError::new(
                        "home_backup_invalid",
                        409,
                        "白名单损坏或过大，无法生成安全备份",
                    )
                })?;
            Ok(serde_json::json!({
                "profile":profile,
                "credentials_included":false,
                "reenter_token_required":true,
            }))
        }

        /// Import is explicitly destructive to old *authority*, but not to
        /// user files: revoke token/grants FIRST, then CAS-save an inert
        /// imported profile. If settings write fails, privileges remain revoked.
        pub fn home_backup_restore(&self, input: &Value) -> Result<Value, CenterError> {
            use moss_voice::settings::{self, HomeOfflineError};
            let invalid =
                || CenterError::new("home_backup_invalid", 400, "备份格式、版本或确认无效");
            let map = input.as_object().ok_or_else(invalid)?;
            if map.len() != 4
                || !map.keys().all(|key| {
                    matches!(
                        key.as_str(),
                        "profile"
                            | "expected_revision"
                            | "expected_credential_revision"
                            | "confirmed"
                    )
                })
            {
                return Err(invalid());
            }
            if map["confirmed"].as_bool() != Some(true) {
                return Err(invalid());
            }
            let payload = map["profile"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 65536)
                .ok_or_else(invalid)?;
            let imported =
                settings::validate_home_portable(payload.as_bytes()).map_err(|_| invalid())?;
            let revision = map["expected_revision"]
                .as_u64()
                .filter(|v| *v <= 9_007_199_254_740_991)
                .ok_or_else(invalid)?;
            let credential = map["expected_credential_revision"]
                .as_u64()
                .filter(|v| *v <= 9_007_199_254_740_991)
                .ok_or_else(invalid)?;
            let file = self.settings_file();
            self.home_control
                .fence_sensitive_change(|| -> Result<(), CenterError> {
                    let (_, current) = settings::read_home_offline(&file).map_err(|_| {
                        CenterError::new("home_conflict", 409, "白名单状态已改变或损坏，未恢复")
                    })?;
                    if current != revision {
                        return Err(CenterError::new(
                            "home_conflict",
                            409,
                            "其他窗口已修改白名单，请重新加载",
                        ));
                    }
                    // Import must also recover a database copied from another
                    // Windows user: it can revoke a foreign DPAPI blob without
                    // decrypting it. SQLite compares the expected revision under
                    // BEGIN IMMEDIATE; it never imports old credentials.
                    let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
                    let changed = db
                        .update_home_token_if_revision(credential, None, now())
                        .map_err(|_| {
                            CenterError::new("home_unavailable", 503, "撤销旧令牌失败，备份未导入")
                        })?;
                    if changed.is_none() {
                        return Err(CenterError::new(
                            "home_conflict",
                            409,
                            "令牌版本并发冲突，备份未导入",
                        ));
                    }
                    // DB mutation and config file write are not one transaction.
                    // If writing the inactive template fails, the old token and
                    // grants remain REVOKED (fail closed), never rolled back.
                    settings::save_home_offline(&file, &imported, revision).map_err(|error| {
                        match error {
                            HomeOfflineError::Invalid(_) => invalid(),
                            HomeOfflineError::Conflict => CenterError::new(
                                "home_conflict",
                                409,
                                "令牌已撤销，但白名单版本变更；请重新加载",
                            ),
                            HomeOfflineError::Storage(_) => CenterError::new(
                                "home_save_failed",
                                500,
                                "令牌已撤销，但白名单保存失败；请重新加载",
                            ),
                        }
                    })?;
                    Ok(())
                })
                .map_err(|_| internal("无法安全撤销在途家居授权"))??;
            Ok(self.home_status())
        }

        /// NV15 desktop-only token mutation: stored only in app_secrets,
        /// guarded by monotonic credential revision. No network call is made.
        pub fn home_token_save(&self, input: &Value) -> Result<Value, CenterError> {
            let invalid = |message: &str| CenterError::new("home_token_invalid", 400, message);
            let map = input
                .as_object()
                .ok_or_else(|| invalid("凭据变更必须是对象"))?;
            let action = map
                .get("action")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("必须选择更新或清除"))?;
            let expected = map
                .get("expected_revision")
                .and_then(Value::as_u64)
                .filter(|rev| *rev <= 9_007_199_254_740_991)
                .ok_or_else(|| invalid("缺少有效的凭据版本"))?;
            if map
                .keys()
                .any(|key| !matches!(key.as_str(), "action" | "expected_revision" | "value"))
            {
                return Err(invalid("凭据操作包含禁止字段"));
            }
            let replacement = match action {
                "set" => Some(
                    map.get("value")
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid("请输入长期访问令牌"))?,
                ),
                "clear" => {
                    if map.contains_key("value") {
                        return Err(invalid("清除操作不能携带令牌"));
                    }
                    None
                }
                _ => return Err(invalid("仅支持更新或清除令牌")),
            };
            let changed = self
                .home_control
                .fence_sensitive_change(|| {
                    let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
                    db.update_home_token_if_revision(expected, replacement, now())
                        .map_err(|_| {
                            CenterError::new(
                                "home_token_invalid",
                                400,
                                "访问令牌未保存；请检查令牌格式和数据库",
                            )
                        })
                })
                .map_err(|_| internal("无法安全撤销在途家居授权"))??;
            if changed.is_none() {
                return Err(CenterError::new(
                    "home_conflict",
                    409,
                    "凭据已更新，请重新加载最新状态",
                ));
            }
            Ok(self.home_status())
        }

        /// Explicit trusted main-window read-only discovery; never sends a
        /// Home Assistant service POST and never exposes the bearer token.
        pub fn home_discover(&self) -> Result<Value, CenterError> {
            use moss_voice::home_readonly::{self, DiscoveryError};
            let (settings, policy_revision) =
                moss_voice::settings::read_home_offline(&self.settings_file()).map_err(|_| {
                    CenterError::new("home_unavailable", 409, "无法安全读取家居白名单")
                })?;
            let (token, credential_revision) = {
                let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
                let (has_token, revision) = db
                    .home_token_status()
                    .map_err(|_| CenterError::new("home_unavailable", 409, "访问令牌配置损坏"))?;
                if !has_token {
                    return Err(CenterError::new(
                        "home_unauthorized",
                        401,
                        "请先设置访问令牌",
                    ));
                }
                let token = db
                    .home_token_for_connection()
                    .map_err(|_| CenterError::new("home_unavailable", 409, "无法读取访问令牌"))?
                    .ok_or_else(|| CenterError::new("home_unauthorized", 401, "访问令牌不存在"))?;
                (token, revision)
            };
            let output=home_readonly::discover_guarded(&settings,&token,policy_revision,credential_revision,||{
                let current_policy=moss_voice::settings::read_home_offline(&self.settings_file()).ok();
                let current_credential=self.host.db.lock().ok()
                    .and_then(|db|db.home_token_status().ok());
                matches!(current_policy,Some((ref latest,rev)) if *latest==settings && rev==policy_revision)
                    && matches!(current_credential,Some((true,rev)) if rev==credential_revision)
            })
                .map_err(|error|match error {
                    DiscoveryError::Unauthorized=>CenterError::new("home_unauthorized",401,"Home Assistant 拒绝访问，请检查令牌"),
                    DiscoveryError::Unavailable=>CenterError::new("home_unavailable",503,"Home Assistant 无法连接或连接超时"),
                    DiscoveryError::MissingCredential=>CenterError::new("home_unauthorized",401,"Home Assistant 令牌格式无效"),
                    DiscoveryError::InvalidSettings=>CenterError::new("home_invalid",400,"地址、白名单或配置版本无效"),
                    DiscoveryError::InvalidResponse=>CenterError::new("home_bad_response",502,"Home Assistant 返回无效或过大的响应"),
                })?;
            let (_, latest_policy) = moss_voice::settings::read_home_offline(&self.settings_file())
                .map_err(|_| {
                    CenterError::new("home_conflict", 409, "家居配置读取失败，请重新加载")
                })?;
            let latest_token = self
                .host
                .db
                .lock()
                .map_err(|_| internal("数据库锁失败"))?
                .home_token_status()
                .map_err(|_| CenterError::new("home_conflict", 409, "令牌状态读取失败"))?;
            if latest_policy != policy_revision
                || latest_token.1 != credential_revision
                || !latest_token.0
            {
                return Err(CenterError::new(
                    "home_conflict",
                    409,
                    "查询期间配置已更改，结果已丢弃",
                ));
            }
            serde_json::to_value(output).map_err(|_| internal("Home Assistant 查询结果序列化失败"))
        }

        /// NV15 private HomeControl inbox. Only sanitized previews; never return
        /// saved connection fingerprint, the request body, secrets or raw URLs.
        pub fn home_approvals(&self) -> Result<Value, CenterError> {
            use laser_contracts::{ApprovalKind, ApprovalStatus};
            let approvals = laser_api::approval::ApprovalService::new(self.host.db.clone());
            let rows = approvals.list(None, 250).map_err(|_| {
                CenterError::new("home_approval_unavailable", 503, "无法读取家居审批记录")
            })?;
            let (policy, credential) = {
                let (_, p) = moss_voice::settings::read_home_offline(&self.settings_file())
                    .map_err(|_| {
                        CenterError::new("home_unavailable", 409, "Home Assistant 白名单不可读取")
                    })?;
                let (_, c) = self
                    .host
                    .db
                    .lock()
                    .map_err(|_| internal("数据库锁失败"))?
                    .home_token_status()
                    .map_err(|_| {
                        CenterError::new("home_unavailable", 409, "HA 凭据状态不可读取")
                    })?;
                (p, c)
            };
            let mut items = Vec::new();
            for row in rows
                .iter()
                .filter(|r| r.kind == ApprovalKind::HomeControl)
                .take(60)
            {
                let requested = row.request.as_ref();
                let path = requested
                    .and_then(|req| req.get("path"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let service = path
                    .strip_prefix("/api/services/")
                    .and_then(|tail| tail.split_once('/'))
                    .filter(|(domain, action)| {
                        matches!(
                            *domain,
                            "light"
                                | "switch"
                                | "climate"
                                | "lock"
                                | "alarm_control_panel"
                                | "cover"
                                | "media_player"
                        ) && matches!(
                            *action,
                            "turn_on"
                                | "turn_off"
                                | "set_temperature"
                                | "lock"
                                | "unlock"
                                | "alarm_arm_home"
                                | "alarm_arm_away"
                                | "alarm_disarm"
                                | "open_cover"
                                | "close_cover"
                                | "stop_cover"
                                | "play_media"
                        )
                    })
                    .map(|(domain, action)| format!("{domain}.{action}"))
                    .unwrap_or_else(|| "unsupported".into());
                let mut safe_parameters = serde_json::Map::new();
                if let Some(body) = requested
                    .and_then(|req| req.get("body"))
                    .and_then(Value::as_object)
                {
                    if let Some(brightness) = body
                        .get("brightness")
                        .and_then(Value::as_u64)
                        .filter(|n| *n <= 255)
                    {
                        safe_parameters.insert("brightness".into(), brightness.into());
                    }
                    if let Some(temperature) = body
                        .get("temperature")
                        .and_then(Value::as_f64)
                        .filter(|n| n.is_finite() && (-40.0..=100.0).contains(n))
                    {
                        safe_parameters
                            .insert("temperature".into(), serde_json::json!(temperature));
                    }
                }
                let (saved_policy, saved_credential) = requested
                    .map(|req| {
                        (
                            req.get("policy")
                                .or_else(|| req.get("policy_revision"))
                                .and_then(Value::as_u64),
                            req.get("credential")
                                .or_else(|| req.get("credential_revision"))
                                .and_then(Value::as_u64),
                        )
                    })
                    .unwrap_or_default();
                let stale = saved_policy != Some(policy) || saved_credential != Some(credential);
                let phase = row
                    .outcome
                    .as_ref()
                    .and_then(|o| o.phase)
                    .and_then(|phase| serde_json::to_value(phase).ok());
                items.push(serde_json::json!({
                    "approval_id":row.approval_id,
                    "digest":row.digest,
                    "status":row.status.as_str(),
                    "entity_id":row.target_record,
                    "service":service,
                    "parameters":safe_parameters,
                    "summary":row.summary.chars().take(200).collect::<String>(),
                    "source":"voice",
                    "device_suffix":row.session_id.as_deref().unwrap_or("").chars().rev().take(4).collect::<String>()
                        .chars().rev().collect::<String>(),
                    "created_at":row.created_at,
                    "expires_at":row.expires_at,
                    "stale":stale,
                    "can_revoke":matches!(row.status,ApprovalStatus::Pending|ApprovalStatus::Approved),
                    "can_approve":!stale && self.home_control.can_approve(&row.approval_id),
                    "outcome":row.outcome.as_ref().map(|outcome|serde_json::json!({
                        "phase":phase,
                        "error_code":outcome.error_code,
                        "message":outcome.message.chars().take(200).collect::<String>(),
                    })),
                }));
            }
            Ok(serde_json::json!({
                "approvals":items,
                "server_time":now(),
                "can_control":self.home_control.enabled(),
                "reason":"控制授权与逐次批准相互独立；语音断开、变更白名单或凭据将拒绝旧请求",
            }))
        }

        /// Independent private-window revoke. Unlike the general approval
        /// routes, this operation accepts HomeControl ONLY and cannot execute.
        pub fn home_approval_cancel(&self, input: &Value) -> Result<Value, CenterError> {
            let invalid = || CenterError::new("home_approval_invalid", 400, "家居撤销请求格式无效");
            let object = input.as_object().ok_or_else(invalid)?;
            if object.len() != 3 || object.get("confirmed") != Some(&Value::Bool(true)) {
                return Err(invalid());
            }
            let id = object
                .get("approval_id")
                .and_then(Value::as_str)
                .filter(|id| {
                    (12..=128).contains(&id.len())
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                })
                .ok_or_else(invalid)?;
            let digest = object
                .get("digest")
                .and_then(Value::as_str)
                .filter(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or_else(invalid)?;
            // Cancellation cannot race between the final permit check and
            // the first byte of a HomeControl POST.
            let result = self
                .home_control
                .fence_request_change(id, || {
                    let service = laser_api::approval::ApprovalService::new(self.host.db.clone());
                    service.cancel_home_control(id, digest).map_err(|err| {
                        CenterError::new(err.code(), err.http_status(), err.message())
                    })
                })
                .map_err(|_| internal("家居待执行请求撤销失败"))??;
            Ok(serde_json::json!({
                "approval_id":result.approval_id,
                "status":result.status.as_str(),
                "can_control":false,
            }))
        }

        /// NV12: private desktop-only settings and API-key presence. Never return the key.
        pub fn vision_status(&self) -> Value {
            use moss_voice_server::vision::VisionFactory;
            let settings = self.settings();
            let vision = &settings.builtin_vision;
            let key_configured = self
                .host
                .db
                .lock()
                .ok()
                .and_then(|db| db.get_secret("voice.vision.main").ok().flatten())
                .is_some_and(|s| !s.trim().is_empty());
            // 2026-10-16：只回对话模型的地址与模型名，用于卡片显示“照片会发给谁”，不回密钥。
            let dialog_llm = match settings.builtin_models.llm.as_ref() {
                Some(llm) => {
                    let key = self
                        .host
                        .db
                        .lock()
                        .ok()
                        .and_then(|db| db.get_secret("voice.llm.main").ok().flatten())
                        .is_some_and(|s| !s.trim().is_empty());
                    serde_json::json!({
                        "configured":llm.kind.is_openai()&&!llm.model.trim().is_empty()
                            &&moss_voice_providers::vision::endpoint(&llm.url).is_ok()&&key,
                        "url":llm.url,"model":llm.model,
                    })
                }
                None => serde_json::json!({"configured":false,"url":"","model":""}),
            };
            let ready = DesktopVisionFactory {
                host: self.host.clone(),
                local: self.local_vision.clone(),
            }
            .ready();
            let colab_active = self.active_colab(&settings).ok().flatten().is_some();
            let colab_override = serde_json::json!({
                "active": colab_active,
                "url": if colab_active { settings.builtin_colab.url.clone() } else { String::new() },
                "model": if colab_active { settings.builtin_colab.llm_model.clone() } else { String::new() },
            });
            // n74：可选的本地视觉模型及是否已下载（下载在“本地模型”里做）
            let local_models: Vec<Value> = moss_voice::local_models::CATALOG
                .iter()
                .filter(|spec| spec.kind == moss_voice::local_models::ModelKind::Vision)
                .map(|spec| {
                    serde_json::json!({
                        "id": spec.id,
                        "label": spec.label,
                        "size": spec.download_size(),
                        "installed": self.models.path_if_installed(spec.id).is_some(),
                    })
                })
                .collect();
            serde_json::json!({"settings":vision,"key_configured":key_configured,"ready":ready,"dialog_llm":dialog_llm,"colab_override":colab_override,"local_models":local_models,"local_runtime":self.local_vision.status()})
        }

        pub fn vision_save(&self, input: &Value) -> Result<Value, CenterError> {
            use moss_voice::settings::BuiltinVision;
            let invalid = |reason: &str| CenterError::new("vision_invalid", 400, reason);
            let next: BuiltinVision =
                serde_json::from_value(input.get("settings").cloned().unwrap_or(Value::Null))
                    .map_err(|_| invalid("视觉配置格式错误"))?;
            if next.url.len() > 1024 || next.model.len() > 200 {
                return Err(invalid("模型名称或地址太长"));
            }
            let local = next.enabled && !next.use_dialog_llm && !next.local_model.is_empty();
            if !next.local_model.is_empty()
                && moss_voice::local_models::spec_of(
                    &next.local_model,
                    moss_voice::local_models::ModelKind::Vision,
                )
                .is_none()
            {
                return Err(invalid("未知的本地视觉模型"));
            }
            // n74：本地视觉只要求模型已下载，不需要地址与密钥
            if local {
                if self.models.path_if_installed(&next.local_model).is_none() {
                    return Err(invalid("请先在“本地模型”里下载这个视觉模型"));
                }
            } else if next.enabled && next.use_dialog_llm {
                let llm_ok = self
                    .settings()
                    .builtin_models
                    .llm
                    .as_ref()
                    .is_some_and(|llm| {
                        llm.kind.is_openai()
                            && !llm.model.trim().is_empty()
                            && moss_voice_providers::vision::endpoint(&llm.url).is_ok()
                    });
                if !llm_ok {
                    return Err(invalid(
                        "请先在模型设置里填写 OpenAI 兼容的对话模型地址与模型名，再用它看照片",
                    ));
                }
                let llm_key = self
                    .host
                    .db
                    .lock()
                    .ok()
                    .and_then(|db| db.get_secret("voice.llm.main").ok().flatten())
                    .is_some_and(|s| !s.trim().is_empty());
                if !llm_key {
                    return Err(invalid("请先保存对话大模型的密钥"));
                }
            } else if next.enabled
                && (next.model.trim().is_empty()
                    || moss_voice_providers::vision::endpoint(&next.url).is_err())
            {
                return Err(invalid(
                    "请填写有效的模型名称和 HTTPS 地址（或本地明确的 HTTP 地址）",
                ));
            }
            let action = input["key"]["action"].as_str().unwrap_or("keep");
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            let existing = db
                .get_secret("voice.vision.main")
                .map_err(db_failure)?
                .is_some_and(|s| !s.is_empty());
            let new_key = match action {
                "keep" => None,
                "set" => {
                    let key = input["key"]["value"].as_str().unwrap_or("");
                    if key.trim().is_empty() || key.len() > 8192 {
                        return Err(invalid("视觉模型密钥不能空或超过 8192 字节"));
                    }
                    Some(Some(key.to_owned()))
                }
                "clear" => Some(None),
                _ => return Err(invalid("密钥操作只能保留、更新或清除")),
            };
            let has_key = match &new_key {
                None => existing,
                Some(Some(_)) => true,
                Some(None) => false,
            };
            if next.enabled && !next.use_dialog_llm && !local && !has_key {
                return Err(invalid("启用视觉前必须保存对应模型密钥"));
            }
            let changes: Vec<(String, Option<String>)> = new_key
                .into_iter()
                .map(|key| ("voice.vision.main".to_owned(), key))
                .collect();
            commit_secrets_then_settings(&db, &changes, || {
                moss_voice::settings::update_voice(&self.settings_file(), |settings| {
                    settings.builtin_vision = next.clone();
                    Ok(())
                })
                .map_err(|_| CenterError::new("vision_save_failed", 500, "视觉配置保存失败"))
            })?;
            drop(db);
            self.sync_local_vision(true);
            Ok(self.vision_status())
        }

        /// 按当前设置配置本地视觉模型：选了已下载的本地模型就记下启动方式（`preload` 时后台加载），
        /// 否则停掉推理进程。服务启动 / 停止、保存视觉设置、删除模型后调用。
        fn sync_local_vision(&self, preload: bool) {
            let settings = self.settings();
            let colab_active = self.active_colab(&settings).ok().flatten().is_some();
            let vision = settings.builtin_vision;
            let launch = (!colab_active
                && vision.enabled
                && !vision.use_dialog_llm
                && !vision.local_model.is_empty())
            .then(|| self.models.path_if_installed(&vision.local_model))
            .flatten()
            .map(|dir| LocalVisionLaunch {
                root: self.models.root_for(&vision.local_model).to_path_buf(),
                model: vision.local_model.clone(),
                dir,
                mirror: settings.builtin_local.mirror,
            });
            self.local_vision.configure(launch);
            if preload && self.running() {
                self.local_vision.preload();
            }
        }

        /// A button press uses only an in-memory synthetic 8x8 image.
        /// It does not access a device or a computer camera.
        pub fn vision_test(&self) -> Result<Value, CenterError> {
            use moss_voice_server::vision::VisionFactory;
            self.sync_local_vision(false);
            let factory = DesktopVisionFactory {
                host: self.host.clone(),
                local: self.local_vision.clone(),
            };
            let settings = self.settings();
            let colab_active = self.active_colab(&settings).ok().flatten().is_some();
            let vision_setting = settings.builtin_vision;
            if !colab_active
                && vision_setting.enabled
                && !vision_setting.use_dialog_llm
                && !vision_setting.local_model.is_empty()
            {
                // 点测试时等本地模型加载完再测；加载失败直接说原因
                self.local_vision
                    .ensure()
                    .map_err(|error| CenterError::new("vision_local_failed", 502, error))?;
            }
            let missing = if self.settings().builtin_vision.use_dialog_llm {
                "请先启用视觉，并在模型设置里配好支持看图的对话模型与密钥"
            } else {
                "请先启用并保存视觉模型与密钥"
            };
            let session = factory
                .open()
                .ok_or_else(|| CenterError::new("vision_disabled", 409, missing))?;
            let mut output = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(8, 8)
                .write_to(&mut output, image::ImageFormat::Png)
                .map_err(|_| internal("生成内存测试图片失败"))?;
            let png = output.into_inner();
            let started = std::time::Instant::now();
            let result = tauri::async_runtime::block_on(session.provider.explain(
                "请仅用一句话描述此测试图像。",
                "image/png",
                &png,
            ));
            match result {
                Ok(text) if factory.still_current(session.generation) => Ok(serde_json::json!({
                    "ok":true,"elapsed_ms":started.elapsed().as_millis(),"preview":text.chars().take(160).collect::<String>(),
                })),
                Ok(_) => Err(CenterError::new(
                    "vision_settings_changed",
                    409,
                    "测试中配置发生变化，请重试",
                )),
                Err(error) => Err(CenterError::new(
                    error.code(),
                    502,
                    "视觉模型连接或返回内容异常，请核对模型和密钥",
                )),
            }
        }

        pub fn plugins_status(&self) -> Value {
            let settings = self.settings().builtin_plugins;
            let mut keys = serde_json::Map::new();
            for kind in ["weather", "search", "metaso", "rag"] {
                let name = format!("voice.plugin.{kind}");
                let found = self
                    .host
                    .db
                    .lock()
                    .ok()
                    .and_then(|db| db.get_secret(&name).ok().flatten())
                    .is_some_and(|value| !value.is_empty());
                keys.insert(kind.into(), Value::Bool(found));
            }
            serde_json::json!({"settings":settings,"keys":keys})
        }

        pub fn plugins_save(&self, input: &Value) -> Result<Value, CenterError> {
            use moss_voice::settings::BuiltinPlugins;
            use moss_voice_server::plugins::http;
            let invalid = |message: &str| CenterError::new("plugin_config_invalid", 400, message);
            let config: BuiltinPlugins =
                serde_json::from_value(input.get("settings").cloned().unwrap_or(Value::Null))
                    .map_err(|_| invalid("信息插件配置格式不正确"))?;
            if !["api_key", "bearer"].contains(&config.weather_auth.as_str()) {
                return Err(invalid("天气认证方式必须为 API Key 或 Bearer"));
            }
            if config.weather_enabled {
                http::validate_endpoint(&config.weather_host, false)
                    .map_err(|_| invalid("天气服务地址必须是合法 HTTPS 地址"))?;
                let lat = config
                    .weather_latitude
                    .ok_or_else(|| invalid("请设置天气地点纬度"))?;
                let lon = config
                    .weather_longitude
                    .ok_or_else(|| invalid("请设置天气地点经度"))?;
                if !lat.is_finite()
                    || !(-90.0..=90.0).contains(&lat)
                    || !lon.is_finite()
                    || !(-180.0..=180.0).contains(&lon)
                {
                    return Err(invalid("纬度经度不在允许范围"));
                }
            }
            if !(-720..=840).contains(&config.lunar_utc_offset_minutes) {
                return Err(invalid("农历时区偏移无效"));
            }
            if config.news_sources.len() > 10 {
                return Err(invalid("RSS 来源最多 10 个"));
            }
            let mut unique = std::collections::HashSet::new();
            for source in &config.news_sources {
                if source.id.is_empty()
                    || source.id.len() > 40
                    || !source
                        .id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    || !unique.insert(source.id.clone())
                {
                    return Err(invalid(
                        "RSS 来源编号必须唯一且仅含字母、数字、短横线和下划线",
                    ));
                }
                let is_newsnow =
                    matches!(source.kind, moss_voice::settings::NewsSourceKind::Newsnow);
                let url = http::validate_endpoint(&source.url, is_newsnow).map_err(|_| {
                    invalid("RSS 必须使用 HTTPS；NewsNow 可使用明确配置的本机/LAN 地址")
                })?;
                if url.query().is_some() {
                    return Err(invalid(
                        "新闻源地址不得包含查询参数，避免将密钥写入普通配置",
                    ));
                }
                if matches!(source.kind, moss_voice::settings::NewsSourceKind::Newsnow)
                    && (source.source_id.is_empty()
                        || source.source_id.len() > 80
                        || !source
                            .source_id
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                {
                    return Err(invalid("NewsNow 来源 ID 必须为字母、数字、短横线或下划线"));
                }
            }
            if config.news_enabled && config.news_sources.is_empty() {
                return Err(invalid("开启新闻插件前必须添加 RSS 来源"));
            }
            if config.search_enabled {
                let host = match config.search_provider {
                    moss_voice::settings::SearchProvider::Tavily => &config.search_host,
                    moss_voice::settings::SearchProvider::Metaso => &config.metaso_host,
                };
                http::validate_endpoint(host, false)
                    .map_err(|_| invalid("联网搜索必须使用 HTTPS 服务地址"))?;
            }
            if config.rag_enabled {
                http::validate_endpoint(&config.rag_host, true)
                    .map_err(|_| invalid("RAGFlow 服务地址必须为 HTTPS 或明确的本机/LAN 地址"))?;
                if config.rag_dataset_ids.is_empty()
                    || config.rag_dataset_ids.len() > 20
                    || config
                        .rag_dataset_ids
                        .iter()
                        .any(|id| id.trim().is_empty() || id.len() > 128)
                {
                    return Err(invalid("RAGFlow 数据集白名单格式不正确"));
                }
            }

            // Keep/Set/Clear: missing key input means keep; no returned secret.
            let keys = &input["keys"];
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            let mut operations = Vec::<(String, Option<String>)>::new();
            for (kind, enabled) in [
                ("weather", config.weather_enabled),
                (
                    "search",
                    config.search_enabled
                        && config.search_provider == moss_voice::settings::SearchProvider::Tavily,
                ),
                (
                    "metaso",
                    config.search_enabled
                        && config.search_provider == moss_voice::settings::SearchProvider::Metaso,
                ),
                ("rag", config.rag_enabled),
            ] {
                let name = format!("voice.plugin.{kind}");
                let command = keys[kind]["action"].as_str().unwrap_or("keep");
                let existing = db
                    .get_secret(&name)
                    .map_err(db_failure)?
                    .is_some_and(|k| !k.is_empty());
                let available = match command {
                    "keep" => existing,
                    "set" => {
                        let value = keys[kind]["value"].as_str().unwrap_or("");
                        if value.trim().is_empty() || value.len() > 4096 {
                            return Err(invalid("API 密钥格式不正确（不能空或超过4096字节）"));
                        }
                        operations.push((name, Some(value.to_owned())));
                        true
                    }
                    "clear" => {
                        operations.push((name, None));
                        false
                    }
                    _ => return Err(invalid("API 密钥操作必须为 keep/set/clear")),
                };
                if enabled && !available {
                    return Err(invalid("开启需要 API 密钥的插件前必须设置密钥"));
                }
            }

            // 持锁到提交完成：校验时看到的密钥状态不会被并发保存改掉
            commit_secrets_then_settings(&db, &operations, || {
                moss_voice::settings::update_voice(&self.settings_file(), |settings| {
                    settings.builtin_plugins = config.clone();
                    Ok(())
                })
                .map_err(|_| CenterError::new("plugin_settings_failed", 500, "插件配置保存失败"))
            })?;
            drop(db);
            Ok(self.plugins_status())
        }

        pub fn memory_list(&self) -> Result<Value, CenterError> {
            serde_json::to_value(self.memory.list().map_err(internal)?)
                .map_err(|_| internal("记忆列表序列化失败"))
        }
        pub fn memory_edit(
            &self,
            device_id: &str,
            revision: i64,
            summary: &str,
        ) -> Result<Value, CenterError> {
            let updated = self
                .memory
                .edit(device_id, revision, summary)
                .map_err(|e| CenterError::new("memory_invalid", 400, e))?;
            serde_json::to_value(updated.ok_or_else(|| {
                CenterError::new("memory_conflict", 409, "记忆已更新，请刷新后重试")
            })?)
            .map_err(|_| internal("保存结果序列化失败"))
        }
        pub fn memory_enable(&self, enabled: bool) -> Result<Value, CenterError> {
            self.memory
                .configure(enabled, || {
                    moss_voice::settings::update_voice(&self.settings_file(), |settings| {
                        settings.builtin_memory_enabled = enabled;
                        Ok(())
                    })
                })
                .map_err(|e| CenterError::new("memory_settings_failed", 500, e))?;
            Ok(serde_json::json!({"enabled": enabled}))
        }

        fn firmware_dir(&self) -> std::path::PathBuf {
            self.host.runtime_root.path.join("firmware")
        }

        pub fn firmware_status(&self) -> Result<Value, CenterError> {
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            let records = db
                .list_firmware_deliveries()
                .map_err(|_| internal("读取固件记录失败"))?;
            let artifacts = db
                .list_firmware_artifacts()
                .map_err(|_| internal("读取固件清单失败"))?;
            let devices=db.voice_devices().map_err(|_|internal("读取设备失败"))?.into_iter().filter(|d|d.is_allowed())
                .map(|d|serde_json::json!({"device_id":d.device_id,"name":d.name,"model":d.model,"firmware_version":d.firmware_version,"last_seen":d.last_seen}))
                .collect::<Vec<_>>();
            Ok(
                serde_json::json!({"enabled":true,"records":records,"artifacts":artifacts,"devices":devices,"now":now(),"offer_hours":FIRMWARE_OFFER_SECONDS/3600.0}),
            )
        }

        /// 导入用户选中的固件：必须是 ESP32 应用镜像，版本号与项目名取自固件头。
        pub fn firmware_import(&self, path: &std::path::Path) -> Result<Value, CenterError> {
            let invalid = |message: &str| CenterError::new("firmware_invalid", 400, message);
            let meta = std::fs::metadata(path).map_err(|_| invalid("无法读取所选文件"))?;
            if !meta.is_file()
                || meta.len() as usize > moss_voice_server::firmware::FIRMWARE_MAX_BYTES
            {
                return Err(invalid("请选择 16 MB 以内的固件文件"));
            }
            let bytes = std::fs::read(path).map_err(|_| invalid("无法读取所选文件"))?;
            let image = moss_voice_server::firmware::parse_app_image(&bytes).map_err(invalid)?;
            let sha = sha256_hex(&bytes);
            let dir = self.firmware_dir();
            std::fs::create_dir_all(&dir)
                .map_err(|e| internal(format!("创建固件目录失败：{e}")))?;
            let target = dir.join(format!("{sha}.bin"));
            if !target.exists() {
                let temp = dir.join(format!(".{sha}.part"));
                std::fs::write(&temp, &bytes)
                    .and_then(|()| std::fs::rename(&temp, &target))
                    .map_err(|e| {
                        let _ = std::fs::remove_file(&temp);
                        internal(format!("保存固件失败：{e}"))
                    })?;
            }
            let project: String = image.project.chars().take(32).collect();
            self.host
                .db
                .lock()
                .map_err(|_| internal("数据库锁失败"))?
                .add_firmware_artifact(&sha, &image.version, &project, bytes.len() as i64, now())
                .map_err(|_| internal("登记固件失败"))?;
            Ok(
                serde_json::json!({"sha256":sha,"version":image.version,"project":project,"size":bytes.len()}),
            )
        }

        /// 推送 / 取消 / 删除固件。推送必须逐台、明确确认，且固件版本比设备当前版本新（否则设备不会升级）。
        pub fn firmware_manage(&self, input: &Value) -> Result<Value, CenterError> {
            let invalid = || CenterError::new("firmware_invalid", 400, "固件操作格式无效");
            let object = input.as_object().ok_or_else(invalid)?;
            let action = input["action"].as_str().ok_or_else(invalid)?;
            let allowed: &[&str] = match action {
                "offer" => &["action", "device_id", "sha256", "confirmed"],
                "cancel" => &["action", "delivery_id"],
                "delete" => &["action", "sha256", "confirmed"],
                _ => return Err(invalid()),
            };
            if object.len() != allowed.len()
                || object.keys().any(|k| !allowed.contains(&k.as_str()))
            {
                return Err(invalid());
            }
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            match action {
                "offer" => {
                    if input["confirmed"].as_bool() != Some(true) {
                        return Err(invalid());
                    }
                    let device_id = input["device_id"].as_str().ok_or_else(invalid)?;
                    let sha = input["sha256"].as_str().ok_or_else(invalid)?;
                    let device = db
                        .voice_device(device_id)
                        .map_err(|_| internal("读取设备失败"))?
                        .filter(|d| d.is_allowed())
                        .ok_or_else(|| {
                            CenterError::new(
                                "firmware_device_invalid",
                                409,
                                "只能推送给已允许的设备",
                            )
                        })?;
                    let artifact = db
                        .list_firmware_artifacts()
                        .map_err(|_| internal("读取固件清单失败"))?
                        .into_iter()
                        .find(|a| a.sha256 == sha)
                        .ok_or_else(|| {
                            CenterError::new("firmware_missing", 404, "找不到这个固件")
                        })?;
                    if !moss_voice_server::firmware::is_newer(
                        &artifact.version,
                        &device.firmware_version,
                    ) {
                        return Err(CenterError::new(
                            "firmware_not_newer",
                            409,
                            "固件版本不比设备当前版本新，设备不会升级",
                        ));
                    }
                    let (delivery, token) = (format!("fw-{}", random_hex(16)?), random_hex(32)?);
                    db.create_firmware_offer(
                        &delivery,
                        device_id,
                        sha,
                        &artifact.version,
                        &token,
                        now() + FIRMWARE_OFFER_SECONDS,
                        now(),
                    )
                    .map_err(|_| internal("创建推送失败"))?;
                }
                "cancel" => {
                    let id = input["delivery_id"].as_str().ok_or_else(invalid)?;
                    if !db
                        .cancel_firmware_offer(id, now())
                        .map_err(|_| internal("取消推送失败"))?
                    {
                        return Err(CenterError::new(
                            "firmware_conflict",
                            409,
                            "这条推送已完成或已取消",
                        ));
                    }
                }
                "delete" => {
                    if input["confirmed"].as_bool() != Some(true) {
                        return Err(invalid());
                    }
                    let sha = input["sha256"].as_str().ok_or_else(invalid)?;
                    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(invalid());
                    }
                    if !db
                        .delete_firmware_artifact(sha, now())
                        .map_err(|_| internal("删除固件失败"))?
                    {
                        return Err(CenterError::new(
                            "firmware_in_use",
                            409,
                            "还有推送在用这个固件，请先取消推送",
                        ));
                    }
                    let _ = std::fs::remove_file(self.firmware_dir().join(format!("{sha}.bin")));
                }
                _ => {}
            }
            drop(db);
            self.firmware_status()
        }

        fn wakeword_dir(&self) -> std::path::PathBuf {
            self.host.runtime_root.path.join("wakeword")
        }

        /// n61：当前保存的自定义唤醒词；没保存过或文件损坏时用“你好 MOSS”。
        fn wakeword_word(&self) -> moss_voice_server::wake_assets::WakeWord {
            use moss_voice_server::wake_assets::{DEFAULT_THRESHOLD, WakeWord};
            std::fs::read(self.wakeword_dir().join("settings.json"))
                .ok()
                .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
                .and_then(|v| {
                    WakeWord::new(
                        v["text"].as_str()?,
                        v["command"].as_str()?,
                        v["threshold"].as_f64()?,
                    )
                    .ok()
                })
                .unwrap_or_else(|| {
                    WakeWord::new("你好 MOSS", "ni hao mo si", DEFAULT_THRESHOLD)
                        .expect("default wake word is valid")
                })
        }

        fn wakeword_base(&self) -> Option<Vec<u8>> {
            std::fs::read(self.wakeword_dir().join("base-assets.bin")).ok()
        }

        fn wakeword_write(&self, name: &str, bytes: &[u8]) -> Result<(), CenterError> {
            let dir = self.wakeword_dir();
            std::fs::create_dir_all(&dir)
                .map_err(|e| internal(format!("创建唤醒词目录失败：{e}")))?;
            let temp = dir.join(format!(".{name}.part"));
            std::fs::write(&temp, bytes)
                .and_then(|()| std::fs::rename(&temp, dir.join(name)))
                .map_err(|e| {
                    let _ = std::fs::remove_file(&temp);
                    internal(format!("保存失败：{e}"))
                })
        }

        /// n61 设备唤醒词：当前设置、基础资源包摘要、在线且已允许的设备及其固件是否支持网络更新资源包。
        pub fn wakeword_status(&self) -> Result<Value, CenterError> {
            let word = self.wakeword_word();
            let base = self
                .wakeword_base()
                .and_then(|b| moss_voice_server::wake_assets::describe_base(&b).ok());
            let allowed: std::collections::HashMap<String, String> = self
                .host
                .db
                .lock()
                .map_err(|_| internal("数据库锁失败"))?
                .voice_devices()
                .map_err(|_| internal("读取设备失败"))?
                .into_iter()
                .filter(|d| d.is_allowed())
                .map(|d| (d.device_id, d.name))
                .collect();
            let sessions = self
                .server
                .lock()
                .map_err(|_| internal("语音服务锁失败"))?
                .as_ref()
                .map(|s| s.status().sessions)
                .unwrap_or_default();
            let mut seen = std::collections::BTreeSet::new();
            let settings = moss_voice::settings::load(&self.settings_file());
            let current_marker = current_screen_pack_marker(&settings);
            let prefs = settings.builtin_device_prefs;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let devices = sessions
                .iter()
                .filter(|r| allowed.contains_key(&r.device_id) && seen.insert(r.device_id.clone()))
                .map(|r| {
                    let screen = (r.screen_monochrome != Some(true))
                        .then_some(r.screen_width.zip(r.screen_height))
                        .flatten();
                    let pref = prefs.get(&r.device_id);
                    let installed = pref
                        .and_then(|p| p.screen_pack.as_deref())
                        .is_some_and(moss_voice_server::screen_pack::marker_installed);
                    let pending = pref.and_then(|p| p.screen_pack_pending.as_deref());
                    let pending_at = pref.and_then(|p| p.screen_pack_pending_at).unwrap_or(0);
                    let expired = pending.is_some()
                        && now.saturating_sub(pending_at)
                            > moss_voice_server::wake_assets::STAGED_TTL.as_secs();
                    // 2026-11-09：画风包状态以“设备整包下载完”为准，推送了没下载不算已刷；
                    // 装的是别的画风时为 style_outdated（开机待命画面还是旧画风，要重刷）
                    let installed_marker = pref.and_then(|p| p.screen_pack.as_deref());
                    let screen_pack_state = match (pending, expired) {
                        (Some(_), true) => "download_expired",
                        (Some(_), false) => "pending_download",
                        (None, _) if installed_marker == Some(current_marker.as_str()) => "installed",
                        (None, _) if installed => "style_outdated",
                        (None, _) => "none",
                    };
                    serde_json::json!({
                        "device_id":r.device_id,"name":allowed.get(&r.device_id),
                        "supported":r.device_tools.iter().any(|t|t=="self.assets.set_download_url"),
                        "screen_pack_size":screen.map(|(w,h)|[w,h]),
                        "screen_pack":installed,
                        "screen_pack_state":screen_pack_state,
                        "screen_pack_style":installed_marker.filter(|_| installed).and_then(|m| m.split(':').nth(1)),
                        "screen_pack_pending_with_pack":pending.map(|p| p != moss_voice::settings::SCREEN_PACK_NONE),
                        "wake_target":pref.and_then(|p| p.wake_target.clone()),
                    })
                })
                .collect::<Vec<_>>();
            Ok(serde_json::json!({
                "word":{"text":word.text,"command":word.command,"threshold":word.threshold},
                "base":base,"devices":devices,
                "running":self.server.lock().map_err(|_|internal("语音服务锁失败"))?.is_some(),
            }))
        }

        /// 导入基础资源包（官方网页生成器导出的 assets.bin）：推送时保留它的字体、表情和背景，只换唤醒词。
        pub fn wakeword_import_base(&self, path: &std::path::Path) -> Result<Value, CenterError> {
            use moss_voice_server::wake_assets::{ASSETS_MAX_BYTES, describe_base};
            let invalid = |message: &str| CenterError::new("wakeword_invalid", 400, message);
            let meta = std::fs::metadata(path).map_err(|_| invalid("无法读取所选文件"))?;
            if !meta.is_file() || meta.len() as usize > ASSETS_MAX_BYTES {
                return Err(invalid("请选择 8 MB 以内的资源包（assets.bin）"));
            }
            let bytes = std::fs::read(path).map_err(|_| invalid("无法读取所选文件"))?;
            let summary = describe_base(&bytes).map_err(|e| invalid(&e))?;
            self.wakeword_write("base-assets.bin", &bytes)?;
            Ok(summary)
        }

        /// 保存唤醒词 / 移除基础资源包 / 逐台确认推送（自定义唤醒词或恢复“你好小智”）。
        /// 自动保持全屏画风（2026-11-09 用户决定，开关默认关）：开着时，挑一台空闲、彩屏、支持在线更新资源包、
        /// 还没装好画风包（或包版本过旧）的在线小智自动推送，沿用它上次的唤醒词。加工中、有待批准请求时不推；
        /// 每台一天最多自动推一次（失败也不反复重试），结果写进“记录与诊断”（来源 MOSS 软件）。返回推送了哪台。
        fn screen_pack_auto_tick(&self, now: u64) -> Option<String> {
            let settings = self.settings();
            if !settings.builtin_local.screen_pack_auto_keep {
                return None;
            }
            if self.host.api_service.device_work_in_progress()
                || self
                    .host
                    .api_service
                    .pending_approval_display_summary()
                    .is_some()
            {
                return None;
            }
            let sessions = self
                .server
                .lock()
                .ok()?
                .as_ref()
                .map(|server| server.status().sessions)?;
            let allowed: std::collections::HashMap<String, String> = self
                .host
                .db
                .lock()
                .ok()?
                .voice_devices()
                .ok()?
                .into_iter()
                .filter(|d| d.is_allowed())
                .map(|d| (d.device_id, d.name))
                .collect();
            let current_marker = current_screen_pack_marker(&settings);
            let candidate = sessions.into_iter().find(|r| {
                allowed.contains_key(&r.device_id)
                    && r.state == "idle"
                    && r.device_tools
                        .iter()
                        .any(|t| t == "self.assets.set_download_url")
                    && r.screen_monochrome != Some(true)
                    && r.screen_width.is_some()
                    && screen_pack_auto_due(
                        settings.builtin_device_prefs.get(&r.device_id),
                        &current_marker,
                        now,
                    )
            })?;
            let device_id = candidate.device_id;
            let name = allowed
                .get(&device_id)
                .filter(|name| !name.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| device_id.clone());
            let target = match settings
                .builtin_device_prefs
                .get(&device_id)
                .and_then(|p| p.wake_target.as_deref())
            {
                Some("custom") => "custom",
                _ => "default",
            };
            // 先记下这次尝试，失败也不会每 20 秒重推、反复重启设备
            let _ = moss_voice::settings::update_voice(&self.settings_file(), |s| {
                s.builtin_device_prefs
                    .entry(device_id.clone())
                    .or_default()
                    .screen_pack_auto_at = Some(now);
                Ok(())
            });
            let record = match self.push_device_assets(&device_id, target, true) {
                Ok(size) => EventRecord::info(
                    DiagCategory::Firmware,
                    format!(
                        "自动保持全屏画风：已给 {name} 推送全屏画风（{:.1} MB），它会重启并下载",
                        size as f64 / 1_048_576.0
                    ),
                )
                .hint("下载完设备列表显示“已刷全屏画风”，之后上电就是全屏画面；期间不要断电"),
                Err(error) => EventRecord::warn(
                    DiagCategory::Firmware,
                    format!("自动保持全屏画风：给 {name} 推送没成功：{}", error.message),
                )
                .hint("一天后会再自动试一次；也可以在“设备 › 唤醒词”手动一键刷入"),
            };
            self.diagnostics
                .push_from(moss_voice_server::DiagSource::Moss, record);
            Some(device_id)
        }

        /// 给一台已允许的在线小智推送资源包（唤醒词，可带全屏画风）：生成、下发下载地址并让它重启，
        /// 设置里记为“等下载”。手动推送（逐台确认）与“自动保持全屏画风”共用；返回资源包字节数。
        fn push_device_assets(
            &self,
            device_id: &str,
            target_name: &str,
            with_screen: bool,
        ) -> Result<usize, CenterError> {
            use moss_voice_server::wake_assets::WakeTarget;
            let target = match target_name {
                "custom" => WakeTarget::Custom(self.wakeword_word()),
                _ => WakeTarget::Default,
            };
            let device_ok = self
                .host
                .db
                .lock()
                .map_err(|_| internal("数据库锁失败"))?
                .voice_device(device_id)
                .map_err(|_| internal("读取设备失败"))?
                .is_some_and(|d| d.is_allowed());
            if !device_ok {
                return Err(CenterError::new(
                    "wakeword_device_invalid",
                    409,
                    "只能推送给已允许的设备",
                ));
            }
            // n70 全屏画风：按这台设备的屏幕分辨率和当前画风生成，随唤醒词一起写进资源分区
            let screen = if with_screen {
                let (size, style) = {
                    let server = self.server.lock().map_err(|_| internal("语音服务锁失败"))?;
                    let server = server.as_ref().ok_or_else(|| {
                        CenterError::new("voice_builtin_stopped", 409, "内置语音服务未运行")
                    })?;
                    (
                        server.screen_pack_size(device_id),
                        server.screen_card_style(),
                    )
                };
                let size = size.ok_or_else(|| {
                            CenterError::new(
                                "screen_pack_unsupported",
                                409,
                                "这台设备没有上报彩屏分辨率（或是单色屏），不能刷全屏画风；可以取消勾选只推唤醒词",
                            )
                        })?;
                let extras = moss_voice_server::screen_pack::build_screen_extras(style, size)
                    .map_err(internal)?;
                Some((extras, style))
            } else {
                None
            };
            let package = moss_voice_server::wake_assets::build_package_with_screen(
                self.wakeword_base().as_deref(),
                &target,
                screen.as_ref().map(|(extras, _)| extras),
            )
            .map_err(|e| CenterError::new("wakeword_invalid", 400, e))?;
            let size = package.len();
            {
                let server = self.server.lock().map_err(|_| internal("语音服务锁失败"))?;
                let server = server.as_ref().ok_or_else(|| {
                    CenterError::new("voice_builtin_stopped", 409, "内置语音服务未运行")
                })?;
                server
                    .install_wake_assets(device_id, package)
                    .map_err(|e| CenterError::new("wakeword_device_unsupported", 409, e))?;
            }
            // 2026-11-09：先记为“已推送、等下载”，设备整包下载完（`assets_downloaded`）才算已装；
            // 不带画风的包下载完会把已装标记清掉。同时记下这次的唤醒词，“一键刷入画风”沿用它。
            let marker = screen
                .as_ref()
                .map(|(_, style)| moss_voice_server::screen_pack::pack_marker(*style))
                .unwrap_or_else(|| moss_voice::settings::SCREEN_PACK_NONE.to_owned());
            let wake_target = Some(target_name.to_owned());
            moss_voice::settings::update_voice(&self.settings_file(), |s| {
                let entry = s
                    .builtin_device_prefs
                    .entry(device_id.to_owned())
                    .or_default();
                entry.screen_pack_pending = Some(marker.clone());
                entry.screen_pack_pending_at = Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                );
                entry.wake_target = wake_target.clone();
                Ok(())
            })
            .map_err(|msg| CenterError::new("voice_settings_write_failed", 500, msg))?;
            Ok(size)
        }

        pub fn wakeword_manage(&self, input: &Value) -> Result<Value, CenterError> {
            use moss_voice_server::wake_assets::{WakeTarget, WakeWord};
            let invalid = || CenterError::new("wakeword_invalid", 400, "唤醒词操作格式无效");
            let object = input.as_object().ok_or_else(invalid)?;
            let action = input["action"].as_str().ok_or_else(invalid)?;
            let allowed: &[&str] = match action {
                "save" => &["action", "text", "command", "threshold"],
                "clear_base" => &["action", "confirmed"],
                "push" => &["action", "device_id", "target", "confirmed", "screen_pack"],
                _ => return Err(invalid()),
            };
            // n70：push 的 screen_pack 可省略（省略即不带全屏画风），其余字段必须齐全
            let optional = usize::from(action == "push" && !object.contains_key("screen_pack"));
            if object.len() + optional != allowed.len()
                || object.keys().any(|k| !allowed.contains(&k.as_str()))
            {
                return Err(invalid());
            }
            match action {
                "save" => {
                    let word = WakeWord::new(
                        input["text"].as_str().ok_or_else(invalid)?,
                        input["command"].as_str().ok_or_else(invalid)?,
                        input["threshold"].as_f64().ok_or_else(invalid)?,
                    )
                    .map_err(|e| CenterError::new("wakeword_invalid", 400, e))?;
                    let raw=serde_json::to_vec(&serde_json::json!({"text":word.text,"command":word.command,"threshold":word.threshold})).map_err(|_|internal("保存唤醒词失败"))?;
                    self.wakeword_write("settings.json", &raw)?;
                }
                "clear_base" => {
                    if input["confirmed"].as_bool() != Some(true) {
                        return Err(invalid());
                    }
                    match std::fs::remove_file(self.wakeword_dir().join("base-assets.bin")) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(internal(format!("移除基础资源包失败：{e}"))),
                    }
                }
                "push" => {
                    if input["confirmed"].as_bool() != Some(true) {
                        return Err(invalid());
                    }
                    let device_id = input["device_id"].as_str().ok_or_else(invalid)?;
                    let target = match input["target"].as_str() {
                        Some("custom") => WakeTarget::Custom(self.wakeword_word()),
                        Some("default") => WakeTarget::Default,
                        _ => return Err(invalid()),
                    };
                    let with_screen = match input.get("screen_pack") {
                        None => false,
                        Some(Value::Bool(b)) => *b,
                        Some(_) => return Err(invalid()),
                    };
                    let target_name = if matches!(target, WakeTarget::Default) {
                        "default"
                    } else {
                        "custom"
                    };
                    let size = self.push_device_assets(device_id, target_name, with_screen)?;
                    let mut status = self.wakeword_status()?;
                    status["pushed"] = serde_json::json!({"device_id":device_id,"size":size,"target":input["target"],"screen_pack":with_screen});
                    return Ok(status);
                }
                _ => {}
            }
            self.wakeword_status()
        }

        pub fn speaker_manage(&self, input: &Value) -> Result<Value, CenterError> {
            let invalid = || CenterError::new("speaker_invalid", 400, "声纹操作格式无效");
            let object = input.as_object().ok_or_else(invalid)?;
            let action = input["action"].as_str().ok_or_else(invalid)?;
            let allowed: &[&str] = match action {
                "status" | "cancel" => &["action"],
                "begin" => &["action", "device_id", "session_id", "name", "confirmed"],
                "delete" => &["action", "id", "revision", "confirmed"],
                "save_memory" => &["action", "id", "revision", "summary"],
                "personal_mode" => &["action", "enabled", "confirmed"],
                "begin_test" => &["action", "device_id", "session_id", "id", "confirmed"],
                "calibrate" | "reset_calibration" => &["action", "confirmed"],
                _ => return Err(invalid()),
            };
            if object.len() != allowed.len()
                || object.keys().any(|k| !allowed.contains(&k.as_str()))
            {
                return Err(invalid());
            }
            match action {
                "begin" => {
                    if input["confirmed"].as_bool() != Some(true) {
                        return Err(invalid());
                    }
                    let device = input["device_id"].as_str().ok_or_else(invalid)?;
                    let session = input["session_id"].as_str().ok_or_else(invalid)?;
                    let name = input["name"].as_str().ok_or_else(invalid)?;
                    let chat = self
                        .server
                        .lock()
                        .map_err(|_| internal("语音服务锁失败"))?
                        .as_ref()
                        .is_some_and(|s| {
                            let status = s.status();
                            status.mode == ServerMode::Chat
                                && status
                                    .sessions
                                    .iter()
                                    .any(|r| r.device_id == device && r.session_id == session)
                        });
                    if !chat {
                        return Err(CenterError::new(
                            "speaker_session_required",
                            409,
                            "请选择已连接的对话模式设备",
                        ));
                    }
                    let context = laser_contracts::CallerContext::Voice {
                        device_id: device.into(),
                        session_id: session.into(),
                    };
                    let permit = self
                        .host
                        .api_service
                        .external_voice_permit(&context)
                        .map_err(|_| {
                            CenterError::new("speaker_revoked", 403, "当前设备会话已撤销")
                        })?;
                    self.prepare_speaker_helper()?;
                    self.speaker
                        .begin(device, session, name, permit, true)
                        .map_err(|code| {
                            CenterError::new(
                                code,
                                409,
                                "登记无法启动，请检查会话、名称或正在进行的登记",
                            )
                        })?;
                }
                "cancel" => {
                    self.speaker
                        .cancel()
                        .map_err(|_| internal("取消声纹登记失败"))?;
                }
                // 简易标定（2026-10-14）：已登记的人各录三段测试音 → 计算阈值并自检 → 通过才开放识别
                "begin_test" => {
                    if input["confirmed"].as_bool() != Some(true) {
                        return Err(invalid());
                    }
                    let device = input["device_id"].as_str().ok_or_else(invalid)?;
                    let session = input["session_id"].as_str().ok_or_else(invalid)?;
                    let id = input["id"].as_str().ok_or_else(invalid)?;
                    let known = self
                        .host
                        .db
                        .lock()
                        .map_err(|_| internal("数据库锁失败"))?
                        .list_voice_speakers()
                        .map_err(|_| internal("读取声纹失败"))?
                        .iter()
                        .any(|r| r.id == id);
                    if !known {
                        return Err(CenterError::new(
                            "speaker_conflict",
                            409,
                            "这个人的声纹已不存在，请刷新",
                        ));
                    }
                    let chat = self
                        .server
                        .lock()
                        .map_err(|_| internal("语音服务锁失败"))?
                        .as_ref()
                        .is_some_and(|s| {
                            let status = s.status();
                            status.mode == ServerMode::Chat
                                && status
                                    .sessions
                                    .iter()
                                    .any(|r| r.device_id == device && r.session_id == session)
                        });
                    if !chat {
                        return Err(CenterError::new(
                            "speaker_session_required",
                            409,
                            "请选择已连接的对话模式设备",
                        ));
                    }
                    let context = laser_contracts::CallerContext::Voice {
                        device_id: device.into(),
                        session_id: session.into(),
                    };
                    let permit = self
                        .host
                        .api_service
                        .external_voice_permit(&context)
                        .map_err(|_| {
                            CenterError::new("speaker_revoked", 403, "当前设备会话已撤销")
                        })?;
                    self.prepare_speaker_helper()?;
                    self.speaker
                        .begin_test(device, session, id, permit, true)
                        .map_err(|code| {
                            CenterError::new(
                                code,
                                409,
                                "测试音采集无法启动，请检查会话或正在进行的登记",
                            )
                        })?;
                }
                "calibrate" => {
                    if input["confirmed"].as_bool() != Some(true) {
                        return Err(invalid());
                    }
                    let templates = self
                        .host
                        .db
                        .lock()
                        .map_err(|_| internal("数据库锁失败"))?
                        .native_voice_speaker_templates()
                        .map_err(|_| internal("读取声纹失败"))?
                        .into_iter()
                        .map(|t| SpeakerTemplate {
                            id: t.id,
                            revision: t.revision,
                            embedding: t.embedding,
                        })
                        .collect::<Vec<_>>();
                    let samples = self
                        .speaker_backend
                        .tests
                        .lock()
                        .map_err(|_| internal("标定数据不可用"))?
                        .iter()
                        .flat_map(|(id, clips)| {
                            clips.iter().map(|clip| CalibrationSample {
                                speaker_id: id.clone(),
                                embedding: clip.clone(),
                            })
                        })
                        .collect::<Vec<_>>();
                    let report = moss_voice_server::person_context::calibrate(&templates, &samples)
                        .map_err(|reason| {
                            CenterError::new("speaker_calibration_failed", 409, reason)
                        })?;
                    let calibration = Calibration {
                        report,
                        directory: speaker_directory(
                            templates.iter().map(|t| (t.id.as_str(), t.revision)),
                        ),
                        calibrated_at: now(),
                    };
                    let text = serde_json::to_string(&calibration)
                        .map_err(|_| internal("标定结果序列化失败"))?;
                    self.host
                        .db
                        .lock()
                        .map_err(|_| internal("数据库锁失败"))?
                        .set_secret(SPEAKER_CALIBRATION_KEY, &text, now())
                        .map_err(|_| internal("保存标定结果失败"))?;
                    *self
                        .identity
                        .calibration
                        .lock()
                        .map_err(|_| internal("标定状态锁失败"))? = Some(calibration);
                    self.speaker_backend
                        .tests
                        .lock()
                        .map_err(|_| internal("标定数据不可用"))?
                        .clear();
                    self.personal.directory_changed();
                    self.context.speakers_invalidated();
                    if self.settings().builtin_person_memory_enabled {
                        self.restart_if_running()?;
                    }
                }
                "reset_calibration" => {
                    if input["confirmed"].as_bool() != Some(true) {
                        return Err(invalid());
                    }
                    self.host
                        .db
                        .lock()
                        .map_err(|_| internal("数据库锁失败"))?
                        .delete_secret(SPEAKER_CALIBRATION_KEY)
                        .map_err(|_| internal("清除标定结果失败"))?;
                    *self
                        .identity
                        .calibration
                        .lock()
                        .map_err(|_| internal("标定状态锁失败"))? = None;
                    self.speaker_backend
                        .tests
                        .lock()
                        .map_err(|_| internal("标定数据不可用"))?
                        .clear();
                    self.personal.directory_changed();
                    self.context.speakers_invalidated();
                    if self.settings().builtin_person_memory_enabled {
                        self.restart_if_running()?;
                    }
                }
                "delete" => {
                    if input["confirmed"].as_bool() != Some(true) {
                        return Err(invalid());
                    }
                    let id = input["id"].as_str().ok_or_else(invalid)?;
                    let revision = input["revision"].as_i64().ok_or_else(invalid)?;
                    if id.is_empty()
                        || id.len() > 64
                        || !id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                        || revision < 1
                    {
                        return Err(invalid());
                    }
                    if !self
                        .personal
                        .person_deleted(id, || {
                            self.host
                                .db
                                .lock()
                                .map_err(|_| "数据库锁失败".to_owned())?
                                .delete_voice_speaker_if_revision(id, revision)
                                .map_err(db_error)
                        })
                        .map_err(internal)?
                    {
                        return Err(CenterError::new(
                            "speaker_conflict",
                            409,
                            "声纹记录已变化，请刷新",
                        ));
                    }
                    self.context.person_changed(id);
                    if let Ok(mut tests) = self.speaker_backend.tests.lock() {
                        tests.remove(id);
                    }
                }
                "save_memory" => {
                    let id = input["id"].as_str().ok_or_else(invalid)?;
                    let revision = input["revision"].as_i64().ok_or_else(invalid)?;
                    let text = input["summary"].as_str().ok_or_else(invalid)?;
                    let row = self
                        .host
                        .db
                        .lock()
                        .map_err(|_| internal("数据库锁失败"))?
                        .list_voice_speakers()
                        .map_err(|_| internal("读取声纹失败"))?
                        .into_iter()
                        .find(|r| r.id == id)
                        .ok_or_else(invalid)?;
                    let scope =
                        PersonScope::candidate(&row.id, row.revision, self.personal.generation())
                            .ok_or_else(invalid)?;
                    if !self
                        .personal
                        .operator_edit(&scope, revision, text)
                        .map_err(|_| invalid())?
                    {
                        return Err(CenterError::new(
                            "speaker_memory_conflict",
                            409,
                            "个人摘要已改变，请刷新后重试",
                        ));
                    }
                }
                "personal_mode" => {
                    let enabled = input["enabled"].as_bool().ok_or_else(invalid)?;
                    input["confirmed"].as_bool().ok_or_else(invalid)?;
                    if enabled
                        && (input["confirmed"].as_bool() != Some(true)
                            || self.identity.valid_calibration().is_none())
                    {
                        return Err(CenterError::new(
                            "speaker_calibration_required",
                            409,
                            "请先完成声纹简易标定（登记名单变化后需重新标定）",
                        ));
                    }
                    if enabled {
                        self.prepare_speaker_helper()?;
                    }
                    self.personal.directory_changed();
                    self.context.speakers_invalidated();
                    moss_voice::settings::update_voice(&self.settings_file(), |settings| {
                        settings.builtin_person_memory_enabled = enabled;
                        Ok(())
                    })
                    .map_err(internal)?;
                    self.restart_if_running()?;
                }
                _ => {}
            }
            let rows = self
                .host
                .db
                .lock()
                .map_err(|_| internal("数据库锁失败"))?
                .list_voice_speakers()
                .map_err(|_| internal("读取声纹失败"))?;
            let rows = {
                let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
                rows.into_iter()
                    .map(|row| {
                        let (summary, revision) = db
                            .voice_person_summary(&row.id)
                            .map_err(|_| internal("读取个人记忆失败"))?
                            .unwrap_or((String::new(), 0));
                        let mut value = serde_json::to_value(row)
                            .map_err(|_| internal("声纹元数据序列化失败"))?;
                        value["memory_summary"] = Value::String(summary);
                        value["memory_revision"] = Value::from(revision);
                        Ok(value)
                    })
                    .collect::<Result<Vec<_>, CenterError>>()?
            };
            let sessions = self
                .server
                .lock()
                .map_err(|_| internal("语音服务锁失败"))?
                .as_ref()
                .map(|s| s.status().sessions)
                .unwrap_or_default()
                .into_iter()
                .map(|r| serde_json::json!({"device_id":r.device_id,"session_id":r.session_id}))
                .collect::<Vec<_>>();
            let valid = self.identity.valid_calibration();
            let calibration = match (
                &valid,
                self.identity
                    .calibration
                    .lock()
                    .ok()
                    .and_then(|c| c.clone()),
            ) {
                (Some(c), _) => {
                    serde_json::json!({"state":"valid","report":c.report,"calibrated_at":c.calibrated_at})
                }
                (None, Some(c)) => {
                    serde_json::json!({"state":"stale","report":c.report,"calibrated_at":c.calibrated_at})
                }
                (None, None) => serde_json::json!({"state":"none"}),
            };
            let test_clips = self
                .speaker_backend
                .tests
                .lock()
                .map(|tests| {
                    tests
                        .iter()
                        .map(|(id, clips)| (id.clone(), Value::from(clips.len())))
                        .collect::<serde_json::Map<_, _>>()
                })
                .unwrap_or_default();
            let warning = self
                .speaker_warning
                .lock()
                .ok()
                .and_then(|slot| slot.clone());
            Ok(
                serde_json::json!({"enrollment":self.speaker.status(),"speakers":rows,"sessions":sessions,"recognition_enabled":valid.is_some(),"personal_memory_enabled":valid.is_some()&&self.settings().builtin_person_memory_enabled,"calibration":calibration,"test_clips":test_clips,"warning":warning}),
            )
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
    }

    /// 启动内置语音服务时尝试扫描本地音乐目录；换电脑或拔盘导致目录不可用时只返回提示，不阻断内置语音主服务启动。
    fn scan_music_for_start(
        config: &moss_voice::settings::BuiltinMusic,
        chat_ready: bool,
    ) -> (
        Option<Arc<moss_voice_server::music::MusicIndex>>,
        Option<String>,
    ) {
        if !config.enabled || !chat_ready {
            return (None, None);
        }
        match moss_voice_server::music::MusicIndex::scan(std::path::Path::new(&config.directory)) {
            Ok(found) => (Some(Arc::new(found)), None),
            Err(_) => {
                let warning = "本地音乐目录当前不可用（换电脑或移动盘拔出时请重新选择音乐文件夹），本次启动已跳过本地音乐".to_owned();
                eprintln!("[moss-desktop] {warning}");
                (None, Some(warning))
            }
        }
    }

    /// 校验并清理 Agent 保存输入：
    /// 1. 关闭开关（`enabled = false`）时只检查长度与控制字符，不因旧电脑遗留的目录或命令路径拦住保存；
    /// 2. 开启状态下，若 CLI 路径明显来自异构系统，或沿用上次保存的旧路径但在本机已失效，自动清空切回自动读取；
    /// 3. 开启状态下，新填写或仍保留的手动路径及工作目录照常严格校验。
    fn normalize_agent_save(
        mut config: moss_voice::settings::BuiltinAgents,
        previous: &moss_voice::settings::BuiltinAgents,
        sensitive: &[std::path::PathBuf],
        detect: impl Fn(laser_host::voice_agent::Provider) -> Option<std::path::PathBuf>,
    ) -> Result<moss_voice::settings::BuiltinAgents, CenterError> {
        use laser_host::voice_agent::{Provider, executable, is_foreign_os_path, workspace};
        let invalid = |message: &str| CenterError::new("agent_config_invalid", 400, message);
        for text in [
            &config.workspace,
            &config.codex_executable,
            &config.claude_executable,
        ] {
            if text.len() > 1024 || text.chars().any(char::is_control) {
                return Err(invalid("Agent 路径最长 1024 字节且不能包含控制字符"));
            }
        }
        if !config.enabled {
            return Ok(config);
        }
        if config.workspace.trim().is_empty() {
            return Err(invalid("开启 Agent 前必须先填写工作目录"));
        }
        for (field, old_field, provider) in [
            (
                &mut config.codex_executable,
                &previous.codex_executable,
                Provider::Codex,
            ),
            (
                &mut config.claude_executable,
                &previous.claude_executable,
                Provider::ClaudeCode,
            ),
        ] {
            let trimmed = field.trim();
            if trimmed.is_empty() {
                continue;
            }
            let inherited_broken = trimmed == old_field.trim()
                && executable(std::path::Path::new(trimmed), provider).is_err();
            if is_foreign_os_path(trimmed) || inherited_broken {
                field.clear();
            }
        }
        if config.codex_executable.trim().is_empty()
            && config.claude_executable.trim().is_empty()
            && detect(Provider::Codex).is_none()
            && detect(Provider::ClaudeCode).is_none()
        {
            return Err(invalid(
                "未在本机自动检测到 Codex 或 Claude Code，请先安装或手动填写路径",
            ));
        }
        workspace(std::path::Path::new(&config.workspace), sensitive)
            .map_err(|_| invalid("工作目录必须为现有、独立、非敏感的本地目录"))?;
        if !config.codex_executable.trim().is_empty() {
            executable(
                std::path::Path::new(config.codex_executable.trim()),
                Provider::Codex,
            )
            .map_err(|_| invalid("Codex 路径无效：请填写本机原生可执行文件，或留空自动读取"))?;
        }
        if !config.claude_executable.trim().is_empty() {
            executable(
                std::path::Path::new(config.claude_executable.trim()),
                Provider::ClaudeCode,
            )
            .map_err(|_| {
                invalid("Claude Code 路径无效：请填写本机原生可执行文件，或留空自动读取")
            })?;
        }
        Ok(config)
    }

    #[cfg(test)]
    mod local_vision_tests {
        use super::*;

        /// 用同样大小的稀疏文件模拟“已下载”的本地视觉模型（不真的启动推理进程）。
        fn fake_install(builtin: &BuiltinVoice, id: &str) -> std::path::PathBuf {
            let spec = moss_voice::local_models::spec(id).unwrap();
            let dir = builtin.models.root_for(id).join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::File::create(dir.join(spec.path))
                .unwrap()
                .set_len(spec.size)
                .unwrap();
            for extra in spec.extras {
                std::fs::File::create(dir.join(extra.path))
                    .unwrap()
                    .set_len(extra.size)
                    .unwrap();
            }
            assert!(
                builtin.models.path_if_installed(id).is_some(),
                "模拟安装应被识别为已下载"
            );
            dir
        }

        /// 2026-11-09：激光后端的事件写进同一本记录，语音中心事件列表里来源标为“激光雕刻”。
        #[test]
        fn laser_events_reach_voice_center_with_laser_source() {
            let temp = tempfile::tempdir().unwrap();
            let host = Arc::new(
                laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
            );
            let builtin = init(Some(host.clone())).0.unwrap();
            let closed = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let body = serde_json::json!({
                "connection_mode": "network", "network_host": "127.0.0.1",
                "network_telnet_port": closed, "network_timeout": 0.5,
            })
            .to_string();
            let request = format!(
                "POST /api/connection/check HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            host.api_service.handle_http(request.as_bytes());
            let events = builtin.status()["events"].as_array().unwrap().clone();
            let laser = events
                .iter()
                .find(|event| event["source"] == "laser")
                .expect("握手失败应写进语音中心事件列表");
            assert_eq!(laser["level"], "warn");
            assert_eq!(laser["category"], "device");
            assert!(
                laser["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("连接雕刻机失败"),
                "{laser}"
            );
            assert!(laser["hint"].as_str().is_some());
        }

        /// n74：没选本地模型时不启动；模型文件不全时启动失败并记下原因；停用后回到空闲。
        #[test]
        fn local_vision_runtime_reports_missing_launch_and_files() {
            let runtime = LocalVision::default();
            assert!(runtime.ensure().unwrap_err().contains("没有选用"));
            assert_eq!(runtime.status()["state"], "idle");
            let temp = tempfile::tempdir().unwrap();
            runtime.configure(Some(LocalVisionLaunch {
                model: "qwen3_vl_2b".into(),
                dir: temp.path().join("qwen3_vl_2b"),
                root: temp.path().to_path_buf(),
                mirror: None,
            }));
            let error = runtime.ensure().unwrap_err();
            assert!(error.contains("还没下载完整"), "{error}");
            assert_eq!(runtime.status()["state"], "failed");
            assert_eq!(runtime.status()["model"], "qwen3_vl_2b");
            runtime.configure(None);
            assert_eq!(runtime.status()["state"], "idle");
        }

        /// 本地视觉只要求模型已下载、不要密钥；未下载或未知模型拒绝保存；保存后视觉工厂就绪且不需要地址。
        #[test]
        fn vision_save_accepts_installed_local_model_without_key() {
            let temp = tempfile::tempdir().unwrap();
            let host = Arc::new(
                laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
            );
            let builtin = init(Some(host)).0.unwrap();
            let save = |model: &str, enabled: bool| {
                builtin.vision_save(&serde_json::json!({
                    "settings": {"enabled": enabled, "url": "", "model": "", "local_model": model},
                    "key": {"action": "keep"},
                }))
            };
            let unknown = save("qwen3_vl_99b", true).unwrap_err();
            assert_eq!(unknown.code, "vision_invalid");
            let missing = save("qwen3_vl_2b", true).unwrap_err();
            assert!(
                missing.message.contains("先在“本地模型”里下载"),
                "{}",
                missing.message
            );
            let status = builtin.vision_status();
            assert_eq!(
                status["local_models"].as_array().unwrap().len(),
                2,
                "2B 与 4B 两档"
            );
            assert_eq!(status["local_models"][0]["installed"], false);

            fake_install(&builtin, "qwen3_vl_2b");
            let status = save("qwen3_vl_2b", true).expect("已下载的本地视觉模型不需要地址与密钥");
            assert_eq!(status["settings"]["local_model"], "qwen3_vl_2b");
            assert_eq!(
                status["ready"], true,
                "选好已下载的本地模型即视为就绪（推理进程按需启动）"
            );
            assert_eq!(
                status["local_runtime"]["state"], "idle",
                "服务没运行时不预加载"
            );
            assert_eq!(status["local_runtime"]["model"], "qwen3_vl_2b");
            assert_eq!(
                builtin.local_vision.launch().map(|launch| launch.model),
                Some("qwen3_vl_2b".to_owned())
            );

            // 改回云端（未填地址、没有密钥）且关闭：不再保留本地启动方式
            builtin
                .vision_save(&serde_json::json!({
                    "settings": {"enabled": false, "url": "", "model": ""},
                    "key": {"action": "keep"},
                }))
                .unwrap();
            assert!(builtin.local_vision.launch().is_none());
            // 删除模型前先停掉（此处未运行，确认不会报错并能删掉）
            fake_install(&builtin, "qwen3_vl_2b");
            builtin.model_delete("qwen3_vl_2b").unwrap();
            assert!(builtin.models.path_if_installed("qwen3_vl_2b").is_none());
        }

        #[test]
        fn n80_colab_bridge_defaults_off_and_restores_on_disable() {
            let temp = tempfile::tempdir().unwrap();
            let host = Arc::new(
                laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
            );
            let builtin = init(Some(host)).0.unwrap();
            fake_install(&builtin, "qwen3_vl_2b");
            builtin
                .vision_save(&serde_json::json!({
                    "settings": {"enabled": true, "url": "", "model": "", "local_model": "qwen3_vl_2b"},
                    "key": {"action": "keep"},
                }))
                .unwrap();

            let initial = builtin.status();
            assert_eq!(initial["local"]["colab"]["enabled"], false);
            assert_eq!(initial["local"]["colab"]["active"], false);
            assert_eq!(initial["local"]["colab"]["cli"]["installed"], true);
            assert_eq!(initial["local"]["colab"]["cli"]["logged_in"], false);
            assert_eq!(
                builtin.local_vision.launch().map(|l| l.model),
                Some("qwen3_vl_2b".to_owned())
            );

            // 未登录 Colab CLI 且缺少手动地址或密钥时拒绝开启
            let err = builtin
                .local_save(&serde_json::json!({
                    "colab": {"enabled": true, "url": "https://abc-123.trycloudflare.com"}
                }))
                .unwrap_err();
            assert!(
                err.message.contains("尚未登录 Colab CLI"),
                "{}",
                err.message
            );

            // 模拟新用户未安装 Colab CLI：cli.installed == false -> 执行 install -> cli.installed == true
            builtin
                .colab_cli_installed_for_test
                .store(false, std::sync::atomic::Ordering::Release);
            let uninstalled = builtin.status();
            assert_eq!(uninstalled["local"]["colab"]["cli"]["installed"], false);
            assert_eq!(uninstalled["local"]["colab"]["cli"]["logged_in"], false);
            let installed_after = builtin
                .local_save(&serde_json::json!({
                    "colab_cli": {"action": "install"}
                }))
                .unwrap();
            assert_eq!(installed_after["local"]["colab"]["cli"]["installed"], true);
            assert_eq!(installed_after["local"]["colab"]["cli"]["logged_in"], false);

            // 本机保存与清除 550W 参考音频（仅存本机，云端启动时自动同步）
            use base64::Engine as _;
            let mut fake_wav = b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec();
            fake_wav.resize(128, 0);
            let wav_b64 = base64::engine::general_purpose::STANDARD.encode(&fake_wav);
            let with_wav = builtin
                .local_save(&serde_json::json!({
                    "colab_cli": {
                        "action": "set_ref_audio",
                        "file_name": "moss_550w.wav",
                        "wav_base64": wav_b64
                    }
                }))
                .unwrap();
            assert_eq!(
                with_wav["local"]["colab"]["cli"]["ref_audio_configured"],
                true
            );
            assert_eq!(
                with_wav["local"]["colab"]["cli"]["ref_audio_name"],
                "moss_550w.wav"
            );
            let cleared_wav = builtin
                .local_save(&serde_json::json!({
                    "colab_cli": {"action": "clear_ref_audio"}
                }))
                .unwrap();
            assert_eq!(
                cleared_wav["local"]["colab"]["cli"]["ref_audio_configured"],
                false
            );

            // 通过 Colab CLI 登录：login_start -> login_complete 后显示已登录，此时即使不填地址和密钥也允许开启并自动拉起云端 T4
            let login_started = builtin
                .local_save(&serde_json::json!({
                    "colab_cli": {"action": "login_start"}
                }))
                .unwrap();
            assert!(
                login_started["local"]["colab"]["cli"]["auth_url"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("https://accounts.google.com/")
            );
            let logged_in_status = builtin
                .local_save(&serde_json::json!({
                    "colab_cli": {"action": "login_complete", "code": "mock-valid-auth-code"}
                }))
                .unwrap();
            assert_eq!(logged_in_status["local"]["colab"]["cli"]["logged_in"], true);
            assert_eq!(
                logged_in_status["local"]["colab"]["cli"]["account_hint"],
                "t***r@gmail.com"
            );

            let auto_on = builtin
                .local_save(&serde_json::json!({
                    "colab": {"enabled": true, "url": ""}
                }))
                .unwrap();
            assert_eq!(auto_on["local"]["colab"]["enabled"], true);
            assert_eq!(
                auto_on["local"]["colab"]["cli"]["runner_state"], "starting",
                "已登录 Colab CLI 时开启开关应自动在后台拉起云端 T4"
            );

            // 退出 Colab CLI 登录后回到未登录状态
            let logged_out = builtin
                .local_save(&serde_json::json!({
                    "colab_cli": {"action": "logout"},
                    "colab": {"enabled": false}
                }))
                .unwrap();
            assert_eq!(logged_out["local"]["colab"]["cli"]["logged_in"], false);

            // 填入隧道地址与密钥并开启：临时接管大模型、看图与合成，暂停本机视觉推理进程预热
            let on_status = builtin
                .local_save(&serde_json::json!({
                    "colab": {
                        "enabled": true,
                        "url": "https://abc-123.trycloudflare.com",
                        "api_key": "moss-test-colab-secret",
                        "llm_model": "qwen3-vl-8b",
                        "tts_model": "qwen3-tts",
                        "tts_voice": "moss",
                        "tts_speed": 1.0,
                    }
                }))
                .unwrap();
            assert_eq!(on_status["local"]["colab"]["enabled"], true);
            assert_eq!(on_status["local"]["colab"]["active"], true);
            assert_eq!(on_status["local"]["colab"]["key_configured"], true);
            assert_eq!(
                on_status["local"]["colab"]["url"],
                "https://abc-123.trycloudflare.com/v1"
            );
            assert_eq!(on_status["local"]["ready"]["llm"], true);
            assert_eq!(on_status["local"]["ready"]["tts"], true);
            assert!(
                builtin.local_vision.launch().is_none(),
                "Colab 接管看图期间不保留本机视觉模型启动项"
            );
            let v_on = builtin.vision_status();
            assert_eq!(v_on["ready"], true);
            assert_eq!(v_on["colab_override"]["active"], true);
            assert_eq!(v_on["colab_override"]["model"], "qwen3-vl-8b");
            let raw_settings =
                std::fs::read_to_string(temp.path().join("settings.json")).unwrap_or_default();
            assert!(
                !raw_settings.contains("moss-test-colab-secret"),
                "Colab 密钥只存 app_secrets，绝不写入 settings.json"
            );

            // 关闭开关：立即恢复原有的本地视觉与大模型/合成状态，且保留已存的地址与密钥供下次直接开
            let off_status = builtin
                .local_save(&serde_json::json!({
                    "colab": {"enabled": false}
                }))
                .unwrap();
            assert_eq!(off_status["local"]["colab"]["enabled"], false);
            assert_eq!(off_status["local"]["colab"]["active"], false);
            assert_eq!(off_status["local"]["colab"]["key_configured"], true);
            assert_eq!(off_status["local"]["ready"]["llm"], false);
            assert_eq!(
                builtin.local_vision.launch().map(|l| l.model),
                Some("qwen3_vl_2b".to_owned()),
                "关闭 Colab 联动后立即恢复原有的本地视觉模型"
            );
            let v_off = builtin.vision_status();
            assert_eq!(v_off["colab_override"]["active"], false);
            assert_eq!(v_off["settings"]["local_model"], "qwen3_vl_2b");
        }
    }

    #[cfg(test)]
    mod device_switch_tests {
        use super::*;
        use laser_host::voice_agent::Provider;
        use moss_voice::models::ProviderKind;
        use moss_voice::settings::{BuiltinAgents, BuiltinModels, BuiltinMusic, BuiltinProvider};

        #[test]
        fn agent_save_allows_disabling_with_stale_paths_and_clears_inherited_dead_cli() {
            let temp = tempfile::tempdir().unwrap();
            let runtime = temp.path().join("runtime");
            let work = temp.path().join("work");
            std::fs::create_dir_all(&runtime).unwrap();
            std::fs::create_dir_all(&work).unwrap();
            let sensitive = [runtime];

            let old = BuiltinAgents {
                enabled: true,
                workspace: "C:\\old-device\\agent-work".into(),
                codex_executable: "C:\\old-device\\codex.exe".into(),
                claude_executable: "/Users/old-user/.local/bin/claude".into(),
            };

            // 1. 换电脑后直接关掉开关点保存：不被旧工作目录或旧命令路径拦住
            let mut turn_off = old.clone();
            turn_off.enabled = false;
            let saved_off =
                normalize_agent_save(turn_off.clone(), &old, &sensitive, |_| None).unwrap();
            assert!(!saved_off.enabled);
            assert_eq!(saved_off.workspace, turn_off.workspace);

            // 2. 换电脑后保持开启、只改新电脑的工作目录：沿用的旧失效 CLI 路径自动清空并走自动读取
            let mut migrated = old.clone();
            migrated.workspace = work.display().to_string();
            let detected_claude = temp.path().join("claude-auto");
            let saved_on = normalize_agent_save(migrated, &old, &sensitive, |kind| {
                (kind == Provider::ClaudeCode).then(|| detected_claude.clone())
            })
            .unwrap();
            assert!(saved_on.enabled);
            assert!(
                saved_on.codex_executable.is_empty(),
                "旧设备的失效 Codex 路径应清空回退自动读取"
            );
            assert!(
                saved_on.claude_executable.is_empty(),
                "旧账户的失效 Claude 路径应清空回退自动读取"
            );

            // 3. 开启状态下若用户新填了一个不存在的手动路径，照常报错拦截
            let freshly_typed_bad = BuiltinAgents {
                enabled: true,
                workspace: work.display().to_string(),
                codex_executable: temp
                    .path()
                    .join("nonexistent-codex.exe")
                    .display()
                    .to_string(),
                claude_executable: String::new(),
            };
            let err = normalize_agent_save(
                freshly_typed_bad,
                &BuiltinAgents::default(),
                &sensitive,
                |_| Some(detected_claude.clone()),
            )
            .unwrap_err();
            assert_eq!(err.code, "agent_config_invalid");

            // 4. 开启状态下若工作目录仍是旧电脑不存在的路径，照常报错拦截
            let stale_work = BuiltinAgents {
                enabled: true,
                workspace: temp.path().join("missing-work").display().to_string(),
                codex_executable: String::new(),
                claude_executable: String::new(),
            };
            assert!(
                normalize_agent_save(stale_work, &old, &sensitive, |_| Some(
                    detected_claude.clone()
                ))
                .is_err()
            );
        }

        #[test]
        fn builtin_voice_starts_cleanly_when_migrated_music_dir_and_speaker_model_are_missing() {
            let temp = tempfile::tempdir().unwrap();
            let host = Arc::new(
                laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
            );
            let ws_port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let http_port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();

            // 模拟从旧电脑拷来的数据库与配置：配齐云端对话三件套、开启了不存在的旧音乐目录、开启了声纹按人记忆但新电脑还没装声纹模型
            {
                let db = host.db.lock().unwrap();
                db.set_secret("voice.llm.main", "sk-test-llm", 1.0).unwrap();
                db.set_secret("voice.asr.main", "sk-test-asr", 1.0).unwrap();
                db.set_secret("voice.tts.main", "sk-test-tts", 1.0).unwrap();

                let mut v1 = vec![0.0f32; 192];
                v1[0] = 1.0;
                let mut v2 = vec![0.0f32; 192];
                v2[1] = 1.0;
                db.enroll_voice_speaker(
                    "sp-a",
                    "甲",
                    SPEAKER_MODEL,
                    &[v1.clone(), v1.clone(), v1],
                    1.0,
                )
                .unwrap();
                db.enroll_voice_speaker(
                    "sp-b",
                    "乙",
                    SPEAKER_MODEL,
                    &[v2.clone(), v2.clone(), v2],
                    1.0,
                )
                .unwrap();
                let cal = Calibration {
                    report: CalibrationReport {
                        threshold: 0.72,
                        margin: 0.05,
                        people: 2,
                        genuine_trials: 6,
                        genuine_accepted: 6,
                        impostor_trials: 6,
                        false_accepts: 0,
                        max_impostor_score: 0.2,
                    },
                    directory: speaker_directory([("sp-a", 1), ("sp-b", 1)].into_iter()),
                    calibrated_at: 2.0,
                };
                db.set_secret(
                    SPEAKER_CALIBRATION_KEY,
                    &serde_json::to_string(&cal).unwrap(),
                    2.0,
                )
                .unwrap();
            }

            let mut voice_settings = moss_voice::VoiceSettings {
                builtin_ws_port: ws_port,
                builtin_http_port: http_port,
                ..Default::default()
            };
            voice_settings.builtin_models = BuiltinModels {
                llm: Some(BuiltinProvider {
                    kind: ProviderKind::Openai,
                    url: "https://llm.example.test/v1".into(),
                    model: "gpt-test".into(),
                    voice: String::new(),
                    speed: 1.0,
                }),
                asr: Some(BuiltinProvider {
                    kind: ProviderKind::Openai,
                    url: "https://asr.example.test/v1/audio/transcriptions".into(),
                    model: "whisper-1".into(),
                    voice: String::new(),
                    speed: 1.0,
                }),
                tts: Some(BuiltinProvider {
                    kind: ProviderKind::Openai,
                    url: "https://tts.example.test/v1/audio/speech".into(),
                    model: "tts-1".into(),
                    voice: "alloy".into(),
                    speed: 1.0,
                }),
            };
            voice_settings.builtin_music = BuiltinMusic {
                enabled: true,
                directory: temp
                    .path()
                    .join("unplugged-old-music-drive")
                    .display()
                    .to_string(),
            };
            voice_settings.builtin_person_memory_enabled = true;
            moss_voice::settings::save(&temp.path().join("settings.json"), &voice_settings)
                .unwrap();

            let state = init(Some(host));
            let builtin = state.0.unwrap();

            let status = builtin
                .start()
                .expect("旧音乐目录缺失与声纹模型未安装不应阻断内置语音主服务启动");
            assert_eq!(status["running"], true);
            assert_eq!(status["mode"], "chat");

            let music_view = builtin.music_status();
            assert!(
                music_view["warning"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("本地音乐目录当前不可用"),
                "{music_view}"
            );

            let speaker_view = builtin
                .speaker_manage(&serde_json::json!({"action": "status"}))
                .unwrap();
            assert!(
                speaker_view["warning"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("声纹按人记忆暂未生效"),
                "{speaker_view}"
            );

            // n68：降级提示出现在状态里，也写进诊断记录（警告级别、带分类与下一步）
            let warnings = status["warnings"].as_array().unwrap();
            assert_eq!(warnings.len(), 2, "{status}");
            let events = builtin.status()["events"].as_array().unwrap().clone();
            let music_event = events
                .iter()
                .find(|e| {
                    e["text"]
                        .as_str()
                        .unwrap()
                        .contains("本地音乐目录当前不可用")
                })
                .expect("音乐降级写进记录");
            assert_eq!(music_event["level"], "warn");
            assert_eq!(music_event["category"], "config");
            assert!(music_event["hint"].is_string());
            assert!(events.iter().any(|e| e["level"] == "warn"
                && e["text"].as_str().unwrap().contains("声纹按人记忆暂未生效")));
            assert!(events.iter().any(|e| {
                e["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("内置语音服务已启动")
            }));

            builtin.stop();
            assert!(builtin.music_status()["warning"].is_null());
            assert!(
                builtin
                    .speaker_manage(&serde_json::json!({"action": "status"}))
                    .unwrap()["warning"]
                    .is_null()
            );
            let after_stop = builtin.status();
            assert_eq!(after_stop["running"], false);
            assert!(after_stop["warnings"].as_array().unwrap().is_empty());
            assert_eq!(
                after_stop["events"].as_array().unwrap().len(),
                events.len(),
                "停止服务不清空诊断记录"
            );

            // 诊断包：整份记录 + 不含任何密钥
            let bundle = builtin.diagnostics_bundle();
            assert_eq!(bundle["format"], "moss-voice-diagnostics");
            assert_eq!(bundle["app"]["version"], env!("CARGO_PKG_VERSION"));
            assert_eq!(bundle["events"].as_array().unwrap().len(), events.len());
            assert_eq!(bundle["events_dropped"]["info"], 0);
            assert_eq!(bundle["approvals"]["limit"], APPROVALS_IN_BUNDLE);
            assert!(
                bundle["approvals"]["items"].is_array() && bundle["approvals"]["error"].is_null()
            );
            assert_eq!(bundle["models"]["llm"]["model"], "gpt-test");
            assert_eq!(bundle["models"]["llm"]["key_set"], true);
            let text = bundle.to_string();
            for secret in ["sk-test-llm", "sk-test-asr", "sk-test-tts"] {
                assert!(!text.contains(secret), "诊断包泄露了 {secret}");
            }
            assert!(
                bundle["excluded"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|e| e.as_str().unwrap().contains("密钥"))
            );
        }

        /// n68：诊断包脱敏覆盖每一个字符串字段（键值写法的密钥、Bearer、带凭据地址、设备 IP）。
        #[test]
        fn diagnostics_bundle_scrub_masks_every_string() {
            let mut bundle = serde_json::json!({
                "events": [{"text": "OTA 请求被拒绝（192.168.31.20）", "detail": "api_key: sk-abc123 Authorization: Bearer tok.en"}],
                "models": {"llm": {"url": "https://user:pw@llm.example.test/v1?key=zzz", "model": "gpt-test"}},
                "sessions": [{"ip": "10.1.2.3", "device_id": "aa:bb:cc:11:22:33"}],
                "nested": [["token=abcd"]],
                "number": 42,
            });
            scrub_bundle(&mut bundle);
            let text = bundle.to_string();
            for leak in [
                "192.168.31.20",
                "sk-abc123",
                "tok.en",
                "user:pw",
                "zzz",
                "10.1.2.3",
                "abcd",
            ] {
                assert!(!text.contains(leak), "{leak} 未脱敏：{text}");
            }
            assert_eq!(
                bundle["models"]["llm"]["url"],
                "https://[REDACTED]@llm.example.test/v1?[REDACTED]"
            );
            assert_eq!(bundle["models"]["llm"]["model"], "gpt-test");
            assert_eq!(bundle["sessions"][0]["ip"], "<ip>");
            assert_eq!(
                bundle["sessions"][0]["device_id"], "aa:bb:cc:11:22:33",
                "设备编号保留用于排障"
            );
            assert_eq!(bundle["number"], 42);
        }

        #[test]
        fn models_list_uses_stored_section_secret_without_echoing_it() {
            use std::io::{Read, Write};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = br#"{"object":"list","data":[{"id":"FunAudioLLM/SenseVoiceSmall"},{"id":"Qwen/Qwen2.5-7B-Instruct"}]}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(body);
                let _ = tx.send(head);
            });

            let temp = tempfile::tempdir().unwrap();
            let host = Arc::new(
                laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
            );
            host.db
                .lock()
                .unwrap()
                .set_secret("voice.asr.main", "sk-stored-asr-secret", 1.0)
                .unwrap();
            let builtin = init(Some(host)).0.unwrap();

            let input = SectionInput {
                url: format!("http://127.0.0.1:{port}/v1/audio/transcriptions"),
                ..Default::default()
            };
            let out = builtin.models_list(Section::Asr, &input).unwrap();
            assert_eq!(out["ok"], true);
            assert_eq!(
                out["models"],
                serde_json::json!(["FunAudioLLM/SenseVoiceSmall", "Qwen/Qwen2.5-7B-Instruct"])
            );
            assert!(!out.to_string().contains("sk-stored-asr-secret"));
            let head = rx.recv().unwrap();
            assert!(head.starts_with("GET /v1/models "), "{head}");
            assert!(
                head.to_ascii_lowercase()
                    .contains("authorization: bearer sk-stored-asr-secret"),
                "{head}"
            );

            // 同基础地址跨分区复用已保存密钥（例如 TTS 填同一个基础地址且留空密钥时，自动沿用 ASR 已保存的本机密钥）
            let mut settings = builtin.settings();
            settings.builtin_models.asr = Some(moss_voice::settings::BuiltinProvider {
                kind: ProviderKind::Openai,
                url: format!("http://127.0.0.1:{port}/v1/audio/transcriptions"),
                model: "FunAudioLLM/SenseVoiceSmall".into(),
                voice: String::new(),
                speed: 1.0,
            });
            moss_voice::settings::save(&builtin.settings_file(), &settings).unwrap();
            let tts_input = SectionInput {
                url: format!("http://127.0.0.1:{port}/v1"),
                ..Default::default()
            };
            assert_eq!(
                builtin
                    .secret_or_same_origin(Section::Tts, &tts_input)
                    .unwrap()
                    .as_deref(),
                Some("sk-stored-asr-secret")
            );
        }

        #[test]
        fn jev_save_shares_system_secret_switches_model_and_clears_cleanly() {
            let temp = tempfile::tempdir().unwrap();
            let host = Arc::new(
                laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
            );
            let builtin = init(Some(host.clone())).0.unwrap();

            // 初始未配置
            let initial = builtin.status();
            assert_eq!(initial["jev"]["key_set"], false);
            assert_eq!(initial["jev"]["model"], "typesafe/jev-1.14");

            // 保存 Jev 配置：写入与系统设置共用的 app_secrets["jev_api_key"] 与顶层 jev_enabled，且返回状态不回显密钥
            let saved = builtin
                .jev_save(
                    Some(&serde_json::json!({
                        "api_key": "sk-or-shared-jev-secret",
                        "model": "typesafe/jev-1.13",
                        "voice_enabled": true,
                        "feedback_enabled": true,
                    })),
                    false,
                )
                .unwrap();
            assert_eq!(saved["jev"]["key_set"], true);
            assert_eq!(saved["jev"]["model"], "typesafe/jev-1.13");
            assert_eq!(saved["jev"]["voice_enabled"], true);
            assert_eq!(saved["jev"]["feedback_enabled"], true);
            assert!(!saved.to_string().contains("sk-or-shared-jev-secret"));
            assert_eq!(
                host.db
                    .lock()
                    .unwrap()
                    .get_secret("jev_api_key")
                    .unwrap()
                    .as_deref(),
                Some("sk-or-shared-jev-secret")
            );
            assert!(moss_voice::settings::jev_feedback_enabled(
                &builtin.settings_file()
            ));

            // 顶部第 5 栏下拉框通过 local_save 切换模型版本与开关
            let switched = builtin
                .local_save(&serde_json::json!({
                    "jev_filter_enabled": true,
                    "jev_model": "typesafe/jev-1.14",
                }))
                .unwrap();
            assert_eq!(switched["jev"]["model"], "typesafe/jev-1.14");
            assert!(
                builtin
                    .local_save(&serde_json::json!({"jev_model": "gpt-4o"}))
                    .is_err()
            );

            // 删除此项：清理 jev_api_key，并重置语音筛选与加工反馈开关
            let cleared = builtin.jev_save(None, true).unwrap();
            assert_eq!(cleared["jev"]["key_set"], false);
            assert_eq!(cleared["jev"]["voice_enabled"], false);
            assert_eq!(cleared["jev"]["feedback_enabled"], false);
            assert_eq!(
                host.db.lock().unwrap().get_secret("jev_api_key").unwrap(),
                None
            );
        }

        #[test]
        fn screen_pack_auto_keep_is_opt_in_and_only_retries_once_a_day() {
            use moss_voice::settings::DeviceHardwarePrefs;
            const NOW: u64 = 1_790_000_000;
            const HUD: &str = "moss-screen-1:moss_hud";
            // 判定：没记录 / 没装 → 推；已装 → 不推；等下载未过期 → 不推；过期 → 推；一天内自动推过 → 不推
            assert!(screen_pack_auto_due(None, HUD, NOW));
            assert!(screen_pack_auto_due(
                Some(&DeviceHardwarePrefs::default()),
                HUD,
                NOW
            ));
            let installed = DeviceHardwarePrefs {
                screen_pack: Some("moss-screen-1:moss_hud".into()),
                ..DeviceHardwarePrefs::default()
            };
            assert!(!screen_pack_auto_due(Some(&installed), HUD, NOW));
            let old_version = DeviceHardwarePrefs {
                screen_pack: Some("moss-screen-0:moss_hud".into()),
                ..DeviceHardwarePrefs::default()
            };
            let other_style = DeviceHardwarePrefs {
                screen_pack: Some("moss-screen-1:goggle_bot".into()),
                ..DeviceHardwarePrefs::default()
            };
            assert!(
                screen_pack_auto_due(Some(&other_style), HUD, NOW),
                "换了画风要重刷，开机待命画面才跟着换"
            );
            assert!(
                screen_pack_auto_due(Some(&old_version), HUD, NOW),
                "包版本过旧要重刷"
            );
            let pending = DeviceHardwarePrefs {
                screen_pack_pending: Some("moss-screen-1:moss_hud".into()),
                screen_pack_pending_at: Some(NOW - 60),
                ..DeviceHardwarePrefs::default()
            };
            assert!(
                !screen_pack_auto_due(Some(&pending), HUD, NOW),
                "等下载时不重复推"
            );
            let expired = DeviceHardwarePrefs {
                screen_pack_pending_at: Some(NOW - 31 * 60),
                ..pending.clone()
            };
            assert!(screen_pack_auto_due(Some(&expired), HUD, NOW));
            let tried = DeviceHardwarePrefs {
                screen_pack_auto_at: Some(NOW - 3600),
                ..DeviceHardwarePrefs::default()
            };
            assert!(
                !screen_pack_auto_due(Some(&tried), HUD, NOW),
                "失败也不在一天内反复重启设备"
            );
            assert!(screen_pack_auto_due(Some(&tried), HUD, NOW + 24 * 3600));

            // 开关默认关；关着或服务没运行时什么也不推
            let temp = tempfile::tempdir().unwrap();
            let host = Arc::new(
                laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
            );
            let builtin = init(Some(host)).0.unwrap();
            assert!(!builtin.settings().builtin_local.screen_pack_auto_keep);
            assert_eq!(builtin.screen_pack_auto_tick(NOW), None);
            let saved = builtin
                .local_save(&serde_json::json!({"screen_pack_auto_keep": true}))
                .unwrap();
            assert_eq!(saved["local"]["settings"]["screen_pack_auto_keep"], true);
            assert_eq!(saved["restarted"], false, "开关热生效，不重启服务");
            assert_eq!(builtin.screen_pack_auto_tick(NOW), None, "服务没运行时不推");
            assert!(
                builtin
                    .local_save(&serde_json::json!({"screen_pack_auto_keep": "yes"}))
                    .is_err()
            );
            // 换画风：清掉“一天内已自动推过”，开着自动保持时马上按新画风重刷；状态里旧画风标为 style_outdated
            moss_voice::settings::update_voice(&builtin.settings_file(), |s| {
                let entry = s.builtin_device_prefs.entry("dev-1".into()).or_default();
                entry.screen_pack = Some(HUD.into());
                entry.screen_pack_auto_at = Some(NOW);
                Ok(())
            })
            .unwrap();
            builtin
                .local_save(&serde_json::json!({"screen_card_style": "moss_hud"}))
                .unwrap();
            assert_eq!(
                moss_voice::settings::load(&builtin.settings_file()).builtin_device_prefs["dev-1"]
                    .screen_pack_auto_at,
                Some(NOW),
                "画风没变不清"
            );
            builtin
                .local_save(&serde_json::json!({"screen_card_style": "goggle_bot"}))
                .unwrap();
            let settings = moss_voice::settings::load(&builtin.settings_file());
            assert_eq!(
                settings.builtin_device_prefs["dev-1"].screen_pack_auto_at,
                None
            );
            assert_eq!(
                current_screen_pack_marker(&settings),
                "moss-screen-1:goggle_bot"
            );
            assert!(screen_pack_auto_due(
                settings.builtin_device_prefs.get("dev-1"),
                &current_screen_pack_marker(&settings),
                NOW
            ));
        }

        #[test]
        fn screen_pack_counts_as_installed_only_after_the_device_downloads_it() {
            let temp = tempfile::tempdir().unwrap();
            let host = Arc::new(
                laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
            );
            let builtin = init(Some(host.clone())).0.unwrap();
            let registry = DbRegistry {
                db: host.db.clone(),
                host: host.clone(),
                settings_file: builtin.settings_file(),
            };
            // 推送后先“等下载”：握手读到的偏好里还没有画风标记，不会按全屏发画面
            moss_voice::settings::update_voice(&builtin.settings_file(), |s| {
                let entry = s.builtin_device_prefs.entry("dev-1".into()).or_default();
                entry.screen_pack_pending = Some("moss-screen-1:moss_hud".into());
                entry.screen_pack_pending_at = Some(1);
                entry.wake_target = Some("default".into());
                Ok(())
            })
            .unwrap();
            assert_eq!(
                registry.device_hardware_prefs("dev-1").unwrap().screen_pack,
                None
            );
            registry.assets_downloaded("someone-else");
            registry.assets_downloaded("dev-1");
            let prefs = moss_voice::settings::load(&builtin.settings_file()).builtin_device_prefs;
            let entry = &prefs["dev-1"];
            assert_eq!(entry.screen_pack.as_deref(), Some("moss-screen-1:moss_hud"));
            assert_eq!(entry.screen_pack_pending, None);
            assert_eq!(
                entry.wake_target.as_deref(),
                Some("default"),
                "一键刷入沿用上次的唤醒词"
            );
            assert!(
                !prefs.contains_key("someone-else"),
                "没推送过的设备不新增记录"
            );
            assert_eq!(
                registry
                    .device_hardware_prefs("dev-1")
                    .unwrap()
                    .screen_pack
                    .as_deref(),
                Some("moss-screen-1:moss_hud"),
                "下载完后重连握手按全屏画风显示"
            );
        }

        #[test]
        fn n61_wake_word_settings_base_package_and_push_guards() {
            let temp = tempfile::tempdir().unwrap();
            let host = Arc::new(
                laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
            );
            let builtin = init(Some(host)).0.unwrap();
            let status = builtin.wakeword_status().unwrap();
            assert_eq!(
                status["word"]["text"], "你好 MOSS",
                "default suggestion is MOSS"
            );
            assert_eq!(status["word"]["command"], "ni hao mo si");
            assert!(status["base"].is_null() && status["devices"].as_array().unwrap().is_empty());

            let saved = builtin.wakeword_manage(&serde_json::json!({"action": "save", "text": " MOSS MOSS ", "command": "Mo Si  mo si", "threshold": 0.3})).unwrap();
            assert_eq!(
                saved["word"],
                serde_json::json!({"text": "MOSS MOSS", "command": "mo si mo si", "threshold": 0.3})
            );
            for bad in [
                serde_json::json!({"action": "save", "text": "MOSS", "command": "moss", "threshold": 0.2}),
                serde_json::json!({"action": "save", "text": "", "command": "mo si", "threshold": 0.2}),
                serde_json::json!({"action": "save", "text": "MOSS", "command": "mo si", "threshold": 2.0}),
                serde_json::json!({"action": "save", "text": "MOSS", "command": "mo si", "threshold": 0.2, "extra": 1}),
                serde_json::json!({"action": "push", "device_id": "dev", "target": "custom"}),
                serde_json::json!({"action": "push", "device_id": "dev", "target": "custom", "confirmed": false}),
                serde_json::json!({"action": "clear_base", "confirmed": false}),
                serde_json::json!({"action": "flash"}),
                // n70：screen_pack 只能是布尔值，且不能夹带别的字段
                serde_json::json!({"action": "push", "device_id": "dev", "target": "custom", "confirmed": true, "screen_pack": "yes"}),
                serde_json::json!({"action": "push", "device_id": "dev", "target": "custom", "confirmed": true, "screen_pack": true, "extra": 1}),
            ] {
                assert!(builtin.wakeword_manage(&bad).is_err(), "{bad}");
            }
            assert_eq!(
                builtin.wakeword_status().unwrap()["word"]["command"],
                "mo si mo si",
                "rejected saves change nothing"
            );
            let push = builtin.wakeword_manage(&serde_json::json!({"action": "push", "device_id": "never-allowed", "target": "custom", "confirmed": true}));
            assert_eq!(
                push.unwrap_err().code,
                "wakeword_device_invalid",
                "only allowed devices"
            );
            let push = builtin.wakeword_manage(&serde_json::json!({"action": "push", "device_id": "never-allowed", "target": "custom", "confirmed": true, "screen_pack": true}));
            assert_eq!(
                push.unwrap_err().code,
                "wakeword_device_invalid",
                "the full-screen pack follows the same per-device guard"
            );

            let base_path = temp.path().join("picked-assets.bin");
            let base = moss_voice_server::wake_assets::pack_assets(&[
                moss_voice_server::wake_assets::AssetEntry {
                    name: "index.json".into(),
                    data: br#"{"version":1,"text_font":"f.bin"}"#.to_vec(),
                    width: 0,
                    height: 0,
                },
                moss_voice_server::wake_assets::AssetEntry {
                    name: "f.bin".into(),
                    data: vec![1; 10],
                    width: 0,
                    height: 0,
                },
            ])
            .unwrap();
            std::fs::write(&base_path, &base).unwrap();
            assert_eq!(
                builtin.wakeword_import_base(&base_path).unwrap()["text_font"],
                "f.bin"
            );
            assert_eq!(builtin.wakeword_status().unwrap()["base"]["files"], 2);
            std::fs::write(&base_path, b"not a package").unwrap();
            assert!(builtin.wakeword_import_base(&base_path).is_err());
            assert_eq!(
                builtin.wakeword_status().unwrap()["base"]["files"],
                2,
                "a bad import keeps the old base"
            );
            builtin
                .wakeword_manage(&serde_json::json!({"action": "clear_base", "confirmed": true}))
                .unwrap();
            assert!(builtin.wakeword_status().unwrap()["base"].is_null());
        }

        #[test]
        fn n59_role_settings_apply_without_restart_and_spoken_switch_is_saved() {
            let server_ids: Vec<&str> = moss_voice_server::persona::BUILTINS
                .iter()
                .map(|b| b.id)
                .collect();
            assert_eq!(
                server_ids,
                moss_voice::settings::BUILTIN_PERSONA_IDS,
                "settings and server role lists must match"
            );

            let temp = tempfile::tempdir().unwrap();
            let host = Arc::new(
                laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
            );
            let builtin = init(Some(host)).0.unwrap();
            assert_eq!(
                builtin.persona.system_prompt(),
                moss_voice_server::conversation::DEFAULT_SYSTEM_PROMPT
            );

            let saved = builtin
                .local_save(&serde_json::json!({
                    "custom_personas": [{"id": "custom_a1", "name": " 老师傅 ", "prompt": "话少，先说结论"}],
                    "persona_style": "custom_a1",
                }))
                .unwrap();
            assert_eq!(saved["local"]["settings"]["persona_style"], "custom_a1");
            assert_eq!(
                saved["local"]["settings"]["custom_personas"][0]["name"],
                "老师傅"
            );
            assert!(
                builtin
                    .persona
                    .system_prompt()
                    .contains("当前说话角色：老师傅"),
                "UI save applies to the running role at once"
            );

            assert!(
                builtin
                    .local_save(&serde_json::json!({"persona_style": "custom_zz"}))
                    .is_err()
            );
            assert!(builtin
                .local_save(&serde_json::json!({"custom_personas": [{"id": "custom_a1", "name": "老师傅", "prompt": "直接发 G1 X10"}]}))
                .is_err());
            assert_eq!(
                builtin.settings().builtin_local.persona_style,
                "custom_a1",
                "rejected saves change nothing"
            );

            // Spoken switch goes through the same settings file.
            // n75：旧叫法“任务简报”换到合并后的 MOSS
            assert_eq!(builtin.persona.switch("任务简报").unwrap(), "MOSS");
            assert_eq!(
                builtin.settings().builtin_local.persona_style,
                "moss_station"
            );
            // n75：“附把握和风险”开关所有角色可用，界面保存立即生效、不重启服务
            let saved = builtin
                .local_save(&serde_json::json!({"persona_risk_notes": true}))
                .unwrap();
            assert_eq!(saved["local"]["settings"]["persona_risk_notes"], true);
            assert!(builtin.persona.system_prompt().contains("把握较大"));
            builtin
                .local_save(&serde_json::json!({"persona_risk_notes": false}))
                .unwrap();
            assert!(!builtin.persona.system_prompt().contains("把握较大"));

            // Deleting the active custom role falls back to the default role.
            builtin
                .local_save(&serde_json::json!({"persona_style": "custom_a1"}))
                .unwrap();
            let removed = builtin
                .local_save(&serde_json::json!({"custom_personas": []}))
                .unwrap();
            assert_eq!(removed["local"]["settings"]["persona_style"], "workshop");
            assert_eq!(
                builtin.persona.system_prompt(),
                moss_voice_server::conversation::DEFAULT_SYSTEM_PROMPT
            );

            // A spoken switch to a role the UI just deleted on disk fails instead of claiming success.
            builtin
                .local_save(&serde_json::json!({"custom_personas": [{"id": "custom_b1", "name": "守门员", "prompt": "话少"}]}))
                .unwrap();
            moss_voice::settings::update_voice(&builtin.settings_file(), |s| {
                s.builtin_local.custom_personas.clear();
                Ok(())
            })
            .unwrap();
            assert!(builtin.persona.switch("守门员").is_err());
            assert_eq!(
                builtin.persona.config(),
                persona_config(&builtin.settings().builtin_local)
            );

            // UI saves and spoken switches racing each other leave the running role equal to the file.
            std::thread::scope(|scope| {
                let ui = &builtin;
                scope.spawn(move || {
                    for (i, style) in ["moss_station", "moss_risk", "beginner_guide"].iter().cycle().take(15).enumerate() {
                        ui.local_save(&serde_json::json!({"persona_style": style, "persona_prompt": format!("背景{i}")})).unwrap();
                    }
                });
                let voice = &builtin;
                scope.spawn(move || {
                    for name in ["守夜", "任务简报", "冷面幽默"].iter().cycle().take(15) {
                        voice.persona.switch(name).unwrap();
                    }
                });
            });
            assert_eq!(
                builtin.persona.config(),
                persona_config(&builtin.settings().builtin_local)
            );
        }

        #[test]
        fn local_laya_device_selection_lifecycle_and_jev_save_preserve_local_routing() {
            assert_eq!(
                BuiltinVoice::select_laya_device(true, false, None),
                "cuda:0"
            );
            assert_eq!(BuiltinVoice::select_laya_device(true, true, None), "cuda:0");
            assert_eq!(BuiltinVoice::select_laya_device(false, true, None), "mps");
            assert_eq!(BuiltinVoice::select_laya_device(false, false, None), "cpu");
            assert_eq!(
                BuiltinVoice::select_laya_device(false, true, Some(" cuda:1 ")),
                "cuda:1"
            );
            assert_eq!(
                BuiltinVoice::select_laya_device(true, false, Some("   ")),
                "cuda:0"
            );
            assert!(!BuiltinVoice::probe_laya_cuda_available(
                std::path::Path::new("nonexistent-python"),
                true
            ));
            assert!(!BuiltinVoice::probe_laya_cuda_available(
                std::path::Path::new("nonexistent-python"),
                false
            ));
            assert!(!BuiltinVoice::probe_laya_mps_available(
                std::path::Path::new("nonexistent-python"),
                true
            ));
            assert!(!BuiltinVoice::probe_laya_mps_available(
                std::path::Path::new("nonexistent-python"),
                false
            ));

            let temp = tempfile::tempdir().unwrap();
            let fake_install = |model_id: &str| {
                let spec = local_models::spec(model_id).unwrap();
                let dir = temp.path().join("models").join(model_id);
                for (rel, len) in std::iter::once((spec.path, spec.size))
                    .chain(spec.extras.iter().map(|e| (e.path, e.size)))
                {
                    let p = dir.join(rel);
                    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                    std::fs::File::create(&p).unwrap().set_len(len).unwrap();
                }
            };
            fake_install("laya_multilingual");
            fake_install("onejev_1_6_0_8b");
            fake_install("onejev_1_6_4b");

            let host = Arc::new(
                laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
            );
            let mut voice_settings = moss_voice::VoiceSettings {
                mode: VoiceMode::Official,
                ..Default::default()
            };
            voice_settings.builtin_local.jev_model = "laya_multilingual".into();
            voice_settings.builtin_local.jev_filter_enabled = true;
            moss_voice::settings::save_with_jev_feedback(
                &temp.path().join("settings.json"),
                &voice_settings,
                Some(false),
            )
            .unwrap();

            let builtin = init(Some(host.clone())).0.unwrap();
            assert!(builtin.should_run_local_laya());
            assert!(!builtin.should_run_local_laya_for_feedback());
            assert!(builtin.status()["local"]["onejev_env"]["accelerator"].is_string());
            assert!(builtin.status()["local"]["onejev_env"]["accelerator_label"].is_string());
            assert!(builtin.status()["local"]["laya_env"]["accelerator"].is_string());

            // 可在本地 OneJev 0.8B / 4B / 9B 与 Laya 之间自由切换（未安装的 9B 在启用时会给出明确下载提示）
            assert!(
                builtin
                    .precheck_jev_save_input(
                        Some(&serde_json::json!({
                            "model": "onejev_1_6_9b",
                            "voice_enabled": true,
                        })),
                        false,
                        &ModelsInput::default(),
                        &[],
                    )
                    .is_err(),
                "未下载的 OneJev 9B 不允许直接作为启用态保存"
            );
            let switched_onejev = builtin
                .local_save(&serde_json::json!({
                    "jev_filter_enabled": true,
                    "jev_model": "onejev_1_6_4b",
                }))
                .unwrap();
            assert_eq!(switched_onejev["jev"]["model"], "onejev_1_6_4b");
            assert_eq!(
                switched_onejev["jev"]["endpoint"],
                moss_voice::models::DEFAULT_LOCAL_LAYA_DECISIONS_URL
            );
            assert_eq!(switched_onejev["jev"]["local_installed"], true);
            assert_eq!(switched_onejev["local"]["jev_key_configured"], true);
            assert!(builtin.should_run_local_laya());

            // 本地 Jev 实际运行情况：已下载已启用但未拉起 → stopped；在途启动 → starting；
            // 后台启动失败 → failed 并带原因；切到云端模型 → cloud（2026-11-07）
            let runtime = |b: &BuiltinVoice| b.status()["local"]["jev_runtime"].clone();
            assert_eq!(runtime(&builtin)["model"], "onejev_1_6_4b");
            assert_eq!(runtime(&builtin)["state"], "stopped");
            {
                let mut runner = builtin.laya.lock().unwrap();
                runner.starting_model = Some("onejev_1_6_4b".to_owned());
                runner.starting_cancel = Some(Arc::new(std::sync::atomic::AtomicBool::new(false)));
            }
            assert_eq!(runtime(&builtin)["state"], "starting");
            {
                let mut runner = builtin.laya.lock().unwrap();
                runner.starting_model = None;
                runner.starting_cancel = None;
                runner.last_failure =
                    Some(("onejev_1_6_4b".to_owned(), "OneJev 模型加载超时".to_owned()));
            }
            assert_eq!(runtime(&builtin)["state"], "failed");
            assert_eq!(runtime(&builtin)["error"], "OneJev 模型加载超时");
            builtin.laya.lock().unwrap().last_failure =
                Some(("onejev_1_6_0_8b".to_owned(), "别的档位的旧失败".to_owned()));
            assert_eq!(
                runtime(&builtin)["state"],
                "stopped",
                "其他档位的失败不串到当前模型"
            );
            builtin.laya.lock().unwrap().last_failure = None;
            let cpu_gpu =
                moss_voice::onejev::expected_gpu(builtin.models.root_for("onejev_1_6_4b"));
            assert_eq!(
                runtime(&builtin)["notice"].as_str(),
                moss_voice::onejev::local_jev_budget("onejev_1_6_4b", cpu_gpu)
                    .unwrap()
                    .notice
            );

            // 切回 laya_multilingual 继续验证免密钥与备用密钥共存
            builtin
                .local_save(&serde_json::json!({
                    "jev_filter_enabled": true,
                    "jev_model": "laya_multilingual",
                }))
                .unwrap();

            // 省略 model 且无 OpenRouter API Key：保留 laya_multilingual，并允许单独开启加工反馈、关闭语音筛选
            builtin
                .precheck_jev_save_input(
                    Some(&serde_json::json!({
                        "voice_enabled": false,
                        "feedback_enabled": true,
                    })),
                    false,
                    &ModelsInput::default(),
                    &[],
                )
                .unwrap();

            let saved_feedback_only = builtin
                .jev_save(
                    Some(&serde_json::json!({
                        "voice_enabled": false,
                        "feedback_enabled": true,
                    })),
                    false,
                )
                .unwrap();
            assert_eq!(saved_feedback_only["jev"]["model"], "laya_multilingual");
            assert_eq!(saved_feedback_only["jev"]["voice_enabled"], false);
            assert_eq!(saved_feedback_only["jev"]["feedback_enabled"], true);
            assert!(
                builtin.should_run_local_laya_for_feedback(),
                "仅开启加工反馈且非内置语音模式时，应用启动也应拉起本地 Jev 服务"
            );

            // 录入备用 OpenRouter 密钥但不传/不改本地模型：写入密钥且不覆盖本地路由
            let saved_backup_key = builtin
                .jev_save(
                    Some(&serde_json::json!({
                        "api_key": "sk-or-backup-only",
                        "model": "",
                    })),
                    false,
                )
                .unwrap();
            assert_eq!(saved_backup_key["jev"]["model"], "laya_multilingual");
            assert_eq!(saved_backup_key["jev"]["feedback_enabled"], true);
            assert_eq!(
                host.db
                    .lock()
                    .unwrap()
                    .get_secret("jev_api_key")
                    .unwrap()
                    .as_deref(),
                Some("sk-or-backup-only")
            );

            // 删除未激活的 onejev_1_6_0_8b 不递增当前运行代次；切换到 onejev_1_6_4b 后删除闲置 laya_multilingual 也不误停当前网关
            let gen_before = builtin.laya.lock().unwrap().generation;
            builtin.model_delete("onejev_1_6_0_8b").unwrap();
            assert_eq!(builtin.laya.lock().unwrap().generation, gen_before);
            builtin
                .local_save(&serde_json::json!({
                    "jev_filter_enabled": true,
                    "jev_model": "onejev_1_6_4b",
                }))
                .unwrap();
            let gen_on_onejev = builtin.laya.lock().unwrap().generation;
            // 2026-11-07：删除 Laya 时连同 MOSS 创建（带标记）的虚拟环境与 tmp 一起删
            let laya_root = temp.path().join("laya-env");
            std::fs::create_dir_all(laya_root.join(".venv").join("bin")).unwrap();
            std::fs::write(
                laya_root
                    .join(".venv")
                    .join(moss_voice::laya_env::MANAGED_MARKER),
                b"",
            )
            .unwrap();
            std::fs::create_dir_all(laya_root.join("tmp")).unwrap();
            *builtin.laya_root_for_test.lock().unwrap() = Some(laya_root.clone());
            assert_eq!(builtin.status()["local"]["laya_setup"]["state"], "idle");
            builtin.model_delete("laya_multilingual").unwrap();
            assert!(
                !laya_root.exists(),
                "MOSS 创建的 Laya 虚拟环境随模型一起删除"
            );
            assert_eq!(
                builtin.laya.lock().unwrap().generation,
                gen_on_onejev,
                "运行 onejev_1_6_4b 时删除闲置 laya_multilingual 不得误停当前 Jev 进程"
            );
            // 当反馈关闭、仅语音筛选开启时：原地重启（keep_local_jev=true）不打断在途预热，完整停止（false）才回收
            builtin
                .jev_save(
                    Some(&serde_json::json!({
                        "voice_enabled": true,
                        "feedback_enabled": false,
                        "model": "onejev_1_6_4b",
                    })),
                    false,
                )
                .unwrap();
            let in_flight_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let mut runner = builtin.laya.lock().unwrap();
                runner.starting_model = Some("onejev_1_6_4b".to_owned());
                runner.starting_cancel = Some(in_flight_cancel.clone());
            }
            let gen_keep = builtin.laya.lock().unwrap().generation;
            builtin.stop_locked_inner(true);
            assert!(!in_flight_cancel.load(std::sync::atomic::Ordering::SeqCst));
            assert_eq!(builtin.laya.lock().unwrap().generation, gen_keep);
            builtin.stop_locked_inner(false);
            assert!(in_flight_cancel.load(std::sync::atomic::Ordering::SeqCst));
            assert!(builtin.laya.lock().unwrap().generation > gen_keep);

            // 删除正在后台加载权重的模型时：必须先置位取消并等待 jev_start_lock 释放，才允许删除磁盘目录
            let bg_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let bg_cleaned_before_delete = Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let mut runner = builtin.laya.lock().unwrap();
                runner.starting_model = Some("onejev_1_6_4b".to_owned());
                runner.starting_cancel = Some(bg_cancel.clone());
            }
            let start_lock_clone = builtin.jev_start_lock.clone();
            let model_dir_check = builtin.models.path_if_installed("onejev_1_6_4b").unwrap();
            let cancel_for_bg = bg_cancel.clone();
            let cleaned_flag = bg_cleaned_before_delete.clone();
            let lock_acquired = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let lock_acquired_bg = lock_acquired.clone();
            let bg_worker = std::thread::spawn(move || {
                let _g = start_lock_clone.lock().unwrap();
                lock_acquired_bg.store(true, std::sync::atomic::Ordering::SeqCst);
                while !cancel_for_bg.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
                // 在释放 jev_start_lock 前，模型权重目录必须尚未被删除（Windows 下否则触发共享冲突）
                if model_dir_check.exists() {
                    cleaned_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            });
            while !lock_acquired.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let gen_before_del = builtin.laya.lock().unwrap().generation;
            builtin.model_delete("onejev_1_6_4b").unwrap();
            bg_worker.join().unwrap();
            assert!(bg_cleaned_before_delete.load(std::sync::atomic::Ordering::SeqCst));
            assert!(builtin.laya.lock().unwrap().generation > gen_before_del);
            assert!(builtin.models.path_if_installed("onejev_1_6_4b").is_none());

            // 先切回 off 触发非阻塞取消（starting_model -> cancelling_model），紧接着删 onejev_1_6_9b 也必须等 jev_start_lock 释放
            fake_install("onejev_1_6_9b");
            let cancel_9b = Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let mut runner = builtin.laya.lock().unwrap();
                runner.starting_model = Some("onejev_1_6_9b".to_owned());
                runner.starting_cancel = Some(cancel_9b.clone());
            }
            let lock_9b = builtin.jev_start_lock.clone();
            let dir_9b_check = builtin.models.path_if_installed("onejev_1_6_9b").unwrap();
            let survived_until_unlock = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let survived_bg = survived_until_unlock.clone();
            let locked_9b = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let locked_9b_bg = locked_9b.clone();
            let cancel_9b_bg = cancel_9b.clone();
            let worker_9b = std::thread::spawn(move || {
                let _g = lock_9b.lock().unwrap();
                locked_9b_bg.store(true, std::sync::atomic::Ordering::SeqCst);
                while !cancel_9b_bg.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
                if dir_9b_check.exists() {
                    survived_bg.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            });
            while !locked_9b.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            builtin
                .local_save(&serde_json::json!({
                    "jev_filter_enabled": false,
                    "jev_model": "typesafe/jev-latest",
                }))
                .unwrap();
            assert_eq!(
                builtin.laya.lock().unwrap().cancelling_model.as_deref(),
                Some("onejev_1_6_9b")
            );
            builtin.model_delete("onejev_1_6_9b").unwrap();
            worker_9b.join().unwrap();
            assert!(survived_until_unlock.load(std::sync::atomic::Ordering::SeqCst));
            assert!(builtin.laya.lock().unwrap().cancelling_model.is_none());
            assert!(builtin.models.path_if_installed("onejev_1_6_9b").is_none());

            assert!(parse_abs_env_path(None).is_none());
            assert!(parse_abs_env_path(Some(std::ffi::OsStr::new(""))).is_none());
            assert!(parse_abs_env_path(Some(std::ffi::OsStr::new("   "))).is_none());
            assert!(parse_abs_env_path(Some(std::ffi::OsStr::new("relative/laya"))).is_none());
            let abs_sample = std::env::temp_dir().join("moss-laya-sample");
            let abs_padded = format!("  {}  ", abs_sample.display());
            assert_eq!(
                parse_abs_env_path(Some(std::ffi::OsStr::new(&abs_padded))),
                Some(abs_sample.clone())
            );
            assert!(parse_non_empty_env_path(None).is_none());
            assert!(parse_non_empty_env_path(Some(std::ffi::OsStr::new("  "))).is_none());
            assert_eq!(
                parse_non_empty_env_path(Some(std::ffi::OsStr::new("  python3  "))),
                Some(std::path::PathBuf::from("python3"))
            );

            if moss_laya_root_env().is_none() && moss_laya_python_env().is_none() {
                let (laya_root, laya_py) = BuiltinVoice::resolve_laya_python();
                let tmp = std::env::temp_dir();
                assert!(
                    !laya_root.starts_with(&tmp) && !laya_py.starts_with(&tmp),
                    "resolve_laya_python must never probe world-writable temp_dir"
                );
            }
            let saved_speed = builtin
                .local_save(&serde_json::json!({ "tts_speed": 1.25 }))
                .unwrap();
            assert_eq!(saved_speed["local"]["settings"]["tts_speed"], 1.25);

            builtin
                .precheck_jev_save_input(
                    Some(&serde_json::json!({
                        "voice_enabled": false,
                        "feedback_enabled": false,
                    })),
                    false,
                    &ModelsInput::default(),
                    &[],
                )
                .unwrap();
            let turned_off = builtin
                .jev_save(
                    Some(&serde_json::json!({
                        "voice_enabled": false,
                        "feedback_enabled": false,
                    })),
                    false,
                )
                .unwrap();
            assert_eq!(turned_off["jev"]["voice_enabled"], false);
            assert_eq!(turned_off["jev"]["feedback_enabled"], false);
            assert!(builtin.laya.lock().unwrap().generation > gen_before);
        }

        #[test]
        fn probe_laya_http_rejects_non_http_tcp_listener_and_accepts_http() {
            use std::io::{Read, Write};
            let non_http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let non_http_addr = non_http.local_addr().unwrap();
            std::thread::spawn(move || {
                if let Ok((mut stream, _)) = non_http.accept() {
                    let mut buf = [0u8; 128];
                    let _ = stream.read(&mut buf);
                    let _ = stream.write_all(b"NOT_HTTP_BANNER\r\n");
                }
            });
            assert!(
                probe_local_jev_health_model(non_http_addr, std::time::Duration::from_millis(200))
                    .is_none()
            );

            for bad_http in [
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nnot found".as_slice(),
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\n\r\nerror".as_slice(),
                b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 13\r\n\r\n<html></html>".as_slice(),
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 18\r\n\r\n{\"status\":\"error\"}".as_slice(),
            ] {
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let addr = listener.local_addr().unwrap();
                let reply = bad_http.to_vec();
                std::thread::spawn(move || {
                    if let Ok((mut stream, _)) = listener.accept() {
                        let mut buf = [0u8; 256];
                        let _ = stream.read(&mut buf);
                        let _ = stream.write_all(&reply);
                    }
                });
                assert!(probe_local_jev_health_model(addr, std::time::Duration::from_millis(200)).is_none());
            }

            let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let http_addr = http.local_addr().unwrap();
            std::thread::spawn(move || {
                if let Ok((mut stream, _)) = http.accept() {
                    let mut buf = [0u8; 256];
                    let _ = stream.read(&mut buf);
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\n\r\n{\"status\":\"ok\"}",
                    );
                }
            });
            assert_eq!(
                probe_local_jev_health_model(http_addr, std::time::Duration::from_millis(200))
                    .as_deref(),
                Some("laya_multilingual")
            );

            let onejev_http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let onejev_addr = onejev_http.local_addr().unwrap();
            std::thread::spawn(move || {
                if let Ok((mut stream, _)) = onejev_http.accept() {
                    let mut buf = [0u8; 256];
                    let _ = stream.read(&mut buf);
                    let body = serde_json::to_vec(&moss_voice::onejev::health_payload(
                        "onejev_1_6_4b",
                        "Apple Metal GPU",
                    ))
                    .unwrap();
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&body);
                }
            });
            assert_eq!(
                probe_local_jev_health_model(onejev_addr, std::time::Duration::from_millis(200))
                    .as_deref(),
                Some("onejev_1_6_4b")
            );

            // 外部上游 qev / OneJev 服务（不含 engine 字段，但含 fork_mode 与自定义 model）不得被误判为 laya_multilingual
            let ext_http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let ext_addr = ext_http.local_addr().unwrap();
            std::thread::spawn(move || {
                if let Ok((mut stream, _)) = ext_http.accept() {
                    let mut buf = [0u8; 256];
                    let _ = stream.read(&mut buf);
                    let body = br#"{"status":"ok","fork_mode":"llama.cpp","model":"custom-qev"}"#;
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body);
                }
            });
            assert_eq!(
                probe_local_jev_health_model(ext_addr, std::time::Duration::from_millis(200))
                    .as_deref(),
                Some("external_onejev:custom-qev")
            );
        }
    }

    /// 测试豆包识别或合成（NV7）：识别送 1 秒低音量正弦音，合成一句短话。结果与错误都不含密钥。
    fn test_doubao(resolved: &moss_voice::models::ResolvedSection) -> moss_voice::TestResult {
        let started = std::time::Instant::now();
        let outcome: Result<(String, Option<String>), moss_voice_providers::ProviderError> =
            tauri::async_runtime::block_on(async {
                match resolved.section {
                    Section::Asr => {
                        let asr =
                            DoubaoAsr::new(&resolved.url, &resolved.api_key, &resolved.model)?;
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
                        let wav =
                            moss_voice_providers::wav::encode_wav(&pcm.samples, pcm.sample_rate);
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
            final_url: resolved.url.clone(),
            detail,
            audio_data_url,
            thinking_off: None,
        }
    }

    fn builtin(app: &tauri::AppHandle) -> Result<Arc<BuiltinVoice>, Value> {
        app.try_state::<BuiltinState>()
            .and_then(|state| state.0.clone())
            .ok_or_else(|| serde_json::json!({"code": "voice_unavailable", "message": "内置语音服务不可用：本机后端未启动"}))
    }

    /// 在阻塞线程池中执行（启动、停止会创建或关闭运行时，不能放在异步线程里）。
    pub async fn run<F>(app: tauri::AppHandle, task: F) -> CmdResult
    where
        F: FnOnce(&BuiltinVoice) -> Result<Value, CenterError> + Send + 'static,
    {
        let builtin = builtin(&app)?;
        tauri::async_runtime::spawn_blocking(move || {
            task(&builtin).map_err(|error| serde_json::to_value(error).unwrap_or_default())
        })
        .await
        .map_err(|_| serde_json::json!({"code": "voice_internal", "message": "内置语音服务任务异常结束"}))?
    }

    pub fn parse_sections(values: &[String]) -> Result<Vec<Section>, CenterError> {
        values
            .iter()
            .map(|value| {
                Section::parse(value).ok_or_else(|| {
                    CenterError::new("model_input_invalid", 400, "分区只能是 llm、asr 或 tts")
                })
            })
            .collect()
    }

    /// MOSS 退出时停止内置语音服务及本地 Jev（OneJev / Laya）子进程（在急停之后调用）。
    pub fn shutdown(app: &tauri::AppHandle) {
        if let Ok(builtin) = builtin(app) {
            builtin.laya_setup.cancel_and_wait();
            builtin.stop();
            // 等待已被 starting_cancel 中断的后台启动线程完成子进程回收，杜绝退出时残留孤儿 llama-server
            builtin.stop_laya_and_wait();
        }
    }
}

#[cfg(desktop)]
pub use imp::{BuiltinVoice, autostart, init, record_moss_event, shutdown};

/// 内置语音状态含设备 IP、会话事件，调试开启时还有原始识别文字与工具参数。
/// 2026-11-05：只给前端实际调用它的两个窗口——主窗口语音页（`VoiceBuiltinCard`）与打包私密页
/// （`voicePrivate.jsx` 轮询运行状态），其他窗口即使拿到 `default` capability（`windows:["*"]`）也读不到。
#[tauri::command]
pub async fn voice_builtin_status(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_status_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |builtin| Ok(builtin.status())).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

#[tauri::command]
pub async fn voice_builtin_start(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |builtin| builtin.start()).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

#[tauri::command]
pub async fn voice_builtin_stop(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |builtin| Ok(builtin.stop())).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

/// 导出诊断包（2026-11-07 技术书 n68）：后端生成并脱敏 JSON，弹出系统“另存为”对话框写入用户选的位置；
/// 路径与内容都不来自网页。与状态读取同权限：只给经 `require_status_window` 确认的 MOSS 主窗口。
/// 用户取消时返回 `{saved: false}`；成功返回文件名（不回传完整路径）、字节数与事件条数。
#[tauri::command]
pub async fn voice_diagnostics_export(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_status_window(&window)?;
    #[cfg(desktop)]
    let result = {
        let bundle = imp::run(app.clone(), |builtin| Ok(builtin.diagnostics_bundle())).await?;
        let events = bundle["events"].as_array().map(Vec::len).unwrap_or(0);
        let text = serde_json::to_string_pretty(&bundle).map_err(|_| {
            serde_json::json!({"code": "diagnostics_export_failed", "message": "诊断包生成失败（内容无法转成 JSON）"})
        })?;
        let default_name = format!(
            "moss-voice-diagnostics-{}.json",
            bundle["generated_at_ms"].as_u64().unwrap_or(0) / 1000
        );
        let picker = app.clone();
        let name = default_name.clone();
        let picked = tauri::async_runtime::spawn_blocking(move || {
            use tauri_plugin_dialog::DialogExt;
            picker
                .dialog()
                .file()
                .set_title("保存语音诊断包")
                .set_file_name(&name)
                .add_filter("JSON", &["json"])
                .blocking_save_file()
                .and_then(|path| path.into_path().ok())
        })
        .await
        .map_err(|_| serde_json::json!({"code": "diagnostics_export_failed", "message": "打开保存对话框失败"}))?;
        match picked {
            None => Ok(serde_json::json!({"saved": false})),
            Some(path) => match std::fs::write(&path, text.as_bytes()) {
                Ok(()) => Ok(serde_json::json!({
                    "saved": true,
                    "file_name": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(default_name),
                    "bytes": text.len(),
                    "events": events,
                })),
                Err(error) => Err(serde_json::json!({
                    "code": "diagnostics_export_failed",
                    "message": "诊断包没能写入所选位置：可能没有写入权限或磁盘已满，换个文件夹再试",
                    "detail": error.kind().to_string(),
                })),
            },
        }
    };
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

/// n80：导出 Colab T4 专属联动笔记本（`.ipynb`）。后端生成固定审计过的 JSON，弹出系统“另存为”对话框写入；
/// 路径与内容都不来自网页，且不含任何本机路径或密钥。
#[tauri::command]
pub async fn voice_colab_export_notebook(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = {
        let text = moss_voice::colab::notebook_json();
        let default_name = moss_voice::colab::COLAB_NOTEBOOK_FILENAME.to_owned();
        let picker = app.clone();
        let name = default_name.clone();
        let picked = tauri::async_runtime::spawn_blocking(move || {
            use tauri_plugin_dialog::DialogExt;
            picker
                .dialog()
                .file()
                .set_title("保存 Colab T4 笔记本")
                .set_file_name(&name)
                .add_filter("Jupyter Notebook", &["ipynb"])
                .blocking_save_file()
                .and_then(|path| path.into_path().ok())
        })
        .await
        .map_err(
            |_| serde_json::json!({"code": "colab_export_failed", "message": "打开保存对话框失败"}),
        )?;
        match picked {
            None => Ok(serde_json::json!({"saved": false})),
            Some(path) => match std::fs::write(&path, text.as_bytes()) {
                Ok(()) => Ok(serde_json::json!({
                    "saved": true,
                    "file_name": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(default_name),
                    "bytes": text.len(),
                })),
                Err(error) => Err(serde_json::json!({
                    "code": "colab_export_failed",
                    "message": "笔记本没能写入所选位置：可能没有写入权限或磁盘已满，换个文件夹再试",
                    "detail": error.kind().to_string(),
                })),
            },
        }
    };
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

/// 允许（`allowed`）或撤销（`revoked`）设备。允许后设备需重启以重新获取 token。
#[tauri::command]
pub async fn voice_device_set_status(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    device_id: String,
    status: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| {
        builtin.set_device_status(&device_id, &status)
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, device_id, status));
    result
}

#[tauri::command]
pub async fn voice_device_request(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    device_id: String,
    request: String,
    value: Option<Value>,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| {
        builtin.device_request(&device_id, &request, value.as_ref())
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, device_id, request, value));
    result
}

#[tauri::command]
pub async fn voice_device_rename(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    device_id: String,
    name: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.rename_device(&device_id, &name)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, device_id, name));
    result
}

/// 测试内置服务的一个模型分区（`llm` / `asr` / `tts` / `jev` / `colab`）；密钥留空时用已保存的密钥。
#[tauri::command]
pub async fn voice_builtin_models_test(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    section: String,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| {
        if section.trim() == "jev" {
            return builtin.jev_test(&input);
        }
        if section.trim() == "colab" {
            return builtin.colab_test(&input);
        }
        let section = imp::parse_sections(std::slice::from_ref(&section))?[0];
        let input: moss_voice::SectionInput = serde_json::from_value(input).map_err(|_| {
            moss_voice::CenterError::new("model_input_invalid", 400, "模型配置格式不正确")
        })?;
        builtin.models_test(section, &input)
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, section, input));
    result
}

/// 读取内置服务一个模型分区（`llm` / `asr` / `tts` / `jev`）支持的模型列表；密钥留空时用已保存的密钥。
#[tauri::command]
pub async fn voice_builtin_models_list(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    section: String,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| {
        if section.trim() == "jev" {
            return builtin.jev_models_list(&input);
        }
        let section = imp::parse_sections(std::slice::from_ref(&section))?[0];
        let input: moss_voice::SectionInput = serde_json::from_value(input).map_err(|_| {
            moss_voice::CenterError::new("model_input_invalid", 400, "模型配置格式不正确")
        })?;
        builtin.models_list(section, &input)
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, section, input));
    result
}

/// 保存内置服务的模型设置。`clear` 中的分区删除设置与密钥。
#[tauri::command]
pub async fn voice_builtin_models_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
    clear: Option<Vec<String>>,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| {
        let clear_list = clear.unwrap_or_default();
        let clear_jev = clear_list.iter().any(|s| s.trim() == "jev");
        let jev_input = input.get("jev");
        if clear_jev && clear_list.len() == 1 && input.as_object().is_none_or(|o| o.is_empty()) {
            return builtin.jev_save(None, true);
        }
        if !clear_jev
            && clear_list.is_empty()
            && jev_input.is_some()
            && input.as_object().is_some_and(|o| o.len() == 1)
        {
            return builtin.jev_save(jev_input, false);
        }
        let non_jev_clear: Vec<String> = clear_list
            .into_iter()
            .filter(|s| s.trim() != "jev")
            .collect();
        let mut non_jev_input = input.clone();
        if let Some(obj) = non_jev_input.as_object_mut() {
            obj.remove("jev");
        }
        let parsed_input: moss_voice::ModelsInput =
            serde_json::from_value(non_jev_input).map_err(|_| {
                moss_voice::CenterError::new("model_input_invalid", 400, "模型配置格式不正确")
            })?;
        let parsed_clear = imp::parse_sections(&non_jev_clear)?;
        if clear_jev || jev_input.is_some() {
            builtin.precheck_jev_save_input(jev_input, clear_jev, &parsed_input, &parsed_clear)?;
        }
        let mut res = builtin.models_save(&parsed_input, &parsed_clear)?;
        if clear_jev || jev_input.is_some() {
            res = builtin.jev_save(jev_input, clear_jev)?;
        }
        Ok(res)
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, input, clear));
    result
}

/// 保存本地模型设置（`asr` / `tts` / `vad` 开关、`tts_speaker`、`tts_speed`、`mirror`）。
#[tauri::command]
pub async fn voice_local_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    settings: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.local_save(&settings)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, settings));
    result
}

/// 开始下载本地模型（id 见 `local_models::CATALOG`）；进度在状态的 `local.models` 中。
#[tauri::command]
pub async fn voice_local_model_download(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    id: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.model_download(&id)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, id));
    result
}

#[tauri::command]
pub async fn voice_local_model_cancel(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    id: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| Ok(builtin.model_cancel(&id))).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, id));
    result
}

#[tauri::command]
pub async fn voice_local_model_delete(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    id: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.model_delete(&id)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, id));
    result
}

/// 自动准备 Laya 的 Python 运行环境（2026-11-07）：`action` 为 `start` / `cancel`。
#[tauri::command]
pub async fn voice_local_laya_setup(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    action: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.laya_setup_action(&action)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, action));
    result
}

/// 语音中心校验用的豆包默认值与模型提供方的默认值一致（两个 crate 各自定义，避免互相依赖）。
#[cfg(all(test, desktop))]
#[test]
fn doubao_defaults_match_the_provider() {
    use moss_voice::models::{
        DOUBAO_ASR_RESOURCE, DOUBAO_ASR_URL, DOUBAO_TTS_RESOURCE, DOUBAO_TTS_URL,
    };
    use moss_voice_providers::doubao;
    assert_eq!(DOUBAO_ASR_URL, doubao::DEFAULT_ASR_URL);
    assert_eq!(DOUBAO_ASR_RESOURCE, doubao::DEFAULT_ASR_RESOURCE);
    assert_eq!(DOUBAO_TTS_URL, doubao::DEFAULT_TTS_URL);
    assert_eq!(DOUBAO_TTS_RESOURCE, doubao::DEFAULT_TTS_RESOURCE);
}

#[tauri::command]
pub async fn voice_agent_status(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| Ok(voice.agent_status())).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}
#[tauri::command]
pub async fn voice_agent_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.agent_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, input));
    result
}
/// 停止正在运行的 Agent 任务（进程树随 Job Object 结束）。
#[tauri::command]
pub async fn voice_agent_stop(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    run_id: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| Ok(voice.agent_stop(&run_id))).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, run_id));
    result
}

#[tauri::command]
pub async fn voice_music_pick_directory(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        // Opens only after an explicit click in the desktop main window.
        // The user-selected path is returned to the form, not saved or scanned.
        let Some(picked) = app
            .dialog()
            .file()
            .set_title("选择本地音乐文件夹")
            .blocking_pick_folder()
        else {
            return Ok(Value::Null);
        };
        let path = picked.into_path().map_err(|_| {
            serde_json::json!({
                "code":"music_invalid","message":"无法识别选择的音乐目录"
            })
        })?;
        let name = path.to_str().ok_or_else(|| {
            serde_json::json!({
                "code":"music_invalid","message":"音乐目录包含不支持的字符"
            })
        })?;
        if !path.is_absolute() || name.len() > 1024 || name.chars().any(char::is_control) {
            return Err(serde_json::json!({
                "code":"music_invalid","message":"所选路径无效或过长"
            }));
        }
        Ok(serde_json::json!({"directory":name}))
    })
    .await
    .map_err(|_| {
        serde_json::json!({
            "code":"voice_internal","message":"选择音乐目录失败"
        })
    })?;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

#[tauri::command]
pub async fn voice_music_status(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| Ok(voice.music_status())).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}
#[tauri::command]
pub async fn voice_music_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.music_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, input));
    result
}

#[tauri::command]
pub async fn voice_home_status(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| Ok(voice.home_status())).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
#[tauri::command]
pub async fn voice_home_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}

#[tauri::command]
pub async fn voice_home_backup_export(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.home_backup_export()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
/// Explicit local-file picker; no path or bytes are supplied by web content.
#[tauri::command]
pub async fn voice_home_backup_audit(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result=tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        let Some(picked)=app.dialog().file().set_title("选择历史备份文件（仅审计，不恢复）").blocking_pick_file() else {
            return Ok(Value::Null);
        };
        let path=picked.into_path().map_err(|_|serde_json::json!({"code":"home_audit_invalid","message":"无法识别所选文件"}))?;
        let audit=moss_voice::home_backup_audit::audit_selected_file(&path)
            .map_err(|_|serde_json::json!({"code":"home_audit_invalid","message":"文件不符合只读安全审计条件（路径、类型或大小）"}))?;
        let category=match audit.risk {
            moss_voice::home_backup_audit::BackupRisk::SuspectedLegacyCredential=>"suspected_legacy_credential",
            moss_voice::home_backup_audit::BackupRisk::EncryptedCredentialMaterial=>"encrypted_credential_material",
            moss_voice::home_backup_audit::BackupRisk::UnknownTreatAsSensitive=>"unknown_treat_as_sensitive",
        };
        Ok(serde_json::json!({"risk":category,"inspected_bytes":audit.inspected_bytes,"safe_to_share":false}))
    }).await.map_err(|_|serde_json::json!({"code":"home_audit_failed","message":"历史文件审计未完成"}))?;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}

#[tauri::command]
pub async fn voice_home_backup_restore(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_backup_restore(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_home_token_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_token_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_home_voice_consent(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_voice_consent(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_home_discover(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.home_discover()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
#[tauri::command]
pub async fn voice_home_approvals(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.home_approvals()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
#[tauri::command]
pub async fn voice_home_approval_cancel(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_approval_cancel(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_home_control_consent(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_control_consent(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_speaker_manage(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.speaker_manage(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_firmware_status(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.firmware_status()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
/// 弹出文件选择框导入固件；路径和内容都不来自网页。
#[tauri::command]
pub async fn voice_firmware_import(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = {
        let picker = app.clone();
        let picked = tauri::async_runtime::spawn_blocking(move || {
            use tauri_plugin_dialog::DialogExt;
            picker
                .dialog()
                .file()
                .set_title("选择 ESP32 应用固件（.bin）")
                .add_filter("固件", &["bin"])
                .blocking_pick_file()
                .and_then(|p| p.into_path().ok())
        })
        .await
        .map_err(|_| serde_json::json!({"code":"firmware_invalid","message":"选择固件失败"}))?;
        match picked {
            None => Ok(Value::Null),
            Some(path) => imp::run(app, move |voice| voice.firmware_import(&path)).await,
        }
    };
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
#[tauri::command]
pub async fn voice_firmware_manage(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.firmware_manage(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
/// n61 设备唤醒词（n67 起与固件一样在主窗口语音中心操作，来源固定为本进程后端上的主窗口）。
#[tauri::command]
pub async fn voice_wakeword_status(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.wakeword_status()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
/// 弹出文件选择框导入基础资源包；路径和内容都不来自网页。
#[tauri::command]
pub async fn voice_wakeword_import_base(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = {
        let picker = app.clone();
        let picked = tauri::async_runtime::spawn_blocking(move || {
            use tauri_plugin_dialog::DialogExt;
            picker
                .dialog()
                .file()
                .set_title("选择小智资源包（assets.bin）")
                .add_filter("资源包", &["bin"])
                .blocking_pick_file()
                .and_then(|p| p.into_path().ok())
        })
        .await
        .map_err(|_| serde_json::json!({"code":"wakeword_invalid","message":"选择资源包失败"}))?;
        match picked {
            None => Ok(Value::Null),
            Some(path) => imp::run(app, move |voice| voice.wakeword_import_base(&path)).await,
        }
    };
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
#[tauri::command]
pub async fn voice_wakeword_manage(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.wakeword_manage(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_home_approval_approve(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_approval_approve(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}
#[tauri::command]
pub async fn voice_vision_status(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| Ok(voice.vision_status())).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}
#[tauri::command]
pub async fn voice_vision_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.vision_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, input));
    result
}
#[tauri::command]
pub async fn voice_vision_test(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.vision_test()).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

#[tauri::command]
pub async fn voice_plugins_status(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| Ok(voice.plugins_status())).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}

#[tauri::command]
pub async fn voice_plugins_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.plugins_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}

#[tauri::command]
pub async fn voice_memory_list(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.memory_list()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}

#[tauri::command]
pub async fn voice_memory_set_enabled(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    enabled: bool,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.memory_enable(enabled)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, enabled));
    result
}

#[tauri::command]
pub async fn voice_memory_save(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    device_id: String,
    expected_revision: i64,
    summary: String,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| {
        voice.memory_edit(&device_id, expected_revision, &summary)
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, device_id, expected_revision, summary));
    result
}

#[tauri::command]
pub async fn voice_memory_clear(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    device_id: String,
    expected_revision: i64,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| {
        voice.memory_edit(&device_id, expected_revision, "")
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, device_id, expected_revision));
    result
}

/// 前端调用的内置语音命令（build.rs 登记与 capability 授权的测试会核对这张表）。
#[cfg(test)]
pub const COMMANDS: [&str; 47] = [
    "voice_firmware_status",
    "voice_firmware_import",
    "voice_firmware_manage",
    "voice_speaker_manage",
    "voice_music_pick_directory",
    "voice_music_status",
    "voice_music_save",
    "voice_home_status",
    "voice_home_save",
    "voice_home_backup_export",
    "voice_home_backup_audit",
    "voice_home_backup_restore",
    "voice_home_token_save",
    "voice_home_voice_consent",
    "voice_home_approvals",
    "voice_home_approval_cancel",
    "voice_home_control_consent",
    "voice_home_approval_approve",
    "voice_home_discover",
    "voice_agent_status",
    "voice_agent_save",
    "voice_agent_stop",
    "voice_vision_status",
    "voice_vision_save",
    "voice_vision_test",
    "voice_plugins_status",
    "voice_plugins_save",
    "voice_memory_list",
    "voice_memory_set_enabled",
    "voice_memory_save",
    "voice_memory_clear",
    "voice_builtin_status",
    "voice_builtin_start",
    "voice_builtin_stop",
    "voice_diagnostics_export",
    "voice_colab_export_notebook",
    "voice_device_set_status",
    "voice_device_rename",
    "voice_device_request",
    "voice_builtin_models_test",
    "voice_builtin_models_list",
    "voice_builtin_models_save",
    "voice_local_save",
    "voice_local_model_download",
    "voice_local_model_cancel",
    "voice_local_model_delete",
    "voice_local_laya_setup",
];
