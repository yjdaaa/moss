//! NV15 trusted desktop-initiated *read-only* HA discovery. No service POST,
//! arbitrary URL forwarding, control approval or voice tool is provided here.
use crate::settings::BuiltinHomeAssistant;
use reqwest::{StatusCode, Url, blocking::Client, header, redirect::Policy};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    time::Duration,
};

const MAX_RESPONSE: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryError {
    InvalidSettings,
    MissingCredential,
    Unauthorized,
    Unavailable,
    InvalidResponse,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DiscoveredEntity {
    pub entity_id: String,
    pub state: String,
    pub last_updated: String,
    pub attributes: Value,
    pub actions_available: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Discovery {
    pub entities: Vec<DiscoveredEntity>,
    pub policy_revision: u64,
    pub credential_revision: u64,
}

fn endpoint(base: &str, resource: &str) -> Result<Url, DiscoveryError> {
    if !matches!(resource, "/api/states" | "/api/services") {
        return Err(DiscoveryError::InvalidSettings);
    }
    let url = Url::parse(base).map_err(|_| DiscoveryError::InvalidSettings)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(DiscoveryError::InvalidSettings);
    }
    url.join(resource)
        .map_err(|_| DiscoveryError::InvalidSettings)
}

fn allowed_ha_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            v.is_loopback() || (o[0]==10) ||
                (o[0]==172 && (16..=31).contains(&o[1])) ||
                (o[0]==192 && o[1]==168) ||
                // RFC6598 addresses are used by Tailscale and other overlays.
                (o[0]==100 && (64..=127).contains(&o[1]))
        }
        IpAddr::V6(v) => v.is_unique_local() || v.is_loopback(),
    }
}
/// Resolve once and bind the HTTPS client to exactly the checked IPs.
/// Never fall back to an unpinned resolver (DNS rebinding / SSRF).
pub fn pinned_client(base: &Url) -> Result<Client, DiscoveryError> {
    use std::net::{SocketAddr, ToSocketAddrs};
    let name = base.host_str().ok_or(DiscoveryError::InvalidSettings)?;
    let port = base
        .port_or_known_default()
        .ok_or(DiscoveryError::InvalidSettings)?;
    let addresses: Vec<SocketAddr> = (name, port)
        .to_socket_addrs()
        .map_err(|_| DiscoveryError::Unavailable)?
        .collect();
    if addresses.is_empty()
        || addresses.len() > 16
        || addresses.iter().any(|entry| !allowed_ha_ip(entry.ip()))
    {
        return Err(DiscoveryError::InvalidSettings);
    }
    let mut builder = Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10));
    for address in addresses {
        builder = builder.resolve(name, address);
    }
    builder.build().map_err(|_| DiscoveryError::Unavailable)
}

fn read_get(client: &Client, target: Url, token: &str) -> Result<Vec<u8>, DiscoveryError> {
    let response = client
        .get(target)
        .header(header::ACCEPT, "application/json")
        .bearer_auth(token)
        .send()
        .map_err(|_| DiscoveryError::Unavailable)?;
    if response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN {
        return Err(DiscoveryError::Unauthorized);
    }
    if response.status() != StatusCode::OK
        || response.headers().contains_key(header::LOCATION)
        || !response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(';')
                    .next()
                    .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
            })
        || response
            .content_length()
            .is_some_and(|len| len > MAX_RESPONSE as u64)
    {
        return Err(DiscoveryError::InvalidResponse);
    }
    let mut body = Vec::new();
    response
        .take((MAX_RESPONSE + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|_| DiscoveryError::InvalidResponse)?;
    if body.is_empty() || body.len() > MAX_RESPONSE {
        return Err(DiscoveryError::InvalidResponse);
    }
    Ok(body)
}

fn parse_snapshots(
    states: &[u8],
    services: &[u8],
    config: &BuiltinHomeAssistant,
    policy_revision: u64,
    credential_revision: u64,
) -> Result<Discovery, DiscoveryError> {
    config
        .validate_offline()
        .map_err(|_| DiscoveryError::InvalidSettings)?;
    if policy_revision == 0 || credential_revision == 0 {
        return Err(DiscoveryError::InvalidSettings);
    }
    let states: Value =
        serde_json::from_slice(states).map_err(|_| DiscoveryError::InvalidResponse)?;
    let groups: Value =
        serde_json::from_slice(services).map_err(|_| DiscoveryError::InvalidResponse)?;
    let states = states
        .as_array()
        .filter(|v| v.len() <= 1000)
        .ok_or(DiscoveryError::InvalidResponse)?;
    let groups = groups
        .as_array()
        .filter(|v| v.len() <= 2000)
        .ok_or(DiscoveryError::InvalidResponse)?;
    let mut supported = HashSet::new();
    for group in groups {
        let domain = group
            .get("domain")
            .and_then(Value::as_str)
            .ok_or(DiscoveryError::InvalidResponse)?;
        let services = group
            .get("services")
            .and_then(Value::as_object)
            .ok_or(DiscoveryError::InvalidResponse)?;
        if services.len() > 100 {
            return Err(DiscoveryError::InvalidResponse);
        }
        for name in services.keys() {
            if name.len() <= 64 && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
                supported.insert((domain.to_owned(), name.clone()));
            }
        }
    }
    let ids: HashSet<&str> = config
        .entities
        .iter()
        .filter(|e| e.enabled)
        .map(|e| e.entity_id.as_str())
        .collect();
    let mut by_id = HashMap::new();
    for state in states {
        let Some(id) = state.get("entity_id").and_then(Value::as_str) else {
            return Err(DiscoveryError::InvalidResponse);
        };
        if !ids.contains(id) {
            continue;
        }
        let current = state
            .get("state")
            .and_then(Value::as_str)
            .ok_or(DiscoveryError::InvalidResponse)?;
        let updated = state
            .get("last_updated")
            .and_then(Value::as_str)
            .ok_or(DiscoveryError::InvalidResponse)?;
        let attrs = state
            .get("attributes")
            .and_then(Value::as_object)
            .ok_or(DiscoveryError::InvalidResponse)?;
        if current.len() > 128
            || updated.len() > 64
            || current.chars().any(char::is_control)
            || by_id.contains_key(id)
        {
            return Err(DiscoveryError::InvalidResponse);
        }
        let mut public = serde_json::Map::new();
        for (key, value) in attrs {
            match key.as_str() {
                "temperature" | "current_temperature" | "min_temp" | "max_temp"
                    if value.as_f64().is_some_and(f64::is_finite) =>
                {
                    public.insert(key.clone(), value.clone());
                }
                "brightness" if value.as_u64().is_some_and(|v| v <= 255) => {
                    public.insert(key.clone(), value.clone());
                }
                "unit_of_measurement"
                    if value
                        .as_str()
                        .is_some_and(|v| v.len() <= 12 && !v.chars().any(char::is_control)) =>
                {
                    public.insert(key.clone(), value.clone());
                }
                _ => {}
            }
        }
        let domain = id.split_once('.').ok_or(DiscoveryError::InvalidResponse)?.0;
        let permissible: &[&str] = match domain {
            "light" | "switch" => &["turn_on", "turn_off"],
            "climate" => &["set_temperature"],
            "lock" => &["lock", "unlock"],
            "cover" => &["open_cover", "close_cover", "stop_cover"],
            "alarm_control_panel" => &["alarm_arm_home", "alarm_arm_away", "alarm_disarm"],
            "media_player" => &["play_media"],
            _ => &[],
        };
        // HA can return an entity with state=unavailable or unknown even if
        // its service exists. Never advertise actions for that entity.
        let online = !matches!(current, "unavailable" | "unknown");
        let actions_available = permissible
            .iter()
            .filter(|service| {
                online && supported.contains(&(domain.to_owned(), (**service).to_owned()))
            })
            .map(|service| service.to_string())
            .collect();
        if !online {
            public.clear();
        }
        by_id.insert(
            id.to_owned(),
            DiscoveredEntity {
                entity_id: id.to_owned(),
                state: current.to_owned(),
                last_updated: updated.to_owned(),
                attributes: Value::Object(public),
                actions_available,
            },
        );
    }
    // Persisted allowlist is never modified by discovery: missing or unavailable
    // devices remain visible to users, never silently erased or granted access.
    let mut entities = Vec::new();
    for selected in config.entities.iter().filter(|e| e.enabled) {
        if let Some(value) = by_id.remove(&selected.entity_id) {
            entities.push(value);
        } else {
            entities.push(DiscoveredEntity {
                entity_id: selected.entity_id.clone(),
                state: "unavailable".into(),
                last_updated: String::new(),
                attributes: serde_json::json!({}),
                actions_available: vec![],
            });
        }
    }
    Ok(Discovery {
        entities,
        policy_revision,
        credential_revision,
    })
}

fn guarded_pair<F: Fn() -> bool, G: FnMut(usize) -> Result<Vec<u8>, DiscoveryError>>(
    valid: &F,
    mut fetch: G,
) -> Result<(Vec<u8>, Vec<u8>), DiscoveryError> {
    if !valid() {
        return Err(DiscoveryError::InvalidSettings);
    }
    let states = fetch(0)?;
    if !valid() {
        return Err(DiscoveryError::InvalidSettings);
    }
    let services = fetch(1)?;
    if !valid() {
        return Err(DiscoveryError::InvalidSettings);
    }
    Ok((states, services))
}

/// Only call on an explicit desktop command with the token obtained from the
/// dedicated local secret store. This is read-only; never sends a POST.
pub fn discover(
    config: &BuiltinHomeAssistant,
    token: &str,
    policy_revision: u64,
    credential_revision: u64,
) -> Result<Discovery, DiscoveryError> {
    discover_guarded(config, token, policy_revision, credential_revision, || true)
}

/// Recheck credential/policy authorization before each HTTP request and again
/// before returning a result; revocation stops the second request. An already
/// transmitted first GET cannot be unsent, but its result is never exposed.
pub fn discover_guarded<F: Fn() -> bool>(
    config: &BuiltinHomeAssistant,
    token: &str,
    policy_revision: u64,
    credential_revision: u64,
    valid: F,
) -> Result<Discovery, DiscoveryError> {
    config
        .validate_offline()
        .map_err(|_| DiscoveryError::InvalidSettings)?;
    if token.len() < 20
        || token
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(DiscoveryError::MissingCredential);
    }
    let states = endpoint(&config.base_url, "/api/states")?;
    let services = endpoint(&config.base_url, "/api/services")?;
    if !valid() {
        return Err(DiscoveryError::InvalidSettings);
    }
    let origin = Url::parse(&config.base_url).map_err(|_| DiscoveryError::InvalidSettings)?;
    let client = pinned_client(&origin)?;
    let (states, services) = guarded_pair(&valid, |which| {
        if which == 0 {
            read_get(&client, states.clone(), token)
        } else {
            read_get(&client, services.clone(), token)
        }
    })?;
    let output = parse_snapshots(
        &states,
        &services,
        config,
        policy_revision,
        credential_revision,
    )?;
    // Token or allowlist may change while the two results are being parsed.
    // Never disclose a stale but apparently successful snapshot.
    if !valid() {
        return Err(DiscoveryError::InvalidSettings);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{BuiltinHomeEntity, HomeApprovalPreference};
    fn config() -> BuiltinHomeAssistant {
        BuiltinHomeAssistant {
            enabled: false,
            base_url: "https://ha.example.test/".into(),
            entities: vec![
                BuiltinHomeEntity {
                    entity_id: "light.desk".into(),
                    alias: "台灯".into(),
                    enabled: true,
                    policy: HomeApprovalPreference::Ask,
                    media: vec![],
                },
                BuiltinHomeEntity {
                    entity_id: "switch.unplugged".into(),
                    alias: "风扇".into(),
                    enabled: true,
                    policy: HomeApprovalPreference::Ask,
                    media: vec![],
                },
            ],
        }
    }
    #[test]
    fn nv15_revocation_guard_is_fail_closed_before_dns_or_requests() {
        use std::cell::Cell;
        let allowed = Cell::new(true);
        let sent = Cell::new(0);
        let result = guarded_pair(&|| allowed.get(), |_| {
            sent.set(sent.get() + 1);
            allowed.set(false);
            Ok(vec![b'{', b'}'])
        });
        assert_eq!(result.unwrap_err(), DiscoveryError::InvalidSettings);
        assert_eq!(
            sent.get(),
            1,
            "revocation between GET requests must suppress second GET"
        );
        assert_eq!(
            guarded_pair(&|| false, |_| panic!("no HTTP after revocation")).unwrap_err(),
            DiscoveryError::InvalidSettings
        );
        let checks = Cell::new(0);
        let res = guarded_pair(
            &|| {
                checks.set(checks.get() + 1);
                checks.get() < 3
            },
            |_| Ok(vec![b'{', b'}']),
        );
        assert_eq!(
            res.unwrap_err(),
            DiscoveryError::InvalidSettings,
            "post-response revocation must suppress output"
        );
    }
    #[test]
    fn nv15_dns_policy_blocks_public_metadata_and_mixed_addresses() {
        use std::net::IpAddr;
        for address in [
            "192.168.1.2",
            "10.4.0.1",
            "172.16.1.8",
            "127.0.0.1",
            "100.100.100.100",
            "fd7a:115c:a1e0::1",
        ] {
            assert!(
                allowed_ha_ip(address.parse::<IpAddr>().unwrap()),
                "{address}"
            );
        }
        for address in [
            "8.8.8.8",
            "169.254.169.254",
            "0.0.0.0",
            "224.0.0.1",
            "::",
            "2001:4860:4860::8888",
        ] {
            assert!(
                !allowed_ha_ip(address.parse::<IpAddr>().unwrap()),
                "{address}"
            );
        }
    }
    #[test]
    fn nv15_discovery_exact_https_read_only_routes() {
        for (base, valid) in [
            ("https://ha.example.test/", true),
            ("http://ha.example.test/", false),
            ("https://u:p@ha.example.test/", false),
            ("https://ha.example.test/p/", false),
            ("https://ha.example.test/?x=1", false),
        ] {
            assert_eq!(endpoint(base, "/api/states").is_ok(), valid);
        }
        assert!(endpoint("https://ha.example.test/", "/api/services").is_ok());
        assert!(endpoint("https://ha.example.test/", "/api/services/turn_on").is_err());
    }
    #[test]
    fn nv15_discovery_allowlist_sanitizes_and_never_auto_enrolls() {
        let states=serde_json::json!([
            {"entity_id":"light.desk","state":"on","last_updated":"now",
                "attributes":{"brightness":123,"access_token":"private","pin":"1234","min_temp":-1}},
            {"entity_id":"lock.extra","state":"unlocked","last_updated":"now",
                "attributes":{"pin":"secret"}}
        ]).to_string();
        let services = serde_json::json!([
            {"domain":"light","services":{"turn_on":{},"turn_off":{},"delete_all":{}}},
            {"domain":"lock","services":{"unlock":{}}}
        ])
        .to_string();
        let output =
            parse_snapshots(states.as_bytes(), services.as_bytes(), &config(), 1, 2).unwrap();
        assert_eq!(output.entities.len(), 2);
        assert_eq!(
            output.entities[0].actions_available,
            vec!["turn_on", "turn_off"]
        );
        assert_eq!(output.entities[0].attributes["brightness"], 123);
        assert!(output.entities[0].attributes.get("access_token").is_none());
        assert!(output.entities[0].attributes.get("pin").is_none());
        assert_eq!(output.entities[1].state, "unavailable");
        assert!(output.entities[1].actions_available.is_empty());
    }
    #[test]
    fn nv15_discovery_offline_and_returning_devices_are_not_auto_authorized() {
        let cfg = config();
        let services = br#"[{"domain":"light","services":{"turn_on":{},"turn_off":{}}}]"#;
        let missing=br#"[{"entity_id":"light.desk","state":"unavailable","last_updated":"now","attributes":{"brightness":55,"private":"secret"}}]"#;
        let offline = parse_snapshots(missing, services, &cfg, 1, 2).unwrap();
        assert_eq!(offline.entities[0].state, "unavailable");
        assert!(offline.entities[0].actions_available.is_empty());
        assert_eq!(offline.entities[0].attributes, serde_json::json!({}));
        let online=br#"[{"entity_id":"light.desk","state":"on","last_updated":"now","attributes":{"brightness":55,"private":"secret"}},{"entity_id":"lock.unlisted","state":"unlocked","last_updated":"now","attributes":{}}]"#;
        let output = parse_snapshots(online, services, &cfg, 1, 2).unwrap();
        assert_eq!(output.entities.len(), 2);
        assert_eq!(
            output.entities[0].actions_available,
            vec!["turn_on", "turn_off"]
        );
        assert_eq!(
            output.entities[0].attributes,
            serde_json::json!({"brightness":55})
        );
        assert!(
            output
                .entities
                .iter()
                .all(|e| e.entity_id != "lock.unlisted")
        );
    }
    #[test]
    fn nv15_discovery_fail_closed_on_bad_schema_and_revisions() {
        let cfg = config();
        assert!(parse_snapshots(b"{}", b"[]", &cfg, 1, 2).is_err());
        assert!(parse_snapshots(b"[]", b"{}", &cfg, 1, 2).is_err());
        assert!(parse_snapshots(b"[]", b"[]", &cfg, 0, 2).is_err());
        assert!(parse_snapshots(b"[]", b"[]", &cfg, 1, 0).is_err());
        assert!(discover(&cfg, "too-short", 1, 2).is_err());
    }
}
