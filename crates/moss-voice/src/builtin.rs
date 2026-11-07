//! 内置语音服务的模型配置（内置语音技术书 §5.1 / §6，NV5）。
//!
//! - 地址、模型、音色、语速存 `settings.json` 的 `voice.builtin_models`；
//! - 密钥存 SQLite `app_secrets`（[`builtin_secret_name`]），本模块只产出“保留 / 替换 / 删除”的计划，由宿主写库；
//! - 页面只看到“已设置 / 未设置”，任何返回值都不含密钥原文。
//!
//! 校验与地址规范化用 [`models::resolve_builtin`]：OpenAI 兼容沿用 P4 模型向导的规则（大模型填基础地址，识别 / 合成
//! 补全为完整地址）；识别与合成还可选豆包流式（NV7，地址与资源 ID 留空用默认值）。

use serde_json::{Value, json};

use crate::models::{self, ModelsInput, ResolvedSection, Section, SectionInput};
use crate::settings::{BuiltinModels, BuiltinProvider, builtin_secret_name};

/// 一个分区的密钥如何处理。
#[derive(Clone, PartialEq, Eq)]
pub enum KeyChange {
    /// 输入框留空：保留已保存的密钥
    Keep,
    Set(String),
    Clear,
}

impl std::fmt::Debug for KeyChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Keep => "Keep",
            Self::Set(_) => "Set(***)",
            Self::Clear => "Clear",
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SavePlan {
    pub models: BuiltinModels,
    pub keys: Vec<(Section, KeyChange)>,
}

/// 根据页面输入计算新的模型设置与密钥变化。分区缺省或全部留空时不修改；`clear` 中的分区删除设置与密钥。
pub fn plan_save(
    current: &BuiltinModels,
    input: &ModelsInput,
    clear: &[Section],
) -> Result<SavePlan, String> {
    let mut models = current.clone();
    let mut keys = Vec::new();
    for section in Section::ALL {
        if clear.contains(&section) {
            models.set(section, None);
            keys.push((section, KeyChange::Clear));
            continue;
        }
        let Some(section_input) = input.section(section) else {
            continue;
        };
        let Some(resolved) = models::resolve_builtin(section, section_input)? else {
            continue;
        };
        models.set(
            section,
            Some(BuiltinProvider {
                kind: resolved.kind,
                url: resolved.url.clone(),
                model: resolved.model.clone(),
                voice: resolved.voice.clone(),
                speed: resolved.speed as f32,
            }),
        );
        keys.push((
            section,
            if resolved.api_key.is_empty() {
                KeyChange::Keep
            } else {
                KeyChange::Set(resolved.api_key)
            },
        ));
    }
    Ok(SavePlan { models, keys })
}

/// 测试连接用的分区：输入的密钥留空时用已保存的密钥（`stored`），保证“保存的就是测试通过的”。
pub fn resolve_for_test(
    section: Section,
    input: &SectionInput,
    stored: Option<String>,
) -> Result<ResolvedSection, String> {
    let mut resolved = models::resolve_builtin(section, input)?
        .ok_or_else(|| format!("{}：请填写地址与模型", section.label()))?;
    if resolved.api_key.is_empty() {
        resolved.api_key = stored.unwrap_or_default();
    }
    Ok(resolved)
}

/// 读取模型列表用的分区参数：输入的密钥留空时用已保存的密钥（`stored`），不要求先填模型名与音色。
pub fn resolve_for_list(
    section: Section,
    input: &SectionInput,
    stored: Option<String>,
) -> Result<models::ResolvedListRequest, String> {
    let mut resolved = models::resolve_list_request(section, input)?;
    if resolved.api_key.is_empty() {
        resolved.api_key = stored.unwrap_or_default();
    }
    Ok(resolved)
}

/// 页面显示用（不含密钥）：每个分区的地址、模型、音色、语速与密钥是否已设置。
pub fn status_view(models: &BuiltinModels, key_set: impl Fn(&str) -> bool) -> Value {
    let mut view = serde_json::Map::new();
    for section in Section::ALL {
        let provider = models.get(section);
        let mut entry = json!({
            "configured": provider.is_some(),
            "key_set": key_set(builtin_secret_name(section)),
        });
        if let Some(provider) = provider {
            entry["kind"] = json!(provider.kind);
            entry["url"] = json!(provider.url);
            entry["model"] = json!(provider.model);
            if section == Section::Tts {
                entry["voice"] = json!(provider.voice);
                entry["speed"] = json!(provider.speed);
            }
        }
        view.insert(section.as_str().into(), entry);
    }
    view.insert("complete".into(), json!(models.complete()));
    Value::Object(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(url: &str, key: &str, model: &str) -> SectionInput {
        SectionInput {
            url: url.into(),
            api_key: key.into(),
            model: model.into(),
            ..SectionInput::default()
        }
    }

    #[test]
    fn save_plan_normalizes_urls_and_keeps_blank_keys() {
        let mut models_input = ModelsInput {
            llm: Some(input(
                "https://llm.test/v1/chat/completions",
                "sk-new-llm",
                "qwen",
            )),
            asr: Some(input("https://asr.test/v1/", "", "whisper-1")),
            tts: Some(SectionInput {
                voice: "alloy".into(),
                speed: Some(1.2),
                ..input("https://tts.test/v1", "", "tts-1")
            }),
        };
        let plan = plan_save(&BuiltinModels::default(), &models_input, &[]).unwrap();
        assert_eq!(plan.models.llm.as_ref().unwrap().url, "https://llm.test/v1");
        assert_eq!(
            plan.models.asr.as_ref().unwrap().url,
            "https://asr.test/v1/audio/transcriptions"
        );
        let tts = plan.models.tts.as_ref().unwrap();
        assert_eq!(
            (tts.url.as_str(), tts.voice.as_str(), tts.speed),
            ("https://tts.test/v1/audio/speech", "alloy", 1.2)
        );
        assert!(plan.models.complete());
        assert_eq!(
            plan.keys,
            vec![
                (Section::Llm, KeyChange::Set("sk-new-llm".into())),
                (Section::Asr, KeyChange::Keep),
                (Section::Tts, KeyChange::Keep)
            ]
        );
        assert!(
            !format!("{plan:?}").contains("sk-new-llm"),
            "调试输出不含密钥"
        );

        // 只改一个分区，其余不动；清除的分区删除设置与密钥
        models_input = ModelsInput {
            asr: Some(input("https://asr2.test/v1", "", "sense")),
            ..ModelsInput::default()
        };
        let next = plan_save(&plan.models, &models_input, &[Section::Tts]).unwrap();
        assert_eq!(next.models.llm, plan.models.llm);
        assert_eq!(next.models.asr.as_ref().unwrap().model, "sense");
        assert!(next.models.tts.is_none() && !next.models.complete());
        assert_eq!(
            next.keys,
            vec![
                (Section::Asr, KeyChange::Keep),
                (Section::Tts, KeyChange::Clear)
            ]
        );
        // 全部留空的分区不修改
        let blank = ModelsInput {
            llm: Some(input("", "", "")),
            ..ModelsInput::default()
        };
        assert_eq!(
            plan_save(&plan.models, &blank, &[]).unwrap().models,
            plan.models
        );
        // 校验错误原样返回
        let bad = ModelsInput {
            llm: Some(input("ftp://x", "", "m")),
            ..ModelsInput::default()
        };
        assert!(
            plan_save(&BuiltinModels::default(), &bad, &[])
                .unwrap_err()
                .contains("http")
        );

        // 豆包流式（NV7）：地址与资源 ID 用默认值，接口类型写入设置
        let doubao = ModelsInput {
            tts: Some(SectionInput {
                kind: crate::models::ProviderKind::Doubao,
                voice: "zh_female_test".into(),
                ..input("", "doubao-key", "")
            }),
            ..ModelsInput::default()
        };
        let plan = plan_save(&BuiltinModels::default(), &doubao, &[]).unwrap();
        let tts = plan.models.tts.unwrap();
        assert_eq!(tts.kind, crate::models::ProviderKind::Doubao);
        assert_eq!(
            (tts.url.as_str(), tts.model.as_str()),
            (
                crate::models::DOUBAO_TTS_URL,
                crate::models::DOUBAO_TTS_RESOURCE
            )
        );
        assert_eq!(
            plan.keys,
            vec![(Section::Tts, KeyChange::Set("doubao-key".into()))]
        );
    }

    #[test]
    fn test_resolution_uses_stored_key_when_blank_and_status_hides_keys() {
        let resolved = resolve_for_test(
            Section::Llm,
            &input("https://llm.test/v1", "", "m"),
            Some("sk-stored".into()),
        )
        .unwrap();
        assert_eq!(resolved.api_key, "sk-stored");
        let typed = resolve_for_test(
            Section::Llm,
            &input("https://llm.test/v1", "sk-typed", "m"),
            Some("sk-stored".into()),
        )
        .unwrap();
        assert_eq!(typed.api_key, "sk-typed");
        assert!(resolve_for_test(Section::Asr, &input("", "", ""), None).is_err());
        let list_req = resolve_for_list(
            Section::Tts,
            &input("https://tts.test/v1/audio/speech", "", ""),
            Some("sk-stored-tts".into()),
        )
        .unwrap();
        assert_eq!(
            (list_req.url.as_str(), list_req.api_key.as_str()),
            ("https://tts.test/v1/models", "sk-stored-tts")
        );

        let models = BuiltinModels {
            llm: Some(BuiltinProvider {
                kind: crate::models::ProviderKind::Openai,
                url: "https://llm.test/v1".into(),
                model: "m".into(),
                voice: String::new(),
                speed: 1.0,
            }),
            ..BuiltinModels::default()
        };
        let view = status_view(&models, |name| name == "voice.llm.main");
        assert_eq!(view["llm"]["configured"], true);
        assert_eq!(view["llm"]["kind"], "openai");
        assert_eq!(view["llm"]["key_set"], true);
        assert_eq!(view["llm"]["url"], "https://llm.test/v1");
        assert_eq!(view["asr"]["configured"], false);
        assert_eq!(view["complete"], false);
        assert!(!view.to_string().contains("sk-"));
    }
}
