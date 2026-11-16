use super::*;

impl ApiService {
    pub(crate) fn load_calibration(&self, calibration_id: &str) -> Option<serde_json::Value> {
        self.load_record_payload(laser_runtime::RecordKind::Calibration, calibration_id)
    }

    /// 写入标定矩阵记录（SQLite 优先，否则内存兼容表）。
    fn save_calibration(&self, record: &serde_json::Value) -> Result<(), String> {
        let calibration_id = record
            .get("calibration_id")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .ok_or("calibration_id 缺失")?;
        let status = record
            .get("status")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let stored = laser_runtime::StoredRecord {
            kind: laser_runtime::RecordKind::Calibration,
            id: calibration_id.to_owned(),
            status,
            payload: record.clone(),
        };
        self.with_effective_store(|store| store.put(&stored))
            .map_err(|e| format!("{e:?}"))
    }

    /// 标定矩阵预览：按行列/功率/速度区间真实生成单元格与 G-code，落盘、冻结 SHA 并入库。
    /// 只生成不出光；返回 `calibration_id`、cells 与 `gcode_file` 供发送与选格复用。
    pub(crate) fn material_lab_preview(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)> {
        let material = object
            .get("material")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();
        if material.is_empty() {
            return Err((400, failure("material 不能为空", None)));
        }
        if let Err(error) = validate_material_safety(material) {
            return Err((
                400,
                failure(
                    error,
                    Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                ),
            ));
        }
        let thickness_mm =
            parse_float(object.get("thickness_mm"), "thickness_mm", 0.0).unwrap_or(0.0);
        if !thickness_mm.is_finite() || thickness_mm <= 0.0 {
            return Err((400, failure("thickness_mm 必须大于 0", None)));
        }
        let laser_mode = object
            .get("laser_mode")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("engrave")
            .to_ascii_lowercase();
        if !matches!(laser_mode.as_str(), "engrave" | "cut") {
            return Err((400, failure("laser_mode 必须是 engrave 或 cut", None)));
        }
        let matrix_kind = object
            .get("matrix_kind")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_ascii_lowercase())
            .unwrap_or_else(|| laser_mode.clone());
        let engraving_mode = if laser_mode == "cut" {
            String::new()
        } else {
            object
                .get("engraving_mode")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .unwrap_or("raster")
                .to_ascii_lowercase()
        };
        let rows = parse_int(object.get("rows"), "rows", 5)
            .map_err(|e| (400, failure(e.to_string(), None)))?;
        let columns = parse_int(object.get("columns"), "columns", 5)
            .map_err(|e| (400, failure(e.to_string(), None)))?;
        if !(1..=25).contains(&rows) || !(1..=25).contains(&columns) {
            return Err((400, failure("rows/columns 必须在 1..25 之间", None)));
        }
        // 区间缺省（-1 或缺失）时以材料推荐参数为中心取 ±40% 区间
        let effective_library = self.effective_material_library().map_err(|error| {
            (
                500,
                failure(
                    "读取材料库失败",
                    Some(serde_json::json!({ "error": error })),
                ),
            )
        })?;
        let recommendation = laser_core::material::recommend_in_library(
            &effective_library,
            material,
            thickness_mm,
            &laser_mode,
            if laser_mode == "cut" {
                None
            } else {
                Some(engraving_mode.as_str())
            },
            1000.0,
        );
        let (base_power, base_speed, base_passes) = recommendation
            .as_ref()
            .map(|r| {
                (
                    r.params.laser_max_power,
                    r.params.feed_rate,
                    r.params.passes.max(1),
                )
            })
            .unwrap_or((400, 1500, 1));
        let bound = |key: &str, fallback: i32| -> Result<i32, (u16, serde_json::Value)> {
            let value = parse_int(object.get(key), key, -1)
                .map_err(|e| (400, failure(e.to_string(), None)))?;
            Ok(if value < 0 { fallback } else { value as i32 })
        };
        let power_min = bound("power_min", (base_power as f64 * 0.6).round() as i32)?;
        let power_max = bound(
            "power_max",
            (base_power as f64 * 1.4).round().min(1000.0) as i32,
        )?;
        let speed_min = bound(
            "speed_min",
            (base_speed as f64 * 0.6).round().max(100.0) as i32,
        )?;
        let speed_max = bound("speed_max", (base_speed as f64 * 1.4).round() as i32)?;
        let passes = {
            let value = parse_int(object.get("passes"), "passes", 0)
                .map_err(|e| (400, failure(e.to_string(), None)))?;
            if value <= 0 {
                base_passes
            } else {
                value as i32
            }
        };
        if power_min > power_max || speed_min > speed_max || power_min < 0 || speed_min <= 0 {
            return Err((
                400,
                failure("功率/速度区间无效：min 必须 ≤ max 且为正数", None),
            ));
        }
        let cells = laser_core::material::calibration_matrix(
            rows as usize,
            columns as usize,
            power_min,
            power_max,
            speed_min,
            speed_max,
            passes,
        )
        .map_err(|e| (400, failure(format!("标定矩阵参数无效: {e}"), None)))?;
        let task_type = if laser_mode == "cut" {
            "cut_contour"
        } else if engraving_mode == "outline" {
            "engrave_outline"
        } else {
            "engrave_logo"
        };
        let gcode = laser_core::material::build_calibration_gcode(material, task_type, &cells)
            .map_err(|e| (500, failure(format!("标定 G-code 生成失败: {e}"), None)))?;

        let root = self
            .generated_root
            .as_deref()
            .or(self.upload_root.as_deref())
            .ok_or_else(|| (500, failure("生成目录未配置", None)))?;
        std::fs::create_dir_all(root)
            .map_err(|e| (500, failure(format!("创建生成目录失败: {e}"), None)))?;
        let calibration_id = format!("cal-{}", next_workflow_id().trim_start_matches("wf-"));
        let gcode_name = format!("{calibration_id}.gcode");
        let gcode_path = root.join(&gcode_name);
        std::fs::write(&gcode_path, gcode.as_bytes())
            .map_err(|e| (500, failure(format!("写入标定 G-code 失败: {e}"), None)))?;
        let gcode_sha = hex::encode(Sha256::digest(gcode.as_bytes()));
        let motion =
            crate::generation::parse_machine_motion(object).map_err(|e| (400, failure(e, None)))?;
        let estimate = laser_core::gcode::estimate_time_with(&gcode, &motion.profile);
        let cells_json = cells
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                serde_json::json!({
                    "cell_number": index + 1,
                    "id": cell.id,
                    "row": cell.row,
                    "column": cell.column,
                    "x_mm": cell.x_mm,
                    "y_mm": cell.y_mm,
                    "size_mm": cell.size_mm,
                    "power": cell.power,
                    "speed": cell.speed,
                    "passes": cell.passes,
                    "pixel_size_mm": cell.pixel_size_mm,
                    "line_interval_mm": cell.line_interval_mm,
                })
            })
            .collect::<Vec<_>>();
        let width_mm = (columns as f64 - 1.0) * 16.0 + 12.0;
        let height_mm = (rows as f64 - 1.0) * 16.0 + 12.0;
        let now = unix_seconds();
        let record = serde_json::json!({
            "calibration_id": calibration_id,
            "status": "prepared",
            "material": material,
            "thickness_mm": thickness_mm,
            "laser_mode": laser_mode,
            "engraving_mode": engraving_mode,
            "matrix_kind": matrix_kind,
            "task_type": task_type,
            "rows": rows,
            "columns": columns,
            "power_min": power_min,
            "power_max": power_max,
            "speed_min": speed_min,
            "speed_max": speed_max,
            "passes": passes,
            "cells": cells_json,
            "cell_count": cells.len(),
            "width_mm": width_mm,
            "height_mm": height_mm,
            "gcode_file": gcode_name,
            "gcode_sha256": gcode_sha,
            "artifacts": {"gcode_file": gcode_name, "gcode_sha256": gcode_sha},
            "confirmation_snapshot": {"gcode_file": gcode_name, "gcode_sha256": gcode_sha, "material": material, "thickness_mm": thickness_mm},
            "time_estimate": crate::generation::time_estimate_json(&estimate, &motion),
            "web": {"gcode_download_url": format!("/preview?path={gcode_name}&download=1")},
            "next_actions": ["send", "select_cell"],
            "created_at": now,
            "updated_at": now,
        });
        self.save_calibration(&record)
            .map_err(|e| (500, failure(format!("保存标定矩阵失败: {e}"), None)))?;
        Ok(record)
    }

    /// 标定矩阵发送：与 workflow confirm_send 同一套门禁（confirmed、终态拦截、物理 SHA 重算）
    /// 后，复用 JobWorker 真实逐行下发；终态由后台线程写回 calibration 记录。
    pub(crate) fn material_lab_send(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
        ctx: &laser_contracts::CallerContext,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)> {
        let calibration_id = object
            .get("calibration_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned();
        if calibration_id.is_empty() {
            return Err((400, failure("calibration_id 不能为空", None)));
        }
        if object.contains_key("start_from_line") {
            return Err((
                400,
                failure(
                    "标定矩阵发送不接受 start_from_line",
                    Some(serde_json::json!({"error_code": "resume_line_not_allowed"})),
                ),
            ));
        }
        if !parse_bool(object.get("confirmed"), false) {
            return Err((400, failure("发送测试矩阵需要 confirmed=true", None)));
        }
        let mut request = object.clone();
        request
            .entry("connection_mode")
            .or_insert(serde_json::Value::String("network".into()));
        let mode = request
            .get("connection_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("network")
            .trim()
            .to_ascii_lowercase();
        if mode == "network" {
            let host = request
                .get("network_host")
                .or_else(|| request.get("host"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if host.trim().is_empty() {
                return Err((400, failure("请填写设备 host/IP", None)));
            }
        } else if mode == "serial" {
            let port = request
                .get("port")
                .or_else(|| request.get("serial_port"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if port.trim().is_empty() {
                return Err((400, failure("请选择或填写串口号", None)));
            }
        }
        let cancel_epoch_before = current_cancel_epoch(&self.launching_workflows, &calibration_id);
        let record = self.load_calibration(&calibration_id).ok_or_else(|| {
            (
                404,
                failure(
                    "未找到标定矩阵，请先生成",
                    Some(serde_json::json!({"error_code": "calibration_missing"})),
                ),
            )
        })?;
        if let Err(error) = validate_material_safety_in_payload(&record) {
            return Err((
                400,
                failure(
                    error,
                    Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                ),
            ));
        }
        let current_status = record.get("status").and_then(|v| v.as_str());
        if let Some(reason) = workflow_send_blocked_reason(current_status) {
            return Err((
                400,
                failure(
                    reason,
                    Some(serde_json::json!({"error_code": "calibration_not_sendable"})),
                ),
            ));
        }
        let _launch_guard = match reserve_workflow_launch(
            &self.active_jobs,
            &self.launching_workflows,
            &calibration_id,
            Some(cancel_epoch_before),
        ) {
            Ok(guard) => guard,
            Err(ReserveLaunchError::Busy) => {
                return Err((
                    409,
                    failure(
                        "该标定矩阵正在启动或发送中，禁止重复操作",
                        Some(serde_json::json!({"error_code": "device_busy"})),
                    ),
                ));
            }
            Err(ReserveLaunchError::Cancelled) => {
                return Err((
                    409,
                    failure(
                        "标定任务已被取消，未开始发送",
                        Some(serde_json::json!({"error_code": "calibration_cancelled"})),
                    ),
                ));
            }
        };
        let mut record = self.load_calibration(&calibration_id).ok_or_else(|| {
            (
                404,
                failure(
                    "未找到标定矩阵，请先生成",
                    Some(serde_json::json!({"error_code": "calibration_missing"})),
                ),
            )
        })?;
        let fresh_status = record.get("status").and_then(|v| v.as_str());
        if let Some(reason) = workflow_send_blocked_reason(fresh_status) {
            return Err((
                400,
                failure(
                    reason,
                    Some(serde_json::json!({"error_code": "calibration_not_sendable"})),
                ),
            ));
        }
        let send_roots = self.send_gcode_roots();
        let (preflight, gcode_sha, gcode_file) = build_send_preflight(&record, &send_roots)
            .map_err(|e| {
                (
                    400,
                    failure(
                        format!("发送安全门禁未通过: {e}"),
                        Some(serde_json::json!({"error_code": "send_gate_rejected"})),
                    ),
                )
            })?;
        laser_device::authorize_send(&preflight).map_err(|e| {
            (
                400,
                failure(
                    format!("发送安全门禁未通过: {e}"),
                    Some(serde_json::json!({"error_code": "send_gate_rejected"})),
                ),
            )
        })?;
        let gcode_path = preflight.gcode_path.clone().unwrap_or_default();
        let snapshot_before_launch = record.clone();
        let send_result = self.launch_send_job_for(
            laser_runtime::RecordKind::Calibration,
            &calibration_id,
            gcode_path,
            &gcode_sha,
            &request,
            ctx,
        )?;
        if let Some(map) = record.as_object_mut() {
            map.insert("status".into(), serde_json::Value::String("sending".into()));
            map.insert("updated_at".into(), serde_json::json!(unix_seconds()));
            map.insert("send_result".into(), send_result);
            map.insert(
                "job_id".into(),
                serde_json::Value::String(calibration_id.clone()),
            );
            map.insert(
                "probe".into(),
                serde_json::json!({"connected": true, "mode": mode, "summary": "握手通过（$32=1），已开始逐行下发"}),
            );
            freeze_confirmation_snapshot(map, &gcode_file, &gcode_sha);
        }
        // 与工作流一致：发送线程可能已写入终态或并发收到取消；若状态读写失败则立即请求停机。
        self.with_effective_store(|store| {
            persist_launched_sending_record(
                store,
                laser_runtime::RecordKind::Calibration,
                &calibration_id,
                &snapshot_before_launch,
                &record,
                &gcode_file,
                &gcode_sha,
                cancel_epoch_before,
                &self.active_jobs,
                &self.launching_workflows,
            )
        })?;
        Ok(record)
    }

    /// 选格回填：把标定矩阵中指定单元格的功率/速度/次数写入材料库（文件 + SQLite），
    /// 并在 calibration 记录上标注 selected_cell。
    pub(crate) fn material_lab_select_cell(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)> {
        let calibration_id = object
            .get("calibration_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned();
        if calibration_id.is_empty() {
            return Err((400, failure("calibration_id 不能为空", None)));
        }
        let cell_number = parse_int(object.get("cell_number"), "cell_number", 0)
            .map_err(|e| (400, failure(e.to_string(), None)))?;
        if cell_number < 1 {
            return Err((400, failure("cell_number 必须大于等于 1", None)));
        }
        let mut record = self.load_calibration(&calibration_id).ok_or_else(|| {
            (
                404,
                failure(
                    "未找到标定矩阵，请先生成",
                    Some(serde_json::json!({"error_code": "calibration_missing"})),
                ),
            )
        })?;
        let cell = record
            .get("cells")
            .and_then(|v| v.as_array())
            .and_then(|cells| cells.get((cell_number - 1) as usize))
            .cloned()
            .ok_or_else(|| {
                (
                    400,
                    failure(format!("cell_number {cell_number} 超出矩阵范围"), None),
                )
            })?;
        let material = object
            .get("material")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .or_else(|| record.get("material").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_owned();
        let thickness_mm = parse_float(object.get("thickness_mm"), "thickness_mm", 0.0)
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0)
            .or_else(|| record.get("thickness_mm").and_then(|v| v.as_f64()))
            .unwrap_or(0.0);
        let laser_mode = object
            .get("laser_mode")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .or_else(|| record.get("laser_mode").and_then(|v| v.as_str()))
            .unwrap_or("engrave")
            .to_ascii_lowercase();
        let engraving_mode = object
            .get("engraving_mode")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .or_else(|| {
                record
                    .get("engraving_mode")
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.is_empty())
            })
            .unwrap_or("raster")
            .to_ascii_lowercase();
        if material.is_empty() || thickness_mm <= 0.0 {
            return Err((400, failure("material 与 thickness_mm 必须有效", None)));
        }
        let power = cell.get("power").and_then(|v| v.as_i64()).unwrap_or(0);
        let speed = cell.get("speed").and_then(|v| v.as_i64()).unwrap_or(0);
        let passes = cell.get("passes").and_then(|v| v.as_i64()).unwrap_or(1);
        let notes = object
            .get("notes")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("标定矩阵 {calibration_id} 单元格 #{cell_number}"));
        let mut save = serde_json::Map::new();
        save.insert("action".into(), serde_json::Value::String("save".into()));
        save.insert(
            "material".into(),
            serde_json::Value::String(material.clone()),
        );
        save.insert("thickness_mm".into(), serde_json::json!(thickness_mm));
        save.insert(
            "laser_mode".into(),
            serde_json::Value::String(laser_mode.clone()),
        );
        save.insert(
            "engraving_mode".into(),
            serde_json::Value::String(engraving_mode.clone()),
        );
        save.insert("laser_max_power".into(), serde_json::json!(power));
        save.insert("feed_rate".into(), serde_json::json!(speed));
        save.insert("passes".into(), serde_json::json!(passes.max(1)));
        if let Some(pixel) = cell.get("pixel_size_mm").and_then(|v| v.as_f64()) {
            save.insert("pixel_size_mm".into(), serde_json::json!(pixel));
        }
        save.insert(
            "source".into(),
            serde_json::Value::String("calibration".into()),
        );
        save.insert("notes".into(), serde_json::Value::String(notes));
        let response = handle_material_manage(
            &self.settings,
            self.runtime_db.as_ref(),
            &self.materials,
            &self.material_write_lock,
            &save,
        );
        let separator = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| i + 4)
            .unwrap_or(0);
        let saved: serde_json::Value =
            serde_json::from_slice(&response[separator..]).unwrap_or_default();
        if saved.get("success") != Some(&serde_json::Value::Bool(true)) {
            let message = saved
                .get("result")
                .and_then(|v| v.as_str())
                .unwrap_or("材料参数保存失败")
                .to_owned();
            return Err((
                400,
                failure(format!("单元格参数写回材料库失败: {message}"), None),
            ));
        }
        let now = unix_seconds();
        if let Some(map) = record.as_object_mut() {
            map.insert(
                "selected_cell".into(),
                serde_json::json!({
                    "cell_number": cell_number,
                    "id": cell.get("id").cloned().unwrap_or(serde_json::Value::Null),
                    "power": power,
                    "speed": speed,
                    "passes": passes,
                    "material": material,
                    "thickness_mm": thickness_mm,
                    "laser_mode": laser_mode,
                    "engraving_mode": engraving_mode,
                    "selected_at": now,
                }),
            );
            map.insert(
                "selected_cell_number".into(),
                serde_json::json!(cell_number),
            );
            map.insert("updated_at".into(), serde_json::json!(now));
        }
        self.save_calibration(&record)
            .map_err(|e| (500, failure(format!("保存标定矩阵失败: {e}"), None)))?;
        Ok(serde_json::json!({
            "calibration_id": calibration_id,
            "selected_cell": record.get("selected_cell").cloned().unwrap_or(serde_json::Value::Null),
            "cell_number": cell_number,
            "materials": saved.get("result").and_then(|r| r.get("materials")).cloned().unwrap_or(serde_json::Value::Null),
            "speech": format!("已将单元格 #{cell_number} 的功率 {power} / 速度 {speed} 保存为推荐参数"),
        }))
    }

    pub fn find_recent_material_workflow(&self) -> Option<serde_json::Value> {
        let mut workflows = if let Some(store) = self.store.as_ref() {
            store
                .list_recent(laser_runtime::RecordKind::Workflow, 50)
                .ok()
                .map(|records| records.into_iter().map(|r| r.payload).collect::<Vec<_>>())
                .unwrap_or_default()
        } else {
            let mut list: Vec<serde_json::Value> = self
                .workflows
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .values()
                .cloned()
                .collect();
            list.sort_by(|a, b| {
                let ts = |v: &serde_json::Value| {
                    v.get("updated_at")
                        .and_then(serde_json::Value::as_u64)
                        .or_else(|| v.get("created_at").and_then(serde_json::Value::as_u64))
                        .unwrap_or(0)
                };
                ts(b).cmp(&ts(a)).then_with(|| {
                    let id_a = a.get("workflow_id").and_then(|s| s.as_str()).unwrap_or("");
                    let id_b = b.get("workflow_id").and_then(|s| s.as_str()).unwrap_or("");
                    id_b.cmp(id_a)
                })
            });
            list
        };
        let effective_library = self.effective_material_library().ok()?;
        for wf in workflows.drain(..) {
            let input = wf.get("input").and_then(serde_json::Value::as_object);
            let artifacts = wf.get("artifacts").and_then(serde_json::Value::as_object);
            let material = input
                .and_then(|i| i.get("material"))
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    artifacts
                        .and_then(|a| a.get("material"))
                        .and_then(serde_json::Value::as_str)
                })
                .map(str::trim);
            let thickness = input
                .and_then(|i| i.get("thickness_mm"))
                .and_then(serde_json::Value::as_f64)
                .or_else(|| {
                    artifacts
                        .and_then(|a| a.get("thickness_mm"))
                        .and_then(serde_json::Value::as_f64)
                });
            let (Some(mat), Some(thick)) = (material, thickness) else {
                continue;
            };
            if mat.is_empty() || !thick.is_finite() || thick <= 0.0 {
                continue;
            }

            let raw_laser_mode = input
                .and_then(|i| i.get("laser_mode"))
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    artifacts
                        .and_then(|a| a.get("laser_mode"))
                        .and_then(serde_json::Value::as_str)
                })
                .unwrap_or("");
            let raw_engraving_mode = input
                .and_then(|i| i.get("engraving_mode"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let laser_mode = if raw_laser_mode.eq_ignore_ascii_case("cut")
                || raw_engraving_mode.eq_ignore_ascii_case("cut")
            {
                "cut"
            } else {
                "engrave"
            };
            let text_fill = artifacts
                .and_then(|a| a.get("text_fill"))
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    input
                        .and_then(|i| i.get("text_fill"))
                        .and_then(serde_json::Value::as_str)
                });
            let draw_mode = artifacts
                .and_then(|a| a.get("mode"))
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    input
                        .and_then(|i| i.get("mode"))
                        .and_then(serde_json::Value::as_str)
                });
            let engraving_mode = if laser_mode == "cut" {
                ""
            } else if matches!(text_fill, Some("outline" | "hatch"))
                || draw_mode == Some("outline")
                || raw_engraving_mode.eq_ignore_ascii_case("outline")
            {
                "outline"
            } else {
                "raster"
            };

            let actual = artifacts
                .and_then(|a| a.get("actual_params"))
                .and_then(serde_json::Value::as_object);
            let params = if let Some(actual) = actual
                && let Some(raw_max) = actual.get("laser_max_power").and_then(|v| v.as_f64())
                && let Some(raw_feed) = actual.get("feed_rate").and_then(|v| v.as_f64())
                && raw_max.is_finite()
                && raw_feed.is_finite()
                && raw_feed > 0.0
            {
                let scale = actual
                    .get("power_scale")
                    .and_then(|v| v.as_f64())
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .unwrap_or(1000.0);
                let ratio = if (scale - 1000.0).abs() > 1e-6 {
                    1000.0 / scale
                } else {
                    1.0
                };
                let raw_min = actual
                    .get("laser_min_power")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let min_power = (raw_min * ratio).round().clamp(0.0, 1000.0) as i32;
                let max_power = (raw_max * ratio).round().clamp(0.0, 1000.0) as i32;
                let feed_rate = raw_feed.round().max(1.0) as i32;
                let travel_rate = actual
                    .get("travel_rate")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(3000)
                    .max(1) as i32;
                let threshold = actual
                    .get("threshold")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(-1)
                    .clamp(-1, 255) as i32;
                let passes = actual
                    .get("passes")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(1)
                    .max(1) as i32;
                let pixel_size_mm = actual
                    .get("pixel_size_mm")
                    .and_then(|v| v.as_f64())
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .unwrap_or(0.1);
                serde_json::json!({
                    "laser_min_power": min_power,
                    "laser_max_power": max_power,
                    "feed_rate": feed_rate,
                    "travel_rate": travel_rate,
                    "threshold": threshold,
                    "passes": passes,
                    "pixel_size_mm": pixel_size_mm
                })
            } else if let Some(rec) = laser_core::material::recommend_in_library(
                &effective_library,
                mat,
                thick,
                laser_mode,
                if laser_mode == "cut" {
                    None
                } else {
                    Some(engraving_mode)
                },
                1000.0,
            ) {
                serde_json::json!({
                    "laser_min_power": rec.params.laser_min_power,
                    "laser_max_power": rec.params.laser_max_power,
                    "feed_rate": rec.params.feed_rate,
                    "travel_rate": rec.params.travel_rate,
                    "threshold": rec.params.threshold,
                    "passes": rec.params.passes,
                    "pixel_size_mm": rec.params.pixel_size_mm
                })
            } else {
                continue;
            };

            return Some(serde_json::json!({
                "workflow_id": wf.get("workflow_id").and_then(serde_json::Value::as_str).unwrap_or(""),
                "material": mat,
                "thickness_mm": thick,
                "laser_mode": laser_mode,
                "engraving_mode": engraving_mode,
                "params": params
            }));
        }
        None
    }
}
