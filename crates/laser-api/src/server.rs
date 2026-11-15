use super::*;

impl ApiService {
    pub fn serve(listener: TcpListener, service: std::sync::Arc<Self>) -> std::io::Result<()> {
        struct ConnPermit(std::sync::Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for ConnPermit {
            fn drop(&mut self) {
                self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            }
        }
        let active_conns = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(15)));
            // 认证要等读完请求才能做，因此在接入时就按来源分配名额：回环（桌面界面 / Tauri 内嵌页）
            // 始终保有预留名额，局域网慢连接占满也不会让本机急停拿到 503（2026-11-05）。
            let Ok(peer) = stream.peer_addr() else {
                continue;
            };
            let loopback = peer.ip().is_loopback();
            if !try_admit_http_connection(&active_conns, loopback) {
                let busy = http_json(503, failure("并发连接过多，请稍后重试", None));
                let _ = stream.write_all(&busy);
                let _ = stream.flush();
                continue;
            }
            let permit = ConnPermit(active_conns.clone());
            let service = service.clone();
            let _ = std::thread::Builder::new()
                .name("moss-http".into())
                .spawn(move || {
                    let _permit = permit;
                    let source = peer.ip().to_string();
                    let mut carry = Vec::with_capacity(4096);
                    let mut served = 0usize;
                    loop {
                        let idle_timeout = if served == 0 {
                            std::time::Duration::from_secs(10)
                        } else {
                            std::time::Duration::from_secs(5)
                        };
                        let Ok(request) =
                            read_http_request_buffered(&mut stream, &mut carry, idle_timeout)
                        else {
                            break;
                        };
                        if request.is_empty() {
                            break;
                        }
                        served += 1;
                        let keep_alive = request_allows_keep_alive(&request, loopback, served);
                        let raw_response = missing_host_rejection(&request, loopback)
                            .unwrap_or_else(|| {
                                service.handle_http_from(&request, loopback, &source, None)
                            });
                        let (response, kept) = apply_response_keep_alive(raw_response, keep_alive);
                        if stream.write_all(&response).is_err() || stream.flush().is_err() {
                            break;
                        }
                        if !kept {
                            break;
                        }
                    }
                });
        }
        Ok(())
    }
}

/// HTTP 并发连接总上限。
pub(crate) const MAX_CONCURRENT_HTTP: usize = 64;
/// 只留给回环来源的名额：非回环连接最多占 `MAX_CONCURRENT_HTTP - LOOPBACK_RESERVED_HTTP` 个。
pub(crate) const LOOPBACK_RESERVED_HTTP: usize = 8;

/// 接入一条 HTTP 连接并占用名额；名额不足返回 `false`（调用方回 503）。
/// 非回环来源只能用到 `MAX - RESERVED`，回环来源可用满 `MAX`，保证本机界面的急停等请求总有余量。
pub(crate) fn try_admit_http_connection(
    active: &std::sync::atomic::AtomicUsize,
    loopback: bool,
) -> bool {
    let limit = if loopback {
        MAX_CONCURRENT_HTTP
    } else {
        MAX_CONCURRENT_HTTP - LOOPBACK_RESERVED_HTTP
    };
    active
        .try_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |n| (n < limit).then_some(n + 1),
        )
        .is_ok()
}

/// 真实 TCP 入口上的回环请求必须带 `Host`：否则 `is_allowed_loopback_host` 的防 DNS 重绑定检查无从执行。
/// 进程内调用（`handle_http` / `handle_http_with_ctx`）不经过这里。
pub(crate) fn missing_host_rejection(request: &[u8], loopback: bool) -> Option<Vec<u8>> {
    if !loopback || header_value(&request_head(request), "host").is_some() {
        return None;
    }
    Some(http_json(
        403,
        failure(
            "请求缺少 Host 头，请用 127.0.0.1 或 localhost 访问",
            Some(serde_json::json!({"error_code": "host_not_allowed"})),
        ),
    ))
}

#[cfg(test)]
pub(crate) fn read_http_request(stream: &mut std::net::TcpStream) -> std::io::Result<Vec<u8>> {
    let mut carry = Vec::with_capacity(4096);
    read_http_request_buffered(stream, &mut carry, std::time::Duration::from_secs(10))
}

const HTTP_KEEP_ALIVE_MAX_REQUESTS: usize = 64;

fn request_allows_keep_alive(request: &[u8], loopback: bool, served_count: usize) -> bool {
    if !loopback || served_count >= HTTP_KEEP_ALIVE_MAX_REQUESTS {
        return false;
    }
    let head = request_head(request);
    let Some(first_line) = head.lines().next() else {
        return false;
    };
    if !first_line.contains("HTTP/1.1") {
        return false;
    }
    if header_value(&head, "transfer-encoding").is_some() {
        return false;
    }
    if let Some(conn) = header_value(&head, "connection")
        && conn
            .split(',')
            .any(|token| token.trim().eq_ignore_ascii_case("close"))
    {
        return false;
    }
    true
}

fn apply_response_keep_alive(response: Vec<u8>, want_keep_alive: bool) -> (Vec<u8>, bool) {
    if !want_keep_alive {
        return (response, false);
    }
    let Some(head_end) = response.windows(4).position(|w| w == b"\r\n\r\n") else {
        return (response, false);
    };
    let needle = b"\r\nConnection: close\r\n";
    let Some(pos) = response[..head_end + 2]
        .windows(needle.len())
        .position(|w| w == needle)
    else {
        return (response, false);
    };
    let replacement = b"\r\nConnection: keep-alive\r\nKeep-Alive: timeout=5, max=64\r\n";
    let mut out = Vec::with_capacity(response.len() + replacement.len() - needle.len());
    out.extend_from_slice(&response[..pos]);
    out.extend_from_slice(replacement);
    out.extend_from_slice(&response[pos + needle.len()..]);
    (out, true)
}

fn read_http_request_buffered(
    stream: &mut std::net::TcpStream,
    buffer: &mut Vec<u8>,
    idle_timeout: std::time::Duration,
) -> std::io::Result<Vec<u8>> {
    const PER_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
    const TOTAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
    const MAX_HEADER_SIZE: usize = 64 * 1024;
    const MAX_BODY_SIZE: usize = 20 * 1024 * 1024;

    let mut started: Option<std::time::Instant> =
        (!buffer.is_empty()).then(std::time::Instant::now);
    let mut chunk = [0u8; 4096];
    let mut content_length: Option<usize> = None;
    let mut header_end: Option<usize> = None;

    loop {
        if header_end.is_none() {
            if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                header_end = Some(pos + 4);
                let head = String::from_utf8_lossy(&buffer[..pos]);
                for line in head.lines() {
                    if let Some((k, v)) = line.split_once(':')
                        && k.trim().eq_ignore_ascii_case("content-length")
                    {
                        content_length = v.trim().parse::<usize>().ok();
                    }
                }
            } else if buffer.len() > MAX_HEADER_SIZE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "HTTP 请求头超出 64KB 限制",
                ));
            }
        }

        if let Some(end) = header_end {
            let body_len = content_length.unwrap_or(0);
            if body_len > MAX_BODY_SIZE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "HTTP 请求体超出 20MB 限制",
                ));
            }
            let needed = end + body_len;
            if buffer.len() >= needed {
                let rest = buffer.split_off(needed);
                let request = std::mem::replace(buffer, rest);
                return Ok(request);
            }
        }

        let timeout = if let Some(t0) = started {
            let elapsed = t0.elapsed();
            if elapsed >= TOTAL_DEADLINE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "HTTP 请求读取总耗时超出 30 秒限制",
                ));
            }
            TOTAL_DEADLINE
                .saturating_sub(elapsed)
                .min(PER_READ_TIMEOUT)
                .max(std::time::Duration::from_millis(1))
        } else {
            idle_timeout.max(std::time::Duration::from_millis(1))
        };

        let _ = stream.set_read_timeout(Some(timeout));
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            let drained = std::mem::take(buffer);
            return Ok(drained);
        }
        if started.is_none() {
            started = Some(std::time::Instant::now());
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
}

impl ApiService {
    pub fn handle_http(&self, request: &[u8]) -> Vec<u8> {
        self.handle_http_from(request, true, "127.0.0.1", None)
    }

    /// 进程内调用方显式声明身份（技术书 §6.4 规则 2）：`InProcessBackend` 传 `McpBridge`，
    /// 网关分发传 `Gateway`。按本机回环处理鉴权，但上下文用调用方给的值，不借用 `Loopback`。
    pub fn handle_http_with_ctx(
        &self,
        request: &[u8],
        ctx: laser_contracts::CallerContext,
    ) -> Vec<u8> {
        self.handle_http_from(request, true, "127.0.0.1", Some(ctx))
    }

    /// HTTP 入口：先做 Host 校验与浏览器来源校验（技术书 §6.5 / D11），再进入路由。
    ///
    /// - 回环连接的 `Host` 必须是 IP 字面量、`localhost` 或 `MOSS_ALLOWED_ORIGINS` 中的主机名，
    ///   否则 403 `host_not_allowed`（防 DNS 重绑定：攻击者域名解析到 127.0.0.1 时 `Host` 仍是其域名）；
    /// - 不带 `Origin` 的请求（插件、MCP 桥接、curl、局域网配对客户端、进程内调用）行为不变，响应不带 CORS 头；
    /// - 带 `Origin` 且允许：正常路由，响应回显该来源并加 `Vary: Origin`；
    /// - 带 `Origin` 且不允许：`GET`/`HEAD` 正常路由但不带任何 CORS 头（跨站页面读不到响应），
    ///   其他方法（含 `OPTIONS` 预检与 `text/plain` 简单请求）一律 403 `origin_not_allowed`，不进入路由。
    pub(crate) fn handle_http_from(
        &self,
        request: &[u8],
        loopback: bool,
        source_ip: &str,
        ctx_override: Option<laser_contracts::CallerContext>,
    ) -> Vec<u8> {
        let head = request_head(request);
        let method = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().next())
            .unwrap_or("")
            .to_owned();
        let host = header_value(&head, "host");
        if loopback
            && host
                .as_deref()
                .is_some_and(|host| !is_allowed_loopback_host(host, &self.extra_allowed_origins))
        {
            return http_json(
                403,
                failure(
                    "请求的主机名不在允许范围内，请用 127.0.0.1 或 localhost 访问",
                    Some(serde_json::json!({"error_code": "host_not_allowed"})),
                ),
            );
        }
        let Some(origin) = header_value(&head, "origin") else {
            return self.route_http(request, loopback, source_ip, ctx_override.clone());
        };
        if !is_allowed_origin(
            &origin,
            host.as_deref(),
            loopback,
            &self.extra_allowed_origins,
        ) {
            if method == "GET" || method == "HEAD" {
                return self.route_http(request, loopback, source_ip, ctx_override.clone());
            }
            return http_json(
                403,
                failure(
                    "请求来源不在允许范围内",
                    Some(serde_json::json!({"error_code": "origin_not_allowed"})),
                ),
            );
        }
        if method == "OPTIONS" {
            return http_options(Some(&origin));
        }
        with_cors_headers(
            self.route_http(request, loopback, source_ip, ctx_override.clone()),
            &origin,
        )
    }

    pub fn handle(&self, method: &str, path: &str, body: &[u8]) -> ApiResponse {
        let response = match (method, path) {
            ("GET", "/api/health") => (200, success(serde_json::json!({"status": "ok"}))),
            ("GET", "/v1/capabilities") => (
                200,
                success(serde_json::json!({
                    "http": true,
                    "serial": true,
                    "network": true,
                    "shell": false,
                    "tray": cfg!(any(target_os = "windows", target_os = "macos", target_os = "linux")),
                    "command_execution": std::env::var("MOSS_ENABLE_DEVELOPER_COMMANDS")
                        .ok()
                        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
                })),
            ),
            ("POST", "/api/ui/discover-controllers") => {
                // 2026-11-09：mDNS 与本网段 23 端口只是候选，逐台发只读的 `?` 与 `$I`，按 GRBL / FluidNC
                // 回应的才算找到（不再把广播 _http 的打印机、路由器当成雕刻机）。
                // 用 POST：跨站页面无法用不带 Origin 的 GET（如 <img>）反复触发局域网扫描
                (200, success(self.discover_verified_controllers()))
            }
            ("GET", "/api/ui/serial-ports") => {
                let ports = (self.serial_port_lister)();
                let default_port = ports.first().cloned().unwrap_or_default();
                (
                    200,
                    success(serde_json::json!({
                        "ports": ports,
                        "default_port": default_port,
                        "baudrate": 115200
                    })),
                )
            }
            ("GET", "/api/ui/runtime-config") => (
                200,
                success(serde_json::json!({
                    "connection": {
                        "default_mode": self.settings.default_connection_mode,
                        "preferred_modes": [self.settings.default_connection_mode, if self.settings.default_connection_mode == "network" {"serial"} else {"network"}],
                        "serial": {"default_port": "", "baudrate": 115200},
                        "network": {
                            "default_host": "",
                            "transport": "telnet",
                            "http_port": 80,
                            "telnet_port": 23,
                            "timeout": 5.0
                        }
                    },
                    "upload": {"gcode_suffixes": [".gcode", ".nc"]}
                })),
            ),
            ("GET", "/api/ui/material-options") => match load_effective_material_library(
                &self.settings,
                self.read_db.as_ref().or(self.runtime_db.as_ref()),
                &self.materials,
            ) {
                Ok(library) => {
                    let materials = library
                        .materials
                        .iter()
                        .map(|(name, entry)| {
                            let mut thicknesses = entry
                                .thicknesses
                                .keys()
                                .filter_map(|value| value.parse::<f64>().ok())
                                .collect::<Vec<_>>();
                            thicknesses.sort_by(f64::total_cmp);
                            serde_json::json!({"name": name, "thicknesses": thicknesses})
                        })
                        .collect::<Vec<_>>();
                    (200, success(serde_json::json!({"materials": materials})))
                }
                Err(error) => (
                    500,
                    failure(
                        "读取材料库失败",
                        Some(serde_json::json!({ "error": error })),
                    ),
                ),
            },
            ("GET", "/api/material-lab/export") => match load_effective_material_library(
                &self.settings,
                self.read_db.as_ref().or(self.runtime_db.as_ref()),
                &self.materials,
            ) {
                Ok(library) => (
                    200,
                    success(serde_json::json!({
                        "params_file": self.settings.material_params_file.as_deref().unwrap_or(""),
                        "library": material_library_export_value(&library),
                    })),
                ),
                Err(error) => (
                    500,
                    failure(
                        "读取材料库失败",
                        Some(serde_json::json!({ "error": error })),
                    ),
                ),
            },
            ("GET" | "POST", "/api/material-lab/recent") => {
                match self.find_recent_material_workflow() {
                    Some(recent) => (200, success(recent)),
                    None => (
                        400,
                        failure("最近的激光 workflow 中没有可保存的完整材料参数", None),
                    ),
                }
            }
            ("GET", "/v1/material-lab/bootstrap") => {
                let source_path = self
                    .settings
                    .material_params_file
                    .as_deref()
                    .and_then(|value| std::path::Path::new(value).file_name())
                    .and_then(|value| value.to_str())
                    .unwrap_or("");
                match load_effective_material_library(
                    &self.settings,
                    self.read_db.as_ref().or(self.runtime_db.as_ref()),
                    &self.materials,
                ) {
                    Ok(materials) => (
                        200,
                        success(serde_json::json!({
                            "version": 1,
                            "materials": materials,
                            "error": serde_json::Value::Null,
                            "prototype": false,
                            "defaults": {"network_host": "", "network_telnet_port": 23},
                            "source_path": source_path,
                            "backup_path": ""
                        })),
                    ),
                    Err(error) => (
                        500,
                        failure(
                            "读取材料库失败",
                            Some(serde_json::json!({ "error": error })),
                        ),
                    ),
                }
            }
            ("POST", _) => match parse_json_object(body, 64 * 1024) {
                Ok(_) => (404, failure("未找到路径", None)),
                Err(error) => (400, failure(error, None)),
            },
            _ => (404, failure("未找到路径", None)),
        };
        ApiResponse {
            status: response.0,
            headers: vec![
                ("Cache-Control", "no-store".into()),
                ("Content-Type", "application/json; charset=utf-8".into()),
            ],
            body: response.1,
        }
    }
}
