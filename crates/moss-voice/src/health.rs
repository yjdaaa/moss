//! 接入点健康检查（技术书 §9.4.5）：`…/mcp_endpoint/health?key=…` 返回的
//! `robot_connections`（>0 表示有设备会话经服务端连到接入点）与 `tool_connections`。
//! 无法获得时显示“未知”，不猜。

use std::io::Read;
use std::time::Duration;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct HealthCounts {
    pub robot_connections: Option<u32>,
    pub tool_connections: Option<u32>,
}

fn find_count(value: &serde_json::Value, key: &str) -> Option<u32> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(found) = map.get(key).and_then(serde_json::Value::as_u64) {
                return u32::try_from(found).ok();
            }
            map.values().find_map(|child| find_count(child, key))
        }
        serde_json::Value::Array(items) => items.iter().find_map(|child| find_count(child, key)),
        _ => None,
    }
}

/// 从健康检查响应中取连接数（字段可能在顶层或 `data` 内）。
pub fn parse_counts(value: &serde_json::Value) -> HealthCounts {
    HealthCounts {
        robot_connections: find_count(value, "robot_connections"),
        tool_connections: find_count(value, "tool_connections"),
    }
}

/// 地址中的 `key` / `token` 等查询参数替换为 `***`（用于页面显示与日志）。
pub fn redact_url(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else { return url.to_owned() };
    let parts: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, _)) => {
                let lower = name.to_ascii_lowercase();
                if ["key", "token", "access_token", "api_key", "secret", "password"].contains(&lower.as_str()) {
                    format!("{name}=***")
                } else {
                    pair.to_owned()
                }
            }
            None => pair.to_owned(),
        })
        .collect();
    format!("{base}?{}", parts.join("&"))
}

/// 请求一次健康检查（3 秒超时、不跟随重定向）。错误信息不含地址。
pub fn fetch(url: &str) -> Result<HealthCounts, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("健康检查地址必须以 http:// 或 https:// 开头".into());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "无法创建网络客户端".to_owned())?;
    let response = client.get(url).send().map_err(|error| {
        if error.is_timeout() { "健康检查超时".to_owned() } else { "无法连接接入点".to_owned() }
    })?;
    if !response.status().is_success() {
        return Err(format!("接入点返回 HTTP {}", response.status().as_u16()));
    }
    let mut bytes = Vec::new();
    response.take(64 * 1024).read_to_end(&mut bytes).map_err(|_| "健康检查响应读取失败".to_owned())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| "健康检查返回的不是 JSON".to_owned())?;
    let counts = parse_counts(&value);
    if counts.robot_connections.is_none() && counts.tool_connections.is_none() {
        return Err("健康检查响应中没有连接数".into());
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::fake_http::serve_once;

    #[test]
    fn counts_are_found_at_any_depth() {
        let value = serde_json::json!({"result": {"data": {"robot_connections": 2, "tool_connections": 1}}});
        assert_eq!(parse_counts(&value), HealthCounts { robot_connections: Some(2), tool_connections: Some(1) });
        assert_eq!(parse_counts(&serde_json::json!({"status": "ok"})), HealthCounts::default());
    }

    #[test]
    fn redact_url_hides_key_and_token() {
        assert_eq!(redact_url("http://h:8004/mcp_endpoint/health?key=abc"), "http://h:8004/mcp_endpoint/health?key=***");
        assert_eq!(redact_url("ws://h/mcp/?token=abc&x=1"), "ws://h/mcp/?token=***&x=1");
        assert_eq!(redact_url("ws://h/mcp/"), "ws://h/mcp/");
    }

    #[test]
    fn fetch_reads_robot_connections() {
        let (base, _rx) = serve_once(
            "HTTP/1.1 200 OK",
            "Content-Type: application/json\r\n",
            br#"{"status":"success","data":{"robot_connections":0,"tool_connections":1}}"#.to_vec(),
        );
        let counts = fetch(&format!("{base}/mcp_endpoint/health?key=k")).unwrap();
        assert_eq!(counts.robot_connections, Some(0));
        assert_eq!(counts.tool_connections, Some(1));
        let (base, _rx) = serve_once("HTTP/1.1 403 Forbidden", "", Vec::new());
        assert!(fetch(&format!("{base}/mcp_endpoint/health?key=bad")).unwrap_err().contains("403"));
    }
}
