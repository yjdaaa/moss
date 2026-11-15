pub(crate) const JEV_API_KEY_SECRET: &str = "jev_api_key";
pub(crate) const DEFAULT_JEV_ENDPOINT: &str = "https://openrouter.ai/api/alpha/decisions";
pub(crate) const JEV_MODEL: &str = "typesafe/jev-1.13";

pub(crate) fn normalize_mcp_endpoint(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.contains('<')
        || !(value.starts_with("ws://") || value.starts_with("wss://"))
    {
        return None;
    }
    Some(value.to_owned())
}

/// `/api/system/info` 的 `voice` 字段：`{mode, device_sessions}`。设备会话数只在自建与内置链路下有意义
/// （自建来自接入点健康检查，内置来自内置语音服务）；未知时为 null。
pub(crate) fn voice_status_json(runtime_path: Option<&std::path::Path>) -> serde_json::Value {
    let settings = read_runtime_settings(runtime_path);
    let mode = match settings
        .get("voice")
        .and_then(|voice| voice.get("mode"))
        .and_then(serde_json::Value::as_str)
    {
        Some("self_hosted") => "self_hosted",
        Some("builtin") => "builtin",
        _ => "official",
    };
    let telemetry = laser_contracts::global_voice_telemetry();
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
        .map(|path| path.join("settings.json"))
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// 读-改-写运行目录的 settings.json（先写临时文件再改名，保留其他键）。
/// 没有运行目录或写入失败时返回错误，调用方不能报告“已保存”。
pub(crate) fn update_runtime_settings(
    runtime_path: Option<&std::path::Path>,
    change: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> Result<(), String> {
    laser_contracts::with_settings_file_lock(|| {
        let runtime_path = runtime_path.ok_or("没有配置运行目录")?;
        let target = runtime_path.join("settings.json");
        let mut current = match std::fs::read(&target) {
            Ok(bytes) if bytes.iter().all(|b| b.is_ascii_whitespace()) => serde_json::Map::new(),
            Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|v| v.as_object().cloned())
                .ok_or_else(|| "settings.json 格式损坏，拒绝覆盖".to_owned())?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
            Err(err) => return Err(format!("读取 settings.json 失败：{err}")),
        };
        change(&mut current);
        let data = serde_json::to_vec_pretty(&current).map_err(|error| error.to_string())?;
        let temp = runtime_path.join(format!("settings.json.api-tmp-{}", std::process::id()));
        std::fs::write(&temp, data)
            .and_then(|()| std::fs::rename(&temp, &target))
            .map_err(|error| {
                let _ = std::fs::remove_file(&temp);
                format!("写入 settings.json 失败：{error}")
            })
    })
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

/// `/api/system/info` 的 `mcp` 段：只反映真实配置与真实遥测（技术书 §9.5.3，验收 A-72）。
///
/// - `configured`：是否配置了接入点地址；
/// - `connected`：桥当前是否与接入点保持连接；
/// - `client_info` / `last_request` / `recent_events` / `messages_count`：没有真实数据时为空 / 0；
/// - `tools_count`：当前开关下实际暴露的工具数（安全模式 19，开发者模式 20）。
pub(crate) fn mcp_status_json(
    endpoint: Option<&str>,
    telemetry: &laser_contracts::McpTelemetrySnapshot,
    tools_count: usize,
) -> serde_json::Value {
    let endpoint_display = endpoint
        .map(|ep| match ep.split_once("token=") {
            Some((head, _)) => format!("{head}token=***"),
            None => ep.to_owned(),
        })
        .unwrap_or_default();
    let client_info = match (
        telemetry.client_name.is_empty(),
        telemetry.client_version.is_empty(),
    ) {
        (true, _) => String::new(),
        (false, true) => telemetry.client_name.clone(),
        (false, false) => format!("{} v{}", telemetry.client_name, telemetry.client_version),
    };
    serde_json::json!({
        "configured": endpoint.is_some(),
        "connected": telemetry.connected,
        "endpoint": endpoint_display,
        "client_info": client_info,
        "last_request": telemetry.last_request,
        "last_request_time": telemetry.last_request_time,
        "messages_count": telemetry.messages_count,
        "tools_count": tools_count,
        // 最近一次握手实际应答的版本；未握手为空，界面显示“未知”（n68：不再写死）
        "protocol_version": telemetry.protocol_version,
        "recent_events": telemetry.recent_events,
        // 各接入点的连接状态（NV8 多接入点；地址已脱敏）
        "endpoints": telemetry.endpoints.iter().map(|link| serde_json::json!({
            "id": link.id,
            "endpoint": link.endpoint,
            "connected": link.connected,
            "connections": link.connections,
            "last_error": link.last_error,
        })).collect::<Vec<_>>(),
        // 以下三项是固定的设计说明，不是检测结果
        "core_tool": "laser_workflow_tool",
        "safety_gate": "发送前重新计算 G-code 文件 SHA-256，与确认时的快照比对",
        "estop_channel": "0x18 软复位走实时通道、不排队；写出成功不代表已停机"
    })
}

pub(crate) fn configured_mcp_endpoint(runtime_path: Option<&std::path::Path>) -> Option<String> {
    let saved = runtime_path
        .map(|path| path.join("settings.json"))
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("mcp_endpoint")
                .and_then(|item| item.as_str())
                .map(str::to_owned)
        });
    saved
        .as_deref()
        .and_then(normalize_mcp_endpoint)
        .or_else(|| {
            std::env::var("MCP_ENDPOINT")
                .ok()
                .and_then(|value| normalize_mcp_endpoint(&value))
        })
}
