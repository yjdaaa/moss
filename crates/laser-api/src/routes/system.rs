use super::super::*;

impl ApiService {
    pub(super) fn route_system(
        &self,
        method: &str,
        path: &str,
        route: &str,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
    ) -> Option<Vec<u8>> {
        match (method, route) {
            ("GET", "/api/system/lan-info") => Some(self.handle_system_lan_info(ctx)),
            ("POST", "/api/system/check-update") => {
                Some(self.handle_system_check_update(body, ctx))
            }
            ("GET", "/api/jobs/stop-targets") => Some(self.handle_jobs_stop_targets(ctx)),
            ("POST", "/api/jobs/acknowledge-stop") => {
                Some(self.handle_jobs_acknowledge_stop(body, ctx))
            }
            ("GET", "/api/system/info") => Some(self.handle_system_info(ctx)),
            ("GET", "/api/jev/status") => Some(self.handle_jev_status(ctx)),
            ("POST", "/api/jev/test") => Some(self.handle_jev_test(ctx)),
            ("POST", "/api/feedback" | "/api/jev/feedback" | "/api/feedback/accept")
            | ("GET", "/api/feedback/history") => {
                Some(self.handle_feedback_route(route, path, body, ctx.label()))
            }
            ("POST", "/api/jev/reveal") => Some(self.handle_jev_reveal(ctx)),
            ("POST", "/api/settings") => Some(self.handle_settings_update(body, ctx)),
            ("POST", "/api/process-trace") => Some(self.handle_process_trace(body, ctx)),
            ("GET", "/v1/events") => Some(self.handle_events_sse(path)),
            ("GET", "/api/text-origin") => Some(http_json(200, success(self.text_origin_json()))),
            ("POST", "/api/text-origin") => Some(self.handle_set_text_origin(body)),
            ("GET", "/api/engrave-route") => {
                Some(http_json(200, success(self.engrave_route_json())))
            }
            ("POST", "/api/engrave-route") => Some(self.handle_set_engrave_route(body)),
            ("GET", "/api/time-calibration") => {
                Some(http_json(200, success(self.time_calibration_status())))
            }
            ("POST", "/api/time-calibration") => Some(self.handle_reset_time_calibration(body)),
            _ => None,
        }
    }

    /// 完整过程记录的状态与清空（n105 / r55）：只给桌面端，局域网与语音、接入点一律拒绝。
    fn handle_process_trace(&self, body: &[u8], ctx: &laser_contracts::CallerContext) -> Vec<u8> {
        if let Some(denied) = self.require_operator_settings_access(ctx) {
            return denied;
        }
        let object = match parse_json_object(body, 4 * 1024) {
            Ok(value) => value,
            Err(error) => return http_json(400, failure(error, None)),
        };
        let Some(trace) = self.process_trace() else {
            return http_json(503, failure("没有运行目录，无法使用完整过程记录", None));
        };
        let trace = trace.scoped();
        match object.get("action").and_then(|v| v.as_str()) {
            Some("status") => {}
            Some("clear") => {
                if let Err(error) = trace.clear() {
                    return http_json(500, failure(format!("清空过程记录失败：{error}"), None));
                }
            }
            _ => return http_json(400, failure("action 只能是 status 或 clear", None)),
        }
        let usage = trace.usage();
        let directory = trace.directory_status();
        http_json(
            200,
            success(serde_json::json!({
                "enabled": trace.enabled(),
                "dir": directory.dir,
                "custom_dir": directory.custom_dir,
                "available": directory.available,
                "error": directory.error,
                "bytes": usage.bytes,
                "days": usage.days,
                "retention_days": laser_runtime::process_trace::RETENTION_DAYS,
            })),
        )
    }

    fn handle_system_lan_info(&self, ctx: &laser_contracts::CallerContext) -> Vec<u8> {
        // 只给电脑本机看：偏好设置“手机连接”分区显示本机局域网地址与端口
        if matches!(ctx, laser_contracts::CallerContext::Lan { .. }) {
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
        let listening_all = matches!(self.settings.web_host.as_str(), "0.0.0.0" | "::" | "[::]");
        let port = self.get_actual_port();
        let addresses: Vec<String> = primary_lan_ipv4()
            .into_iter()
            .map(|ip| ip.to_string())
            .collect();
        http_json(
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
        )
    }

    fn handle_system_check_update(
        &self,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
    ) -> Vec<u8> {
        if matches!(ctx, laser_contracts::CallerContext::Lan { .. }) {
            return http_json(403, failure("仅允许在电脑本机检查更新", None));
        }
        if let Some(denied) = self.require_operator_settings_access(ctx) {
            return denied;
        }
        let object = if !body.is_empty() {
            match parse_json_object(body, 16 * 1024) {
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
        match check_github_release_update(
            current_moss_version(),
            include_prerelease,
            DEFAULT_GITHUB_RELEASES_API,
        ) {
            Ok(report) => http_json(200, success(report)),
            Err(error) => http_json(502, failure(error, None)),
        }
    }

    fn can_acknowledge_stop(&self, ctx: &laser_contracts::CallerContext) -> bool {
        !matches!(
            ctx,
            laser_contracts::CallerContext::Voice { .. }
                | laser_contracts::CallerContext::McpBridge
                | laser_contracts::CallerContext::Approval { .. }
                | laser_contracts::CallerContext::Gateway { .. }
        ) && !matches!(
            ctx,
            laser_contracts::CallerContext::Lan { pairing_id } if pairing_id.is_empty()
        ) && !(self.desktop_session.get().is_some()
            && matches!(ctx, laser_contracts::CallerContext::Loopback))
    }

    fn handle_jobs_stop_targets(&self, ctx: &laser_contracts::CallerContext) -> Vec<u8> {
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
        http_json(
            200,
            success(serde_json::json!({
                "jobs": targets.into_values().collect::<Vec<_>>(),
                "can_acknowledge_stop": self.can_acknowledge_stop(ctx),
            })),
        )
    }

    fn handle_jobs_acknowledge_stop(
        &self,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
    ) -> Vec<u8> {
        if !self.can_acknowledge_stop(ctx) {
            return http_json(
                403,
                failure(
                    "必须在界面由操作者现场核实后确认停机",
                    Some(serde_json::json!({ "error_code": "operator_confirmation_required" })),
                ),
            );
        }
        let object = match parse_json_object(body, 64 * 1024) {
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
        http_json(200, success(serde_json::json!({ "acknowledged": removed })))
    }

    fn handle_system_info(&self, ctx: &laser_contracts::CallerContext) -> Vec<u8> {
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
        // n105 / r55：完整过程记录默认关闭；目录与占用只经 /api/process-trace（仅桌面）给出
        let process_trace_enabled = saved_settings
            .get(laser_runtime::process_trace::SETTING_KEY)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mcp_endpoint = configured_mcp_endpoint(self.runtime_path.as_deref());
        let telemetry = self.mcp_telemetry.snapshot();
        let include_mcp_details = !matches!(
            ctx,
            laser_contracts::CallerContext::Lan { pairing_id } if pairing_id.is_empty()
        );
        // 严格模式下接入点不列出永不开放的 R4 工具（与 laser_mcp::tools_list_for 一致）
        // 用户关闭的工具同样不列出（NV8）
        let tools_count = laser_tools::ToolCatalog::from_environment()
            .specs()
            .iter()
            .filter(|spec| !(self.strict_mode() && laser_contracts::tool_never_exposed(spec.name)))
            .filter(|spec| !self.tool_disabled(spec.name))
            .count();
        let dry_run = self.dry_run_snapshot();
        // 未鉴权局域网访问不暴露含串口号 / 控制器 IP 的机器档案键
        let machine_profiles = if include_mcp_details {
            self.machine_profiles_json()
        } else {
            serde_json::json!({ "targets": {} })
        };
        let verified_target_key = if include_mcp_details {
            self.verified_connection_target_key()
        } else {
            None
        };
        http_json(
            200,
            success(serde_json::json!({
                "version": current_moss_version(),
                "auto_check_update": auto_check_update,
                "check_prerelease": check_prerelease,
                "process_trace_enabled": process_trace_enabled,
                "laser_dry_run_enabled": dry_run.enabled,
                "dry_run_max_feed_mm_min": dry_run.max_feed_mm_min,
                "laser_dry_run_effective_max_feed_mm_min": dry_run.effective_max_feed_mm_min,
                "laser_dry_run_remaining_seconds": dry_run.remaining_seconds,
                "laser_dry_run_expires_at": dry_run.expires_at,
                "laser_dry_run_ttl_seconds": laser_device::DRY_RUN_TTL_SECS,
                "machine_profiles": machine_profiles,
                "laser_framing_shape": self.framing_shape().as_str(),
                "verified_connection_target_key": verified_target_key,
                "actual_port": actual,
                "preferred_port": preferred,
                "is_fallback": actual != preferred,
                "web_host": self.settings.web_host,
                "can_edit_operator_settings": self.require_operator_settings_access(ctx).is_none(),
                "can_write_reference_data": !matches!(ctx, laser_contracts::CallerContext::Lan { .. }),
                "can_acknowledge_stop": self.can_acknowledge_stop(ctx),
                "mcp": mcp_status_json(mcp_endpoint.as_deref(), &telemetry, tools_count, include_mcp_details),
                // 链路模式来自 settings.json 的 voice.mode（语音中心写入）；设备会话数由桌面端内置语音服务
                // 上报，官方链路或未知时为 null（技术书 §9.4.5）
                "voice": voice_status_json(self.runtime_path.as_deref(), self.voice_telemetry.as_ref())
            })),
        )
    }

    fn handle_jev_status(&self, ctx: &laser_contracts::CallerContext) -> Vec<u8> {
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
        let show_masked = self.require_operator_settings_access(ctx).is_none();
        http_json(
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
        )
    }

    fn handle_jev_test(&self, ctx: &laser_contracts::CallerContext) -> Vec<u8> {
        if let Some(denied) = self.require_operator_settings_access(ctx) {
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
        match feedback::call_jev(
            DEFAULT_JEV_ENDPOINT,
            &api_key,
            &serde_json::json!({
                "feedback_text": "效果正常，无需调整"
            }),
        ) {
            Ok(_) => http_json(200, success(serde_json::json!({"message": "Jev 连接正常"}))),
            Err(error) => http_json(502, failure(format!("Jev 连接失败: {error}"), None)),
        }
    }

    fn handle_jev_reveal(&self, ctx: &laser_contracts::CallerContext) -> Vec<u8> {
        if let Some(denied) = self.require_operator_settings_access(ctx) {
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
        http_json(200, success(serde_json::json!({ "api_key": api_key })))
    }

    fn handle_settings_update(&self, body: &[u8], ctx: &laser_contracts::CallerContext) -> Vec<u8> {
        if let Some(denied) = self.require_operator_settings_access(ctx) {
            return denied;
        }
        let object = match parse_json_object(body, 64 * 1024) {
            Ok(value) => value,
            Err(error) => return http_json(400, failure(error, None)),
        };
        if object.contains_key("process_trace_dir") {
            return http_json(
                400,
                failure("过程记录保存位置只能通过桌面文件夹选择框修改", None),
            );
        }
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
        let process_trace_changed = object.contains_key(laser_runtime::process_trace::SETTING_KEY);
        if let Some(value) = object.get(laser_runtime::process_trace::SETTING_KEY) {
            let Some(enabled) = value.as_bool() else {
                return http_json(400, failure("process_trace_enabled 必须是布尔值", None));
            };
            file_changes.push((
                laser_runtime::process_trace::SETTING_KEY,
                Some(serde_json::json!(enabled)),
            ));
        }
        // LB2：空跑模式开关与限速上限仅允许桌面操作者修改；带 X-Moss-Bench 头的测试脚本一律禁止开关。
        let touches_dry_run = object.contains_key("laser_dry_run_enabled")
            || object.contains_key("dry_run_max_feed_mm_min");
        if touches_dry_run && crate::process_trace::current_bench_source().is_some() {
            return http_json(
                403,
                failure(
                    "测试脚本不能开启、关闭或修改空跑模式，请在 MOSS 桌面端操作",
                    Some(serde_json::json!({"error_code": "bench_cannot_toggle_dry_run"})),
                ),
            );
        }
        let dry_run_enabled_update = match object.get("laser_dry_run_enabled") {
            None => None,
            Some(v) => match v.as_bool() {
                Some(enabled) => Some(enabled),
                None => return http_json(400, failure("laser_dry_run_enabled 必须是布尔值", None)),
            },
        };
        let dry_run_max_feed_update = match object.get("dry_run_max_feed_mm_min") {
            None => None,
            Some(v) => {
                let Some(raw) = v.as_u64().and_then(|n| u32::try_from(n).ok()) else {
                    return http_json(
                        400,
                        failure("dry_run_max_feed_mm_min 必须是 300 ~ 5000 之间的整数", None),
                    );
                };
                match laser_device::validate_dry_run_max_feed(raw) {
                    Ok(valid) => {
                        file_changes
                            .push(("dry_run_max_feed_mm_min", Some(serde_json::json!(valid))));
                        Some(valid)
                    }
                    Err(error) => return http_json(400, failure(error, None)),
                }
            }
        };
        if dry_run_enabled_update == Some(true) && !self.is_laser_connected() {
            return http_json(
                409,
                failure(
                    "请先连接激光雕刻机后再开启空跑模式",
                    Some(serde_json::json!({"error_code": "laser_not_connected"})),
                ),
            );
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
                if let Err(err) = auth.set_static_pin((!trimmed.is_empty()).then_some(trimmed)) {
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
        // r78：机器档案（手填行程 / 手填运动参数 / 扫描偏移）与全局走边框形状校验（先算新态，写盘成功再更新内存）
        let mut next_profiles: Option<crate::machine_profiles::MachineProfileStore> = None;
        if let Some(migrate_val) = object.get("machine_profiles_migrate") {
            let Some(migrate_obj) = migrate_val.as_object() else {
                return http_json(400, failure("machine_profiles_migrate 必须是对象", None));
            };
            let mut draft = self
                .machine_profiles
                .read()
                .map(|g| g.clone())
                .unwrap_or_default();
            match draft.merge_migration(migrate_obj) {
                Ok(true) => next_profiles = Some(draft),
                Ok(false) => {}
                Err(error) => return http_json(400, failure(error, None)),
            }
        }
        if let Some(patch_val) = object.get("machine_profile_patch") {
            let Some(patch_obj) = patch_val.as_object() else {
                return http_json(400, failure("machine_profile_patch 必须是对象", None));
            };
            let mut draft = next_profiles.take().unwrap_or_else(|| {
                self.machine_profiles
                    .read()
                    .map(|g| g.clone())
                    .unwrap_or_default()
            });
            if let Err(error) = draft.apply_patch(patch_obj) {
                return http_json(400, failure(error, None));
            }
            next_profiles = Some(draft);
        }
        if let Some(ref updated_profiles) = next_profiles {
            file_changes.push((
                crate::machine_profiles::MACHINE_PROFILES_KEY,
                Some(updated_profiles.to_json()),
            ));
        }
        let mut next_framing_shape: Option<crate::machine_profiles::FramingShape> = None;
        if let Some(migrate_shape) = object.get("laser_framing_shape_migrate") {
            let Some(raw) = migrate_shape.as_str() else {
                return http_json(
                    400,
                    failure("laser_framing_shape_migrate 必须是字符串", None),
                );
            };
            match crate::machine_profiles::FramingShape::parse(raw) {
                Ok(shape) => {
                    if !self.framing_shape_configured() {
                        next_framing_shape = Some(shape);
                    }
                }
                Err(error) => return http_json(400, failure(error, None)),
            }
        }
        if let Some(shape_val) = object.get("laser_framing_shape") {
            let Some(raw) = shape_val.as_str() else {
                return http_json(400, failure("laser_framing_shape 必须是字符串", None));
            };
            match crate::machine_profiles::FramingShape::parse(raw) {
                Ok(shape) => next_framing_shape = Some(shape),
                Err(error) => return http_json(400, failure(error, None)),
            }
        }
        if let Some(shape) = next_framing_shape {
            file_changes.push((
                crate::machine_profiles::FRAMING_SHAPE_KEY,
                Some(serde_json::Value::String(shape.as_str().into())),
            ));
        }
        // 纯内存测试实例无 runtime_path 时，允许机器档案与走边框形状更新内存状态；其他落盘设置沿用原错处理
        let only_profile_or_framing = !file_changes.is_empty()
            && file_changes.iter().all(|(k, _)| {
                *k == crate::machine_profiles::MACHINE_PROFILES_KEY
                    || *k == crate::machine_profiles::FRAMING_SHAPE_KEY
            });
        if !file_changes.is_empty() && !(self.runtime_path.is_none() && only_profile_or_framing) {
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
            if process_trace_changed && let Some(trace) = self.process_trace() {
                trace.invalidate();
            }
        }
        if let Some(updated_profiles) = next_profiles
            && let Ok(mut slot) = self.machine_profiles.write()
        {
            *slot = updated_profiles;
        }
        if let Some(shape) = next_framing_shape
            && let Ok(mut slot) = self.framing_shape.write()
        {
            *slot = Some(shape);
        }
        // 更新运行时空跑模式状态（开启时计时 60 分钟自动关闭，重启不持久化开启状态）
        match (dry_run_enabled_update, dry_run_max_feed_update) {
            (Some(true), feed) => {
                if let Err(error) = self.dry_run.enable(feed) {
                    return http_json(400, failure(error, None));
                }
            }
            (Some(false), feed) => {
                self.dry_run.disable();
                if let Some(f) = feed
                    && let Err(error) = self.dry_run.set_max_feed(f)
                {
                    return http_json(400, failure(error, None));
                }
            }
            (None, Some(feed)) => {
                if let Err(error) = self.dry_run.set_max_feed(feed) {
                    return http_json(400, failure(error, None));
                }
            }
            (None, None) => {}
        }
        let dry_run = self.dry_run_snapshot();
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
        http_json(
            200,
            success(serde_json::json!({
                "message": if has_mcp_setting {
                    "设置已保存，小智 MCP 端点将在下次启动时生效"
                } else {
                    "设置已保存，将在下次启动时生效"
                },
                "laser_dry_run_enabled": dry_run.enabled,
                "dry_run_max_feed_mm_min": dry_run.max_feed_mm_min,
                "laser_dry_run_effective_max_feed_mm_min": dry_run.effective_max_feed_mm_min,
                "laser_dry_run_remaining_seconds": dry_run.remaining_seconds,
                "laser_dry_run_expires_at": dry_run.expires_at,
                "machine_profiles": self.machine_profiles_json(),
                "laser_framing_shape": self.framing_shape().as_str(),
            })),
        )
    }

    fn handle_events_sse(&self, path: &str) -> Vec<u8> {
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
        http_sse(format!(
            "event: ready\ndata: {}\n\n",
            serde_json::json!({"workflow_id": workflow_id, "status": status})
        ))
    }

    fn handle_set_text_origin(&self, body: &[u8]) -> Vec<u8> {
        let object = match parse_json_object(body, 1024) {
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
        match self.set_text_origin(origin) {
            Ok(()) => http_json(200, success(self.text_origin_json())),
            Err(error) => http_json(500, failure(format!("保存对刀起点失败：{error}"), None)),
        }
    }

    fn handle_set_engrave_route(&self, body: &[u8]) -> Vec<u8> {
        let object = match parse_json_object(body, 1024) {
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
        match self.set_engrave_route(route) {
            Ok(()) => http_json(200, success(self.engrave_route_json())),
            Err(error) => http_json(500, failure(format!("保存雕刻路线失败：{error}"), None)),
        }
    }

    fn handle_reset_time_calibration(&self, body: &[u8]) -> Vec<u8> {
        let object = match parse_json_object(body, 4 * 1024) {
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
        match self.reset_time_calibration(target) {
            Ok(()) => http_json(200, success(self.time_calibration_status())),
            Err(error) => http_json(500, failure(format!("清除工时校准失败：{error}"), None)),
        }
    }
}
