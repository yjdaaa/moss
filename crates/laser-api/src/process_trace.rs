//! 激光侧完整过程记录（n105 / r55，默认关闭）：写进当天时间线的激光步骤。
//!
//! - `api_begin` / `api_call`：每次改动类接口调用（连接检查、生成、确认发送、走边框、急停与其他安全动作、审批、
//!   材料与设置修改……）开始处理时记一步（发起方与参数），处理完再记一步（结果与耗时），两步用同一个 `call`
//!   编号对上；中间触发的生成、加工步骤按真实先后排在两步之间（2026-11-23 n110）。只读轮询与凭据接口不记；
//!   只隐藏密钥本身（字段名是密钥类词、或文字里的令牌片段），其余原样保留、不截断；
//! - `generate` / `generate_failed`：生成预览的参数、产物与失败原因，G-code、预览图、画板场景复制一份
//!   （`generated_assets/` 里没被记录引用的旧文件 7 天后会被清掉）；
//! - `job_start` / `job_progress` / `job_end`：加工线程的开始、每 1% 进度（含当时的状态报告）与结束结果；
//! - `feedback_jev`：加工反馈的 Jev 请求与原始返回。

use super::*;

/// 生成结果里指向 `generated_assets/` 等目录下文件的字段。
const ARTIFACT_FILE_KEYS: &[&str] = &[
    "gcode_file",
    "image_file",
    "scene_file",
    "path_preview_file",
    "source_image_file",
];

/// 请求里的大块字段（画板截图 data URL、画板场景、内部产物清单）：不塞进时间线这一行，而是另存成附件，
/// 记录里写附件路径与字节数（2026-11-23 n110 起不再只留字节数）。
const BULKY_KEYS: &[&str] = &[
    "image_data_url",
    "scene_json",
    "scene",
    "_artifacts",
    "content_base64",
];

/// 超大 HTTP 请求/响应体（如 20 MB 上传或 6 MB 画板保存）不在接口线程里解析成 JSON，原样存成附件。
const LARGE_BODY_BYTES: usize = 256 * 1024;

/// 本次运行内的接口调用编号：`api_begin` 与 `api_call` 两步靠它对上。
static API_CALL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn response_body(response: &[u8]) -> Option<serde_json::Value> {
    let start = response.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    serde_json::from_slice(&response[start..]).ok()
}

/// 从 HTTP 响应里取 `workflow_id`（`result.workflow_id` 或顶层）。
pub(crate) fn response_workflow_id(response: &[u8]) -> Option<String> {
    let body = response_body(response)?;
    body.pointer("/result/workflow_id")
        .or_else(|| body.get("workflow_id"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

/// 响应不是 2xx 时返回错误文字（`result` 或整个响应体），供记 `generate_failed`。
pub(crate) fn response_failure(response: &[u8]) -> Option<String> {
    let ok = response.starts_with(b"HTTP/1.1 2") || response.starts_with(b"HTTP/1.0 2");
    if ok {
        return None;
    }
    let status = std::str::from_utf8(response.split(|b| *b == b'\r').next().unwrap_or_default())
        .unwrap_or("")
        .to_owned();
    let detail = response_body(response)
        .map(|body| match body.get("result") {
            Some(serde_json::Value::String(text)) => text.clone(),
            _ => body.to_string(),
        })
        .unwrap_or_default();
    Some(format!("{status} {detail}").trim().to_owned())
}

/// 把大块字段另存成附件（递归），原位置换成 `{"file": 附件路径, "bytes": 字节数}`。
/// data URL 解码成对应格式（`data:image/png;base64,…` 存成 `.png`），其余字符串存 `.txt`、对象存 `.json`。
/// 调用前必须先经 [`scrub_trace_value`] 去掉凭据；文字类内容另存前再按文字规则脱敏一遍。
pub(crate) fn externalize_bulky(
    trace: &laser_runtime::ProcessTrace,
    stem: &str,
    value: &mut serde_json::Value,
) {
    match value {
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                let bulky = BULKY_KEYS.contains(&key.as_str());
                let Some(child) = map.get_mut(&key) else {
                    continue;
                };
                if bulky && !child.is_null() {
                    *child = save_bulky(trace, &format!("{stem}-{key}"), child);
                } else {
                    externalize_bulky(trace, stem, child);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                externalize_bulky(trace, stem, item);
            }
        }
        _ => {}
    }
}

/// 解码后是二进制图片 / 音频等、不需要按文字脱敏的扩展名。
const BINARY_EXTS: &[&str] = &[
    "png", "jpeg", "jpg", "gif", "webp", "bmp", "wav", "mp3", "ogg",
];

fn save_bulky(
    trace: &laser_runtime::ProcessTrace,
    stem: &str,
    value: &serde_json::Value,
) -> serde_json::Value {
    use base64::Engine as _;
    let (bytes, ext): (Vec<u8>, String) = match value {
        serde_json::Value::String(text) => match text
            .strip_prefix("data:")
            .and_then(|rest| rest.split_once(";base64,"))
            .and_then(|(mime, data)| {
                let ext = mime
                    .rsplit('/')
                    .next()
                    .unwrap_or("bin")
                    .split('+')
                    .next()
                    .unwrap_or("bin");
                base64::engine::general_purpose::STANDARD
                    .decode(data.trim())
                    .ok()
                    .map(|raw| (raw, ext.to_owned()))
            }) {
            Some(decoded) => decoded,
            None => (text.as_bytes().to_vec(), "txt".to_owned()),
        },
        other => (other.to_string().into_bytes(), "json".to_owned()),
    };
    let bytes = if BINARY_EXTS.contains(&ext.as_str()) {
        bytes
    } else {
        // 文字类（含 svg / 纯文本 / JSON）：按文字规则再去一遍凭据片段
        scrub_trace_text(&String::from_utf8_lossy(&bytes)).into_bytes()
    };
    let file = trace.save_file("laser", stem, &ext, &bytes);
    serde_json::json!({"file": file, "bytes": bytes.len()})
}

/// 字段名切成词后，含这些词（或其复数）的值不记（密钥、令牌、签名、配对码、密码）。
/// 按整词比较：`spindle`、`keywords`、`clipping` 这类字段名不会被误伤。
const SECRET_KEY_WORDS: &[&str] = &[
    "key",
    "apikey",
    "token",
    "secret",
    "password",
    "passwd",
    "passcode",
    "pin",
    "authorization",
    "auth",
    "credential",
    "cookie",
    "sig",
    "signature",
    "jwt",
    "otp",
];

/// 字段名切词：`_` / `-` / `.` / 空白分隔，驼峰与连续大写缩写也切开（`APIToken` → api、token；
/// `openAIKey` → open、ai、key）。
fn field_words(key: &str) -> Vec<String> {
    let chars: Vec<char> = key.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        if c.is_ascii_uppercase() && !current.is_empty() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_lower)
            {
                words.push(std::mem::take(&mut current));
            }
        }
        current.push(c.to_ascii_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn is_secret_field(key: &str) -> bool {
    field_words(key).iter().any(|word| {
        let singular = word.strip_suffix('s').unwrap_or(word);
        SECRET_KEY_WORDS.contains(&word.as_str()) || SECRET_KEY_WORDS.contains(&singular)
    })
}

/// 这些接口的请求或响应本身就是凭据，或是无 `action` 字段的只读轮询/查询，整条不记。
const UNTRACED_ROUTES: &[&str] = &[
    "/api/process-trace",
    "/api/jev/reveal",
    "/v1/auth/pair",
    "/v1/auth/challenge",
    "/api/material-lab/status",
    "/api/material-lab/recent",
    "/api/registration/read-position",
];
/// 只读的轮询动作不进时间线（前端每几秒查一次状态，记下来全是噪音）。
const READ_ONLY_ACTIONS: &[&str] = &[
    "status", "recent", "list", "get", "history", "query", "search", "detail", "poll",
];
/// 画板编辑时高频自动同步的接口：每步只记一句摘要与字节数，不逐次另存整包画板
/// （作品在生成预览时整份复制进记录）。
const SUMMARY_ONLY_ROUTES: &[&str] = &["/api/draw/state"];

const HIDDEN: &str = "[已隐藏]";

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// 值在哪里结束：带引号（含转义 `\"`）时到配对引号；否则到空白、`&`、`,`、`;`、`}`、`]`、`)`、引号为止。
fn value_span(text: &str, start: usize) -> (usize, usize) {
    let rest = &text[start..];
    if rest.starts_with(HIDDEN) {
        return (start, start + HIDDEN.len());
    }
    if rest.starts_with("\\\"") {
        let mut i = 2;
        while i < rest.len() {
            if rest[i..].starts_with("\\\"") {
                return (start + 2, start + i);
            }
            i += rest[i..].chars().next().map_or(1, char::len_utf8);
        }
        return (start + 2, text.len());
    }
    let mut chars = rest.char_indices();
    match chars.next() {
        Some((_, q @ ('"' | '\''))) => {
            let mut escaped = false;
            for (i, c) in rest.char_indices().skip(1) {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    return (start + 1, start + i);
                }
            }
            (start + 1, text.len())
        }
        Some(_) => {
            let end = rest
                .char_indices()
                .find(|(idx, c)| {
                    c.is_whitespace()
                        || rest[*idx..].starts_with("\\\"")
                        || matches!(
                            c,
                            '&' | ',' | ';' | '}' | ']' | ')' | '"' | '\'' | '<' | '>' | '#'
                        )
                })
                .map_or(text.len(), |(i, _)| start + i);
            (start, end)
        }
        None => (start, start),
    }
}

fn scan_url_len(s: &str) -> usize {
    let mut idx = 0;
    let mut in_ipv6 = s.starts_with('[');
    while idx < s.len() {
        if s[idx..].starts_with(HIDDEN) {
            idx += HIDDEN.len();
            continue;
        }
        let Some(c) = s[idx..].chars().next() else {
            break;
        };
        if in_ipv6 {
            idx += c.len_utf8();
            if c == ']' {
                in_ipv6 = false;
            }
            continue;
        }
        if c.is_whitespace()
            || s[idx..].starts_with("\\\"")
            || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']' | '}')
        {
            break;
        }
        idx += c.len_utf8();
    }
    idx
}

/// 若密钥类字段（如 `Authorization:`）后面先跟着 `Bearer` / `Basic` 方案名，跳过方案名只遮后面的真凭据。
fn skip_auth_scheme_prefix(text: &str, mut start: usize) -> usize {
    let rest = &text[start..];
    for scheme in ["bearer", "basic"] {
        if rest.len() > scheme.len()
            && rest
                .get(..scheme.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
        {
            let after = start + scheme.len();
            if text[after..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
            {
                let mut val_start = after;
                while let Some(ws) = text[val_start..]
                    .chars()
                    .next()
                    .filter(|c| c.is_whitespace())
                {
                    val_start += ws.len_utf8();
                }
                let (from, to) = value_span(text, val_start);
                if to > from {
                    start = val_start;
                }
                break;
            }
        }
    }
    start
}

fn find_ascii_case_insensitive(haystack: &str, from: usize, needle_lower: &str) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let needle = needle_lower.as_bytes();
    if needle.is_empty() || from + needle.len() > bytes.len() {
        return None;
    }
    bytes[from..]
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
        .map(|pos| from + pos)
}

/// 只把文字里的凭据片段换成 `[已隐藏]`，其余原样保留（n110：原先整段替换，审查后补齐引号、转义引号、冒号、
/// `Authorization: Bearer` 方案前缀、URL query/fragment、空白换行与无谓堆分配）。
fn redact_secret_spans(text: &str) -> String {
    let mut out = text.to_owned();
    // 1. `名字=值` / `名字: 值` / `"名字": "值"` / `\"名字\":\"值\"`：名字按字段名规则判断是不是密钥类
    let mut i = 0;
    while i < out.len() {
        let Some(c) = out[i..].chars().next() else {
            break;
        };
        if c == '=' || c == ':' {
            // 往回取名字（跳过紧挨着的空白、引号与转义反斜杠）
            let before = &out[..i];
            let trimmed = before
                .trim_end_matches(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '\\'));
            let name_start = trimmed
                .char_indices()
                .rev()
                .take_while(|(_, ch)| is_ident_char(*ch))
                .last()
                .map_or(trimmed.len(), |(idx, _)| idx);
            let name = &trimmed[name_start..];
            // `://` 里的冒号不是赋值
            let is_scheme = c == ':' && out[i..].starts_with("://");
            if !name.is_empty() && !is_scheme && is_secret_field(name) {
                let mut start = i + c.len_utf8();
                while let Some(ws) = out[start..].chars().next().filter(|ch| ch.is_whitespace()) {
                    start += ws.len_utf8();
                }
                start = skip_auth_scheme_prefix(&out, start);
                let (from, to) = value_span(&out, start);
                if to > from && &out[from..to] != HIDDEN {
                    out.replace_range(from..to, HIDDEN);
                    i = from + HIDDEN.len();
                    continue;
                }
            }
        }
        i += c.len_utf8();
    }
    // 2. `Bearer xxx` / `Basic xxx`：跳过空白；Bearer 后 6 个字符以上即换，
    //    Basic 要像编码过的凭据（含数字或 `=+/`，或 16 个字符以上），免得误伤 “basic shapes”
    for scheme in ["bearer", "basic"] {
        let mut from = 0;
        while let Some(pos) = find_ascii_case_insensitive(&out, from, scheme) {
            let boundary = pos == 0
                || !out[..pos]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_alphanumeric());
            let mut start = pos + scheme.len();
            let mut ws_bytes = 0usize;
            for ch in out[start..].chars() {
                if matches!(ch, ' ' | '\t') {
                    ws_bytes += ch.len_utf8();
                } else {
                    break;
                }
            }
            start += ws_bytes;
            let len: usize = out[start..]
                .chars()
                .take_while(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, '-' | '_' | '.' | '~' | '+' | '/' | '=' | ':')
                })
                .map(char::len_utf8)
                .sum();
            let token = &out[start..start + len];
            let looks_secret = if scheme == "bearer" {
                len >= 6
            } else {
                len >= 16
                    || (len >= 8
                        && token
                            .chars()
                            .any(|c| c.is_ascii_digit() || matches!(c, '=' | '+' | '/')))
            };
            if boundary && ws_bytes > 0 && looks_secret {
                out.replace_range(start..start + len, HIDDEN);
                from = start + HIDDEN.len();
            } else {
                from = pos + scheme.len();
            }
        }
    }
    // 3. URL 脱敏：`userinfo@`、`ws://`/`wss://` 全部 query 值与裸参数、以及任意 URL 的裸参数与 `#fragment`
    let mut from = 0;
    while let Some(pos) = out[from..].find("://").map(|p| p + from) {
        let scheme_start = out[..pos]
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            .last()
            .map_or(pos, |(idx, _)| idx);
        let is_ws = out[scheme_start..pos].eq_ignore_ascii_case("ws")
            || out[scheme_start..pos].eq_ignore_ascii_case("wss");
        let auth_start = pos + 3;
        let url_len = scan_url_len(&out[auth_start..]);
        let url_end = auth_start + url_len;
        let raw_rest = &out[auth_start..url_end];
        let auth_len = raw_rest.find(['/', '?', '#']).unwrap_or(raw_rest.len());
        let authority = &raw_rest[..auth_len];
        let mut rebuilt = String::with_capacity(raw_rest.len());
        if let Some(at) = authority.rfind('@') {
            rebuilt.push_str(HIDDEN);
            rebuilt.push('@');
            rebuilt.push_str(&authority[at + 1..]);
        } else {
            rebuilt.push_str(authority);
        }
        let after_auth = &raw_rest[auth_len..];
        let (path_and_query, frag_opt) = match after_auth.split_once('#') {
            Some((pq, frag)) => (pq, Some(frag)),
            None => (after_auth, None),
        };
        if let Some((path, query)) = path_and_query.split_once('?') {
            rebuilt.push_str(path);
            rebuilt.push('?');
            let masked_segs: Vec<String> = query
                .split('&')
                .map(|seg| {
                    if seg.is_empty() {
                        String::new()
                    } else if let Some((k, v)) = seg.split_once('=') {
                        if v.is_empty() || v == HIDDEN || (!is_ws && !is_secret_field(k)) {
                            seg.to_owned()
                        } else if k.is_empty() {
                            HIDDEN.to_owned()
                        } else {
                            format!("{k}={HIDDEN}")
                        }
                    } else if seg == HIDDEN {
                        seg.to_owned()
                    } else {
                        HIDDEN.to_owned()
                    }
                })
                .collect();
            rebuilt.push_str(&masked_segs.join("&"));
        } else {
            rebuilt.push_str(path_and_query);
        }
        if let Some(frag) = frag_opt {
            rebuilt.push('#');
            if frag.is_empty() || frag == HIDDEN {
                rebuilt.push_str(frag);
            } else {
                rebuilt.push_str(HIDDEN);
            }
        }
        if rebuilt != raw_rest {
            out.replace_range(auth_start..url_end, &rebuilt);
            from = auth_start + rebuilt.len();
        } else {
            from = url_end;
        }
    }
    // 4. `sk-…` 形式的 API Key（先快速扫是否存在 `sk-`，避免对大块无密钥文本分配 Vec<char>）
    if find_ascii_case_insensitive(&out, 0, "sk-").is_none() {
        return out;
    }
    let chars: Vec<char> = out.chars().collect();
    let mut result = String::with_capacity(out.len());
    let mut i = 0;
    while i < chars.len() {
        let starts = (chars[i] == 's' || chars[i] == 'S')
            && chars.get(i + 1).is_some_and(|c| *c == 'k' || *c == 'K')
            && chars.get(i + 2) == Some(&'-')
            && (i == 0 || !chars[i - 1].is_ascii_alphanumeric());
        if starts {
            let len = chars[i + 3..]
                .iter()
                .take_while(|c| c.is_ascii_alphanumeric() || **c == '-' || **c == '_')
                .count();
            if len >= 8 {
                result.push_str(HIDDEN);
                i += 3 + len;
                continue;
            }
        }
        result.push(chars[i]);
        i += 1;
    }
    result
}

/// 文字里有没有会被脱敏的凭据片段。
fn contains_secret_pattern(text: &str) -> bool {
    redact_secret_spans(text) != text
}

/// 脱敏单条文本（含 `step` 与错误串），确保凭据绝不漏进 `timeline.jsonl` / `timeline.txt`；不截断。
pub(crate) fn scrub_trace_text(text: &str) -> String {
    redact_secret_spans(text)
}

/// 去掉凭据（递归）：密钥类字段的值不论是字符串、数字、数组还是对象都整个隐藏，其余字符串按文字规则
/// 只换凭据片段。大块字段应在脱敏之后经 [`externalize_bulky`] 另存成附件；没另存的（如加工反馈里的）只留字节数，
/// 免得一整张截图塞进时间线的一行。
pub(crate) fn scrub_trace_value(value: &mut serde_json::Value) {
    omit_bulky_strings(value);
    redact_secret_values(value, false);
}

/// 只去凭据，不动大块字段（`skip_bulky = true` 时跳过即将交给 [`externalize_bulky`] 另存的大块非密钥字段，
/// 避免对 20 MiB 上传体重复做全文脱敏）。
fn redact_secret_values(value: &mut serde_json::Value, skip_bulky: bool) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                let filled = match child {
                    serde_json::Value::String(s) => !s.is_empty(),
                    serde_json::Value::Number(_) => true,
                    serde_json::Value::Array(items) => !items.is_empty(),
                    serde_json::Value::Object(inner) => !inner.is_empty(),
                    _ => false,
                };
                if filled && is_secret_field(key) {
                    *child = serde_json::json!(HIDDEN);
                } else if skip_bulky && BULKY_KEYS.contains(&key.as_str()) {
                    // 非密钥名的大块字段交由 save_bulky 单次解码或脱敏另存
                } else {
                    redact_secret_values(child, skip_bulky);
                }
            }
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .for_each(|item| redact_secret_values(item, skip_bulky)),
        serde_json::Value::String(text) => {
            let scrubbed = redact_secret_spans(text);
            if scrubbed != *text {
                *text = scrubbed;
            }
        }
        _ => {}
    }
}

fn omit_bulky_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if BULKY_KEYS.contains(&key.as_str()) && (child.is_string() || child.is_array()) {
                    let bytes = match &*child {
                        serde_json::Value::String(s) => s.len(),
                        other => other.to_string().len(),
                    };
                    *child = serde_json::json!(format!("[已省略，{bytes} 字节]"));
                } else {
                    omit_bulky_strings(child);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(omit_bulky_strings),
        _ => {}
    }
}

/// 发起方的中文说法。
pub(crate) fn origin_label(origin: &str) -> &'static str {
    match origin {
        "desktop" => "电脑工作台",
        "voice" => "内置语音",
        "mcp_bridge" => "小智接入点",
        "gateway" => "网关",
        "lan" => "手机 / 局域网",
        "approval" => "审批后执行",
        "draw" => "创作画板",
        "loopback" => "本机",
        _ => "其他",
    }
}

/// 激光流程标签：有任务编号时带上编号。
pub(crate) fn laser_flow(workflow_id: Option<&str>) -> String {
    match workflow_id.filter(|id| !id.is_empty()) {
        Some(id) => format!("激光·{id}"),
        None => "激光".to_owned(),
    }
}

fn short(text: &str, max: usize) -> String {
    let flat = text.replace(['\r', '\n'], " ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

fn str_field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// 接口调用这一步在干什么（一句中文）。
pub(crate) fn api_step_label(method: &str, route: &str, body: &serde_json::Value) -> String {
    let action = str_field(body, "action").unwrap_or("");
    match route {
        "/api/connection/check" => "检查激光机连接（握手）".into(),
        "/api/text-task/generate" => "请求生成文字刻图预览".into(),
        "/api/draw/preview" => "创作画板请求生成预览".into(),
        "/api/upload" => "上传文件".into(),
        "/api/workflow" => match action {
            "preview" | "plan" | "create" => "请求生成图片 / G-code 预览".into(),
            "confirm_send" | "send" => "确认发送到激光机".into(),
            "cancel" => "取消任务".into(),
            "frame" => "按预览走边框".into(),
            "resume" => "断点续雕".into(),
            "" => "任务操作".into(),
            other => format!("任务操作：{other}"),
        },
        "/api/safe-action" => match action {
            "emergency_stop" => "急停（实时 0x18）".into(),
            "soft_reset" => "软复位".into(),
            "laser_off" => "关激光".into(),
            // pause / resume 只用于走边框（正式加工禁用，safety.md §2）
            "pause" => "暂停走边框".into(),
            "resume" => "继续走边框".into(),
            "stop_framing" => "停止走边框".into(),
            "home" => "回原点".into(),
            "unlock" => "解锁（$X）".into(),
            "set_origin" => "把当前位置设为工作原点".into(),
            "relative_move" => "移动激光头（相对当前位置）".into(),
            "absolute_move" => "移动激光头到指定坐标".into(),
            "status" => "查询激光机状态".into(),
            a if a.starts_with("feed_") => format!("调整进给倍率（{a}）"),
            a if a.starts_with("spindle_") || a.starts_with("power_") => {
                format!("调整功率倍率（{a}）")
            }
            "" => "安全动作".into(),
            other => format!("安全动作：{other}"),
        },
        "/api/device/framing" => "走边框".into(),
        "/api/registration/preview" => "两点对位：按读到的两点生成对位预览".into(),
        "/api/draw/state" => "画板自动同步编辑内容".into(),
        "/api/device/pointer-fire" => "低功率点射".into(),
        "/api/jobs/acknowledge-stop" => "确认激光机已停机".into(),
        "/api/settings" => {
            let keys: Vec<&str> = body
                .as_object()
                .map(|m| m.keys().map(String::as_str).collect())
                .unwrap_or_default();
            format!("修改设置：{}", keys.join("、"))
        }
        "/api/feedback" | "/api/jev/feedback" => "提交加工效果反馈".into(),
        "/api/feedback/accept" => "采用反馈建议的参数".into(),
        "/api/time-calibration" => "清除工时校准".into(),
        "/api/engrave-route" => "切换雕刻路线（矢量 / 转图片）".into(),
        "/api/text-origin" => "修改文字对刀起点".into(),
        r if r.starts_with("/v1/approvals") => match action {
            "" => format!("审批操作（{method} {r}）"),
            other => format!("审批操作：{other}"),
        },
        r if r.starts_with("/api/material-lab") => match action {
            "" => format!("材料实验室：{r}"),
            other => format!("材料实验室：{other}"),
        },
        r if r.starts_with("/api/materials") || r.starts_with("/api/material") => {
            format!("材料库修改（{r}）")
        }
        r => format!("{method} {r}"),
    }
}

/// LB4：测试脚本通过请求头 `X-Moss-Bench` 传递的过程记录来源标签（仅作标签，绝不授予任何权限）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BenchTraceSource {
    Readonly,
    Dryrun,
}

impl BenchTraceSource {
    pub(crate) fn from_header(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "readonly" => Some(Self::Readonly),
            "dryrun" => Some(Self::Dryrun),
            _ => None,
        }
    }

    pub(crate) const fn source_key(self) -> &'static str {
        match self {
            Self::Readonly => "ai_laser_bench_readonly",
            Self::Dryrun => "ai_laser_bench_dryrun",
        }
    }

    pub(crate) const fn display_label(self) -> &'static str {
        match self {
            Self::Readonly => "AI 激光测试 · 只读核对",
            Self::Dryrun => "AI 激光测试 · 空跑",
        }
    }
}

thread_local! {
    static CURRENT_BENCH_SOURCE: std::cell::Cell<Option<BenchTraceSource>> = const { std::cell::Cell::new(None) };
}

pub(crate) struct BenchTraceScope {
    prev: Option<BenchTraceSource>,
}

impl Drop for BenchTraceScope {
    fn drop(&mut self) {
        CURRENT_BENCH_SOURCE.with(|cell| cell.set(self.prev));
    }
}

pub(crate) fn enter_bench_trace_scope(source: Option<BenchTraceSource>) -> BenchTraceScope {
    let prev = CURRENT_BENCH_SOURCE.with(|cell| cell.replace(source));
    BenchTraceScope { prev }
}

pub(crate) fn current_bench_source() -> Option<BenchTraceSource> {
    CURRENT_BENCH_SOURCE.with(std::cell::Cell::get)
}

/// LB4：按请求头来源与当前空跑模式，为激光记录补齐 `source` / `dry_run` 字段与 `timeline.txt` 可读前缀。
pub(crate) fn apply_laser_trace_tags(
    step: &str,
    fields: &mut serde_json::Value,
    bench_source: Option<BenchTraceSource>,
    dry_run: bool,
) -> String {
    if bench_source.is_none() && !dry_run {
        return step.to_owned();
    }
    let mut tags: Vec<&'static str> = Vec::with_capacity(2);
    if let Some(src) = bench_source {
        if let Some(map) = fields.as_object_mut() {
            map.insert("source".into(), serde_json::json!(src.source_key()));
        }
        tags.push(src.display_label());
    }
    if dry_run {
        if let Some(map) = fields.as_object_mut() {
            map.insert("dry_run".into(), serde_json::json!(true));
        }
        tags.push("空跑模式");
    }
    format!("[{}] {step}", tags.join(" · "))
}

/// 这次接口调用要不要进时间线：只记改动类请求，跳过凭据接口与只读轮询；
/// 带 `X-Moss-Bench` 标签的测试脚本调用 `status` 也记录，便于区分 L1/L2 测试步骤。
pub(crate) fn should_trace_api_with_bench(
    method: &str,
    route: &str,
    body: &serde_json::Value,
    bench_source: Option<BenchTraceSource>,
) -> bool {
    if matches!(method, "GET" | "HEAD" | "OPTIONS") || UNTRACED_ROUTES.contains(&route) {
        return false;
    }
    if route == "/api/workflow" && str_field(body, "action").is_none() {
        return false;
    }
    if bench_source.is_some()
        && route == "/api/safe-action"
        && str_field(body, "action") == Some("status")
    {
        return true;
    }
    !str_field(body, "action").is_some_and(|action| READ_ONLY_ACTIONS.contains(&action))
}

fn response_status(response: &[u8]) -> u16 {
    std::str::from_utf8(&response[..response.len().min(16)])
        .ok()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

/// 加工线程里记一步（发送线程拿不到 `ApiService`，记录器随参数传进来）。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn trace_job_step(
    trace: Option<&laser_runtime::ProcessTrace>,
    workflow_id: &str,
    kind: &str,
    step: &str,
    fields: serde_json::Value,
) {
    trace_job_step_with_meta(trace, workflow_id, kind, step, fields, None, false);
}

pub(crate) fn trace_job_step_with_meta(
    trace: Option<&laser_runtime::ProcessTrace>,
    workflow_id: &str,
    kind: &str,
    step: &str,
    mut fields: serde_json::Value,
    bench_source: Option<BenchTraceSource>,
    dry_run: bool,
) {
    let Some(trace) = trace.filter(|t| t.enabled()) else {
        return;
    };
    scrub_trace_value(&mut fields);
    let safe_step =
        apply_laser_trace_tags(&scrub_trace_text(step), &mut fields, bench_source, dry_run);
    trace.record(
        "laser",
        &laser_flow(Some(workflow_id)),
        kind,
        &safe_step,
        fields,
    );
}

/// LC：空跑传输层兜底拦截到 `G28`/`G30`/`G93`/`G20` 或未改写固件文件/宏执行指令时记一条带来源标签的告警事件。
pub(crate) fn trace_dry_run_blocked_with_meta(
    trace: Option<&laser_runtime::ProcessTrace>,
    hit: &laser_device::DryRunBlockedCommand,
    bench_source: Option<BenchTraceSource>,
) {
    let Some(trace) = trace.filter(|t| t.enabled()) else {
        return;
    };
    let safe_line = scrub_trace_text(&hit.original_line);
    let replacement = hit.blocked_comment();
    let mut fields = serde_json::json!({
        "level": "warn",
        "code": hit.code,
        "error_code": hit.error_code(),
        "original_line": safe_line,
        "replacement": replacement,
    });
    let raw_step = format!(
        "空跑传输层兜底拦截 {}：已替换为 `{replacement}`（原指令：{}）",
        hit.code,
        short(&safe_line, 120)
    );
    let safe_step = apply_laser_trace_tags(&raw_step, &mut fields, bench_source, true);
    trace.record(
        "laser",
        &laser_flow(None),
        "dry_run_blocked",
        &safe_step,
        fields,
    );
}

/// `trace_api_begin` 交给 `trace_api_end` 的上下文。
pub(crate) struct ApiTraceCall {
    trace: laser_runtime::ProcessTrace,
    call: u64,
    label: String,
    origin: String,
    workflow_id: Option<String>,
    summary_only: bool,
    bench_source: Option<BenchTraceSource>,
    dry_run: bool,
}

/// 大请求 / 响应体：复用已解析并脱敏的 JSON 另存大块字段与整份附件；
/// 解析不了的（上传的二进制文件等）原样存，文字里有凭据片段时不存并写明原因。
fn save_large_body(
    trace: &laser_runtime::ProcessTrace,
    stem: &str,
    body: &[u8],
    parsed: Option<serde_json::Value>,
) -> serde_json::Value {
    if let Some(mut parsed) = parsed {
        externalize_bulky(trace, stem, &mut parsed);
        let text = parsed.to_string();
        let file = trace.save_file("laser", stem, "json", text.as_bytes());
        return serde_json::json!({"body_bytes": body.len(), "body_file": file});
    }
    if contains_secret_pattern(&String::from_utf8_lossy(body)) {
        return serde_json::json!({"body_bytes": body.len(), "body_file": null, "reason": "内容里有凭据片段，没有另存"});
    }
    let file = trace.save_file("laser", stem, "bin", body);
    serde_json::json!({"body_bytes": body.len(), "body_file": file})
}

/// 急停类等必须先执行的请求：开始那一步在执行后补记（`received_at` 写收到时刻），
/// 执行前不读开关、不抢记录锁、不解析请求体（n110 审查修复：记录不能排在 `0x18` 前面）。
pub(crate) fn must_run_before_trace(route: &str, body: &[u8]) -> bool {
    match route {
        "/api/safe-action" | "/api/jobs/acknowledge-stop" => true,
        "/api/workflow" => body.windows(8).any(|w| w == b"\"cancel\""),
        _ => false,
    }
}

impl ApiService {
    fn trace_on(&self) -> Option<laser_runtime::ProcessTrace> {
        self.process_trace()
            .filter(|trace| trace.enabled())
            .map(laser_runtime::ProcessTrace::scoped)
    }

    /// 改动类接口调用开始处理：记一步“谁发起、要做什么、参数”。返回给 [`Self::trace_api_end`] 用的上下文；
    /// 开关关着、只读轮询或凭据接口时返回 `None`。`received_at` 为补记时收到请求的时刻（先执行后补记的
    /// 急停类请求、或这次调用本身打开了记录开关）。
    pub(crate) fn trace_api_begin(
        &self,
        method: &str,
        route: &str,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
        received_at: Option<laser_runtime::process_trace::TraceTimestamp>,
    ) -> Option<ApiTraceCall> {
        let trace = self.trace_on()?;
        if matches!(method, "GET" | "HEAD" | "OPTIONS") || UNTRACED_ROUTES.contains(&route) {
            return None;
        }
        let large = body.len() > LARGE_BODY_BYTES;
        let summary_only = SUMMARY_ONLY_ROUTES.contains(&route);
        let bench_source = current_bench_source();
        let dry_run = self.is_dry_run_active();
        if summary_only {
            // 画板高频自动同步：不在 HTTP 同步线程解析或扫描数 MB 请求体，直接记录字节数摘要
            let call = API_CALL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let label = api_step_label(method, route, &serde_json::Value::Null);
            let origin = ctx.label();
            let late = received_at
                .map(|at| format!("，先执行后补记，收到于 {}", at.format("%H:%M:%S%.3f")))
                .unwrap_or_default();
            let mut fields = serde_json::json!({
                "call": call,
                "method": method,
                "route": route,
                "origin": origin,
                "received_at": received_at.map(laser_runtime::process_trace::format_timestamp),
                "request": {"body_bytes": body.len(), "note": "画板自动同步：只记大小，作品在生成预览时整份另存"},
            });
            let step = apply_laser_trace_tags(
                &format!(
                    "{}发起：{label}（开始处理，调用 #{call}{late}）",
                    origin_label(origin)
                ),
                &mut fields,
                bench_source,
                dry_run,
            );
            trace.record("laser", &laser_flow(None), "api_begin", &step, fields);
            return Some(ApiTraceCall {
                trace,
                call,
                label,
                origin: origin.to_owned(),
                workflow_id: None,
                summary_only: true,
                bench_source,
                dry_run,
            });
        }
        // 只解析一次（大块非密钥字段在 redact_secret_values(skip_bulky=true) 跳过前置扫描，交由 save_bulky 处理一次）
        let raw_json = serde_json::from_slice::<serde_json::Value>(body).ok();
        let mut labeled = match raw_json {
            Some(v) => v,
            None if large => serde_json::Value::Null,
            None => serde_json::Value::String(scrub_trace_text(&String::from_utf8_lossy(body))),
        };
        if !should_trace_api_with_bench(method, route, &labeled, bench_source) {
            return None;
        }
        let call = API_CALL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let was_json = !labeled.is_null() || body.trim_ascii() == b"null";
        redact_secret_values(&mut labeled, true);
        let label = api_step_label(method, route, &labeled);
        let workflow_id = str_field(&labeled, "workflow_id").map(str::to_owned);
        let request = if large {
            save_large_body(
                &trace,
                &format!("api{call}-request"),
                body,
                was_json.then_some(labeled),
            )
        } else {
            externalize_bulky(&trace, &format!("api{call}"), &mut labeled);
            labeled
        };
        let origin = ctx.label();
        let late = received_at
            .map(|at| format!("，先执行后补记，收到于 {}", at.format("%H:%M:%S%.3f")))
            .unwrap_or_default();
        let mut fields = serde_json::json!({
            "call": call,
            "method": method,
            "route": route,
            "origin": origin,
            "received_at": received_at.map(laser_runtime::process_trace::format_timestamp),
            "request": request,
        });
        let step = apply_laser_trace_tags(
            &format!(
                "{}发起：{label}（开始处理，调用 #{call}{late}）",
                origin_label(origin)
            ),
            &mut fields,
            bench_source,
            dry_run,
        );
        trace.record(
            "laser",
            &laser_flow(workflow_id.as_deref()),
            "api_begin",
            &step,
            fields,
        );
        Some(ApiTraceCall {
            trace,
            call,
            label,
            origin: origin.to_owned(),
            workflow_id,
            summary_only,
            bench_source,
            dry_run,
        })
    }

    /// 接口处理完：记结果与耗时（与开始那一步用同一个 `call` 编号）。开始那一步记下了就一定补上这一步，
    /// 即使处理中开关刚被关掉（不留没有结果的“开始处理”）。
    pub(crate) fn trace_api_end(
        &self,
        begun: Option<ApiTraceCall>,
        method: &str,
        route: &str,
        response: &[u8],
        elapsed_ms: u64,
    ) {
        let Some(begun) = begun else {
            return;
        };
        let trace = &begun.trace;
        let status = response_status(response);
        let body_start = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map_or(0, |p| p + 4);
        let dry_run = begun.dry_run || self.is_dry_run_active();
        if begun.summary_only && (200..300).contains(&status) {
            let mut fields = serde_json::json!({
                "call": begun.call,
                "method": method,
                "route": route,
                "origin": begun.origin,
                "status": status,
                "response": {"body_bytes": response.len() - body_start},
                "elapsed_ms": elapsed_ms,
            });
            let step = apply_laser_trace_tags(
                &format!(
                    "{}发起的{} → 成功（{elapsed_ms} ms，调用 #{}）",
                    origin_label(&begun.origin),
                    begun.label,
                    begun.call
                ),
                &mut fields,
                begun.bench_source,
                dry_run,
            );
            trace.record_closing(
                "laser",
                &laser_flow(begun.workflow_id.as_deref()),
                "api_call",
                &step,
                fields,
            );
            return;
        }
        let raw_parsed = response_body(response);
        let was_json = raw_parsed.is_some();
        let mut parsed = raw_parsed.unwrap_or(serde_json::Value::Null);
        redact_secret_values(&mut parsed, true);
        let workflow_id = begun.workflow_id.clone().or_else(|| {
            parsed
                .pointer("/result/workflow_id")
                .or_else(|| parsed.get("workflow_id"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        });
        let outcome = if (200..300).contains(&status) {
            match parsed.get("result").and_then(|r| r.as_str()) {
                Some(text) if !text.is_empty() => format!("成功：{}", short(text, 120)),
                _ => "成功".to_owned(),
            }
        } else {
            let detail = parsed
                .get("result")
                .and_then(|r| r.as_str())
                .map(|t| short(t, 160))
                .unwrap_or_default();
            format!("失败（HTTP {status}）：{detail}")
        };
        let reply = if begun.summary_only {
            serde_json::json!({"body_bytes": response.len() - body_start})
        } else if response.len() > LARGE_BODY_BYTES {
            save_large_body(
                trace,
                &format!("api{}-response", begun.call),
                &response[body_start..],
                was_json.then_some(parsed),
            )
        } else {
            externalize_bulky(trace, &format!("api{}-response", begun.call), &mut parsed);
            parsed
        };
        let mut fields = serde_json::json!({
            "call": begun.call,
            "method": method,
            "route": route,
            "origin": begun.origin,
            "status": status,
            "response": reply,
            "elapsed_ms": elapsed_ms,
        });
        let step = apply_laser_trace_tags(
            &format!(
                "{}发起的{} → {outcome}（{elapsed_ms} ms，调用 #{}）",
                origin_label(&begun.origin),
                begun.label,
                begun.call
            ),
            &mut fields,
            begun.bench_source,
            dry_run,
        );
        trace.record_closing(
            "laser",
            &laser_flow(workflow_id.as_deref()),
            "api_call",
            &step,
            fields,
        );
    }

    /// 生成成功：记参数、发起方、结果，并复制产物文件。
    pub(crate) fn trace_generation(
        &self,
        route: &str,
        origin: &str,
        workflow_id: Option<&str>,
        object: &serde_json::Map<String, serde_json::Value>,
        artifacts: &serde_json::Value,
    ) {
        let Some(trace) = self.trace_on() else {
            return;
        };
        let mut files = Vec::new();
        let mut saved_names = Vec::new();
        for key in ARTIFACT_FILE_KEYS {
            let Some(name) = artifacts.get(*key).and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(source) = self.find_artifact_file(name) else {
                files.push(serde_json::json!({"field": key, "name": name, "saved": null}));
                continue;
            };
            let stem = format!("{}-{key}", workflow_id.unwrap_or("preview"));
            let saved = trace.copy_file("laser", &stem, &source);
            if let Some(path) = &saved {
                saved_names.push(path.clone());
            }
            files.push(serde_json::json!({"field": key, "name": name, "saved": saved}));
        }
        // 截图、画板场景等大块字段在这次接口调用的 `api_begin` 里已另存成附件，这里只留字节数；
        // 内部产物清单 `_artifacts` 与下面的 `artifacts` 重复，不再记
        let mut input = serde_json::Value::Object(object.clone());
        if let Some(map) = input.as_object_mut() {
            map.remove("_artifacts");
        }
        scrub_trace_value(&mut input);
        let mut clean_artifacts = artifacts.clone();
        scrub_trace_value(&mut clean_artifacts);
        let what = match object.get("source_type").and_then(|v| v.as_str()) {
            Some("image") => "图片".to_owned(),
            Some("gcode") => "导入的 G-code".to_owned(),
            _ if route == "/api/draw/preview" => "画板作品".to_owned(),
            _ => str_field(&input, "text")
                .map(|t| format!("文字「{}」", short(t, 30)))
                .unwrap_or_else(|| "文字".to_owned()),
        };
        let material = [
            str_field(&clean_artifacts, "material").or_else(|| str_field(&input, "material")),
            clean_artifacts
                .get("thickness_mm")
                .or_else(|| input.get("thickness_mm"))
                .and_then(|v| v.as_f64())
                .map(|mm| format!("{mm}mm"))
                .as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
        let step = format!(
            "生成预览完成（{}）：{what}{}，产物已存 {} 个文件{}",
            origin_label(origin),
            if material.is_empty() {
                String::new()
            } else {
                format!("，{material}")
            },
            saved_names.len(),
            if saved_names.is_empty() {
                String::new()
            } else {
                format!("：{}", saved_names.join("、"))
            }
        );
        let mut fields = serde_json::json!({
            "route": route,
            "origin": origin,
            "workflow_id": workflow_id,
            "source_type": object.get("source_type").cloned().unwrap_or(serde_json::Value::Null),
            "input": input,
            "artifacts": clean_artifacts,
            "files": files,
        });
        let step = apply_laser_trace_tags(
            &step,
            &mut fields,
            current_bench_source(),
            self.is_dry_run_active(),
        );
        trace.record("laser", &laser_flow(workflow_id), "generate", &step, fields);
    }

    /// 生成失败：记参数与脱敏后的错误原文。
    pub(crate) fn trace_generation_failed(
        &self,
        route: &str,
        origin: &str,
        object: &serde_json::Map<String, serde_json::Value>,
        error: &str,
    ) {
        let Some(trace) = self.trace_on() else {
            return;
        };
        let mut input = serde_json::Value::Object(object.clone());
        if let Some(map) = input.as_object_mut() {
            map.remove("_artifacts");
        }
        scrub_trace_value(&mut input);
        let safe_error = scrub_trace_text(error);
        let flow = laser_flow(str_field(&input, "workflow_id"));
        let mut fields = serde_json::json!({
            "route": route,
            "origin": origin,
            "input": input,
            "error": safe_error,
        });
        let step = apply_laser_trace_tags(
            &format!(
                "生成预览失败（{}）：{}",
                origin_label(origin),
                short(&safe_error, 200)
            ),
            &mut fields,
            current_bench_source(),
            self.is_dry_run_active(),
        );
        trace.record("laser", &flow, "generate_failed", &step, fields);
    }

    /// 只在生成目录、上传目录与预览目录里按文件名找（不接受路径，防止越界）。
    fn find_artifact_file(&self, name: &str) -> Option<std::path::PathBuf> {
        if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
            return None;
        }
        self.generated_root
            .iter()
            .chain(self.upload_root.iter())
            .chain(self.preview_roots.iter())
            .map(|root| root.join(name))
            .find(|path| path.is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_id_is_read_from_http_response() {
        let response = http_json(
            200,
            success(serde_json::json!({"workflow_id": "wf-1", "status": "preview_ready"})),
        );
        assert_eq!(response_workflow_id(&response).as_deref(), Some("wf-1"));
        let top = http_json(200, serde_json::json!({"workflow_id": "wf-2"}));
        assert_eq!(response_workflow_id(&top).as_deref(), Some("wf-2"));
        assert_eq!(
            response_workflow_id(b"HTTP/1.1 200 OK\r\n\r\nnot json"),
            None
        );
        assert_eq!(response_failure(&response), None);
        let conflict = http_json(409, failure("这个预览正在发送", None));
        let error = response_failure(&conflict).unwrap();
        assert!(
            error.contains("409") && error.contains("这个预览正在发送"),
            "{error}"
        );
    }

    #[test]
    fn generation_is_traced_only_when_enabled_with_copied_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        let generated = tmp.path().join("generated_assets");
        std::fs::create_dir_all(&generated).unwrap();
        std::fs::write(generated.join("text-1.gcode"), b"G0 X0\n").unwrap();
        std::fs::write(generated.join("text-1.png"), b"PNG").unwrap();
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_generated_root(generated.clone())
        .with_runtime_path(tmp.path().to_path_buf());
        let mut object = serde_json::Map::new();
        object.insert("text".into(), serde_json::json!("你好"));
        object.insert(
            "image_data_url".into(),
            serde_json::json!("data:image/png;base64,AAAA"),
        );
        let artifacts = serde_json::json!({
            "gcode_file": "text-1.gcode",
            "image_file": "text-1.png",
            "scene_file": "../escape.json",
            "gcode_sha256": "abc",
        });
        let trace_dir = tmp.path().join(laser_runtime::process_trace::TRACE_DIR);

        service.trace_generation(
            "/api/text-task/generate",
            "voice",
            Some("wf-1"),
            &object,
            &artifacts,
        );
        assert!(!trace_dir.exists(), "开关关着时不写");

        laser_runtime::settings_store::update(
            &laser_runtime::settings_store::settings_path(tmp.path()),
            |map| {
                map.insert(
                    laser_runtime::process_trace::SETTING_KEY.into(),
                    serde_json::json!(true),
                );
                Ok(())
            },
        )
        .unwrap();
        service.process_trace().unwrap().invalidate();
        service.trace_generation(
            "/api/text-task/generate",
            "voice",
            Some("wf-1"),
            &object,
            &artifacts,
        );
        service.trace_generation_failed("/api/text-task/generate", "voice", &object, "字太多");
        service.process_trace().unwrap().flush();

        let day = std::fs::read_dir(&trace_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let text = std::fs::read_to_string(day.join(laser_runtime::process_trace::TIMELINE_JSONL))
            .unwrap();
        let mut lines = text
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap());
        let generate = lines.next().unwrap();
        assert_eq!(generate["kind"], "generate");
        assert_eq!(generate["flow"], "激光·wf-1");
        assert!(
            generate["step"]
                .as_str()
                .unwrap()
                .starts_with("生成预览完成（内置语音）：文字「你好」，产物已存 2 个文件"),
            "{generate}"
        );
        assert_eq!(generate["origin"], "voice");
        assert_eq!(generate["workflow_id"], "wf-1");
        assert_eq!(generate["input"]["text"], "你好");
        // n110：截图在这次接口调用的 api_begin 里已另存成附件，生成这一步只留字节数，不再存第二份
        assert_eq!(generate["input"]["image_data_url"], "[已省略，26 字节]");
        assert_eq!(generate["artifacts"]["gcode_sha256"], "abc");
        let files = generate["files"].as_array().unwrap();
        let saved: Vec<_> = files.iter().filter_map(|f| f["saved"].as_str()).collect();
        assert_eq!(saved.len(), 2, "G-code 与预览图各复制一份：{files:?}");
        assert!(
            generate["step"]
                .as_str()
                .unwrap()
                .contains("产物已存 2 个文件")
        );
        let gcode_copy = trace_dir.join(saved.iter().find(|s| s.ends_with(".gcode")).unwrap());
        assert_eq!(std::fs::read(gcode_copy).unwrap(), b"G0 X0\n");
        assert!(
            files
                .iter()
                .any(|f| f["field"] == "scene_file" && f["saved"].is_null()),
            "带路径的文件名不复制"
        );
        let failed = lines.next().unwrap();
        assert_eq!(failed["kind"], "generate_failed");
        assert_eq!(failed["error"], "字太多");
    }

    #[test]
    fn scrubber_redacts_urls_embedded_keys_numeric_pins_and_step_summaries() {
        let long = "长".repeat(5000);
        let mut payload = serde_json::json!({
            "lan_static_pin": 123456,
            "jev_delete_api_key": true,
            "jevApiKey": "abc123",
            "mcp_endpoint": "wss://user:pass@mcp.example.com/ws",
            "spindle_max": 1000,
            "keywords": "停,退下",
            "clipping": "on",
            "nested": [
                "上游返回 Bearer abcdef1234567890",
                "上游失败：invalid key sk-or-v1-secret1234567890",
                "failed calling https://api.example.com via http://user:pass@proxy.local:8080",
                "wss://single-secret-token@mcp.example.com/ws",
                "正常提示文字保留",
                "basic shapes 用 basic 模式",
                "https://x.test/cb?token=abc123456&mode=raster",
                long.clone(),
            ],
        });
        scrub_trace_value(&mut payload);
        assert_eq!(payload["lan_static_pin"], "[已隐藏]");
        assert_eq!(payload["jevApiKey"], "[已隐藏]", "驼峰字段名也按词判断");
        assert_eq!(
            payload["jev_delete_api_key"], true,
            "布尔控制开关不被误掩码"
        );
        // n110：只换凭据片段，其余原样保留；按整词判断字段名，不误伤 spindle / keywords / clipping
        assert_eq!(payload["mcp_endpoint"], "wss://[已隐藏]@mcp.example.com/ws");
        assert_eq!(payload["spindle_max"], 1000);
        assert_eq!(payload["keywords"], "停,退下");
        assert_eq!(payload["clipping"], "on");
        assert_eq!(payload["nested"][0], "上游返回 Bearer [已隐藏]");
        assert_eq!(payload["nested"][1], "上游失败：invalid key [已隐藏]");
        assert_eq!(
            payload["nested"][2],
            "failed calling https://api.example.com via http://[已隐藏]@proxy.local:8080"
        );
        assert_eq!(payload["nested"][3], "wss://[已隐藏]@mcp.example.com/ws");
        assert_eq!(payload["nested"][4], "正常提示文字保留");
        assert_eq!(
            payload["nested"][5], "basic shapes 用 basic 模式",
            "不是令牌的 basic 不动"
        );
        assert_eq!(
            payload["nested"][6],
            "https://x.test/cb?token=[已隐藏]&mode=raster"
        );
        assert_eq!(payload["nested"][7], long.as_str(), "长文字不截断");

        // 审查补的漏网情况：引号、冒号、任意密钥类参数名、Bearer 多空格、缩写驼峰字段名、密钥字段下的数组
        for (raw, want) in [
            (r#"api_key="sec123" 失败"#, r#"api_key="[已隐藏]" 失败"#),
            ("password=p@ss!123 登录失败", "password=[已隐藏] 登录失败"),
            (
                "https://x.example/cb?auth=sec123&v=1",
                "https://x.example/cb?auth=[已隐藏]&v=1",
            ),
            (
                "wss://x.example/ws?auth=sec123&v=1",
                "wss://x.example/ws?auth=[已隐藏]&v=[已隐藏]",
            ),
            ("api_key: abcdef 无效", "api_key: [已隐藏] 无效"),
            (
                "Authorization: Bearer   123:abcdef9",
                "Authorization: Bearer   [已隐藏]",
            ),
            (
                "Authorization:\tBasic\tdXNlcjpwYXNzMTIzNA==",
                "Authorization:\tBasic\t[已隐藏]",
            ),
            (
                "wss://example.com/mcp?k=sec_val&bare_tok#frag_sec",
                "wss://example.com/mcp?k=[已隐藏]&[已隐藏]#[已隐藏]",
            ),
            (
                r#"{\"jev_api_key\":\"tvly-escaped\"}"#,
                r#"{\"jev_api_key\":\"[已隐藏]\"}"#,
            ),
            (
                "\"api_key\":\n  \"multiline-sec\"",
                "\"api_key\":\n  \"[已隐藏]\"",
            ),
            (
                r#"{"jev_api_key":"tvly-xxx",}"#,
                r#"{"jev_api_key":"[已隐藏]",}"#,
            ),
            ("模型 basic 版本 2", "模型 basic 版本 2"),
        ] {
            assert_eq!(scrub_trace_text(raw), want, "{raw}");
        }
        let mut fields = serde_json::json!({
            "APIToken": "t1",
            "openAIKey": "k1",
            "apiKeys": ["a", "b"],
            "cookies": {"sid": "x"},
            "spindle": 1,
        });
        scrub_trace_value(&mut fields);
        assert_eq!(
            fields,
            serde_json::json!({
                "APIToken": "[已隐藏]",
                "openAIKey": "[已隐藏]",
                "apiKeys": "[已隐藏]",
                "cookies": "[已隐藏]",
                "spindle": 1,
            })
        );

        // trace_api_call 与 trace_generation_failed 的 step 摘要也必须走脱敏
        let tmp = tempfile::tempdir().unwrap();
        laser_runtime::settings_store::update(
            &laser_runtime::settings_store::settings_path(tmp.path()),
            |map| {
                map.insert(
                    laser_runtime::process_trace::SETTING_KEY.into(),
                    serde_json::json!(true),
                );
                Ok(())
            },
        )
        .unwrap();
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_runtime_path(tmp.path().to_path_buf());
        let bad_reply = http_json(
            502,
            failure("upstream failed with Bearer top-secret-bearer-999", None),
        );
        let begun = service.trace_api_begin(
            "POST",
            "/api/jev/test",
            br#"{"lan_static_pin":654321}"#,
            &laser_contracts::CallerContext::Loopback,
            None,
        );
        service.trace_api_end(begun, "POST", "/api/jev/test", &bad_reply, 12);
        service.trace_generation_failed(
            "/api/text-task/generate",
            "desktop",
            &serde_json::Map::new(),
            "failed using sk-live-supersecret987654",
        );
        service.process_trace().unwrap().flush();

        let day = std::fs::read_dir(tmp.path().join(laser_runtime::process_trace::TRACE_DIR))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        for name in [
            laser_runtime::process_trace::TIMELINE_JSONL,
            laser_runtime::process_trace::TIMELINE_TXT,
        ] {
            let content = std::fs::read_to_string(day.join(name)).unwrap();
            assert!(
                !content.contains("top-secret-bearer-999")
                    && !content.contains("sk-live-supersecret987654")
                    && !content.contains("654321"),
                "{name} 不得泄露凭据：\n{content}"
            );
        }
    }

    #[test]
    fn api_call_is_recorded_at_begin_and_end_around_generation() {
        let tmp = tempfile::tempdir().unwrap();
        laser_runtime::settings_store::update(
            &laser_runtime::settings_store::settings_path(tmp.path()),
            |map| {
                map.insert(
                    laser_runtime::process_trace::SETTING_KEY.into(),
                    serde_json::json!(true),
                );
                Ok(())
            },
        )
        .unwrap();
        let gen_dir = tmp.path().join("gen");
        std::fs::create_dir_all(&gen_dir).unwrap();
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_runtime_path(tmp.path().to_path_buf())
        .with_generated_root(gen_dir.clone())
        .with_preview_roots(vec![gen_dir]);

        let (status, _) = service.dispatch_json(
            "POST",
            "/api/text-task/generate",
            &serde_json::json!({
                "text": "AB",
                "font_path": concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/fonts/geometry.ttf"),
                "material": "椴木",
                "thickness_mm": 3,
                "font_size_mm": 10,
                "text_fill": "fill",
            }),
            laser_contracts::CallerContext::Desktop,
        );
        assert_eq!(status, 200);
        service.process_trace().unwrap().flush();

        let day = std::fs::read_dir(tmp.path().join(laser_runtime::process_trace::TRACE_DIR))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let text = std::fs::read_to_string(day.join(laser_runtime::process_trace::TIMELINE_JSONL))
            .unwrap();
        let rows: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        // n110：开始处理先记一步，生成排在中间，处理完再记一步；文件里的先后就是真实先后
        let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["api_begin", "generate", "api_call"], "{rows:?}");
        assert_eq!(rows[0]["call"], rows[2]["call"], "两步用同一个调用编号");
        assert_eq!(rows[0]["request"]["text"], "AB");
        assert!(
            rows[0]["step"]
                .as_str()
                .unwrap()
                .starts_with("电脑工作台发起：请求生成文字刻图预览（开始处理")
        );
        assert!(rows[2]["step"].as_str().unwrap().contains("→ 成功"));
        let seqs: Vec<u64> = rows.iter().map(|r| r["seq"].as_u64().unwrap()).collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]));
        let ats: Vec<&str> = rows.iter().map(|r| r["at"].as_str().unwrap()).collect();
        assert!(ats.windows(2).all(|w| w[0] <= w[1]), "{ats:?}");
    }

    #[test]
    fn large_request_body_is_saved_as_attachment_not_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        laser_runtime::settings_store::update(
            &laser_runtime::settings_store::settings_path(tmp.path()),
            |map| {
                map.insert(
                    laser_runtime::process_trace::SETTING_KEY.into(),
                    serde_json::json!(true),
                );
                Ok(())
            },
        )
        .unwrap();
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_runtime_path(tmp.path().to_path_buf());
        let body = serde_json::to_vec(&serde_json::json!({
            "action": "save",
            "jev_api_key": "tvly-big-body-secret",
            "image_data_url": "data:image/png;base64,AAAA",
            "scene_json": "x".repeat(300 * 1024),
        }))
        .unwrap();
        let begun = service.trace_api_begin(
            "POST",
            "/api/draw/save",
            &body,
            &laser_contracts::CallerContext::Desktop,
            None,
        );
        assert!(begun.is_some());
        service.process_trace().unwrap().flush();
        let trace_dir = tmp.path().join(laser_runtime::process_trace::TRACE_DIR);
        let day = std::fs::read_dir(&trace_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let row: serde_json::Value = serde_json::from_str(
            std::fs::read_to_string(day.join(laser_runtime::process_trace::TIMELINE_JSONL))
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(row["kind"], "api_begin");
        assert_eq!(row["request"]["body_bytes"], body.len());
        let file = row["request"]["body_file"]
            .as_str()
            .expect("大请求体另存成附件");
        // 大请求体先按字段脱敏、大块字段另存，再存整份：密钥不落盘
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(trace_dir.join(file)).unwrap()).unwrap();
        assert_eq!(saved["jev_api_key"], "[已隐藏]");
        assert_eq!(saved["action"], "save");
        let shot = saved["image_data_url"]["file"].as_str().unwrap();
        assert_eq!(
            std::fs::read(trace_dir.join(shot)).unwrap(),
            [0u8, 0, 0],
            "截图解码成图片另存"
        );
        let scene = saved["scene_json"]["file"].as_str().unwrap();
        assert_eq!(
            std::fs::read(trace_dir.join(scene)).unwrap().len(),
            300 * 1024
        );
        let all = std::fs::read_dir(day.join("laser-files"))
            .unwrap()
            .map(|e| std::fs::read(e.unwrap().path()).unwrap())
            .collect::<Vec<_>>();
        assert!(
            all.iter()
                .all(|bytes| !String::from_utf8_lossy(bytes).contains("tvly-big-body-secret")),
            "任何附件里都没有明文密钥"
        );
    }

    fn enabled_service(tmp: &std::path::Path) -> ApiService {
        laser_runtime::settings_store::update(
            &laser_runtime::settings_store::settings_path(tmp),
            |map| {
                map.insert(
                    laser_runtime::process_trace::SETTING_KEY.into(),
                    serde_json::json!(true),
                );
                Ok(())
            },
        )
        .unwrap();
        ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_runtime_path(tmp.to_path_buf())
    }

    fn timeline_rows(tmp: &std::path::Path) -> Vec<serde_json::Value> {
        let day = std::fs::read_dir(tmp.join(laser_runtime::process_trace::TRACE_DIR))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::read_to_string(day.join(laser_runtime::process_trace::TIMELINE_JSONL))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn stop_actions_run_before_tracing_and_are_recorded_afterwards() {
        assert!(must_run_before_trace("/api/safe-action", b"{}"));
        assert!(must_run_before_trace(
            "/api/workflow",
            br#"{"action":"cancel","workflow_id":"wf-1"}"#
        ));
        assert!(!must_run_before_trace(
            "/api/workflow",
            br#"{"action":"confirm_send"}"#
        ));
        let tmp = tempfile::tempdir().unwrap();
        let service = enabled_service(tmp.path());
        let _ = service.dispatch_json(
            "POST",
            "/api/safe-action",
            &serde_json::json!({"action": "emergency_stop", "confirmed": true}),
            laser_contracts::CallerContext::Desktop,
        );
        service.process_trace().unwrap().flush();
        let rows = timeline_rows(tmp.path());
        let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["api_begin", "api_call"], "{rows:?}");
        assert!(
            rows[0]["received_at"].as_str().is_some(),
            "先执行后补记：写明收到时刻"
        );
        let step = rows[0]["step"].as_str().unwrap();
        assert!(
            step.contains("急停（实时 0x18）") && step.contains("先执行后补记"),
            "{step}"
        );
    }

    #[test]
    fn directory_change_keeps_api_call_and_attachments_together() {
        let tmp = tempfile::tempdir().unwrap();
        let selected = tempfile::tempdir().unwrap();
        let service = enabled_service(tmp.path());
        let trace = service.process_trace().unwrap();
        let old_dir = trace.dir();
        let begun = service.trace_api_begin(
            "POST",
            "/api/draw/preview",
            br#"{"scene_json":"original scene"}"#,
            &laser_contracts::CallerContext::Desktop,
            None,
        );
        assert!(begun.is_some());
        trace.set_directory(Some(selected.path())).unwrap();
        service.trace_api_end(
            begun,
            "POST",
            "/api/draw/preview",
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"success\":true,\"result\":{\"image_data_url\":\"data:image/png;base64,AAAA\"}}",
            1,
        );
        trace.record(
            "laser",
            "下一步",
            "new_step",
            "切换后的记录",
            serde_json::Value::Null,
        );
        trace.flush();
        let rows = timeline_rows(tmp.path());
        assert_eq!(rows.len(), 2, "同一次接口调用的开始和结束都留在原目录");
        assert_eq!(rows[0]["call"], rows[1]["call"]);
        let request_file = rows[0]["request"]["scene_json"]["file"].as_str().unwrap();
        let response_file = rows[1]["response"]["result"]["image_data_url"]["file"]
            .as_str()
            .unwrap();
        assert_eq!(
            std::fs::read(old_dir.join(request_file)).unwrap(),
            b"original scene"
        );
        assert_eq!(
            std::fs::read(old_dir.join(response_file)).unwrap(),
            [0, 0, 0]
        );
        let day = std::fs::read_dir(trace.dir())
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.file_type().unwrap().is_dir())
            .unwrap()
            .path();
        let new_timeline =
            std::fs::read_to_string(day.join(laser_runtime::process_trace::TIMELINE_JSONL))
                .unwrap();
        assert_eq!(new_timeline.lines().count(), 1);
        assert!(new_timeline.contains("new_step"));
    }

    #[test]
    fn closing_step_is_written_even_if_switch_is_turned_off_mid_call() {
        let tmp = tempfile::tempdir().unwrap();
        let service = enabled_service(tmp.path());
        let (status, _) = service.dispatch_json(
            "POST",
            "/api/settings",
            &serde_json::json!({"process_trace_enabled": false}),
            laser_contracts::CallerContext::Desktop,
        );
        assert_eq!(status, 200);
        service.process_trace().unwrap().flush();
        let rows = timeline_rows(tmp.path());
        let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            ["api_begin", "api_call"],
            "关掉记录的那次调用也成对：{rows:?}"
        );
        assert_eq!(rows[0]["call"], rows[1]["call"]);
        assert!(rows[1]["step"].as_str().unwrap().contains("→ 成功"));
    }
}
