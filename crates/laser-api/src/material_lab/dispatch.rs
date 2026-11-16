//! 材料实验室标定矩阵发送（与 workflow confirm_send 同一套门禁）与选格回填材料库。

use super::*;

impl ApiService {
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
        // 线距（LightBurn Interval）在材料库里就是 pixel_size_mm：填充扫描线距 / 光栅点距
        let line_interval_mm = cell
            .get("line_interval_mm")
            .and_then(|v| v.as_f64())
            .or_else(|| cell.get("pixel_size_mm").and_then(|v| v.as_f64()))
            .filter(|v| v.is_finite() && *v > 0.0);
        // 表面通用条目：请求显式指定，或生成矩阵时已标记
        let to_surface = crate::materials::is_surface_request(object)
            || (!object.contains_key("surface")
                && record.get("surface_entry").and_then(|v| v.as_bool()) == Some(true));
        if to_surface && laser_mode == "cut" {
            return Err((
                400,
                failure("表面通用条目只能存雕刻参数，切割必须按真实厚度保存", None),
            ));
        }
        if record
            .get("test_type")
            .and_then(|v| v.as_str())
            .is_some_and(|t| crate::material_lab_patterns::WIZARD_PATTERN_TEST_TYPES.contains(&t))
        {
            return Err((
                400,
                failure(
                    "这是标定向导的测试图案，不按格选参数；请量好结果后在向导里填写保存",
                    Some(serde_json::json!({"error_code": "pattern_not_selectable"})),
                ),
            ));
        }
        let is_interval_test = record.get("test_type").and_then(|v| v.as_str()) == Some("interval");
        let test_label = if is_interval_test {
            "线距测试"
        } else {
            "标定矩阵"
        };
        let notes = object
            .get("notes")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                let interval = line_interval_mm
                    .map(|v| format!("，线距 {v} mm"))
                    .unwrap_or_default();
                format!(
                    "{test_label} {calibration_id} 单元格 #{cell_number}（功率 S{power}、速度 {speed} mm/min、{passes} 遍{interval}）"
                )
            });
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
        if let Some(interval) = line_interval_mm {
            save.insert("pixel_size_mm".into(), serde_json::json!(interval));
        }
        if to_surface {
            save.insert("surface".into(), serde_json::Value::Bool(true));
        }
        if let Some(title) = object.get("title") {
            save.insert("title".into(), title.clone());
        }
        save.insert(
            "source".into(),
            serde_json::Value::String(
                if is_interval_test {
                    "interval_test"
                } else {
                    "calibration"
                }
                .into(),
            ),
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
                    "line_interval_mm": line_interval_mm,
                    "surface": to_surface,
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
            "speech": match line_interval_mm {
                Some(interval) => format!("已将单元格 #{cell_number} 的功率 {power} / 速度 {speed} / {passes} 遍 / 线距 {interval} mm 保存为推荐参数"),
                None => format!("已将单元格 #{cell_number} 的功率 {power} / 速度 {speed} 保存为推荐参数"),
            },
        }))
    }
}
