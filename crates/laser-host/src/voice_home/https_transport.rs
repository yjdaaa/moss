//! NV15 HTTPS transport for approved plans only; never registered directly to
//! an LLM, public HTTP route or Tauri command. Even a prepared transport
//! cannot submit without the durable HomeControl approval coordinator.
#![allow(dead_code)]
use super::approved::HomeControlTransport;
use reqwest::{StatusCode, Url, blocking::Client, header, redirect::Policy};
use std::{io::Read, time::Duration};

const MAX_REPLY: usize = 8 * 1024;

/// The endpoint and token must be obtained only from trusted desktop config
/// and decrypted credential storage. No arbitrary URLs from the model.
pub(crate) struct HttpsHomeTransport {
    origin: Url,
    bearer: String,
    client: Client,
}
impl HttpsHomeTransport {
    pub(crate) fn new(origin: &str, bearer: &str) -> Result<Self, ()> {
        let url = Url::parse(origin).map_err(|_| ())?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || bearer.len() < 20
            || bearer.len() > 8192
            || bearer
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return Err(());
        }
        // Production GET and POST share the identical one-time DNS/IP pinning
        // policy, TLS verification, proxy exclusion and redirect denial.
        #[cfg(not(test))]
        let client = moss_voice::home_readonly::pinned_client(&url).map_err(|_| ())?;
        #[cfg(test)]
        let client = Client::builder()
            .https_only(true).no_proxy().redirect(Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(7))
            .build().map_err(|_| ())?;
        Ok(Self {
            origin: url,
            bearer: bearer.to_owned(),
            client,
        })
    }
    fn url(&self, path: &str, post: bool) -> Result<Url, ()> {
        // Only paths selected by the whitelisted HomeCatalog plan can ever
        // reach the transport, but independently refuse other paths too.
        let parts: Vec<_> = path.split('/').collect();
        let valid = if post {
            parts.len() == 5
                && parts[0].is_empty()
                && parts[1] == "api"
                && parts[2] == "services"
                && domain_ok(parts[3])
                && service_ok(parts[3], parts[4])
        } else {
            parts.len() == 4
                && parts[0].is_empty()
                && parts[1] == "api"
                && parts[2] == "states"
                && entity_ok(parts[3])
        };
        if !valid {
            return Err(());
        }
        self.origin.join(path).map_err(|_| ())
    }
}
fn entity_ok(entity: &str) -> bool {
    let Some((domain, name)) = entity.split_once('.') else {
        return false;
    };
    domain_ok(domain)
        && !name.is_empty()
        && entity.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn domain_ok(domain: &str) -> bool {
    matches!(
        domain,
        "light" | "switch" | "climate" | "lock" | "alarm_control_panel" | "cover" | "media_player"
    )
}
fn service_ok(domain: &str, action: &str) -> bool {
    matches!(
        (domain, action),
        ("light" | "switch", "turn_on" | "turn_off")
            | ("climate", "set_temperature")
            | ("lock", "lock" | "unlock")
            | (
                "alarm_control_panel",
                "alarm_arm_home" | "alarm_arm_away" | "alarm_disarm"
            )
            | ("cover", "open_cover" | "close_cover" | "stop_cover")
            | ("media_player", "play_media")
    )
}
fn read_limited(response: reqwest::blocking::Response) -> Result<Vec<u8>, ()> {
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
            .is_some_and(|len| len > MAX_REPLY as u64)
    {
        return Err(());
    }
    let mut data = Vec::new();
    response
        .take((MAX_REPLY + 1) as u64)
        .read_to_end(&mut data)
        .map_err(|_| ())?;
    if data.is_empty() || data.len() > MAX_REPLY {
        return Err(());
    }
    Ok(data)
}
impl HomeControlTransport for HttpsHomeTransport {
    fn submit_once(&mut self, path: &str, body: &serde_json::Value) -> Result<(), ()> {
        // Fail-closed even if another caller bypasses the coordinator.
        #[cfg(not(test))]
        return Err(());
        #[cfg(test)]
        {
        let url = self.url(path, true)?;
        if body.as_object().is_none() || body.to_string().len() > 4096 {
            return Err(());
        }
        let raw = read_limited(
            self.client
                .post(url)
                .bearer_auth(&self.bearer)
                .header(header::ACCEPT, "application/json")
                .json(body)
                .send()
                .map_err(|_| ())?,
        )?;
        let json: serde_json::Value = serde_json::from_slice(&raw).map_err(|_| ())?;
        if !json.is_array() {
            return Err(());
        }
        Ok(())
    }
    }
    fn read_target_state(&mut self, entity: &str, budget: Duration) -> Result<String, ()> {
        if budget.is_zero() || budget > Duration::from_millis(900) {
            return Err(());
        }
        let url = self.url(&format!("/api/states/{entity}"), false)?;
        // Must bound the full response read, not just the TLS connect.
        let raw = read_limited(
            self.client
                .get(url)
                .timeout(budget)
                .bearer_auth(&self.bearer)
                .header(header::ACCEPT, "application/json")
                .send()
                .map_err(|_| ())?,
        )?;
        let json: serde_json::Value = serde_json::from_slice(&raw).map_err(|_| ())?;
        if json.get("entity_id").and_then(serde_json::Value::as_str) != Some(entity) {
            return Err(());
        }
        let state = json
            .get("state")
            .and_then(serde_json::Value::as_str)
            .ok_or(())?;
        if state.len() > 128 || state.chars().any(char::is_control) {
            return Err(());
        }
        Ok(state.to_owned())
    }
}
#[cfg(test)]
mod tls_fixture;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nv15_https_control_transport_rejects_arbitrary_urls_and_services() {
        let h = HttpsHomeTransport::new(
            "https://ha.example.test/",
            "synthetic_not_a_real_secret_123456",
        )
        .unwrap();
        assert!(h.url("/api/services/light/turn_on", true).is_ok());
        assert!(h.url("/api/states/light.desk", false).is_ok());
        for path in [
            "/api/states/light.desk",
            "/api/services/light/delete_all",
            "/api/services/light/%2e%2e",
            "/api/services/shell_command/execute",
            "/api/services/light/turn_on?url=x",
            "http://ha.example.test/api/states",
            "/api/states/light.desk/../../private",
        ] {
            assert!(h.url(path, true).is_err(), "{path}");
        }
        assert!(
            h.url("/api/states/light.desk/../../../private", false)
                .is_err()
        );
        assert!(
            HttpsHomeTransport::new(
                "http://ha.example.test/",
                "synthetic_not_a_real_secret_123456"
            )
            .is_err()
        );
        assert!(
            HttpsHomeTransport::new(
                "https://ha.example.test/x/",
                "synthetic_not_a_real_secret_123456"
            )
            .is_err()
        );
    }
}
