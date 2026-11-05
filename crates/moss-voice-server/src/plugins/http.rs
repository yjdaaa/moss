//! NV11: bounded HTTP client for opt-in information plugins.
//! No model-generated hosts, no redirects, no authentication details in errors.
use std::io::Read;
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use reqwest::header::CONTENT_TYPE;
use reqwest::{Method, Url};
use serde_json::{Value, json};

pub const MAX_BODY_BYTES: usize = 1024 * 1024;
pub const MAX_REPLY_CHARS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchError {
    InvalidUrl,
    UntrustedEndpoint,
    Unauthorized,
    RateLimited,
    QuotaExceeded,
    ProviderRejected,
    Timeout,
    ResponseTooLarge,
    InvalidResponse,
    Network,
}

impl FetchError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidUrl | Self::UntrustedEndpoint => "plugin_endpoint_invalid",
            Self::Unauthorized => "plugin_auth_failed",
            Self::RateLimited => "plugin_rate_limited",
            Self::QuotaExceeded => "plugin_quota_exceeded",
            Self::ProviderRejected => "plugin_provider_rejected",
            Self::Timeout => "plugin_timeout",
            Self::ResponseTooLarge => "plugin_response_too_large",
            Self::InvalidResponse => "plugin_schema_invalid",
            Self::Network => "plugin_network_error",
        }
    }

    pub fn result(self) -> Value {
        json!({"ok": false, "error": {"code": self.code(), "message": "信息服务暂时不可用或配置不正确", "retryable": matches!(self, Self::RateLimited | Self::Timeout | Self::Network)}})
    }
}

pub fn validate_endpoint(endpoint: &str, allow_private_http: bool) -> Result<Url, FetchError> {
    let url = Url::parse(endpoint).map_err(|_| FetchError::InvalidUrl)?;
    if url.username() != "" || url.password().is_some() || url.fragment().is_some() {
        return Err(FetchError::InvalidUrl);
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" if allow_private_http && is_private_target(&url) => Ok(url),
        _ => Err(FetchError::UntrustedEndpoint),
    }
}

fn is_private_target(url: &Url) -> bool {
    let Some(host) = url.host_str() else { return false };
    if host.eq_ignore_ascii_case("localhost") { return true; }
    let Ok(ip) = host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() else { return false };
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback(),
        std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
    }
}

pub fn client() -> Result<Client, FetchError> {
    client_builder().build().map_err(|_| FetchError::Network)
}

pub(super) fn client_builder() -> reqwest::blocking::ClientBuilder {
    Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
}

pub fn read_bounded(mut response: Response) -> Result<Vec<u8>, FetchError> {
    let code = response.status().as_u16();
    if code == 401 || code == 403 { return Err(FetchError::Unauthorized); }
    if code == 429 { return Err(FetchError::RateLimited); }
    // Tavily uses these statuses for plan/pay-as-you-go quota exhaustion.
    // Retrying the same billable query cannot restore quota.
    if code == 432 || code == 433 { return Err(FetchError::QuotaExceeded); }
    if !(200..300).contains(&code) { return Err(FetchError::Network); }
    if response.content_length().is_some_and(|n| n > MAX_BODY_BYTES as u64) {
        return Err(FetchError::ResponseTooLarge);
    }
    let mut limited = Vec::new();
    response.by_ref().take(MAX_BODY_BYTES as u64 + 1)
        .read_to_end(&mut limited).map_err(|error| {
            if error.kind() == std::io::ErrorKind::TimedOut
                || error.get_ref().and_then(|inner| inner.downcast_ref::<reqwest::Error>()).is_some_and(|inner| inner.is_timeout()) {
                FetchError::Timeout
            } else { FetchError::Network }
        })?;
    if limited.len() > MAX_BODY_BYTES { return Err(FetchError::ResponseTooLarge); }
    Ok(limited)
}

pub fn request_json(
    client: &Client, method: Method, url: &Url, token: Option<(&str, &str)>, body: Option<Value>,
) -> Result<Value, FetchError> {
    let request = build_json_request(client, method, url, token, body)?;
    send_json(client, request)
}

pub(crate) fn build_json_request(
    client: &Client, method: Method, url: &Url, token: Option<(&str, &str)>, body: Option<Value>,
) -> Result<reqwest::blocking::Request, FetchError> {
    let mut request = client.request(method, url.clone()).header(CONTENT_TYPE, "application/json");
    if let Some((header, secret)) = token {
        request = request.header(header, secret);
    }
    if let Some(body) = body { request = request.json(&body); }
    request.build().map_err(|_| FetchError::InvalidResponse)
}

pub(crate) fn send_json(client: &Client, request: reqwest::blocking::Request) -> Result<Value, FetchError> {
    let response = client.execute(request).map_err(|e| {
        if e.is_timeout() { FetchError::Timeout } else { FetchError::Network }
    })?;
    serde_json::from_slice(&read_bounded(response)?).map_err(|_| FetchError::InvalidResponse)
}

pub fn trim_result(data: Value) -> (Value, bool) {
    let serialized = data.to_string();
    let count = serialized.chars().count();
    // Leave room for the outer result envelope.
    const MAX_DATA_CHARS: usize = MAX_REPLY_CHARS - 320;
    if count <= MAX_DATA_CHARS { return (data, false); }
    // Budget the JSON-escaped output, not the unescaped string. Quotes, slashes
    // and control characters otherwise multiply its size after serialization.
    let mut summary = String::new();
    let mut remaining = MAX_DATA_CHARS.saturating_sub(80);
    for ch in serialized.chars() {
        let cost = serde_json::to_string(&ch.to_string()).unwrap().chars().count() - 2;
        if cost > remaining { break; }
        remaining -= cost;
        summary.push(ch);
    }
    (json!({"text": summary, "note": "数据已截断"}), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_http_and_url_credentials_are_restricted() {
        assert!(validate_endpoint("https://api.example.org/search", false).is_ok());
        assert!(validate_endpoint("http://127.0.0.1:8080/", true).is_ok());
        assert!(validate_endpoint("http://192.168.1.2:8080/", true).is_ok());
        assert!(validate_endpoint("http://[::1]:8080/", true).is_ok());
        assert!(validate_endpoint("http://[fd00::1]:8080/", true).is_ok());
        assert!(validate_endpoint("http://[fe80::1]:8080/", true).is_err());
        assert!(validate_endpoint("http://8.8.8.8/", true).is_err());
        assert!(validate_endpoint("http://example.com/", true).is_err());
        assert!(validate_endpoint("https://user:key@example.org/", false).is_err());
        assert!(validate_endpoint("file:///etc/passwd", true).is_err());
    }
    #[test]
    fn truncated_payload_is_explicit_and_bounded() {
        let (result, truncated) = trim_result(json!({"content": "a".repeat(4000)}));
        assert!(truncated);
        assert!(result.to_string().chars().count() <= MAX_REPLY_CHARS - 250);
        let (_, truncated) = trim_result(json!({"content": "short"}));
        assert!(!truncated);
    }
    fn fake_http(status: &str, body: &str, extra_headers: &str) -> Result<Vec<u8>, FetchError> {
        use std::io::{Read,Write};
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port=listener.local_addr().unwrap().port();
        let status=status.to_owned();
        let body=body.to_owned();
        let extra=extra_headers.to_owned();
        let server=std::thread::spawn(move || {
            let (mut stream,_) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut incoming=[0u8;1024];
            let _=stream.read(&mut incoming);
            let response=format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",body.len());
            // 客户端按 Content-Length 超限拒收时会立即断开；macOS 的 socket 缓冲较小，
            // 剩余写入会收到 ECONNRESET。客户端提前断开正是被测行为，模拟服务器不得因此 panic。
            let _ = stream.write_all(response.as_bytes());
        });
        let target=format!("http://127.0.0.1:{port}/");
        let response=client().unwrap().get(target).send().unwrap();
        let result=read_bounded(response);
        server.join().unwrap();
        result
    }

    #[test]
    fn loopback_mock_enforces_http_error_mapping_and_redirect_block() {
        assert_eq!(fake_http("401 Unauthorized","secret","").unwrap_err(),FetchError::Unauthorized);
        assert_eq!(fake_http("429 Too Many Requests","secret","").unwrap_err(),FetchError::RateLimited);
        assert_eq!(fake_http("302 Found","","Location: http://example.org/leak\r\n").unwrap_err(),FetchError::Network);
        assert_eq!(fake_http("200 OK","{}","").unwrap(),b"{}");
        assert_eq!(fake_http("200 OK",&"x".repeat(MAX_BODY_BYTES+1),"").unwrap_err(),FetchError::ResponseTooLarge);
    }

    #[test]
    fn errors_are_sanitized() {
        for issue in [FetchError::Unauthorized, FetchError::RateLimited, FetchError::Timeout] {
            assert!(!issue.result().to_string().contains("sk-secret"));
        }
    }
}
