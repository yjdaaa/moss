#[derive(Debug, Clone, PartialEq)]
pub struct ApiResponse {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: serde_json::Value,
}

pub fn success(result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"success": true, "result": result})
}

const CORS_ALLOW_METHODS: &str = "GET, POST, PUT, DELETE, OPTIONS";
const CORS_ALLOW_HEADERS: &str = "Content-Type, Authorization, Accept, X-Moss-Desktop-Session";

pub(crate) fn request_head(request: &[u8]) -> std::borrow::Cow<'_, str> {
    let end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or(request.len());
    String::from_utf8_lossy(&request[..end])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyFramingError {
    TransferEncodingUnsupported,
    InvalidContentLength,
}

/// 统一校验 HTTP/1.1 请求体长度头，供 `server.rs` 读取阶段与 `routes.rs` 路由阶段共用，
/// 杜绝重复 `Content-Length` 取首/取尾分歧与非法值静默按 0 处理。
pub(crate) fn parse_content_length(head: &str) -> Result<usize, BodyFramingError> {
    let mut parsed: Option<usize> = None;
    for line in head.lines().skip(1) {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let name = key.trim();
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(BodyFramingError::TransferEncodingUnsupported);
        }
        if name.eq_ignore_ascii_case("content-length") {
            let trimmed = value.trim();
            if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
                return Err(BodyFramingError::InvalidContentLength);
            }
            let len = trimmed
                .parse::<usize>()
                .map_err(|_| BodyFramingError::InvalidContentLength)?;
            if let Some(prev) = parsed
                && prev != len
            {
                return Err(BodyFramingError::InvalidContentLength);
            }
            parsed = Some(len);
        }
    }
    Ok(parsed.unwrap_or(0))
}

pub(crate) fn header_value(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}

fn cors_header_block(origin: &str) -> String {
    format!(
        "Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\nAccess-Control-Allow-Methods: {CORS_ALLOW_METHODS}\r\nAccess-Control-Allow-Headers: {CORS_ALLOW_HEADERS}\r\n"
    )
}

/// 在状态行之后插入允许来源的 CORS 头。响应构造函数本身不再输出任何 CORS 头。
pub(crate) fn with_cors_headers(response: Vec<u8>, origin: &str) -> Vec<u8> {
    let Some(line_end) = response.windows(2).position(|window| window == b"\r\n") else {
        return response;
    };
    let mut out = Vec::with_capacity(response.len() + 256);
    out.extend_from_slice(&response[..line_end + 2]);
    out.extend_from_slice(cors_header_block(origin).as_bytes());
    out.extend_from_slice(&response[line_end + 2..]);
    out
}

/// `OPTIONS` 响应：允许来源时带 CORS 头与缓存时长；无 `Origin` 时只返回空 200。
pub(crate) fn http_options(origin: Option<&str>) -> Vec<u8> {
    let cors = origin
        .map(|origin| {
            format!(
                "{}Access-Control-Max-Age: 86400\r\n",
                cors_header_block(origin)
            )
        })
        .unwrap_or_default();
    format!("HTTP/1.1 200 OK\r\n{cors}Content-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()
}

pub(crate) fn http_json(status: u16, body: serde_json::Value) -> Vec<u8> {
    http_json_with_headers(status, body, &[])
}

pub(crate) fn http_json_with_headers(
    status: u16,
    body: serde_json::Value,
    extra_headers: &[(&str, String)],
) -> Vec<u8> {
    let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{\"success\":false}".to_vec());
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        410 => "Gone",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "HTTP",
    };
    let mut headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nCache-Control: no-store\r\n"
    );
    for (key, value) in extra_headers {
        headers.push_str(&format!("{key}: {value}\r\n"));
    }
    headers.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    ));
    headers.into_bytes().into_iter().chain(bytes).collect()
}

pub(crate) fn http_bytes_with_disposition(
    status: u16,
    content_type: &str,
    disposition: Option<&str>,
    bytes: Vec<u8>,
) -> Vec<u8> {
    let reason = if status == 200 { "OK" } else { "Not Found" };
    let disp_header = match disposition {
        Some(d) => format!("Content-Disposition: {d}\r\n"),
        None => String::new(),
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\n{disp_header}Cache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    ).into_bytes().into_iter().chain(bytes).collect()
}

pub(crate) fn http_bytes(status: u16, content_type: &str, bytes: Vec<u8>) -> Vec<u8> {
    http_bytes_with_disposition(status, content_type, None, bytes)
}

pub(crate) fn http_sse(body: String) -> Vec<u8> {
    let bytes = body.into_bytes();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nContent-Length: {}\r\n\r\n",
        bytes.len()
    ).into_bytes().into_iter().chain(bytes).collect()
}

pub(crate) fn query_param(query: &str, wanted: &str) -> Option<String> {
    query.split('&').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        if key != wanted {
            return None;
        }
        Some(percent_decode(value))
    })
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(hi), Some(lo)) =
                (hex_nibble(bytes[index + 1]), hex_nibble(bytes[index + 2]))
        {
            output.push((hi << 4) | lo);
            index += 3;
            continue;
        }
        output.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

pub fn failure(message: impl Into<String>, detail: Option<serde_json::Value>) -> serde_json::Value {
    let mut response = serde_json::json!({"success": false, "result": message.into()});
    if let Some(detail) = detail {
        response["detail"] = detail;
    }
    response
}

pub fn parse_json_object(
    body: &[u8],
    limit: usize,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    if body.len() > limit {
        return Err("请求体过大".into());
    }
    if body.is_empty() {
        return Ok(serde_json::Map::new());
    }
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|error| format!("JSON 解析失败: {error}"))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| "JSON 顶层必须是对象".into())
}

pub fn parse_bool(value: Option<&serde_json::Value>, default: bool) -> bool {
    match value {
        None | Some(serde_json::Value::Null) => default,
        Some(serde_json::Value::Bool(value)) => *value,
        Some(serde_json::Value::String(value)) if value.trim().is_empty() => default,
        Some(value) => matches!(
            value
                .as_str()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "1" | "true" | "yes" | "y" | "on" | "确认" | "confirmed"
        ),
    }
}

pub fn parse_optional_float(
    value: Option<&serde_json::Value>,
    key: &str,
) -> Result<Option<f64>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() || value.as_str().is_some_and(|v| v.trim().is_empty()) {
        return Ok(None);
    }
    let parsed = value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok());
    parsed
        .filter(|v: &f64| v.is_finite())
        .map(Some)
        .ok_or_else(|| format!("{key} 必须是数字"))
}

pub fn parse_float(
    value: Option<&serde_json::Value>,
    key: &str,
    default: f64,
) -> Result<f64, String> {
    Ok(parse_optional_float(value, key)?.unwrap_or(default))
}

pub fn parse_int(
    value: Option<&serde_json::Value>,
    key: &str,
    default: i64,
) -> Result<i64, String> {
    let number = parse_float(value, key, default as f64)?;
    if number < i64::MIN as f64 || number > i64::MAX as f64 {
        return Err(format!("{key} 必须是整数"));
    }
    Ok(number.trunc() as i64)
}

pub fn parse_optional_positive(
    value: Option<&serde_json::Value>,
    key: &str,
) -> Result<Option<f64>, String> {
    let number = parse_float(value, key, 0.0)?;
    if number < 0.0 {
        return Err(format!("{key} 不能小于 0"));
    }
    if number == 0.0 {
        Ok(None)
    } else {
        Ok(Some(number))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_optional_float;
    use serde_json::json;

    #[test]
    fn parse_optional_float_handles_empty_numbers_strings_and_invalid_values() {
        assert_eq!(parse_optional_float(None, "x").unwrap(), None);
        assert_eq!(parse_optional_float(Some(&json!(null)), "x").unwrap(), None);
        assert_eq!(parse_optional_float(Some(&json!("")), "x").unwrap(), None);
        assert_eq!(
            parse_optional_float(Some(&json!("   ")), "x").unwrap(),
            None
        );

        assert_eq!(
            parse_optional_float(Some(&json!(0)), "x").unwrap(),
            Some(0.0)
        );
        assert_eq!(
            parse_optional_float(Some(&json!(-3.5)), "x").unwrap(),
            Some(-3.5)
        );
        assert_eq!(
            parse_optional_float(Some(&json!(" 12.25 ")), "x").unwrap(),
            Some(12.25)
        );

        assert_eq!(
            parse_optional_float(Some(&json!("abc")), "fill_options.angle_deg"),
            Err("fill_options.angle_deg 必须是数字".into())
        );
        assert_eq!(
            parse_optional_float(Some(&json!(true)), "x"),
            Err("x 必须是数字".into())
        );
        assert_eq!(
            parse_optional_float(Some(&json!("NaN")), "x"),
            Err("x 必须是数字".into())
        );
        assert_eq!(
            parse_optional_float(Some(&json!("inf")), "x"),
            Err("x 必须是数字".into())
        );
    }
}
