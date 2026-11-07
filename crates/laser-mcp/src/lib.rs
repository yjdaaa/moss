//! MCP 服务层：JSON-RPC 处理、工具分发与小智 WebSocket 桥接。
//!
//! - `HostBackend`：把需要持久化/真实设备作业的工具转发给 moss-host 的 HTTP 处理器
//!   （生产由 `laser-host` 进程内接线，测试注入假后端）；
//! - 无后端可用时，涉及 workflow / 标定 / 材料库的工具明确返回失败，绝不伪装成功；
//! - `run_bridge`：以 WebSocket 客户端连接 `MCP_ENDPOINT`（小智/MOSS 控制台生成），
//!   逐帧处理 JSON-RPC，断线指数退避重连（1s → 60s）。每个接入点一条桥（NV8 多接入点），
//!   定时发心跳，长时间收不到数据即判定断线重连；
//! - 工具开关（NV8）：后端报告为关闭的工具不出现在 `tools/list`，调用时也拒绝（停止类工具不可关闭）；
//!   每次调用按结果计入统计（`McpTelemetrySnapshot::tool_stats`）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use laser_contracts::{McpTelemetry, McpTelemetrySnapshot, global_mcp_telemetry};

pub const PROTOCOL_VERSION: &str = "2024-11-05";
pub const SERVER_NAME: &str = "YOKO_MCP_SERVER";

/// moss-host 后端调用面：`method` + `route` + JSON 请求体 → (HTTP 状态码, JSON 响应体)。
pub trait HostBackend: Send + Sync {
    fn request(
        &self,
        method: &str,
        route: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, serde_json::Value), String>;

    /// 严格模式（技术书 §6.2）。默认关闭；`laser-host` 的进程内后端返回 ApiService 的实际开关。
    fn strict_mode(&self) -> bool {
        false
    }

    /// 记一条审计（技术书 §7.3：R4 拒绝、严格模式拦截）。默认不记录；`laser-host` 写入本地数据库。
    /// 拒绝本身不依赖审计是否写入成功。
    fn audit(&self, _entry: &laser_contracts::AuditEntry) -> Result<(), String> {
        Ok(())
    }

    /// 审计记录里的调用方标签。接入点为 `mcp`；内置语音的后端返回 `voice:<设备>`（NV4）。
    fn audit_client(&self) -> String {
        "mcp".into()
    }

    /// 严格模式关闭时是否仍拒绝 R4 与未分级工具（命令执行、打开应用、快捷键等）。接入点为否（行为不变）；
    /// 内置语音为是：放宽的只是运动与出光的确认方式，桌面控制类工具始终不对内置语音开放。
    fn always_deny_r4(&self) -> bool {
        false
    }

    /// 用户是否在 MOSS 中关闭了这个工具（NV8）。关闭的工具不列出、不可调用；默认全部打开。
    fn tool_disabled(&self, _name: &str) -> bool {
        false
    }
}

pub type SharedBackend = Arc<dyn HostBackend>;

pub use laser_contracts::{ALWAYS_ENABLED_TOOLS, tool_can_be_disabled};

/// 附加接入点的数量上限（另有一个主接入点 `mcp_endpoint`）。
pub const MAX_EXTRA_ENDPOINTS: usize = 4;

pub fn tools_list_json(developer_mode: bool) -> serde_json::Value {
    let mut raw: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/golden/mcp_tools.json"))
            .expect("golden mcp_tools.json must parse");
    for tool in raw["tools"].as_array_mut().unwrap() {
        match tool["name"].as_str().unwrap_or("") {
            "refine_laser_params_from_feedback_tool" => {
                tool["description"] = serde_json::json!(
                    "关联原 task_id 判断加工反馈并保存有界建议；不发送。原任务缺少实际参数时必须显式提供功率、速度、次数和 power_scale。"
                );
                tool["inputSchema"]["properties"]["power_scale"] = serde_json::json!({
                    "type":"number", "minimum":1, "maximum":100000,
                    "description":"原任务功率标尺；只有旧任务缺少实际参数时才需要显式提供"
                });
                for key in ["laser_max_power", "feed_rate", "passes"] {
                    tool["inputSchema"]["properties"][key]
                        .as_object_mut()
                        .unwrap()
                        .remove("default");
                }
                tool["inputSchema"]["required"] = serde_json::json!(["task_id", "feedback_text"]);
            }
            "regenerate_text_laser_task_tool" => {
                tool["description"] = serde_json::json!(
                    "重新生成任务预览，不发送。采用已保存调参建议时传 task_id、feedback_id、confirmed=true；返回 adopted_workflow_id，新任务发送仍须独立确认。"
                );
                tool["inputSchema"]["properties"]["feedback_id"] = serde_json::json!({
                    "type":"string", "description":"待采用的已保存反馈 ID"
                });
                tool["inputSchema"]["oneOf"] = serde_json::json!([
                    {"not":{"required":["feedback_id"]}},
                    {"required":["task_id","feedback_id","confirmed"],"properties":{"confirmed":{"const":true},"feedback_id":{"minLength":1}}}
                ]);
            }
            "generate_text_laser_task_tool"
            | "generate_text_image_gcode_tool"
            | "ai_laser_gcode_tool"
            | "laser_workflow_tool" => {
                // 2026-11-06 文字刻法、排版与起点（用户决定：默认描边、激光头对准木牌中心）
                let props = &mut tool["inputSchema"]["properties"];
                props["text_fill"] = serde_json::json!({
                    "type": "string", "enum": ["outline", "fill", "hatch"], "default": "outline",
                    "description": "文字刻法：outline 只描轮廓（默认）；fill 扫描填满（小字、中文更清楚）；hatch 平行线填满并描边（大字更快）"
                });
                props["text_align"] = serde_json::json!({
                    "type": "string", "enum": ["left", "center", "right"], "default": "center",
                    "description": "多行文字（用换行分行）的对齐方式"
                });
                props["origin"] = serde_json::json!({
                    "type": "string", "enum": ["center", "lower_left"], "default": "center",
                    "description": "开刻前激光头对准的位置：center 木牌（图案）中心（默认），lower_left 木牌左下角"
                });
                props["width_mm"] = serde_json::json!({
                    "type": "number", "description": "文字任务：木牌宽度 mm；不给字号时自动取能放下的最大字号并居中"
                });
                props["height_mm"] = serde_json::json!({
                    "type": "number", "description": "文字任务：木牌高度 mm（可与 width_mm 一起或单独给）"
                });
                props["safe_margin_mm"] = serde_json::json!({
                    "type": "number", "default": 2, "description": "文字放进木牌时四周留边 mm"
                });
            }
            _ => {}
        }
    }
    if developer_mode {
        raw
    } else {
        let tools = raw["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["name"].as_str() != Some("command_execution_tool"))
            .cloned()
            .collect::<Vec<_>>();
        serde_json::json!({ "tools": tools })
    }
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
            let summary = if success {
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
            let detail = serde_json::to_string_pretty(&body).unwrap_or_default();
            laser_contracts::ToolCallResult::with_detail(success, summary, detail)
        }
    }
}

/// AI 生成预览后给模型的固定说明（2026-11-06 统一 AI 预览流程）：预览会弹到 MOSS 桌面，
/// 必须等用户在预览窗点“同意开刻”或（未开严格模式时）口头明确确认，才能 confirm_send。
pub const DESKTOP_PREVIEW_HINT: &str = "预览已弹到 MOSS 电脑屏幕上。请把尺寸、材料和预计时间告诉用户，让用户在预览窗核对后点「同意开刻」；未开启严格模式时，用户也可以口头明确说确认，你再调用 laser_workflow_tool(action=confirm_send, confirmed=true)。用户确认前不要发送。";

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
    laser_contracts::ToolCallResult {
        success: true,
        result: format!(
            "{}。{DESKTOP_PREVIEW_HINT}{note}",
            result.result.trim_end_matches('。')
        ),
        detail: result.detail,
    }
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
    if !body.contains_key("connection_mode") {
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

/// 严格模式下接入点实际列出的工具：去掉永不对语音开放的 R4 工具（技术书 §6.1）。
pub fn tools_list_for(developer_mode: bool, strict: bool) -> serde_json::Value {
    let raw = tools_list_json(developer_mode);
    if !strict {
        return raw;
    }
    let tools = raw["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter(|tool| {
                    !laser_contracts::tool_never_exposed(tool["name"].as_str().unwrap_or(""))
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    serde_json::json!({ "tools": tools })
}

/// 按后端的状态列出工具：严格模式去掉永不开放的工具，再去掉用户关闭的工具（NV8）。
pub fn tools_list_with(
    developer_mode: bool,
    backend: Option<&dyn HostBackend>,
) -> serde_json::Value {
    let strict = backend.is_some_and(|backend| backend.strict_mode());
    let mut list = tools_list_for(developer_mode, strict);
    if let (Some(backend), Some(tools)) = (backend, list["tools"].as_array_mut()) {
        tools.retain(|tool| !backend.tool_disabled(tool["name"].as_str().unwrap_or("")));
    }
    list
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
    global_mcp_telemetry().record_tool_call(counted, &caller, outcome);
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
            let args = serde_json::from_value(arguments.clone()).unwrap_or_default();
            laser_tools::execute_check_laser_connection(&args)
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
        "open_jsjds_website_tool" => laser_tools::execute_open_jsjds_website(),
        "shortcut_key_execution_tool" => match serde_json::from_value(arguments.clone()) {
            Ok(args) => laser_tools::execute_shortcut_key(&args),
            Err(e) => ToolCallResult::err(format!("参数校验失败: {e}")),
        },
        "generate_text_image_tool" => match serde_json::from_value(arguments.clone()) {
            Ok(args) => laser_tools::execute_generate_text_image(&args),
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
            if matches!(action, "confirm_send" | "send" | "resume") {
                if !confirmed {
                    return ToolCallResult::err("发送动作未确认 (confirmed=false)");
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
            let route = if action == "preview"
                && arg_str(arguments, "text").is_some()
                && arg_str(arguments, "gcode_file").is_none()
            {
                "/api/text-task/generate"
            } else {
                "/api/workflow"
            };
            let body = if matches!(action, "confirm_send" | "send" | "resume") {
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
        "generate_text_laser_task_tool"
        | "generate_text_image_gcode_tool"
        | "ai_laser_gcode_tool" => {
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
                    let args = serde_json::from_value(arguments.clone()).unwrap_or_default();
                    laser_tools::execute_check_laser_connection(&args)
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

pub fn handle_json_rpc(
    message: &serde_json::Value,
    developer_mode: bool,
) -> Option<serde_json::Value> {
    handle_json_rpc_with(message, developer_mode, None)
}

/// 处理一条 JSON-RPC 消息；通知（无 id）返回 None，不回包。
pub fn handle_json_rpc_with(
    message: &serde_json::Value,
    developer_mode: bool,
    backend: Option<&dyn HostBackend>,
) -> Option<serde_json::Value> {
    let method = message.get("method")?.as_str()?;
    let id = message.get("id")?;
    match method {
        "initialize" => {
            // 只记录对方真实上报的客户端信息；未上报时留空，不编造名称。
            let client_name = message
                .pointer("/params/clientInfo/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let client_ver = message
                .pointer("/params/clientInfo/version")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            global_mcp_telemetry().record_client_info(client_name, client_ver);
            global_mcp_telemetry().record_protocol_version(PROTOCOL_VERSION);
            let shown = match (client_name.is_empty(), client_ver.is_empty()) {
                (true, _) => "未上报".to_owned(),
                (false, true) => client_name.to_owned(),
                (false, false) => format!("{client_name} v{client_ver}"),
            };
            global_mcp_telemetry().record_event(&format!("[RECV] initialize (client: {shown})"));
            global_mcp_telemetry().record_event(&format!(
                "[SEND] initialize 握手响应 (protocol: {PROTOCOL_VERSION}, server: {SERVER_NAME})"
            ));
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
                    "capabilities": { "tools": { "listChanged": false } }
                }
            }))
        }
        "ping" => {
            global_mcp_telemetry().record_event("[PING] 心跳保持正常");
            Some(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} }))
        }
        "tools/list" => {
            let tools = tools_list_with(developer_mode, backend);
            let count = tools["tools"].as_array().map(Vec::len).unwrap_or(0);
            global_mcp_telemetry().record_request("tools/list (查询工具列表)");
            global_mcp_telemetry().record_event(&format!(
                "[RECV] tools/list -> 已返回 {count} 项激光数控工具定义"
            ));
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": tools
            }))
        }
        "tools/call" => {
            let Some(params) = message.get("params") else {
                return Some(rpc_error(id, -32602, "缺少 params"));
            };
            let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
                return Some(rpc_error(id, -32602, "缺少工具名 name"));
            };
            global_mcp_telemetry().record_request(&format!("tools/call: {name}"));
            global_mcp_telemetry()
                .record_event(&format!("[CALL] tools/call: {name} (正在执行受控安全操作)"));
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let result = dispatch_tool_call_with(name, &arguments, developer_mode, backend);
            let formatted_text = format_mcp_response(&result);
            global_mcp_telemetry().record_event(&format!(
                "[RESP] tools/call: {name} -> {}",
                if result.success {
                    "执行成功"
                } else {
                    "拦截/失败"
                }
            ));
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [ { "type": "text", "text": formatted_text } ],
                    "isError": !result.success
                }
            }))
        }
        _ => Some(rpc_error(id, -32601, "Method not found")),
    }
}

fn rpc_error(id: &serde_json::Value, code: i64, message: &str) -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

// ---------------------------------------------------------------------------
// WebSocket 桥接（小智 MCP_ENDPOINT）
// ---------------------------------------------------------------------------

/// 端点可用性：为空或仍是模板占位符（含 `<your-token>`）视为未配置。
pub fn endpoint_from_value(value: &str) -> Option<String> {
    let value = value.trim().to_owned();
    if value.is_empty()
        || value.contains('<')
        || !(value.starts_with("ws://") || value.starts_with("wss://"))
    {
        return None;
    }
    Some(value)
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
struct BridgeTiming {
    /// 向接入点发 WebSocket Ping 的间隔
    heartbeat: Duration,
    /// 超过这么久收不到接入点的任何数据（含 Pong）即判定断线：网络中断时 TCP 可能很久都不报错
    idle_limit: Duration,
    /// 连接保持超过这么久后断开，重连等待从头（1 秒）开始
    stable: Duration,
    first_backoff: Duration,
    max_backoff: Duration,
    /// 读超时：周期性检查退出标志、心跳与断线
    poll: Duration,
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

fn run_bridge_timed(
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
                global_mcp_telemetry().link_down(link_id, Some(&error));
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
struct LinkGuard {
    id: String,
}

impl LinkGuard {
    fn connect(id: &str, redacted_endpoint: &str) -> Self {
        global_mcp_telemetry().link_up(id, redacted_endpoint);
        global_mcp_telemetry().record_event(&format!("[LINK] 已连接接入点 {redacted_endpoint}"));
        Self { id: id.to_owned() }
    }
}

impl Drop for LinkGuard {
    fn drop(&mut self) {
        global_mcp_telemetry().link_down(&self.id, None);
        global_mcp_telemetry().record_event(&format!("[LINK] 与接入点（{}）的连接已断开", self.id));
    }
}

/// 隐去地址中的 token（`token=***`），用于日志、遥测与页面显示。
pub fn redact_endpoint(endpoint: &str) -> String {
    match endpoint.split_once("token=") {
        Some((head, _)) => format!("{head}token=***"),
        None => endpoint.to_owned(),
    }
}

/// 错误文本里如果带出了 token（个别库会回显地址），替换为 `***`。
fn scrub_token(text: &str, endpoint: &str) -> String {
    match endpoint
        .split_once("token=")
        .map(|(_, token)| token.split('&').next().unwrap_or(""))
    {
        Some(token) if !token.is_empty() => text.replace(token, "***"),
        _ => text.to_owned(),
    }
}

/// 接入点身份（NV8）：解码地址中 `token=` 的 JWT 载荷，只取标识类字段（键名以 id 结尾，或 purpose / name / sub 等）
/// 与签发、过期时间。不校验签名，只用于在语音中心显示“这是哪个智能体的接入点、何时过期”；不是 JWT 时返回 `None`。
pub fn endpoint_identity(endpoint: &str) -> Option<serde_json::Value> {
    use base64::Engine;
    let token = endpoint.split_once("token=")?.1.split('&').next()?;
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
    // 顶栏“小智”状态的来源：连接期间该接入点为已连接，函数以任何方式返回时由守卫置为断开。
    let _link = LinkGuard::connect(link_id, &redact_endpoint(endpoint));
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
                global_mcp_telemetry().record_message();
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
                            global_mcp_telemetry().record_link_client(
                                link_id,
                                &info("name"),
                                &info("version"),
                            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `global_mcp_telemetry().endpoints` 是进程级全局列表，其中 `register_endpoints`
    /// 会替换整张列表；涉及端点列表读写的测试串行执行，避免并行测试互相清空计数。
    static ENDPOINT_TELEMETRY_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn safe_tools_list_hides_developer_command() {
        let value = tools_list_json(false);
        let names = value["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 19);
        assert!(!names.contains(&"command_execution_tool"));
    }

    #[test]
    fn developer_tools_list_has_twenty_tools_matching_catalog() {
        let listed = tools_list_json(true)["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        let catalog = laser_tools::ToolCatalog::developer_mode()
            .names()
            .into_iter()
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(listed.len(), 20);
        assert_eq!(listed, catalog, "tools/list 与 ToolCatalog 必须一致");
    }

    /// 记录请求并按路由返回预设响应的假后端。
    struct FakeBackend {
        calls: Mutex<Vec<(String, String, serde_json::Value)>>,
        responses: Mutex<Vec<(u16, serde_json::Value)>>,
    }
    impl FakeBackend {
        fn new(responses: Vec<(u16, serde_json::Value)>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into_iter().rev().collect()),
            }
        }
        fn calls(&self) -> Vec<(String, String, serde_json::Value)> {
            self.calls.lock().unwrap().clone()
        }
    }
    impl HostBackend for FakeBackend {
        fn request(
            &self,
            method: &str,
            route: &str,
            body: &serde_json::Value,
        ) -> Result<(u16, serde_json::Value), String> {
            self.calls
                .lock()
                .unwrap()
                .push((method.into(), route.into(), body.clone()));
            Ok(self.responses.lock().unwrap().pop().unwrap_or((
                500,
                serde_json::json!({"success": false, "result": "no response"}),
            )))
        }
    }

    /// 严格模式开启的假后端（技术书 §6.2），记录收到的审计。
    struct StrictBackend(FakeBackend, Mutex<Vec<laser_contracts::AuditEntry>>);
    impl HostBackend for StrictBackend {
        fn request(
            &self,
            method: &str,
            route: &str,
            body: &serde_json::Value,
        ) -> Result<(u16, serde_json::Value), String> {
            self.0.request(method, route, body)
        }
        fn strict_mode(&self) -> bool {
            true
        }
        fn audit(&self, entry: &laser_contracts::AuditEntry) -> Result<(), String> {
            self.1.lock().unwrap().push(entry.clone());
            Ok(())
        }
    }

    /// P1-06：工具清单里的每个工具都必须有分级；新增工具忘了分级，这个测试就失败。
    #[test]
    fn every_listed_tool_has_a_risk_level() {
        let tools = tools_list_json(true);
        let names: Vec<String> = tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names.len(), 20);
        for name in &names {
            assert!(
                laser_contracts::tool_risk(name, &serde_json::json!({})).is_some(),
                "工具 {name} 缺少 R0–R4 分级"
            );
        }
        for name in laser_tools::ToolCatalog::developer_mode().names() {
            assert!(
                laser_contracts::tool_risk(name, &serde_json::json!({})).is_some(),
                "工具目录中的 {name} 缺少分级"
            );
        }
    }

    /// A-28：严格模式下 R4 工具（与材料库删除 / 覆盖）返回 tool_not_allowed，不执行、不触达后端；
    /// tools/list 不再列出永不开放的工具。
    #[test]
    fn strict_bridge_refuses_r4_tools() {
        let backend = StrictBackend(FakeBackend::new(vec![]), Mutex::new(Vec::new()));
        for (name, arguments) in [
            ("open_app_tool", serde_json::json!({"app_name": "notepad"})),
            (
                "shortcut_key_execution_tool",
                serde_json::json!({"keys": "ctrl+s"}),
            ),
            (
                "command_execution_tool",
                serde_json::json!({"command": "dir"}),
            ),
            ("open_jsjds_website_tool", serde_json::json!({})),
            (
                "material_params_tool",
                serde_json::json!({"action": "delete", "material": "椴木"}),
            ),
            (
                "material_params_tool",
                serde_json::json!({"action": "save", "material": "椴木"}),
            ),
            (
                "laser_grbl_tool",
                serde_json::json!({"action": "send_command", "gcode_command": "G0 X10"}),
            ),
            ("totally_unknown_tool", serde_json::json!({})),
        ] {
            let result = dispatch_tool_call_with(name, &arguments, true, Some(&backend));
            assert!(!result.success, "{name}");
            assert!(
                result
                    .detail
                    .as_deref()
                    .unwrap_or("")
                    .contains("tool_not_allowed"),
                "{name}: {result:?}"
            );
        }
        let adopt = dispatch_tool_call_with(
            "regenerate_text_laser_task_tool",
            &serde_json::json!({"task_id": "wf-1", "feedback_id": "fb-1", "confirmed": true}),
            false,
            Some(&backend),
        );
        assert!(!adopt.success);
        assert!(
            adopt
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("strict_mode_blocked")
        );
        assert!(backend.0.calls().is_empty(), "被拒绝的调用不得触达后端");
        // P1-08：每次拒绝都记审计（8 次 R4 + 1 次采用反馈）
        let audits = backend.1.lock().unwrap().clone();
        assert_eq!(audits.len(), 9);
        assert!(audits[..8].iter().all(|entry| entry.action
            == laser_contracts::AuditAction::R4Denied
            && entry.client == "mcp"
            && entry.result == laser_contracts::AuditResult::Denied));
        assert_eq!(audits[0].detail.tool.as_deref(), Some("open_app_tool"));
        assert_eq!(
            audits[8].action,
            laser_contracts::AuditAction::StrictModeBlocked
        );

        let list = handle_json_rpc_with(
            &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            true,
            Some(&backend),
        )
        .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 16, "{names:?}");
        assert!(!names.contains(&"open_app_tool") && !names.contains(&"command_execution_tool"));
        assert!(names.contains(&"laser_grbl_tool") && names.contains(&"material_params_tool"));
    }

    /// A-20 / A-25：严格模式下模型带 confirmed=true 调 confirm_send，桥接照常转发，
    /// 执行层只创建审批；工具结果是“请在电脑上确认”，不是发送成功。R2 停止类照常转发。
    #[test]
    fn strict_bridge_forwards_r3_for_approval_and_r2_unchanged() {
        let backend = StrictBackend(
            FakeBackend::new(vec![
                (
                    202,
                    serde_json::json!({"success": true, "result": {
                        "status": "approval_required", "approval_id": "apr_test", "expires_at": 1.0,
                        "summary": "发送加工任务 wf-9", "speech": "已提交，请在电脑上的 MOSS 里确认后开始。"
                    }}),
                ),
                (
                    200,
                    serde_json::json!({"success": true, "result": "急停 0x18 已发送"}),
                ),
            ]),
            Mutex::new(Vec::new()),
        );
        let send = dispatch_tool_call_with(
            "laser_workflow_tool",
            &serde_json::json!({"action": "confirm_send", "workflow_id": "wf-9", "confirmed": true, "network_host": "10.0.0.5"}),
            false,
            Some(&backend),
        );
        assert!(send.success, "{send:?}");
        assert!(send.result.contains("确认"), "{send:?}");
        assert!(
            send.detail
                .as_deref()
                .unwrap_or("")
                .contains("approval_required")
        );
        let stop = dispatch_tool_call_with(
            "laser_safe_action_tool",
            &serde_json::json!({"action": "emergency_stop", "confirmed": true}),
            false,
            Some(&backend),
        );
        assert!(stop.success, "{stop:?}");
        let calls = backend.0.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1, "/api/workflow");
        assert_eq!(calls[1].1, "/api/safe-action");
    }

    /// A-40：严格模式关闭时分级不改变任何行为（材料库保存照常转发）。
    #[test]
    fn non_strict_bridge_behavior_is_unchanged() {
        let backend = FakeBackend::new(vec![(
            200,
            serde_json::json!({"success": true, "result": {"materials": {}}}),
        )]);
        let saved = dispatch_tool_call_with(
            "material_params_tool",
            &serde_json::json!({"action": "save", "material": "椴木", "thickness_mm": 3, "laser_max_power": 500, "feed_rate": 1200}),
            false,
            Some(&backend),
        );
        assert!(saved.success, "{saved:?}");
        assert_eq!(backend.calls()[0].1, "/api/material-lab/manage");
        let list = handle_json_rpc_with(
            &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            false,
            Some(&backend),
        )
        .unwrap();
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 19);
    }

    #[test]
    fn json_rpc_handles_initialize_tools_list_notifications_and_errors() {
        let init = handle_json_rpc(
            &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
            false,
        )
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(
            global_mcp_telemetry().snapshot().protocol_version,
            PROTOCOL_VERSION,
            "遥测记录实际应答的协议版本"
        );
        let list = handle_json_rpc(
            &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            false,
        )
        .unwrap();
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 19);
        assert!(
            handle_json_rpc(
                &serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                false
            )
            .is_none()
        );
        let ping = handle_json_rpc(
            &serde_json::json!({"jsonrpc":"2.0","id":3,"method":"ping"}),
            false,
        )
        .unwrap();
        assert_eq!(ping["result"], serde_json::json!({}));
        let unknown = handle_json_rpc(
            &serde_json::json!({"jsonrpc":"2.0","id":4,"method":"nope"}),
            false,
        )
        .unwrap();
        assert_eq!(unknown["error"]["code"], -32601);

        let call = handle_json_rpc(&serde_json::json!({
            "jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"recommend_laser_params_tool","arguments":{"material":"椴木","thickness_mm":3.0}}
        }), false).unwrap();
        assert_eq!(call["result"]["isError"], false);
        assert!(
            call["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("推荐参数")
        );
    }

    #[test]
    fn unimplemented_and_backendless_tools_report_errors_instead_of_fake_success() {
        for name in [
            "laser_workflow_tool",
            "run_calibration_grid_tool",
            "material_params_tool",
            "start_tuned_job_tool",
            "generate_text_laser_task_tool",
            "totally_unknown_tool",
        ] {
            let args = serde_json::json!({"action":"status","material":"椴木","thickness_mm":3,"text":"hi","task_id":"wf-1","confirmed":true,"workflow_id":"wf-1"});
            let result = dispatch_tool_call(name, &args, false);
            assert!(!result.success, "{name} 无后端时必须失败: {result:?}");
        }
        let rpc = handle_json_rpc(&serde_json::json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"laser_grbl_tool","arguments":{"action":"send_command","gcode_command":"G0 X10"}}}), false).unwrap();
        assert_eq!(rpc["result"]["isError"], true);
    }

    #[test]
    fn workflow_and_calibration_tools_forward_to_host_backend() {
        // confirm_send → POST /api/workflow，携带连接字段
        let backend = FakeBackend::new(vec![(
            200,
            serde_json::json!({"success": true, "result": {"workflow_id": "wf-9", "status": "sending", "send_result": {"lines_sent": 0, "total_lines": 120}}}),
        )]);
        let result = dispatch_tool_call_with(
            "laser_workflow_tool",
            &serde_json::json!({"action":"confirm_send","workflow_id":"wf-9","confirmed":true,"network_host":"10.0.0.5","network_telnet_port":23}),
            false,
            Some(&backend),
        );
        assert!(result.success, "{result:?}");
        assert!(result.result.contains("wf-9") && result.result.contains("sending"));
        let calls = backend.calls();
        assert_eq!(calls[0].1, "/api/workflow");
        assert_eq!(calls[0].2["network_host"], "10.0.0.5");
        assert_eq!(calls[0].2["connection_mode"], "network");

        // 未确认 confirm_send 不触达后端
        let backend = FakeBackend::new(vec![]);
        let refused = dispatch_tool_call_with(
            "laser_workflow_tool",
            &serde_json::json!({"action":"confirm_send","workflow_id":"wf-9"}),
            false,
            Some(&backend),
        );
        assert!(!refused.success);
        assert!(backend.calls().is_empty());

        // 标定：无 calibration_id → preview；有 id + confirmed → send
        let backend = FakeBackend::new(vec![
            (
                200,
                serde_json::json!({"success": true, "result": {"calibration_id": "cal-1", "status": "prepared", "cell_count": 25}}),
            ),
            (
                200,
                serde_json::json!({"success": true, "result": {"calibration_id": "cal-1", "status": "sending", "send_result": {"lines_sent": 0, "total_lines": 900}}}),
            ),
        ]);
        let preview = dispatch_tool_call_with(
            "run_calibration_grid_tool",
            &serde_json::json!({"material":"椴木","thickness_mm":3,"rows":5,"columns":5}),
            false,
            Some(&backend),
        );
        assert!(
            preview.success && preview.result.contains("cal-1"),
            "{preview:?}"
        );
        let send = dispatch_tool_call_with(
            "run_calibration_grid_tool",
            &serde_json::json!({"calibration_id":"cal-1","confirmed":true,"network_host":"10.0.0.5"}),
            false,
            Some(&backend),
        );
        assert!(send.success && send.result.contains("900"), "{send:?}");
        let calls = backend.calls();
        assert_eq!(calls[0].1, "/api/material-lab/preview");
        assert_eq!(calls[1].1, "/api/material-lab/send");
        assert_eq!(calls[1].2["confirmed"], true);

        // 后端返回失败 → 工具失败并透传文案
        let backend = FakeBackend::new(vec![(
            400,
            serde_json::json!({"success": false, "result": "发送安全门禁未通过: SHA-256 不一致"}),
        )]);
        let gated = dispatch_tool_call_with(
            "select_calibration_cell_tool",
            &serde_json::json!({"calibration_id":"cal-1","cell_number":6}),
            false,
            Some(&backend),
        );
        assert!(!gated.success);
        assert!(gated.result.contains("SHA-256"));

        // material_params get → GET recommendation；save → manage
        let backend = FakeBackend::new(vec![
            (
                200,
                serde_json::json!({"success": true, "result": {"material": "椴木", "laser_max_power": 420}}),
            ),
            (
                200,
                serde_json::json!({"success": true, "result": {"materials": {}}}),
            ),
        ]);
        let got = dispatch_tool_call_with(
            "material_params_tool",
            &serde_json::json!({"action":"get","material":"椴木","thickness_mm":3}),
            false,
            Some(&backend),
        );
        assert!(got.success);
        let saved = dispatch_tool_call_with(
            "material_params_tool",
            &serde_json::json!({"action":"save","material":"椴木","thickness_mm":3,"laser_max_power":500,"feed_rate":1200}),
            false,
            Some(&backend),
        );
        assert!(saved.success);
        let calls = backend.calls();
        assert_eq!(calls[0].0, "GET");
        assert!(
            calls[0]
                .1
                .starts_with("/api/ui/material-recommendation?material=%E6%A4%B4%E6%9C%A8")
        );
        assert_eq!(calls[1].1, "/api/material-lab/manage");
        assert_eq!(calls[1].2["action"], "save");
    }

    #[test]
    fn feedback_uses_shared_backend_without_silent_local_defaults() {
        let args = serde_json::json!({"task_id":"wf-1","feedback_text":"有点太焦了"});
        assert!(
            !dispatch_tool_call("refine_laser_params_from_feedback_tool", &args, false).success
        );
        let backend = FakeBackend::new(vec![(
            400,
            serde_json::json!({"success":false,"result":"原任务参数缺失"}),
        )]);
        assert!(
            !dispatch_tool_call_with(
                "refine_laser_params_from_feedback_tool",
                &args,
                false,
                Some(&backend)
            )
            .success
        );
        assert_eq!(backend.calls()[0].1, "/api/feedback");
        assert_eq!(backend.calls()[0].2, args);
        let tools = tools_list_json(false);
        let refine = tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "refine_laser_params_from_feedback_tool")
            .unwrap();
        assert!(
            refine["inputSchema"]["properties"]["laser_max_power"]
                .get("default")
                .is_none()
        );
        assert_eq!(
            refine["inputSchema"]["required"],
            serde_json::json!(["task_id", "feedback_text"])
        );
    }

    #[test]
    fn feedback_adoption_only_requests_new_preview() {
        let backend = FakeBackend::new(vec![(
            200,
            serde_json::json!({"success":true,"result":{"adopted_workflow_id":"wf-2"}}),
        )]);
        let result = dispatch_tool_call_with(
            "regenerate_text_laser_task_tool",
            &serde_json::json!({
                "task_id":"wf-1","feedback_id":"fb-1","confirmed":true
            }),
            false,
            Some(&backend),
        );
        assert!(result.success);
        assert_eq!(backend.calls().len(), 1);
        assert_eq!(backend.calls()[0].1, "/api/feedback/accept");
        assert_eq!(backend.calls()[0].2["workflow_id"], "wf-1");
    }

    #[test]
    fn endpoint_placeholder_is_treated_as_unconfigured() {
        assert_eq!(
            redact_endpoint("wss://api.example/mcp/?token=abc"),
            "wss://api.example/mcp/?token=***"
        );
        // 直接测试判定逻辑（不依赖进程环境变量）
        for bad in [
            "",
            "wss://api.xiaozhi.me/mcp/?token=<your-token>",
            "http://x",
        ] {
            let value = bad.trim();
            let invalid = value.is_empty()
                || value.contains('<')
                || !(value.starts_with("ws://") || value.starts_with("wss://"));
            assert!(invalid, "{bad}");
        }
    }

    /// 本地 WebSocket 服务端：接收 initialize / tools/call，校验桥接真实往返，然后关闭。
    #[test]
    fn bridge_round_trips_json_rpc_over_loopback_websocket() {
        let _telemetry_lock = ENDPOINT_TELEMETRY_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.send(tungstenite::Message::Text(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#.into(),
            ))
            .unwrap();
            let init: serde_json::Value =
                serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            ws.send(tungstenite::Message::Text(
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.into(),
            ))
            .unwrap();
            ws.send(tungstenite::Message::Text(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"laser_workflow_tool","arguments":{"action":"status","workflow_id":"wf-1"}}}"#.into())).unwrap();
            let call: serde_json::Value =
                serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            // 桥连接期间遥测必须显示已连接（总状态与本接入点）
            let snapshot = global_mcp_telemetry().snapshot();
            let link = snapshot
                .endpoints
                .iter()
                .find(|link| link.id == "test-roundtrip")
                .cloned();
            let connected_while_open =
                snapshot.connected && link.as_ref().is_some_and(|link| link.connected);
            let _ = ws.close(None);
            (init, call, connected_while_open)
        });
        let backend: SharedBackend = Arc::new(FakeBackend::new(vec![(
            200,
            serde_json::json!({"success": true, "result": {"workflow_id": "wf-1", "status": "completed"}}),
        )]));
        let shutdown = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(BridgeStats::default());
        let endpoint = format!("ws://127.0.0.1:{port}/mcp/?token=test");
        let shutdown_for_bridge = shutdown.clone();
        let stats_for_bridge = stats.clone();
        let bridge = std::thread::spawn(move || {
            run_bridge(
                "test-roundtrip",
                &endpoint,
                Some(backend),
                false,
                shutdown_for_bridge,
                stats_for_bridge,
            )
        });
        let (init, call, connected_while_open) = server.join().unwrap();
        shutdown.store(true, Ordering::SeqCst);
        bridge.join().unwrap();
        assert!(
            connected_while_open,
            "连接期间 telemetry.connected 必须为真"
        );
        // 其他测试的桥可能同时在线，这里只看本接入点
        let after = global_mcp_telemetry().snapshot();
        let link = after
            .endpoints
            .iter()
            .find(|link| link.id == "test-roundtrip")
            .unwrap();
        assert!(!link.connected, "桥退出后本接入点必须为断开");
        assert_eq!(
            link.endpoint,
            format!("ws://127.0.0.1:{port}/mcp/?token=***"),
            "遥测里的地址必须脱敏"
        );
        assert_eq!(init["result"]["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(call["id"], 2);
        assert_eq!(call["result"]["isError"], false);
        assert!(
            call["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("completed")
        );
        assert_eq!(stats.connections.load(Ordering::Relaxed), 1);
        assert_eq!(stats.messages_in.load(Ordering::Relaxed), 3);
        assert_eq!(stats.messages_out.load(Ordering::Relaxed), 2);
    }

    /// 关闭了部分工具的假后端（NV8）。
    struct SwitchBackend {
        inner: FakeBackend,
        disabled: Vec<&'static str>,
    }
    impl HostBackend for SwitchBackend {
        fn request(
            &self,
            method: &str,
            route: &str,
            body: &serde_json::Value,
        ) -> Result<(u16, serde_json::Value), String> {
            self.inner.request(method, route, body)
        }
        fn tool_disabled(&self, name: &str) -> bool {
            self.disabled.contains(&name)
        }
        fn audit_client(&self) -> String {
            "mcp-switch-test".into()
        }
    }

    /// NV8：关闭的工具不出现在 tools/list，调用被拒绝且不触达后端；停止类工具即使后端报告关闭也照常可用；
    /// 每次调用按结果计入统计。
    #[test]
    fn disabled_tools_are_neither_listed_nor_callable() {
        let backend = SwitchBackend {
            inner: FakeBackend::new(vec![
                (
                    200,
                    serde_json::json!({"success": true, "result": "急停 0x18 已发送"}),
                ),
                (
                    200,
                    serde_json::json!({"success": true, "result": {"materials": {}}}),
                ),
            ]),
            disabled: vec!["material_params_tool", "laser_safe_action_tool"],
        };
        let list = handle_json_rpc_with(
            &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            false,
            Some(&backend),
        )
        .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 17, "{names:?}");
        assert!(
            !names.contains(&"material_params_tool") && !names.contains(&"laser_safe_action_tool")
        );

        let before = global_mcp_telemetry()
            .snapshot()
            .tool_stats
            .get("material_params_tool")
            .cloned()
            .unwrap_or_default();
        let call = handle_json_rpc_with(
            &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"material_params_tool","arguments":{"action":"list"}}}),
            false,
            Some(&backend),
        )
        .unwrap();
        assert_eq!(call["result"]["isError"], true);
        assert!(
            call["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("tool_disabled")
        );
        assert!(backend.inner.calls().is_empty(), "关闭的工具不得触达后端");
        let after = global_mcp_telemetry().snapshot().tool_stats["material_params_tool"].clone();
        assert_eq!(after.calls - before.calls, 1);
        assert_eq!(after.denied - before.denied, 1);
        assert_eq!(after.failures, before.failures, "被开关拒绝不算失败");

        // 停止类工具不能关闭：即使后端报告关闭，调用照常转发
        assert!(!tool_can_be_disabled("laser_safe_action_tool"));
        let stop = dispatch_tool_call_with(
            "laser_safe_action_tool",
            &serde_json::json!({"action": "emergency_stop"}),
            false,
            Some(&backend),
        );
        assert!(stop.success, "{stop:?}");
        assert_eq!(backend.inner.calls()[0].1, "/api/safe-action");
        let stat = global_mcp_telemetry().snapshot().tool_stats["laser_safe_action_tool"].clone();
        assert!(stat.calls >= 1);

        // 未知工具名统一计入“未知工具”，不按名字新增条目（其他测试也会计入，只比较本次的增量）
        let unknown =
            |stats: &std::collections::BTreeMap<String, laser_contracts::ToolCallStat>| {
                stats
                    .get("（未知工具）")
                    .map(|stat| stat.calls + stat.failures)
                    .unwrap_or(0)
            };
        let before = unknown(&global_mcp_telemetry().snapshot().tool_stats);
        let made_up = dispatch_tool_call_with(
            "made_up_tool_for_stats",
            &serde_json::json!({}),
            false,
            Some(&backend),
        );
        assert!(!made_up.success);
        let stats = global_mcp_telemetry().snapshot().tool_stats;
        assert!(!stats.contains_key("made_up_tool_for_stats"));
        assert!(
            unknown(&stats) >= before + 2,
            "未知工具计一次调用、一次失败"
        );
    }

    /// NV8：桥定时发 Ping；对方长时间没有任何数据（连 Pong 都没有）时判定断线并自动重连，
    /// 断线原因记入该接入点的状态。
    #[test]
    fn bridge_sends_heartbeats_and_reconnects_when_the_endpoint_goes_silent() {
        let _telemetry_lock = ENDPOINT_TELEMETRY_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut first = tungstenite::accept(stream).unwrap();
            let heartbeat = first.read().unwrap();
            // 收到 Ping 后不再读写：Pong 不会发出，桥应在空闲上限后断开并重连
            let (stream, _) = listener.accept().unwrap();
            let second = tungstenite::accept(stream).unwrap();
            (heartbeat.is_ping(), first, second)
        });
        let shutdown = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(BridgeStats::default());
        let timing = BridgeTiming {
            heartbeat: Duration::from_millis(200),
            idle_limit: Duration::from_secs(1),
            stable: Duration::from_secs(60),
            first_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_secs(1),
            poll: Duration::from_millis(100),
        };
        let endpoint = format!("ws://127.0.0.1:{port}/mcp/?token=heartbeat-secret");
        let (shutdown_for_bridge, stats_for_bridge) = (shutdown.clone(), stats.clone());
        let bridge = std::thread::spawn(move || {
            run_bridge_timed(
                "test-heartbeat",
                &endpoint,
                None,
                false,
                &shutdown_for_bridge,
                &stats_for_bridge,
                timing,
            )
        });
        let (got_ping, _first, _second) = server.join().unwrap();
        assert!(got_ping, "连接后应在心跳间隔内收到 Ping");
        // 第二次连接已建立，说明空闲判定触发了重连；此时上一次的断线原因已记录过
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while stats.connections.load(Ordering::Relaxed) < 2 && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(stats.connections.load(Ordering::Relaxed), 2);
        let events = global_mcp_telemetry().snapshot().recent_events.join("\n");
        assert!(events.contains("（test-heartbeat）"), "{events}");
        shutdown.store(true, Ordering::SeqCst);
        bridge.join().unwrap();
        let link = global_mcp_telemetry()
            .snapshot()
            .endpoints
            .into_iter()
            .find(|link| link.id == "test-heartbeat")
            .unwrap();
        assert!(!link.connected);
        assert_eq!(link.connections, 2);
        assert!(!link.endpoint.contains("heartbeat-secret"));
    }

    /// 连接失败的原因记入接入点状态，且不含 token。
    #[test]
    fn connection_failures_are_recorded_without_the_token() {
        let _telemetry_lock = ENDPOINT_TELEMETRY_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        use std::net::TcpListener;
        // 先占用再释放一个端口，得到一个大概率没人监听的地址
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(BridgeStats::default());
        let timing = BridgeTiming {
            first_backoff: Duration::from_millis(50),
            ..BridgeTiming::default()
        };
        global_mcp_telemetry().register_endpoints(&[(
            "test-refused".into(),
            format!("ws://127.0.0.1:{port}/mcp/?token=***"),
        )]);
        let endpoint = format!("ws://127.0.0.1:{port}/mcp/?token=refused-secret");
        let (shutdown_for_bridge, stats_for_bridge) = (shutdown.clone(), stats.clone());
        let bridge = std::thread::spawn(move || {
            run_bridge_timed(
                "test-refused",
                &endpoint,
                None,
                false,
                &shutdown_for_bridge,
                &stats_for_bridge,
                timing,
            )
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut error = None;
        while error.is_none() && std::time::Instant::now() < deadline {
            error = global_mcp_telemetry()
                .snapshot()
                .endpoints
                .into_iter()
                .find(|link| link.id == "test-refused")
                .and_then(|link| link.last_error);
            std::thread::sleep(Duration::from_millis(20));
        }
        shutdown.store(true, Ordering::SeqCst);
        bridge.join().unwrap();
        let error = error.expect("连接失败应记录原因");
        assert!(!error.contains("refused-secret"), "{error}");
        assert_eq!(stats.connections.load(Ordering::Relaxed), 0);
        assert_eq!(
            scrub_token("bad url wss://x/?token=abc&k=1", "wss://x/?token=abc&k=1"),
            "bad url wss://x/?token=***&k=1"
        );
    }

    /// NV8：接入点身份只取标识类字段与有效期，不是 JWT 的地址返回 None。
    #[test]
    fn endpoint_identity_reads_only_identity_fields_from_the_token() {
        use base64::Engine;
        let encode = |value: serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
        };
        let header = encode(serde_json::json!({"alg": "HS256", "typ": "JWT"}));
        let payload = encode(serde_json::json!({
            "userId": 42, "agentId": "agent-7", "endpointId": "ep_1", "purpose": "mcp-endpoint",
            "iat": 1_700_000_000, "exp": 1_700_000_100, "role": "admin", "nested": {"id": 1}
        }));
        let endpoint =
            format!("wss://api.example.test/mcp/?token={header}.{payload}.signature&x=1");
        let identity = endpoint_identity(&endpoint).unwrap();
        let fields = identity["fields"].as_object().unwrap();
        assert_eq!(fields["agentId"], "agent-7");
        assert_eq!(fields["userId"], "42");
        assert_eq!(fields["endpointId"], "ep_1");
        assert_eq!(fields["purpose"], "mcp-endpoint");
        assert!(
            !fields.contains_key("role") && !fields.contains_key("nested"),
            "{fields:?}"
        );
        assert_eq!(identity["expires_at"], 1_700_000_100.0);
        assert_eq!(identity["expired"], true);
        for endpoint in [
            "wss://api.example.test/mcp/?token=plain-token",
            "wss://api.example.test/mcp/",
            "wss://x/?token=a.%%%.c",
        ] {
            assert!(endpoint_identity(endpoint).is_none(), "{endpoint}");
        }
    }

    #[test]
    fn disabled_gate_never_blocks_r2_workflow_cancel() {
        let backend = SwitchBackend {
            inner: FakeBackend::new(vec![(
                200,
                serde_json::json!({"success": true, "result": {"workflow_id": "wf-9", "status": "cancelled"}}),
            )]),
            disabled: vec!["laser_workflow_tool"],
        };
        // 非停止类调用被关闭开关拦截
        let blocked = dispatch_tool_call_with(
            "laser_workflow_tool",
            &serde_json::json!({"action": "status", "workflow_id": "wf-9"}),
            false,
            Some(&backend),
        );
        assert!(!blocked.success);
        assert!(
            blocked
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("tool_disabled")
        );
        assert!(backend.inner.calls().is_empty());

        // R2 取消动作即使 laser_workflow_tool 被关闭也照常放行
        let cancel = dispatch_tool_call_with(
            "laser_workflow_tool",
            &serde_json::json!({"action": "cancel", "workflow_id": "wf-9"}),
            false,
            Some(&backend),
        );
        assert!(cancel.success, "{cancel:?}");
        let calls = backend.inner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, "/api/workflow");
        assert_eq!(calls[0].2["action"], "cancel");
    }

    #[test]
    fn recommend_forwards_to_backend_and_resume_delete_enforce_confirmed() {
        let backend = FakeBackend::new(vec![
            (
                200,
                serde_json::json!({
                    "success": true,
                    "result": {
                        "material": "特制亚克力",
                        "thickness_mm": 4.0,
                        "matched_thickness_mm": 4.0,
                        "laser_mode": "cut",
                        "laser_max_power": 230,
                        "feed_rate": 240,
                        "passes": 2
                    }
                }),
            ),
            (
                200,
                serde_json::json!({"success": true, "result": {"workflow_id": "wf-2", "status": "sending"}}),
            ),
            (
                400,
                serde_json::json!({"success": false, "result": "删除当前参数需要 confirmed=true"}),
            ),
        ]);
        let rec = dispatch_tool_call_with(
            "recommend_laser_params_tool",
            &serde_json::json!({
                "material": "特制亚克力",
                "thickness_mm": 4.0,
                "laser_mode": "cut",
                "power_scale": 255.0
            }),
            false,
            Some(&backend),
        );
        assert!(rec.success, "{rec:?}");
        assert!(
            rec.result.contains("推荐参数") && rec.result.contains("特制亚克力"),
            "{rec:?}"
        );

        // 未确认 resume 被拦截，不触达后端
        let unconfirmed_resume = dispatch_tool_call_with(
            "laser_workflow_tool",
            &serde_json::json!({"action": "resume", "workflow_id": "wf-2"}),
            false,
            Some(&backend),
        );
        assert!(!unconfirmed_resume.success);
        assert_eq!(backend.calls().len(), 1);

        // 确认后的 resume 携带连接参数转发
        let ok_resume = dispatch_tool_call_with(
            "laser_workflow_tool",
            &serde_json::json!({"action": "resume", "workflow_id": "wf-2", "confirmed": true, "host": "10.0.0.8"}),
            false,
            Some(&backend),
        );
        assert!(ok_resume.success, "{ok_resume:?}");

        // delete 未传 confirmed 时不得自动注入 confirmed=true
        let _ = dispatch_tool_call_with(
            "material_params_tool",
            &serde_json::json!({"action": "delete", "material": "特制亚克力"}),
            false,
            Some(&backend),
        );
        let calls = backend.calls();
        assert_eq!(calls.len(), 3);
        assert!(calls[0].1.contains("power_scale=255"));
        assert_eq!(calls[1].2["network_host"], "10.0.0.8");
        assert!(calls[2].2.get("confirmed").is_none());
    }

    /// 2026-11-06：AI 生成文字任务后只停在预览，`confirmed + send_after_generate` 不再顺带发送，
    /// 结果提示模型等用户在电脑预览窗确认。
    #[test]
    fn ai_generation_stops_at_the_desktop_preview_even_when_asked_to_send() {
        let ok = (
            200,
            serde_json::json!({"success": true, "result": {"workflow_id": "wf-ai", "status": "preview_ready"}}),
        );
        let backend = FakeBackend::new(vec![ok.clone(), ok.clone(), ok.clone(), ok]);
        for name in [
            "ai_laser_gcode_tool",
            "generate_text_laser_task_tool",
            "generate_text_image_gcode_tool",
        ] {
            let result = dispatch_tool_call_with(
                name,
                &serde_json::json!({"text": "生日快乐", "material": "椴木", "thickness_mm": 3, "confirmed": true, "send_after_generate": true, "host": "10.0.0.8"}),
                false,
                Some(&backend),
            );
            assert!(result.success, "{result:?}");
            assert!(
                result.result.contains("同意开刻") && result.result.contains("没有直接开始加工"),
                "{}",
                result.result
            );
        }
        let preview = dispatch_tool_call_with(
            "laser_workflow_tool",
            &serde_json::json!({"action": "preview", "text": "MOSS", "material": "椴木", "thickness_mm": 3}),
            false,
            Some(&backend),
        );
        assert!(preview.result.contains("同意开刻"), "{}", preview.result);
        let calls = backend.calls();
        assert_eq!(calls.len(), 4);
        assert!(
            calls.iter().all(|call| call.1 == "/api/text-task/generate"),
            "{calls:?}"
        );
    }
}
