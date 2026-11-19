use super::super::*;

impl ApiService {
    pub(super) fn route_material(
        &self,
        method: &str,
        path: &str,
        route: &str,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
    ) -> Option<Vec<u8>> {
        match (method, route) {
            ("GET", "/api/ui/material-recommendation") => {
                Some(self.handle_material_recommendation(path))
            }
            ("POST", "/api/material-lab/preview") => Some(self.handle_material_lab_preview(body)),
            ("POST", "/api/material-lab/send") => Some(self.handle_material_lab_send(body, ctx)),
            ("POST", "/api/material-lab/select-cell") => {
                Some(self.handle_material_lab_select_cell(body))
            }
            ("POST", "/api/material-lab/status") => Some(self.handle_material_lab_status(body)),
            ("POST", "/api/material-lab/manage") => Some(self.handle_material_lab_manage(body)),
            _ => None,
        }
    }

    fn handle_material_recommendation(&self, path: &str) -> Vec<u8> {
        let query = path.split_once('?').map(|(_, value)| value).unwrap_or("");
        let material = query_param(query, "material").unwrap_or_default();
        let thickness =
            query_param(query, "thickness_mm").and_then(|value| value.parse::<f64>().ok());
        let laser_mode = query_param(query, "laser_mode").unwrap_or_default();
        let engraving_mode = query_param(query, "engraving_mode");
        let Some(thickness_mm) = thickness.filter(|value| value.is_finite() && *value > 0.0) else {
            return http_json(400, failure("thickness_mm 必须是大于 0 的数字", None));
        };
        if material.trim().is_empty() || laser_mode.trim().is_empty() {
            return http_json(400, failure("material 和 laser_mode 不能为空", None));
        }
        let power_scale = query_param(query, "power_scale")
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(1000.0);
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
        let recommendation = laser_core::material::recommend_in_library(
            &effective_library,
            &material,
            thickness_mm,
            &laser_mode,
            engraving_mode.as_deref(),
            power_scale,
        );
        let Some(recommendation) = recommendation else {
            return http_json(400, failure("读取材料推荐失败", None));
        };
        let match_name = if recommendation.surface {
            "surface"
        } else if (recommendation.requested_thickness_mm - recommendation.matched_thickness_mm)
            .abs()
            < 1e-9
        {
            "exact"
        } else {
            "nearest_thickness"
        };
        let mut result = serde_json::json!({
            "material": recommendation.material,
            "thickness_mm": recommendation.requested_thickness_mm,
            "matched_thickness_mm": recommendation.matched_thickness_mm,
            "match": match_name,
            "laser_mode": recommendation.laser_mode,
            "laser_max_power": recommendation.params.laser_max_power,
            "feed_rate": recommendation.params.feed_rate,
            "passes": recommendation.params.passes,
        });
        if let Some(title) = recommendation.params.title() {
            result["title"] = serde_json::Value::String(title.to_owned());
        }
        // r40：随条目保存的填充 / 切割设置，工作台与画板据此预填设置面板（没有就不带这两个键）
        for key in [
            laser_contracts::MATERIAL_FILL_OPTIONS_KEY,
            laser_contracts::MATERIAL_CUT_OPTIONS_KEY,
        ] {
            if let Some(options) = recommendation.params.layer_options(key) {
                result[key] = serde_json::Value::Object(options.clone());
            }
        }
        if let Some(mode) = recommendation.engraving_mode {
            result["engraving_mode"] = serde_json::Value::String(mode);
        }
        http_json(200, success(result))
    }

    fn handle_material_lab_preview(&self, body: &[u8]) -> Vec<u8> {
        let object = match parse_json_object(body, 64 * 1024) {
            Ok(value) => value,
            Err(error) => return http_json(400, failure(error, None)),
        };
        match self.material_lab_preview(&object) {
            Ok(record) => http_json(200, success(record)),
            Err((status, body)) => http_json(status, body),
        }
    }

    fn handle_material_lab_send(
        &self,
        body: &[u8],
        ctx: &laser_contracts::CallerContext,
    ) -> Vec<u8> {
        let mut object = match parse_json_object(body, 64 * 1024) {
            Ok(value) => value,
            Err(error) => return http_json(400, failure(error, None)),
        };
        strip_identity_fields(&mut object);
        match self.material_lab_send(&object, ctx) {
            Ok(record) => http_json(200, success(record)),
            Err((status, body)) => http_json(status, body),
        }
    }

    fn handle_material_lab_select_cell(&self, body: &[u8]) -> Vec<u8> {
        let object = match parse_json_object(body, 64 * 1024) {
            Ok(value) => value,
            Err(error) => return http_json(400, failure(error, None)),
        };
        match self.material_lab_select_cell(&object) {
            Ok(record) => http_json(200, success(record)),
            Err((status, body)) => http_json(status, body),
        }
    }

    fn handle_material_lab_status(&self, body: &[u8]) -> Vec<u8> {
        let object = match parse_json_object(body, 64 * 1024) {
            Ok(value) => value,
            Err(error) => return http_json(400, failure(error, None)),
        };
        let calibration_id = object
            .get("calibration_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();
        if calibration_id.is_empty() {
            return http_json(400, failure("calibration_id 不能为空", None));
        }
        match self.load_calibration(calibration_id) {
            Some(mut record) => {
                overlay_live_progress(&mut record, &self.active_jobs, calibration_id);
                http_json(200, success(record))
            }
            None => http_json(404, failure("未找到标定矩阵", None)),
        }
    }

    fn handle_material_lab_manage(&self, body: &[u8]) -> Vec<u8> {
        let object = match parse_json_object(body, 64 * 1024) {
            Ok(value) => value,
            Err(error) => return http_json(400, failure(error, None)),
        };
        handle_material_manage(
            &self.settings,
            self.runtime_db.as_ref(),
            &self.materials,
            &self.material_write_lock,
            &object,
        )
    }
}
