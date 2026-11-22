//! 激光侧完整过程记录（n105 / r55，默认关闭）：写进当天时间线的激光步骤。
//!
//! - `api_call`：每次改动类接口调用（连接检查、生成、确认发送、走边框、急停与其他安全动作、审批、材料与设置
//!   修改……）的发起方、参数、结果与耗时；只读轮询与凭据接口不记，密钥类字段一律隐藏；
//! - `generate` / `generate_failed`：生成预览的参数、产物与失败原因，G-code、预览图、画板场景复制一份
//!   （`generated_assets/` 里没被记录引用的旧文件 7 天后会被清掉）；
//! - `job_start` / `job_progress` / `job_end`：加工线程的开始、每 10% 进度与结束结果；
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

/// 请求里太大或已另存一份的字段（画板截图 data URL、画板场景——场景文件会复制进记录）不进记录，
/// 只留字节数。
const DROPPED_INPUT_KEYS: &[&str] = &["image_data_url", "scene_json", "scene", "_artifacts"];

/// 超大 HTTP 请求/响应体（如 20 MB 上传或 6 MB 画板保存）直接按字节数省略，避免同步 JSON 重解析卡住接口。
const MAX_TRACE_RAW_BODY_BYTES: usize = 256 * 1024;

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

fn trimmed_input(
    object: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut input = object.clone();
    for key in DROPPED_INPUT_KEYS {
        if let Some(removed) = input.remove(*key)
            && let Some(text) = removed.as_str()
        {
            input.insert(
                format!("{key}_omitted_bytes"),
                serde_json::json!(text.len()),
            );
        }
    }
    input
}

/// 字段名里带这些词的值一律不记（密钥、令牌、配对码、密码）。
const SECRET_KEY_HINTS: &[&str] = &["key", "token", "secret", "password", "pin", "authorization"];
/// 单个字符串超过这么长时截断（完整产物另有附件）。
const MAX_TRACE_STRING: usize = 4000;
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

fn contains_secret_pattern(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    for marker in [
        "token=",
        "api_key=",
        "apikey=",
        "access_token=",
        "secret=",
        "password=",
        "passwd=",
        "key=",
        "bearer ",
        "basic ",
    ] {
        if lower.contains(marker) {
            return true;
        }
    }
    // URL 中带 userinfo@（含多 URL 与单令牌 wss://secret@host 形式）
    for (scheme_end, _) in lower.match_indices("://") {
        let rest = &lower[scheme_end + 3..];
        let authority = rest
            .split(['/', '?', '#', ' ', '\t', '\r', '\n'])
            .next()
            .unwrap_or("");
        if authority.contains('@') {
            return true;
        }
    }
    // 任意位置出现的 sk-... API Key（如 OpenAI / OpenRouter sk-or-...）
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 10 <= bytes.len() {
        if (bytes[i] == b's' || bytes[i] == b'S')
            && (bytes[i + 1] == b'k' || bytes[i + 1] == b'K')
            && bytes[i + 2] == b'-'
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
        {
            let tail = &bytes[i + 3..];
            let token_len = tail
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric() || **b == b'-' || **b == b'_')
                .count();
            if token_len >= 8 {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// 脱敏并截断单条文本（含 `step` 与错误串），确保凭据绝不漏进 `timeline.jsonl` / `timeline.txt`。
pub(crate) fn scrub_trace_text(text: &str) -> String {
    if contains_secret_pattern(text) {
        return "[已隐藏]".to_owned();
    }
    let mut chars = text.chars();
    let cut: String = chars.by_ref().take(MAX_TRACE_STRING).collect();
    let remaining = chars.count();
    if remaining > 0 {
        let total = MAX_TRACE_STRING + remaining;
        format!("{cut}…[共 {total} 字，已截断]")
    } else {
        cut
    }
}

/// 去掉凭据、省略大块数据、截断超长字符串（递归）。
pub(crate) fn scrub_trace_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                let lower = key.to_ascii_lowercase();
                if DROPPED_INPUT_KEYS.contains(&key.as_str()) || lower == "content_base64" {
                    let bytes = match map.get(&key) {
                        Some(serde_json::Value::String(s)) => s.len(),
                        Some(other) => other.to_string().len(),
                        None => 0,
                    };
                    map.insert(key, serde_json::json!(format!("[已省略，{bytes} 字节]")));
                } else if SECRET_KEY_HINTS.iter().any(|hint| lower.contains(hint))
                    && map
                        .get(&key)
                        .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()) || v.is_number())
                {
                    map.insert(key, serde_json::json!("[已隐藏]"));
                } else if let Some(child) = map.get_mut(&key) {
                    scrub_trace_value(child);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(scrub_trace_value),
        serde_json::Value::String(text) => {
            let scrubbed = scrub_trace_text(text);
            if scrubbed != *text {
                *text = scrubbed;
            }
        }
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
            "feed_hold" | "pause" => "暂停（进给保持）".into(),
            "cycle_start" | "resume" => "继续加工".into(),
            "home" => "回原点".into(),
            "stop_framing" => "停止走边框".into(),
            "pause_framing" => "暂停走边框".into(),
            "resume_framing" => "继续走边框".into(),
            "" => "安全动作".into(),
            other => format!("安全动作：{other}"),
        },
        "/api/device/framing" => "走边框".into(),
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

/// 这次接口调用要不要进时间线：只记改动类请求，跳过凭据接口与只读轮询。
pub(crate) fn should_trace_api(method: &str, route: &str, body: &serde_json::Value) -> bool {
    if matches!(method, "GET" | "HEAD" | "OPTIONS") || UNTRACED_ROUTES.contains(&route) {
        return false;
    }
    if route == "/api/workflow" && str_field(body, "action").is_none() {
        return false;
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
pub(crate) fn trace_job_step(
    trace: Option<&laser_runtime::ProcessTrace>,
    workflow_id: &str,
    kind: &str,
    step: &str,
    mut fields: serde_json::Value,
) {
    let Some(trace) = trace.filter(|t| t.enabled()) else {
        return;
    };
    scrub_trace_value(&mut fields);
    let safe_step = scrub_trace_text(step);
    trace.record(
        "laser",
        &laser_flow(Some(workflow_id)),
        kind,
        &safe_step,
        fields,
    );
}

impl ApiService {
    fn trace_on(&self) -> Option<&laser_runtime::ProcessTrace> {
        self.process_trace().filter(|trace| trace.enabled())
    }

    /// 一次改动类接口调用：谁发起、做什么、参数、结果与耗时（时间线里的一步）。
    pub(crate) fn trace_api_call(
        &self,
        method: &str,
        route: &str,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
        response: &[u8],
        elapsed_ms: u64,
    ) {
        let Some(trace) = self.trace_on() else {
            return;
        };
        if matches!(method, "GET" | "HEAD" | "OPTIONS") || UNTRACED_ROUTES.contains(&route) {
            return;
        }
        let mut request: serde_json::Value = if body.len() > MAX_TRACE_RAW_BODY_BYTES {
            serde_json::json!({ "_omitted_large_body_bytes": body.len() })
        } else {
            serde_json::from_slice(body).unwrap_or(serde_json::Value::Null)
        };
        if !should_trace_api(method, route, &request) {
            return;
        }
        let status = response_status(response);
        let mut reply = if response.len() > MAX_TRACE_RAW_BODY_BYTES {
            serde_json::json!({ "_omitted_large_response_bytes": response.len() })
        } else {
            response_body(response).unwrap_or(serde_json::Value::Null)
        };
        // 先递归脱敏，再取 workflow_id 与拼可读 step（并复用已解析的 reply，避免对大响应重复解析）
        scrub_trace_value(&mut request);
        scrub_trace_value(&mut reply);
        let workflow_id = str_field(&request, "workflow_id")
            .map(str::to_owned)
            .or_else(|| {
                reply
                    .pointer("/result/workflow_id")
                    .or_else(|| reply.get("workflow_id"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            });
        let label = api_step_label(method, route, &request);
        let outcome = if (200..300).contains(&status) {
            match reply.get("result").and_then(|r| r.as_str()) {
                Some(text) if !text.is_empty() => format!("成功：{}", short(text, 80)),
                _ => "成功".to_owned(),
            }
        } else {
            let detail = reply
                .get("result")
                .and_then(|r| r.as_str())
                .map(|t| short(t, 120))
                .unwrap_or_default();
            format!("失败（HTTP {status}）：{detail}")
        };
        let step = format!(
            "{}发起：{label} → {outcome}（{elapsed_ms} ms）",
            origin_label(ctx.label())
        );
        trace.record(
            "laser",
            &laser_flow(workflow_id.as_deref()),
            "api_call",
            &step,
            serde_json::json!({
                "method": method,
                "route": route,
                "origin": ctx.label(),
                "request": request,
                "status": status,
                "response": reply,
                "elapsed_ms": elapsed_ms,
            }),
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
        let mut input = serde_json::Value::Object(trimmed_input(object));
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
        trace.record(
            "laser",
            &laser_flow(workflow_id),
            "generate",
            &step,
            serde_json::json!({
                "route": route,
                "origin": origin,
                "workflow_id": workflow_id,
                "source_type": object.get("source_type").cloned().unwrap_or(serde_json::Value::Null),
                "input": input,
                "artifacts": clean_artifacts,
                "files": files,
            }),
        );
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
        let mut input = serde_json::Value::Object(trimmed_input(object));
        scrub_trace_value(&mut input);
        let safe_error = scrub_trace_text(error);
        trace.record(
            "laser",
            &laser_flow(str_field(&input, "workflow_id")),
            "generate_failed",
            &format!(
                "生成预览失败（{}）：{}",
                origin_label(origin),
                short(&safe_error, 160)
            ),
            serde_json::json!({
                "route": route,
                "origin": origin,
                "input": input,
                "error": safe_error,
            }),
        );
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
        assert!(
            generate["input"].get("image_data_url").is_none(),
            "截图 data URL 不进记录"
        );
        assert_eq!(generate["input"]["image_data_url_omitted_bytes"], 26);
        assert_eq!(generate["artifacts"]["gcode_sha256"], "abc");
        let files = generate["files"].as_array().unwrap();
        let saved: Vec<_> = files.iter().filter_map(|f| f["saved"].as_str()).collect();
        assert_eq!(saved.len(), 2, "G-code 与预览图各复制一份：{files:?}");
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
        let mut payload = serde_json::json!({
            "lan_static_pin": 123456,
            "jev_delete_api_key": true,
            "mcp_endpoint": "wss://user:pass@mcp.example.com/ws",
            "scene": {"elements": [1, 2, 3]},
            "nested": [
                "上游返回 Bearer abcdef1234567890",
                "上游失败：invalid key sk-or-v1-secret1234567890",
                "failed calling https://api.example.com via http://user:pass@proxy.local:8080",
                "wss://single-secret-token@mcp.example.com/ws",
                "正常提示文字保留",
            ],
        });
        scrub_trace_value(&mut payload);
        assert_eq!(payload["lan_static_pin"], "[已隐藏]");
        assert_eq!(
            payload["jev_delete_api_key"], true,
            "布尔控制开关不被误掩码"
        );
        assert_eq!(payload["mcp_endpoint"], "[已隐藏]");
        assert!(
            payload["scene"].as_str().unwrap().starts_with("[已省略，"),
            "画板大对象 scene 只留字节数"
        );
        assert_eq!(payload["nested"][0], "[已隐藏]");
        assert_eq!(payload["nested"][1], "[已隐藏]");
        assert_eq!(payload["nested"][2], "[已隐藏]");
        assert_eq!(payload["nested"][3], "[已隐藏]");
        assert_eq!(payload["nested"][4], "正常提示文字保留");

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
        service.trace_api_call(
            "POST",
            "/api/jev/test",
            br#"{"lan_static_pin":654321}"#,
            &laser_contracts::CallerContext::Loopback,
            &bad_reply,
            12,
        );
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
}
