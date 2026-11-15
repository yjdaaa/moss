//! 设置文件与密钥读写、密钥与设置的原子提交、时间与数据库错误映射等通用小工具。

use super::*;

pub(super) fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// 字符串版本（设备登记、记忆等内部接口用）：大白话在前，脱敏后的原始错误在括号里。
pub(super) fn db_error(error: impl std::fmt::Debug) -> String {
    format!(
        "MOSS 本机设置数据库读写失败（原始信息：{}）",
        moss_voice_providers::scrub_diagnostic_text(&format!("{error:?}"))
    )
}

/// 本机数据库失败：界面只说大白话，原始错误（Debug 文本，已脱敏）放进 `detail`（2026-11-07 n68）。
pub(super) fn db_failure(error: impl std::fmt::Debug) -> CenterError {
    internal("MOSS 本机设置数据库读写失败，请重启 MOSS 再试；一直失败请导出诊断包反馈").with_detail(
        moss_voice_providers::scrub_diagnostic_text(&format!("{error:?}")),
    )
}

pub(super) fn internal(message: impl Into<String>) -> CenterError {
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

impl BuiltinVoice {
    pub(super) fn hotwords(&self, local: &BuiltinLocal) -> Vec<String> {
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
    pub(super) fn settings_file(&self) -> std::path::PathBuf {
        self.host.runtime_root.path.join("settings.json")
    }

    pub(super) fn settings(&self) -> moss_voice::VoiceSettings {
        moss_voice::settings::load(&self.settings_file())
    }

    pub(super) fn secret(&self, section: Section) -> Result<Option<String>, CenterError> {
        let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
        db.get_secret(builtin_secret_name(section))
            .map_err(db_failure)
    }
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
