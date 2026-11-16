//! 材料实验室：标定矩阵的存取与最近材料任务查询；预览见 `preview`，发送与选格回填见 `dispatch`。

use super::*;

mod dispatch;
mod preview;

impl ApiService {
    pub(crate) fn load_calibration(&self, calibration_id: &str) -> Option<serde_json::Value> {
        self.load_record_payload(laser_runtime::RecordKind::Calibration, calibration_id)
    }

    /// 写入标定矩阵记录（SQLite 优先，否则内存兼容表）。
    pub(crate) fn save_calibration(&self, record: &serde_json::Value) -> Result<(), String> {
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
