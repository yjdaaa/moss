//! WebSocket 桥接（小智 MCP_ENDPOINT）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::{HostBackend, McpTelemetry, SharedBackend, handle_json_rpc_with, telemetry_for};

/// 附加接入点的数量上限（另有一个主接入点 `mcp_endpoint`）。
pub const MAX_EXTRA_ENDPOINTS: usize = 4;

/// 端点可用性：为空或仍是模板占位符（含 `<your-token>`）视为未配置。
pub fn endpoint_from_value(value: &str) -> Option<String> {
    laser_contracts::parse_mcp_endpoint_url(value)
}

pub fn endpoint_from_env() -> Option<String> {
    endpoint_from_value(&std::env::var("MCP_ENDPOINT").ok()?)
}

/// 桥接统计（供测试与日志）。
#[derive(Debug, Default)]
pub struct BridgeStats {
    pub connections: std::sync::atomic::AtomicU64,
    pub messages_in: std::sync::atomic::AtomicU64,
    pub messages_out: std::sync::atomic::AtomicU64,
}

/// 桥的时间参数。生产用 [`BridgeTiming::default`]；测试缩短以便验证心跳与断线判定。
#[derive(Debug, Clone, Copy)]
pub(crate) struct BridgeTiming {
    /// 向接入点发 WebSocket Ping 的间隔
    pub(crate) heartbeat: Duration,
    /// 超过这么久收不到接入点的任何数据（含 Pong）即判定断线：网络中断时 TCP 可能很久都不报错
    pub(crate) idle_limit: Duration,
    /// 连接保持超过这么久后断开，重连等待从头（1 秒）开始
    pub(crate) stable: Duration,
    pub(crate) first_backoff: Duration,
    pub(crate) max_backoff: Duration,
    /// 读超时：周期性检查退出标志、心跳与断线
    pub(crate) poll: Duration,
}

impl Default for BridgeTiming {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_secs(30),
            idle_limit: Duration::from_secs(90),
            stable: Duration::from_secs(60),
            first_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            poll: Duration::from_millis(500),
        }
    }
}

/// 阻塞运行一个接入点的桥接循环，直到 `shutdown` 置位。`link_id` 是遥测里的接入点编号
/// （`primary`、`extra-1`……）。断线按 1s→60s 指数退避重连；连接稳定运行 60 秒以上后断开，等待从 1 秒重新开始。
pub fn run_bridge(
    link_id: &str,
    endpoint: &str,
    backend: Option<SharedBackend>,
    developer_mode: bool,
    shutdown: Arc<AtomicBool>,
    stats: Arc<BridgeStats>,
) {
    run_bridge_timed(
        link_id,
        endpoint,
        backend,
        developer_mode,
        &shutdown,
        &stats,
        BridgeTiming::default(),
    );
}

pub(crate) fn run_bridge_timed(
    link_id: &str,
    endpoint: &str,
    backend: Option<SharedBackend>,
    developer_mode: bool,
    shutdown: &AtomicBool,
    stats: &BridgeStats,
    timing: BridgeTiming,
) {
    let mut backoff = timing.first_backoff;
    let redacted = redact_endpoint(endpoint);
    while !shutdown.load(Ordering::SeqCst) {
        let started = std::time::Instant::now();
        let outcome = serve_one_connection(
            link_id,
            endpoint,
            backend.as_deref(),
            developer_mode,
            shutdown,
            stats,
            timing,
        );
        match &outcome {
            Ok(()) => eprintln!("[mcp-bridge] 连接正常关闭: {redacted}"),
            Err(error) => {
                let error = scrub_token(error, endpoint);
                telemetry_for(backend.as_deref()).link_down(link_id, Some(&error));
                eprintln!(
                    "[mcp-bridge] 连接失败/断开 ({redacted}): {error}；{}s 后重连",
                    backoff.as_secs()
                );
            }
        }
        if outcome.is_ok() || started.elapsed() >= timing.stable {
            backoff = timing.first_backoff;
        }
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let wait_until = std::time::Instant::now() + backoff;
        while std::time::Instant::now() < wait_until && !shutdown.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
        }
        backoff = (backoff * 2).min(timing.max_backoff);
    }
}

/// 连接存续期间把该接入点标记为已连接；离开作用域（正常关闭、出错、退出）时标记为断开。
struct LinkGuard<'a> {
    telemetry: &'a McpTelemetry,
    id: String,
}

impl<'a> LinkGuard<'a> {
    fn connect(telemetry: &'a McpTelemetry, id: &str, redacted_endpoint: &str) -> Self {
        telemetry.link_up(id, redacted_endpoint);
        telemetry.record_event(&format!("[LINK] 已连接接入点 {redacted_endpoint}"));
        Self {
            telemetry,
            id: id.to_owned(),
        }
    }
}

impl Drop for LinkGuard<'_> {
    fn drop(&mut self) {
        self.telemetry.link_down(&self.id, None);
        self.telemetry
            .record_event(&format!("[LINK] 与接入点（{}）的连接已断开", self.id));
    }
}

/// 按 URL 解析脱敏 userinfo 与全部查询参数（`***`），用于日志、遥测与页面显示。
pub fn redact_endpoint(endpoint: &str) -> String {
    laser_contracts::redact_mcp_endpoint(endpoint)
}

/// 错误文本里如果带出了接入点凭据（userinfo 或查询参数），替换为 `***`。
pub(crate) fn scrub_token(text: &str, endpoint: &str) -> String {
    laser_contracts::scrub_endpoint_secrets(text, endpoint)
}

/// 接入点身份（NV8）：解码地址中 `token=` 的 JWT 载荷，只取标识类字段（键名以 id 结尾，或 purpose / name / sub 等）
/// 与签发、过期时间。不校验签名，只用于在语音中心显示“这是哪个智能体的接入点、何时过期”；不是 JWT 时返回 `None`。
pub fn endpoint_identity(endpoint: &str) -> Option<serde_json::Value> {
    use base64::Engine;
    let token = laser_contracts::mcp_query_param(endpoint, "token")?;
    let mut parts = token.split('.');
    let (_header, payload, _signature) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&bytes).ok()?;
    let mut fields = serde_json::Map::new();
    for (key, value) in &claims {
        let lower = key.to_ascii_lowercase();
        let wanted = lower.ends_with("id")
            || matches!(
                lower.as_str(),
                "purpose" | "name" | "nickname" | "agentname" | "sub"
            );
        let shown = match value {
            serde_json::Value::String(text) => text.chars().take(64).collect::<String>(),
            serde_json::Value::Number(number) => number.to_string(),
            _ => continue,
        };
        if wanted && fields.len() < 8 {
            fields.insert(
                key.chars().take(32).collect(),
                serde_json::Value::String(shown),
            );
        }
    }
    let time = |key: &str| claims.get(key).and_then(serde_json::Value::as_f64);
    let expires_at = time("exp");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0);
    Some(serde_json::json!({
        "fields": fields,
        "issued_at": time("iat"),
        "expires_at": expires_at,
        "expired": expires_at.map(|exp| exp < now),
    }))
}

fn serve_one_connection(
    link_id: &str,
    endpoint: &str,
    backend: Option<&dyn HostBackend>,
    developer_mode: bool,
    shutdown: &AtomicBool,
    stats: &BridgeStats,
    timing: BridgeTiming,
) -> Result<(), String> {
    use std::time::Instant;
    use tungstenite::{Message, stream::MaybeTlsStream};
    let (mut socket, _response) = tungstenite::connect(endpoint).map_err(|e| e.to_string())?;
    stats.connections.fetch_add(1, Ordering::Relaxed);
    eprintln!("[mcp-bridge] 已连接 {}", redact_endpoint(endpoint));
    let telemetry = telemetry_for(backend);
    // 顶栏“小智”状态的来源：连接期间该接入点为已连接，函数以任何方式返回时由守卫置为断开。
    let _link = LinkGuard::connect(telemetry, link_id, &redact_endpoint(endpoint));
    let stream = match socket.get_ref() {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    };
    if let Some(stream) = stream {
        let _ = stream.set_read_timeout(Some(timing.poll));
    }
    let mut last_seen = Instant::now();
    let mut last_ping = Instant::now();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            let _ = socket.close(None);
            return Ok(());
        }
        if last_seen.elapsed() >= timing.idle_limit {
            let _ = socket.close(None);
            return Err(format!(
                "{} 秒没有收到接入点的任何数据，判定连接已断开",
                timing.idle_limit.as_secs()
            ));
        }
        if last_ping.elapsed() >= timing.heartbeat {
            socket
                .send(Message::Ping(Default::default()))
                .map_err(|e| format!("发送心跳失败: {e}"))?;
            last_ping = Instant::now();
        }
        let message = match socket.read() {
            Ok(message) => {
                last_seen = Instant::now();
                message
            }
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                let _ = socket.flush();
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
        match message {
            Message::Text(text) => {
                stats.messages_in.fetch_add(1, Ordering::Relaxed);
                telemetry.record_message();
                let response = match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(message) => {
                        if message.get("method").and_then(serde_json::Value::as_str)
                            == Some("initialize")
                        {
                            let info = |key: &str| {
                                message
                                    .pointer(&format!("/params/clientInfo/{key}"))
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("")
                                    .to_owned()
                            };
                            telemetry.record_link_client(link_id, &info("name"), &info("version"));
                        }
                        handle_json_rpc_with(&message, developer_mode, backend)
                    }
                    Err(error) => Some(serde_json::json!({
                        "jsonrpc": "2.0", "id": serde_json::Value::Null,
                        "error": { "code": -32700, "message": format!("Parse error: {error}") }
                    })),
                };
                if let Some(response) = response {
                    socket
                        .send(Message::Text(response.to_string().into()))
                        .map_err(|e| e.to_string())?;
                    stats.messages_out.fetch_add(1, Ordering::Relaxed);
                }
            }
            Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
                // tungstenite 已自动排队 pong；flush 把它发出去
                let _ = socket.flush();
            }
            Message::Close(_) => return Ok(()),
        }
    }
}
