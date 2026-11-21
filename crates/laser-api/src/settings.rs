pub(crate) const JEV_API_KEY_SECRET: &str = "jev_api_key";
pub(crate) const DEFAULT_JEV_ENDPOINT: &str = laser_contracts::OPENROUTER_DECISIONS_URL;
pub(crate) const JEV_MODEL: &str = "typesafe/jev-1.13";

pub(crate) fn normalize_mcp_endpoint(value: &str) -> Option<String> {
    laser_contracts::parse_mcp_endpoint_url(value)
}

/// `/api/system/info` 的 `voice` 字段：`{mode, device_sessions}`。设备会话数只在内置链路下有意义
/// （来自内置语音服务）；未知时为 null。2026-11-17 删除的自建链路（遗留 `self_hosted`）按内置链路报告，
/// 与桌面端启动时的设置迁移一致。
pub(crate) fn voice_status_json(
    runtime_path: Option<&std::path::Path>,
    telemetry: &laser_contracts::VoiceTelemetry,
) -> serde_json::Value {
    let settings = read_runtime_settings(runtime_path);
    let mode = match settings
        .get("voice")
        .and_then(|voice| voice.get("mode"))
        .and_then(serde_json::Value::as_str)
    {
        Some("builtin" | "self_hosted") => "builtin",
        _ => "official",
    };
    let sessions = if mode != "official" {
        telemetry
            .device_sessions()
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    };
    let (phase, level) = if mode == "builtin" {
        telemetry.pulse()
    } else {
        (None, 0)
    };
    serde_json::json!({
        "mode": mode,
        "device_sessions": sessions,
        "phase": phase,
        "level": level,
    })
}

pub(crate) fn read_runtime_settings(
    runtime_path: Option<&std::path::Path>,
) -> serde_json::Map<String, serde_json::Value> {
    runtime_path
        .map(|path| {
            laser_runtime::settings_store::read(&laser_runtime::settings_store::settings_path(path))
        })
        .unwrap_or_default()
}

/// 读-改-写运行目录的 settings.json（经 `laser_runtime::settings_store` 持锁原子替换，保留其他键）。
/// 没有运行目录或写入失败时返回错误，调用方不能报告“已保存”。
pub(crate) fn update_runtime_settings(
    runtime_path: Option<&std::path::Path>,
    change: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> Result<(), String> {
    let runtime_path = runtime_path.ok_or("没有配置运行目录")?;
    laser_runtime::settings_store::update(
        &laser_runtime::settings_store::settings_path(runtime_path),
        |root| {
            change(root);
            Ok(())
        },
    )
}

pub(crate) fn mask_secret(value: &str) -> String {
    let value = value.trim();
    let char_count = value.chars().count();
    if char_count <= 8 {
        return "••••••••".to_owned();
    }
    let prefix: String = value.chars().take(5).collect();
    let suffix: String = value.chars().skip(char_count - 4).collect();
    format!("{prefix}••••••••{suffix}")
}

fn scrub_telemetry_str(text: &str, endpoints: &[&str]) -> String {
    if text.is_empty() {
        return String::new();
    }
    if endpoints.is_empty() {
        return laser_contracts::scrub_endpoint_secrets(text, "");
    }
    endpoints.iter().fold(text.to_owned(), |acc, ep| {
        laser_contracts::scrub_endpoint_secrets(&acc, ep)
    })
}

/// `/api/system/info` 的 `mcp` 段：只反映真实配置与真实遥测（技术书 §9.5.3，验收 A-72）。
///
/// - `configured`：是否配置了接入点地址；
/// - `connected`：桥当前是否与接入点保持连接；
/// - `include_details` 为假（非回环未鉴权请求）时隐藏接入点地址、客户端、请求、事件与多接入点错误遥测；
/// - `tools_count`：当前开关下实际暴露的工具数（安全模式 19，开发者模式 20）。
pub(crate) fn mcp_status_json(
    endpoint: Option<&str>,
    telemetry: &laser_contracts::McpTelemetrySnapshot,
    tools_count: usize,
    include_details: bool,
) -> serde_json::Value {
    let scrub_endpoints: Vec<&str> = endpoint
        .into_iter()
        .chain(
            telemetry
                .endpoints
                .iter()
                .map(|link| link.endpoint.as_str()),
        )
        .collect();
    let endpoint_display = if include_details {
        endpoint
            .map(laser_contracts::redact_mcp_endpoint)
            .unwrap_or_default()
    } else {
        String::new()
    };
    let client_info = if !include_details {
        String::new()
    } else {
        let client_name = scrub_telemetry_str(&telemetry.client_name, &scrub_endpoints);
        let client_version = scrub_telemetry_str(&telemetry.client_version, &scrub_endpoints);
        match (client_name.is_empty(), client_version.is_empty()) {
            (true, _) => String::new(),
            (false, true) => client_name,
            (false, false) => format!("{client_name} v{client_version}"),
        }
    };
    let last_request = if include_details {
        scrub_telemetry_str(&telemetry.last_request, &scrub_endpoints)
    } else {
        String::new()
    };
    let last_request_time = if include_details {
        scrub_telemetry_str(&telemetry.last_request_time, &scrub_endpoints)
    } else {
        String::new()
    };
    let protocol_version = if include_details {
        scrub_telemetry_str(&telemetry.protocol_version, &scrub_endpoints)
    } else {
        String::new()
    };
    let recent_events: Vec<String> = if include_details {
        telemetry
            .recent_events
            .iter()
            .map(|event| scrub_telemetry_str(event, &scrub_endpoints))
            .collect()
    } else {
        Vec::new()
    };
    let endpoints: Vec<serde_json::Value> = if include_details {
        telemetry
            .endpoints
            .iter()
            .map(|link| {
                serde_json::json!({
                    "id": scrub_telemetry_str(&link.id, &scrub_endpoints),
                    "endpoint": laser_contracts::redact_mcp_endpoint(&link.endpoint),
                    "connected": link.connected,
                    "connections": link.connections,
                    "last_error": link.last_error.as_deref().map(|err| scrub_telemetry_str(err, &scrub_endpoints)),
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    serde_json::json!({
        "configured": endpoint.is_some(),
        "connected": telemetry.connected,
        "endpoint": endpoint_display,
        "client_info": client_info,
        "last_request": last_request,
        "last_request_time": last_request_time,
        "messages_count": if include_details { telemetry.messages_count } else { 0 },
        "tools_count": tools_count,
        // 最近一次握手实际应答的版本；未握手或非回环未鉴权时为空
        "protocol_version": protocol_version,
        "recent_events": recent_events,
        // 各接入点的连接状态（NV8 多接入点；地址已脱敏，非回环未鉴权时隐藏）
        "endpoints": endpoints,
        // 以下三项是固定的设计说明，不是检测结果
        "core_tool": "laser_workflow_tool",
        "safety_gate": "发送前重新计算 G-code 文件 SHA-256，与确认时的快照比对",
        "estop_channel": "0x18 软复位走实时通道、不排队；写出成功不代表已停机"
    })
}

pub(crate) fn configured_mcp_endpoint(runtime_path: Option<&std::path::Path>) -> Option<String> {
    let saved = read_runtime_settings(runtime_path)
        .get("mcp_endpoint")
        .and_then(|item| item.as_str())
        .map(str::to_owned);
    saved
        .as_deref()
        .and_then(normalize_mcp_endpoint)
        .or_else(|| {
            std::env::var("MCP_ENDPOINT")
                .ok()
                .and_then(|value| normalize_mcp_endpoint(&value))
        })
}
