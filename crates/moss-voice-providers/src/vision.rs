//! NV12: isolated vision model. Images never enter the text-only LLM/history API.
use crate::BoxFuture;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{Client, Url};
use serde_json::{Value, json};
use std::time::Duration;

pub const MAX_RESULT_CHARS: usize = 2000;
pub const MAX_REPLY_BYTES: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisionFailure {
    Configuration,
    Auth,
    Timeout,
    Upstream,
    Malformed,
}
impl VisionFailure {
    pub fn code(self) -> &'static str {
        match self {
            Self::Configuration => "vision_not_configured",
            Self::Auth => "vision_model_auth_failed",
            Self::Timeout => "vision_model_timeout",
            Self::Upstream => "vision_model_error",
            Self::Malformed => "vision_model_invalid_response",
        }
    }
}
pub trait VisionProvider: Send + Sync {
    fn explain<'a>(
        &'a self,
        question: &'a str,
        mime: &'a str,
        image: &'a [u8],
    ) -> BoxFuture<'a, Result<String, VisionFailure>>;
}
#[derive(Clone)]
pub struct OpenAiVision {
    client: Client,
    url: Url,
    model: String,
    key: String,
}
pub fn endpoint(base: &str) -> Result<Url, VisionFailure> {
    let mut url = Url::parse(base.trim()).map_err(|_| VisionFailure::Configuration)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err(VisionFailure::Configuration);
    }
    match url.scheme() {
        "https" => {}
        "http" => {
            let host = url.host_str().ok_or(VisionFailure::Configuration)?;
            let private = host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|addr| match addr {
                        std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback(),
                        std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
                    });
            if !private {
                return Err(VisionFailure::Configuration);
            }
        }
        _ => return Err(VisionFailure::Configuration),
    }
    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with("/chat/completions") {
        path.to_string()
    } else {
        format!("{path}/chat/completions")
    };
    url.set_path(&path);
    Ok(url)
}
/// 把图片字节编码成 OpenAI 兼容的 `data:` URL（多模态消息与视觉请求共用）。
pub fn data_url(mime: &str, image: &[u8]) -> String {
    format!("data:{mime};base64,{}", STANDARD.encode(image))
}

impl OpenAiVision {
    pub fn new(base: &str, model: &str, key: &str) -> Result<Self, VisionFailure> {
        if model.trim().is_empty() || model.len() > 200 || key.trim().is_empty() || key.len() > 8192
        {
            return Err(VisionFailure::Configuration);
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| VisionFailure::Configuration)?;
        Ok(Self {
            client,
            url: endpoint(base)?,
            model: model.into(),
            key: key.into(),
        })
    }
    fn body(&self, question: &str, mime: &str, image: &[u8]) -> Value {
        let data = data_url(mime, image);
        json!({"model":self.model,"stream":false,"messages":[{"role":"user","content":[
   {"type":"text","text":question},{"type":"image_url","image_url":{"url":data}}
  ]}],"max_tokens":512})
    }
}
impl VisionProvider for OpenAiVision {
    fn explain<'a>(
        &'a self,
        question: &'a str,
        mime: &'a str,
        image: &'a [u8],
    ) -> BoxFuture<'a, Result<String, VisionFailure>> {
        Box::pin(async move {
            let mut request = self.client.post(self.url.clone()).bearer_auth(&self.key);
            if self.url.host_str().is_some_and(|h| {
                let lower = h.to_ascii_lowercase();
                lower == "cline.bot" || lower.ends_with(".cline.bot")
            }) {
                request = request
                    .header("X-Response-Format", "openai")
                    .header("HTTP-Referer", "https://cline.bot")
                    .header("X-Title", "Cline")
                    .header("X-IS-MULTIROOT", "false")
                    .header("X-CLIENT-TYPE", "cline-desktop");
            }
            let response = request
                .json(&self.body(question, mime, image))
                .send()
                .await
                .map_err(|e| {
                    if e.is_timeout() {
                        VisionFailure::Timeout
                    } else {
                        VisionFailure::Upstream
                    }
                })?;
            if response.status().as_u16() == 401 || response.status().as_u16() == 403 {
                return Err(VisionFailure::Auth);
            }
            if !response.status().is_success() {
                return Err(VisionFailure::Upstream);
            }
            if response
                .content_length()
                .is_some_and(|v| v > MAX_REPLY_BYTES as u64)
            {
                return Err(VisionFailure::Malformed);
            }
            let mut response = response;
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|error| {
                if error.is_timeout() {
                    VisionFailure::Timeout
                } else {
                    VisionFailure::Upstream
                }
            })? {
                if bytes.len().saturating_add(chunk.len()) > MAX_REPLY_BYTES {
                    return Err(VisionFailure::Malformed);
                }
                bytes.extend_from_slice(&chunk);
            }
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| VisionFailure::Malformed)?;
            let answer = value
                .pointer("/choices/0/message/content")
                .or_else(|| value.pointer("/data/choices/0/message/content"))
                .and_then(Value::as_str)
                .ok_or(VisionFailure::Malformed)?;
            if answer.trim().is_empty() {
                return Err(VisionFailure::Malformed);
            }
            Ok(answer.chars().take(MAX_RESULT_CHARS).collect())
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    async fn fake_server(
        status: &'static str,
        response: &'static str,
    ) -> (
        String,
        tokio::sync::oneshot::Receiver<(String, serde_json::Value)>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let (send, receive) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut tmp = [0u8; 4096];
            let end = loop {
                let n = socket.read(&mut tmp).await.unwrap();
                if n == 0 {
                    return;
                }
                bytes.extend_from_slice(&tmp[..n]);
                if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let head = String::from_utf8_lossy(&bytes[..end]).to_string();
            let len = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|s| s.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            while bytes.len() < end + len {
                let n = socket.read(&mut tmp).await.unwrap();
                if n == 0 {
                    return;
                }
                bytes.extend_from_slice(&tmp[..n]);
            }
            let body = serde_json::from_slice(&bytes[end..end + len]).unwrap();
            let _ = send.send((head, body));
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
        });
        (base, receive)
    }

    #[tokio::test]
    async fn real_http_fixture_has_correct_multimodal_payload_and_bearer() {
        let (url, captured) = fake_server(
            "200 OK",
            r#"{"choices":[{"message":{"content":"这是一张小图片"}}]}"#,
        )
        .await;
        let client =
            OpenAiVision::new(&url, "vision-test", "test-secret-please-never-log").unwrap();
        assert_eq!(
            client
                .explain("这是什么", "image/png", b"png")
                .await
                .unwrap(),
            "这是一张小图片"
        );
        let (head, request) = captured.await.unwrap();
        assert!(head.starts_with("POST /v1/chat/completions HTTP/1.1"));
        assert!(
            head.to_lowercase()
                .contains("authorization: bearer test-secret-please-never-log")
        );
        assert_eq!(request["max_tokens"], 512);
        assert_eq!(request["messages"][0]["content"][0]["text"], "这是什么");
        assert_eq!(
            request["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,cG5n"
        );
        assert!(request.get("tools").is_none());

        let (url, _) = fake_server(
            "200 OK",
            r#"{"success":true,"data":{"choices":[{"message":{"content":"套壳图片描述"}}]}}"#,
        )
        .await;
        let client = OpenAiVision::new(&url, "cline-pass/qwen3.8-max", "test-secret").unwrap();
        assert_eq!(
            client
                .explain("这是什么", "image/png", b"png")
                .await
                .unwrap(),
            "套壳图片描述"
        );
    }

    #[tokio::test]
    async fn fake_model_auth_failure_and_malformed_reply_are_not_accepted() {
        let (url, _) = fake_server("401 Unauthorized", r#"{"error":"bad key"}"#).await;
        let client = OpenAiVision::new(&url, "vision-test", "secret").unwrap();
        assert_eq!(
            client.explain("what", "image/png", b"x").await.unwrap_err(),
            VisionFailure::Auth
        );
        let (url, _) = fake_server("200 OK", r#"{"choices":[{"message":{"content":null}}]}"#).await;
        let client = OpenAiVision::new(&url, "vision-test", "secret").unwrap();
        assert_eq!(
            client.explain("what", "image/png", b"x").await.unwrap_err(),
            VisionFailure::Malformed
        );
        for (status, reply, expected) in [
            ("403 Forbidden", "{}", VisionFailure::Auth),
            ("302 Found", "{}", VisionFailure::Upstream),
            ("429 Too Many Requests", "{}", VisionFailure::Upstream),
            ("500 Internal Server Error", "{}", VisionFailure::Upstream),
            ("200 OK", "not json", VisionFailure::Malformed),
            (
                "200 OK",
                r#"{"choices":[{"message":{"content":"  "}}]}"#,
                VisionFailure::Malformed,
            ),
        ] {
            let (url, _) = fake_server(status, reply).await;
            assert_eq!(
                OpenAiVision::new(&url, "fixture", "secret")
                    .unwrap()
                    .explain("what", "image/png", b"x")
                    .await
                    .unwrap_err(),
                expected
            );
        }
    }
    #[test]
    fn url_must_be_explicitly_trusted() {
        assert_eq!(
            endpoint("https://api.example.test/v1").unwrap().path(),
            "/v1/chat/completions"
        );
        assert!(endpoint("https://user:secret@example.com/v1").is_err());
        assert!(endpoint("http://google.com/v1").is_err());
        assert!(endpoint("file:///etc/passwd").is_err());
        assert!(endpoint("http://127.0.0.1:1234/v1").is_ok());
    }
    #[test]
    fn request_contains_image_and_no_tools() {
        let v = OpenAiVision::new("http://127.0.0.1:1234/v1", "vision-v1", "fake").unwrap();
        let request = v.body("what?", "image/png", b"abc");
        assert_eq!(request["messages"][0]["content"][0]["text"], "what?");
        assert_eq!(
            request["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,YWJj"
        );
        assert!(request.get("tools").is_none());
        assert_eq!(request["stream"], false);
    }
}
