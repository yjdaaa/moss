//! 工具风险分级 R0–R4（技术书 §6.1；实施清单 P1-06）。
//!
//! 小智 MCP 桥接（接入点）在严格模式下据此处理每次工具调用；以后可选的网关（P2b）复用同一函数。
//! 分级按工具名 + 参数（action、confirmed 等）判定；不认识的工具返回 `None`，调用方按 R4 处理。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::safe_action_requires_approval;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub enum RiskLevel {
    /// 只读
    R0,
    /// 生成，不触设备
    R1,
    /// 降低风险（停止、急停、降低倍率），任何模式下都不追加条件
    R2,
    /// 运动或出光：严格模式下需要桌面授权
    R3,
    /// 永不对语音开放
    R4,
}

impl RiskLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::R0 => "R0",
            Self::R1 => "R1",
            Self::R2 => "R2",
            Self::R3 => "R3",
            Self::R4 => "R4",
        }
    }
}

fn text<'a>(arguments: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    arguments
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn flag(arguments: &serde_json::Value, key: &str) -> bool {
    match arguments.get(key) {
        Some(serde_json::Value::Bool(value)) => *value,
        Some(serde_json::Value::String(value)) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes"
        ),
        Some(serde_json::Value::Number(value)) => value.as_i64() == Some(1),
        _ => false,
    }
}

/// 无论参数如何都属于 R4 的工具（桌面命令、打开应用、快捷键、打开网站）。
/// 严格模式下接入点的 `tools/list` 不再列出它们，避免模型尝试调用。
pub fn tool_never_exposed(name: &str) -> bool {
    matches!(
        name,
        "command_execution_tool"
            | "open_app_tool"
            | "shortcut_key_execution_tool"
            | "open_jsjds_website_tool"
    )
}

/// 不能被用户关闭的工具（NV8 工具开关）：语音“停”要靠它们（技术书 §6.1：停止只映射 `cancel` / `emergency_stop`）。
pub const ALWAYS_ENABLED_TOOLS: [&str; 1] = ["laser_safe_action_tool"];

/// 工具能否被用户关闭：停止类工具不能。
pub fn tool_can_be_disabled(name: &str) -> bool {
    !ALWAYS_ENABLED_TOOLS.contains(&name)
}

/// 分级规则与 `laser_mcp::dispatch_tool_call_with` 的分支一一对应；
/// 工具清单（`tests/golden/mcp_tools.json`）新增工具而这里没有分级时，`laser-mcp` 的测试会失败。
pub fn tool_risk(name: &str, arguments: &serde_json::Value) -> Option<RiskLevel> {
    use crate::risk::RiskLevel::{R0, R1, R2, R3, R4};
    let action = text(arguments, "action").map(str::to_ascii_lowercase);
    let level = match name {
        "check_laser_connection_tool" | "recommend_laser_params_tool" => R0,
        "laser_safe_action_tool" => {
            let action = action.unwrap_or_default();
            if action == "status" {
                R0
            } else if safe_action_requires_approval(&action) {
                R3
            } else {
                R2
            }
        }
        "laser_workflow_tool" => match action.as_deref().unwrap_or("status") {
            "status" | "recent" | "list" => R0,
            "preview" => R1,
            "cancel" => R2,
            // confirm_send / send / resume 以及不认识的 action：按 R3 处理（不认识的由后端拒绝）
            _ => R3,
        },
        "laser_grbl_tool" | "laser_network_grbl_tool" => match action.as_deref().unwrap_or("") {
            "status" | "check" | "probe" => R0,
            "cancel" | "stop" => R2,
            // send_file / send_command 已被禁用：任意 G-code 永不对语音开放
            _ => R4,
        },
        "material_params_tool" => match action.as_deref().unwrap_or("list") {
            "list" | "get" | "recommend" | "export" => R0,
            // 保存、导入可能覆盖已有参数，删除不可逆：材料库删除 / 覆盖属于 R4
            _ => R4,
        },
        "generate_text_image_tool"
        | "select_calibration_cell_tool"
        | "refine_laser_params_from_feedback_tool" => R1,
        "generate_text_laser_task_tool"
        | "generate_text_image_gcode_tool"
        | "ai_laser_gcode_tool" => {
            if flag(arguments, "confirmed") && flag(arguments, "send_after_generate") {
                R3
            } else {
                R1
            }
        }
        "run_calibration_grid_tool" => {
            if text(arguments, "calibration_id").is_some() && flag(arguments, "confirmed") {
                R3
            } else {
                R1
            }
        }
        "start_tuned_job_tool" => {
            if flag(arguments, "confirmed") && !flag(arguments, "dry_run") {
                R3
            } else {
                R0
            }
        }
        // 采用反馈建议会改写后续加工参数（§6.1 列为 R3）；不采用时只是重新生成预览
        "regenerate_text_laser_task_tool" => {
            if text(arguments, "feedback_id").is_some() {
                R3
            } else {
                R1
            }
        }
        "command_execution_tool"
        | "open_app_tool"
        | "shortcut_key_execution_tool"
        | "open_jsjds_website_tool" => R4,
        _ => return None,
    };
    Some(level)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn grading_follows_spec_table() {
        use crate::risk::RiskLevel::{R0, R1, R2, R3, R4};
        let cases = [
            ("check_laser_connection_tool", json!({}), R0),
            ("laser_workflow_tool", json!({}), R0),
            ("laser_workflow_tool", json!({"action": "status"}), R0),
            (
                "laser_workflow_tool",
                json!({"action": "preview", "text": "你好"}),
                R1,
            ),
            ("laser_workflow_tool", json!({"action": "cancel"}), R2),
            (
                "laser_workflow_tool",
                json!({"action": "confirm_send", "confirmed": true}),
                R3,
            ),
            ("laser_workflow_tool", json!({"action": "resume"}), R3),
            ("laser_workflow_tool", json!({"action": "SEND"}), R3),
            ("laser_safe_action_tool", json!({"action": "status"}), R0),
            (
                "laser_safe_action_tool",
                json!({"action": "emergency_stop"}),
                R2,
            ),
            ("laser_safe_action_tool", json!({"action": "laser_off"}), R2),
            (
                "laser_safe_action_tool",
                json!({"action": "feed_override_minus_10"}),
                R2,
            ),
            ("laser_safe_action_tool", json!({"action": "home"}), R3),
            (
                "laser_safe_action_tool",
                json!({"action": "power_override_plus_10"}),
                R3,
            ),
            ("laser_safe_action_tool", json!({}), R3),
            ("laser_grbl_tool", json!({"action": "probe"}), R0),
            ("laser_network_grbl_tool", json!({"action": "stop"}), R2),
            ("laser_grbl_tool", json!({"action": "send_command"}), R4),
            (
                "laser_network_grbl_tool",
                json!({"action": "send_file"}),
                R4,
            ),
            ("material_params_tool", json!({}), R0),
            ("material_params_tool", json!({"action": "get"}), R0),
            ("material_params_tool", json!({"action": "save"}), R4),
            ("material_params_tool", json!({"action": "delete"}), R4),
            ("material_params_tool", json!({"action": "import"}), R4),
            ("recommend_laser_params_tool", json!({}), R0),
            ("generate_text_image_tool", json!({}), R1),
            ("select_calibration_cell_tool", json!({}), R1),
            ("refine_laser_params_from_feedback_tool", json!({}), R1),
            (
                "generate_text_laser_task_tool",
                json!({"confirmed": true}),
                R1,
            ),
            (
                "generate_text_laser_task_tool",
                json!({"confirmed": true, "send_after_generate": true}),
                R3,
            ),
            (
                "ai_laser_gcode_tool",
                json!({"confirmed": "true", "send_after_generate": "true"}),
                R3,
            ),
            ("run_calibration_grid_tool", json!({"material": "椴木"}), R1),
            (
                "run_calibration_grid_tool",
                json!({"calibration_id": "cal_1"}),
                R1,
            ),
            (
                "run_calibration_grid_tool",
                json!({"calibration_id": "cal_1", "confirmed": true}),
                R3,
            ),
            ("start_tuned_job_tool", json!({"task_id": "wf"}), R0),
            (
                "start_tuned_job_tool",
                json!({"task_id": "wf", "confirmed": true, "dry_run": true}),
                R0,
            ),
            (
                "start_tuned_job_tool",
                json!({"task_id": "wf", "confirmed": true}),
                R3,
            ),
            (
                "regenerate_text_laser_task_tool",
                json!({"task_id": "wf"}),
                R1,
            ),
            (
                "regenerate_text_laser_task_tool",
                json!({"task_id": "wf", "feedback_id": "fb"}),
                R3,
            ),
            ("command_execution_tool", json!({}), R4),
            ("open_app_tool", json!({}), R4),
            ("shortcut_key_execution_tool", json!({}), R4),
            ("open_jsjds_website_tool", json!({}), R4),
        ];
        for (name, arguments, expected) in cases {
            assert_eq!(
                tool_risk(name, &arguments),
                Some(expected),
                "{name} {arguments}"
            );
        }
        assert_eq!(tool_risk("unknown_tool", &json!({})), None);
        assert!(RiskLevel::R2 < RiskLevel::R3 && RiskLevel::R3 < RiskLevel::R4);
        assert_eq!(RiskLevel::R3.as_str(), "R3");
        for name in [
            "command_execution_tool",
            "open_app_tool",
            "shortcut_key_execution_tool",
            "open_jsjds_website_tool",
        ] {
            assert!(tool_never_exposed(name));
        }
        assert!(!tool_never_exposed("laser_grbl_tool"));
        assert!(!tool_never_exposed("material_params_tool"));
    }
}
