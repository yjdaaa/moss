//! 激光侧完整过程记录（n105 / r55，默认关闭）：每次生成预览时把收到的参数（AI 填的工具参数也在这里）、
//! 发起方、生成结果与产物文件（G-code、预览图、画板场景）复制进 `logs/process-trace/`，
//! 生成失败也记一条。产物复制一份，是因为 `generated_assets/` 里没被记录引用的旧文件 7 天后会被清掉。

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
const DROPPED_INPUT_KEYS: &[&str] = &["image_data_url", "scene_json", "_artifacts"];

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

impl ApiService {
    fn trace_on(&self) -> Option<&laser_runtime::ProcessTrace> {
        self.process_trace().filter(|trace| trace.enabled())
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
        for key in ARTIFACT_FILE_KEYS {
            let Some(name) = artifacts.get(*key).and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(source) = self.find_artifact_file(name) else {
                files.push(serde_json::json!({"field": key, "name": name, "saved": null}));
                continue;
            };
            let stem = format!("{}-{key}", workflow_id.unwrap_or("preview"));
            files.push(serde_json::json!({
                "field": key,
                "name": name,
                "saved": trace.copy_file("laser", &stem, &source),
            }));
        }
        trace.record(
            "laser",
            "generate",
            serde_json::json!({
                "route": route,
                "origin": origin,
                "workflow_id": workflow_id,
                "source_type": object.get("source_type").cloned().unwrap_or(serde_json::Value::Null),
                "input": trimmed_input(object),
                "artifacts": artifacts,
                "files": files,
            }),
        );
    }

    /// 生成失败：记参数与错误原文。
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
        trace.record(
            "laser",
            "generate_failed",
            serde_json::json!({
                "route": route,
                "origin": origin,
                "input": trimmed_input(object),
                "error": error,
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

        let day = std::fs::read_dir(&trace_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let text = std::fs::read_to_string(day.join("laser.jsonl")).unwrap();
        let mut lines = text
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap());
        let generate = lines.next().unwrap();
        assert_eq!(generate["kind"], "generate");
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
}
