//! 模拟轮次安全门禁（2026-11-27 VA2）：
//! - 模拟轮次（文字直通联调）不能代替真人做任何授权，所有具物理后果、外部控制、本机执行或刷写/重启风险的工具
//!   在工具执行入口一律拒绝，绝不进入 `ToolHost` / 桌面审批通道，并把拒绝原因如实返回给大模型。
//! - 仅放行预览生成、状态查询、设备只读信息以及音量/亮度/主题等设备自身无风险控制。

use serde_json::Value;

/// 判断 `recent_workflows` 响应中是否存在正在发送的激光加工任务。
pub(crate) fn has_active_job_in_recent_workflows(value: &Value) -> bool {
    if value["active_stop_targets"].as_u64().unwrap_or(0) > 0 {
        return true;
    }
    value["workflows"].as_array().is_some_and(|items| {
        items.iter().any(|w| {
            matches!(
                w["status"].as_str(),
                Some("sending" | "running" | "paused" | "pausing" | "stopping" | "uncertain")
            )
        })
    })
}

/// 标准化设备 MCP 工具名：把大模型看到的 `device__self_...` 或原始 `self....` 统一成带下划线的小写键，
/// 用于严格白名单判定（拒绝固件升级、资源包推送、重启、Wi-Fi 重配与未知控制动作）。
fn canonical_device_tool(name: &str) -> String {
    let raw = name.strip_prefix("device__").unwrap_or(name);
    raw.trim().replace('.', "_").to_ascii_lowercase()
}

fn arg_bool(args: &Value, key: &str) -> bool {
    args.get(key)
        .and_then(|v| {
            v.as_bool().or_else(|| {
                v.as_str()
                    .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
            })
        })
        .unwrap_or(false)
}

fn arg_non_empty_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// 校验模拟轮次中是否允许执行工具 `name`（若为设备端 MCP 工具，可传入其原始名 `device_original`）。
/// 返回 `Ok(())` 表示允许执行；返回 `Err(reason)` 表示拒绝，调用方必须直接把原因回填大模型且不得触发审批。
pub fn check_simulated_tool(
    name: &str,
    device_original: Option<&str>,
    arguments: &str,
    trusted_read_only: bool,
) -> Result<(), String> {
    let args: Value = if arguments.trim().is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(arguments).unwrap_or(Value::Null)
    };
    let action = arg_non_empty_str(&args, "action").map(str::to_ascii_lowercase);

    // 1. 设备端 MCP 工具：按白名单放行只读信息与音量/亮度/主题/预览，拒绝固件、唤醒词/资源包、重启与未知控制
    if device_original.is_some() || name.starts_with("device__") {
        let canonical = canonical_device_tool(device_original.unwrap_or(name));
        if canonical.contains("firmware")
            || canonical.contains("upgrade")
            || canonical.contains("reboot")
            || canonical.contains("assets")
            || canonical.contains("download_url")
            || canonical.contains("wake_word")
            || canonical.contains("reconfigure_wifi")
            || canonical.contains("factory_reset")
        {
            return Err(
                "模拟轮次禁止执行固件升级、资源包或唤醒词推送、设备重启及网络重配置".into(),
            );
        }
        let allowed = matches!(
            canonical.as_str(),
            "self_get_device_status"
                | "self_audio_speaker_set_volume"
                | "self_audio_speaker_get_volume"
                | "self_audio_speaker_mute"
                | "self_screen_set_brightness"
                | "self_screen_set_theme"
                | "self_screen_get_info"
                | "self_screen_preview_image"
                | "self_disp_setbacklight"
                | "self_camera_take_photo"
                | "self_battery_get_level"
        );
        return if allowed {
            Ok(())
        } else {
            Err(format!(
                "模拟轮次仅允许查询设备信息或调节音量、亮度、主题等无风险控制，已拒绝设备工具 {name}"
            ))
        };
    }

    // 2. 内置激光、家居、本机 Agent、信息与语音工具
    match name {
        "laser_workflow_tool" => match action.as_deref() {
            Some("status" | "recent" | "list" | "preview") => Ok(()),
            _ => Err(
                "模拟轮次禁止发送激光加工任务、续雕、走边框或取消任务（仅允许查询与生成预览）"
                    .into(),
            ),
        },
        "laser_safe_action_tool" => match action.as_deref() {
            Some("status") => Ok(()),
            _ => Err(
                "模拟轮次禁止执行激光机移动、回零、解锁、实时倍率、暂停、继续或急停等物理控制（仅允许查询状态）"
                    .into(),
            ),
        },
        "laser_grbl_tool" | "laser_network_grbl_tool" => match action.as_deref() {
            Some("status" | "check" | "probe") => Ok(()),
            _ => Err("模拟轮次禁止向激光机下发原始指令或发送文件".into()),
        },
        "generate_text_laser_task_tool" => {
            if arg_bool(&args, "confirmed") && arg_bool(&args, "send_after_generate") {
                Err("模拟轮次只允许生成雕刻预览，禁止在生成后直接发送任务".into())
            } else {
                Ok(())
            }
        }
        "regenerate_text_laser_task_tool" => {
            if arg_bool(&args, "confirmed") && arg_bool(&args, "send_after_generate") {
                Err("模拟轮次只允许重新生成雕刻预览，禁止发送任务".into())
            } else {
                Ok(())
            }
        }
        "run_calibration_grid_tool" => {
            if arg_non_empty_str(&args, "calibration_id").is_some() || arg_bool(&args, "confirmed")
            {
                Err("模拟轮次只允许生成标定矩阵预览，禁止向设备发送标定任务".into())
            } else {
                Ok(())
            }
        }
        "start_tuned_job_tool" => {
            if arg_bool(&args, "dry_run") && !arg_bool(&args, "confirmed") {
                Ok(())
            } else {
                Err("模拟轮次禁止启动或发送加工任务".into())
            }
        }
        "material_params_tool" => match action.as_deref() {
            None | Some("list" | "get" | "recommend" | "export") => Ok(()),
            _ => Err("模拟轮次禁止修改、导入或删除材料参数库".into()),
        },
        "check_laser_connection_tool"
        | "recommend_laser_params_tool"
        | "select_calibration_cell_tool"
        | "refine_laser_params_from_feedback_tool" => Ok(()),
        "hass_get_state" => Ok(()),
        "hass_set_state" | "hass_play_music" => Err("模拟轮次禁止执行智能家居控制动作".into()),
        n if n.starts_with("hass_") => Err("模拟轮次禁止执行智能家居控制动作".into()),
        "agent_codex" | "agent_claude_code" | "command_execution_tool" | "open_app_tool" => {
            Err("模拟轮次禁止执行本机 Agent 或系统命令".into())
        }
        "agent_run_list" | "agent_run_detail" => Ok(()),
        "get_weather" | "get_news" | "web_search" | "search_rag" | "play_music"
        | "end_conversation" | "stay_silent" => Ok(()),
        "set_persona" => Err("模拟轮次不允许修改说话角色设置".into()),
        // 3. 第三方 MCP 工具：仅放行用户在设置中明确标为可信只读的工具，其余一律拒绝（绝不进入桌面审批）
        n if n.contains("__") => {
            if trusted_read_only {
                Ok(())
            } else {
                Err(format!(
                    "模拟轮次禁止执行未标记为可信只读的第三方 MCP 工具 {n}"
                ))
            }
        }
        other => Err(format!("模拟轮次拒绝执行未知工具 {other} 以防意外副作用")),
    }
}

/// 构造返回给大模型的模拟门禁拒绝 JSON（明确告诉模型这是模拟联调拦截，而非设备故障）。
pub fn blocked_tool_reply(name: &str, reason: &str) -> String {
    serde_json::json!({
        "success": false,
        "error_code": "simulated_turn_blocked",
        "blocked_by_simulation": true,
        "tool": name,
        "result": reason,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulated_gate_blocks_physical_actions_and_allows_preview_and_safe_device_controls() {
        // 激光物理控制一律拒绝
        assert!(
            check_simulated_tool(
                "laser_workflow_tool",
                None,
                r#"{"action":"confirm_send","workflow_id":"wf-1","confirmed":true}"#,
                false
            )
            .is_err()
        );
        assert!(
            check_simulated_tool(
                "laser_workflow_tool",
                None,
                r#"{"action":"resume","workflow_id":"wf-1","confirmed":true}"#,
                false
            )
            .is_err()
        );
        assert!(
            check_simulated_tool(
                "laser_workflow_tool",
                None,
                r#"{"action":"frame","workflow_id":"wf-1"}"#,
                false
            )
            .is_err()
        );
        assert!(
            check_simulated_tool(
                "laser_safe_action_tool",
                None,
                r#"{"action":"absolute_move","x_mm":10,"y_mm":10,"confirmed":true}"#,
                false
            )
            .is_err()
        );
        assert!(
            check_simulated_tool(
                "laser_safe_action_tool",
                None,
                r#"{"action":"relative_move","dx_mm":5,"confirmed":true}"#,
                false
            )
            .is_err()
        );
        assert!(
            check_simulated_tool(
                "laser_safe_action_tool",
                None,
                r#"{"action":"home","confirmed":true}"#,
                false
            )
            .is_err()
        );
        assert!(
            check_simulated_tool(
                "laser_safe_action_tool",
                None,
                r#"{"action":"unlock","confirmed":true}"#,
                false
            )
            .is_err()
        );
        assert!(
            check_simulated_tool(
                "laser_safe_action_tool",
                None,
                r#"{"action":"feed_plus_10"}"#,
                false
            )
            .is_err()
        );
        assert!(
            check_simulated_tool(
                "laser_safe_action_tool",
                None,
                r#"{"action":"power_minus_10"}"#,
                false
            )
            .is_err()
        );
        assert!(
            check_simulated_tool(
                "start_tuned_job_tool",
                None,
                r#"{"task_id":"wf-1","confirmed":true}"#,
                false
            )
            .is_err()
        );

        // 家居控制、本机 Agent、固件/唤醒词推送一律拒绝
        assert!(
            check_simulated_tool(
                "hass_set_state",
                None,
                r#"{"entity_id":"light.desk","state":"on"}"#,
                false
            )
            .is_err()
        );
        assert!(check_simulated_tool("agent_codex", None, r#"{"prompt":"ls"}"#, false).is_err());
        assert!(
            check_simulated_tool("agent_claude_code", None, r#"{"prompt":"ls"}"#, false).is_err()
        );
        assert!(
            check_simulated_tool(
                "device__self_upgrade_firmware",
                Some("self.upgrade_firmware"),
                "{}",
                false
            )
            .is_err()
        );
        assert!(
            check_simulated_tool(
                "device__self_assets_set_download_url",
                Some("self.assets.set_download_url"),
                r#"{"url":"http://127.0.0.1/a.bin"}"#,
                false
            )
            .is_err()
        );
        assert!(
            check_simulated_tool("device__self_reboot", Some("self.reboot"), "{}", false).is_err()
        );

        // 预览、只读查询、设备自身音量/亮度控制放行
        assert!(
            check_simulated_tool(
                "laser_workflow_tool",
                None,
                r#"{"action":"preview","text":"新年快乐","material":"椴木","thickness_mm":3}"#,
                false
            )
            .is_ok()
        );
        assert!(
            check_simulated_tool(
                "generate_text_laser_task_tool",
                None,
                r#"{"text":"新年快乐","material":"椴木","thickness_mm":3}"#,
                false
            )
            .is_ok()
        );
        assert!(
            check_simulated_tool(
                "laser_safe_action_tool",
                None,
                r#"{"action":"status"}"#,
                false
            )
            .is_ok()
        );
        assert!(check_simulated_tool("check_laser_connection_tool", None, "{}", false).is_ok());
        assert!(
            check_simulated_tool(
                "device__self_audio_speaker_set_volume",
                Some("self.audio_speaker.set_volume"),
                r#"{"volume":50}"#,
                false
            )
            .is_ok()
        );
        assert!(
            check_simulated_tool(
                "device__self_screen_set_brightness",
                Some("self.screen.set_brightness"),
                r#"{"brightness":70}"#,
                false
            )
            .is_ok()
        );
        assert!(
            check_simulated_tool(
                "device__self_get_device_status",
                Some("self.get_device_status"),
                "{}",
                false
            )
            .is_ok()
        );
    }
}
