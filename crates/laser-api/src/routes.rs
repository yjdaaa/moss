use super::*;

impl ApiService {
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
        let content_length = lines
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())
                    .flatten()
            })
            .unwrap_or(0usize);
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
        if !loopback && !public_route {
            let Some(auth) = self.auth.as_ref() else {
                return http_json(401, failure("需要 Bearer 鉴权", None));
            };
            let Some(token) = authorization.as_deref() else {
                return http_json(401, failure("需要 Bearer 鉴权", None));
            };
            match auth.authorize(token) {
                Ok(id) => pairing_id = Some(id),
                Err(_) => return http_json(401, failure("Bearer 鉴权失败", None)),
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
        if !self.voice_caller_allowed(&ctx) {
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
            && is_reference_data_write(method, route, &body[..content_length])
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
            return self.handle_approval_route(method, route, query, &body[..content_length], &ctx);
        }
        if method == "POST"
            && matches!(
                route,
                "/api/text-task/generate" | "/api/draw/preview" | "/api/workflow"
            )
            && serde_json::from_slice::<serde_json::Value>(&body[..content_length])
                .ok()
                .is_some_and(|value| value.get("feedback_params").is_some())
        {
            return http_json(
                400,
                failure("feedback_params 为内部字段，请通过已保存反馈确认采用", None),
            );
        }
        if method == "GET" && route == "/api/system/lan-info" {
            // 只给电脑本机看：偏好设置“手机连接”分区显示本机局域网地址与端口
            if !loopback {
                return http_json(403, failure("仅允许在电脑本机查看", None));
            }
            let saved_map = read_runtime_settings(self.runtime_path.as_deref());
            let saved = saved_map
                .get("lan_access")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let static_pin_set = self
                .auth
                .as_ref()
                .map(|a| a.has_static_pin())
                .unwrap_or_else(|| {
                    saved_map
                        .get("lan_static_pin_hash")
                        .and_then(|v| v.as_str())
                        .is_some()
                });
            let paired_devices_count = self
                .auth
                .as_ref()
                .map(|a| a.paired_count())
                .unwrap_or_default();
            let listening_all =
                matches!(self.settings.web_host.as_str(), "0.0.0.0" | "::" | "[::]");
            let port = self.get_actual_port();
            let addresses: Vec<String> = primary_lan_ipv4()
                .into_iter()
                .map(|ip| ip.to_string())
                .collect();
            return http_json(
                200,
                success(serde_json::json!({
                    "lan_access_saved": saved,
                    "lan_access_active": listening_all,
                    "web_host": self.settings.web_host,
                    "port": port,
                    "addresses": addresses,
                    "restart_required": saved != listening_all,
                    "static_pin_set": static_pin_set,
                    "paired_devices_count": paired_devices_count,
                })),
            );
        }
        if method == "POST" && route == "/api/system/check-update" {
            if !loopback {
                return http_json(403, failure("仅允许在电脑本机检查更新", None));
            }
            if let Some(denied) = self.require_operator_settings_access(&ctx) {
                return denied;
            }
            let object = if content_length > 0 {
                match parse_json_object(&body[..content_length], 16 * 1024) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                }
            } else {
                serde_json::Map::new()
            };
            let saved_map = read_runtime_settings(self.runtime_path.as_deref());
            let include_prerelease = object
                .get("include_prerelease")
                .and_then(|v| v.as_bool())
                .or_else(|| saved_map.get("check_prerelease").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            return match check_github_release_update(
                current_moss_version(),
                include_prerelease,
                DEFAULT_GITHUB_RELEASES_API,
            ) {
                Ok(report) => http_json(200, success(report)),
                Err(error) => http_json(502, failure(error, None)),
            };
        }
        if method == "GET" && route == "/api/jobs/stop-targets" {
            // 顶栏急停不依赖 React 页面的生命周期，从后端读取精确任务和原连接目标。
            let Ok(running) = self.active_jobs.lock() else {
                return http_json(500, failure("设备任务表不可用", None));
            };
            let Ok(uncertain) = self.uncertain_jobs.lock() else {
                return http_json(500, failure("待确认停止任务表不可用", None));
            };
            let mut targets = std::collections::BTreeMap::<String, serde_json::Value>::new();
            for (id, job) in uncertain.iter() {
                targets.insert(
                    id.clone(),
                    serde_json::json!({"job_id": id, "target": job.target, "state": "unknown"}),
                );
            }
            for (id, job) in running.iter() {
                // 进度浮层（2026-11-02）：顶栏急停只用 job_id / state，这里额外带上只读进度字段
                // （workflow_id、phase、lines_sent、total_lines、elapsed_ms、mode），供全局浮层显示；
                // 只读查询，不改变门禁、不触发任何设备写入。
                let mut live = job.live_send_result();
                if let Some(map) = live.as_object_mut() {
                    map.insert("job_id".into(), serde_json::json!(id));
                    map.insert("workflow_id".into(), serde_json::json!(job.workflow_id));
                    map.insert("state".into(), serde_json::json!("running"));
                }
                targets.insert(id.clone(), live);
            }
            return http_json(
                200,
                success(serde_json::json!({"jobs": targets.into_values().collect::<Vec<_>>() })),
            );
        }
        if method == "POST" && route == "/api/jobs/acknowledge-stop" {
            if matches!(
                ctx,
                laser_contracts::CallerContext::Voice { .. }
                    | laser_contracts::CallerContext::McpBridge
                    | laser_contracts::CallerContext::Approval { .. }
                    | laser_contracts::CallerContext::Gateway { .. }
            ) || (self.desktop_session.get().is_some()
                && matches!(ctx, laser_contracts::CallerContext::Loopback))
            {
                return http_json(
                    403,
                    failure(
                        "必须在界面由操作者现场核实后确认停机",
                        Some(serde_json::json!({ "error_code": "operator_confirmation_required" })),
                    ),
                );
            }
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let job_id = object
                .get("job_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let confirmed = parse_bool(object.get("confirmed"), false);
            let (Some(id), true) = (job_id, confirmed) else {
                return http_json(
                    400,
                    failure(
                        "请同时提供非空 job_id 与 confirmed=true 确认已现场核实停机",
                        None,
                    ),
                );
            };
            let dropped_job = {
                let Ok(mut uncertain) = self.uncertain_jobs.lock() else {
                    return http_json(500, failure("待确认停止任务表不可用", None));
                };
                uncertain.remove(id)
            };
            let removed = dropped_job.is_some();
            drop(dropped_job);
            return http_json(200, success(serde_json::json!({ "acknowledged": removed })));
        }
        if method == "GET" && route == "/api/system/info" {
            let actual = self.get_actual_port();
            let saved_settings = read_runtime_settings(self.runtime_path.as_deref());
            let preferred_from_file = saved_settings
                .get("web_port")
                .and_then(|p| p.as_u64())
                .map(|p| p as u16);
            let preferred = preferred_from_file.unwrap_or(self.settings.web_port);
            // 2026-11-07 用户决定：启动自动检查更新默认开启，其他用户安装后打开即自动检查并可在软件内更新；
            // 用户在偏好设置中取消勾选后写入 settings.json 的 auto_check_update=false。
            let auto_check_update = saved_settings
                .get("auto_check_update")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let check_prerelease = saved_settings
                .get("check_prerelease")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let mcp_endpoint = configured_mcp_endpoint(self.runtime_path.as_deref());
            let telemetry = laser_contracts::global_mcp_telemetry().snapshot();
            // 严格模式下接入点不列出永不开放的 R4 工具（与 laser_mcp::tools_list_for 一致）
            // 用户关闭的工具同样不列出（NV8）
            let tools_count = laser_tools::ToolCatalog::from_environment()
                .specs()
                .iter()
                .filter(|spec| {
                    !(self.strict_mode() && laser_contracts::tool_never_exposed(spec.name))
                })
                .filter(|spec| !self.tool_disabled(spec.name))
                // n72：精简清单（默认）只列 8 个激光工具与电脑控制类工具
                .filter(|spec| self.full_tool_list() || laser_mcp_compact_tool(spec.name))
                .count();
            return http_json(
                200,
                success(serde_json::json!({
                    "version": current_moss_version(),
                    "auto_check_update": auto_check_update,
                    "check_prerelease": check_prerelease,
                    "actual_port": actual,
                    "preferred_port": preferred,
                    "is_fallback": actual != preferred,
                    "web_host": self.settings.web_host,
                    "mcp": mcp_status_json(mcp_endpoint.as_deref(), &telemetry, tools_count),
                    // 链路模式来自 settings.json 的 voice.mode（语音中心写入）；设备会话数由桌面端语音中心
                    // 轮询自建接入点 health 后提供，官方链路或未知时为 null（技术书 §9.4.5）
                    "voice": voice_status_json(self.runtime_path.as_deref())
                })),
            );
        }
        if method == "GET" && route == "/api/jev/status" {
            let settings = read_runtime_settings(self.runtime_path.as_deref());
            let api_key = match self.runtime_db.as_ref() {
                Some(db) => match db
                    .lock()
                    .map_err(|_| "运行时数据库锁失败".to_owned())
                    .and_then(|db| {
                        db.get_secret(JEV_API_KEY_SECRET)
                            .map_err(|error| format!("{error:?}"))
                    }) {
                    Ok(value) => value,
                    Err(error) => return http_json(500, failure(error, None)),
                },
                None => None,
            };
            let show_masked = self.require_operator_settings_access(&ctx).is_none();
            return http_json(
                200,
                success(serde_json::json!({
                    "enabled": settings.get("jev_enabled").and_then(|value| value.as_bool()).unwrap_or(false),
                    "endpoint": DEFAULT_JEV_ENDPOINT,
                    "model": JEV_MODEL,
                    "provider": "OpenRouter",
                    "configured": api_key.as_deref().is_some_and(|value| !value.trim().is_empty()),
                    "masked_key": if show_masked {
                        api_key.as_deref().map(mask_secret).unwrap_or_default()
                    } else {
                        String::new()
                    },
                })),
            );
        }
        if method == "POST" && route == "/api/jev/test" {
            if let Some(denied) = self.require_operator_settings_access(&ctx) {
                return denied;
            }
            let Some(db) = self.runtime_db.as_ref() else {
                return http_json(503, failure("运行时数据库未启用", None));
            };
            let api_key = match db
                .lock()
                .map_err(|_| "运行时数据库锁失败".to_owned())
                .and_then(|db| {
                    db.get_secret(JEV_API_KEY_SECRET)
                        .map_err(|error| format!("{error:?}"))
                }) {
                Ok(Some(value)) if !value.trim().is_empty() => value,
                Ok(_) => return http_json(404, failure("请先保存 Jev API Key", None)),
                Err(error) => return http_json(500, failure(error, None)),
            };
            return match feedback::call_jev(
                DEFAULT_JEV_ENDPOINT,
                &api_key,
                &serde_json::json!({
                    "feedback_text": "效果正常，无需调整"
                }),
            ) {
                Ok(_) => http_json(200, success(serde_json::json!({"message": "Jev 连接正常"}))),
                Err(error) => http_json(502, failure(format!("Jev 连接失败: {error}"), None)),
            };
        }
        if (method == "POST"
            && matches!(
                route,
                "/api/feedback" | "/api/jev/feedback" | "/api/feedback/accept"
            ))
            || (method == "GET" && route == "/api/feedback/history")
        {
            return self.handle_feedback_route(route, path, &body[..content_length], ctx.label());
        }
        if method == "POST" && route == "/api/jev/reveal" {
            if let Some(denied) = self.require_operator_settings_access(&ctx) {
                return denied;
            }
            let Some(db) = self.runtime_db.as_ref() else {
                return http_json(503, failure("运行时数据库未启用", None));
            };
            let api_key = match db
                .lock()
                .map_err(|_| "运行时数据库锁失败".to_owned())
                .and_then(|db| {
                    db.get_secret(JEV_API_KEY_SECRET)
                        .map_err(|error| format!("{error:?}"))
                }) {
                Ok(Some(value)) if !value.trim().is_empty() => value,
                Ok(_) => return http_json(404, failure("Jev API Key 尚未配置", None)),
                Err(error) => return http_json(500, failure(error, None)),
            };
            return http_json(200, success(serde_json::json!({ "api_key": api_key })));
        }
        if method == "POST" && route == "/api/settings" {
            if let Some(denied) = self.require_operator_settings_access(&ctx) {
                return denied;
            }
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            if let Some(endpoint) = object.get("jev_endpoint")
                && !endpoint
                    .as_str()
                    .is_some_and(|s| s.trim().is_empty() || s.trim() == DEFAULT_JEV_ENDPOINT)
            {
                return http_json(
                    400,
                    failure("Jev 仅支持固定 OpenRouter Decisions 地址", None),
                );
            }
            if object.get("jev_enabled").is_some_and(|v| !v.is_boolean())
                || object.get("jev_api_key").is_some_and(|v| !v.is_string())
            {
                return http_json(
                    400,
                    failure("jev_enabled 必须是布尔值，jev_api_key 必须是字符串", None),
                );
            }
            let mcp_endpoint = match object.get("mcp_endpoint") {
                None => None,
                Some(serde_json::Value::String(value)) if value.trim().is_empty() => Some(None),
                Some(serde_json::Value::String(value)) => {
                    let Some(normalized) = normalize_mcp_endpoint(value) else {
                        return http_json(
                            400,
                            failure(
                                "小智 MCP 端点必须是 ws:// 或 wss:// 地址，且不能包含模板占位符",
                                None,
                            ),
                        );
                    };
                    Some(Some(normalized))
                }
                Some(_) => return http_json(400, failure("mcp_endpoint 必须是字符串", None)),
            };
            // 先校验全部字段，再一次写入 settings.json；写不进去就报错，不能显示“已保存”
            let mut file_changes: Vec<(&str, Option<serde_json::Value>)> = Vec::new();
            if let Some(port) = object.get("web_port").and_then(|v| v.as_u64()) {
                if !(1024..=65535).contains(&port) {
                    return http_json(400, failure("端口号必须在 1024 ~ 65535 之间", None));
                }
                file_changes.push(("web_port", Some(serde_json::json!(port))));
            }
            if let Some(enabled) = object.get("jev_enabled").and_then(|value| value.as_bool()) {
                file_changes.push(("jev_enabled", Some(serde_json::json!(enabled))));
            }
            if let Some(value) = object.get("lan_access") {
                let Some(enabled) = value.as_bool() else {
                    return http_json(400, failure("lan_access 必须是布尔值", None));
                };
                file_changes.push(("lan_access", Some(serde_json::json!(enabled))));
            }
            if let Some(value) = object.get("auto_check_update") {
                let Some(enabled) = value.as_bool() else {
                    return http_json(400, failure("auto_check_update 必须是布尔值", None));
                };
                file_changes.push(("auto_check_update", Some(serde_json::json!(enabled))));
            }
            if let Some(value) = object.get("check_prerelease") {
                let Some(enabled) = value.as_bool() else {
                    return http_json(400, failure("check_prerelease 必须是布尔值", None));
                };
                file_changes.push(("check_prerelease", Some(serde_json::json!(enabled))));
            }
            if let Some(pin_val) = object.get("lan_static_pin") {
                let Some(pin_str) = pin_val.as_str() else {
                    return http_json(400, failure("lan_static_pin 必须是字符串", None));
                };
                let trimmed = pin_str.trim();
                if !trimmed.is_empty()
                    && (trimmed.len() != 6 || !trimmed.bytes().all(|b| b.is_ascii_digit()))
                {
                    return http_json(400, failure("自定义配对码必须是 6 位数字", None));
                }
                if let Some(auth) = self.auth.as_ref() {
                    if let Err(err) = auth.set_static_pin((!trimmed.is_empty()).then_some(trimmed))
                    {
                        return http_json(500, failure(err, None));
                    }
                } else {
                    let hash = (!trimmed.is_empty()).then(|| {
                        serde_json::Value::String(hex::encode(Sha256::digest(trimmed.as_bytes())))
                    });
                    file_changes.push(("lan_static_pin_hash", hash));
                }
            }
            if object
                .get("lan_revoke_paired")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                if let Some(auth) = self.auth.as_ref() {
                    if let Err(err) = auth.revoke_all_tokens() {
                        return http_json(500, failure(err, None));
                    }
                } else {
                    file_changes.push(("lan_paired_tokens", None));
                }
            }
            if let Some(endpoint) = object.get("jev_endpoint") {
                let Some(endpoint) = endpoint.as_str() else {
                    return http_json(400, failure("jev_endpoint 必须是字符串", None));
                };
                let endpoint = endpoint.trim();
                if !endpoint.is_empty() && endpoint != DEFAULT_JEV_ENDPOINT {
                    return http_json(
                        400,
                        failure("Jev 仅支持固定 OpenRouter Decisions 地址", None),
                    );
                }
                file_changes.push((
                    "jev_endpoint",
                    (!endpoint.is_empty()).then(|| serde_json::json!(endpoint)),
                ));
            }
            let has_mcp_setting = mcp_endpoint.is_some();
            if let Some(endpoint) = mcp_endpoint {
                file_changes.push(("mcp_endpoint", endpoint.map(serde_json::Value::String)));
            }
            if !file_changes.is_empty() {
                let written = update_runtime_settings(self.runtime_path.as_deref(), |current| {
                    for (key, value) in file_changes {
                        match value {
                            Some(value) => current.insert(key.to_owned(), value),
                            None => current.remove(key),
                        };
                    }
                });
                if let Err(error) = written {
                    return http_json(
                        500,
                        failure(
                            format!("设置没有保存：{error}"),
                            Some(serde_json::json!({"error_code": "settings_write_failed"})),
                        ),
                    );
                }
            }
            if let Some(api_key) = object.get("jev_api_key") {
                let Some(api_key) = api_key.as_str() else {
                    return http_json(400, failure("jev_api_key 必须是字符串", None));
                };
                if !api_key.trim().is_empty() {
                    let Some(db) = self.runtime_db.as_ref() else {
                        return http_json(
                            503,
                            failure("运行时数据库未启用，无法保存 Jev API Key", None),
                        );
                    };
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|duration| duration.as_secs_f64())
                        .unwrap_or(0.0);
                    if let Err(error) = db
                        .lock()
                        .map_err(|_| "运行时数据库锁失败".to_owned())
                        .and_then(|db| {
                            db.set_secret(JEV_API_KEY_SECRET, api_key.trim(), now)
                                .map_err(|error| format!("{error:?}"))
                        })
                    {
                        return http_json(500, failure(error, None));
                    }
                }
            }
            if object
                .get("jev_delete_api_key")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
                && let Some(db) = self.runtime_db.as_ref()
                && let Err(error) = db
                    .lock()
                    .map_err(|_| "运行时数据库锁失败".to_owned())
                    .and_then(|db| {
                        db.delete_secret(JEV_API_KEY_SECRET)
                            .map_err(|error| format!("{error:?}"))
                    })
            {
                return http_json(500, failure(error, None));
            }
            return http_json(
                200,
                success(serde_json::json!({
                    "message": if has_mcp_setting {
                        "设置已保存，小智 MCP 端点将在下次启动时生效"
                    } else {
                        "设置已保存，将在下次启动时生效"
                    }
                })),
            );
        }
        if method == "GET" && route == "/v1/events" {
            let workflow_id = path
                .split_once('?')
                .and_then(|(_, query)| query_param(query, "workflow_id"))
                .unwrap_or_default();
            let status = self
                .store
                .as_ref()
                .and_then(|store| {
                    store
                        .get(laser_runtime::RecordKind::Workflow, &workflow_id)
                        .ok()
                        .flatten()
                })
                .and_then(|record| {
                    record.status.or_else(|| {
                        record
                            .payload
                            .get("status")
                            .and_then(|value| value.as_str())
                            .map(str::to_owned)
                    })
                })
                .unwrap_or_else(|| "connected".to_owned());
            return http_sse(format!(
                "event: ready\ndata: {}\n\n",
                serde_json::json!({"workflow_id": workflow_id, "status": status})
            ));
        }
        if method == "GET" {
            let asset = if route == "/" || is_spa_route(route) {
                Some("index.html".to_owned())
            } else if let Some(path) = route.strip_prefix("/assets/") {
                safe_asset_path(path).map(|value| format!("assets/{value}"))
            } else if let Some(path) = route.strip_prefix("/vendor/") {
                safe_asset_path(path).map(|value| format!("vendor/{value}"))
            } else {
                None
            };
            if let Some(asset) = asset {
                return match FrontendFixture::get(&asset) {
                    Some(file) => {
                        http_bytes(200, embedded_content_type(&asset), file.data.to_vec())
                    }
                    None if is_spa_route(route) => http_bytes(
                        200,
                        "text/html; charset=utf-8",
                        FrontendFixture::get("index.html")
                            .map(|file| file.data.to_vec())
                            .unwrap_or_default(),
                    ),
                    None => http_json(404, failure("未找到路径", None)),
                };
            }
        }
        if method == "GET" && route == "/preview" {
            let query = path.split_once('?').map(|(_, value)| value).unwrap_or("");
            let requested = query_param(query, "path").map(std::path::PathBuf::from);
            let Some(requested) = requested else {
                return http_json(404, failure("缺少 path", None));
            };
            let resolved = match resolve_preview_file(&requested, &self.preview_roots) {
                Ok(path) => path,
                Err(error) => return http_json(404, failure(error, None)),
            };
            let bytes = match std::fs::read(&resolved) {
                Ok(bytes) => bytes,
                Err(_) => return http_json(404, failure("文件不存在", None)),
            };
            let is_download =
                query_param(query, "download").is_some_and(|v| v == "1" || v == "true");
            let disposition = if is_download {
                let fname = requested
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or("download");
                Some(format!("attachment; filename=\"{fname}\""))
            } else {
                None
            };
            return http_bytes_with_disposition(
                200,
                content_type(&resolved),
                disposition.as_deref(),
                bytes,
            );
        }
        if method == "GET" && route == "/api/ui/material-recommendation" {
            let query = path.split_once('?').map(|(_, value)| value).unwrap_or("");
            let material = query_param(query, "material").unwrap_or_default();
            let thickness =
                query_param(query, "thickness_mm").and_then(|value| value.parse::<f64>().ok());
            let laser_mode = query_param(query, "laser_mode").unwrap_or_default();
            let engraving_mode = query_param(query, "engraving_mode");
            let Some(thickness_mm) = thickness.filter(|value| value.is_finite() && *value > 0.0)
            else {
                return http_json(400, failure("thickness_mm 必须是大于 0 的数字", None));
            };
            if material.trim().is_empty() || laser_mode.trim().is_empty() {
                return http_json(400, failure("material 和 laser_mode 不能为空", None));
            }
            let power_scale = query_param(query, "power_scale")
                .and_then(|value| value.parse::<f64>().ok())
                .unwrap_or(1000.0);
            let effective_library = match self.effective_material_library() {
                Ok(lib) => lib,
                Err(error) => {
                    return http_json(
                        500,
                        failure(
                            "读取材料库失败",
                            Some(serde_json::json!({ "error": error })),
                        ),
                    );
                }
            };
            let recommendation = laser_core::material::recommend_in_library(
                &effective_library,
                &material,
                thickness_mm,
                &laser_mode,
                engraving_mode.as_deref(),
                power_scale,
            );
            let Some(recommendation) = recommendation else {
                return http_json(400, failure("读取材料推荐失败", None));
            };
            let match_name = if recommendation.surface {
                "surface"
            } else if (recommendation.requested_thickness_mm - recommendation.matched_thickness_mm)
                .abs()
                < 1e-9
            {
                "exact"
            } else {
                "nearest_thickness"
            };
            let mut result = serde_json::json!({
                "material": recommendation.material,
                "thickness_mm": recommendation.requested_thickness_mm,
                "matched_thickness_mm": recommendation.matched_thickness_mm,
                "match": match_name,
                "laser_mode": recommendation.laser_mode,
                "laser_max_power": recommendation.params.laser_max_power,
                "feed_rate": recommendation.params.feed_rate,
                "passes": recommendation.params.passes,
            });
            if let Some(title) = recommendation.params.title() {
                result["title"] = serde_json::Value::String(title.to_owned());
            }
            // r40：随条目保存的填充 / 切割设置，工作台与画板据此预填设置面板（没有就不带这两个键）
            for key in [
                laser_contracts::MATERIAL_FILL_OPTIONS_KEY,
                laser_contracts::MATERIAL_CUT_OPTIONS_KEY,
            ] {
                if let Some(options) = recommendation.params.layer_options(key) {
                    result[key] = serde_json::Value::Object(options.clone());
                }
            }
            if let Some(mode) = recommendation.engraving_mode {
                result["engraving_mode"] = serde_json::Value::String(mode);
            }
            return http_json(200, success(result));
        }
        if method == "POST"
            && matches!(
                route,
                "/api/workflow" | "/api/text-task/generate" | "/api/text-task/send"
            )
        {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let mut object = object;
            strip_identity_fields(&mut object);
            // 新预览记下发起方（服务端按调用身份填写，客户端自带的值一律丢弃）；
            // 桌面端据此把 AI（语音 / 小智 / 网关）生成的任务弹出预览窗（2026-11-06）
            object.remove("_origin");
            object.remove("_origin_mpos");
            object.remove("_artifacts");
            let creates_preview = route == "/api/text-task/generate"
                || (route == "/api/workflow"
                    && matches!(
                        object.get("action").and_then(|v| v.as_str()),
                        Some("preview" | "plan" | "create")
                    ));
            if creates_preview {
                object.insert(
                    "_origin".into(),
                    serde_json::Value::String(ctx.label().to_owned()),
                );
            }
            if route == "/api/text-task/generate" {
                let effective_library = match self.effective_material_library() {
                    Ok(lib) => lib,
                    Err(error) => {
                        return http_json(
                            500,
                            failure(
                                "读取材料库失败",
                                Some(serde_json::json!({ "error": error })),
                            ),
                        );
                    }
                };
                match generate_text_artifacts(
                    &object,
                    self.generated_root
                        .as_deref()
                        .or(self.upload_root.as_deref()),
                    &effective_library,
                    self.engrave_route(),
                    self.text_origin(),
                ) {
                    Ok(mut artifacts) => {
                        self.apply_time_calibration(artifacts.get_mut("time_estimate"), &object);
                        object.insert("_artifacts".into(), serde_json::Value::Object(artifacts));
                    }
                    Err(error) => return http_json(400, failure(error, None)),
                }
            } else if route == "/api/workflow" {
                let action = object.get("action").and_then(|v| v.as_str()).unwrap_or("");
                if action == "preview" || action == "create" || action == "plan" {
                    let source_type = object
                        .get("source_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if source_type == "image" || source_type == "gcode" {
                        let effective_library = match self.effective_material_library() {
                            Ok(lib) => lib,
                            Err(error) => {
                                return http_json(
                                    500,
                                    failure(
                                        "读取材料库失败",
                                        Some(serde_json::json!({ "error": error })),
                                    ),
                                );
                            }
                        };
                        match generate_image_or_gcode_artifacts(
                            &object,
                            self.upload_root.as_deref(),
                            self.generated_root.as_deref(),
                            &self.preview_roots,
                            &effective_library,
                        ) {
                            Ok(Some(mut artifacts)) => {
                                self.apply_time_calibration(
                                    artifacts.get_mut("time_estimate"),
                                    &object,
                                );
                                object.insert(
                                    "_artifacts".into(),
                                    serde_json::Value::Object(artifacts),
                                );
                            }
                            Ok(None) => {}
                            Err(error) => return http_json(400, failure(error, None)),
                        }
                    }
                }
            }
            let send_roots = self.send_gcode_roots();
            let launcher =
                |workflow_id: &str,
                 gcode_path: std::path::PathBuf,
                 expected_sha: &str,
                 request: &serde_json::Map<String, serde_json::Value>| {
                    self.launch_send_job(workflow_id, gcode_path, expected_sha, request, &ctx)
                };
            return self.with_effective_store(|store| {
                handle_workflow_store_request(
                    store,
                    route,
                    &object,
                    &send_roots,
                    &self.active_jobs,
                    &self.launching_workflows,
                    &launcher,
                )
            });
        }
        // 低功率点射（2026-11-16）：只给界面上的人用，语音 / MCP / 网关 / 审批重放一律拒绝
        if method == "POST" && route == "/api/device/pointer-fire" {
            let mut object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            strip_identity_fields(&mut object);
            return self.run_pointer_fire(&object, &ctx);
        }
        // 两点对位（2026-11-16）：读位置只发状态查询；生成对位预览是新的 preview_ready workflow，发送照常确认
        if method == "POST"
            && matches!(
                route,
                "/api/registration/read-position" | "/api/registration/preview"
            )
        {
            return self.handle_registration_route(route, &body[..content_length], &ctx);
        }
        if method == "POST" && route == "/api/safe-action" {
            let mut object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            strip_identity_fields(&mut object);
            let mut args: laser_contracts::LaserSafeActionArgs =
                match serde_json::from_value(serde_json::Value::Object(object.clone())) {
                    Ok(a) => a,
                    Err(e) => return http_json(400, failure(format!("参数解析失败: {e}"), None)),
                };
            // 严格模式检查点（技术书 §6.2）：只对 R3 动作；急停、软复位、关激光、降低倍率永不追加条件（A-27）
            let mut authority = approval::R3Authority::Unchecked;
            if laser_contracts::safe_action_requires_approval(&args.action) {
                let action = args.action.trim().to_owned();
                let facts = approval::safe_action_digest_facts(&object);
                let summary = safe_action_approval_summary(&action, &facts);
                authority = match self.require_r3_authority(
                    &ctx,
                    approval::R3Action {
                        kind: laser_contracts::ApprovalKind::SafeAction,
                        target_record: &action,
                        facts,
                        request: &object,
                        summary,
                    },
                ) {
                    Ok(authority) => authority,
                    Err((status, body)) => return http_json(status, body),
                };
                if let approval::R3Authority::Approval(record) = &authority
                    && let Some(connection) = record.connection.as_ref()
                {
                    apply_connection_to_safe_action(&mut args, connection);
                }
            }
            let job_id = object.get("job_id").and_then(serde_json::Value::as_str);
            let res = self.handle_safe_action_for(&args, &ctx, job_id);
            let action_name = args.action.trim().to_owned();
            if action_name == "emergency_stop" {
                self.emit_event(if res.success {
                    LaserEvent {
                        level: LaserEventLevel::Warn,
                        text: format!("已发出急停（0x18）：{}", res.result),
                        hint: Some(
                            "0x18 只是软件复位请求：到设备旁确认机器已停、激光已灭".to_owned(),
                        ),
                        detail: None,
                    }
                } else {
                    LaserEvent {
                        level: LaserEventLevel::Error,
                        text: format!("急停没有发出：{}", res.result),
                        hint: Some("立刻按设备上的物理急停或断电".to_owned()),
                        detail: res.detail.clone(),
                    }
                });
            } else if !res.success && action_name != "status" {
                self.emit_event(LaserEvent {
                    level: LaserEventLevel::Warn,
                    text: format!("设备操作 {action_name} 没有完成：{}", res.result),
                    hint: laser_failure_hint(&res.result).map(str::to_owned),
                    detail: res.detail.clone(),
                });
            }
            let outcome: Result<serde_json::Value, (u16, serde_json::Value)> = if res.success {
                Ok(serde_json::Value::Null)
            } else {
                Err((400, failure(res.result.clone(), None)))
            };
            self.finish_r3_authority(&authority, &outcome, None);
            return http_json(
                if res.success { 200 } else { 400 },
                serde_json::json!({
                    "success": res.success,
                    "result": res.result,
                    "detail": res.detail,
                }),
            );
        }
        if method == "POST" && route == "/api/connection/check" {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let (result, verified) = self.probe_connections(&object);
            let summary = result["summary"].as_str().unwrap_or("").to_owned();
            if result["connected"].as_bool() == Some(true) {
                // 握手成功但有配置提醒（off_on_alarm、verbose_errors、$10 等）时记为警告
                let warned = summary.contains("建议");
                self.emit_event(LaserEvent {
                    level: if warned {
                        LaserEventLevel::Warn
                    } else {
                        LaserEventLevel::Info
                    },
                    text: format!("握手探测成功：{summary}"),
                    hint: warned.then(|| "按摘要里的建议改好控制器配置后重启控制器".to_owned()),
                    detail: None,
                });
            } else {
                self.emit_event(LaserEvent {
                    level: LaserEventLevel::Warn,
                    text: format!("连接雕刻机失败：{summary}"),
                    hint: Some(
                        laser_failure_hint(&summary)
                            .unwrap_or(
                                "确认雕刻机已开机，IP / 端口或串口与波特率填写正确后再点“握手探测”",
                            )
                            .to_owned(),
                    ),
                    detail: Some(result["attempts"].to_string()),
                });
            }
            // n73：工作台（桌面、本机网页、局域网配对）握手成功的连接记下来，语音与接入点默认用它
            if let Some(spec) = verified
                && matches!(
                    ctx,
                    laser_contracts::CallerContext::Desktop
                        | laser_contracts::CallerContext::Loopback
                        | laser_contracts::CallerContext::Lan { .. }
                )
            {
                self.remember_verified_connection(&spec);
            }
            return http_json(200, success(result));
        }
        if method == "POST" && route == "/api/device/framing" {
            let mut object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            strip_identity_fields(&mut object);
            // 先校验整数参数，审批摘要与实际下发使用同一份已校验的值（2026-11-05）。
            if let Err(error) = framing_feed_and_power(&object) {
                return framing_params_invalid(error);
            }
            // 橡皮筋轮廓同样先校验，非法时不创建审批（与 run_framing 用同一套默认范围）
            let number = |key: &str, default: f64| {
                object.get(key).and_then(|v| v.as_f64()).unwrap_or(default)
            };
            let bounds = (
                number("min_x", 0.0),
                number("min_y", 0.0),
                number("max_x", 100.0),
                number("max_y", 100.0),
            );
            if let Err(error) = crate::safe_action::framing_outline(&object, bounds) {
                return framing_params_invalid(error);
            }
            // 严格模式检查点（技术书 §6.2）：走边框真实驱动机器
            let framing_facts = approval::framing_digest_facts(&object);
            let framing_summary = framing_approval_summary(&framing_facts);
            let framing_authority = match self.require_r3_authority(
                &ctx,
                approval::R3Action {
                    kind: laser_contracts::ApprovalKind::Framing,
                    target_record: "framing",
                    facts: framing_facts,
                    request: &object,
                    summary: framing_summary,
                },
            ) {
                Ok(authority) => authority,
                Err((status, body)) => return http_json(status, body),
            };
            if let approval::R3Authority::Approval(record) = &framing_authority {
                match record.connection.as_ref() {
                    Some(connection) => {
                        object = approval::object_with_connection(&object, connection)
                    }
                    None => {
                        let (status, body) =
                            approval::error_response(&approval::ApprovalError::Mismatch);
                        return http_json(status, body);
                    }
                }
            }
            let response = self.run_framing(&object, &ctx);
            let outcome: Result<serde_json::Value, (u16, serde_json::Value)> =
                if response.starts_with(b"HTTP/1.1 200") {
                    Ok(serde_json::Value::Null)
                } else {
                    Err((400, failure("走边框未完成", None)))
                };
            if !response.starts_with(b"HTTP/1.1 200") {
                let body = http_response_json(&response);
                let message = body["result"].as_str().unwrap_or("走边框未完成").to_owned();
                self.emit_event(LaserEvent {
                    level: LaserEventLevel::Error,
                    text: format!("走边框没有完成：{message}"),
                    hint: Some(
                        laser_failure_hint(&message)
                            .unwrap_or("到设备旁确认机器状态，排除原因后再走边框")
                            .to_owned(),
                    ),
                    detail: Some(body.to_string()),
                });
            }
            self.finish_r3_authority(&framing_authority, &outcome, None);
            return response;
        }
        if method == "POST" && route == "/api/draw/preview" {
            let object = match parse_json_object(&body[..content_length], 10 * 1024 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let effective_library = match self.effective_material_library() {
                Ok(lib) => lib,
                Err(error) => {
                    return http_json(
                        500,
                        failure(
                            "读取材料库失败",
                            Some(serde_json::json!({ "error": error })),
                        ),
                    );
                }
            };
            let mut artifacts = match generate_draw_artifacts(
                &object,
                self.generated_root
                    .as_deref()
                    .or(self.upload_root.as_deref()),
                &effective_library,
                self.engrave_route(),
            ) {
                Ok(a) => a,
                Err(error) => return http_json(400, failure(error, None)),
            };
            self.apply_time_calibration(artifacts.get_mut("time_estimate"), &object);
            let workflow_id = format!("draw-{}", next_workflow_id());
            let material = artifacts
                .get("material")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let thickness_mm = artifacts
                .get("thickness_mm")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let img_name = artifacts
                .get("image_file")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let gcode_name = artifacts
                .get("gcode_file")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let scene_name = artifacts
                .get("scene_file")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let path_preview_name = artifacts
                .get("path_preview_file")
                .and_then(|v| v.as_str())
                .unwrap_or(img_name);
            let fingerprint = artifacts
                .get("fingerprint")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let gcode_sha = artifacts
                .get("gcode_sha256")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let est_secs = artifacts
                .get("time_estimate")
                .and_then(|t| t.get("estimated_seconds"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);

            let mut input = object.clone();
            input.remove("image_data_url");
            let mut record = serde_json::json!({
                "workflow_id": workflow_id,
                "status": "preview_ready",
                "source_type": "draw",
                "input": input,
                "material": material,
                "thickness_mm": thickness_mm,
                "artifacts": artifacts,
                "confirmation_snapshot": {
                    "gcode_file": gcode_name,
                    "gcode_sha256": gcode_sha,
                    "material": material,
                    "thickness_mm": thickness_mm,
                },
                "draw_lab": {
                    "job_summary": format!("画板任务就绪 · 预估 {:.1} 秒", est_secs),
                    "content_fingerprint": fingerprint,
                    "uploaded_image_file": img_name,
                    "scene_file": scene_name,
                },
                "web": {
                    "material_preview_url": format!("/preview?path={img_name}"),
                    "uploaded_image_url": format!("/preview?path={img_name}"),
                    "gcode_preview_url": format!("/preview?path={path_preview_name}"),
                    "image_preview_url": format!("/preview?path={path_preview_name}"),
                    "gcode_download_url": format!("/preview?path={gcode_name}&download=1"),
                    "scene_download_url": if scene_name.is_empty() { "".into() } else { format!("/preview?path={scene_name}&download=1") }
                },
                "next_actions": ["status", "cancel", "confirm_send"],
                "created_at": unix_seconds(),
                "updated_at": unix_seconds()
            });
            feedback::add_draw_view(&mut record);
            let stored = laser_runtime::StoredRecord {
                kind: laser_runtime::RecordKind::Workflow,
                id: workflow_id.clone(),
                status: Some("preview_ready".into()),
                payload: record.clone(),
            };
            if self
                .with_effective_store(|store| store.put(&stored))
                .is_err()
            {
                return http_json(500, failure("画板预览保存失败", None));
            }
            return http_json(200, success(record));
        }
        if method == "POST" && route == "/api/material-lab/preview" {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            return match self.material_lab_preview(&object) {
                Ok(record) => http_json(200, success(record)),
                Err((status, body)) => http_json(status, body),
            };
        }
        if method == "POST" && route == "/api/material-lab/send" {
            let mut object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            strip_identity_fields(&mut object);
            return match self.material_lab_send(&object, &ctx) {
                Ok(record) => http_json(200, success(record)),
                Err((status, body)) => http_json(status, body),
            };
        }
        if method == "POST" && route == "/api/material-lab/select-cell" {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            return match self.material_lab_select_cell(&object) {
                Ok(record) => http_json(200, success(record)),
                Err((status, body)) => http_json(status, body),
            };
        }
        if method == "POST" && route == "/api/material-lab/status" {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let calibration_id = object
                .get("calibration_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim();
            if calibration_id.is_empty() {
                return http_json(400, failure("calibration_id 不能为空", None));
            }
            return match self.load_calibration(calibration_id) {
                Some(mut record) => {
                    overlay_live_progress(&mut record, &self.active_jobs, calibration_id);
                    http_json(200, success(record))
                }
                None => http_json(404, failure("未找到标定矩阵", None)),
            };
        }
        if method == "POST" && route == "/api/upload" {
            let object = match parse_json_object(&body[..content_length], 20 * 1024 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let Some(root) = self.upload_root.as_ref() else {
                return http_json(500, failure("上传目录未配置", None));
            };
            let filename = object
                .get("filename")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let encoded = object
                .get("content_base64")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            return match save_upload(filename, encoded, root, unix_seconds()) {
                Ok(upload) => http_json(
                    200,
                    success(serde_json::json!({
                        "file_path": upload.filename.clone(),
                        "preview_url": format!("/preview?path={}", upload.filename),
                        "filename": upload.filename,
                        "size": upload.size,
                        "suffix": upload.suffix,
                    })),
                ),
                Err(error) => {
                    let detail = if error == "不支持上传该文件类型" {
                        let basename = filename.rsplit(['\\', '/']).next().unwrap_or(filename);
                        let cleaned = basename
                            .replace(['<', '>', ':', '"', '|', '?', '*'], "_")
                            .chars()
                            .filter(|c| !c.is_control())
                            .collect::<String>();
                        Some(serde_json::json!({
                            "filename": if cleaned.is_empty() { "upload" } else { &cleaned },
                            "allowed": [
                                ".bmp", ".gcode", ".gif", ".jpeg", ".jpg", ".nc", ".png", ".webp"
                            ]
                        }))
                    } else {
                        None
                    };
                    http_json(400, failure(error, detail))
                }
            };
        }
        if method == "POST" && route == "/api/material-lab/manage" {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            return handle_material_manage(
                &self.settings,
                self.runtime_db.as_ref(),
                &self.materials,
                &self.material_write_lock,
                &object,
            );
        }
        // 文字对刀起点全局设置（r48）：工作台与语音共用（局域网只读）
        if route == "/api/text-origin" {
            if method == "GET" {
                return http_json(200, success(self.text_origin_json()));
            }
            if method == "POST" {
                let object = match parse_json_object(&body[..content_length], 1024) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                };
                let origin = match object.get("origin").and_then(|v| v.as_str()) {
                    Some(value) if !value.trim().is_empty() => {
                        laser_core::engine::TextOrigin::parse(Some(value))
                    }
                    _ => Err("origin 只能是 center / lower_left".to_owned()),
                };
                let origin = match origin {
                    Ok(origin) => origin,
                    Err(error) => return http_json(400, failure(error, None)),
                };
                return match self.set_text_origin(origin) {
                    Ok(()) => http_json(200, success(self.text_origin_json())),
                    Err(error) => {
                        http_json(500, failure(format!("保存对刀起点失败：{error}"), None))
                    }
                };
            }
        }
        // 雕刻路线全局开关（r45）：矢量 / 转图片，工作台、画板与语音共用（局域网只读）
        if route == "/api/engrave-route" {
            if method == "GET" {
                return http_json(200, success(self.engrave_route_json()));
            }
            if method == "POST" {
                let object = match parse_json_object(&body[..content_length], 1024) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                };
                let route = match object
                    .get("route")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "route 只能是 vector / image".to_owned())
                    .and_then(crate::engrave_route::EngraveRoute::parse)
                {
                    Ok(route) => route,
                    Err(error) => return http_json(400, failure(error, None)),
                };
                return match self.set_engrave_route(route) {
                    Ok(()) => http_json(200, success(self.engrave_route_json())),
                    Err(error) => {
                        http_json(500, failure(format!("保存雕刻路线失败：{error}"), None))
                    }
                };
            }
        }
        // 工时自校准（r40）：查看各台机器的校准系数，或清除（局域网只读，见 is_reference_data_write）
        if route == "/api/time-calibration" {
            if method == "GET" {
                return http_json(200, success(self.time_calibration_status()));
            }
            if method == "POST" {
                let object = match parse_json_object(&body[..content_length], 4 * 1024) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                };
                if object.get("action").and_then(|v| v.as_str()) != Some("reset") {
                    return http_json(400, failure("action 只能是 reset", None));
                }
                let target = object
                    .get("target")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty());
                return match self.reset_time_calibration(target) {
                    Ok(()) => http_json(200, success(self.time_calibration_status())),
                    Err(error) => {
                        http_json(500, failure(format!("清除工时校准失败：{error}"), None))
                    }
                };
            }
        }
        let response = self.handle(method, route, &body[..content_length]);
        http_json(response.status, response.body)
    }
}
