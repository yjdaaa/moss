//! VA2 #4–#5 / VC1 / VC2 本机调试 HTTP 入口（仅监听 `127.0.0.1`）。
//!
//! 严格分层：位于语音服务侧 `moss-voice-server`，与激光域（`laser-api`）互不依赖；
//! 独立于全网卡 OTA/HTTP 路由（`0.0.0.0:8003`）之外单独监听本机回环地址。
//!
//! 安全门禁：
//! 1. 仅绑定 `127.0.0.1`，并在处理入口再次校验 `ConnectInfo<SocketAddr>` 为回环地址；
//! 2. 拒绝任何带 `Origin` 请求头的浏览器跨源访问；
//! 3. 要求 `debug_text_inject_enabled` 开启、令牌未过期（24 小时自动作废）、且 `Authorization: Bearer <token>` 常量时间匹配；
//! 4. 仅提供受限联调操作：列出在线设备、文字注入（恒带 `simulated = true`）、测试时间窗（VC1）、
//!    时间窗内数字音频注入对照（VC2）、查询轮次明细、启停本次过程记录归档；
//!    不提供直接调用工具、修改其他设置或下发设备命令的接口。

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::Shared;

/// 本机调试入口默认回环端口（被占用时回退系统分配端口并写入运行目录元数据文件）。
pub const DEFAULT_DEBUG_PORT: u16 = 18009;
/// 调试令牌最长有效期：24 小时（秒）。
pub const DEBUG_TOKEN_TTL_SECS: u64 = 24 * 60 * 60;
/// VC1 测试时间窗默认最长时长（分钟）。
pub const DEFAULT_BENCH_WINDOW_MINUTES: u32 = 30;
/// VC1 测试时间窗硬上限（分钟）。
pub const MAX_BENCH_WINDOW_MINUTES: u32 = 60;
/// VC2 数字注入音频单次最长时长（秒）。
const MAX_INJECT_AUDIO_SECS: usize = 30;

/// VC1 单台设备的测试时间窗视图（用于桌面端红标、调试接口返回与日志记录）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BenchWindowView {
    pub device_id: String,
    pub session_id: String,
    pub active: bool,
    pub opened_at: u64,
    pub expires_at: u64,
    pub max_minutes: u32,
}

#[derive(Debug, Clone)]
struct BenchWindowEntry {
    device_id: String,
    session_id: String,
    opened_at: u64,
    expires_at: u64,
    max_minutes: u32,
    deadline: Instant,
}

impl BenchWindowEntry {
    fn to_view(&self, active: bool) -> BenchWindowView {
        BenchWindowView {
            device_id: self.device_id.clone(),
            session_id: self.session_id.clone(),
            active,
            opened_at: self.opened_at,
            expires_at: self.expires_at,
            max_minutes: self.max_minutes,
        }
    }
}

/// VC1 按设备维度管理的测试时间窗状态存储。
pub struct BenchWindowStore {
    entries: Mutex<BTreeMap<String, BenchWindowEntry>>,
    now_fn: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Default for BenchWindowStore {
    fn default() -> Self {
        Self::new()
    }
}

impl BenchWindowStore {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            now_fn: Arc::new(crate::unix_now),
        }
    }

    #[cfg(test)]
    pub fn with_clock(now_fn: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            now_fn,
        }
    }

    pub fn open(&self, device_id: &str, session_id: &str, max_minutes: u32) -> BenchWindowView {
        let clamped = max_minutes.clamp(1, MAX_BENCH_WINDOW_MINUTES);
        let ttl = Duration::from_secs(u64::from(clamped) * 60);
        self.open_internal(device_id, session_id, clamped, ttl)
    }

    #[cfg(test)]
    pub fn open_with_duration(
        &self,
        device_id: &str,
        session_id: &str,
        ttl: Duration,
    ) -> BenchWindowView {
        self.open_internal(device_id, session_id, 1, ttl)
    }

    fn open_internal(
        &self,
        device_id: &str,
        session_id: &str,
        max_minutes: u32,
        ttl: Duration,
    ) -> BenchWindowView {
        let now = (self.now_fn)();
        let expires_at = now.saturating_add(ttl.as_secs().max(1));
        let entry = BenchWindowEntry {
            device_id: device_id.to_owned(),
            session_id: session_id.to_owned(),
            opened_at: now,
            expires_at,
            max_minutes,
            deadline: Instant::now() + ttl,
        };
        let view = entry.to_view(true);
        if let Ok(mut guard) = self.entries.lock() {
            guard.insert(device_id.to_owned(), entry);
        }
        view
    }

    pub fn close(&self, device_id: &str) -> Option<BenchWindowView> {
        self.entries
            .lock()
            .ok()?
            .remove(device_id)
            .map(|e| e.to_view(false))
    }

    pub fn close_if_session(&self, device_id: &str, session_id: &str) -> Option<BenchWindowView> {
        let mut guard = self.entries.lock().ok()?;
        let matches = guard
            .get(device_id)
            .is_some_and(|e| e.session_id == session_id);
        if matches {
            guard.remove(device_id).map(|e| e.to_view(false))
        } else {
            None
        }
    }

    pub fn close_all(&self) -> Vec<BenchWindowView> {
        let Ok(mut guard) = self.entries.lock() else {
            return Vec::new();
        };
        let keys: Vec<String> = guard.keys().cloned().collect();
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(entry) = guard.remove(&key) {
                out.push(entry.to_view(false));
            }
        }
        out
    }

    pub fn expire_due(&self) -> Vec<BenchWindowView> {
        let now = (self.now_fn)();
        let now_inst = Instant::now();
        let Ok(mut guard) = self.entries.lock() else {
            return Vec::new();
        };
        let expired_keys: Vec<String> = guard
            .iter()
            .filter_map(|(k, e)| {
                (now >= e.expires_at || now_inst >= e.deadline).then_some(k.clone())
            })
            .collect();
        let mut out = Vec::with_capacity(expired_keys.len());
        for key in expired_keys {
            if let Some(entry) = guard.remove(&key) {
                out.push(entry.to_view(false));
            }
        }
        out
    }

    pub fn active_for(&self, device_id: &str) -> Option<BenchWindowView> {
        let now = (self.now_fn)();
        let now_inst = Instant::now();
        let guard = self.entries.lock().ok()?;
        let entry = guard.get(device_id)?;
        (now < entry.expires_at && now_inst < entry.deadline).then(|| entry.to_view(true))
    }

    pub fn next_deadline_for(&self, device_id: &str) -> Option<Instant> {
        self.entries.lock().ok()?.get(device_id).map(|e| e.deadline)
    }

    pub fn list_active(&self) -> Vec<BenchWindowView> {
        let now = (self.now_fn)();
        let now_inst = Instant::now();
        self.entries
            .lock()
            .map(|guard| {
                guard
                    .values()
                    .filter(|e| now < e.expires_at && now_inst < e.deadline)
                    .map(|e| e.to_view(true))
                    .collect()
            })
            .unwrap_or_default()
    }
}

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
    pub expired: bool,
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
            guard.expired = false;
            guard.token = Some(token);
            guard.issued_at = issued_at;
            guard.expires_at = expires_at;
        }
    }

    pub fn disable(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.enabled = false;
            guard.expired = false;
            guard.token = None;
            guard.issued_at = 0;
            guard.expires_at = 0;
        }
    }

    pub fn snapshot(&self) -> DebugApiAuthState {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// 检查 24 小时调试令牌是否已到期；若到期则立即清掉内存状态、触发回调并返回 `true`。
    pub fn check_expiry(&self) -> bool {
        let now = (self.now_fn)();
        let expired = {
            let mut guard = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.enabled && guard.expires_at > 0 && now >= guard.expires_at {
                guard.enabled = false;
                guard.expired = true;
                guard.token = None;
                guard.issued_at = 0;
                guard.expires_at = 0;
                true
            } else {
                false
            }
        };
        if expired {
            self.fire_on_expired();
        }
        expired
    }

    pub fn is_enabled_valid(&self) -> bool {
        if self.check_expiry() {
            return false;
        }
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.enabled && guard.token.is_some()
    }

    pub fn remaining_until_expiry(&self) -> Option<Duration> {
        let now = (self.now_fn)();
        let guard = self.inner.lock().ok()?;
        if !guard.enabled || guard.expires_at == 0 {
            return None;
        }
        Some(Duration::from_secs(guard.expires_at.saturating_sub(now)))
    }

    #[cfg(test)]
    pub fn force_expire_for_test(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.expires_at = 1;
        }
    }

    fn fire_on_expired(&self) {
        if let Ok(slot) = self.on_expired.lock()
            && let Some(cb) = slot.as_ref()
        {
            let cb = cb.clone();
            std::thread::spawn(move || cb());
        }
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
            if guard.expired {
                return Err((
                    StatusCode::UNAUTHORIZED,
                    json!({
                        "error": "token_expired",
                        "message": "调试令牌已满 24 小时自动作废，请在桌面主窗口重新开启"
                    }),
                ));
            }
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
                guard.expired = true;
                guard.token = None;
                guard.issued_at = 0;
                guard.expires_at = 0;
                drop(guard);
                self.fire_on_expired();
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

/// 构建本机调试入口 axum 路由（VA2 #5 + VC1 测试时间窗 + VC2 时间窗内数字音频对照）。
pub(crate) fn router(state: DebugApiState) -> Router {
    Router::new()
        .route("/debug/v1/devices", get(list_devices))
        .route(
            "/debug/v1/devices/{device_id}/turns",
            get(list_device_turns),
        )
        .route("/debug/v1/devices/{device_id}/inject", post(inject_text))
        .route(
            "/debug/v1/devices/{device_id}/inject-audio",
            post(inject_audio),
        )
        .route("/debug/v1/bench-window", post(control_bench_window))
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
    let bench_windows = state.shared.bench_windows.list_active();
    let active_laser_job = match state.active_laser_job.clone() {
        Some(probe) => tokio::task::spawn_blocking(move || probe())
            .await
            .unwrap_or(false),
        None => false,
    };
    let devices: Vec<Value> = sessions
        .into_iter()
        .map(|s| {
            let latest_turn_id =
                (s.latest_turn > 0).then(|| format!("{}:{}", s.session_id, s.latest_turn));
            json!({
                "device_id": s.device_id,
                "session_id": s.session_id,
                "state": s.state,
                "level": s.level,
                "recent_peak_level": s.recent_peak_level,
                "latest_turn": s.latest_turn,
                "latest_turn_id": latest_turn_id,
                "bench_window": s.bench_window,
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
            "bench_windows": bench_windows,
            "devices": devices,
        })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct DeviceTurnsQuery {
    #[serde(default)]
    limit: Option<usize>,
}

async fn list_device_turns(
    State(state): State<DebugApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Query(query): Query<DeviceTurnsQuery>,
) -> Response {
    if let Err((status, err)) = state.gate.verify(&headers, peer) {
        return json_error(status, err);
    }
    state.shared.sync_bench_windows();
    let limit = query.limit.unwrap_or(20).clamp(1, 50);
    let turns = state
        .shared
        .state
        .turn_details_for_device(&device_id, limit);
    (
        StatusCode::OK,
        Json(json!({
            "device_id": device_id,
            "turns": turns,
        })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct BenchWindowBody {
    device_id: String,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    open: Option<bool>,
    #[serde(default)]
    max_minutes: Option<u32>,
}

async fn control_bench_window(
    State(state): State<DebugApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<BenchWindowBody>,
) -> Response {
    if let Err((status, err)) = state.gate.verify(&headers, peer) {
        return json_error(status, err);
    }
    let device_id = body.device_id.trim();
    if device_id.is_empty() || device_id.len() > 128 {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({
                "error": "invalid_device_id",
                "message": "device_id 不能为空"
            }),
        );
    }
    let is_open = match (body.action.as_deref().map(str::trim), body.open) {
        (Some("open"), _) => true,
        (Some("close"), _) => false,
        (None, Some(flag)) => flag,
        _ => {
            return json_error(
                StatusCode::BAD_REQUEST,
                json!({
                    "error": "invalid_action",
                    "message": "bench_window 的 action 只能是 open 或 close"
                }),
            );
        }
    };
    if is_open {
        let max_minutes = body.max_minutes.unwrap_or(DEFAULT_BENCH_WINDOW_MINUTES);
        if max_minutes == 0 || max_minutes > MAX_BENCH_WINDOW_MINUTES {
            return json_error(
                StatusCode::BAD_REQUEST,
                json!({
                    "error": "invalid_max_minutes",
                    "message": format!("max_minutes 须为 1 ~ {MAX_BENCH_WINDOW_MINUTES} 分钟")
                }),
            );
        }
        match state.shared.open_bench_window(device_id, max_minutes) {
            Ok(view) => (
                StatusCode::OK,
                Json(json!({
                    "device_id": device_id,
                    "action": "open",
                    "bench_window": view,
                })),
            )
                .into_response(),
            Err(err) => {
                let status = StatusCode::from_u16(err.http_status)
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                json_error(
                    status,
                    json!({
                        "error": err.code,
                        "message": err.message,
                    }),
                )
            }
        }
    } else {
        let closed = state.shared.close_bench_window(device_id, "脚本主动关闭");
        (
            StatusCode::OK,
            Json(json!({
                "device_id": device_id,
                "action": "close",
                "closed": closed,
                "bench_window": Value::Null,
            })),
        )
            .into_response()
    }
}

#[derive(Deserialize)]
struct InjectAudioBody {
    pcm_base64: String,
    #[serde(default)]
    sample_rate: Option<u32>,
    #[serde(default)]
    frame_ms: Option<u32>,
    #[serde(default)]
    lead_silence_ms: Option<u32>,
    #[serde(default)]
    tail_silence_ms: Option<u32>,
}

async fn inject_audio(
    State(state): State<DebugApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(body): Json<InjectAudioBody>,
) -> Response {
    if let Err((status, err)) = state.gate.verify(&headers, peer) {
        return json_error(status, err);
    }
    if !state.shared.is_bench_window_active(&device_id) {
        return json_error(
            StatusCode::FORBIDDEN,
            json!({
                "error": "bench_window_required",
                "message": "数字音频注入仅允许在已打开的测试时间窗内使用"
            }),
        );
    }
    let sample_rate = body.sample_rate.unwrap_or(16_000);
    if !matches!(sample_rate, 16_000 | 24_000) {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({
                "error": "invalid_sample_rate",
                "message": "数字音频注入采样率仅支持 16000 或 24000 Hz"
            }),
        );
    }
    let frame_ms = body.frame_ms.unwrap_or(60);
    if !matches!(frame_ms, 20 | 40 | 60) {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({
                "error": "invalid_frame_ms",
                "message": "frame_ms 仅支持 20、40 或 60"
            }),
        );
    }
    let lead_silence_ms = body.lead_silence_ms.unwrap_or(180).min(1_000);
    let tail_silence_ms = body.tail_silence_ms.unwrap_or(600).clamp(200, 2_000);
    let max_bytes = (sample_rate as usize) * 2 * MAX_INJECT_AUDIO_SECS;
    let raw_bytes = match decode_base64(&body.pcm_base64, max_bytes) {
        Ok(bytes) => bytes,
        Err(message) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                json!({
                    "error": "invalid_audio",
                    "message": message,
                }),
            );
        }
    };
    if raw_bytes.is_empty() || raw_bytes.len() % 2 != 0 {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({
                "error": "invalid_audio",
                "message": "pcm_base64 须为非空且字节数为偶数的 16 位有符号小端 PCM"
            }),
        );
    }
    let pcm: Vec<i16> = raw_bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    match state
        .shared
        .inject_audio_async(
            &device_id,
            pcm,
            sample_rate,
            frame_ms,
            lead_silence_ms,
            tail_silence_ms,
        )
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

fn decode_base64(input: &str, max_decoded_bytes: usize) -> Result<Vec<u8>, String> {
    let mut cleaned = Vec::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        cleaned.push(byte);
    }
    if cleaned.is_empty() || cleaned.len() % 4 != 0 {
        return Err("pcm_base64 编码长度无效".into());
    }
    let estimated = (cleaned.len() / 4) * 3;
    if estimated > max_decoded_bytes + 3 {
        return Err(format!("数字音频超出最长 {MAX_INJECT_AUDIO_SECS} 秒上限"));
    }
    let val = |b: u8| -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(estimated);
    let last_chunk_idx = cleaned.len() / 4 - 1;
    for (idx, chunk) in cleaned.as_chunks::<4>().0.iter().enumerate() {
        let a = val(chunk[0]).ok_or_else(|| "pcm_base64 包含非法字符".to_owned())?;
        let b = val(chunk[1]).ok_or_else(|| "pcm_base64 包含非法字符".to_owned())?;
        let pad2 = chunk[2] == b'=';
        let pad3 = chunk[3] == b'=';
        if (pad2 || pad3) && idx != last_chunk_idx {
            return Err("pcm_base64 填充位置非法".into());
        }
        if pad2 && !pad3 {
            return Err("pcm_base64 填充格式非法".into());
        }
        out.push((a << 2) | (b >> 4));
        if !pad2 {
            let c = val(chunk[2]).ok_or_else(|| "pcm_base64 包含非法字符".to_owned())?;
            out.push((b << 4) | (c >> 2));
            if !pad3 {
                let d = val(chunk[3]).ok_or_else(|| "pcm_base64 包含非法字符".to_owned())?;
                out.push((c << 6) | d);
            }
        }
        if out.len() > max_decoded_bytes {
            return Err(format!("数字音频超出最长 {MAX_INJECT_AUDIO_SECS} 秒上限"));
        }
    }
    Ok(out)
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

    #[test]
    fn bench_window_store_and_base64_decoder_enforce_bounds() {
        let clock = Arc::new(AtomicU64::new(10_000));
        let c = clock.clone();
        let store = BenchWindowStore::with_clock(Arc::new(move || c.load(Ordering::Acquire)));

        let view = store.open("dev-1", "sess-1", 30);
        assert!(view.active);
        assert_eq!(view.max_minutes, 30);
        assert_eq!(view.expires_at, 10_000 + 30 * 60);
        assert!(store.active_for("dev-1").is_some());

        // 不同 session_id 不能误关当前会话的窗口
        assert!(store.close_if_session("dev-1", "sess-other").is_none());
        assert!(store.active_for("dev-1").is_some());

        // 到达最长时长后自动到期清理
        clock.store(10_000 + 30 * 60, Ordering::Release);
        assert!(store.active_for("dev-1").is_none());
        let expired = store.expire_due();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].device_id, "dev-1");
        assert!(store.list_active().is_empty());

        // base64 解码校验：正常填充、非法字符与超长拦截
        assert_eq!(decode_base64("AAECAw==", 16).unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(decode_base64("AQID", 16).unwrap(), vec![1, 2, 3]);
        assert!(decode_base64("!!!", 16).is_err());
        assert!(decode_base64("AAECAw==", 2).is_err());
    }
}
