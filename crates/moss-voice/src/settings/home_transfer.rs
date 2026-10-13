//! NV15 safe, portable Home Assistant configuration template.
//!
//! This is intentionally NOT a SQLite/DPAPI credential backup. No tokens,
//! grants, approvals, network URLs, or enabled device controls are exported.
//! Restoring this template must be followed by explicit local HA login/token
//! input. The desktop import entry must clear credentials first (fail closed).
use super::{BuiltinHomeAssistant, HomeApprovalPreference, HomeOfflineError};
use serde::{Deserialize, Serialize};

pub const HOME_TEMPLATE_FORMAT: &str = "moss.homeassistant.safe_template";
pub const HOME_TEMPLATE_VERSION: u8 = 1;
const MAX_TEMPLATE_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HomeTemplateV1 {
    format: String,
    version: u8,
    /// A deliberately inactive allowlist; NEVER credentials or a host URL.
    home: BuiltinHomeAssistant,
}

pub(super) fn sanitize(mut home: BuiltinHomeAssistant) -> BuiltinHomeAssistant {
    home.enabled = false;
    home.base_url.clear();
    for item in &mut home.entities {
        item.enabled = false;
        item.policy = HomeApprovalPreference::Ask;
        // Avoid transferring private media-library identifiers as part of
        // a credentials migration. They can be selected again locally.
        item.media.clear();
    }
    home
}

/// Create an explicit, sanitized template from a validated settings snapshot.
/// Use only in an authenticated private desktop window; not HTTP exports.
pub fn export_home_template(
    home: &BuiltinHomeAssistant,
) -> Result<serde_json::Value, HomeOfflineError> {
    home.validate_offline().map_err(HomeOfflineError::Invalid)?;
    let safe = sanitize(home.clone());
    let result = serde_json::to_value(HomeTemplateV1 {
        format: HOME_TEMPLATE_FORMAT.into(),
        version: HOME_TEMPLATE_VERSION,
        home: safe,
    })
    .map_err(|_| HomeOfflineError::Invalid("无法创建安全迁移模板"))?;
    let serialized = serde_json::to_vec(&result)
        .map_err(|_| HomeOfflineError::Invalid("无法创建安全迁移模板"))?;
    if serialized.len() > MAX_TEMPLATE_BYTES {
        return Err(HomeOfflineError::Invalid("迁移模板太大"));
    }
    Ok(result)
}

/// Strict template parser. Rejects private fields and anything that would
/// enable a device, media, policy exception, URL or bearer on import.
pub fn parse_home_template(
    input: &serde_json::Value,
) -> Result<BuiltinHomeAssistant, HomeOfflineError> {
    let bytes =
        serde_json::to_vec(input).map_err(|_| HomeOfflineError::Invalid("迁移模板序列化失败"))?;
    if bytes.len() > MAX_TEMPLATE_BYTES {
        return Err(HomeOfflineError::Invalid("迁移模板太大"));
    }
    let decoded: HomeTemplateV1 = serde_json::from_slice(&bytes)
        .map_err(|_| HomeOfflineError::Invalid("迁移模板结构或字段无效"))?;
    if decoded.format != HOME_TEMPLATE_FORMAT || decoded.version != HOME_TEMPLATE_VERSION {
        return Err(HomeOfflineError::Invalid("迁移模板版本不受支持"));
    }
    decoded
        .home
        .validate_offline()
        .map_err(HomeOfflineError::Invalid)?;
    if decoded.home != sanitize(decoded.home.clone()) {
        return Err(HomeOfflineError::Invalid(
            "迁移模板不能包含启用项、网络地址或媒体标识",
        ));
    }
    Ok(decoded.home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{BuiltinHomeEntity, BuiltinHomeMedia};
    use serde_json::json;

    fn configured() -> BuiltinHomeAssistant {
        BuiltinHomeAssistant {
            enabled: false,
            base_url: "https://ha.example.test:8123/".into(),
            entities: vec![
                BuiltinHomeEntity {
                    entity_id: "light.desk".into(),
                    alias: "书桌灯".into(),
                    enabled: true,
                    policy: HomeApprovalPreference::Ask,
                    media: vec![],
                },
                BuiltinHomeEntity {
                    entity_id: "media_player.living".into(),
                    alias: "客厅音响".into(),
                    enabled: true,
                    policy: HomeApprovalPreference::Allow,
                    media: vec![BuiltinHomeMedia {
                        id: "library_private_playlist".into(),
                        media_type: "playlist".into(),
                    }],
                },
            ],
        }
    }

    #[test]
    fn nv15_portable_template_excludes_host_secrets_permissions_media_and_credentials() {
        let exported = export_home_template(&configured()).unwrap();
        let text = exported.to_string();
        for forbidden in [
            "ha.example.test",
            "8123",
            "library_private_playlist",
            "app_secrets",
            "token",
            "dpapi",
            "credential",
            "homeassistant.voice_control_consent",
        ] {
            assert!(!text.contains(forbidden), "template leaked {forbidden}");
        }
        assert_eq!(exported["format"], HOME_TEMPLATE_FORMAT);
        assert_eq!(exported["version"], HOME_TEMPLATE_VERSION);
        let decoded = parse_home_template(&exported).unwrap();
        assert!(!decoded.enabled && decoded.base_url.is_empty());
        assert_eq!(decoded.entities.len(), 2);
        assert!(decoded.entities.iter().all(|entry| !entry.enabled
            && entry.policy == HomeApprovalPreference::Ask
            && entry.media.is_empty()));
        assert_eq!(export_home_template(&decoded).unwrap(), exported);
    }
    #[test]
    fn nv15_template_rejects_unknown_fields_legacy_raw_database_malicious_reactivation() {
        let good = export_home_template(&configured()).unwrap();
        for mut bad in [
            json!({"format":"moss.homeassistant.safe_template","version":1,
                "home":{"enabled":false,"base_url":"","entities":[]},
                "token":"fake-bearer"}),
            json!({"format":"moss.homeassistant.safe_template","version":2,
                "home":{"enabled":false,"base_url":"","entities":[]}}),
            json!({"app_secrets":{"voice.homeassistant.token":"fake"}}),
            json!({"format":"moss.homeassistant.safe_template","version":1,
                "home":{"enabled":true,"base_url":"","entities":[]}}),
        ] {
            assert!(parse_home_template(&bad).is_err());
            bad["version"] = json!(0);
            assert!(parse_home_template(&bad).is_err());
        }
        for (path, value) in [
            ("/home/base_url", json!("https://user:password@ha.test")),
            ("/home/entities/0/enabled", json!(true)),
            ("/home/entities/0/policy", json!("allow")),
            (
                "/home/entities/1/media",
                json!([{"id":"library_secret","media_type":"music"}]),
            ),
        ] {
            let mut tampered = good.clone();
            assert!(tampered.pointer_mut(path).is_some());
            *tampered.pointer_mut(path).unwrap() = value;
            assert!(parse_home_template(&tampered).is_err(), "{path}");
        }
        let mut extra = good.clone();
        extra["home"]["entities"][0]["token"] = json!("not-a-real-secret");
        assert!(parse_home_template(&extra).is_err());
        let mut huge = good.clone();
        huge["home"]["entities"][0]["alias"] = json!("a".repeat(MAX_TEMPLATE_BYTES));
        assert!(parse_home_template(&huge).is_err());
    }
}
