//! MCP 服务层：JSON-RPC 处理、工具分发与小智 WebSocket 桥接。
//!
//! - `HostBackend`：把需要持久化/真实设备作业的工具转发给 moss-host 的 HTTP 处理器
//!   （生产由 `laser-host` 进程内接线，测试注入假后端）；
//! - 无后端可用时，涉及 workflow / 标定 / 材料库的工具明确返回失败，绝不伪装成功；
//! - `run_bridge`：以 WebSocket 客户端连接 `MCP_ENDPOINT`（小智/MOSS 控制台生成），
//!   逐帧处理 JSON-RPC，断线指数退避重连（1s → 60s）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use laser_contracts::{global_mcp_telemetry, McpTelemetry, McpTelemetrySnapshot};

pub const PROTOCOL_VERSION: &str = "2024-11-05";
pub const SERVER_NAME: &str = "YOKO_MCP_SERVER";

/// moss-host 后端调用面：`method` + `route` + JSON 请求体 → (HTTP 状态码, JSON 响应体)。
pub trait HostBackend: Send + Sync {
    fn request(&self, method: &str, route: &str, body: &serde_json::Value) -> Result<(u16, serde_json::Value), String>;
}

pub type SharedBackend = Arc<dyn HostBackend>;

pub fn tools_list_json(developer_mode: bool) -> serde_json::Value {
    let raw: serde_json::Value = serde_json::from_str(include_str!("../../../tests/golden/mcp_tools.json"))
        .expect("golden mcp_tools.json must parse");
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
    args.get(key).and_then(serde_json::Value::as_str).map(str::trim).filter(|v| !v.is_empty())
}

fn arg_bool(args: &serde_json::Value, key: &str) -> bool {
    match args.get(key) {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes"),
        Some(serde_json::Value::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0),
        _ => false,
    }
}

fn no_backend(tool: &str, what: &str) -> laser_contracts::ToolCallResult {
    laser_contracts::ToolCallResult::err(format!("{tool} 需要 moss-host 后端执行{what}，当前 MCP 会话未连接 host"))
}

/// 把后端 JSON 响应折叠为工具结果：`success` 字段决定成败，`speech`/`result` 作摘要，全文放 detail。
fn backend_result(tool: &str, response: Result<(u16, serde_json::Value), String>) -> laser_contracts::ToolCallResult {
    match response {
        Err(error) => laser_contracts::ToolCallResult::err(format!("{tool} 调用 moss-host 失败: {error}")),
        Ok((status, body)) => {
            let success = body.get("success").and_then(|v| v.as_bool()).unwrap_or(status < 400);
            let result = body.get("result").cloned().unwrap_or(serde_json::Value::Null);
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
    if let Some(send) = result.get("send_result") {
        if let (Some(sent), Some(total)) = (send.get("lines_sent").and_then(|v| v.as_u64()), send.get("total_lines").and_then(|v| v.as_u64())) {
            parts.push(format!("已下发 {sent}/{total} 行"));
        }
    }
    if let Some(cells) = result.get("cell_count").and_then(|v| v.as_u64()) {
        parts.push(format!("{cells} 个标定单元格"));
    }
    parts.join("，")
}

/// 从工具参数提取连接字段并合并到后端请求体（工具参数名 host/port/telnet_port 与 HTTP 契约对齐）。
fn with_connection_fields(args: &serde_json::Value, mut body: serde_json::Map<String, serde_json::Value>) -> serde_json::Map<String, serde_json::Value> {
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
        let mode = if body.contains_key("port") && !body.contains_key("network_host") { "serial" } else { "network" };
        body.insert("connection_mode".into(), serde_json::Value::String(mode.into()));
    }
    body
}

/// 有界调参建议（原 Python `refine_laser_params_from_feedback` 的规则移植）：不生成文件、不碰设备。
fn refine_from_feedback(args: &serde_json::Value) -> laser_contracts::ToolCallResult {
    let Some(feedback) = arg_str(args, "feedback_text") else {
        return laser_contracts::ToolCallResult::err("feedback_text 不能为空");
    };
    let power = args.get("laser_max_power").and_then(|v| v.as_i64()).unwrap_or(400);
    let feed = args.get("feed_rate").and_then(|v| v.as_i64()).unwrap_or(1500);
    let passes = args.get("passes").and_then(|v| v.as_i64()).unwrap_or(1);
    let text = feedback.to_lowercase();
    let (power_scale, feed_scale, pass_delta, reason) = if text.contains("焦") || text.contains("糊") || text.contains("烧") || text.contains("过深") {
        (0.85, 1.15, 0, "结果过焦/过深：降功率并提速")
    } else if text.contains("浅") || text.contains("淡") || text.contains("看不清") {
        (1.15, 0.9, 0, "结果过浅：加功率并降速")
    } else if text.contains("切不透") || text.contains("没切透") || text.contains("未切透") {
        (1.1, 0.85, 1, "切不透：加功率、降速并多走一遍")
    } else if text.contains("毛边") || text.contains("锯齿") {
        (0.95, 0.9, 0, "毛边：略降功率并降速")
    } else if text.contains("断线") || text.contains("断续") {
        (1.05, 0.85, 0, "断线：略加功率并降速")
    } else {
        return laser_contracts::ToolCallResult::err(format!("无法从反馈中识别调参方向: {feedback}（支持：太焦/太浅/切不透/毛边/断线）"));
    };
    let new_power = ((power as f64) * power_scale).round().clamp(1.0, 1000.0) as i64;
    let new_feed = ((feed as f64) * feed_scale).round().clamp(100.0, 6000.0) as i64;
    let new_passes = (passes + pass_delta).clamp(1, 5);
    laser_contracts::ToolCallResult::with_detail(
        true,
        format!("{reason}：功率 {power}→{new_power}，速度 {feed}→{new_feed}，次数 {passes}→{new_passes}"),
        serde_json::json!({"laser_max_power": new_power, "feed_rate": new_feed, "passes": new_passes, "reason": reason}).to_string(),
    )
}

pub fn dispatch_tool_call(
    name: &str,
    arguments: &serde_json::Value,
    developer_mode: bool,
) -> laser_contracts::ToolCallResult {
    dispatch_tool_call_with(name, arguments, developer_mode, None)
}

pub fn dispatch_tool_call_with(
    name: &str,
    arguments: &serde_json::Value,
    developer_mode: bool,
    backend: Option<&dyn HostBackend>,
) -> laser_contracts::ToolCallResult {
    use laser_contracts::ToolCallResult;
    let post = |route: &str, body: serde_json::Map<String, serde_json::Value>| -> Option<ToolCallResult> {
        backend.map(|b| backend_result(name, b.request("POST", route, &serde_json::Value::Object(body))))
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
        "recommend_laser_params_tool" => match serde_json::from_value(arguments.clone()) {
            Ok(args) => laser_tools::execute_recommend_laser_params(&args),
            Err(e) => ToolCallResult::err(format!("参数校验失败: {e}")),
        },
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
        "refine_laser_params_from_feedback_tool" => refine_from_feedback(arguments),

        // ---- 以下工具需要 host 后端（持久化 / 真实作业）----
        "laser_workflow_tool" => {
            let action = arg_str(arguments, "action").unwrap_or("status");
            let workflow_id = arg_str(arguments, "workflow_id");
            let confirmed = arg_bool(arguments, "confirmed");
            if action == "confirm_send" {
                if !confirmed {
                    return ToolCallResult::err("发送动作未确认 (confirmed=false)");
                }
                if workflow_id.is_none() {
                    return ToolCallResult::err("confirm_send 缺少 workflow_id");
                }
            }
            let Some(backend) = backend else {
                return laser_tools::execute_laser_workflow(Some(action), workflow_id, confirmed);
            };
            let mut body = arguments.as_object().cloned().unwrap_or_default();
            body.insert("action".into(), serde_json::Value::String(action.into()));
            let route = if action == "preview" && arg_str(arguments, "text").is_some() && arg_str(arguments, "gcode_file").is_none() {
                "/api/text-task/generate"
            } else {
                "/api/workflow"
            };
            let body = if action == "confirm_send" { with_connection_fields(arguments, body) } else { body };
            backend_result(name, backend.request("POST", route, &serde_json::Value::Object(body)))
        }
        "run_calibration_grid_tool" => {
            let calibration_id = arg_str(arguments, "calibration_id");
            let confirmed = arg_bool(arguments, "confirmed");
            let mut body = arguments.as_object().cloned().unwrap_or_default();
            if let Some(id) = calibration_id.filter(|_| confirmed) {
                body.insert("calibration_id".into(), serde_json::Value::String(id.into()));
                body.insert("confirmed".into(), serde_json::Value::Bool(true));
                let body = with_connection_fields(arguments, body);
                return post("/api/material-lab/send", body).unwrap_or_else(|| no_backend(name, "标定矩阵下发"));
            }
            if arg_str(arguments, "material").is_none() {
                return ToolCallResult::err("run_calibration_grid 需要 material 与 thickness_mm 生成矩阵，或 calibration_id + confirmed=true 发送已生成的矩阵");
            }
            body.remove("confirmed");
            post("/api/material-lab/preview", body).unwrap_or_else(|| no_backend(name, "标定矩阵生成"))
        }
        "select_calibration_cell_tool" => {
            let Ok(args) = serde_json::from_value::<laser_contracts::SelectCalibrationCellArgs>(arguments.clone()) else {
                return ToolCallResult::err("参数校验失败: calibration_id 与 cell_number 必填");
            };
            if args.calibration_id.trim().is_empty() || args.cell_number < 1 {
                return laser_tools::execute_select_calibration_cell(&args);
            }
            let body = arguments.as_object().cloned().unwrap_or_default();
            post("/api/material-lab/select-cell", body).unwrap_or_else(|| laser_tools::execute_select_calibration_cell(&args))
        }
        "material_params_tool" => {
            let action = arg_str(arguments, "action").unwrap_or("list").to_ascii_lowercase();
            let Some(backend) = backend else { return no_backend(name, "材料库读写") };
            match action.as_str() {
                "list" => backend_result(name, backend.request("GET", "/v1/material-lab/bootstrap", &serde_json::Value::Null)),
                "get" | "recommend" => {
                    let material = arg_str(arguments, "material").unwrap_or("");
                    let thickness = arguments.get("thickness_mm").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    if material.is_empty() || thickness <= 0.0 {
                        return ToolCallResult::err("get/recommend 需要 material 与 thickness_mm");
                    }
                    let laser_mode = arg_str(arguments, "laser_mode").unwrap_or("engrave");
                    let engraving = arg_str(arguments, "engraving_mode").unwrap_or("raster");
                    let route = format!(
                        "/api/ui/material-recommendation?material={}&thickness_mm={thickness}&laser_mode={laser_mode}&engraving_mode={engraving}",
                        percent_encode(material)
                    );
                    backend_result(name, backend.request("GET", &route, &serde_json::Value::Null))
                }
                "save" | "delete" | "import" | "export" => {
                    let mut body = arguments.as_object().cloned().unwrap_or_default();
                    body.insert("action".into(), serde_json::Value::String(action.clone()));
                    if action == "delete" {
                        body.insert("confirmed".into(), serde_json::Value::Bool(true));
                    }
                    for (from, to) in [("aliases_json", "aliases"), ("params_json", "library_json")] {
                        if let Some(raw) = arg_str(arguments, from) {
                            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) {
                                body.insert(to.into(), parsed);
                            }
                        }
                    }
                    backend_result(name, backend.request("POST", "/api/material-lab/manage", &serde_json::Value::Object(body)))
                }
                other => ToolCallResult::err(format!("material_params 不支持的 action: {other}")),
            }
        }
        "generate_text_laser_task_tool" | "generate_text_image_gcode_tool" | "ai_laser_gcode_tool" => {
            if arg_str(arguments, "text").is_none() {
                return ToolCallResult::err(format!("{name} 当前仅支持 text 素材；图片素材请通过网页/桌面端上传后使用 laser_workflow_tool"));
            }
            let Some(backend) = backend else { return no_backend(name, "文字任务生成") };
            let mut body = arguments.as_object().cloned().unwrap_or_default();
            if let Some(size) = arguments.get("font_size").and_then(|v| v.as_f64()) {
                // 工具参数 font_size 为像素/字号语义；HTTP 契约以 mm 计，按 32px≈1mm 折算
                body.entry("font_size_mm").or_insert(serde_json::json!((size / 32.0).clamp(3.0, 80.0)));
            }
            let generated = backend.request("POST", "/api/text-task/generate", &serde_json::Value::Object(body));
            let confirmed = arg_bool(arguments, "confirmed") && arg_bool(arguments, "send_after_generate");
            match generated {
                Ok((status, response)) if status < 400 && confirmed => {
                    let Some(workflow_id) = response.pointer("/result/workflow_id").and_then(|v| v.as_str()) else {
                        return backend_result(name, Ok((status, response)));
                    };
                    let mut send = serde_json::Map::new();
                    send.insert("action".into(), serde_json::Value::String("confirm_send".into()));
                    send.insert("workflow_id".into(), serde_json::Value::String(workflow_id.into()));
                    send.insert("confirmed".into(), serde_json::Value::Bool(true));
                    let send = with_connection_fields(arguments, send);
                    backend_result(name, backend.request("POST", "/api/workflow", &serde_json::Value::Object(send)))
                }
                other => backend_result(name, other),
            }
        }
        "start_tuned_job_tool" => {
            let Some(workflow_id) = arg_str(arguments, "task_id").or_else(|| arg_str(arguments, "workflow_id")) else {
                return ToolCallResult::err("start_tuned_job 需要 task_id（由 generate_text_laser_task_tool 返回的 workflow_id）");
            };
            let confirmed = arg_bool(arguments, "confirmed");
            let dry_run = arg_bool(arguments, "dry_run");
            let Some(backend) = backend else { return no_backend(name, "任务发送") };
            let mut body = serde_json::Map::new();
            body.insert("workflow_id".into(), serde_json::Value::String(workflow_id.into()));
            if !confirmed || dry_run {
                body.insert("action".into(), serde_json::Value::String("status".into()));
                let result = backend_result(name, backend.request("POST", "/api/workflow", &serde_json::Value::Object(body)));
                return ToolCallResult::with_detail(result.success, format!("dry_run/未确认：仅返回任务 {workflow_id} 当前状态，未发送"), result.detail.unwrap_or_default());
            }
            body.insert("action".into(), serde_json::Value::String("confirm_send".into()));
            body.insert("confirmed".into(), serde_json::Value::Bool(true));
            let body = with_connection_fields(arguments, body);
            backend_result(name, backend.request("POST", "/api/workflow", &serde_json::Value::Object(body)))
        }
        "regenerate_text_laser_task_tool" => {
            let Some(task_id) = arg_str(arguments, "task_id") else {
                return ToolCallResult::err("regenerate 需要 task_id");
            };
            let Some(backend) = backend else { return no_backend(name, "任务重生成") };
            // 读取原任务输入，套用反馈调参后重新生成
            let status = backend.request("POST", "/api/workflow", &serde_json::json!({"action": "status", "workflow_id": task_id}));
            let Ok((code, previous)) = status else {
                return backend_result(name, status);
            };
            if code >= 400 {
                return backend_result(name, Ok((code, previous)));
            }
            let Some(input) = previous.pointer("/result/input").and_then(|v| v.as_object()).cloned() else {
                return ToolCallResult::err(format!("任务 {task_id} 没有可重生成的输入快照"));
            };
            let mut body = input;
            for key in ["font_size_mm", "letter_spacing_mm", "font_path", "engraving_mode", "material", "thickness_mm", "text"] {
                if let Some(value) = arguments.get(key).filter(|v| !v.is_null()) {
                    body.insert(key.into(), value.clone());
                }
            }
            body.insert("regenerated_from".into(), serde_json::Value::String(task_id.into()));
            backend_result(name, backend.request("POST", "/api/text-task/generate", &serde_json::Value::Object(body)))
        }
        "laser_grbl_tool" | "laser_network_grbl_tool" => {
            let action = arg_str(arguments, "action").unwrap_or("").to_ascii_lowercase();
            match action.as_str() {
                "status" | "check" | "probe" => {
                    let args = serde_json::from_value(arguments.clone()).unwrap_or_default();
                    laser_tools::execute_check_laser_connection(&args)
                }
                "cancel" | "stop" => {
                    let Some(job_id) = arg_str(arguments, "job_id") else {
                        return ToolCallResult::err("cancel 需要 job_id（workflow_id）");
                    };
                    post("/api/workflow", serde_json::json!({"action": "cancel", "workflow_id": job_id}).as_object().cloned().unwrap_or_default())
                        .unwrap_or_else(|| no_backend(name, "任务取消"))
                }
                "send_file" => ToolCallResult::err("send_file 已由安全 workflow 取代：请先用 laser_workflow_tool(action=preview) 生成并冻结快照，再 confirm_send 下发"),
                "send_command" => ToolCallResult::err("send_command 不允许直接下发任意 G-code；请使用 laser_safe_action_tool 的受控动作"),
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
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn handle_json_rpc(message: &serde_json::Value, developer_mode: bool) -> Option<serde_json::Value> {
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
            let client_name = message.pointer("/params/clientInfo/name").and_then(|v| v.as_str()).unwrap_or("xz-mcp-broker");
            let client_ver = message.pointer("/params/clientInfo/version").and_then(|v| v.as_str()).unwrap_or("0.0.1");
            global_mcp_telemetry().record_client_info(client_name, client_ver);
            global_mcp_telemetry().record_event(&format!("[RECV] initialize (client: {client_name} v{client_ver})"));
            global_mcp_telemetry().record_event(&format!("[SEND] initialize 握手响应 (protocol: {PROTOCOL_VERSION}, server: {SERVER_NAME})"));
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
            global_mcp_telemetry().record_request("tools/list (查询工具列表)");
            global_mcp_telemetry().record_event("[RECV] tools/list -> 已返回 20 项激光数控工具定义");
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": tools_list_json(developer_mode)
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
            global_mcp_telemetry().record_event(&format!("[CALL] tools/call: {name} (正在执行受控安全操作)"));
            let arguments = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));
            let result = dispatch_tool_call_with(name, &arguments, developer_mode, backend);
            let formatted_text = format_mcp_response(&result);
            global_mcp_telemetry().record_event(&format!("[RESP] tools/call: {name} -> {}", if result.success { "执行成功" } else { "拦截/失败" }));
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
pub fn endpoint_from_env() -> Option<String> {
    let value = std::env::var("MCP_ENDPOINT").ok()?;
    let value = value.trim().to_owned();
    if value.is_empty() || value.contains('<') || !(value.starts_with("ws://") || value.starts_with("wss://")) {
        return None;
    }
    Some(value)
}

/// 桥接统计（供测试与日志）。
#[derive(Debug, Default)]
pub struct BridgeStats {
    pub connections: std::sync::atomic::AtomicU64,
    pub messages_in: std::sync::atomic::AtomicU64,
    pub messages_out: std::sync::atomic::AtomicU64,
}

/// 阻塞运行桥接循环，直到 `shutdown` 置位。每次断线按 1s→60s 指数退避重连。
pub fn run_bridge(
    endpoint: &str,
    backend: Option<SharedBackend>,
    developer_mode: bool,
    shutdown: Arc<AtomicBool>,
    stats: Arc<BridgeStats>,
) {
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);
    let redacted = redact_endpoint(endpoint);
    while !shutdown.load(Ordering::SeqCst) {
        match serve_one_connection(endpoint, backend.as_deref(), developer_mode, &shutdown, &stats) {
            Ok(()) => {
                eprintln!("[mcp-bridge] 连接正常关闭: {redacted}");
                backoff = Duration::from_secs(1);
            }
            Err(error) => {
                eprintln!("[mcp-bridge] 连接失败/断开 ({redacted}): {error}；{}s 后重连", backoff.as_secs());
            }
        }
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let wait_until = std::time::Instant::now() + backoff;
        while std::time::Instant::now() < wait_until && !shutdown.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
        }
        backoff = (backoff * 2).min(max_backoff);
    }
}

fn redact_endpoint(endpoint: &str) -> String {
    match endpoint.split_once("token=") {
        Some((head, _)) => format!("{head}token=***"),
        None => endpoint.to_owned(),
    }
}

fn serve_one_connection(
    endpoint: &str,
    backend: Option<&dyn HostBackend>,
    developer_mode: bool,
    shutdown: &AtomicBool,
    stats: &BridgeStats,
) -> Result<(), String> {
    use tungstenite::{Message, stream::MaybeTlsStream};
    let (mut socket, _response) = tungstenite::connect(endpoint).map_err(|e| e.to_string())?;
    stats.connections.fetch_add(1, Ordering::Relaxed);
    eprintln!("[mcp-bridge] 已连接 {}", redact_endpoint(endpoint));
    // 读超时用于周期性检查退出标志与刷新 pong
    let stream = match socket.get_ref() {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    };
    if let Some(stream) = stream {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    }
    loop {
        if shutdown.load(Ordering::SeqCst) {
            let _ = socket.close(None);
            return Ok(());
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                stats.messages_in.fetch_add(1, Ordering::Relaxed);
                let response = match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(message) => handle_json_rpc_with(&message, developer_mode, backend),
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
            Ok(Message::Binary(_)) | Ok(Message::Ping(_)) | Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {
                // tungstenite 已自动排队 pong；flush 把它发出去
                let _ = socket.flush();
            }
            Ok(Message::Close(_)) => return Ok(()),
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
            {
                let _ = socket.flush();
                continue;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn safe_tools_list_hides_developer_command() {
        let value = tools_list_json(false);
        let names = value["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap()).collect::<Vec<_>>();
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
            Self { calls: Mutex::new(Vec::new()), responses: Mutex::new(responses.into_iter().rev().collect()) }
        }
        fn calls(&self) -> Vec<(String, String, serde_json::Value)> { self.calls.lock().unwrap().clone() }
    }
    impl HostBackend for FakeBackend {
        fn request(&self, method: &str, route: &str, body: &serde_json::Value) -> Result<(u16, serde_json::Value), String> {
            self.calls.lock().unwrap().push((method.into(), route.into(), body.clone()));
            Ok(self.responses.lock().unwrap().pop().unwrap_or((500, serde_json::json!({"success": false, "result": "no response"}))))
        }
    }

    #[test]
    fn json_rpc_handles_initialize_tools_list_notifications_and_errors() {
        let init = handle_json_rpc(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}), false).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
        let list = handle_json_rpc(&serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), false).unwrap();
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 19);
        assert!(handle_json_rpc(&serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}), false).is_none());
        let ping = handle_json_rpc(&serde_json::json!({"jsonrpc":"2.0","id":3,"method":"ping"}), false).unwrap();
        assert_eq!(ping["result"], serde_json::json!({}));
        let unknown = handle_json_rpc(&serde_json::json!({"jsonrpc":"2.0","id":4,"method":"nope"}), false).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);

        let call = handle_json_rpc(&serde_json::json!({
            "jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"recommend_laser_params_tool","arguments":{"material":"椴木","thickness_mm":3.0}}
        }), false).unwrap();
        assert_eq!(call["result"]["isError"], false);
        assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("推荐参数"));
    }

    #[test]
    fn unimplemented_and_backendless_tools_report_errors_instead_of_fake_success() {
        for name in ["laser_workflow_tool", "run_calibration_grid_tool", "material_params_tool", "start_tuned_job_tool", "generate_text_laser_task_tool", "totally_unknown_tool"] {
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
        let backend = FakeBackend::new(vec![(200, serde_json::json!({"success": true, "result": {"workflow_id": "wf-9", "status": "sending", "send_result": {"lines_sent": 0, "total_lines": 120}}}))]);
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
        let refused = dispatch_tool_call_with("laser_workflow_tool", &serde_json::json!({"action":"confirm_send","workflow_id":"wf-9"}), false, Some(&backend));
        assert!(!refused.success);
        assert!(backend.calls().is_empty());

        // 标定：无 calibration_id → preview；有 id + confirmed → send
        let backend = FakeBackend::new(vec![
            (200, serde_json::json!({"success": true, "result": {"calibration_id": "cal-1", "status": "prepared", "cell_count": 25}})),
            (200, serde_json::json!({"success": true, "result": {"calibration_id": "cal-1", "status": "sending", "send_result": {"lines_sent": 0, "total_lines": 900}}})),
        ]);
        let preview = dispatch_tool_call_with("run_calibration_grid_tool", &serde_json::json!({"material":"椴木","thickness_mm":3,"rows":5,"columns":5}), false, Some(&backend));
        assert!(preview.success && preview.result.contains("cal-1"), "{preview:?}");
        let send = dispatch_tool_call_with("run_calibration_grid_tool", &serde_json::json!({"calibration_id":"cal-1","confirmed":true,"network_host":"10.0.0.5"}), false, Some(&backend));
        assert!(send.success && send.result.contains("900"), "{send:?}");
        let calls = backend.calls();
        assert_eq!(calls[0].1, "/api/material-lab/preview");
        assert_eq!(calls[1].1, "/api/material-lab/send");
        assert_eq!(calls[1].2["confirmed"], true);

        // 后端返回失败 → 工具失败并透传文案
        let backend = FakeBackend::new(vec![(400, serde_json::json!({"success": false, "result": "发送安全门禁未通过: SHA-256 不一致"}))]);
        let gated = dispatch_tool_call_with("select_calibration_cell_tool", &serde_json::json!({"calibration_id":"cal-1","cell_number":6}), false, Some(&backend));
        assert!(!gated.success);
        assert!(gated.result.contains("SHA-256"));

        // material_params get → GET recommendation；save → manage
        let backend = FakeBackend::new(vec![
            (200, serde_json::json!({"success": true, "result": {"material": "椴木", "laser_max_power": 420}})),
            (200, serde_json::json!({"success": true, "result": {"materials": {}}})),
        ]);
        let got = dispatch_tool_call_with("material_params_tool", &serde_json::json!({"action":"get","material":"椴木","thickness_mm":3}), false, Some(&backend));
        assert!(got.success);
        let saved = dispatch_tool_call_with("material_params_tool", &serde_json::json!({"action":"save","material":"椴木","thickness_mm":3,"laser_max_power":500,"feed_rate":1200}), false, Some(&backend));
        assert!(saved.success);
        let calls = backend.calls();
        assert_eq!(calls[0].0, "GET");
        assert!(calls[0].1.starts_with("/api/ui/material-recommendation?material=%E6%A4%B4%E6%9C%A8"));
        assert_eq!(calls[1].1, "/api/material-lab/manage");
        assert_eq!(calls[1].2["action"], "save");
    }

    #[test]
    fn refine_feedback_gives_bounded_suggestions() {
        let burnt = dispatch_tool_call("refine_laser_params_from_feedback_tool", &serde_json::json!({"feedback_text":"有点太焦了","laser_max_power":400,"feed_rate":1000}), false);
        assert!(burnt.success, "{burnt:?}");
        let detail: serde_json::Value = serde_json::from_str(burnt.detail.as_deref().unwrap()).unwrap();
        assert_eq!(detail["laser_max_power"], 340);
        assert_eq!(detail["feed_rate"], 1150);
        let unknown = dispatch_tool_call("refine_laser_params_from_feedback_tool", &serde_json::json!({"feedback_text":"挺好的"}), false);
        assert!(!unknown.success);
    }

    #[test]
    fn endpoint_placeholder_is_treated_as_unconfigured() {
        assert_eq!(redact_endpoint("wss://api.example/mcp/?token=abc"), "wss://api.example/mcp/?token=***");
        // 直接测试判定逻辑（不依赖进程环境变量）
        for bad in ["", "wss://api.xiaozhi.me/mcp/?token=<your-token>", "http://x"] {
            let value = bad.trim();
            let invalid = value.is_empty() || value.contains('<') || !(value.starts_with("ws://") || value.starts_with("wss://"));
            assert!(invalid, "{bad}");
        }
    }

    /// 本地 WebSocket 服务端：接收 initialize / tools/call，校验桥接真实往返，然后关闭。
    #[test]
    fn bridge_round_trips_json_rpc_over_loopback_websocket() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.send(tungstenite::Message::Text(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#.into())).unwrap();
            let init: serde_json::Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            ws.send(tungstenite::Message::Text(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.into())).unwrap();
            ws.send(tungstenite::Message::Text(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"laser_workflow_tool","arguments":{"action":"status","workflow_id":"wf-1"}}}"#.into())).unwrap();
            let call: serde_json::Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            let _ = ws.close(None);
            (init, call)
        });
        let backend: SharedBackend = Arc::new(FakeBackend::new(vec![(200, serde_json::json!({"success": true, "result": {"workflow_id": "wf-1", "status": "completed"}}))]));
        let shutdown = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(BridgeStats::default());
        let endpoint = format!("ws://127.0.0.1:{port}/mcp/?token=test");
        let shutdown_for_bridge = shutdown.clone();
        let stats_for_bridge = stats.clone();
        let bridge = std::thread::spawn(move || run_bridge(&endpoint, Some(backend), false, shutdown_for_bridge, stats_for_bridge));
        let (init, call) = server.join().unwrap();
        shutdown.store(true, Ordering::SeqCst);
        bridge.join().unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(call["id"], 2);
        assert_eq!(call["result"]["isError"], false);
        assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("completed"));
        assert_eq!(stats.connections.load(Ordering::Relaxed), 1);
        assert_eq!(stats.messages_in.load(Ordering::Relaxed), 3);
        assert_eq!(stats.messages_out.load(Ordering::Relaxed), 2);
    }
}
