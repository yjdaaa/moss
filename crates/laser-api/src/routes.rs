mod device;
mod material;
mod system;
mod workflow;

use super::*;

fn parse_http_json_reply(raw: &[u8]) -> (u16, serde_json::Value) {
    let Some(separator) = raw.windows(4).position(|window| window == b"\r\n\r\n") else {
        return (500, failure("后端响应缺少头部分隔符", None));
    };
    let head = String::from_utf8_lossy(&raw[..separator]);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(500);
    let body = serde_json::from_slice(&raw[separator + 4..]).unwrap_or_else(|_| {
        serde_json::json!({
            "success": status < 400,
            "result": String::from_utf8_lossy(&raw[separator + 4..]),
        })
    });
    (status, body)
}

impl ApiService {
    /// 进程内类型化 JSON 分发入口：绕过原始 HTTP/1.1 报文拼装与头解析，直接走鉴权后路由并携带显式 `CallerContext`。
    pub fn dispatch_json(
        &self,
        method: &str,
        path: &str,
        body: &serde_json::Value,
        caller: laser_contracts::CallerContext,
    ) -> (u16, serde_json::Value) {
        self.dispatch_json_with_watch(method, path, body, caller, None)
    }

    /// 进程内类型化 JSON 分发入口（可显式传入短作业开始回执，供语音按预览走边框立即获知开始状态）。
    pub fn dispatch_json_with_watch(
        &self,
        method: &str,
        path: &str,
        body: &serde_json::Value,
        caller: laser_contracts::CallerContext,
        short_job_watch: Option<&std::sync::Arc<ShortJobWatch>>,
    ) -> (u16, serde_json::Value) {
        let payload = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(body).unwrap_or_default()
        };
        let raw = self.dispatch_authenticated(method, path, &payload, &caller, short_job_watch);
        parse_http_json_reply(&raw)
    }

    pub(crate) fn route_http(
        &self,
        request: &[u8],
        loopback: bool,
        source_ip: &str,
        ctx_override: Option<laser_contracts::CallerContext>,
    ) -> Vec<u8> {
        let Some(separator) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            return http_json(400, failure("请求格式错误", None));
        };
        let head = String::from_utf8_lossy(&request[..separator]);
        let mut lines = head.lines();
        let Some(request_line) = lines.next() else {
            return http_json(400, failure("请求格式错误", None));
        };
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("/");
        let authorization = head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().strip_prefix("Bearer ").map(str::to_string))
                .flatten()
        });
        let content_length = match parse_content_length(&head) {
            Ok(len) => len,
            Err(BodyFramingError::TransferEncodingUnsupported) => {
                return http_json(
                    501,
                    failure(
                        "不支持 Transfer-Encoding 分块传输，请使用 Content-Length",
                        Some(serde_json::json!({"error_code": "transfer_encoding_unsupported"})),
                    ),
                );
            }
            Err(BodyFramingError::InvalidContentLength) => {
                return http_json(
                    400,
                    failure(
                        "Content-Length 请求头无效或存在冲突",
                        Some(serde_json::json!({"error_code": "invalid_content_length"})),
                    ),
                );
            }
        };
        let body = &request[separator + 4..];
        if body.len() < content_length {
            return http_json(400, failure("请求体不完整", None));
        }
        let route = path.split('?').next().unwrap_or(path);
        if method == "OPTIONS" {
            return http_options(None);
        }
        if method == "POST" && route == "/v1/auth/challenge" {
            if !loopback {
                return http_json(403, failure("仅允许本地回环客户端生成配对挑战码", None));
            }
            let Some(auth) = self.auth.as_ref() else {
                return http_json(500, failure("认证服务未配置", None));
            };
            return match auth.issue_code(unix_seconds()) {
                Ok(challenge) => http_json(
                    200,
                    success(serde_json::json!({
                        "pairing_code": challenge.code,
                        "expires_at": challenge.expires_at,
                        "valid_seconds": 300
                    })),
                ),
                Err(error) => http_json(500, failure(error, None)),
            };
        }
        if method == "POST" && route == "/v1/auth/pair" {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let code = object
                .get("pairing_code")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let Some(auth) = self.auth.as_ref() else {
                return http_json(404, failure("未找到路径", None));
            };
            return match auth.pair(source_ip, code, unix_seconds()) {
                Ok((token, pairing_id)) => http_json(
                    200,
                    success(serde_json::json!({
                        "token": token, "pairing_id": pairing_id
                    })),
                ),
                Err(AuthError::Locked(seconds)) => http_json_with_headers(
                    429,
                    failure("配对请求过于频繁，请稍后重试", None),
                    &[("Retry-After", seconds.to_string())],
                ),
                Err(AuthError::Invalid) => http_json(400, failure("配对码无效或已过期", None)),
                Err(AuthError::Internal) => http_json(500, failure("认证服务不可用", None)),
                Err(AuthError::Unauthorized) => http_json(400, failure("配对码无效或已过期", None)),
            };
        }
        let public_route = (method == "GET"
            && (route == "/api/health"
                || route == "/v1/capabilities"
                || route == "/api/system/info"))
            || (method == "GET"
                && (route == "/"
                    || is_spa_route(route)
                    || route.starts_with("/assets/")
                    || route.starts_with("/vendor/")));
        let mut pairing_id = None;
        if !loopback {
            if let Some(auth) = self.auth.as_ref()
                && let Some(token) = authorization.as_deref()
                && let Ok(id) = auth.authorize(token)
            {
                pairing_id = Some(id);
            } else if !public_route {
                if self.auth.is_none() || authorization.is_none() {
                    return http_json(401, failure("需要 Bearer 鉴权", None));
                }
                return http_json(401, failure("Bearer 鉴权失败", None));
            }
        }
        // 桌面会话头（技术书 §5.3）：只在回环连接上有效，值不对时 403，前端重新取会话后只重试一次
        let desktop = match self.check_desktop_session(&head, loopback) {
            Ok(desktop) => desktop,
            Err(response) => return response,
        };
        // 调用方上下文只由入口构造（技术书 §6.4）：进程内调用方显式声明优先，其次桌面会话
        let ctx = if desktop && ctx_override.is_none() {
            laser_contracts::CallerContext::Desktop
        } else {
            caller_context_for(ctx_override, loopback, pairing_id)
        };
        self.dispatch_authenticated(method, path, &body[..content_length], &ctx, None)
    }

    /// 已完成传输层鉴权与调用方识别后的统一路由分发（HTTP 与进程内调用共用同一门禁与状态机）。
    pub(crate) fn dispatch_authenticated(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
        short_job_watch: Option<&std::sync::Arc<ShortJobWatch>>,
    ) -> Vec<u8> {
        let route = path.split('?').next().unwrap_or(path);
        if !self.voice_caller_allowed(ctx) {
            return http_json(
                403,
                failure(
                    "语音设备授权已失效，请重新连接",
                    Some(serde_json::json!({"error_code":"voice_session_revoked"})),
                ),
            );
        }
        // 手机 / 局域网连接（技术书 §9.8）：可以操作雕刻机，但材料、校准参数与系统设置只读，只能在电脑上修改
        if matches!(ctx, laser_contracts::CallerContext::Lan { .. })
            && is_reference_data_write(method, route, body)
        {
            return http_json(
                403,
                failure(
                    "手机 / 局域网连接为只读：材料、校准参数和系统设置请在电脑上修改",
                    Some(serde_json::json!({"error_code": "lan_read_only"})),
                ),
            );
        }
        if route == "/v1/approvals" || route.starts_with("/v1/approvals/") {
            let query = path.split_once('?').map(|(_, query)| query).unwrap_or("");
            return self.handle_approval_route(method, route, query, body, ctx);
        }
        if method == "POST"
            && matches!(
                route,
                "/api/text-task/generate" | "/api/draw/preview" | "/api/workflow"
            )
            && serde_json::from_slice::<serde_json::Value>(body)
                .ok()
                .is_some_and(|value| value.get("feedback_params").is_some())
        {
            return http_json(
                400,
                failure("feedback_params 为内部字段，请通过已保存反馈确认采用", None),
            );
        }
        if let Some(reply) = self.route_system(method, path, route, body, ctx) {
            return reply;
        }
        if let Some(reply) = self.route_static_and_preview(method, path, route) {
            return reply;
        }
        if let Some(reply) = self.route_material(method, path, route, body, ctx) {
            return reply;
        }
        if let Some(reply) = self.route_workflow_and_draw(method, route, body, ctx, short_job_watch)
        {
            return reply;
        }
        if let Some(reply) = self.route_device(method, route, body, ctx, short_job_watch) {
            return reply;
        }
        let response = self.handle(method, route, body);
        http_json(response.status, response.body)
    }
}

#[cfg(test)]
mod dispatch_json_tests {
    use super::*;

    #[test]
    fn dispatch_json_handles_typed_requests_and_enforces_caller_guards() {
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        );
        let (status, info) = service.dispatch_json(
            "GET",
            "/api/system/info",
            &serde_json::Value::Null,
            laser_contracts::CallerContext::Loopback,
        );
        assert_eq!(status, 200);
        assert_eq!(info["success"], true);
        assert!(info["result"]["version"].as_str().is_some());

        let (lan_status, lan_body) = service.dispatch_json(
            "GET",
            "/api/system/lan-info",
            &serde_json::Value::Null,
            laser_contracts::CallerContext::Lan {
                pairing_id: "pair-1".to_owned(),
            },
        );
        assert_eq!(lan_status, 403);
        assert_eq!(lan_body["success"], false);
    }
}
