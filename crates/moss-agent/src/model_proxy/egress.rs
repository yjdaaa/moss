//! Pure outbound policy gates for a FUTURE trusted HTTPS adapter.
//!
//! There is deliberately NO network dialer here: validating a DNS answer,
//! TLS SNI and a response header is not proof of real server identity. The
//! future adapter MUST dial only the returned addresses, verify TLS for the
//! exact host name, reject redirects, and never re-resolve independently.
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

pub(super) const TLS_HOST: &str = "api.openai.com";
pub(super) const TLS_PORT: u16 = 443;
pub(super) const FIXED_URL: &str = "https://api.openai.com/v1/responses";
const MAX_DNS_ANSWERS: usize = 8;
pub(super) const MAX_UPSTREAM_BYTES: usize = 128 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum EgressDeny {
    Target,
    UnsafeIp,
    Response,
    TooLarge,
}

/// Contains only public synthetic address values approved by pure policy.
/// No resolver/HTTP client is contained in or constructed by this module.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct VerifiedDestination {
    pub(super) sni: &'static str,
    pub(super) port: u16,
    pub(super) dial: Vec<SocketAddr>,
}
impl VerifiedDestination {
    fn check(method: &str, target: &str, answers: &[IpAddr]) -> Result<Self, EgressDeny> {
        if method != "POST" || target != FIXED_URL {
            return Err(EgressDeny::Target);
        }
        if answers.is_empty()
            || answers.len() > MAX_DNS_ANSWERS
            || answers.iter().any(|address| !public_ip_only(*address))
        {
            return Err(EgressDeny::UnsafeIp);
        }
        let dial = answers
            .iter()
            .copied()
            .map(|address| SocketAddr::new(address, TLS_PORT))
            .collect();
        Ok(Self {
            sni: TLS_HOST,
            port: TLS_PORT,
            dial,
        })
    }
}

fn public_ip_only(ip: IpAddr) -> bool {
    // IPv6 is intentionally disabled until full IPv6 special-purpose
    // registry handling, firewall/egress pinning and TLS adapter review.
    let IpAddr::V4(v4) = ip else {
        return false;
    };
    let [a, b, c, _] = v4.octets();
    if a == 0 || a == 10 || a == 127 || a >= 224 {
        return false;
    }
    if a == 100 && (64..=127).contains(&b) {
        return false;
    }
    if a == 169 && b == 254 {
        return false;
    }
    if a == 172 && (16..=31).contains(&b) {
        return false;
    }
    if a == 192 && (b == 0 || b == 168 || (b == 88 && c == 99)) {
        return false;
    }
    if a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)) {
        return false;
    }
    if a == 203 && b == 0 && c == 113 {
        return false;
    }
    if a == 192 && b == 0 && c == 2 {
        return false;
    }
    if a == 255 || v4 == Ipv4Addr::BROADCAST {
        return false;
    }
    true
}

/// No 3xx redirect is ever followed; no JSON/cookies/Authorization from an
/// upstream may be used as arbitrary next-target instructions.
pub(super) fn accept_upstream_result<'a>(
    status: u16,
    content_type: &str,
    bytes: &'a [u8],
) -> Result<&'a [u8], EgressDeny> {
    if bytes.len() > MAX_UPSTREAM_BYTES {
        return Err(EgressDeny::TooLarge);
    }
    if status != 200 || content_type != "application/json" {
        return Err(EgressDeny::Response);
    }
    let json: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| EgressDeny::Response)?;
    if !json.is_object() {
        return Err(EgressDeny::Response);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ipv4(text: &str) -> IpAddr {
        text.parse().unwrap()
    }
    #[test]
    fn only_exact_https_responses_route_and_pinned_public_addresses() {
        let ok = VerifiedDestination::check("POST", FIXED_URL, &[ipv4("8.8.8.8"), ipv4("1.1.1.1")])
            .unwrap();
        assert_eq!(ok.sni, "api.openai.com");
        assert_eq!(ok.port, 443);
        assert_eq!(
            ok.dial,
            vec![
                "8.8.8.8:443".parse().unwrap(),
                "1.1.1.1:443".parse().unwrap()
            ]
        );
        for target in [
            "http://api.openai.com/v1/responses",
            "https://api.openai.com:443/v1/responses",
            "https://API.OPENAI.COM/v1/responses",
            "https://user:secret@api.openai.com/v1/responses",
            "https://api.openai.com/v1/responses?target=127.0.0.1",
            "https://api.openai.com/v1/responses/",
            "https://api.openai.com/v1/%72esponses",
            "https://api.openai.com.evil.test/v1/responses",
            "https://127.0.0.1/v1/responses",
            "https://169.254.169.254/latest/meta-data",
            "//api.openai.com/v1/responses",
            "/v1/responses",
        ] {
            assert_eq!(
                VerifiedDestination::check("POST", target, &[ipv4("8.8.8.8")]),
                Err(EgressDeny::Target),
                "unexpected route accepted: {target}"
            );
        }
        for method in ["GET", "CONNECT", "PUT", "DELETE", "POST\n"] {
            assert_eq!(
                VerifiedDestination::check(method, FIXED_URL, &[ipv4("8.8.8.8")]),
                Err(EgressDeny::Target)
            );
        }
    }
    #[test]
    fn public_dns_result_must_not_include_one_private_metadata_or_reserved_address() {
        let denied = [
            "0.0.0.0",
            "10.5.1.1",
            "100.64.0.1",
            "100.127.255.254",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.3.4",
            "172.31.255.255",
            "192.0.0.2",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.2",
            "198.18.0.1",
            "198.19.0.1",
            "198.51.100.3",
            "203.0.113.6",
            "224.0.0.1",
            "240.1.1.1",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fc00::10",
            "::ffff:127.0.0.1",
            "2001:db8::1",
            "2606:4700:4700::1111",
        ];
        for value in denied {
            assert_eq!(
                VerifiedDestination::check("POST", FIXED_URL, &[ipv4("8.8.8.8"), ipv4(value)]),
                Err(EgressDeny::UnsafeIp),
                "DNS answer not rejected: {value}"
            );
        }
        assert_eq!(
            VerifiedDestination::check("POST", FIXED_URL, &[]),
            Err(EgressDeny::UnsafeIp)
        );
        assert_eq!(
            VerifiedDestination::check("POST", FIXED_URL, &[ipv4("8.8.8.8"); MAX_DNS_ANSWERS + 1]),
            Err(EgressDeny::UnsafeIp)
        );
    }
    #[test]
    fn never_accept_redirect_html_oversized_or_invalid_json_from_upstream() {
        let payload = br#"{"id":"test","output":[]}"#;
        assert_eq!(
            accept_upstream_result(200, "application/json", payload),
            Ok(&payload[..])
        );
        for status in [201, 301, 302, 303, 307, 308, 401, 403, 429, 500] {
            assert_eq!(
                accept_upstream_result(status, "application/json", payload),
                Err(EgressDeny::Response)
            );
        }
        for content_type in [
            "text/html",
            "application/json; charset=utf-8",
            "text/event-stream",
        ] {
            assert_eq!(
                accept_upstream_result(200, content_type, payload),
                Err(EgressDeny::Response)
            );
        }
        assert_eq!(
            accept_upstream_result(200, "application/json", b"not json"),
            Err(EgressDeny::Response)
        );
        assert_eq!(
            accept_upstream_result(200, "application/json", &vec![b'a'; MAX_UPSTREAM_BYTES + 1]),
            Err(EgressDeny::TooLarge)
        );
    }
}
