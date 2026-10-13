//! Real loopback TLS bytes through each production adapter. All content, keys,
//! certificates and hosts are synthetic; no account or external request.
use super::*;
use http::FetchError;
use reqwest::blocking::Client;
use rustls::{
    ServerConfig, ServerConnection, StreamOwned,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    thread,
    time::Duration,
};

const KEY: &str = "NV11_SYNTHETIC_ONLY_KEY";
const HOST: &str = "provider.example.test";
#[derive(Clone, Copy, Debug)]
enum Provider {
    Weather,
    Tavily,
    Metaso,
    Rag,
    NewsNow,
    Rss,
}
const PROVIDERS: [Provider; 6] = [
    Provider::Weather,
    Provider::Tavily,
    Provider::Metaso,
    Provider::Rag,
    Provider::NewsNow,
    Provider::Rss,
];
impl Provider {
    fn fixture(self) -> &'static str {
        match self {
            Self::Weather => include_str!("../../tests/fixtures/nv11/qweather-current.json"),
            Self::Tavily => include_str!("../../tests/fixtures/nv11/tavily-search.json"),
            Self::Metaso => include_str!("../../tests/fixtures/nv11/metaso-search.json"),
            Self::Rag => include_str!("../../tests/fixtures/nv11/ragflow-retrieval.json"),
            Self::NewsNow => include_str!("../../tests/fixtures/nv11/newsnow-source.json"),
            Self::Rss => include_str!("../../tests/fixtures/nv11/rss-news.xml"),
        }
    }
    fn invoke(self, client: &Client, base: &str) -> Result<Value, FetchError> {
        match self {
            Self::Weather => weather::current(client, base, KEY, "api_key", 39.9, 116.4),
            Self::Tavily => search::search(client, base, KEY, "合成问题"),
            Self::Metaso => metaso::search(client, base, KEY, "合成问题"),
            Self::Rag => rag::retrieve(client, base, KEY, &["allowed".into()], "合成问题"),
            Self::NewsNow => newsnow::fetch(client, base, "synthetic-source", 3),
            Self::Rss => news::fetch_rss(client, base, 3),
        }
    }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
    chunked: bool,
    delay_headers: bool,
    delay_body: bool,
    location: Option<String>,
}
impl Reply {
    fn body(body: impl AsRef<[u8]>) -> Self {
        Self {
            status: 200,
            body: body.as_ref().to_vec(),
            chunked: false,
            delay_headers: false,
            delay_body: false,
            location: None,
        }
    }
}

fn exchange(provider: Provider, reply: Reply) -> (Result<Value, FetchError>, Vec<u8>) {
    let deadline_test = reply.delay_headers || reply.delay_body;
    let cert = rcgen::generate_simple_self_signed(vec![HOST.into()]).unwrap();
    let der = cert.cert.der().to_vec();
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    let tls = ServerConfig::builder_with_provider(crypto)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(der.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der())),
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        // Bounded accept ensures fixture failure cannot hang the workspace.
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        let socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(e) => panic!("fixture accept: {e}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut stream = StreamOwned::new(ServerConnection::new(Arc::new(tls)).unwrap(), socket);
        let mut raw = Vec::new();
        let mut packet = [0; 4096];
        loop {
            let count = stream.read(&mut packet).unwrap();
            assert!(count > 0);
            raw.extend_from_slice(&packet[..count]);
            assert!(raw.len() <= 65536);
            if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                let length = String::from_utf8_lossy(&raw[..pos])
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if raw.len() >= pos + 4 + length {
                    break;
                }
            }
        }
        if reply.delay_headers {
            thread::sleep(Duration::from_millis(900));
        }
        let mut headers = format!(
            "HTTP/1.1 {} Fixture\r\nConnection: close\r\nContent-Type: application/json\r\n",
            reply.status
        );
        if reply.chunked {
            headers.push_str("Transfer-Encoding: chunked\r\n");
        } else {
            headers.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
        }
        if let Some(location) = reply.location {
            headers.push_str(&format!("Location: {location}\r\n"));
        }
        headers.push_str("\r\n");
        let _ = stream.write_all(headers.as_bytes());
        let _ = stream.flush();
        if reply.delay_body {
            thread::sleep(Duration::from_millis(900));
        }
        if reply.chunked {
            let _ = write!(stream, "{:x}\r\n", reply.body.len());
            let _ = stream.write_all(&reply.body);
            let _ = stream.write_all(b"\r\n0\r\n\r\n");
        } else {
            let _ = stream.write_all(&reply.body);
        }
        let _ = stream.flush();
        raw
    });
    // Same builder/policies as production, with only synthetic CA/DNS and a
    // shorter TEST-ONLY total deadline. Certificate verification stays enabled.
    let client = http::client_builder()
        .no_proxy()
        .resolve(HOST, addr)
        .add_root_certificate(reqwest::Certificate::from_der(&der).unwrap())
        .timeout(if deadline_test {
            Duration::from_millis(600)
        } else {
            Duration::from_secs(3)
        })
        .build()
        .unwrap();
    let base = format!("https://{HOST}:{}/feed.xml", addr.port());
    let result = provider.invoke(&client, &base);
    (result, server.join().unwrap())
}
#[test]
fn all_provider_requests_and_success_fixtures_roundtrip_over_verified_tls() {
    for provider in PROVIDERS {
        let (result, raw) = exchange(provider, Reply::body(provider.fixture()));
        let value = result.unwrap_or_else(|e| panic!("{provider:?}: {e:?}"));
        let encoded = value.to_string();
        assert!(!encoded.contains(KEY));
        assert!(!encoded.contains("must-not-expose"));
        let wire = String::from_utf8(raw).unwrap();
        let (head, body) = wire.split_once("\r\n\r\n").unwrap();
        let headers = head.to_ascii_lowercase();
        match provider {
            Provider::Weather => {
                assert!(head.starts_with("GET /weather/v1/current/39.9/116.4 HTTP/1.1"));
                assert!(headers.contains(&format!("x-qw-api-key: {}", KEY.to_lowercase())));
                assert_eq!(body, "");
                assert_eq!(value["humidity"], 0.5);
            }
            Provider::Tavily => {
                assert!(head.starts_with("POST /search HTTP/1.1"));
                assert_eq!(
                    serde_json::from_str::<Value>(body).unwrap(),
                    json!({"query":"合成问题","search_depth":"basic","max_results":5,"include_raw_content":false})
                );
            }
            Provider::Metaso => {
                assert!(head.starts_with("POST /api/open/search/v2 HTTP/1.1"));
                assert_eq!(
                    serde_json::from_str::<Value>(body).unwrap(),
                    json!({"question":"合成问题","lang":"zh","stream":false,"enableMix":false,"newEngine":false})
                );
            }
            Provider::Rag => {
                assert!(head.starts_with("POST /api/v1/retrieval HTTP/1.1"));
                assert_eq!(
                    serde_json::from_str::<Value>(body).unwrap(),
                    json!({"question":"合成问题","dataset_ids":["allowed"],"page":1,"page_size":5})
                );
                assert_eq!(value["chunks"][0]["dataset_id"], "allowed");
            }
            Provider::NewsNow => {
                assert!(head.starts_with("GET /api/s?id=synthetic-source HTTP/1.1"));
                assert_eq!(value["items"][1]["published"], 1720000000000_i64);
            }
            Provider::Rss => {
                assert!(head.starts_with("GET /feed.xml HTTP/1.1"));
                assert_eq!(value["items"][0]["title"], "合成标题");
                assert_eq!(
                    value["items"][0]["url"],
                    "https://source.example.test/one?a=1&b=2"
                );
                assert_eq!(value["items"][1]["title"], "第二条 & 更多");
            }
        }
        if matches!(
            provider,
            Provider::Tavily | Provider::Metaso | Provider::Rag
        ) {
            assert!(headers.contains(&format!("authorization: bearer {}", KEY.to_lowercase())));
        }
        if matches!(provider, Provider::NewsNow | Provider::Rss) {
            assert!(!headers.contains("authorization:"));
            assert!(!wire.contains(KEY));
        }
    }
}

#[test]
fn each_provider_rejects_auth_rate_quota_server_and_redirect_errors_without_leaks() {
    let sink = TcpListener::bind("127.0.0.1:0").unwrap();
    sink.set_nonblocking(true).unwrap();
    for provider in PROVIDERS {
        for (status, expected) in [
            (401, FetchError::Unauthorized),
            (403, FetchError::Unauthorized),
            (429, FetchError::RateLimited),
            (432, FetchError::QuotaExceeded),
            (433, FetchError::QuotaExceeded),
            (500, FetchError::Network),
            (302, FetchError::Network),
        ] {
            let mut reply = Reply::body(format!("{KEY} private provider diagnostic"));
            reply.status = status;
            reply.location = Some(format!("http://{}/leak", sink.local_addr().unwrap()));
            let (result, _) = exchange(provider, reply);
            let error = result.unwrap_err();
            assert_eq!(error, expected, "{provider:?}/{status}");
            assert!(!error.result().to_string().contains(KEY));
            if matches!(
                expected,
                FetchError::Unauthorized | FetchError::QuotaExceeded
            ) {
                assert_eq!(error.result()["error"]["retryable"], false);
            }
        }
    }
    assert_eq!(
        sink.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "Redirect must never reach sink"
    );
}

#[test]
fn each_provider_bounds_known_and_chunked_response_bodies() {
    for provider in PROVIDERS {
        for chunked in [false, true] {
            let mut reply = Reply::body(vec![b'x'; http::MAX_BODY_BYTES + 1]);
            reply.chunked = chunked;
            assert_eq!(
                exchange(provider, reply).0.unwrap_err(),
                FetchError::ResponseTooLarge,
                "{provider:?}/{chunked}"
            );
        }
    }
}

#[test]
fn exact_one_mib_response_boundary_is_accepted_when_schema_is_valid() {
    for provider in PROVIDERS {
        let body = if matches!(provider, Provider::Rss) {
            let mut body = provider.fixture().as_bytes().to_vec();
            body.resize(http::MAX_BODY_BYTES, b' ');
            body
        } else {
            let mut value: Value = serde_json::from_str(provider.fixture()).unwrap();
            value["fixture_padding"] = json!("");
            let padding = http::MAX_BODY_BYTES - value.to_string().len();
            value["fixture_padding"] = json!("x".repeat(padding));
            value.to_string().into_bytes()
        };
        assert_eq!(body.len(), http::MAX_BODY_BYTES);
        assert!(
            exchange(provider, Reply::body(body)).0.is_ok(),
            "{provider:?} exact boundary"
        );
    }
}

#[test]
fn each_provider_maps_header_and_body_deadlines_to_timeout() {
    for provider in PROVIDERS {
        for body_timeout in [false, true] {
            let mut reply = Reply::body(provider.fixture());
            reply.delay_headers = !body_timeout;
            reply.delay_body = body_timeout;
            assert_eq!(
                exchange(provider, reply).0.unwrap_err(),
                FetchError::Timeout,
                "{provider:?}/{body_timeout}"
            );
        }
    }
}

#[test]
fn each_provider_rejects_malformed_or_missing_payload_instead_of_empty_success() {
    for provider in PROVIDERS {
        for body in ["not JSON/XML", "{}"] {
            assert_eq!(
                exchange(provider, Reply::body(body)).0.unwrap_err(),
                FetchError::InvalidResponse,
                "{provider:?}"
            );
        }
    }
    for (provider, body) in [
        (Provider::Tavily, r#"{"results":[]}"#),
        (Provider::Rag, r#"{"code":0,"data":{"chunks":[]}}"#),
        (
            Provider::NewsNow,
            r#"{"status":"success","id":"synthetic-source","updatedTime":0,"items":[]}"#,
        ),
        (Provider::Rss, "<rss><channel/></rss>"),
    ] {
        assert!(
            exchange(provider, Reply::body(body)).0.is_ok(),
            "{provider:?} explicit empty response"
        );
    }
}

#[test]
fn parser_provenance_timestamps_and_bounded_envelopes_fail_closed() {
    let mut rag: Value = serde_json::from_str(Provider::Rag.fixture()).unwrap();
    for key in ["content", "document_id", "dataset_id"] {
        let mut bad = rag.clone();
        bad["data"]["chunks"][0]
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(super::rag::parse(&bad).is_err());
    }
    rag["data"]["chunks"][0]["dataset_id"] = json!("outside-allowlist");
    assert_eq!(
        exchange(Provider::Rag, Reply::body(rag.to_string()))
            .0
            .unwrap_err(),
        FetchError::InvalidResponse
    );
    let news: Value = serde_json::from_str(Provider::NewsNow.fixture()).unwrap();
    for bad in [Value::Null, json!(""), json!({}), json!(false)] {
        let mut row = news.clone();
        row["updatedTime"] = bad;
        assert!(newsnow::parse(&row, "synthetic-source", 3).is_err());
    }
    let mut string_time = news.clone();
    string_time["updatedTime"] = json!("1720000000000");
    assert_eq!(
        newsnow::parse(&string_time, "synthetic-source", 3).unwrap()["updated_time"],
        "1720000000000"
    );
    let mut bad = news;
    bad["items"][0]["pubDate"] = json!({});
    assert!(newsnow::parse(&bad, "synthetic-source", 3).is_err());
    let (data, truncated) = http::trim_result(json!({"content":"\u{0}\\\"汉字😀".repeat(2000)}));
    assert!(truncated);
    assert!(json!({"ok":true,"source":"search_rag","observed_at":"2026-10-12T00:00:00Z","data":data,"truncated":truncated}).to_string().chars().count()<=http::MAX_REPLY_CHARS);
    for xml in [
        "<rss><channel><item><title>x</title><link>https://example.test</link></item>",
        "<rss><channel/></rss><rss><channel/></rss>",
        "<rss><channel><item><title>x</title><link>javascript:alert(1)</link></item></channel></rss>",
        "<x/><rss><channel/></rss>",
        "<rss><channel/></rss>&unknown;",
        "<rss><channel/></rss><![CDATA[trailing]]>",
    ] {
        assert!(news::parse_rss(xml.as_bytes(), 1).is_err());
    }
}

#[test]
fn provider_field_removal_and_wrong_types_are_explicit_schema_failures() {
    let weather: Value = serde_json::from_str(Provider::Weather.fixture()).unwrap();
    for pointer in [
        "/temperature/value",
        "/temperature/unit",
        "/condition/text",
        "/condition/code",
        "/humidity",
        "/wind/speed/value",
        "/wind/speed/unit",
        "/metadata/attributions",
    ] {
        for wrong in [Value::Null, json!({"private":"must-not-expose"})] {
            let mut value = weather.clone();
            *value.pointer_mut(pointer).unwrap() = wrong;
            assert_eq!(
                super::weather::parse(&value).unwrap_err(),
                FetchError::InvalidResponse,
                "{pointer}"
            );
        }
    }
    for key in ["title", "url", "content"] {
        let mut value: Value = serde_json::from_str(Provider::Tavily.fixture()).unwrap();
        value["results"][0].as_object_mut().unwrap().remove(key);
        assert_eq!(
            search::parse(&value).unwrap_err(),
            FetchError::InvalidResponse,
            "Tavily/{key}"
        );
    }
    for pointer in ["/data/text", "/data/references", "/errCode"] {
        let mut value: Value = serde_json::from_str(Provider::Metaso.fixture()).unwrap();
        *value.pointer_mut(pointer).unwrap() = Value::Null;
        assert_eq!(
            metaso::parse(&value).unwrap_err(),
            FetchError::InvalidResponse,
            "Metaso/{pointer}"
        );
    }
    for key in ["title", "url"] {
        let mut value: Value = serde_json::from_str(Provider::NewsNow.fixture()).unwrap();
        value["items"][0].as_object_mut().unwrap().remove(key);
        assert_eq!(
            newsnow::parse(&value, "synthetic-source", 3).unwrap_err(),
            FetchError::InvalidResponse,
            "NewsNow/{key}"
        );
    }
    for xml in [
        "<rss><channel><item><title>x</title></item></channel></rss>",
        "<rss><channel><item><link>https://example.test</link></item></channel></rss>",
        "<rss><channel><item><title>&unknown;</title><link>https://example.test</link></item></channel></rss>",
    ] {
        assert_eq!(
            news::parse_rss(xml.as_bytes(), 3).unwrap_err(),
            FetchError::InvalidResponse
        );
    }
}

#[test]
fn metaso_http200_application_errors_are_classified_and_never_echoed() {
    for (code, expected) in [
        (401, FetchError::Unauthorized),
        (403, FetchError::Unauthorized),
        (4002, FetchError::QuotaExceeded),
        (4009, FetchError::ProviderRejected),
        (400, FetchError::ProviderRejected),
        (500, FetchError::Network),
        (-500, FetchError::Network),
        (123456, FetchError::InvalidResponse),
    ] {
        let body=json!({"errCode":code,"errMsg":format!("{KEY} private diagnostic"),"data":{"text":"must-not-expose","references":[]}}).to_string();
        let error = exchange(Provider::Metaso, Reply::body(body)).0.unwrap_err();
        assert_eq!(error, expected);
        assert!(!error.result().to_string().contains(KEY));
        if matches!(
            expected,
            FetchError::Unauthorized | FetchError::QuotaExceeded | FetchError::ProviderRejected
        ) {
            assert_eq!(error.result()["error"]["retryable"], false);
        }
    }
}
