//! VA2 #4–#5 本机文字直通调试 HTTP 入口（仅监听 `127.0.0.1`）。
//!
//! 严格分层：位于语音服务侧 `moss-voice-server`，与激光域（`laser-api`）互不依赖；
//! 独立于全网卡 OTA/HTTP 路由（`0.0.0.0:8003`）之外单独监听本机回环地址。
//!
//! 安全门禁：
//! 1. 仅绑定 `127.0.0.1`，并在处理入口再次校验 `ConnectInfo<SocketAddr>` 为回环地址；
//! 2. 拒绝任何带 `Origin` 请求头的浏览器跨源访问；
//! 3. 要求 `debug_text_inject_enabled` 开启、令牌未过期（24 小时自动作废）、且 `Authorization: Bearer <token>` 常量时间匹配；
//! 4. 仅提供 4 项受限操作：列出在线设备、注入文字（恒带 `simulated = true`）、查询单轮明细、启停本次过程记录归档；
//!    不提供直接调用工具、修改其他设置或下发设备命令的接口。

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Shared;

/// 本机调试入口默认回环端口（被占用时回退系统分配端口并写入运行目录元数据文件）。
pub const DEFAULT_DEBUG_PORT: u16 = 18009;
/// 调试令牌最长有效期：24 小时（秒）。
pub const DEBUG_TOKEN_TTL_SECS: u64 = 24 * 60 * 60;

/// 过程记录开关控制回调（由宿主注入，负责读写 `settings.json` 的 `process_trace_enabled` 并刷新 `ProcessTrace`）。
pub trait ProcessTraceControl: Send + Sync {
    fn set_enabled(&self, enabled: bool) -> Result<ProcessTraceToggleResult, String>;
    fn flush(&self);
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessTraceToggleResult {
    pub enabled: bool,
    pub previous_enabled: bool,
    pub trace_dir_rel: String,
    pub using_custom_dir: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DebugApiAuthState {
    pub enabled: bool,
    pub token: Option<String>,
    pub issued_at: u64,
    pub expires_at: u64,
}

pub struct DebugApiGate {
    inner: Mutex<DebugApiAuthState>,
    now_fn: Arc<dyn Fn() -> u64 + Send + Sync>,
    on_expired: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Default for DebugApiGate {
    fn default() -> Self {
        Self::new()
    }
}

impl DebugApiGate {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(DebugApiAuthState::default()),
            now_fn: Arc::new(crate::unix_now),
            on_expired: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub fn with_clock(now_fn: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        Self {
            inner: Mutex::new(DebugApiAuthState::default()),
            now_fn,
            on_expired: Mutex::new(None),
        }
    }

    pub fn set_on_expired(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        if let Ok(mut slot) = self.on_expired.lock() {
            *slot = cb;
        }
    }

    pub fn enable(&self, token: String, issued_at: u64, expires_at: u64) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.enabled = true;
            guard.token = Some(token);
            guard.issued_at = issued_at;
            guard.expires_at = expires_at;
        }
    }

    pub fn disable(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.enabled = false;
            guard.token = None;
            guard.issued_at = 0;
            guard.expires_at = 0;
        }
    }

    pub fn snapshot(&self) -> DebugApiAuthState {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    fn verify(&self, headers: &HeaderMap, peer: SocketAddr) -> Result<(), (StatusCode, Value)> {
        if !peer.ip().is_loopback() {
            return Err((
                StatusCode::FORBIDDEN,
                json!({
                    "error": "loopback_only",
                    "message": "调试入口仅允许本机 127.0.0.1 访问"
                }),
            ));
        }
        if headers.contains_key(axum::http::header::ORIGIN) {
            return Err((
                StatusCode::FORBIDDEN,
                json!({
                    "error": "origin_not_allowed",
                    "message": "调试入口拒绝浏览器跨源访问"
                }),
            ));
        }
        let now = (self.now_fn)();
        let expected_token = {
            let mut guard = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !guard.enabled || guard.token.is_none() {
                return Err((
                    StatusCode::FORBIDDEN,
                    json!({
                        "error": "debug_disabled",
                        "message": "本机 AI 调试注入开关未开启"
                    }),
                ));
            }
            if guard.expires_at > 0 && now >= guard.expires_at {
                guard.enabled = false;
                guard.token = None;
                guard.issued_at = 0;
                guard.expires_at = 0;
                drop(guard);
                if let Ok(slot) = self.on_expired.lock()
                    && let Some(cb) = slot.as_ref()
                {
                    let cb = cb.clone();
                    std::thread::spawn(move || cb());
                }
                return Err((
                    StatusCode::UNAUTHORIZED,
                    json!({
                        "error": "token_expired",
                        "message": "调试令牌已满 24 小时自动作废，请在桌面主窗口重新开启"
                    }),
                ));
            }
            guard.token.clone().unwrap_or_default()
        };
        let provided = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                let mut parts = v.splitn(2, ' ');
                let scheme = parts.next()?.trim();
                let token = parts.next()?.trim();
                (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
            });
        let Some(provided) = provided else {
            return Err((
                StatusCode::UNAUTHORIZED,
                json!({
                    "error": "unauthorized",
                    "message": "缺少调试令牌（Authorization: Bearer <token>）"
                }),
            ));
        };
        if !constant_time_eq(provided.as_bytes(), expected_token.as_bytes()) {
            return Err((
                StatusCode::UNAUTHORIZED,
                json!({
                    "error": "unauthorized",
                    "message": "调试令牌无效"
                }),
            ));
        }
        Ok(())
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[derive(Clone)]
pub(crate) struct DebugApiState {
    pub shared: Arc<Shared>,
    pub gate: Arc<DebugApiGate>,
    pub trace_control: Option<Arc<dyn ProcessTraceControl>>,
    pub active_laser_job: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

/// 构建本机调试入口 axum 路由（仅暴露 VA2 #5 规定的 4 项操作）。
pub(crate) fn router(state: DebugApiState) -> Router {
    Router::new()
        .route("/debug/v1/devices", get(list_devices))
        .route("/debug/v1/devices/{device_id}/inject", post(inject_text))
        .route("/debug/v1/turns/{turn_id}", get(get_turn_detail))
        .route("/debug/v1/process-trace", post(toggle_process_trace))
        .with_state(state)
}

/// 严格绑定 `127.0.0.1`，绝不监听 `0.0.0.0` 或局域网地址。
pub(crate) fn bind_loopback_listener(
    preferred_port: u16,
) -> Result<(std::net::TcpListener, u16), String> {
    let bind_one = |port: u16| -> Result<(std::net::TcpListener, u16), std::io::Error> {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let listener = std::net::TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        let actual = listener.local_addr()?.port();
        Ok((listener, actual))
    };
    match bind_one(preferred_port) {
        Ok(pair) => Ok(pair),
        Err(_) if preferred_port != 0 => {
            bind_one(0).map_err(|e| format!("绑定本机调试端口 127.0.0.1 失败：{e}"))
        }
        Err(e) => Err(format!("绑定本机调试端口 127.0.0.1 失败：{e}")),
    }
}

fn json_error(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

async fn list_devices(
    State(state): State<DebugApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Err((status, body)) = state.gate.verify(&headers, peer) {
        return json_error(status, body);
    }
    let sessions = state.shared.active_device_sessions();
    let active_laser_job = match state.active_laser_job.clone() {
        Some(probe) => tokio::task::spawn_blocking(move || probe())
            .await
            .unwrap_or(false),
        None => false,
    };
    let devices: Vec<Value> = sessions
        .into_iter()
        .map(|s| {
            json!({
                "device_id": s.device_id,
                "session_id": s.session_id,
                "state": s.state,
                "level": s.level,
                "transport": s.transport,
                "connected_at": s.connected_at,
                "device_mcp_enabled": s.device_mcp_enabled,
                "device_tools_ready": s.device_tools_ready,
                "device_tools_count": s.device_tools.len(),
                "screen_monochrome": s.screen_monochrome,
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(json!({
            "mode": state.shared.config.mode,
            "active_laser_job": active_laser_job,
            "devices": devices,
        })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct InjectBody {
    text: String,
}

async fn inject_text(
    State(state): State<DebugApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(body): Json<InjectBody>,
) -> Response {
    if let Err((status, err)) = state.gate.verify(&headers, peer) {
        return json_error(status, err);
    }
    // 本机调试入口注入的轮次强制 simulated = true，调用方无法绕过模拟门禁。
    match state
        .shared
        .inject_text_async(&device_id, body.text, true)
        .await
    {
        Ok(ack) => (StatusCode::ACCEPTED, Json(ack)).into_response(),
        Err(err) => {
            let status =
                StatusCode::from_u16(err.http_status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            json_error(
                status,
                json!({
                    "error": err.code,
                    "message": err.message,
                }),
            )
        }
    }
}

async fn get_turn_detail(
    State(state): State<DebugApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(turn_id): Path<String>,
) -> Response {
    if let Err((status, err)) = state.gate.verify(&headers, peer) {
        return json_error(status, err);
    }
    let Some(detail) = state.shared.state.turn_detail(&turn_id) else {
        return json_error(
            StatusCode::NOT_FOUND,
            json!({
                "error": "turn_not_found",
                "message": "找不到该轮次的联调明细"
            }),
        );
    };
    let session = state.shared.state.session_by_id(&detail.session_id);
    let mut value = serde_json::to_value(&detail).unwrap_or_else(|_| json!({}));
    if let Some(map) = value.as_object_mut() {
        map.insert("device_online".into(), Value::Bool(session.is_some()));
        map.insert(
            "device_state".into(),
            session
                .as_ref()
                .map(|s| Value::String(s.state.clone()))
                .unwrap_or_else(|| Value::String("offline".into())),
        );
        map.insert(
            "device_level".into(),
            Value::from(session.as_ref().map_or(0, |s| s.level)),
        );
    }
    (StatusCode::OK, Json(value)).into_response()
}

#[derive(Deserialize)]
struct ProcessTraceBody {
    enabled: bool,
    #[serde(default)]
    flush: Option<bool>,
}

async fn toggle_process_trace(
    State(state): State<DebugApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<ProcessTraceBody>,
) -> Response {
    if let Err((status, err)) = state.gate.verify(&headers, peer) {
        return json_error(status, err);
    }
    let Some(control) = state.trace_control.clone() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({
                "error": "process_trace_unavailable",
                "message": "过程记录控制未挂载"
            }),
        );
    };
    let should_flush = body.flush.unwrap_or(!body.enabled);
    let enabled = body.enabled;
    let outcome = tokio::task::spawn_blocking(move || {
        if should_flush {
            control.flush();
        }
        let result = control.set_enabled(enabled)?;
        if should_flush {
            control.flush();
        }
        Ok::<_, String>(result)
    })
    .await;
    match outcome {
        Ok(Ok(result)) => (StatusCode::OK, Json(result)).into_response(),
        Ok(Err(message)) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({
                "error": "process_trace_failed",
                "message": message,
            }),
        ),
        Err(_) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({
                "error": "process_trace_failed",
                "message": "过程记录切换任务异常中断",
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn dummy_headers(token: Option<&str>, origin: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(t) = token {
            h.insert(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {t}").parse().unwrap(),
            );
        }
        if let Some(o) = origin {
            h.insert(axum::http::header::ORIGIN, o.parse().unwrap());
        }
        h
    }

    #[test]
    fn gate_enforces_loopback_origin_switch_token_and_24h_ttl() {
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let c = clock.clone();
        let gate = DebugApiGate::with_clock(Arc::new(move || c.load(Ordering::Acquire)));
        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 51234);
        let lan = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 51234);

        // 开关关闭时拒绝
        let err = gate
            .verify(&dummy_headers(Some("tok"), None), loopback)
            .unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert_eq!(err.1["error"], "debug_disabled");

        // 开启后：非回环地址拒绝
        gate.enable(
            "secret-token-123".into(),
            1_000_000,
            1_000_000 + DEBUG_TOKEN_TTL_SECS,
        );
        let err = gate
            .verify(&dummy_headers(Some("secret-token-123"), None), lan)
            .unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert_eq!(err.1["error"], "loopback_only");

        // 带浏览器 Origin 拒绝
        let err = gate
            .verify(
                &dummy_headers(Some("secret-token-123"), Some("http://localhost:5173")),
                loopback,
            )
            .unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert_eq!(err.1["error"], "origin_not_allowed");

        // 缺少或错误令牌拒绝
        let err = gate
            .verify(&dummy_headers(None, None), loopback)
            .unwrap_err();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
        let err = gate
            .verify(&dummy_headers(Some("wrong-token"), None), loopback)
            .unwrap_err();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);

        // 正确令牌 + 回环放行
        assert!(
            gate.verify(&dummy_headers(Some("secret-token-123"), None), loopback)
                .is_ok()
        );

        // 满 24 小时后自动作废
        clock.store(1_000_000 + DEBUG_TOKEN_TTL_SECS, Ordering::Release);
        let err = gate
            .verify(&dummy_headers(Some("secret-token-123"), None), loopback)
            .unwrap_err();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
        assert_eq!(err.1["error"], "token_expired");
        assert!(!gate.snapshot().enabled, "过期后应立即清掉内存开关状态");
    }
}
