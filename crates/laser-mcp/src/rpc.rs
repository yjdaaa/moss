//! JSON-RPC 消息处理（initialize / ping / tools/list / tools/call）。

use crate::{
    HostBackend, PROTOCOL_VERSION, SERVER_NAME, dispatch_tool_call_with, format_mcp_response,
    telemetry_for, tools_list_with,
};

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
    let telemetry = telemetry_for(backend);
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
            telemetry.record_client_info(client_name, client_ver);
            telemetry.record_protocol_version(PROTOCOL_VERSION);
            let shown = match (client_name.is_empty(), client_ver.is_empty()) {
                (true, _) => "未上报".to_owned(),
                (false, true) => client_name.to_owned(),
                (false, false) => format!("{client_name} v{client_ver}"),
            };
            telemetry.record_event(&format!("[RECV] initialize (client: {shown})"));
            telemetry.record_event(&format!(
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
            telemetry.record_event("[PING] 心跳保持正常");
            Some(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} }))
        }
        "tools/list" => {
            let tools = tools_list_with(developer_mode, backend);
            let count = tools["tools"].as_array().map(Vec::len).unwrap_or(0);
            telemetry.record_request("tools/list (查询工具列表)");
            telemetry.record_event(&format!(
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
            telemetry.record_request(&format!("tools/call: {name}"));
            telemetry.record_event(&format!("[CALL] tools/call: {name} (正在执行受控安全操作)"));
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let result = dispatch_tool_call_with(name, &arguments, developer_mode, backend);
            let formatted_text = format_mcp_response(&result);
            telemetry.record_event(&format!(
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
