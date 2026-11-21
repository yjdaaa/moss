use super::*;

impl ApiService {
    /// 在持久化 `RuntimeStore` 或内存兼容表上统一执行读写操作，消除双轨分支。
    pub(crate) fn with_effective_store<R>(
        &self,
        f: impl FnOnce(&dyn laser_runtime::RuntimeStore) -> R,
    ) -> R {
        if let Some(store) = self.store.as_deref() {
            f(store)
        } else {
            let fallback = LegacyMapStore {
                workflows: &self.workflows,
                calibrations: &self.calibrations,
            };
            f(&fallback)
        }
    }

    /// 读取 workflow / 标定记录（持久化存储优先，否则内存兼容表）。调用方不得持有内存表的锁。
    pub(crate) fn load_record_payload(
        &self,
        kind: laser_runtime::RecordKind,
        id: &str,
    ) -> Option<serde_json::Value> {
        self.with_effective_store(|store| {
            store
                .get(kind, id)
                .ok()
                .flatten()
                .map(|record| record.payload)
        })
    }
}

pub(crate) fn workflow_send_blocked_reason(status: Option<&str>) -> Option<&'static str> {
    match status {
        Some("cancelled") => Some("该 workflow 已取消，不能再发送，请重新生成预览"),
        Some("failed") => Some("该 workflow 已失败，不能再发送，请重新生成预览"),
        Some("sending") => Some("该 workflow 正在发送中，禁止重复下发"),
        Some("completed") => {
            Some("该 workflow 已完成加工；重复加工同一图案请重新生成预览（防止二次烧蚀）")
        }
        _ => None,
    }
}

/// 发送准入预检（规格 §8/§22）：把 workflow 记录中的刀路文件名限定在 RuntimeRoot 允许目录内解析为物理文件，
/// 严格读取记录里已冻结的 64 位 SHA-256 指纹（拒绝用发码当刻磁盘文件现场补算 SHA），
/// 再交给 `laser_device::authorize_send` 以物理文件重算比对。
/// 返回 (preflight, 冻结的 SHA-256, 记录中的刀路文件名)。
pub(crate) fn build_send_preflight(
    payload: &serde_json::Value,
    send_roots: &[std::path::PathBuf],
) -> Result<(laser_device::SendPreflight, String, String), String> {
    let artifact_file = payload
        .get("artifacts")
        .and_then(|a| a.get("gcode_file"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let snapshot_file = payload
        .get("confirmation_snapshot")
        .and_then(|c| c.get("gcode_file"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let (Some(a), Some(s)) = (artifact_file, snapshot_file)
        && a != s
    {
        return Err("artifacts 与 confirmation_snapshot 的刀路文件名不一致，拒绝发送".into());
    }
    let gcode_file = snapshot_file.or(artifact_file).unwrap_or("");
    if gcode_file.is_empty() {
        return Err("当前 workflow 缺少冻结的 G-code 刀路快照（请重新预览生成）".into());
    }
    let gcode_path = resolve_gcode_file(std::path::Path::new(gcode_file), send_roots)
        .map_err(|_| "G-code 刀路文件不存在或不在允许目录内，拒绝发送".to_string())?;
    let valid_sha = |v: Option<&serde_json::Value>| -> Option<String> {
        v.and_then(|s| s.as_str())
            .map(str::trim)
            .filter(|sha| sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(|sha| sha.to_ascii_lowercase())
    };
    let artifact_sha = valid_sha(payload.get("artifacts").and_then(|a| a.get("gcode_sha256")));
    let snapshot_sha = valid_sha(
        payload
            .get("confirmation_snapshot")
            .and_then(|c| c.get("gcode_sha256")),
    );
    if let (Some(a), Some(s)) = (artifact_sha.as_deref(), snapshot_sha.as_deref())
        && a != s
    {
        return Err("artifacts 与 confirmation_snapshot 的 G-code SHA-256 不一致，拒绝发送".into());
    }
    let expected_sha256 = snapshot_sha.or(artifact_sha).ok_or_else(|| {
        "当前记录缺少冻结的 64 位 G-code SHA-256 指纹（老数据请重新预览生成确认快照）".to_string()
    })?;
    let preflight = laser_device::SendPreflight {
        status: "pending".into(),
        expected_sha256: expected_sha256.clone(),
        actual_sha256: None,
        gcode_path: Some(gcode_path),
        workflow_snapshot_valid: true,
        calibration_sha_valid: false,
    };
    Ok((preflight, expected_sha256, gcode_file.to_owned()))
}

/// 通过门禁后把刀路文件名与 SHA 冻结进 confirmation_snapshot（保留画板等已有字段）。
pub(crate) fn freeze_confirmation_snapshot(
    map: &mut serde_json::Map<String, serde_json::Value>,
    gcode_file: &str,
    gcode_sha: &str,
) {
    let now = unix_seconds();
    match map
        .get_mut("confirmation_snapshot")
        .and_then(|value| value.as_object_mut())
    {
        Some(snapshot) => {
            snapshot.insert(
                "gcode_file".into(),
                serde_json::Value::String(gcode_file.to_owned()),
            );
            snapshot.insert(
                "gcode_sha256".into(),
                serde_json::Value::String(gcode_sha.to_owned()),
            );
            snapshot.insert("confirmed_at".into(), serde_json::json!(now));
        }
        None => {
            map.insert(
                "confirmation_snapshot".into(),
                serde_json::json!({
                    "gcode_file": gcode_file,
                    "gcode_sha256": gcode_sha,
                    "confirmed_at": now
                }),
            );
        }
    }
}

/// 后台发送线程收尾：把终态与 send_result 写回存储（SQLite/内存）。
/// 串行化任务记录的两次“读 → 改 → 写”（2026-11-08）：启动方在任务启动后补写 `sending`，
/// 发送线程收尾写终态。二者都对同一条记录先读后写；不加锁时启动方读完、写回前被挂起，
/// 线程恰好在这个空档写入 `completed`，随后会被启动方的 `sending` 覆盖，任务永远显示“发送中”
/// （全工作区并行测试负载下复现）。只在任务启动与结束时各持有一次，开销可以忽略。
static RECORD_TRANSITION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn record_transition_guard() -> std::sync::MutexGuard<'static, ()> {
    RECORD_TRANSITION_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 无持久化 `RuntimeStore` 时的内存兼容适配器，将 `workflows` / `calibrations` 内存表统一纳入
/// `RuntimeStore` 契约，使单套 `handle_workflow_store_request` 覆盖持久化与纯内存两种运行态。
struct LegacyMapStore<'a> {
    workflows: &'a std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>,
    calibrations: &'a std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>,
}

impl<'a> LegacyMapStore<'a> {
    fn map_for(
        &self,
        kind: laser_runtime::RecordKind,
    ) -> Option<&'a std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>> {
        match kind {
            laser_runtime::RecordKind::Workflow => Some(self.workflows),
            laser_runtime::RecordKind::Calibration => Some(self.calibrations),
            _ => None,
        }
    }
}

impl laser_runtime::RuntimeStore for LegacyMapStore<'_> {
    fn get(
        &self,
        kind: laser_runtime::RecordKind,
        id: &str,
    ) -> Result<Option<laser_runtime::StoredRecord>, laser_runtime::SchemaError> {
        let Some(map) = self.map_for(kind) else {
            return Ok(None);
        };
        let guard = map.lock().map_err(|_| laser_runtime::SchemaError::Busy)?;
        let Some(payload) = guard.get(id).cloned() else {
            return Ok(None);
        };
        let status = payload
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        Ok(Some(laser_runtime::StoredRecord {
            kind,
            id: id.to_owned(),
            status,
            payload,
        }))
    }

    fn put(&self, record: &laser_runtime::StoredRecord) -> Result<(), laser_runtime::SchemaError> {
        let Some(map) = self.map_for(record.kind) else {
            return Ok(());
        };
        let mut guard = map.lock().map_err(|_| laser_runtime::SchemaError::Busy)?;
        let mut payload = record.payload.clone();
        if let Some(status) = record.status.as_deref()
            && let Some(obj) = payload.as_object_mut()
        {
            obj.insert(
                "status".into(),
                serde_json::Value::String(status.to_owned()),
            );
        }
        if record.kind == laser_runtime::RecordKind::Workflow
            && !payload["feedback_history"].is_array()
            && let Some(history) = guard
                .get(&record.id)
                .and_then(|prev| prev.get("feedback_history"))
                .filter(|v| v.is_array())
        {
            payload["feedback_history"] = history.clone();
        }
        guard.insert(record.id.clone(), payload);
        Ok(())
    }

    fn set_status(
        &self,
        kind: laser_runtime::RecordKind,
        id: &str,
        status: &str,
        now: f64,
    ) -> Result<bool, laser_runtime::SchemaError> {
        let Some(map) = self.map_for(kind) else {
            return Ok(false);
        };
        let mut guard = map.lock().map_err(|_| laser_runtime::SchemaError::Busy)?;
        let Some(record) = guard.get_mut(id) else {
            return Ok(false);
        };
        record["status"] = serde_json::Value::String(status.to_owned());
        record["updated_at"] = serde_json::json!(now as u64);
        Ok(true)
    }

    fn list_recent(
        &self,
        kind: laser_runtime::RecordKind,
        limit: usize,
    ) -> Result<Vec<laser_runtime::StoredRecord>, laser_runtime::SchemaError> {
        let Some(map) = self.map_for(kind) else {
            return Ok(Vec::new());
        };
        let guard = map.lock().map_err(|_| laser_runtime::SchemaError::Busy)?;
        let mut records: Vec<laser_runtime::StoredRecord> = guard
            .iter()
            .map(|(id, payload)| {
                let status = payload
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                laser_runtime::StoredRecord {
                    kind,
                    id: id.clone(),
                    status,
                    payload: payload.clone(),
                }
            })
            .collect();
        let ts = |v: &serde_json::Value| {
            v.get("updated_at")
                .and_then(serde_json::Value::as_f64)
                .or_else(|| v.get("created_at").and_then(serde_json::Value::as_f64))
                .unwrap_or(0.0)
        };
        records.sort_by(|a, b| {
            ts(&b.payload)
                .total_cmp(&ts(&a.payload))
                .then_with(|| a.id.cmp(&b.id))
        });
        records.truncate(limit);
        Ok(records)
    }

    fn save_feedback(
        &self,
        workflow_id: &str,
        feedback: &serde_json::Value,
    ) -> Result<bool, laser_runtime::SchemaError> {
        let mut guard = self
            .workflows
            .lock()
            .map_err(|_| laser_runtime::SchemaError::Busy)?;
        let Some(payload) = guard.get_mut(workflow_id) else {
            return Ok(false);
        };
        laser_runtime::store::merge_feedback(payload, feedback);
        Ok(true)
    }

    fn adopt_feedback(
        &self,
        workflow_id: &str,
        feedback: &serde_json::Value,
        preview: &laser_runtime::StoredRecord,
    ) -> Result<bool, laser_runtime::SchemaError> {
        let mut guard = self
            .workflows
            .lock()
            .map_err(|_| laser_runtime::SchemaError::Busy)?;
        let Some(payload) = guard.get_mut(workflow_id) else {
            return Ok(false);
        };
        if !laser_runtime::store::feedback_adoptable(payload, feedback) {
            return Ok(false);
        }
        laser_runtime::store::merge_feedback(payload, feedback);
        guard.insert(preview.id.clone(), preview.payload.clone());
        Ok(true)
    }
}

pub(crate) fn finalize_workflow_record(
    store: Option<&(dyn laser_runtime::RuntimeStore + Send + Sync)>,
    workflows: &std::sync::Arc<
        std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>,
    >,
    calibrations: &std::sync::Arc<
        std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>,
    >,
    event_sink: &SharedLaserEventSink,
    kind: laser_runtime::RecordKind,
    workflow_id: &str,
    status: &str,
    send_result: serde_json::Value,
) {
    let now = unix_seconds();
    let _transition = record_transition_guard();
    let fallback = LegacyMapStore {
        workflows,
        calibrations,
    };
    let effective_store: &dyn laser_runtime::RuntimeStore = match store {
        Some(store) => store,
        None => &fallback,
    };
    match effective_store.get(kind, workflow_id) {
        Ok(Some(mut stored)) => {
            // 关停协调器写入的 failed / result_unknown 是不可逆终态，后台发送线程收尾绝不得覆写为 cancelled 或 completed
            let already_result_unknown = stored.status.as_deref() == Some("failed")
                && stored
                    .payload
                    .get("send_result")
                    .and_then(|sr| sr.get("phase"))
                    .and_then(|v| v.as_str())
                    == Some("result_unknown");
            if already_result_unknown {
                return;
            }
            // 取消请求可能已由 HTTP 层写入 cancelled；不要用 completed 覆盖用户取消
            let already_cancelled = stored.status.as_deref() == Some("cancelled")
                || stored.payload.get("status").and_then(|v| v.as_str()) == Some("cancelled");
            let final_status = if already_cancelled && status == "completed" {
                "cancelled"
            } else {
                status
            };
            stored.status = Some(final_status.into());
            if let Some(map) = stored.payload.as_object_mut() {
                map.insert(
                    "status".into(),
                    serde_json::Value::String(final_status.into()),
                );
                map.insert("updated_at".into(), serde_json::json!(now));
                map.insert("send_result".into(), send_result);
            }
            if let Err(error) = effective_store.put(&stored) {
                eprintln!("[laser-api] 任务 {workflow_id} 终态落库失败: {error:?}");
                emit_laser_event(
                    event_sink,
                    LaserEvent {
                        level: LaserEventLevel::Warn,
                        text: format!(
                            "任务 {workflow_id} 结束状态写入数据库失败（{final_status}）"
                        ),
                        hint: Some(
                            "请检查运行目录磁盘空间与文件权限，并到设备旁核实实际停机状态".into(),
                        ),
                        detail: Some(format!("{error:?}")),
                    },
                );
            }
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("[laser-api] 任务 {workflow_id} 终态读取失败: {error:?}");
            emit_laser_event(
                event_sink,
                LaserEvent {
                    level: LaserEventLevel::Warn,
                    text: format!("任务 {workflow_id} 结束状态读取失败（{status}）"),
                    hint: Some("请检查运行目录数据库状态，并到设备旁核实实际停机状态".into()),
                    detail: Some(format!("{error:?}")),
                },
            );
        }
    }
}

/// 发送线程启动后将 `sending` 状态落库；无论是读取还是写回失败，都立即请求停止刚启动的发送并返回 500，
/// 杜绝状态落库失败却向前端返回 200 并让设备在后台无记录盲刻。
pub(crate) fn persist_launched_sending_record(
    store: &dyn laser_runtime::RuntimeStore,
    kind: laser_runtime::RecordKind,
    record_id: &str,
    snapshot_before_launch: &serde_json::Value,
    response_payload: &serde_json::Value,
    gcode_file: &str,
    gcode_sha: &str,
    cancel_epoch_before: u64,
    jobs: &ActiveJobs,
    reservations: &LaunchReservations,
) -> Result<(), (u16, serde_json::Value)> {
    let transition = record_transition_guard();
    if current_cancel_epoch(reservations, record_id) != cancel_epoch_before {
        return Ok(());
    }
    let (label, error_code) = match kind {
        laser_runtime::RecordKind::Calibration => ("标定矩阵", "calibration_persist_failed"),
        _ => ("workflow", "workflow_persist_failed"),
    };
    let abort_reason = match store.get(kind, record_id) {
        Ok(Some(mut latest)) => {
            if &latest.payload == snapshot_before_launch {
                latest.status = Some("sending".into());
                latest.payload = response_payload.clone();
            } else if let Some(map) = latest.payload.as_object_mut() {
                freeze_confirmation_snapshot(map, gcode_file, gcode_sha);
            }
            match store.put(&latest) {
                Ok(()) => return Ok(()),
                Err(error) => format!("{error:?}"),
            }
        }
        Ok(None) => "记录不存在".into(),
        Err(error) => format!("{error:?}"),
    };
    let (_, job_to_hold) = mark_job_cancelled_in_memory(jobs, reservations, record_id);
    drop(transition);
    flush_cancel_feed_hold(job_to_hold);
    Err((
        500,
        failure(
            format!(
                "保存 {label} 状态失败（{abort_reason}），已请求停止刚启动的发送，请到设备旁确认"
            ),
            Some(serde_json::json!({"error_code": error_code})),
        ),
    ))
}

/// 已全量完成且无活动发送的任务不可再取消：`completed` 是设备逐行确认后的历史记录，
/// 改写成 `cancelled` 会抹掉事实，此时取消也不会向设备发出任何字节。
fn cancel_completed_rejected() -> Vec<u8> {
    http_json(
        400,
        failure(
            "该任务已全量发送完成，取消不会再向设备发出任何指令；如设备仍在运动，请使用急停",
            Some(serde_json::json!({"error_code": "workflow_completed"})),
        ),
    )
}

/// 断点行号只能由 `resume` 从服务端记录的 `lines_sent` 写入；`confirm_send` / `send` 若接受客户端
/// 自带的 `start_from_line`，会绕过续雕校验并在跳过开头 G21/G90/M5 设置行前先发 `$X` 解锁。
fn reject_client_resume_line(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Option<Vec<u8>> {
    object.contains_key("start_from_line").then(|| {
        http_json(
            400,
            failure(
                "确认发送不接受 start_from_line；断点续雕请使用 action=resume",
                Some(serde_json::json!({"error_code": "resume_line_not_allowed"})),
            ),
        )
    })
}

pub(crate) fn preview_workflow_id_conflict(
    store: &dyn laser_runtime::RuntimeStore,
    jobs: &ActiveJobs,
    reservations: &LaunchReservations,
    workflow_id: &str,
) -> Option<(u16, String, &'static str)> {
    let busy_in_ram = jobs
        .lock()
        .map(|g| g.contains_key(workflow_id))
        .unwrap_or(true)
        || reservations
            .lock()
            .map(|r| r.active.contains_key(workflow_id))
            .unwrap_or(true);
    if busy_in_ram {
        return Some((
            409,
            format!("workflow {workflow_id} 正在启动或发送中，禁止预览覆盖"),
            "device_busy",
        ));
    }
    for kind in [
        laser_runtime::RecordKind::Workflow,
        laser_runtime::RecordKind::Calibration,
    ] {
        match store.get(kind, workflow_id) {
            Ok(Some(existing)) => {
                let st = existing
                    .status
                    .as_deref()
                    .or_else(|| existing.payload.get("status").and_then(|v| v.as_str()))
                    .unwrap_or("");
                if matches!(st, "sending" | "running" | "preparing" | "processing") {
                    return Some((
                        409,
                        format!("workflow {workflow_id} 正在发送中，禁止预览覆盖"),
                        "device_busy",
                    ));
                }
                return Some((
                    409,
                    format!("workflow {workflow_id} 已存在（状态 {st}），请使用新 ID 创建预览"),
                    "workflow_id_conflict",
                ));
            }
            Ok(None) => {}
            Err(_) => {
                return Some((500, "检查 workflow_id 是否已存在失败".into(), "store_error"));
            }
        }
    }
    None
}

pub(crate) fn handle_workflow_store_request(
    store: &dyn laser_runtime::RuntimeStore,
    route: &str,
    object: &serde_json::Map<String, serde_json::Value>,
    send_roots: &[std::path::PathBuf],
    jobs: &ActiveJobs,
    reservations: &LaunchReservations,
    requested_spec: &Result<laser_device::ConnectionSpec, String>,
    launch: SendLauncher<'_>,
) -> Vec<u8> {
    let action = if route == "/api/text-task/generate" {
        "preview"
    } else if route == "/api/text-task/send" {
        "confirm_send"
    } else {
        object
            .get("action")
            .and_then(|value| value.as_str())
            .unwrap_or("status")
    };
    let requested_id = object
        .get("workflow_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    if action == "recent" || (action == "status" && requested_id.is_none()) {
        let records = match store.list_recent(laser_runtime::RecordKind::Workflow, 50) {
            Ok(records) => records,
            Err(_) => return http_json(500, failure("读取 workflow 失败", None)),
        };
        let workflows = records
            .into_iter()
            .map(|record| record.payload)
            .collect::<Vec<_>>();
        return http_json(
            200,
            success(serde_json::json!({
                "workflows": workflows,
                "speech": "已列出最近的激光 workflow；可以用 workflow_id 继续查询、反馈或重生成。"
            })),
        );
    }
    match action {
        "preview" | "plan" | "create" => {
            let _transition = record_transition_guard();
            let workflow_id = match requested_id {
                Some(id) => {
                    if let Some((status, msg, code)) =
                        preview_workflow_id_conflict(store, jobs, reservations, &id)
                    {
                        return http_json(
                            status,
                            failure(msg, Some(serde_json::json!({"error_code": code}))),
                        );
                    }
                    id
                }
                None => next_workflow_id(),
            };
            let mut artifacts = object
                .get("_artifacts")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let mut input = object.clone();
            input.remove("_artifacts");
            let origin = input.remove("_origin").unwrap_or(serde_json::Value::Null);
            if let Err(error) =
                validate_material_safety_in_payload(&serde_json::Value::Object(input.clone()))
            {
                return http_json(
                    400,
                    failure(
                        error,
                        Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                    ),
                );
            }

            let gcode_name = artifacts
                .get("gcode_file")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("")
                .to_owned();
            let mut gcode_sha = artifacts
                .get("gcode_sha256")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|sha| sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(|sha| sha.to_ascii_lowercase())
                .unwrap_or_default();
            if !gcode_name.is_empty() && gcode_sha.is_empty() {
                let Ok(gcode_path) =
                    resolve_gcode_file(std::path::Path::new(&gcode_name), send_roots)
                else {
                    return http_json(
                        400,
                        failure("预览刀路文件不存在或不在允许目录内，无法冻结 SHA-256", None),
                    );
                };
                let Ok(bytes) = std::fs::read(&gcode_path) else {
                    return http_json(400, failure("读取预览刀路文件失败，无法冻结 SHA-256", None));
                };
                gcode_sha = hex::encode(Sha256::digest(&bytes));
                if let Some(art_map) = artifacts.as_object_mut() {
                    art_map.insert(
                        "gcode_sha256".into(),
                        serde_json::Value::String(gcode_sha.clone()),
                    );
                }
            }
            let material = input.get("material").and_then(|v| v.as_str()).unwrap_or("");
            let thickness =
                parse_float(input.get("thickness_mm"), "thickness_mm", 3.0).unwrap_or(3.0);
            let confirmation_snapshot = if !gcode_name.is_empty() && !gcode_sha.is_empty() {
                serde_json::json!({
                    "gcode_file": gcode_name,
                    "gcode_sha256": gcode_sha,
                    "material": material,
                    "thickness_mm": thickness,
                })
            } else {
                serde_json::Value::Null
            };

            let record = serde_json::json!({
                "workflow_id": workflow_id,
                "status": "preview_ready",
                "source_type": object.get("source_type").and_then(|value| value.as_str()).unwrap_or(
                    if route == "/api/text-task/generate" { "text" } else { "image" }
                ),
                "input": input,
                "artifacts": artifacts,
                "confirmation_snapshot": confirmation_snapshot,
                "origin": origin,
                "next_actions": ["status", "cancel", "confirm_send"],
                "created_at": unix_seconds(),
                "updated_at": unix_seconds()
            });
            let stored = laser_runtime::StoredRecord {
                kind: laser_runtime::RecordKind::Workflow,
                id: workflow_id,
                status: Some("preview_ready".into()),
                payload: record.clone(),
            };
            if store.put(&stored).is_err() {
                return http_json(500, failure("保存 workflow 失败", None));
            }
            http_json(200, success(record))
        }
        "status" => {
            let Some(workflow_id) = requested_id else {
                return http_json(400, failure("workflow_id 不能为空", None));
            };
            match store.get(laser_runtime::RecordKind::Workflow, &workflow_id) {
                Ok(Some(mut stored)) => {
                    overlay_live_progress(&mut stored.payload, jobs, &workflow_id);
                    http_json(200, success(stored.payload))
                }
                Ok(None) => match store.get(laser_runtime::RecordKind::Calibration, &workflow_id) {
                    Ok(Some(mut stored)) => {
                        overlay_live_progress(&mut stored.payload, jobs, &workflow_id);
                        http_json(200, success(stored.payload))
                    }
                    Ok(None) => {
                        if let Ok(guard) = jobs.lock()
                            && let Some(job) = guard.get(&workflow_id)
                        {
                            return http_json(
                                200,
                                success(serde_json::json!({
                                    "workflow_id": workflow_id,
                                    "status": "sending",
                                    "send_result": job.live_send_result(),
                                })),
                            );
                        }
                        http_json(404, failure("未找到 workflow", None))
                    }
                    Err(_) => http_json(500, failure("读取 workflow 失败", None)),
                },
                Err(_) => http_json(500, failure("读取 workflow 失败", None)),
            }
        }
        "cancel" => {
            let Some(workflow_id) = requested_id else {
                return http_json(400, failure("workflow_id 不能为空", None));
            };
            let transition = record_transition_guard();
            // 发送线程在 G4 P0 同步 ACK + Idle 双确认后持同一把 transition 锁写入 completed；
            // 只要记录已是 completed（即便 active_jobs 移除尚未执行完），绝不将其篡改为 cancelled。
            let existing = [
                laser_runtime::RecordKind::Workflow,
                laser_runtime::RecordKind::Calibration,
            ]
            .into_iter()
            .find_map(|kind| store.get(kind, &workflow_id).ok().flatten());
            if existing.as_ref().is_some_and(|stored| {
                stored
                    .status
                    .as_deref()
                    .or_else(|| stored.payload.get("status").and_then(|v| v.as_str()))
                    == Some("completed")
            }) {
                return cancel_completed_rejected();
            }
            // 安全铁律：一旦确认非 completed，无论后续落库是否报错都必须先置位取消令牌并在锁外发 FeedHold 停机。
            let (signalled, job_to_hold) =
                mark_job_cancelled_in_memory(jobs, reservations, &workflow_id);
            let now = unix_seconds() as f64;
            let response = match store.set_status(
                laser_runtime::RecordKind::Workflow,
                &workflow_id,
                "cancelled",
                now,
            ) {
                Ok(true) => match store.get(laser_runtime::RecordKind::Workflow, &workflow_id) {
                    Ok(Some(stored)) => http_json(200, success(stored.payload)),
                    _ => http_json(500, failure("读取 workflow 失败", None)),
                },
                Ok(false) => match store.set_status(
                    laser_runtime::RecordKind::Calibration,
                    &workflow_id,
                    "cancelled",
                    now,
                ) {
                    Ok(true) => {
                        match store.get(laser_runtime::RecordKind::Calibration, &workflow_id) {
                            Ok(Some(stored)) => http_json(200, success(stored.payload)),
                            _ => http_json(500, failure("读取标定任务失败", None)),
                        }
                    }
                    Ok(false) => {
                        if signalled {
                            http_json(
                                200,
                                success(serde_json::json!({
                                    "workflow_id": workflow_id,
                                    "status": "cancelled"
                                })),
                            )
                        } else {
                            http_json(404, failure("未找到 workflow", None))
                        }
                    }
                    Err(_) => http_json(500, failure("取消 workflow 失败", None)),
                },
                Err(_) => http_json(500, failure("取消 workflow 失败", None)),
            };
            drop(transition);
            flush_cancel_feed_hold(job_to_hold);
            response
        }
        "resume" => {
            let Some(workflow_id) = requested_id else {
                return http_json(400, failure("workflow_id 不能为空", None));
            };
            let confirmed = object
                .get("confirmed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !confirmed {
                return http_json(
                    400,
                    failure("请确认断点恢复安全选项 (confirmed=true)", None),
                );
            }
            let cancel_epoch_before = current_cancel_epoch(reservations, &workflow_id);
            let Some(mut stored) =
                (match store.get(laser_runtime::RecordKind::Workflow, &workflow_id) {
                    Ok(value) => value,
                    Err(_) => return http_json(500, failure("读取 workflow 失败", None)),
                })
            else {
                return http_json(404, failure("未找到 workflow", None));
            };
            let validate_resume_record =
                |rec: &laser_runtime::StoredRecord| -> Result<usize, (u16, serde_json::Value)> {
                    if let Err(error) = validate_material_safety_in_payload(&rec.payload) {
                        return Err((
                            400,
                            failure(
                                error,
                                Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                            ),
                        ));
                    }
                    let status = rec
                        .status
                        .as_deref()
                        .or_else(|| rec.payload.get("status").and_then(|v| v.as_str()));
                    if status == Some("sending") {
                        return Err((
                            409,
                            failure(
                                "该 workflow 正在发送中，禁止重复操作",
                                Some(serde_json::json!({"error_code": "device_busy"})),
                            ),
                        ));
                    }
                    if status == Some("completed") {
                        return Err((
                            400,
                            failure(
                                "该 workflow 已全量完成，无法断点继续",
                                Some(serde_json::json!({"error_code": "workflow_completed"})),
                            ),
                        ));
                    }
                    if status != Some("cancelled") {
                        return Err((
                            400,
                            failure(
                                "仅已取消且保留可恢复断点的 workflow 支持断点续雕，请重新发起确认发送",
                                Some(serde_json::json!({"error_code": "workflow_not_resumable"})),
                            ),
                        ));
                    }
                    let send_result = rec.payload.get("send_result").cloned().unwrap_or_default();
                    if send_result.get("mode").and_then(|v| v.as_str()) == Some("sd_offline")
                        || send_result.get("can_resume").and_then(|v| v.as_bool()) != Some(true)
                        || object.get("offline_sd").and_then(|v| v.as_bool()) == Some(true)
                    {
                        return Err((
                            400,
                            failure(
                                "该任务不支持按行断点续雕（如 SD 脱机整文件作业或未产生可恢复断点），请重新发起确认发送",
                                Some(serde_json::json!({"error_code": "workflow_not_resumable"})),
                            ),
                        ));
                    }
                    let original_target = send_result
                        .get("target")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty());
                    let Some(orig_target) = original_target else {
                        return Err((
                            400,
                            failure(
                                "原任务记录缺少连接目标锁定信息（target），禁止跨设备或盲目续雕，请重新发起确认发送",
                                Some(serde_json::json!({"error_code": "workflow_not_resumable"})),
                            ),
                        ));
                    };
                    let req_spec = match &requested_spec {
                        Ok(spec) => spec,
                        Err(e) => {
                            return Err((
                                400,
                                failure(
                                    format!("连接参数无效: {e}"),
                                    Some(
                                        serde_json::json!({"error_code": "connection_params_invalid"}),
                                    ),
                                ),
                            ));
                        }
                    };
                    if !req_spec.matches_target(orig_target) {
                        return Err((
                            400,
                            failure(
                                format!(
                                    "续雕连接目标（{}）与原任务锁定设备（{orig_target}）不一致，禁止用旧断点/坐标对新目标续雕",
                                    req_spec.describe()
                                ),
                                Some(serde_json::json!({"error_code": "workflow_not_resumable"})),
                            ),
                        ));
                    }
                    let total_lines = send_result
                        .get("total_lines")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                    let boundary_source = send_result
                        .get("resume_boundary_source")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let resumable_from_line = send_result
                        .get("resumable_from_line")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as usize);
                    let lines_completed = send_result
                        .get("lines_completed")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as usize);
                    let lines_acked = send_result
                        .get("lines_sent")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as usize);
                    match (boundary_source, resumable_from_line, lines_completed) {
                        ("device_hold_ln", Some(resumable), Some(completed))
                            if resumable > 0
                                && resumable == completed
                                && (total_lines == 0 || resumable < total_lines)
                                && lines_acked.is_none_or(|acked| resumable <= acked) =>
                        {
                            Ok(resumable)
                        }
                        _ => Err((
                            400,
                            failure(
                                "该任务断点缺少设备直接证实来源（resume_boundary_source=device_hold_ln），拒绝按未知或入队 ACK 续雕，请重新发起确认发送",
                                Some(serde_json::json!({"error_code": "workflow_not_resumable"})),
                            ),
                        )),
                    }
                };
            if let Err((status, body)) = validate_resume_record(&stored) {
                return http_json(status, body);
            }

            let _launch_guard = match reserve_workflow_launch(
                jobs,
                reservations,
                &workflow_id,
                Some(cancel_epoch_before),
            ) {
                Ok(guard) => guard,
                Err(ReserveLaunchError::Busy) => {
                    return http_json(
                        409,
                        failure(
                            "该 workflow 正在启动或发送中，禁止重复操作",
                            Some(serde_json::json!({"error_code": "device_busy"})),
                        ),
                    );
                }
                Err(ReserveLaunchError::Cancelled) => {
                    return http_json(
                        409,
                        failure(
                            "任务已被取消，未启动续雕",
                            Some(serde_json::json!({"error_code": "workflow_cancelled"})),
                        ),
                    );
                }
            };
            // 持预约锁后重新读取记录并完整复核续雕资格与断点行，杜绝并发续雕或失败/急停后的 TOCTOU 复放旧行号
            let resume_line = match store.get(laser_runtime::RecordKind::Workflow, &workflow_id) {
                Ok(Some(fresh)) => {
                    let fresh_line = match validate_resume_record(&fresh) {
                        Ok(line) => line,
                        Err((status, body)) => return http_json(status, body),
                    };
                    stored = fresh;
                    fresh_line
                }
                _ => return http_json(500, failure("读取 workflow 失败", None)),
            };
            let (preflight, gcode_sha, gcode_file) =
                match build_send_preflight(&stored.payload, send_roots) {
                    Ok(value) => value,
                    Err(error) => {
                        return http_json(
                            400,
                            failure(
                                format!("发送安全门禁未通过: {error}"),
                                Some(serde_json::json!({"error_code": "send_gate_rejected"})),
                            ),
                        );
                    }
                };
            if let Err(error) = laser_device::authorize_send(&preflight) {
                return http_json(
                    400,
                    failure(
                        format!("发送安全门禁未通过: {error}"),
                        Some(serde_json::json!({"error_code": "send_gate_rejected"})),
                    ),
                );
            }
            let gcode_path = preflight.gcode_path.clone().unwrap_or_default();
            let mut resume_object = object.clone();
            resume_object.remove("_origin_mpos");
            if let Some(origin_mpos) = stored
                .payload
                .get("send_result")
                .and_then(|sr| sr.get("origin_mpos"))
                .cloned()
            {
                resume_object.insert("_origin_mpos".into(), origin_mpos);
            }
            resume_object.insert("start_from_line".into(), serde_json::json!(resume_line));
            let snapshot_before_launch = stored.payload.clone();
            let send_result = match launch(&workflow_id, gcode_path, &gcode_sha, &resume_object) {
                Ok(value) => value,
                Err((status, body)) => return http_json(status, body),
            };
            let mut response_payload = snapshot_before_launch.clone();
            if let Some(map) = response_payload.as_object_mut() {
                map.insert("status".into(), serde_json::Value::String("sending".into()));
                map.insert("updated_at".into(), serde_json::json!(unix_seconds()));
                map.insert("send_result".into(), send_result);
                freeze_confirmation_snapshot(map, &gcode_file, &gcode_sha);
            }
            if let Err((status, body)) = persist_launched_sending_record(
                store,
                laser_runtime::RecordKind::Workflow,
                &workflow_id,
                &snapshot_before_launch,
                &response_payload,
                &gcode_file,
                &gcode_sha,
                cancel_epoch_before,
                jobs,
                reservations,
            ) {
                return http_json(status, body);
            }
            http_json(200, success(response_payload))
        }
        "confirm_send" | "send" => {
            let Some(workflow_id) = requested_id else {
                return http_json(400, failure("workflow_id 不能为空", None));
            };
            if let Some(rejected) = reject_client_resume_line(object) {
                return rejected;
            }
            let confirmed = object
                .get("confirmed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !confirmed {
                return http_json(400, failure("请确认发送安全选项 (confirmed=true)", None));
            };
            let cancel_epoch_before = current_cancel_epoch(reservations, &workflow_id);
            match store.get(laser_runtime::RecordKind::Workflow, &workflow_id) {
                Ok(Some(mut stored)) => {
                    let has_frozen_snapshot = |obj: Option<&serde_json::Value>| -> bool {
                        let Some(map) = obj.and_then(|v| v.as_object()) else {
                            return false;
                        };
                        let has_file = map
                            .get("gcode_file")
                            .and_then(|v| v.as_str())
                            .is_some_and(|s| !s.trim().is_empty());
                        let has_sha = map
                            .get("gcode_sha256")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .is_some_and(|sha| {
                                sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())
                            });
                        has_file && has_sha
                    };
                    let has_gcode =
                        has_frozen_snapshot(stored.payload.get("confirmation_snapshot"))
                            || has_frozen_snapshot(stored.payload.get("artifacts"));
                    if !has_gcode {
                        return http_json(
                            400,
                            failure(
                                "当前 workflow 没有有效的冻结 G-code 快照（缺少文件或 64 位 SHA-256 指纹），请重新预览生成",
                                Some(
                                    serde_json::json!({"error_code": "confirmation_snapshot_missing"}),
                                ),
                            ),
                        );
                    }
                    if let Err(error) = validate_material_safety_in_payload(&stored.payload) {
                        return http_json(
                            400,
                            failure(
                                error,
                                Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                            ),
                        );
                    }

                    let current_status = stored
                        .status
                        .as_deref()
                        .or_else(|| stored.payload.get("status").and_then(|v| v.as_str()));
                    if let Some(reason) = workflow_send_blocked_reason(current_status) {
                        return http_json(
                            400,
                            failure(
                                reason,
                                Some(serde_json::json!({"error_code": "workflow_not_sendable"})),
                            ),
                        );
                    }
                    let _launch_guard = match reserve_workflow_launch(
                        jobs,
                        reservations,
                        &workflow_id,
                        Some(cancel_epoch_before),
                    ) {
                        Ok(guard) => guard,
                        Err(ReserveLaunchError::Busy) => {
                            return http_json(
                                409,
                                failure(
                                    "该 workflow 正在启动或发送中，禁止重复操作",
                                    Some(serde_json::json!({"error_code": "device_busy"})),
                                ),
                            );
                        }
                        Err(ReserveLaunchError::Cancelled) => {
                            return http_json(
                                409,
                                failure(
                                    "任务已被取消，未开始发送",
                                    Some(serde_json::json!({"error_code": "workflow_cancelled"})),
                                ),
                            );
                        }
                    };
                    match store.get(laser_runtime::RecordKind::Workflow, &workflow_id) {
                        Ok(Some(fresh)) => {
                            let fresh_status = fresh
                                .status
                                .as_deref()
                                .or_else(|| fresh.payload.get("status").and_then(|v| v.as_str()));
                            if let Some(reason) = workflow_send_blocked_reason(fresh_status) {
                                return http_json(
                                    400,
                                    failure(
                                        reason,
                                        Some(
                                            serde_json::json!({"error_code": "workflow_not_sendable"}),
                                        ),
                                    ),
                                );
                            }
                            stored = fresh;
                        }
                        _ => return http_json(500, failure("读取 workflow 失败", None)),
                    }
                    let (preflight, gcode_sha, gcode_file) =
                        match build_send_preflight(&stored.payload, send_roots) {
                            Ok(value) => value,
                            Err(error) => {
                                return http_json(
                                    400,
                                    failure(
                                        format!("发送安全门禁未通过: {error}"),
                                        Some(
                                            serde_json::json!({"error_code": "send_gate_rejected"}),
                                        ),
                                    ),
                                );
                            }
                        };
                    if let Err(error) = laser_device::authorize_send(&preflight) {
                        return http_json(
                            400,
                            failure(
                                format!("发送安全门禁未通过: {error}"),
                                Some(serde_json::json!({"error_code": "send_gate_rejected"})),
                            ),
                        );
                    }

                    let gcode_path = preflight.gcode_path.clone().unwrap_or_default();
                    let snapshot_before_launch = stored.payload.clone();
                    let send_result = match launch(&workflow_id, gcode_path, &gcode_sha, object) {
                        Ok(value) => value,
                        Err((status, body)) => return http_json(status, body),
                    };
                    let mut response_payload = snapshot_before_launch.clone();
                    if let Some(map) = response_payload.as_object_mut() {
                        map.insert("status".into(), serde_json::Value::String("sending".into()));
                        map.insert("updated_at".into(), serde_json::json!(unix_seconds()));
                        map.insert("send_result".into(), send_result);
                        freeze_confirmation_snapshot(map, &gcode_file, &gcode_sha);
                    }
                    if let Err((status, body)) = persist_launched_sending_record(
                        store,
                        laser_runtime::RecordKind::Workflow,
                        &workflow_id,
                        &snapshot_before_launch,
                        &response_payload,
                        &gcode_file,
                        &gcode_sha,
                        cancel_epoch_before,
                        jobs,
                        reservations,
                    ) {
                        return http_json(status, body);
                    }
                    http_json(200, success(response_payload))
                }
                Ok(None) => http_json(404, failure("未找到 workflow", None)),
                Err(_) => http_json(500, failure("读取 workflow 失败", None)),
            }
        }
        _ => http_json(400, failure("未知 workflow action", None)),
    }
}
