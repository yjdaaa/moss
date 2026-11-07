//! NV15 mock-only Home Assistant service and policy fixtures.
//!
//! SAFETY: This module is NOT a production HA connector. It cannot send a
//! control request: plans are inert data pending a dedicated HomeControl
//! approval transaction, trusted credentials and actual service support.
//! Tests use synthetic states/services plus a local loopback fake HA server.
//! No real devices, user tokens, network endpoints, or smart homes are read.
#![allow(dead_code)]

#[cfg(test)]
mod approval_mock;
pub(crate) mod approved;
#[cfg(test)]
mod control_mock;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(crate) mod https_transport;
#[cfg(test)]
mod mock_http;
#[cfg(test)]
mod observe_mock;

use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicBool, Ordering},
};

use serde::Deserialize;
use serde_json::{Value, json};

const MAX_SCHEMA_BYTES: usize = 1024 * 1024;
const MAX_STATES: usize = 1000;
const MAX_SERVICES: usize = 2000;
const MAX_ENTITIES: usize = 128;
const MAX_ID_CHARS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HomeDeny {
    InvalidConfig,
    InvalidSchema,
    UnknownEntity,
    Unauthorized,
    DuplicateAlias,
    ServiceUnavailable,
    InvalidParameters,
    ApprovalRequired,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HomePolicy {
    Ask,
    Allow,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AllowedMedia {
    pub id: String,
    pub media_type: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AllowedEntity {
    pub entity_id: String,
    pub alias: String,
    pub enabled: bool,
    pub policy: HomePolicy,
    #[serde(default)]
    pub media: Vec<AllowedMedia>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HomeAction {
    TurnOn,
    TurnOff,
    SetTemperature,
    Lock,
    Unlock,
    ArmHome,
    ArmAway,
    Disarm,
    OpenCover,
    CloseCover,
    StopCover,
    PlayMedia,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HomeState {
    pub entity_id: String,
    pub state: String,
    pub last_updated: String,
    pub attributes: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HomeControlPlan {
    pub action: HomeAction,
    pub arguments: Value,
    pub service_path: String,
    pub body: Value,
    pub entity_id: String,
    pub policy_revision: u64,
    pub credential_revision: u64,
    /// Only a dedicated future HomeControl executor may consume an approval.
    /// Model/tool callers must never send this inert plan directly.
    pub requires_dedicated_approval: bool,
}

pub(crate) struct HomeCatalog {
    entities: HashMap<String, (AllowedEntity, HomeState)>,
    aliases: HashMap<String, String>,
    services: HashSet<(String, String)>,
    policy_revision: u64,
    credential_revision: u64,
}

fn domain_of(entity: &str) -> Option<&str> {
    let (domain, id) = entity.split_once('.')?;
    if !matches!(
        domain,
        "light" | "switch" | "climate" | "lock" | "alarm_control_panel" | "cover" | "media_player"
    ) || id.is_empty()
        || entity.len() > MAX_ID_CHARS
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return None;
    }
    Some(domain)
}

fn allowed_alias(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= 64 && !name.chars().any(char::is_control)
}
fn safe_media(item: &AllowedMedia) -> bool {
    // Whitelisted local HA library identifiers only. No URL, path, templating,
    // arbitrary MIME or credential can be supplied by an LLM.
    matches!(item.media_type.as_str(), "music" | "playlist")
        && item.id.starts_with("library_")
        && item.id.len() <= 100
        && item
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}
/// Never expose arbitrary HA attributes to LLM tools. Credentials, PINs,
/// access codes, nested templates and manufacturer metadata are dropped.
fn public_attributes(attributes: &serde_json::Map<String, Value>) -> Value {
    let mut safe = serde_json::Map::new();
    for (key, value) in attributes {
        match key.as_str() {
            "min_temp" | "max_temp" | "temperature" | "current_temperature" => {
                if value.as_f64().is_some_and(f64::is_finite) {
                    safe.insert(key.clone(), value.clone());
                }
            }
            "brightness" => {
                if value.as_u64().is_some_and(|v| v <= 255) {
                    safe.insert(key.clone(), value.clone());
                }
            }
            "unit_of_measurement" => {
                if value
                    .as_str()
                    .is_some_and(|v| v.len() <= 12 && !v.chars().any(char::is_control))
                {
                    safe.insert(key.clone(), value.clone());
                }
            }
            _ => {}
        }
    }
    Value::Object(safe)
}

fn read_json(bytes: &[u8]) -> Result<Value, HomeDeny> {
    if bytes.is_empty() || bytes.len() > MAX_SCHEMA_BYTES {
        return Err(HomeDeny::InvalidSchema);
    }
    serde_json::from_slice(bytes).map_err(|_| HomeDeny::InvalidSchema)
}

impl HomeCatalog {
    /// Both JSON payloads must be obtained by the trusted desktop sync path.
    /// The allowlist is desktop-authored, NEVER generated by a model.
    pub(crate) fn from_snapshots(
        states_json: &[u8],
        services_json: &[u8],
        allowlist: Vec<AllowedEntity>,
        policy_revision: u64,
        credential_revision: u64,
    ) -> Result<Self, HomeDeny> {
        if policy_revision == 0 || credential_revision == 0 || allowlist.len() > MAX_ENTITIES {
            return Err(HomeDeny::InvalidConfig);
        }
        let states = read_json(states_json)?;
        let states = states.as_array().ok_or(HomeDeny::InvalidSchema)?;
        if states.len() > MAX_STATES {
            return Err(HomeDeny::InvalidSchema);
        }
        let mut present = HashMap::new();
        for record in states {
            let id = record
                .get("entity_id")
                .and_then(Value::as_str)
                .ok_or(HomeDeny::InvalidSchema)?;
            if domain_of(id).is_none() {
                continue;
            }
            let state = record
                .get("state")
                .and_then(Value::as_str)
                .ok_or(HomeDeny::InvalidSchema)?;
            let updated = record
                .get("last_updated")
                .and_then(Value::as_str)
                .ok_or(HomeDeny::InvalidSchema)?;
            let attributes = record
                .get("attributes")
                .and_then(Value::as_object)
                .ok_or(HomeDeny::InvalidSchema)?;
            if state.len() > 128
                || updated.len() > 64
                || state.chars().any(char::is_control)
                || present.contains_key(id)
            {
                return Err(HomeDeny::InvalidSchema);
            }
            present.insert(
                id.to_owned(),
                HomeState {
                    entity_id: id.to_owned(),
                    state: state.to_owned(),
                    last_updated: updated.to_owned(),
                    attributes: public_attributes(attributes),
                },
            );
        }
        let services = read_json(services_json)?;
        let services = services.as_array().ok_or(HomeDeny::InvalidSchema)?;
        if services.len() > MAX_SERVICES {
            return Err(HomeDeny::InvalidSchema);
        }
        let mut available = HashSet::new();
        for group in services {
            let domain = group
                .get("domain")
                .and_then(Value::as_str)
                .ok_or(HomeDeny::InvalidSchema)?;
            let listed = group
                .get("services")
                .and_then(Value::as_object)
                .ok_or(HomeDeny::InvalidSchema)?;
            if listed.len() > 100 {
                return Err(HomeDeny::InvalidSchema);
            }
            for service in listed.keys() {
                if service.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                    && service.len() <= 64
                {
                    available.insert((domain.to_owned(), service.to_owned()));
                }
            }
        }
        // Reserve every enabled entity ID before registering aliases, including
        // entities missing from this snapshot. An alias must never redirect a
        // literal entity ID to a different device.
        let selected_ids: HashMap<String, String> = allowlist
            .iter()
            .filter(|item| item.enabled)
            .map(|item| (item.entity_id.to_ascii_lowercase(), item.entity_id.clone()))
            .collect();
        let mut entities = HashMap::new();
        let mut aliases = HashMap::new();
        let mut ids = HashSet::new();
        for item in allowlist {
            let Some(domain) = domain_of(&item.entity_id) else {
                return Err(HomeDeny::InvalidConfig);
            };
            if !ids.insert(item.entity_id.to_ascii_lowercase())
                || !allowed_alias(&item.alias)
                || (matches!(domain, "lock" | "alarm_control_panel" | "cover")
                    && item.policy == HomePolicy::Allow)
                || (!item.media.is_empty() && domain != "media_player")
                || item.media.len() > 32
                || item.media.iter().any(|m| !safe_media(m))
            {
                return Err(HomeDeny::InvalidConfig);
            }
            if !item.enabled {
                continue;
            }
            let alias_key = item.alias.trim().to_lowercase();
            // Even a not-yet-listed entity ID must never be a voice alias
            // for another entity: a future HA sync could otherwise make an
            // explicitly requested device ID target the wrong device.
            if (domain_of(&alias_key).is_some() && alias_key != item.entity_id.to_ascii_lowercase())
                || selected_ids
                    .get(&alias_key)
                    .is_some_and(|id| id != &item.entity_id)
            {
                return Err(HomeDeny::DuplicateAlias);
            }
            if aliases.contains_key(&alias_key) {
                return Err(HomeDeny::DuplicateAlias);
            }
            aliases.insert(alias_key, item.entity_id.clone());
            if let Some(state) = present.get(&item.entity_id) {
                entities.insert(item.entity_id.clone(), (item, state.clone()));
            }
        }
        Ok(Self {
            entities,
            aliases,
            services: available,
            policy_revision,
            credential_revision,
        })
    }
    /// Repeat all current allowlist, service and parameter checks immediately
    /// before consuming a dedicated approval; revisions must also match.
    pub(crate) fn revalidate_plan(
        &self,
        previous: &HomeControlPlan,
        permit: &AtomicBool,
    ) -> Result<(), HomeDeny> {
        if !permit.load(Ordering::Acquire) {
            return Err(HomeDeny::Cancelled);
        }
        if previous.policy_revision != self.policy_revision
            || previous.credential_revision != self.credential_revision
        {
            return Err(HomeDeny::InvalidConfig);
        }
        let now = self.plan(
            &previous.entity_id,
            previous.action,
            &previous.arguments,
            permit,
        )?;
        if &now != previous {
            return Err(HomeDeny::InvalidConfig);
        }
        Ok(())
    }

    pub(crate) fn selected_count(&self) -> usize {
        self.entities.len()
    }
    pub(crate) fn lookup(&self, query: &str) -> Result<&HomeState, HomeDeny> {
        let key = query.trim().to_lowercase();
        let id = self
            .entities
            .get(query)
            .map(|_| query)
            .or_else(|| self.aliases.get(&key).map(String::as_str))
            .ok_or(HomeDeny::UnknownEntity)?;
        let state = &self.entities.get(id).ok_or(HomeDeny::UnknownEntity)?.1;
        Ok(state)
    }
    /// Pure inert service PLAN. It is never sent without a future audited,
    /// one-time HomeControl approval service; all actions are Ask for now.
    pub(crate) fn plan(
        &self,
        target: &str,
        action: HomeAction,
        parameters: &Value,
        permit: &AtomicBool,
    ) -> Result<HomeControlPlan, HomeDeny> {
        if !permit.load(Ordering::Acquire) {
            return Err(HomeDeny::Cancelled);
        }
        let state = self.lookup(target)?;
        if state.state == "unavailable" || state.state == "unknown" {
            return Err(HomeDeny::ServiceUnavailable);
        }
        let (entry, _) = self
            .entities
            .get(&state.entity_id)
            .ok_or(HomeDeny::UnknownEntity)?;
        let domain = domain_of(&state.entity_id).ok_or(HomeDeny::InvalidConfig)?;
        let service = match (domain, action) {
            ("light" | "switch", HomeAction::TurnOn) => "turn_on",
            ("light" | "switch", HomeAction::TurnOff) => "turn_off",
            ("climate", HomeAction::SetTemperature) => "set_temperature",
            ("lock", HomeAction::Lock) => "lock",
            ("lock", HomeAction::Unlock) => "unlock",
            ("alarm_control_panel", HomeAction::ArmHome) => "alarm_arm_home",
            ("alarm_control_panel", HomeAction::ArmAway) => "alarm_arm_away",
            ("alarm_control_panel", HomeAction::Disarm) => "alarm_disarm",
            ("cover", HomeAction::OpenCover) => "open_cover",
            ("cover", HomeAction::CloseCover) => "close_cover",
            ("cover", HomeAction::StopCover) => "stop_cover",
            ("media_player", HomeAction::PlayMedia) => "play_media",
            _ => return Err(HomeDeny::InvalidParameters),
        };
        if !self
            .services
            .contains(&(domain.to_owned(), service.to_owned()))
        {
            return Err(HomeDeny::ServiceUnavailable);
        }
        let args = parameters.as_object().ok_or(HomeDeny::InvalidParameters)?;
        let mut body = json!({"entity_id":state.entity_id});
        match action {
            HomeAction::TurnOn if domain == "light" => {
                if args.keys().any(|key| key != "brightness") {
                    return Err(HomeDeny::InvalidParameters);
                }
                if let Some(brightness) = args.get("brightness") {
                    let n = brightness.as_u64().ok_or(HomeDeny::InvalidParameters)?;
                    if n > 255 {
                        return Err(HomeDeny::InvalidParameters);
                    }
                    body["brightness"] = json!(n);
                }
            }
            HomeAction::SetTemperature => {
                if args.len() != 1 || !args.contains_key("temperature") {
                    return Err(HomeDeny::InvalidParameters);
                }
                let value = args["temperature"]
                    .as_f64()
                    .ok_or(HomeDeny::InvalidParameters)?;
                let min = state.attributes["min_temp"]
                    .as_f64()
                    .ok_or(HomeDeny::InvalidParameters)?;
                let max = state.attributes["max_temp"]
                    .as_f64()
                    .ok_or(HomeDeny::InvalidParameters)?;
                if !value.is_finite()
                    || !min.is_finite()
                    || !max.is_finite()
                    || min > max
                    || value < min
                    || value > max
                {
                    return Err(HomeDeny::InvalidParameters);
                }
                body["temperature"] = json!(value);
            }
            HomeAction::PlayMedia => {
                if args.len() != 1 || !args.contains_key("library_id") {
                    return Err(HomeDeny::InvalidParameters);
                }
                let id = args["library_id"]
                    .as_str()
                    .ok_or(HomeDeny::InvalidParameters)?;
                let saved = entry
                    .media
                    .iter()
                    .find(|media| media.id == id)
                    .ok_or(HomeDeny::InvalidParameters)?;
                body["media_content_id"] = json!(saved.id);
                body["media_content_type"] = json!(saved.media_type);
            }
            _ if !args.is_empty() => return Err(HomeDeny::InvalidParameters),
            _ => {}
        }
        if !permit.load(Ordering::Acquire) {
            return Err(HomeDeny::Cancelled);
        }
        Ok(HomeControlPlan {
            action,
            arguments: parameters.clone(),
            service_path: format!("/api/services/{domain}/{service}"),
            body,
            entity_id: state.entity_id.clone(),
            policy_revision: self.policy_revision,
            credential_revision: self.credential_revision,
            requires_dedicated_approval: true,
        })
    }
}

/// Deliberate deny-by-default mock-only executor stub. No POST exists and
/// user approval, even if confirmed elsewhere, must not bypass NV15-3.
pub(crate) fn execute_without_home_control_approval(
    _plan: &HomeControlPlan,
) -> Result<(), HomeDeny> {
    Err(HomeDeny::ApprovalRequired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    pub(super) fn sample_states() -> Vec<u8> {
        serde_json::to_vec(&json!([
            {"entity_id":"light.desk","state":"off","last_updated":"2026-10-11T00:00:00Z","attributes":{"brightness":0}},
            {"entity_id":"switch.fan","state":"on","last_updated":"2026-10-11T00:00:00Z","attributes":{}},
            {"entity_id":"climate.room","state":"heat","last_updated":"2026-10-11T00:00:00Z","attributes":{"min_temp":17.0,"max_temp":28.0}},
            {"entity_id":"lock.front","state":"locked","last_updated":"2026-10-11T00:00:00Z","attributes":{}},
            {"entity_id":"alarm_control_panel.home","state":"disarmed","last_updated":"2026-10-11T00:00:00Z","attributes":{}},
            {"entity_id":"cover.blinds","state":"closed","last_updated":"2026-10-11T00:00:00Z","attributes":{}},
            {"entity_id":"media_player.room","state":"idle","last_updated":"2026-10-11T00:00:00Z","attributes":{}},
            {"entity_id":"light.unavailable","state":"unavailable","last_updated":"2026-10-11T00:00:00Z","attributes":{}},
            {"entity_id":"light.not_selected","state":"off","last_updated":"2026-10-11T00:00:00Z","attributes":{"private":"never-share"}}
        ])).unwrap()
    }
    pub(super) fn sample_services() -> Vec<u8> {
        serde_json::to_vec(&json!([
            {"domain":"light","services":{"turn_on":{},"turn_off":{}}},
            {"domain":"switch","services":{"turn_on":{},"turn_off":{}}},
            {"domain":"climate","services":{"set_temperature":{}}},
            {"domain":"lock","services":{"lock":{},"unlock":{}}},
            {"domain":"alarm_control_panel","services":{"alarm_arm_home":{},"alarm_arm_away":{},"alarm_disarm":{}}},
            {"domain":"cover","services":{"open_cover":{},"close_cover":{},"stop_cover":{}}},
            {"domain":"media_player","services":{"play_media":{}}}
        ])).unwrap()
    }
    pub(super) fn selected() -> Vec<AllowedEntity> {
        [
            ("light.desk", "台灯"),
            ("switch.fan", "风扇"),
            ("climate.room", "空调"),
            ("lock.front", "门锁"),
            ("alarm_control_panel.home", "警报"),
            ("cover.blinds", "窗帘"),
            ("media_player.room", "音箱"),
            ("light.unavailable", "离线灯"),
        ]
        .into_iter()
        .map(|(id, alias)| AllowedEntity {
            entity_id: id.into(),
            alias: alias.into(),
            enabled: true,
            policy: HomePolicy::Ask,
            media: if id == "media_player.room" {
                vec![AllowedMedia {
                    id: "library_demo_song".into(),
                    media_type: "music".into(),
                }]
            } else {
                vec![]
            },
        })
        .collect()
    }
    fn catalog() -> HomeCatalog {
        HomeCatalog::from_snapshots(&sample_states(), &sample_services(), selected(), 7, 9).unwrap()
    }
    #[test]
    fn unsafe_home_assistant_attributes_are_never_exposed() {
        let mut states: Value = serde_json::from_slice(&sample_states()).unwrap();
        states[0]["attributes"]["pin"] = json!("1234");
        states[0]["attributes"]["token"] = json!("NV15_SENSITIVE_CANARY");
        states[0]["attributes"]["secret_object"] = json!({"deep":"private"});
        let index = HomeCatalog::from_snapshots(
            &serde_json::to_vec(&states).unwrap(),
            &sample_services(),
            selected(),
            7,
            9,
        )
        .unwrap();
        let shown = index.lookup("台灯").unwrap();
        let serialized = serde_json::to_string(&shown.attributes).unwrap();
        assert!(!serialized.contains("NV15_SENSITIVE_CANARY"));
        assert!(!serialized.contains("pin"));
        assert!(!serialized.contains("secret_object"));
        assert_eq!(shown.attributes["brightness"], 0);
    }

    #[test]
    fn only_desktop_allowlisted_entities_visible_and_ambiguous_alias_rejected() {
        let index = catalog();
        assert_eq!(index.selected_count(), 8);
        assert_eq!(index.lookup("台灯").unwrap().state, "off");
        assert_eq!(index.lookup("light.desk").unwrap().entity_id, "light.desk");
        assert_eq!(
            index.lookup("light.not_selected"),
            Err(HomeDeny::UnknownEntity)
        );
        assert_eq!(
            index.lookup("light.desk/../../api"),
            Err(HomeDeny::UnknownEntity)
        );
        let mut items = selected();
        items[1].alias = " 台灯 ".into();
        assert!(matches!(
            HomeCatalog::from_snapshots(&sample_states(), &sample_services(), items, 7, 9),
            Err(HomeDeny::DuplicateAlias)
        ));
        let mut items = selected();
        items[3].policy = HomePolicy::Allow;
        assert!(matches!(
            HomeCatalog::from_snapshots(&sample_states(), &sample_services(), items, 7, 9),
            Err(HomeDeny::InvalidConfig)
        ));
        let mut items = selected();
        items[3].enabled = false;
        let catalog =
            HomeCatalog::from_snapshots(&sample_states(), &sample_services(), items, 7, 9).unwrap();
        assert!(matches!(
            catalog.lookup("门锁"),
            Err(HomeDeny::UnknownEntity)
        ));
    }
    #[test]
    fn aliases_cannot_shadow_another_home_entity_id() {
        let mut items = selected();
        items[0].alias = "switch.fan".into();
        assert!(matches!(
            HomeCatalog::from_snapshots(&sample_states(), &sample_services(), items, 7, 9),
            Err(HomeDeny::DuplicateAlias)
        ));
        let mut items = selected();
        items[0].alias = " SWITCH.FAN ".into();
        assert!(matches!(
            HomeCatalog::from_snapshots(&sample_states(), &sample_services(), items, 7, 9),
            Err(HomeDeny::DuplicateAlias)
        ));
        for alias in ["light.not_selected", "switch.future_device"] {
            let mut items = selected();
            items[0].alias = alias.into();
            assert!(matches!(
                HomeCatalog::from_snapshots(&sample_states(), &sample_services(), items, 7, 9),
                Err(HomeDeny::DuplicateAlias)
            ));
        }
        let mut items = selected();
        items[0].alias = "   ".into();
        assert!(matches!(
            HomeCatalog::from_snapshots(&sample_states(), &sample_services(), items, 7, 9),
            Err(HomeDeny::InvalidConfig)
        ));
        let mut items = selected();
        items[0].alias = "light.desk".into();
        let index =
            HomeCatalog::from_snapshots(&sample_states(), &sample_services(), items, 7, 9).unwrap();
        assert_eq!(index.lookup("light.desk").unwrap().entity_id, "light.desk");
        let plan = index
            .plan(
                "switch.fan",
                HomeAction::TurnOff,
                &json!({}),
                &AtomicBool::new(true),
            )
            .unwrap();
        assert_eq!(plan.body["entity_id"], "switch.fan");
    }
    #[test]
    fn fixed_action_map_never_generates_states_post_shell_template_or_extra_args() {
        let index = catalog();
        let permit = AtomicBool::new(true);
        let cases = [
            ("台灯", HomeAction::TurnOn, "/api/services/light/turn_on"),
            ("台灯", HomeAction::TurnOff, "/api/services/light/turn_off"),
            ("风扇", HomeAction::TurnOn, "/api/services/switch/turn_on"),
            (
                "空调",
                HomeAction::SetTemperature,
                "/api/services/climate/set_temperature",
            ),
            ("门锁", HomeAction::Lock, "/api/services/lock/lock"),
            ("门锁", HomeAction::Unlock, "/api/services/lock/unlock"),
            (
                "警报",
                HomeAction::ArmHome,
                "/api/services/alarm_control_panel/alarm_arm_home",
            ),
            (
                "警报",
                HomeAction::ArmAway,
                "/api/services/alarm_control_panel/alarm_arm_away",
            ),
            (
                "警报",
                HomeAction::Disarm,
                "/api/services/alarm_control_panel/alarm_disarm",
            ),
            (
                "窗帘",
                HomeAction::OpenCover,
                "/api/services/cover/open_cover",
            ),
            (
                "窗帘",
                HomeAction::CloseCover,
                "/api/services/cover/close_cover",
            ),
            (
                "窗帘",
                HomeAction::StopCover,
                "/api/services/cover/stop_cover",
            ),
            (
                "音箱",
                HomeAction::PlayMedia,
                "/api/services/media_player/play_media",
            ),
        ];
        for (alias, action, path) in cases {
            let parameters = match action {
                HomeAction::SetTemperature => json!({"temperature":22.0}),
                HomeAction::PlayMedia => json!({"library_id":"library_demo_song"}),
                _ => json!({}),
            };
            let plan = index.plan(alias, action, &parameters, &permit).unwrap();
            assert_eq!(plan.service_path, path);
            assert!(plan.requires_dedicated_approval);
            assert_eq!(
                execute_without_home_control_approval(&plan),
                Err(HomeDeny::ApprovalRequired)
            );
            assert_eq!(
                plan.body["entity_id"],
                index.lookup(alias).unwrap().entity_id
            );
            assert!(!plan.service_path.contains("states"));
        }
        assert_eq!(
            index.plan("风扇", HomeAction::Lock, &json!({}), &permit),
            Err(HomeDeny::InvalidParameters)
        );
        assert_eq!(
            index.plan("离线灯", HomeAction::TurnOn, &json!({}), &permit),
            Err(HomeDeny::ServiceUnavailable)
        );
    }
    #[test]
    fn parameter_limits_media_allowlist_temperatures_and_revocation() {
        let index = catalog();
        let allow = AtomicBool::new(true);
        for body in [
            json!({"brightness":256}),
            json!({"brightness":-1}),
            json!({"brightness":42.5}),
            json!({"color":"red"}),
            json!({"entity_id":"lock.front"}),
            json!({"brightness":true}),
        ] {
            assert_eq!(
                index.plan("台灯", HomeAction::TurnOn, &body, &allow),
                Err(HomeDeny::InvalidParameters)
            );
        }
        assert_eq!(
            index
                .plan("台灯", HomeAction::TurnOn, &json!({"brightness":0}), &allow)
                .unwrap()
                .body["brightness"],
            0
        );
        assert_eq!(
            index
                .plan(
                    "台灯",
                    HomeAction::TurnOn,
                    &json!({"brightness":255}),
                    &allow
                )
                .unwrap()
                .body["brightness"],
            255
        );
        for temp in [-273.0, 16.999, 28.001, 10000.0] {
            assert_eq!(
                index.plan(
                    "空调",
                    HomeAction::SetTemperature,
                    &json!({"temperature":temp}),
                    &allow
                ),
                Err(HomeDeny::InvalidParameters)
            );
        }
        assert_eq!(
            index
                .plan(
                    "空调",
                    HomeAction::SetTemperature,
                    &json!({"temperature":28.0}),
                    &allow
                )
                .unwrap()
                .body["temperature"],
            28.0
        );
        for body in [
            json!({"library_id":"http://evil.example"}),
            json!({"library_id":"library_unknown"}),
            json!({"media_content_id":"library_demo_song"}),
            json!({"library_id":"library_demo_song","media_content_type":"video"}),
        ] {
            assert_eq!(
                index.plan("音箱", HomeAction::PlayMedia, &body, &allow),
                Err(HomeDeny::InvalidParameters)
            );
        }
        allow.store(false, Ordering::Release);
        assert_eq!(
            index.plan("台灯", HomeAction::TurnOn, &json!({}), &allow),
            Err(HomeDeny::Cancelled)
        );
    }
    #[test]
    fn malformed_or_untrusted_snapshots_cannot_create_controls() {
        for bad in [b"{}".as_slice(), b"{}".as_slice(), b"not-json".as_slice()] {
            assert!(matches!(
                HomeCatalog::from_snapshots(bad, &sample_services(), selected(), 7, 9),
                Err(HomeDeny::InvalidSchema)
            ));
        }
        let too_large = vec![b'a'; MAX_SCHEMA_BYTES + 1];
        assert!(matches!(
            HomeCatalog::from_snapshots(&too_large, &sample_services(), selected(), 7, 9),
            Err(HomeDeny::InvalidSchema)
        ));
        assert!(
            matches!(
                HomeCatalog::from_snapshots(&sample_states(), b"[]", selected(), 7, 9),
                Ok(_)
            ),
            "missing service catalog allows read-only state but NEVER a control"
        );
        let no_services =
            HomeCatalog::from_snapshots(&sample_states(), b"[]", selected(), 7, 9).unwrap();
        assert_eq!(
            no_services.plan(
                "台灯",
                HomeAction::TurnOn,
                &json!({}),
                &AtomicBool::new(true)
            ),
            Err(HomeDeny::ServiceUnavailable)
        );
        let mut bad = selected();
        bad[0].entity_id = "light.desk/../../api/services".into();
        assert!(matches!(
            HomeCatalog::from_snapshots(&sample_states(), &sample_services(), bad, 7, 9),
            Err(HomeDeny::InvalidConfig)
        ));
        assert!(matches!(
            HomeCatalog::from_snapshots(&sample_states(), &sample_services(), selected(), 0, 9),
            Err(HomeDeny::InvalidConfig)
        ));
    }
}
