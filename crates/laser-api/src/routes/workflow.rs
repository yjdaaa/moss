use super::super::*;

impl ApiService {
    pub(super) fn route_static_and_preview(
        &self,
        method: &str,
        path: &str,
        route: &str,
    ) -> Option<Vec<u8>> {
        if method != "GET" {
            return None;
        }
        let asset = if route == "/" || is_spa_route(route) {
            Some("index.html".to_owned())
        } else if let Some(path) = route.strip_prefix("/assets/") {
            safe_asset_path(path).map(|value| format!("assets/{value}"))
        } else if let Some(path) = route.strip_prefix("/vendor/") {
            safe_asset_path(path).map(|value| format!("vendor/{value}"))
        } else {
            None
        };
        if let Some(asset) = asset {
            return Some(match FrontendFixture::get(&asset) {
                Some(file) => http_bytes(200, embedded_content_type(&asset), file.data.to_vec()),
                None if is_spa_route(route) => http_bytes(
                    200,
                    "text/html; charset=utf-8",
                    FrontendFixture::get("index.html")
                        .map(|file| file.data.to_vec())
                        .unwrap_or_default(),
                ),
                None => http_json(404, failure("未找到路径", None)),
            });
        }
        if route == "/preview" {
            return Some(self.handle_preview_file(path));
        }
        None
    }

    fn handle_preview_file(&self, path: &str) -> Vec<u8> {
        let query = path.split_once('?').map(|(_, value)| value).unwrap_or("");
        let requested = query_param(query, "path").map(std::path::PathBuf::from);
        let Some(requested) = requested else {
            return http_json(404, failure("缺少 path", None));
        };
        let resolved = match resolve_preview_file(&requested, &self.preview_roots) {
            Ok(path) => path,
            Err(error) => return http_json(404, failure(error, None)),
        };
        let bytes = match std::fs::read(&resolved) {
            Ok(bytes) => bytes,
            Err(_) => return http_json(404, failure("文件不存在", None)),
        };
        let is_download = query_param(query, "download").is_some_and(|v| v == "1" || v == "true");
        let disposition = if is_download {
            let raw_name = requested
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("download");
            let safe_name: String = raw_name
                .chars()
                .filter(|c| !matches!(c, '"' | '\\' | '\r' | '\n') && !c.is_control())
                .collect();
            let fname = if safe_name.trim().is_empty() {
                "download"
            } else {
                safe_name.trim()
            };
            Some(format!("attachment; filename=\"{fname}\""))
        } else {
            None
        };
        http_bytes_with_disposition(200, content_type(&resolved), disposition.as_deref(), bytes)
    }

    pub(super) fn route_workflow_and_draw(
        &self,
        method: &str,
        route: &str,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
        short_job_watch: Option<&std::sync::Arc<ShortJobWatch>>,
    ) -> Option<Vec<u8>> {
        match (method, route) {
            ("POST", "/api/workflow" | "/api/text-task/generate" | "/api/text-task/send") => {
                Some(self.handle_workflow_or_text_task(route, body, ctx, short_job_watch))
            }
            ("POST", "/api/draw/preview") => Some(self.handle_draw_preview(body)),
            ("POST", "/api/upload") => Some(self.handle_upload(body)),
            ("GET" | "POST", "/api/draw/state") => Some(self.handle_draw_state(method, body, ctx)),
            _ => None,
        }
    }

    fn handle_workflow_or_text_task(
        &self,
        route: &str,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
        short_job_watch: Option<&std::sync::Arc<ShortJobWatch>>,
    ) -> Vec<u8> {
        let object = match parse_json_object(body, 64 * 1024) {
            Ok(value) => value,
            Err(error) => return http_json(400, failure(error, None)),
        };
        let mut object = object;
        strip_identity_fields(&mut object);
        // 新预览记下发起方（服务端按调用身份填写，客户端自带的值一律丢弃）；
        // 桌面端据此把 AI（语音 / 小智 / 网关）生成的任务弹出预览窗（2026-11-06）
        object.remove("_origin");
        object.remove("_origin_mpos");
        object.remove("_artifacts");
        let creates_preview = route == "/api/text-task/generate"
            || (route == "/api/workflow"
                && matches!(
                    object.get("action").and_then(|v| v.as_str()),
                    Some("preview" | "plan" | "create")
                ));
        if creates_preview {
            object.insert(
                "_origin".into(),
                serde_json::Value::String(ctx.label().to_owned()),
            );
        }
        if creates_preview
            && let Some(req_wf_id) = object
                .get("workflow_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        {
            let _transition = crate::workflow::record_transition_guard();
            if let Some((status, msg, code)) = self.with_effective_store(|store| {
                crate::workflow::preview_workflow_id_conflict(
                    store,
                    &self.active_jobs,
                    &self.launching_workflows,
                    &req_wf_id,
                )
            }) {
                return http_json(
                    status,
                    failure(msg, Some(serde_json::json!({"error_code": code}))),
                );
            }
        }
        if route == "/api/text-task/generate" {
            let effective_library = match self.effective_material_library() {
                Ok(lib) => lib,
                Err(error) => {
                    return http_json(
                        500,
                        failure(
                            "读取材料库失败",
                            Some(serde_json::json!({ "error": error })),
                        ),
                    );
                }
            };
            match generate_text_artifacts(
                &object,
                self.generated_root
                    .as_deref()
                    .or(self.upload_root.as_deref()),
                &effective_library,
                self.engrave_route(),
                self.text_origin(),
            ) {
                Ok(mut artifacts) => {
                    self.apply_time_calibration(artifacts.get_mut("time_estimate"), &object);
                    object.insert("_artifacts".into(), serde_json::Value::Object(artifacts));
                }
                Err(error) => {
                    self.trace_generation_failed(route, ctx.label(), &object, &error);
                    return http_json(400, failure(error, None));
                }
            }
        } else if route == "/api/workflow" {
            let action = object.get("action").and_then(|v| v.as_str()).unwrap_or("");
            // 按预览走边框（2026-11-18）：不进 workflow 状态机，走边框完不改记录状态
            if action == "frame" {
                return self.workflow_frame(&object, ctx, short_job_watch);
            }
            // 走边框途中 AI 调了 cancel（2026-11-18）：只有语音 / 小智 / 网关发来、唯一的活动作业是走边框、
            // 且这份预览确实在等待发送（不在启动或发送中）时，停的是走边框、预览保留——
            // 用户说“取消”是想让激光头别走了，不能把预览作废了而激光头还在走。其余一律照常取消。
            if action == "cancel"
                && matches!(
                    ctx,
                    laser_contracts::CallerContext::Voice { .. }
                        | laser_contracts::CallerContext::McpBridge
                        | laser_contracts::CallerContext::Gateway { .. }
                )
                && let Some(framing_id) = self.framing_to_stop_for_cancel(&object)
            {
                let stop_args = laser_contracts::LaserSafeActionArgs {
                    action: "stop_framing".into(),
                    ..Default::default()
                };
                let stopped = self.control_framing("stop_framing", &stop_args, Some(&framing_id));
                return http_json(
                    if stopped.success { 200 } else { 400 },
                    serde_json::json!({
                        "success": stopped.success,
                        "result": format!("{}（预览保留，没有作废）", stopped.result),
                        "detail": {"framing_stopped": framing_id},
                    }),
                );
            }
            if action == "preview" || action == "create" || action == "plan" {
                let source_type = object
                    .get("source_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if source_type == "image" || source_type == "gcode" {
                    let effective_library = match self.effective_material_library() {
                        Ok(lib) => lib,
                        Err(error) => {
                            return http_json(
                                500,
                                failure(
                                    "读取材料库失败",
                                    Some(serde_json::json!({ "error": error })),
                                ),
                            );
                        }
                    };
                    match generate_image_or_gcode_artifacts(
                        &object,
                        self.upload_root.as_deref(),
                        self.generated_root.as_deref(),
                        &self.preview_roots,
                        &effective_library,
                    ) {
                        Ok(Some(mut artifacts)) => {
                            self.apply_time_calibration(
                                artifacts.get_mut("time_estimate"),
                                &object,
                            );
                            object
                                .insert("_artifacts".into(), serde_json::Value::Object(artifacts));
                        }
                        Ok(None) => {}
                        Err(error) => {
                            self.trace_generation_failed(route, ctx.label(), &object, &error);
                            return http_json(400, failure(error, None));
                        }
                    }
                }
            }
        }
        let send_roots = self.send_gcode_roots();
        let requested_spec = self.connection_spec_from_request(&object);
        let launcher =
            |workflow_id: &str,
             gcode_path: std::path::PathBuf,
             expected_sha: &str,
             request: &serde_json::Map<String, serde_json::Value>| {
                self.launch_send_job(workflow_id, gcode_path, expected_sha, request, ctx)
            };
        let response = self.with_effective_store(|store| {
            handle_workflow_store_request(
                store,
                route,
                &object,
                &send_roots,
                &self.active_jobs,
                &self.launching_workflows,
                &requested_spec,
                &launcher,
            )
        });
        // 完整过程记录（默认关）：生成了预览产物的请求记下参数、发起方与产物
        if creates_preview && let Some(artifacts) = object.get("_artifacts") {
            if let Some(error) = crate::process_trace::response_failure(&response) {
                // 产物生成了但预览没保存（编号冲突等）：记失败，不冒充一次成功的生成
                self.trace_generation_failed(route, ctx.label(), &object, &error);
                return response;
            }
            let workflow_id = crate::process_trace::response_workflow_id(&response);
            self.trace_generation(
                route,
                ctx.label(),
                workflow_id.as_deref(),
                &object,
                artifacts,
            );
        }
        response
    }

    fn handle_draw_preview(&self, body: &[u8]) -> Vec<u8> {
        let object = match parse_json_object(body, 10 * 1024 * 1024) {
            Ok(value) => value,
            Err(error) => return http_json(400, failure(error, None)),
        };
        let effective_library = match self.effective_material_library() {
            Ok(lib) => lib,
            Err(error) => {
                return http_json(
                    500,
                    failure(
                        "读取材料库失败",
                        Some(serde_json::json!({ "error": error })),
                    ),
                );
            }
        };
        let mut artifacts = match generate_draw_artifacts(
            &object,
            self.generated_root
                .as_deref()
                .or(self.upload_root.as_deref()),
            &effective_library,
            self.engrave_route(),
        ) {
            Ok(a) => a,
            Err(error) => {
                self.trace_generation_failed("/api/draw/preview", "draw", &object, &error);
                return http_json(400, failure(error, None));
            }
        };
        self.apply_time_calibration(artifacts.get_mut("time_estimate"), &object);
        let workflow_id = format!("draw-{}", next_workflow_id());
        let material = artifacts
            .get("material")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let thickness_mm = artifacts
            .get("thickness_mm")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let gcode_name = artifacts
            .get("gcode_file")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let gcode_sha = artifacts
            .get("gcode_sha256")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let mut input = object.clone();
        input.remove("image_data_url");
        let mut record = serde_json::json!({
            "workflow_id": workflow_id,
            "status": "preview_ready",
            "source_type": "draw",
            "input": input,
            "material": material,
            "thickness_mm": thickness_mm,
            "artifacts": artifacts,
            "confirmation_snapshot": {
                "gcode_file": gcode_name,
                "gcode_sha256": gcode_sha,
                "material": material,
                "thickness_mm": thickness_mm,
            },
            "next_actions": ["status", "cancel", "confirm_send"],
            "created_at": unix_seconds(),
            "updated_at": unix_seconds()
        });
        feedback::add_draw_view(&mut record);
        let stored = laser_runtime::StoredRecord {
            kind: laser_runtime::RecordKind::Workflow,
            id: workflow_id.clone(),
            status: Some("preview_ready".into()),
            payload: record.clone(),
        };
        if self
            .with_effective_store(|store| store.put(&stored))
            .is_err()
        {
            return http_json(500, failure("画板预览保存失败", None));
        }
        self.trace_generation(
            "/api/draw/preview",
            "draw",
            Some(&workflow_id),
            &object,
            &record["artifacts"],
        );
        http_json(200, success(record))
    }

    fn handle_upload(&self, body: &[u8]) -> Vec<u8> {
        let object = match parse_json_object(body, 20 * 1024 * 1024) {
            Ok(value) => value,
            Err(error) => return http_json(400, failure(error, None)),
        };
        let Some(root) = self.upload_root.as_ref() else {
            return http_json(500, failure("上传目录未配置", None));
        };
        let filename = object
            .get("filename")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let encoded = object
            .get("content_base64")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        match save_upload(filename, encoded, root, unix_seconds()) {
            Ok(upload) => http_json(
                200,
                success(serde_json::json!({
                    "file_path": upload.filename.clone(),
                    "preview_url": format!("/preview?path={}", upload.filename),
                    "filename": upload.filename,
                    "size": upload.size,
                    "suffix": upload.suffix,
                })),
            ),
            Err(error) => {
                let detail = if error == "不支持上传该文件类型" {
                    let basename = filename.rsplit(['\\', '/']).next().unwrap_or(filename);
                    let cleaned = basename
                        .replace(['<', '>', ':', '"', '|', '?', '*'], "_")
                        .chars()
                        .filter(|c| !c.is_control())
                        .collect::<String>();
                    Some(serde_json::json!({
                        "filename": if cleaned.is_empty() { "upload" } else { &cleaned },
                        "allowed": [
                            ".bmp", ".gcode", ".gif", ".jpeg", ".jpg", ".nc", ".png", ".webp"
                        ]
                    }))
                } else {
                    None
                };
                http_json(400, failure(error, detail))
            }
        }
    }

    fn handle_draw_state(
        &self,
        method: &str,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
    ) -> Vec<u8> {
        if !matches!(
            ctx,
            laser_contracts::CallerContext::Desktop | laser_contracts::CallerContext::Loopback
        ) {
            return http_json(403, failure("画板状态只能在本机界面访问", None));
        }
        if method == "GET" {
            return match self.read_draw_state() {
                Ok((record, exists)) => http_json(200, success(record.to_response_json(exists))),
                Err(error) => http_json(500, failure(error, None)),
            };
        }
        if method == "POST" {
            let object = match parse_json_object(body, 8 * 1024 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            return match self.update_draw_state(&object) {
                Ok((record, exists)) => http_json(200, success(record.to_response_json(exists))),
                Err((status, error)) => http_json(status, failure(error, None)),
            };
        }
        http_json(405, failure("方法不支持", None))
    }
}
