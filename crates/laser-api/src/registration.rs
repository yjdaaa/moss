//! 无摄像头两点对位（2026-11-16，对齐 LightBurn Print and Cut）。
//!
//! - `POST /api/registration/read-position`：只发实时状态查询 `?` 读激光头当前位置，不发任何运动或出光指令；
//! - `POST /api/registration/preview`：按两对参考点求旋转 + 平移（+ 可选等比缩放），把原任务的刀路整体变换后
//!   生成**新的** `preview_ready` workflow（新文件、新 SHA-256、照常需要确认发送与全部门禁）。
//!
//! 新任务仍以开头 `G92 X0 Y0` 把开始时激光头位置声明为原点，开始位置固定为第 2 个标记点（记录第 2 点时
//! 激光头就停在那里），因此不需要回到原来的起点，也不使用机床坐标指令。

use super::*;

/// 对位点数组固定两个 `{x, y}`。
fn parse_point_pair(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<[(f64, f64); 2], String> {
    let Some(items) = object.get(key).and_then(serde_json::Value::as_array) else {
        return Err(format!("{key} 必须是两个 {{x, y}} 点"));
    };
    if items.len() != 2 {
        return Err(format!("{key} 必须正好两个点"));
    }
    let mut out = [(0.0, 0.0); 2];
    for (index, item) in items.iter().enumerate() {
        let coord = |axis: &str| {
            item.get(axis)
                .and_then(serde_json::Value::as_f64)
                .filter(|v| v.is_finite() && v.abs() <= 100_000.0)
        };
        match (coord("x"), coord("y")) {
            (Some(x), Some(y)) => out[index] = (x, y),
            _ => return Err(format!("{key} 第 {} 个点的 x / y 必须是数字", index + 1)),
        }
    }
    Ok(out)
}

fn registration_error(status: u16, message: impl Into<String>, code: &str) -> Vec<u8> {
    http_json(
        status,
        failure(
            message.into(),
            Some(serde_json::json!({"error_code": code})),
        ),
    )
}

fn round3(value: f64) -> f64 {
    let rounded = (value * 1000.0).round() / 1000.0;
    if rounded == 0.0 { 0.0 } else { rounded }
}

/// 画板任务对位后的画板视图：沿用原任务的作业摘要（材料、功率、速度、遍数、能否发送），
/// 尺寸与工时换成变换后的刀路；内容指纹用新刀路的 SHA-256（与原画板图案不同，同一份对位结果
/// 再次发送时照常要求勾选“已移动材料或原点”）。
fn registered_draw_view(
    source: &serde_json::Value,
    source_id: &str,
    artifacts: &serde_json::Map<String, serde_json::Value>,
    frame: Option<(f64, f64, f64, f64)>,
    new_sha: &str,
) -> serde_json::Value {
    let mut summary = source
        .pointer("/draw_lab/job_summary")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some((min_x, max_x, min_y, max_y)) = frame {
        summary["actual_width_mm"] = serde_json::json!(round3(max_x - min_x));
        summary["actual_height_mm"] = serde_json::json!(round3(max_y - min_y));
    }
    if let Some(seconds) = artifacts
        .get("time_estimate")
        .and_then(|t| t.get("estimated_seconds"))
        .and_then(serde_json::Value::as_f64)
    {
        summary["estimated_display"] = serde_json::json!(format!("{seconds:.1} 秒"));
    }
    summary["registered"] = serde_json::json!(true);
    serde_json::json!({
        "job_summary": summary,
        "content_fingerprint": new_sha,
        "registered_from": source_id,
    })
}

impl ApiService {
    pub(crate) fn handle_registration_route(
        &self,
        route: &str,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
    ) -> Vec<u8> {
        let mut object = match parse_json_object(body, 64 * 1024) {
            Ok(value) => value,
            Err(error) => return http_json(400, failure(error, None)),
        };
        strip_identity_fields(&mut object);
        if route == "/api/registration/read-position" {
            self.registration_read_position(&object, ctx)
        } else {
            self.registration_preview(&object, ctx)
        }
    }

    /// 读当前位置：复用 `/api/safe-action` 的 `status`（只写实时字节 `?`），要求机器 Idle。
    fn registration_read_position(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
        ctx: &laser_contracts::CallerContext,
    ) -> Vec<u8> {
        let mut request = object.clone();
        for key in [
            "x",
            "y",
            "z",
            "feed_rate",
            "power",
            "confirmed",
            "dry_run",
            "job_id",
        ] {
            request.remove(key);
        }
        request.insert("action".into(), serde_json::json!("status"));
        // 同时接受 §1.5 的工作台连接字段（network_host / network_telnet_port / serial_port / baud）
        for (alias, key) in [
            ("network_host", "host"),
            ("network_telnet_port", "telnet_port"),
            ("serial_port", "port"),
            ("baud", "baudrate"),
            ("preferred_mode", "connection_mode"),
        ] {
            if let Some(value) = request.remove(alias)
                && !request.contains_key(key)
            {
                request.insert(key.into(), value);
            }
        }
        for key in ["telnet_port", "baudrate"] {
            if let Some(text) = request.get(key).and_then(|v| v.as_str()) {
                match text.trim().parse::<i32>() {
                    Ok(number) => {
                        request.insert(key.into(), serde_json::json!(number));
                    }
                    Err(_) => {
                        return http_json(400, failure(format!("{key} 必须是整数"), None));
                    }
                }
            }
        }
        let args: laser_contracts::LaserSafeActionArgs =
            match serde_json::from_value(serde_json::Value::Object(request)) {
                Ok(args) => args,
                Err(error) => {
                    return http_json(400, failure(format!("参数解析失败: {error}"), None));
                }
            };
        let res = self.handle_safe_action_for(&args, ctx, None);
        let detail = res.detail.clone().unwrap_or_default();
        if detail == "active_job_streaming" || detail == "job_result_unknown" {
            return registration_error(
                409,
                "设备正在加工或上一任务停机状态未确认，不能记录对位点",
                "registration_device_busy",
            );
        }
        if !res.success {
            return registration_error(
                400,
                format!("读取位置失败：{}", res.result),
                "registration_status_failed",
            );
        }
        let Some(report) = laser_device::parse_status_report(&detail) else {
            return registration_error(
                400,
                "设备没有返回状态报告，读不到位置",
                "registration_position_unavailable",
            );
        };
        if report.activity != laser_device::MachineActivity::Idle {
            return registration_error(
                409,
                format!(
                    "机器当前状态是 {}，等停稳（Idle）后再记录对位点",
                    report.state
                ),
                "registration_not_idle",
            );
        }
        let triple = |value: Option<(f64, f64, f64)>| {
            value.map(|(x, y, z)| serde_json::json!({"x": x, "y": y, "z": z}))
        };
        let (frame, position) = match (report.mpos, report.wpos) {
            (Some((x, y, _)), _) => ("mpos", (x, y)),
            (None, Some((x, y, _))) => ("wpos", (x, y)),
            (None, None) => {
                return registration_error(
                    400,
                    "状态报告里没有 MPos / WPos 坐标，读不到位置",
                    "registration_position_unavailable",
                );
            }
        };
        http_json(
            200,
            success(serde_json::json!({
                "state": report.state,
                "frame": frame,
                "position": {"x": position.0, "y": position.1},
                "mpos": triple(report.mpos),
                "wpos": triple(report.wpos),
                "raw": report.raw,
            })),
        )
    }

    fn registration_preview(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
        ctx: &laser_contracts::CallerContext,
    ) -> Vec<u8> {
        let Some(source_id) = object
            .get("workflow_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            return http_json(400, failure("workflow_id 不能为空", None));
        };
        let design = match parse_point_pair(object, "design_points") {
            Ok(points) => points,
            Err(error) => return registration_error(400, error, "registration_points_invalid"),
        };
        let measured = match parse_point_pair(object, "measured_points") {
            Ok(points) => points,
            Err(error) => return registration_error(400, error, "registration_points_invalid"),
        };
        let allow_scale = match object.get("allow_scale") {
            None | Some(serde_json::Value::Null) => false,
            Some(serde_json::Value::Bool(value)) => *value,
            Some(_) => {
                return registration_error(
                    400,
                    "allow_scale 必须是布尔值",
                    "registration_points_invalid",
                );
            }
        };
        let position_frame = match object.get("position_frame").and_then(|v| v.as_str()) {
            None => "mpos",
            Some("mpos") => "mpos",
            Some("wpos") => "wpos",
            Some(_) => {
                return registration_error(
                    400,
                    "position_frame 只能是 mpos 或 wpos",
                    "registration_points_invalid",
                );
            }
        };
        let Some(source) = self.load_record_payload(laser_runtime::RecordKind::Workflow, source_id)
        else {
            return http_json(404, failure("未找到 workflow", None));
        };
        let busy = matches!(
            source.get("status").and_then(|v| v.as_str()),
            Some("sending" | "running" | "preparing")
        ) || self
            .active_jobs
            .lock()
            .map(|jobs| jobs.contains_key(source_id))
            .unwrap_or(true);
        if busy {
            return registration_error(
                409,
                "原任务正在加工，不能生成对位预览",
                "registration_source_busy",
            );
        }
        if let Err(error) = validate_material_safety_in_payload(&source) {
            return registration_error(400, error, "material_safety_blocked");
        }
        // 新任务以第 2 个标记点为开始位置：实测点换算成相对第 2 点的坐标
        let start = measured[1];
        let relative = [
            (measured[0].0 - start.0, measured[0].1 - start.1),
            (0.0, 0.0),
        ];
        let fit = match laser_core::registration::fit_two_point(design, relative, allow_scale) {
            Ok(fit) => fit,
            Err(error) => return registration_error(400, error.to_string(), error.code()),
        };
        // 原刀路必须与预览时冻结的 SHA-256 一致，才拿来变换
        let (preflight, source_sha, source_file) =
            match build_send_preflight(&source, &self.send_gcode_roots()) {
                Ok(value) => value,
                Err(error) => {
                    return registration_error(
                        400,
                        format!("原任务刀路不可用：{error}"),
                        "send_gate_rejected",
                    );
                }
            };
        if let Err(error) = laser_device::authorize_send(&preflight) {
            return registration_error(
                400,
                format!("原任务刀路校验未通过：{error}"),
                "send_gate_rejected",
            );
        }
        let Some(source_path) = preflight.gcode_path.as_ref() else {
            return registration_error(400, "原任务没有刀路文件", "send_gate_rejected");
        };
        let source_text = match std::fs::read(source_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => {
                return registration_error(
                    400,
                    format!("读取原任务刀路失败：{error}"),
                    "send_gate_rejected",
                );
            }
        };
        let transformed =
            match laser_core::registration::transform_gcode(&source_text, &fit.transform) {
                Ok(value) => value,
                Err(error) => {
                    return registration_error(
                        400,
                        format!("这份刀路不能自动对位：{error}"),
                        "registration_gcode_unsupported",
                    );
                }
            };
        let Some(root) = self.generated_root.as_ref() else {
            return http_json(500, failure("生成目录未配置", None));
        };
        if let Err(error) = std::fs::create_dir_all(root) {
            return http_json(500, failure(format!("创建生成目录失败：{error}"), None));
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let gcode_name = format!(
            "reg-{}-{nanos}-{}.gcode",
            unix_seconds(),
            std::process::id()
        );
        if let Err(error) = std::fs::write(root.join(&gcode_name), transformed.gcode.as_bytes()) {
            return http_json(500, failure(format!("保存对位刀路失败：{error}"), None));
        }

        let source_artifacts = source.get("artifacts").cloned().unwrap_or_default();
        let material = material_from_payload(&source)
            .or_else(|| source_artifacts.get("material").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_owned();
        let thickness = source_artifacts
            .get("thickness_mm")
            .and_then(serde_json::Value::as_f64)
            .or_else(|| {
                source
                    .pointer("/input/thickness_mm")
                    .and_then(|v| v.as_f64())
            })
            .unwrap_or(3.0);
        let mut generate = serde_json::Map::new();
        generate.insert("source_type".into(), serde_json::json!("gcode"));
        generate.insert("gcode_file".into(), serde_json::json!(gcode_name));
        generate.insert("material".into(), serde_json::json!(material));
        generate.insert("thickness_mm".into(), serde_json::json!(thickness));
        if let Some(motion) = object.get("machine_motion") {
            generate.insert("machine_motion".into(), motion.clone());
        }
        let library = match self.effective_material_library() {
            Ok(library) => library,
            Err(error) => {
                return http_json(
                    500,
                    failure("读取材料库失败", Some(serde_json::json!({"error": error}))),
                );
            }
        };
        let mut artifacts = match generate_image_or_gcode_artifacts(
            &generate,
            self.upload_root.as_deref(),
            self.generated_root.as_deref(),
            &self.send_gcode_roots(),
            &library,
        ) {
            Ok(Some(artifacts)) => artifacts,
            Ok(None) => return http_json(500, failure("对位预览生成失败", None)),
            Err(error) => return http_json(400, failure(error, None)),
        };
        self.apply_time_calibration(artifacts.get_mut("time_estimate"), &generate);
        let new_sha = artifacts
            .get("gcode_sha256")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();

        // 走边框范围：变换后出光点的凸包与外接矩形（都以第 2 个标记点为原点）
        let hull = laser_core::gcode::burn_hull(&transformed.gcode);
        let frame = match &hull {
            Some(points) => Some(points.iter().fold(
                (
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                ),
                |acc, p| {
                    (
                        acc.0.min(p.0),
                        acc.1.max(p.0),
                        acc.2.min(p.1),
                        acc.3.max(p.1),
                    )
                },
            )),
            None => laser_core::gcode::motion_extents(&transformed.gcode)
                .ok()
                .flatten()
                .map(|e| (e.min_x, e.max_x, e.min_y, e.max_y)),
        };
        if let Some((min_x, max_x, min_y, max_y)) = frame {
            artifacts.insert(
                "frame_mm".into(),
                serde_json::json!({
                    "min_x": round3(min_x), "max_x": round3(max_x),
                    "min_y": round3(min_y), "max_y": round3(max_y),
                }),
            );
        }
        artifacts.insert("frame_hull_mm".into(), frame_hull_json(&transformed.gcode));
        if let Some(params) = source_artifacts.get("actual_params") {
            artifacts.insert("actual_params".into(), params.clone());
        }
        artifacts.insert("origin".into(), serde_json::json!("registration_mark_2"));
        artifacts.insert(
            "origin_note".into(),
            serde_json::json!("开刻前激光头必须停在第 2 个对位标记上（记录第 2 点时的位置）"),
        );
        let point_json = |points: &[(f64, f64); 2]| {
            serde_json::json!(
                points
                    .iter()
                    .map(|p| serde_json::json!({"x": p.0, "y": p.1}))
                    .collect::<Vec<_>>()
            )
        };
        let registration = serde_json::json!({
            "source_workflow_id": source_id,
            "source_gcode_file": source_file,
            "source_gcode_sha256": source_sha,
            "design_points": point_json(&design),
            "measured_points": point_json(&measured),
            "position_frame": position_frame,
            "start_point": "mark_2",
            "allow_scale": allow_scale,
            "rotation_deg": (fit.rotation_deg * 10_000.0).round() / 10_000.0,
            "scale": (fit.scale * 100_000.0).round() / 100_000.0,
            "scale_applied": fit.scale_applied,
            "design_distance_mm": round3(fit.design_distance_mm),
            "measured_distance_mm": round3(fit.measured_distance_mm),
            "distance_ratio": (fit.distance_ratio * 100_000.0).round() / 100_000.0,
            "residual_mm": round3(fit.residual_mm),
            // 原任务起点落在新起点（第 2 个标记点）的哪里
            "offset_mm": {"x": round3(fit.transform.tx), "y": round3(fit.transform.ty)},
            "moves": transformed.moves,
            "arcs": transformed.arcs,
        });
        artifacts.insert("registration".into(), registration.clone());

        let image_name = artifacts
            .get("image_file")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let workflow_id = next_workflow_id();
        let mut web = serde_json::json!({
            "gcode_preview_url": format!("/preview?path={image_name}"),
            "image_preview_url": format!("/preview?path={image_name}"),
            "gcode_download_url": format!("/preview?path={gcode_name}&download=1"),
        });
        // 画板任务对位（2026-11-16）：带上画板视图，画板页沿用自己的作业摘要、重复发送防呆与确认框
        let draw_lab =
            (source.get("source_type").and_then(|v| v.as_str()) == Some("draw")).then(|| {
                if let Some(url) = source.pointer("/web/scene_download_url") {
                    web["scene_download_url"] = url.clone();
                }
                registered_draw_view(&source, source_id, &artifacts, frame, &new_sha)
            });
        let mut record = serde_json::json!({
            "workflow_id": workflow_id,
            "status": "preview_ready",
            "source_type": "gcode",
            "input": {
                "source_type": "gcode",
                "gcode_file": gcode_name,
                "material": material,
                "thickness_mm": thickness,
                "registration": true,
            },
            "artifacts": artifacts,
            "confirmation_snapshot": {
                "gcode_file": gcode_name,
                "gcode_sha256": new_sha,
                "material": material,
                "thickness_mm": thickness,
            },
            "registered_from": source_id,
            "registration": registration,
            "origin": ctx.label(),
            "web": web,
            "next_actions": ["status", "cancel", "confirm_send"],
            "created_at": unix_seconds(),
            "updated_at": unix_seconds(),
        });
        if let Some(view) = draw_lab {
            record["draw_lab"] = view;
        }
        let stored = laser_runtime::StoredRecord {
            kind: laser_runtime::RecordKind::Workflow,
            id: workflow_id,
            status: Some("preview_ready".into()),
            payload: record.clone(),
        };
        if self
            .with_effective_store(|store| store.put(&stored))
            .is_err()
        {
            return http_json(500, failure("保存对位预览失败", None));
        }
        http_json(200, success(record))
    }
}
