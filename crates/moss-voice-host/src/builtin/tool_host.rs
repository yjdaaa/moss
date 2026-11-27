//! 语音调用 MOSS 激光工具的宿主与刀路预览摘要格式化。

use super::*;

/// 对话中调用 MOSS 工具（NV4）：身份 `CallerContext::Voice`；运动与出光跟随严格模式开关，桌面控制类工具始终拒绝。
fn preview_candidate_from_json(val: &Value) -> Option<&str> {
    [
        val.pointer("/workflow/artifacts/path_preview_file"),
        val.pointer("/workflow/artifacts/preview_file"),
        val.pointer("/workflow/artifacts/image_file"),
        val.pointer("/artifacts/path_preview_file"),
        val.pointer("/artifacts/preview_file"),
        val.pointer("/artifacts/image_file"),
        val.get("path_preview_file"),
        val.get("preview_file"),
        val.get("image_file"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .map(str::trim)
    .find(|s| !s.is_empty())
}

fn format_workflow_oled_summary(val: &Value) -> Option<String> {
    let wf = val.get("workflow").unwrap_or(val);
    if !wf.is_object() {
        return None;
    }
    let input = wf.get("input").unwrap_or(wf);
    let snap = wf.get("confirmation_snapshot").unwrap_or(wf);
    let artifacts = wf.get("artifacts").unwrap_or(wf);
    let estimate = wf
        .get("estimate")
        .or_else(|| artifacts.get("estimate"))
        .unwrap_or(wf);

    let mut parts: Vec<String> = Vec::new();
    let mat = input
        .get("material")
        .or_else(|| wf.get("material"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let thick = input
        .get("thickness_mm")
        .or_else(|| wf.get("thickness_mm"))
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v > 0.0);
    let mode_label = match input
        .get("laser_mode")
        .or_else(|| wf.get("laser_mode"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
    {
        "cut" => "切割",
        "engrave" => "雕刻",
        _ => "",
    };
    match (thick, mat) {
        (Some(t), Some(m)) => {
            let t_str = if (t.fract()).abs() < 1e-3 {
                format!("{}mm", t.round() as i64)
            } else {
                format!("{t:.1}mm")
            };
            parts.push(format!("{t_str}{m}{mode_label}"));
        }
        (None, Some(m)) => parts.push(format!("{m}{mode_label}")),
        (Some(t), None) if !mode_label.is_empty() => parts.push(format!("{t:.1}mm{mode_label}")),
        _ if !mode_label.is_empty() => parts.push(mode_label.to_owned()),
        _ => {}
    }

    if let Some(txt) = input
        .get("text")
        .or_else(|| wf.get("text"))
        .and_then(Value::as_str)
        .map(|s| s.replace(['\r', '\n'], " ").trim().to_owned())
        .filter(|s| !s.is_empty())
    {
        let short: String = txt.chars().take(10).collect();
        let ell = if txt.chars().count() > 10 { "…" } else { "" };
        parts.push(format!("「{short}{ell}」"));
    }

    let w_mm = estimate
        .get("width_mm")
        .or_else(|| artifacts.get("width_mm"))
        .or_else(|| input.get("width_mm"))
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v > 0.0);
    let h_mm = estimate
        .get("height_mm")
        .or_else(|| artifacts.get("height_mm"))
        .or_else(|| input.get("height_mm"))
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v > 0.0);
    if let (Some(w), Some(h)) = (w_mm, h_mm) {
        parts.push(format!("{}×{}mm", w.round() as i64, h.round() as i64));
    }

    let power = snap
        .get("power")
        .or_else(|| input.get("power"))
        .and_then(Value::as_i64);
    let feed = snap
        .get("feed_rate")
        .or_else(|| input.get("feed_rate"))
        .and_then(Value::as_i64);
    let passes = snap
        .get("passes")
        .or_else(|| input.get("passes"))
        .and_then(Value::as_i64);
    let mut p_tokens = Vec::new();
    if let Some(p) = power {
        p_tokens.push(format!("S{p}"));
    }
    if let Some(f) = feed {
        p_tokens.push(format!("F{f}"));
    }
    if let Some(ps) = passes {
        p_tokens.push(format!("×{ps}遍"));
    }
    if !p_tokens.is_empty() {
        parts.push(p_tokens.join(" "));
    }

    if let Some(secs) = estimate
        .get("estimated_seconds")
        .or_else(|| wf.get("estimated_seconds"))
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 1.0)
    {
        let total = secs.round() as u64;
        if total >= 60 {
            parts.push(format!("约{}分{}秒", total / 60, total % 60));
        } else {
            parts.push(format!("约{total}秒"));
        }
    }

    if parts.is_empty() {
        wf.get("workflow_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|id| format!("预览就绪 · {id}"))
    } else {
        Some(parts.join(" · "))
    }
}

/// 提取用于彩屏 PNG 底部 5×7 像素字模的精简 ASCII 标尺（如 `40X30MM S600 F1200`）。
fn format_workflow_ruler_ascii(val: &Value) -> Option<String> {
    let wf = val.get("workflow").unwrap_or(val);
    if !wf.is_object() {
        return None;
    }
    let input = wf.get("input").unwrap_or(wf);
    let snap = wf.get("confirmation_snapshot").unwrap_or(wf);
    let artifacts = wf.get("artifacts").unwrap_or(wf);
    let estimate = wf
        .get("estimate")
        .or_else(|| artifacts.get("estimate"))
        .unwrap_or(wf);
    let mut tokens: Vec<String> = Vec::new();
    let w_mm = estimate
        .get("width_mm")
        .or_else(|| artifacts.get("width_mm"))
        .or_else(|| input.get("width_mm"))
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v > 0.0);
    let h_mm = estimate
        .get("height_mm")
        .or_else(|| artifacts.get("height_mm"))
        .or_else(|| input.get("height_mm"))
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v > 0.0);
    if let (Some(w), Some(h)) = (w_mm, h_mm) {
        tokens.push(format!("{}X{}MM", w.round() as i64, h.round() as i64));
    }
    if let Some(p) = snap
        .get("power")
        .or_else(|| input.get("power"))
        .and_then(Value::as_i64)
    {
        tokens.push(format!("S{p}"));
    }
    if let Some(f) = snap
        .get("feed_rate")
        .or_else(|| input.get("feed_rate"))
        .and_then(Value::as_i64)
    {
        tokens.push(format!("F{f}"));
    }
    if let Some(ps) = snap
        .get("passes")
        .or_else(|| input.get("passes"))
        .and_then(Value::as_i64)
        .filter(|v| *v > 1)
    {
        tokens.push(format!("P{ps}"));
    }
    (!tokens.is_empty()).then(|| tokens.join(" "))
}

pub(super) fn extract_preview_bundle_for_host(
    host: &laser_host::HostApplication,
    tool_detail: Option<&str>,
) -> Result<(Option<Vec<u8>>, String, Option<String>), String> {
    use laser_runtime::RuntimeStore;
    let roots = host.api_service.preview_roots();
    let try_read = |rel_or_abs: &str| -> Option<Vec<u8>> {
        let resolved =
            laser_api::resolve_preview_file(std::path::Path::new(rel_or_abs), roots).ok()?;
        std::fs::read(resolved).ok().filter(|b| !b.is_empty())
    };
    if let Some(detail) = tool_detail
        && let Ok(parsed) = serde_json::from_str::<Value>(detail)
    {
        let bytes = preview_candidate_from_json(&parsed).and_then(try_read);
        let summary = format_workflow_oled_summary(&parsed);
        let ruler = format_workflow_ruler_ascii(&parsed);
        if bytes.is_some() || summary.is_some() {
            return Ok((
                bytes,
                summary.unwrap_or_else(|| "激光雕刻预览已就绪".into()),
                ruler,
            ));
        }
    }
    let recent = host
        .db
        .list_recent(laser_runtime::RecordKind::Workflow, 10)
        .map_err(|_| "查询最近加工预览失败".to_owned())?;
    for record in recent {
        let bytes = preview_candidate_from_json(&record.payload).and_then(try_read);
        let summary = format_workflow_oled_summary(&record.payload);
        let ruler = format_workflow_ruler_ascii(&record.payload);
        if bytes.is_some() || summary.is_some() {
            return Ok((
                bytes,
                summary.unwrap_or_else(|| "激光雕刻预览已就绪".into()),
                ruler,
            ));
        }
    }
    Err("当前还没有可推送的激光加工预览（请先在工作台、画板或语音生成一次预览）".into())
}
/// 对话中调用 MOSS 工具（NV4）：身份 `CallerContext::Voice`；运动与出光跟随严格模式开关，桌面控制类工具始终拒绝。
pub(super) struct DesktopToolHost {
    pub(super) host: Arc<laser_host::HostApplication>,
}
impl ToolHost for DesktopToolHost {
    fn list(&self) -> Value {
        let mut base = laser_host::voice_tools_list(&self.host.api_service);
        let extra = self.host.api_service.agent_tools_list();
        if let (Some(base_tools), Some(agent_tools)) =
            (base["tools"].as_array_mut(), extra["tools"].as_array())
        {
            base_tools.extend(agent_tools.iter().cloned());
        }
        base
    }

    fn call(&self, context: &ToolContext, name: &str, arguments: &Value) -> ToolHostResult {
        let result = if matches!(name, "agent_codex" | "agent_claude_code") {
            let caller = laser_contracts::CallerContext::Voice {
                device_id: context.device_id.clone(),
                session_id: context.session_id.clone(),
            };
            self.host
                .api_service
                .call_agent_tool(&caller, name, arguments)
        } else {
            laser_host::voice_tool_call(
                &self.host.api_service,
                &context.device_id,
                &context.session_id,
                name,
                arguments,
            )
        };
        // n70（2026-11-08 用户决定）：生成预览时不再自动推到小智屏幕，开始雕刻后才显示刀路（见 `workflow_preview`）
        ToolHostResult {
            success: result.success,
            result: result.result,
            detail: result.detail,
        }
    }

    /// 加工结束播报的只读轮询：走不计入调用统计的进程内只读通道。
    fn recent_workflows(&self, device_id: &str, session_id: &str) -> Option<Value> {
        laser_host::voice_recent_workflows(&self.host.api_service, device_id, session_id)
    }

    /// VA2 #2 只读查询激光侧是否存在运行中/未决的加工或走边框任务（不经工具分发；探测不可用返回 None 保守跳过急停）。
    fn has_active_laser_job(&self) -> Option<bool> {
        laser_host::voice_has_active_laser_job(&self.host.api_service)
    }

    /// n70 正在雕刻的任务的刀路预览原图：只读本机任务记录里的预览文件（限预览目录内），不走工具分发。
    fn workflow_preview(
        &self,
        device_id: &str,
        session_id: &str,
        workflow_id: &str,
    ) -> Option<Vec<u8>> {
        use laser_runtime::RuntimeStore;
        if !self
            .host
            .api_service
            .voice_session_allowed(device_id, session_id)
        {
            return None;
        }
        let record = self
            .host
            .db
            .get(laser_runtime::RecordKind::Workflow, workflow_id)
            .ok()
            .flatten()?;
        let rel = preview_candidate_from_json(&record.payload)?;
        let resolved = laser_api::resolve_preview_file(
            std::path::Path::new(rel),
            self.host.api_service.preview_roots(),
        )
        .ok()?;
        std::fs::read(resolved).ok().filter(|b| {
            !b.is_empty() && b.len() <= moss_voice_server::preview_push::MAX_SOURCE_IMAGE_BYTES
        })
    }
}
