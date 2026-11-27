//! 工具分发：工具开关与严格模式门禁、连接参数补全、后端结果折叠。

use crate::{HostBackend, telemetry_for, tool_can_be_disabled};

/// n73 需要连接机器的工具。
const CONNECTION_TOOLS: [&str; 7] = [
    "check_laser_connection_tool",
    "laser_safe_action_tool",
    "laser_workflow_tool",
    "run_calibration_grid_tool",
    "start_tuned_job_tool",
    "laser_grbl_tool",
    "laser_network_grbl_tool",
];

/// 调用方没给任何连接参数时补上已验证的连接；给了任何一个（串口号、IP、连接方式）就完全按调用方的来。
fn with_default_connection(
    name: &str,
    arguments: &serde_json::Value,
    backend: Option<&dyn HostBackend>,
) -> Option<serde_json::Value> {
    if !CONNECTION_TOOLS.contains(&name) {
        return None;
    }
    let object = arguments.as_object()?;
    let given = [
        "port",
        "serial_port",
        "host",
        "network_host",
        "connection_mode",
    ]
    .iter()
    .any(|key| {
        object
            .get(*key)
            .is_some_and(|v| !v.is_null() && v.as_str().is_none_or(|s| !s.trim().is_empty()))
    });
    if given {
        return None;
    }
    let fields = backend?.default_connection()?;
    let mut merged = object.clone();
    for (key, value) in fields {
        merged.entry(key).or_insert(value);
    }
    Some(serde_json::Value::Object(merged))
}

pub fn format_mcp_response(result: &laser_contracts::ToolCallResult) -> String {
    let value = serde_json::json!({
        "success": result.success,
        "result": result.result,
        "detail": result.detail,
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
}

// ---------------------------------------------------------------------------
// 参数读取小工具
// ---------------------------------------------------------------------------

fn arg_str<'a>(args: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn arg_bool(args: &serde_json::Value, key: &str) -> bool {
    match args.get(key) {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => {
            matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
        }
        Some(serde_json::Value::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0),
        _ => false,
    }
}

fn no_backend(tool: &str, what: &str) -> laser_contracts::ToolCallResult {
    laser_contracts::ToolCallResult::err(format!(
        "{tool} 需要 moss-host 后端执行{what}，当前 MCP 会话未连接 host"
    ))
}

/// 把后端 JSON 响应折叠为工具结果：`success` 字段决定成败，`speech`/`result` 作摘要，全文放 detail。
fn backend_result(
    tool: &str,
    response: Result<(u16, serde_json::Value), String>,
) -> laser_contracts::ToolCallResult {
    match response {
        Err(error) => {
            laser_contracts::ToolCallResult::err(format!("{tool} 调用 moss-host 失败: {error}"))
        }
        Ok((status, body)) => {
            let success = body
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(status < 400);
            let result = body
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let mut summary = if success {
                result
                    .get("speech")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| summarize_success(tool, &result))
            } else {
                result
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{tool} 失败 (HTTP {status})"))
            };
            if success {
                for warn in extract_record_warnings(&result) {
                    if !summary.contains(&warn) {
                        summary.push_str(&format!("；⚠️ {warn}"));
                    }
                }
            }
            let detail = serde_json::to_string_pretty(&body).unwrap_or_default();
            laser_contracts::ToolCallResult::with_detail(success, summary, detail)
        }
    }
}

/// AI 生成预览后给模型的固定说明（2026-11-06 统一 AI 预览流程）：预览会弹到 MOSS 桌面，
/// 必须等用户在预览窗点“同意开刻”或（未开严格模式时）口头明确确认，才能 confirm_send。
pub const DESKTOP_PREVIEW_HINT: &str = "预览已弹到 MOSS 电脑屏幕上。请把尺寸、材料和预计时间告诉用户，让用户在预览窗核对后点「同意开刻」；未开启严格模式时，用户也可以口头明确说确认，你再调用 laser_workflow_tool(action=confirm_send, confirmed=true)。用户想先看位置时，可以在用户确认走边框后用 action=frame 带同一个 workflow_id 空走一圈边框（不出光）。用户确认前不要发送。";

fn with_desktop_preview_hint(
    result: laser_contracts::ToolCallResult,
    wanted_send: bool,
) -> laser_contracts::ToolCallResult {
    if !result.success {
        return result;
    }
    let note = if wanted_send {
        "（本次没有直接开始加工：AI 生成的任务必须先在电脑上预览）"
    } else {
        ""
    };
    let params = result
        .detail
        .as_deref()
        .and_then(|detail| serde_json::from_str::<serde_json::Value>(detail).ok())
        .map(|body| preview_params_sentence(&body["result"]))
        .unwrap_or_default();
    laser_contracts::ToolCallResult {
        success: true,
        result: format!(
            "{}。{params}{DESKTOP_PREVIEW_HINT}{note}",
            result.result.trim_end_matches('。')
        ),
        detail: result.detail,
    }
}

fn extract_record_warnings(record: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    for arr in [
        record.get("warnings").and_then(|v| v.as_array()),
        record
            .pointer("/artifacts/warnings")
            .and_then(|v| v.as_array()),
        record
            .pointer("/send_result/warnings")
            .and_then(|v| v.as_array()),
    ]
    .into_iter()
    .flatten()
    {
        for item in arr {
            if let Some(s) = item.as_str().map(str::trim).filter(|s| !s.is_empty())
                && !out.iter().any(|existing| existing == s)
            {
                out.push(s.to_owned());
            }
        }
    }
    out
}

/// n73 预览实际用的功率 / 速度 / 遍数，告诉模型以便如实复述给用户（手动指定的注明）。
fn preview_params_sentence(record: &serde_json::Value) -> String {
    let params = &record["artifacts"]["actual_params"];
    let (Some(power), Some(feed)) = (
        params["laser_max_power"].as_f64(),
        params["feed_rate"].as_f64(),
    ) else {
        return String::new();
    };
    let scale = params["power_scale"]
        .as_f64()
        .filter(|s| *s > 0.0)
        .unwrap_or(1000.0);
    let passes = params["passes"].as_u64().unwrap_or(1);
    let source = if record["input"]["manual_params"].is_object() {
        "（这次手动指定）"
    } else {
        "（材料库推荐）"
    };
    let warnings = extract_record_warnings(record);
    let warn_sentence = if warnings.is_empty() {
        String::new()
    } else {
        format!("注意：{}。", warnings.join("；"))
    };
    format!(
        "这次参数：功率 {:.0}%，速度 {feed:.0} mm/min，{passes} 遍{source}。{warn_sentence}",
        power / scale * 100.0
    )
}

fn summarize_success(tool: &str, result: &serde_json::Value) -> String {
    let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let id = result
        .get("workflow_id")
        .or_else(|| result.get("calibration_id"))
        .or_else(|| result.get("task_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let mut parts = vec![format!("{tool} 完成")];
    if !id.is_empty() {
        parts.push(format!("id={id}"));
    }
    if !status.is_empty() {
        parts.push(format!("status={status}"));
    }
    if let Some(send) = result.get("send_result")
        && let (Some(sent), Some(total)) = (
            send.get("lines_sent").and_then(|v| v.as_u64()),
            send.get("total_lines").and_then(|v| v.as_u64()),
        )
    {
        parts.push(format!("已下发 {sent}/{total} 行"));
    }
    if let Some(cells) = result.get("cell_count").and_then(|v| v.as_u64()) {
        parts.push(format!("{cells} 个标定单元格"));
    }
    parts.join("，")
}

/// 从工具参数提取连接字段并合并到后端请求体（工具参数名 host/port/telnet_port 与 HTTP 契约对齐）。
fn with_connection_fields(
    args: &serde_json::Value,
    mut body: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let copy = |from: &str, to: &str, body: &mut serde_json::Map<String, serde_json::Value>| {
        if let Some(value) = args.get(from) {
            let is_empty = value.as_str().is_some_and(|s| s.trim().is_empty()) || value.is_null();
            if !is_empty && !body.contains_key(to) {
                body.insert(to.to_owned(), value.clone());
            }
        }
    };
    copy("network_host", "network_host", &mut body);
    copy("host", "network_host", &mut body);
    copy("network_telnet_port", "network_telnet_port", &mut body);
    copy("telnet_port", "network_telnet_port", &mut body);
    copy("port", "port", &mut body);
    copy("baudrate", "baudrate", &mut body);
    copy("connection_mode", "connection_mode", &mut body);
    // 只有给了串口号或 IP 才推断连接方式；都没给时留空，由后端按 `.env` 的默认连接方式决定
    // （n72 修正：原来一律补成 network，`.env` 配的默认串口会被忽略）
    if !body.contains_key("connection_mode")
        && (body.contains_key("port") || body.contains_key("network_host"))
    {
        let mode = if body.contains_key("port") && !body.contains_key("network_host") {
            "serial"
        } else {
            "network"
        };
        body.insert(
            "connection_mode".into(),
            serde_json::Value::String(mode.into()),
        );
    }
    body
}

/// 经 `HostBackend` 调用 `/api/connection/check`，复用 `ApiService` 的活动作业忙碌保护与 `target_open_locks`，
/// 避免在加工中另开串口/Telnet 下发 `$$` 打断或重启控制器。
fn probe_connection_via_backend(
    tool: &str,
    arguments: &serde_json::Value,
    backend: &dyn HostBackend,
) -> laser_contracts::ToolCallResult {
    let mut body = with_connection_fields(arguments, serde_json::Map::new());
    if !body.contains_key("connection_mode") {
        match tool {
            "laser_grbl_tool" => {
                body.insert("connection_mode".into(), serde_json::json!("serial"));
            }
            "laser_network_grbl_tool" => {
                body.insert("connection_mode".into(), serde_json::json!("network"));
            }
            _ => {}
        }
    }
    if body.contains_key("connection_mode") {
        body.insert("auto_fallback".into(), serde_json::json!(false));
    }
    if let Some(timeout) = arguments.get("timeout").filter(|v| !v.is_null()) {
        body.insert("network_timeout".into(), timeout.clone());
    }
    let response = backend.request(
        "POST",
        "/api/connection/check",
        &serde_json::Value::Object(body),
    );
    let (status, resp) = match response {
        Ok((status, resp))
            if status < 400
                && resp
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true) =>
        {
            (status, resp)
        }
        other => return backend_result(tool, other),
    };
    let _ = status;
    let result = &resp["result"];
    let required_mode = match tool {
        "laser_grbl_tool" => Some("serial"),
        "laser_network_grbl_tool" => Some("network"),
        _ => None,
    };
    let connected = match required_mode {
        Some("serial") => result["serial_connected"].as_bool().unwrap_or(false),
        Some("network") => result["network_connected"].as_bool().unwrap_or(false),
        _ => result["connected"].as_bool().unwrap_or(false),
    };
    let mode_summary = required_mode.and_then(|mode| {
        result["attempts"].as_array().and_then(|arr| {
            arr.iter()
                .find(|entry| entry["mode"].as_str() == Some(mode))
                .and_then(|entry| entry["summary"].as_str())
                .filter(|s| !s.is_empty())
        })
    });
    let summary = mode_summary
        .or_else(|| result["summary"].as_str().filter(|s| !s.is_empty()))
        .unwrap_or(if connected {
            "设备已连通"
        } else {
            "设备未连通"
        })
        .to_owned();
    laser_contracts::ToolCallResult::with_detail(
        connected,
        summary,
        serde_json::to_string_pretty(&resp).unwrap_or_default(),
    )
}

/// 用户关闭的工具（NV8）：不执行、不触达后端。停止类工具不受开关影响。
fn disabled_gate(
    name: &str,
    arguments: &serde_json::Value,
    backend: Option<&dyn HostBackend>,
) -> Option<laser_contracts::ToolCallResult> {
    let backend = backend?;
    if !tool_can_be_disabled(name)
        || laser_contracts::tool_risk(name, arguments) == Some(laser_contracts::RiskLevel::R2)
        || !backend.tool_disabled(name)
    {
        return None;
    }
    Some(laser_contracts::ToolCallResult::with_detail(
        false,
        format!("{name} 已在 MOSS 中关闭，需要时请在 MOSS 语音中心的工具开关里打开"),
        serde_json::json!({"error_code": "tool_disabled"}).to_string(),
    ))
}

/// 严格模式下的接入点分级处理（技术书 §6.1–§6.3，实施清单 P1-06）：
/// - R4 与未分级工具：`tool_not_allowed`，不执行；
/// - R3：照常交给后端，由执行层（`require_r3_authority`）创建审批并返回 `approval_required`；
///   后端执行层没有检查点的 R3（采用反馈建议）在这里直接拒绝，提示到桌面端操作；
/// - R0–R2：不变。
///
/// 严格模式关闭或没有后端时返回 `None`，行为与之前一致。
fn strict_bridge_gate(
    name: &str,
    arguments: &serde_json::Value,
    backend: Option<&dyn HostBackend>,
) -> Option<laser_contracts::ToolCallResult> {
    use laser_contracts::{
        AuditAction, AuditDetail, AuditEntry, AuditResult, RiskLevel, ToolCallResult,
    };
    let backend = backend?;
    let strict = backend.strict_mode();
    if !strict && !backend.always_deny_r4() {
        return None;
    }
    let (action, error_code, result) = match laser_contracts::tool_risk(name, arguments) {
        None | Some(RiskLevel::R4) => (
            AuditAction::R4Denied,
            "tool_not_allowed",
            ToolCallResult::with_detail(
                false,
                format!("{name} 在严格模式下不对语音开放，请在 MOSS 桌面端操作"),
                serde_json::json!({"error_code": "tool_not_allowed", "risk": "R4"}).to_string(),
            ),
        ),
        Some(RiskLevel::R3) if strict && name == "regenerate_text_laser_task_tool" => (
            AuditAction::StrictModeBlocked,
            "strict_mode_blocked",
            ToolCallResult::with_detail(
                false,
                "严格模式下采用调参建议需要在 MOSS 桌面端操作",
                serde_json::json!({"error_code": "strict_mode_blocked", "risk": "R3"}).to_string(),
            ),
        ),
        _ => return None,
    };
    let entry = AuditEntry {
        at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64())
            .unwrap_or(0.0),
        client: backend.audit_client(),
        action,
        target: name.chars().take(64).collect(),
        result: AuditResult::Denied,
        detail: AuditDetail {
            tool: Some(name.chars().take(64).collect()),
            error_code: Some(error_code.into()),
            ..AuditDetail::default()
        },
    };
    if let Err(error) = backend.audit(&entry) {
        eprintln!("[laser-mcp] 审计记录写入失败（拒绝照常生效）: {error}");
    }
    Some(result)
}

pub fn dispatch_tool_call(
    name: &str,
    arguments: &serde_json::Value,
    developer_mode: bool,
) -> laser_contracts::ToolCallResult {
    dispatch_tool_call_with(name, arguments, developer_mode, None)
}

/// 调用一个工具：先过工具开关与严格模式分级，再分发；结果计入调用统计（未分级的工具名统一记为“未知工具”，
/// 避免接入点发来的任意名字撑大统计表）。
pub fn dispatch_tool_call_with(
    name: &str,
    arguments: &serde_json::Value,
    developer_mode: bool,
    backend: Option<&dyn HostBackend>,
) -> laser_contracts::ToolCallResult {
    use laser_contracts::ToolCallOutcome;
    // n73：没给连接参数时用工作台握手成功的连接（审批与门禁看到的就是补全后的目标）
    let filled = with_default_connection(name, arguments, backend);
    let arguments = filled.as_ref().unwrap_or(arguments);
    let gate = disabled_gate(name, arguments, backend)
        .or_else(|| strict_bridge_gate(name, arguments, backend));
    let (result, outcome) = match gate {
        Some(denied) => (denied, ToolCallOutcome::Denied),
        None => {
            let result = dispatch_allowed(name, arguments, developer_mode, backend);
            let outcome = if result.success {
                ToolCallOutcome::Ok
            } else {
                ToolCallOutcome::Failed
            };
            (result, outcome)
        }
    };
    let caller = backend
        .map(|backend| backend.audit_client())
        .unwrap_or_else(|| "local".into());
    let counted = if laser_contracts::tool_risk(name, &serde_json::json!({})).is_some() {
        name
    } else {
        "（未知工具）"
    };
    telemetry_for(backend).record_tool_call(counted, &caller, outcome);
    result
}

fn dispatch_allowed(
    name: &str,
    arguments: &serde_json::Value,
    developer_mode: bool,
    backend: Option<&dyn HostBackend>,
) -> laser_contracts::ToolCallResult {
    use laser_contracts::ToolCallResult;
    let post =
        |route: &str, body: serde_json::Map<String, serde_json::Value>| -> Option<ToolCallResult> {
            backend.map(|b| {
                backend_result(
                    name,
                    b.request("POST", route, &serde_json::Value::Object(body)),
                )
            })
        };
    match name {
        "check_laser_connection_tool" => {
            if let Some(b) = backend {
                probe_connection_via_backend(name, arguments, b)
            } else {
                let args = serde_json::from_value(arguments.clone()).unwrap_or_default();
                laser_tools::execute_check_laser_connection(&args)
            }
        }
        "laser_safe_action_tool" => {
            if let Some(b) = backend {
                backend_result(name, b.request("POST", "/api/safe-action", arguments))
            } else {
                let args = serde_json::from_value(arguments.clone()).unwrap_or_default();
                laser_tools::execute_laser_safe_action(&args)
            }
        }
        "recommend_laser_params_tool" => {
            let args = match serde_json::from_value(arguments.clone()) {
                Ok(args) => args,
                Err(e) => return ToolCallResult::err(format!("参数校验失败: {e}")),
            };
            let Some(backend) = backend else {
                return laser_tools::execute_recommend_laser_params(&args);
            };
            let material = arg_str(arguments, "material").unwrap_or("");
            let thickness = arguments
                .get("thickness_mm")
                .and_then(|v| {
                    v.as_f64()
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
                })
                .unwrap_or(0.0);
            if material.is_empty() || !thickness.is_finite() || thickness <= 0.0 {
                return laser_tools::execute_recommend_laser_params(&args);
            }
            let laser_mode = arg_str(arguments, "laser_mode").unwrap_or("engrave");
            let mut route = format!(
                "/api/ui/material-recommendation?material={}&thickness_mm={thickness}&laser_mode={}",
                percent_encode(material),
                percent_encode(laser_mode),
            );
            if let Some(engraving) = arg_str(arguments, "engraving_mode") {
                route.push_str(&format!("&engraving_mode={}", percent_encode(engraving)));
            } else if !laser_mode.eq_ignore_ascii_case("cut") {
                route.push_str("&engraving_mode=raster");
            }
            if let Some(power_scale) = arguments
                .get("power_scale")
                .and_then(|v| {
                    v.as_f64()
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
                })
                .filter(|v| v.is_finite() && *v > 0.0)
            {
                route.push_str(&format!("&power_scale={power_scale}"));
            }
            match backend.request("GET", &route, &serde_json::Value::Null) {
                Ok((status, body)) => {
                    let ok = body
                        .get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(status < 400);
                    if ok {
                        let res = &body["result"];
                        let mat = res
                            .get("material")
                            .and_then(|v| v.as_str())
                            .unwrap_or(material);
                        let matched = res
                            .get("matched_thickness_mm")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(thickness);
                        let mode = res
                            .get("laser_mode")
                            .and_then(|v| v.as_str())
                            .unwrap_or(laser_mode);
                        let power = res
                            .get("laser_max_power")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        let feed = res.get("feed_rate").and_then(|v| v.as_i64()).unwrap_or(0);
                        let passes = res.get("passes").and_then(|v| v.as_i64()).unwrap_or(1);
                        let summary = format!(
                            "推荐参数：{mat} {matched}mm ({mode}) -> 最大功率 S{power}，速度 F{feed} mm/min，次数 {passes}"
                        );
                        let detail = serde_json::to_string_pretty(&body).unwrap_or_default();
                        ToolCallResult::with_detail(true, summary, detail)
                    } else {
                        backend_result(name, Ok((status, body)))
                    }
                }
                Err(error) => backend_result(name, Err(error)),
            }
        }
        "command_execution_tool" => match serde_json::from_value(arguments.clone()) {
            Ok(args) => laser_tools::execute_command(&args, developer_mode),
            Err(e) => ToolCallResult::err(format!("参数校验失败: {e}")),
        },
        "open_app_tool" => match serde_json::from_value(arguments.clone()) {
            Ok(args) => laser_tools::execute_open_app(&args),
            Err(e) => ToolCallResult::err(format!("参数校验失败: {e}")),
        },
        "refine_laser_params_from_feedback_tool" => {
            if let Some(backend) = backend {
                return backend_result(name, backend.request("POST", "/api/feedback", arguments));
            }
            no_backend(name, "反馈调参与历史记录")
        }

        // ---- 以下工具需要 host 后端（持久化 / 真实作业）----
        "laser_workflow_tool" => {
            let action = arg_str(arguments, "action").unwrap_or("status");
            let workflow_id = arg_str(arguments, "workflow_id");
            let confirmed = arg_bool(arguments, "confirmed");
            // frame（按预览走边框，2026-11-18）同样会让机器运动：与发送一样先确认、带编号、补连接
            if matches!(action, "confirm_send" | "send" | "resume" | "frame") {
                if !confirmed {
                    return ToolCallResult::err(if action == "frame" {
                        "走边框未确认 (confirmed=false)：先问用户“确认走边框吗”"
                    } else {
                        "发送动作未确认 (confirmed=false)"
                    });
                }
                if workflow_id.is_none() {
                    return ToolCallResult::err(format!("{action} 缺少 workflow_id"));
                }
            }
            let Some(backend) = backend else {
                return laser_tools::execute_laser_workflow(Some(action), workflow_id, confirmed);
            };
            let mut body = arguments.as_object().cloned().unwrap_or_default();
            body.insert("action".into(), serde_json::Value::String(action.into()));
            // 重新预览始终生成新编号，模型带回的旧编号不得触发 HTTP 冲突。
            if action == "preview" {
                body.remove("workflow_id");
            }
            // 文字或基础图形（r44：可以只刻图形）走文字任务生成
            let has_design = ["shapes", "border"]
                .iter()
                .any(|key| arguments.get(*key).is_some_and(|v| !v.is_null()));
            let route = if action == "preview"
                && (arg_str(arguments, "text").is_some() || has_design)
                && arg_str(arguments, "gcode_file").is_none()
            {
                "/api/text-task/generate"
            } else {
                "/api/workflow"
            };
            let body = if matches!(action, "confirm_send" | "send" | "resume" | "frame") {
                with_connection_fields(arguments, body)
            } else {
                body
            };
            let result = backend_result(
                name,
                backend.request("POST", route, &serde_json::Value::Object(body)),
            );
            if matches!(action, "preview" | "plan" | "create") {
                with_desktop_preview_hint(result, false)
            } else {
                result
            }
        }
        "run_calibration_grid_tool" => {
            let calibration_id = arg_str(arguments, "calibration_id");
            let confirmed = arg_bool(arguments, "confirmed");
            let mut body = arguments.as_object().cloned().unwrap_or_default();
            if let Some(id) = calibration_id.filter(|_| confirmed) {
                body.insert(
                    "calibration_id".into(),
                    serde_json::Value::String(id.into()),
                );
                body.insert("confirmed".into(), serde_json::Value::Bool(true));
                let body = with_connection_fields(arguments, body);
                return post("/api/material-lab/send", body)
                    .unwrap_or_else(|| no_backend(name, "标定矩阵下发"));
            }
            if arg_str(arguments, "material").is_none() {
                return ToolCallResult::err(
                    "run_calibration_grid 需要 material 与 thickness_mm 生成矩阵，或 calibration_id + confirmed=true 发送已生成的矩阵",
                );
            }
            body.remove("confirmed");
            post("/api/material-lab/preview", body)
                .unwrap_or_else(|| no_backend(name, "标定矩阵生成"))
        }
        "select_calibration_cell_tool" => {
            let Ok(args) = serde_json::from_value::<laser_contracts::SelectCalibrationCellArgs>(
                arguments.clone(),
            ) else {
                return ToolCallResult::err("参数校验失败: calibration_id 与 cell_number 必填");
            };
            if args.calibration_id.trim().is_empty() || args.cell_number < 1 {
                return laser_tools::execute_select_calibration_cell(&args);
            }
            let body = arguments.as_object().cloned().unwrap_or_default();
            post("/api/material-lab/select-cell", body)
                .unwrap_or_else(|| laser_tools::execute_select_calibration_cell(&args))
        }
        "material_params_tool" => {
            let action = arg_str(arguments, "action")
                .unwrap_or("list")
                .to_ascii_lowercase();
            let Some(backend) = backend else {
                return no_backend(name, "材料库读写");
            };
            match action.as_str() {
                "list" => backend_result(
                    name,
                    backend.request(
                        "GET",
                        "/v1/material-lab/bootstrap",
                        &serde_json::Value::Null,
                    ),
                ),
                "get" | "recommend" => {
                    let material = arg_str(arguments, "material").unwrap_or("");
                    let thickness = arguments
                        .get("thickness_mm")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    if material.is_empty() || thickness <= 0.0 {
                        return ToolCallResult::err("get/recommend 需要 material 与 thickness_mm");
                    }
                    let laser_mode = arg_str(arguments, "laser_mode").unwrap_or("engrave");
                    let engraving = arg_str(arguments, "engraving_mode").unwrap_or("raster");
                    let mut route = format!(
                        "/api/ui/material-recommendation?material={}&thickness_mm={thickness}&laser_mode={}&engraving_mode={}",
                        percent_encode(material),
                        percent_encode(laser_mode),
                        percent_encode(engraving),
                    );
                    if let Some(power_scale) = arguments
                        .get("power_scale")
                        .and_then(|v| {
                            v.as_f64()
                                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
                        })
                        .filter(|v| v.is_finite() && *v > 0.0)
                    {
                        route.push_str(&format!("&power_scale={power_scale}"));
                    }
                    backend_result(
                        name,
                        backend.request("GET", &route, &serde_json::Value::Null),
                    )
                }
                "save" | "delete" | "import" | "export" => {
                    let mut body = arguments.as_object().cloned().unwrap_or_default();
                    body.insert("action".into(), serde_json::Value::String(action.clone()));
                    if arguments.get("confirmed").is_some() {
                        body.insert(
                            "confirmed".into(),
                            serde_json::Value::Bool(arg_bool(arguments, "confirmed")),
                        );
                    }
                    for (from, to) in [("aliases_json", "aliases"), ("params_json", "library_json")]
                    {
                        if let Some(raw) = arg_str(arguments, from)
                            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw)
                        {
                            body.insert(to.into(), parsed);
                        }
                    }
                    backend_result(
                        name,
                        backend.request(
                            "POST",
                            "/api/material-lab/manage",
                            &serde_json::Value::Object(body),
                        ),
                    )
                }
                other => ToolCallResult::err(format!("material_params 不支持的 action: {other}")),
            }
        }
        "generate_text_laser_task_tool" => {
            if arg_str(arguments, "text").is_none() {
                return ToolCallResult::err(format!(
                    "{name} 当前仅支持 text 素材；图片素材请通过网页/桌面端上传后使用 laser_workflow_tool"
                ));
            }
            let Some(backend) = backend else {
                return no_backend(name, "文字任务生成");
            };
            let mut body = arguments.as_object().cloned().unwrap_or_default();
            if let Some(size) = arguments.get("font_size").and_then(|v| v.as_f64()) {
                // 工具参数 font_size 为像素/字号语义；HTTP 契约以 mm 计，按 32px≈1mm 折算
                body.entry("font_size_mm")
                    .or_insert(serde_json::json!((size / 32.0).clamp(3.0, 80.0)));
            }
            let generated = backend.request(
                "POST",
                "/api/text-task/generate",
                &serde_json::Value::Object(body),
            );
            // 2026-11-06 用户决定：AI 生成的任务必须先在电脑上弹出预览，生成后不再顺带发送；
            // `send_after_generate` 只保留在工具参数里兼容旧调用，生成结果会说明需要用户确认后再 confirm_send。
            let wanted_send =
                arg_bool(arguments, "confirmed") && arg_bool(arguments, "send_after_generate");
            with_desktop_preview_hint(backend_result(name, generated), wanted_send)
        }
        "start_tuned_job_tool" => {
            let Some(workflow_id) =
                arg_str(arguments, "task_id").or_else(|| arg_str(arguments, "workflow_id"))
            else {
                return ToolCallResult::err(
                    "start_tuned_job 需要 task_id（由 generate_text_laser_task_tool 返回的 workflow_id）",
                );
            };
            let confirmed = arg_bool(arguments, "confirmed");
            let dry_run = arg_bool(arguments, "dry_run");
            let Some(backend) = backend else {
                return no_backend(name, "任务发送");
            };
            let mut body = serde_json::Map::new();
            body.insert(
                "workflow_id".into(),
                serde_json::Value::String(workflow_id.into()),
            );
            if !confirmed || dry_run {
                body.insert("action".into(), serde_json::Value::String("status".into()));
                let result = backend_result(
                    name,
                    backend.request("POST", "/api/workflow", &serde_json::Value::Object(body)),
                );
                return ToolCallResult::with_detail(
                    result.success,
                    format!("dry_run/未确认：仅返回任务 {workflow_id} 当前状态，未发送"),
                    result.detail.unwrap_or_default(),
                );
            }
            body.insert(
                "action".into(),
                serde_json::Value::String("confirm_send".into()),
            );
            body.insert("confirmed".into(), serde_json::Value::Bool(true));
            let body = with_connection_fields(arguments, body);
            backend_result(
                name,
                backend.request("POST", "/api/workflow", &serde_json::Value::Object(body)),
            )
        }
        "regenerate_text_laser_task_tool" => {
            if arg_str(arguments, "feedback_id").is_some() {
                let Some(backend) = backend else {
                    return no_backend(name, "采用反馈建议");
                };
                let mut body = arguments.clone();
                body["workflow_id"] = arguments
                    .get("workflow_id")
                    .or_else(|| arguments.get("task_id"))
                    .cloned()
                    .unwrap_or_default();
                return backend_result(
                    name,
                    backend.request("POST", "/api/feedback/accept", &body),
                );
            }
            let Some(task_id) = arg_str(arguments, "task_id") else {
                return ToolCallResult::err("regenerate 需要 task_id");
            };
            let Some(backend) = backend else {
                return no_backend(name, "任务重生成");
            };
            // 读取原任务输入，套用反馈调参后重新生成
            let status = backend.request(
                "POST",
                "/api/workflow",
                &serde_json::json!({"action": "status", "workflow_id": task_id}),
            );
            let Ok((code, previous)) = status else {
                return backend_result(name, status);
            };
            if code >= 400 {
                return backend_result(name, Ok((code, previous)));
            }
            let Some(input) = previous
                .pointer("/result/input")
                .and_then(|v| v.as_object())
                .cloned()
            else {
                return ToolCallResult::err(format!("任务 {task_id} 没有可重生成的输入快照"));
            };
            let mut body = input;
            for key in [
                "font_size_mm",
                "letter_spacing_mm",
                "font_path",
                "engraving_mode",
                "material",
                "thickness_mm",
                "text",
                "manual_params",
                // r44：图形、边框、文字区域与刻法也能改（给了就整组替换）
                "shapes",
                "border",
                "text_box",
                "text_fill",
            ] {
                if let Some(value) = arguments.get(key).filter(|v| !v.is_null()) {
                    body.insert(key.into(), value.clone());
                }
            }
            body.insert(
                "regenerated_from".into(),
                serde_json::Value::String(task_id.into()),
            );
            backend_result(
                name,
                backend.request(
                    "POST",
                    "/api/text-task/generate",
                    &serde_json::Value::Object(body),
                ),
            )
        }
        "laser_grbl_tool" | "laser_network_grbl_tool" => {
            let action = arg_str(arguments, "action")
                .unwrap_or("")
                .to_ascii_lowercase();
            match action.as_str() {
                "status" | "check" | "probe" => {
                    if let Some(b) = backend {
                        probe_connection_via_backend(name, arguments, b)
                    } else {
                        let args = serde_json::from_value(arguments.clone()).unwrap_or_default();
                        laser_tools::execute_check_laser_connection(&args)
                    }
                }
                "cancel" | "stop" => {
                    let Some(job_id) = arg_str(arguments, "job_id") else {
                        return ToolCallResult::err("cancel 需要 job_id（workflow_id）");
                    };
                    post(
                        "/api/workflow",
                        serde_json::json!({"action": "cancel", "workflow_id": job_id})
                            .as_object()
                            .cloned()
                            .unwrap_or_default(),
                    )
                    .unwrap_or_else(|| no_backend(name, "任务取消"))
                }
                "send_file" => ToolCallResult::err(
                    "send_file 已由安全 workflow 取代：请先用 laser_workflow_tool(action=preview) 生成并冻结快照，再 confirm_send 下发",
                ),
                "send_command" => ToolCallResult::err(
                    "send_command 不允许直接下发任意 G-code；请使用 laser_safe_action_tool 的受控动作",
                ),
                other => ToolCallResult::err(format!("{name} 不支持的 action: {other}")),
            }
        }
        other => ToolCallResult::err(format!("工具 {other} 尚未实现")),
    }
}

fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
