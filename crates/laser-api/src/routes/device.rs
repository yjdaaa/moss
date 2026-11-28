use super::super::*;

impl ApiService {
    pub(super) fn route_device(
        &self,
        method: &str,
        route: &str,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
        short_job_watch: Option<&std::sync::Arc<ShortJobWatch>>,
    ) -> Option<Vec<u8>> {
        if method != "POST" {
            return None;
        }
        match route {
            "/api/device/pointer-fire" => Some(self.handle_pointer_fire(body, ctx)),
            "/api/registration/read-position" | "/api/registration/preview" => {
                Some(self.handle_registration_route(route, body, ctx))
            }
            "/api/safe-action" => Some(self.handle_safe_action_route(body, ctx)),
            "/api/connection/check" => Some(self.handle_connection_check(body, ctx)),
            "/api/device/framing" => Some(self.handle_device_framing(body, ctx, short_job_watch)),
            _ => None,
        }
    }

    fn handle_pointer_fire(&self, body: &[u8], ctx: &laser_contracts::CallerContext) -> Vec<u8> {
        if self.is_shutdown_requested() {
            return http_json(
                503,
                failure(
                    "服务正在关闭，已拒绝点射操作",
                    Some(serde_json::json!({"error_code": "host_shutting_down"})),
                ),
            );
        }
        let mut object = match parse_json_object(body, 64 * 1024) {
            Ok(value) => value,
            Err(error) => return http_json(400, failure(error, None)),
        };
        strip_identity_fields(&mut object);
        self.run_pointer_fire(&object, ctx)
    }

    fn handle_safe_action_route(
        &self,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
    ) -> Vec<u8> {
        let mut object = match parse_json_object(body, 64 * 1024) {
            Ok(value) => value,
            Err(error) => return http_json(400, failure(error, None)),
        };
        strip_identity_fields(&mut object);
        if !matches!(ctx, laser_contracts::CallerContext::Approval { .. }) {
            object.remove("override_job_id");
        }
        let mut args: laser_contracts::LaserSafeActionArgs =
            match serde_json::from_value(serde_json::Value::Object(object.clone())) {
                Ok(a) => a,
                Err(e) => return http_json(400, failure(format!("参数解析失败: {e}"), None)),
            };
        if self.is_shutdown_requested()
            && !laser_contracts::is_safe_action_stop_or_halt(&args.action)
        {
            return http_json(
                503,
                failure(
                    "服务正在关闭，已拒绝新的设备安全动作",
                    Some(serde_json::json!({"error_code": "host_shutting_down"})),
                ),
            );
        }
        // 继续走边框（2026-11-18）：先确认、找到正在走的那次边框，再建审批或执行——
        // 没有边框在走、未确认都不建废审批；审批绑定这次作业的 job_id 与它的设备连接
        if args.action.trim().eq_ignore_ascii_case("resume")
            && !matches!(ctx, laser_contracts::CallerContext::Approval { .. })
        {
            if let Err(response) = self.bind_framing_resume(&mut object) {
                return response;
            }
            args = match serde_json::from_value(serde_json::Value::Object(object.clone())) {
                Ok(a) => a,
                Err(e) => return http_json(400, failure(format!("参数解析失败: {e}"), None)),
            };
        }
        // 严格模式下的 R3 倍率提高动作（2026-11-27，技术书 r73）：
        // 申请创建审批前先校验命中正在运行的任务并绑定原任务与连接，无运行中任务不建废审批；
        // 批准重放时仍由 handle_safe_action_with_override_job 按执行当刻状态重新校验。
        let action_lower = args.action.trim().to_ascii_lowercase();
        let is_override_action =
            laser_tools::realtime_action(&action_lower).is_some_and(|rt| !rt.alters_alarm_state);
        if is_override_action
            && laser_contracts::safe_action_requires_approval(&args.action)
            && self.strict_mode()
            && laser_contracts::ApprovalSource::from_caller(ctx).is_some()
        {
            if let Err(response) = self.bind_live_override_for_approval(&args, &mut object) {
                return response;
            }
            args = match serde_json::from_value(serde_json::Value::Object(object.clone())) {
                Ok(a) => a,
                Err(e) => return http_json(400, failure(format!("参数解析失败: {e}"), None)),
            };
        }
        // 严格模式检查点（技术书 §6.2）：只对 R3 动作；急停、软复位、关激光、降低倍率永不追加条件（A-27）
        let mut authority = approval::R3Authority::Unchecked;
        if laser_contracts::safe_action_requires_approval(&args.action) {
            let action = args.action.trim().to_owned();
            let facts = approval::safe_action_digest_facts(&object);
            let summary = safe_action_approval_summary(&action, &facts);
            authority = match self.require_r3_authority(
                ctx,
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
            if let approval::R3Authority::Approval(record) = &authority {
                match record.connection.as_ref() {
                    Some(connection) => apply_connection_to_safe_action(&mut args, connection),
                    None => {
                        let (status, body) =
                            approval::error_response(&approval::ApprovalError::Mismatch);
                        self.finish_r3_authority(&authority, &Err((status, body.clone())), None);
                        return http_json(status, body);
                    }
                }
            }
        }
        let job_id = object.get("job_id").and_then(serde_json::Value::as_str);
        let expected_override_job = matches!(ctx, laser_contracts::CallerContext::Approval { .. })
            .then(|| {
                object
                    .get("override_job_id")
                    .and_then(serde_json::Value::as_str)
            })
            .flatten();
        let res =
            self.handle_safe_action_with_override_job(&args, ctx, job_id, expected_override_job);
        let action_name = args.action.trim().to_owned();
        if action_name == "emergency_stop" {
            self.emit_event(if res.success {
                LaserEvent {
                    level: LaserEventLevel::Warn,
                    text: format!("已发出急停（0x18）：{}", res.result),
                    hint: Some("0x18 只是软件复位请求：到设备旁确认机器已停、激光已灭".to_owned()),
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
            let detail = res
                .detail
                .as_ref()
                .map(|code| serde_json::json!({ "error_code": code }));
            Err((400, failure(res.result.clone(), detail)))
        };
        self.finish_r3_authority(&authority, &outcome, None);
        http_json(
            if res.success { 200 } else { 400 },
            serde_json::json!({
                "success": res.success,
                "result": res.result,
                "detail": res.detail,
            }),
        )
    }

    fn handle_connection_check(
        &self,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
    ) -> Vec<u8> {
        let object = match parse_json_object(body, 64 * 1024) {
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
        // n73 / r66 / r79：工作台（桌面、本机网页、局域网配对）同步本进程激光连通态；握手成功时记下连接与固件行程
        if matches!(
            ctx,
            laser_contracts::CallerContext::Desktop
                | laser_contracts::CallerContext::Loopback
                | laser_contracts::CallerContext::Lan { .. }
        ) {
            self.set_laser_connected(verified.is_some());
            if let Some(spec) = verified {
                self.remember_verified_connection_with_limits(&spec, result.get("machine_limits"));
            }
        }
        http_json(200, success(result))
    }

    fn handle_device_framing(
        &self,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
        short_job_watch: Option<&std::sync::Arc<ShortJobWatch>>,
    ) -> Vec<u8> {
        if self.is_shutdown_requested() {
            return http_json(
                503,
                failure(
                    "服务正在关闭，已拒绝走边框操作",
                    Some(serde_json::json!({"error_code": "host_shutting_down"})),
                ),
            );
        }
        let mut object = match parse_json_object(body, 64 * 1024) {
            Ok(value) => value,
            Err(error) => return http_json(400, failure(error, None)),
        };
        strip_identity_fields(&mut object);
        self.framing_with_authority(object, ctx, short_job_watch)
    }
}
