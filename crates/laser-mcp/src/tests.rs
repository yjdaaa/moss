use super::*;
use crate::bridge::{BridgeTiming, run_bridge_timed, scrub_token};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// `global_mcp_telemetry().endpoints` 是进程级全局列表，其中 `register_endpoints`
/// 会替换整张列表；涉及端点列表读写的测试串行执行，避免并行测试互相清空计数。
static ENDPOINT_TELEMETRY_TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn safe_tools_list_hides_developer_command() {
    let value = tools_list_json(false);
    let names = value["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 14);
    assert!(!names.contains(&"command_execution_tool"));
}

#[test]
fn developer_tools_list_has_eighteen_tools_matching_catalog() {
    let listed = tools_list_json(true)["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    let catalog = laser_tools::ToolCatalog::developer_mode()
        .names()
        .into_iter()
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(listed.len(), 15);
    assert_eq!(listed, catalog, "tools/list 与 ToolCatalog 必须一致");
}

/// 记录请求并按路由返回预设响应的假后端。
struct FakeBackend {
    calls: Mutex<Vec<(String, String, serde_json::Value)>>,
    responses: Mutex<Vec<(u16, serde_json::Value)>>,
}
impl FakeBackend {
    fn new(responses: Vec<(u16, serde_json::Value)>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into_iter().rev().collect()),
        }
    }
    fn calls(&self) -> Vec<(String, String, serde_json::Value)> {
        self.calls.lock().unwrap().clone()
    }
}
impl HostBackend for FakeBackend {
    fn request(
        &self,
        method: &str,
        route: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, serde_json::Value), String> {
        self.calls
            .lock()
            .unwrap()
            .push((method.into(), route.into(), body.clone()));
        Ok(self.responses.lock().unwrap().pop().unwrap_or((
            500,
            serde_json::json!({"success": false, "result": "no response"}),
        )))
    }
}

/// r44（2026-11-17）：只刻图形的预览也走文字任务生成；两种清单的 laser_workflow_tool 都列图形参数，
/// 重新生成可整组替换图形。
#[test]
fn shape_previews_route_to_text_generation_and_schemas_list_them() {
    let backend = FakeBackend::new(vec![(
        200,
        serde_json::json!({"success": true, "result": {}}),
    )]);
    dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action": "preview", "material": "椴木", "thickness_mm": 3,
            "width_mm": 40, "height_mm": 40, "shapes": [{"kind": "heart", "size_mm": 30}]}),
        false,
        Some(&backend),
    );
    let calls = backend.calls();
    assert_eq!(calls[0].1, "/api/text-task/generate");
    assert_eq!(calls[0].2["shapes"][0]["kind"], "heart", "原样交给后端");

    let backend = FakeBackend::new(vec![
        (
            200,
            serde_json::json!({"success": true, "result": {"input": {
            "text": "AB", "material": "椴木", "thickness_mm": 3,
            "shapes": [{"kind": "star", "size_mm": 10}]}}}),
        ),
        (200, serde_json::json!({"success": true, "result": {}})),
    ]);
    dispatch_tool_call_with(
        "regenerate_text_laser_task_tool",
        &serde_json::json!({"task_id": "wf-1", "shapes": [{"kind": "star", "size_mm": 14}],
            "border": {"shape": "rounded"}}),
        false,
        Some(&backend),
    );
    let calls = backend.calls();
    assert_eq!(calls[1].1, "/api/text-task/generate");
    assert_eq!(calls[1].2["shapes"][0]["size_mm"], 14);
    assert_eq!(calls[1].2["border"]["shape"], "rounded");
    assert_eq!(calls[1].2["text"], "AB");

    let props = |list: serde_json::Value, name: &str| {
        list["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap()["inputSchema"]["properties"]
            .clone()
    };
    for list in [tools_list_json(false), compact_tools_list_json(false)] {
        let workflow = props(list, "laser_workflow_tool");
        for key in ["shapes", "border", "text_box"] {
            assert!(workflow[key]["description"].is_string(), "{key}");
        }
        assert!(
            workflow["shapes"]["items"]["properties"]["kind"]["enum"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("heart"))
        );
    }
    assert!(props(tools_list_json(false), "regenerate_text_laser_task_tool")["shapes"].is_object());
    // r48：起点不写死默认中心，没说就按电脑上的对刀起点
    for list in [tools_list_json(false), compact_tools_list_json(false)] {
        let origin = &props(list, "laser_workflow_tool")["origin"];
        assert!(origin.get("default").is_none(), "{origin}");
        assert!(
            origin["description"]
                .as_str()
                .unwrap()
                .contains("没说就不填")
        );
    }
    assert!(
        props(
            compact_tools_list_json(false),
            "regenerate_text_laser_task_tool"
        )["shapes"]
            .is_null(),
        "精简清单的重新生成不列图形（字数预算），改图形就重新 preview"
    );
}

/// 严格模式开启的假后端（技术书 §6.2），记录收到的审计。
struct StrictBackend(FakeBackend, Mutex<Vec<laser_contracts::AuditEntry>>);
impl HostBackend for StrictBackend {
    fn request(
        &self,
        method: &str,
        route: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, serde_json::Value), String> {
        self.0.request(method, route, body)
    }
    fn strict_mode(&self) -> bool {
        true
    }
    fn audit(&self, entry: &laser_contracts::AuditEntry) -> Result<(), String> {
        self.1.lock().unwrap().push(entry.clone());
        Ok(())
    }
}

/// P1-06：工具清单里的每个工具都必须有分级；新增工具忘了分级，这个测试就失败。
#[test]
fn every_listed_tool_has_a_risk_level() {
    let tools = tools_list_json(true);
    let names: Vec<String> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names.len(), 15);
    for name in &names {
        assert!(
            laser_contracts::tool_risk(name, &serde_json::json!({})).is_some(),
            "工具 {name} 缺少 R0–R4 分级"
        );
    }
    for name in laser_tools::ToolCatalog::developer_mode().names() {
        assert!(
            laser_contracts::tool_risk(name, &serde_json::json!({})).is_some(),
            "工具目录中的 {name} 缺少分级"
        );
    }
}

/// A-28：严格模式下 R4 工具（与材料库删除 / 覆盖）返回 tool_not_allowed，不执行、不触达后端；
/// tools/list 不再列出永不开放的工具。
#[test]
fn strict_bridge_refuses_r4_tools() {
    let backend = StrictBackend(FakeBackend::new(vec![]), Mutex::new(Vec::new()));
    for (name, arguments) in [
        ("open_app_tool", serde_json::json!({"app_name": "notepad"})),
        (
            "command_execution_tool",
            serde_json::json!({"command": "dir"}),
        ),
        (
            "material_params_tool",
            serde_json::json!({"action": "delete", "material": "椴木"}),
        ),
        (
            "material_params_tool",
            serde_json::json!({"action": "save", "material": "椴木"}),
        ),
        (
            "laser_grbl_tool",
            serde_json::json!({"action": "send_command", "gcode_command": "G0 X10"}),
        ),
        ("totally_unknown_tool", serde_json::json!({})),
    ] {
        let result = dispatch_tool_call_with(name, &arguments, true, Some(&backend));
        assert!(!result.success, "{name}");
        assert!(
            result
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("tool_not_allowed"),
            "{name}: {result:?}"
        );
    }
    let adopt = dispatch_tool_call_with(
        "regenerate_text_laser_task_tool",
        &serde_json::json!({"task_id": "wf-1", "feedback_id": "fb-1", "confirmed": true}),
        false,
        Some(&backend),
    );
    assert!(!adopt.success);
    assert!(
        adopt
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("strict_mode_blocked")
    );
    assert!(backend.0.calls().is_empty(), "被拒绝的调用不得触达后端");
    // P1-08：每次拒绝都记审计（6 次 R4 + 1 次采用反馈）
    let audits = backend.1.lock().unwrap().clone();
    assert_eq!(audits.len(), 7);
    assert!(audits[..6].iter().all(
        |entry| entry.action == laser_contracts::AuditAction::R4Denied
            && entry.client == "mcp"
            && entry.result == laser_contracts::AuditResult::Denied
    ));
    assert_eq!(audits[0].detail.tool.as_deref(), Some("open_app_tool"));
    assert_eq!(
        audits[6].action,
        laser_contracts::AuditAction::StrictModeBlocked
    );

    let list = handle_json_rpc_with(
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        true,
        Some(&backend),
    )
    .unwrap();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 13, "{names:?}");
    assert!(!names.contains(&"open_app_tool") && !names.contains(&"command_execution_tool"));
    assert!(names.contains(&"laser_grbl_tool") && names.contains(&"material_params_tool"));
}

/// A-20 / A-25：严格模式下模型带 confirmed=true 调 confirm_send，桥接照常转发，
/// 执行层只创建审批；工具结果是“请在电脑上确认”，不是发送成功。R2 停止类照常转发。
#[test]
fn strict_bridge_forwards_r3_for_approval_and_r2_unchanged() {
    let backend = StrictBackend(
        FakeBackend::new(vec![
            (
                202,
                serde_json::json!({"success": true, "result": {
                    "status": "approval_required", "approval_id": "apr_test", "expires_at": 1.0,
                    "summary": "发送加工任务 wf-9", "speech": "已提交，请在电脑上的 MOSS 里确认后开始。"
                }}),
            ),
            (
                200,
                serde_json::json!({"success": true, "result": "急停 0x18 已发送"}),
            ),
        ]),
        Mutex::new(Vec::new()),
    );
    let send = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action": "confirm_send", "workflow_id": "wf-9", "confirmed": true, "network_host": "10.0.0.5"}),
        false,
        Some(&backend),
    );
    assert!(send.success, "{send:?}");
    assert!(send.result.contains("确认"), "{send:?}");
    assert!(
        send.detail
            .as_deref()
            .unwrap_or("")
            .contains("approval_required")
    );
    let stop = dispatch_tool_call_with(
        "laser_safe_action_tool",
        &serde_json::json!({"action": "emergency_stop", "confirmed": true}),
        false,
        Some(&backend),
    );
    assert!(stop.success, "{stop:?}");
    let calls = backend.0.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1, "/api/workflow");
    assert_eq!(calls[1].1, "/api/safe-action");
}

/// A-40：严格模式关闭时分级不改变任何行为（材料库保存照常转发）。
#[test]
fn non_strict_bridge_behavior_is_unchanged() {
    let backend = FakeBackend::new(vec![(
        200,
        serde_json::json!({"success": true, "result": {"materials": {}}}),
    )]);
    let saved = dispatch_tool_call_with(
        "material_params_tool",
        &serde_json::json!({"action": "save", "material": "椴木", "thickness_mm": 3, "laser_max_power": 500, "feed_rate": 1200}),
        false,
        Some(&backend),
    );
    assert!(saved.success, "{saved:?}");
    assert_eq!(backend.calls()[0].1, "/api/material-lab/manage");
    let list = handle_json_rpc_with(
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        false,
        Some(&backend),
    )
    .unwrap();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 14);
}

#[test]
fn json_rpc_handles_initialize_tools_list_notifications_and_errors() {
    let init = handle_json_rpc(
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        false,
    )
    .unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], SERVER_NAME);
    assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(
        global_mcp_telemetry().snapshot().protocol_version,
        PROTOCOL_VERSION,
        "遥测记录实际应答的协议版本"
    );
    let list = handle_json_rpc(
        &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        false,
    )
    .unwrap();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 14);
    assert!(
        handle_json_rpc(
            &serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            false
        )
        .is_none()
    );
    let ping = handle_json_rpc(
        &serde_json::json!({"jsonrpc":"2.0","id":3,"method":"ping"}),
        false,
    )
    .unwrap();
    assert_eq!(ping["result"], serde_json::json!({}));
    let unknown = handle_json_rpc(
        &serde_json::json!({"jsonrpc":"2.0","id":4,"method":"nope"}),
        false,
    )
    .unwrap();
    assert_eq!(unknown["error"]["code"], -32601);

    let call = handle_json_rpc(&serde_json::json!({
        "jsonrpc":"2.0","id":5,"method":"tools/call",
        "params":{"name":"recommend_laser_params_tool","arguments":{"material":"椴木","thickness_mm":3.0}}
    }), false).unwrap();
    assert_eq!(call["result"]["isError"], false);
    assert!(
        call["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("推荐参数")
    );
}

#[test]
fn unimplemented_and_backendless_tools_report_errors_instead_of_fake_success() {
    for name in [
        "laser_workflow_tool",
        "run_calibration_grid_tool",
        "material_params_tool",
        "start_tuned_job_tool",
        "generate_text_laser_task_tool",
        "totally_unknown_tool",
    ] {
        let args = serde_json::json!({"action":"status","material":"椴木","thickness_mm":3,"text":"hi","task_id":"wf-1","confirmed":true,"workflow_id":"wf-1"});
        let result = dispatch_tool_call(name, &args, false);
        assert!(!result.success, "{name} 无后端时必须失败: {result:?}");
    }
    let rpc = handle_json_rpc(&serde_json::json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"laser_grbl_tool","arguments":{"action":"send_command","gcode_command":"G0 X10"}}}), false).unwrap();
    assert_eq!(rpc["result"]["isError"], true);
}

/// n72 精简清单：只含 8 个激光工具与电脑控制类工具；每个工具都在完整清单里且有风险分级，
/// 每个参数都有中文说明，没有 Python 旧版残留；内置语音看到的说明总量大幅缩小。
#[test]
fn compact_tool_list_is_small_described_and_consistent() {
    let full = tools_list_json(true);
    let full_names: Vec<&str> = full["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    let compact = compact_tools_list_json(false);
    let tools = compact["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for name in COMPACT_LASER_TOOLS {
        assert!(names.contains(&name), "精简清单缺 {name}");
    }
    for name in &names {
        assert!(
            full_names.contains(name),
            "{name} 必须是已有工具（执行逻辑不变）"
        );
        assert!(
            laser_contracts::tool_risk(name, &serde_json::json!({})).is_some(),
            "{name} 要有风险分级"
        );
    }
    for hidden in [
        "laser_grbl_tool",
        "laser_network_grbl_tool",
        "start_tuned_job_tool",
        "generate_text_laser_task_tool",
        "recommend_laser_params_tool",
        "command_execution_tool",
    ] {
        assert!(!names.contains(&hidden), "{hidden} 只在完整清单里");
    }
    for tool in tools
        .iter()
        .filter(|t| COMPACT_LASER_TOOLS.contains(&t["name"].as_str().unwrap()))
    {
        let text = tool.to_string();
        for leftover in ["core.", ":param", "LASER_NETWORK_HOST", "LASER_DEFAULT"] {
            assert!(
                !text.contains(leftover),
                "{} 还有旧说明 {leftover}",
                tool["name"]
            );
        }
        for (key, prop) in tool["inputSchema"]["properties"].as_object().unwrap() {
            assert!(
                prop["description"]
                    .as_str()
                    .is_some_and(|d| !d.trim().is_empty()),
                "{} 的参数 {key} 没有说明",
                tool["name"]
            );
        }
    }
    // 急停在执行层要求 confirmed=true（laser_tools::execute_laser_safe_action）：说明必须让模型带上，不能写成“不用确认”
    let safe = tools
        .iter()
        .find(|t| t["name"] == "laser_safe_action_tool")
        .unwrap();
    assert!(
        safe["description"]
            .as_str()
            .unwrap()
            .contains("emergency_stop 并带 confirmed=true")
    );
    let voice = tools_list_profile(false, true, false).to_string();
    let voice_full = tools_list_profile(false, true, true).to_string();
    assert!(
        voice.chars().count() < 12_000,
        "内置语音每轮发给模型的工具说明 {} 字符，过大",
        voice.chars().count()
    );
    assert!(voice.chars().count() * 3 < voice_full.chars().count());
}

struct RememberedBackend {
    inner: FakeBackend,
}
impl HostBackend for RememberedBackend {
    fn request(
        &self,
        method: &str,
        route: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, serde_json::Value), String> {
        self.inner.request(method, route, body)
    }
    fn default_connection(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        serde_json::json!({"connection_mode": "serial", "port": "COM7", "baudrate": 115200})
            .as_object()
            .cloned()
    }
}

/// n73：没给任何连接参数时补上工作台握手成功的连接；给了任何一个就完全按调用方的来；不连机器的工具不补。
#[test]
fn remembered_connection_fills_only_when_caller_gave_none() {
    let ok = || (200, serde_json::json!({"success": true, "result": {}}));
    let backend = RememberedBackend {
        inner: FakeBackend::new(vec![ok()]),
    };
    let _ = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action":"confirm_send","workflow_id":"wf-1","confirmed":true}),
        false,
        Some(&backend),
    );
    let sent = &backend.inner.calls()[0].2;
    assert_eq!(sent["port"], "COM7");
    assert_eq!(sent["connection_mode"], "serial");

    let backend = RememberedBackend {
        inner: FakeBackend::new(vec![ok()]),
    };
    let _ = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action":"confirm_send","workflow_id":"wf-1","confirmed":true,"network_host":"10.0.0.9"}),
        false,
        Some(&backend),
    );
    let sent = &backend.inner.calls()[0].2;
    assert_eq!(sent["network_host"], "10.0.0.9");
    assert!(
        sent.get("port").is_none(),
        "调用方给了 IP 就不混入记住的串口"
    );

    let backend = RememberedBackend {
        inner: FakeBackend::new(vec![ok()]),
    };
    let _ = dispatch_tool_call_with(
        "material_params_tool",
        &serde_json::json!({"action":"get","material":"椴木","thickness_mm":3}),
        false,
        Some(&backend),
    );
    assert!(backend.inner.calls()[0].2.get("port").is_none());
}

/// n73：预览结果把这次实际的功率 / 速度 / 遍数告诉模型，并注明是手动指定还是材料库推荐。
#[test]
fn preview_result_tells_the_model_the_actual_params() {
    let record = |manual: bool| {
        let mut input = serde_json::json!({"text": "AB"});
        if manual {
            input["manual_params"] = serde_json::json!({"power_percent": 37});
        }
        (
            200,
            serde_json::json!({"success": true, "result": {
                "workflow_id": "wf-1", "status": "preview_ready", "input": input,
                "artifacts": {"actual_params": {"laser_max_power": 370.0, "feed_rate": 900.0, "passes": 2, "power_scale": 1000.0}}
            }}),
        )
    };
    let backend = FakeBackend::new(vec![record(true)]);
    let result = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action":"preview","text":"AB","material":"椴木","thickness_mm":3,"manual_params":{"power_percent":37}}),
        false,
        Some(&backend),
    );
    assert!(
        result
            .result
            .contains("功率 37%，速度 900 mm/min，2 遍（这次手动指定）"),
        "{}",
        result.result
    );
    assert_eq!(
        backend.calls()[0].2["manual_params"]["power_percent"],
        37,
        "原样交给后端"
    );
    let backend = FakeBackend::new(vec![record(false)]);
    let result = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action":"preview","text":"AB","material":"椴木","thickness_mm":3}),
        false,
        Some(&backend),
    );
    assert!(
        result.result.contains("（材料库推荐）"),
        "{}",
        result.result
    );
}

/// n72：没给串口号或 IP 时不擅自补成网络模式，由后端按 `.env` 默认连接方式决定。
#[test]
fn missing_connection_does_not_force_network_mode() {
    let backend = FakeBackend::new(vec![(
        200,
        serde_json::json!({"success": true, "result": {}}),
    )]);
    let _ = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action":"confirm_send","workflow_id":"wf-1","confirmed":true}),
        false,
        Some(&backend),
    );
    assert!(backend.calls()[0].2.get("connection_mode").is_none());
    let backend = FakeBackend::new(vec![(
        200,
        serde_json::json!({"success": true, "result": {}}),
    )]);
    let _ = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action":"confirm_send","workflow_id":"wf-1","confirmed":true,"port":"COM3"}),
        false,
        Some(&backend),
    );
    assert_eq!(backend.calls()[0].2["connection_mode"], "serial");
}

#[test]
fn image_and_material_test_options_are_listed_and_forwarded() {
    // 2026-11-16（技术书 r41）：图片转换方式只在完整清单（图片要已上传的文件）；材料测试字段两种清单都有
    let full = tools_list_json(false);
    let find = |list: &serde_json::Value, name: &str| {
        list["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .cloned()
            .unwrap()
    };
    let workflow = find(&full, "laser_workflow_tool");
    let modes: Vec<&str> = workflow["inputSchema"]["properties"]["image_mode"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(modes, laser_core::image_job::TONE_MODES);
    assert_eq!(
        workflow["inputSchema"]["properties"]["line_interval_scale"]["maximum"],
        3
    );
    let compact = compact_tools_list_json(false);
    let compact_workflow = find(&compact, "laser_workflow_tool");
    assert!(
        compact_workflow["inputSchema"]["properties"]
            .get("image_mode")
            .is_none(),
        "精简清单不加图片参数（语音无法引用图片）"
    );
    assert!(
        compact_workflow["description"]
            .as_str()
            .unwrap()
            .contains("工作台上传")
    );
    for list in [&full, &compact] {
        let grid = find(list, "run_calibration_grid_tool");
        let props = &grid["inputSchema"]["properties"];
        assert_eq!(
            props["test_type"]["enum"],
            serde_json::json!(["matrix", "interval"])
        );
        for axis in ["x_axis", "y_axis"] {
            for value in props[axis]["enum"].as_array().unwrap() {
                assert!(
                    laser_core::material::TestAxis::parse(value.as_str().unwrap()).is_some(),
                    "{axis}={value}"
                );
            }
        }
        for key in [
            "interval_min_mm",
            "interval_max_mm",
            "passes_min",
            "passes_max",
            "labels",
        ] {
            assert!(props[key]["description"].is_string(), "缺 {key}");
        }
    }
    assert!(
        find(&compact, "run_calibration_grid_tool")["description"]
            .as_str()
            .unwrap()
            .contains("test_type=interval")
    );

    // 分发层原样转发这些字段
    let backend = FakeBackend::new(vec![
        (
            200,
            serde_json::json!({"success": true, "result": {"calibration_id": "cal-2", "status": "prepared"}}),
        ),
        (
            200,
            serde_json::json!({"success": true, "result": {"workflow_id": "wf-img", "status": "preview_ready"}}),
        ),
    ]);
    let grid = dispatch_tool_call_with(
        "run_calibration_grid_tool",
        &serde_json::json!({
            "material": "椴木", "thickness_mm": 3, "test_type": "interval",
            "interval_min_mm": 0.06, "interval_max_mm": 0.2, "columns": 8, "labels": false
        }),
        false,
        Some(&backend),
    );
    assert!(grid.success, "{grid:?}");
    let image = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({
            "action": "preview", "source_type": "image", "image_file": "photo.png",
            "material": "椴木", "thickness_mm": 3, "image_mode": "jarvis", "line_interval_scale": 0.8
        }),
        false,
        Some(&backend),
    );
    assert!(image.success, "{image:?}");
    let calls = backend.calls();
    assert_eq!(calls[0].1, "/api/material-lab/preview");
    assert_eq!(calls[0].2["test_type"], "interval");
    assert_eq!(calls[0].2["interval_max_mm"], 0.2);
    assert_eq!(calls[0].2["labels"], false);
    assert_eq!(calls[1].1, "/api/workflow");
    assert_eq!(calls[1].2["image_mode"], "jarvis");
    assert_eq!(calls[1].2["line_interval_scale"], 0.8);
}

/// 按预览走边框（2026-11-18）：未确认或缺编号不触达后端；确认后带连接字段转发 /api/workflow。
#[test]
fn workflow_frame_requires_confirmation_and_forwards_with_connection() {
    for args in [
        serde_json::json!({"action": "frame", "workflow_id": "wf-9"}),
        serde_json::json!({"action": "frame", "confirmed": true}),
    ] {
        let backend = FakeBackend::new(vec![]);
        let refused = dispatch_tool_call_with("laser_workflow_tool", &args, false, Some(&backend));
        assert!(!refused.success, "{args}");
        assert!(backend.calls().is_empty(), "{args}");
    }
    let backend = FakeBackend::new(vec![(
        200,
        serde_json::json!({"success": true, "result": {"message": "走边框完成", "workflow_id": "wf-9", "laser_on": false}}),
    )]);
    let framed = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action": "frame", "workflow_id": "wf-9", "confirmed": true, "network_host": "10.0.0.5"}),
        false,
        Some(&backend),
    );
    assert!(framed.success, "{framed:?}");
    let calls = backend.calls();
    assert_eq!(calls[0].1, "/api/workflow");
    assert_eq!(calls[0].2["action"], "frame");
    assert_eq!(calls[0].2["connection_mode"], "network");
    // 无 host 后端：不伪造结果
    let offline = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action": "frame", "workflow_id": "wf-9", "confirmed": true}),
        false,
        None,
    );
    assert!(
        !offline.success && offline.result.contains("moss-host"),
        "{offline:?}"
    );
}

#[test]
fn workflow_preview_discards_old_id_on_both_backend_routes() {
    for (input, route) in [
        (
            serde_json::json!({"text": "测试"}),
            "/api/text-task/generate",
        ),
        (
            serde_json::json!({"source_type": "image", "image_file": "demo.png"}),
            "/api/workflow",
        ),
    ] {
        let backend = FakeBackend::new(vec![(
            200,
            serde_json::json!({
                "success": true, "result": {"workflow_id": "wf-new"}
            }),
        )]);
        let mut arguments = input.as_object().unwrap().clone();
        arguments.insert("action".into(), "preview".into());
        arguments.insert("workflow_id".into(), "wf-old".into());
        let result = dispatch_tool_call_with(
            "laser_workflow_tool",
            &arguments.into(),
            false,
            Some(&backend),
        );
        assert!(result.success, "{result:?}");
        let calls = backend.calls();
        assert_eq!(calls[0].1, route);
        assert!(calls[0].2.get("workflow_id").is_none(), "{:?}", calls[0].2);
    }
}

#[test]
fn workflow_and_calibration_tools_forward_to_host_backend() {
    // confirm_send → POST /api/workflow，携带连接字段
    let backend = FakeBackend::new(vec![(
        200,
        serde_json::json!({"success": true, "result": {"workflow_id": "wf-9", "status": "sending", "send_result": {"lines_sent": 0, "total_lines": 120}}}),
    )]);
    let result = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action":"confirm_send","workflow_id":"wf-9","confirmed":true,"network_host":"10.0.0.5","network_telnet_port":23}),
        false,
        Some(&backend),
    );
    assert!(result.success, "{result:?}");
    assert!(result.result.contains("wf-9") && result.result.contains("sending"));
    let calls = backend.calls();
    assert_eq!(calls[0].1, "/api/workflow");
    assert_eq!(calls[0].2["network_host"], "10.0.0.5");
    assert_eq!(calls[0].2["connection_mode"], "network");

    // 未确认 confirm_send 不触达后端
    let backend = FakeBackend::new(vec![]);
    let refused = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action":"confirm_send","workflow_id":"wf-9"}),
        false,
        Some(&backend),
    );
    assert!(!refused.success);
    assert!(backend.calls().is_empty());

    // 标定：无 calibration_id → preview；有 id + confirmed → send
    let backend = FakeBackend::new(vec![
        (
            200,
            serde_json::json!({"success": true, "result": {"calibration_id": "cal-1", "status": "prepared", "cell_count": 25}}),
        ),
        (
            200,
            serde_json::json!({"success": true, "result": {"calibration_id": "cal-1", "status": "sending", "send_result": {"lines_sent": 0, "total_lines": 900}}}),
        ),
    ]);
    let preview = dispatch_tool_call_with(
        "run_calibration_grid_tool",
        &serde_json::json!({"material":"椴木","thickness_mm":3,"rows":5,"columns":5}),
        false,
        Some(&backend),
    );
    assert!(
        preview.success && preview.result.contains("cal-1"),
        "{preview:?}"
    );
    let send = dispatch_tool_call_with(
        "run_calibration_grid_tool",
        &serde_json::json!({"calibration_id":"cal-1","confirmed":true,"network_host":"10.0.0.5"}),
        false,
        Some(&backend),
    );
    assert!(send.success && send.result.contains("900"), "{send:?}");
    let calls = backend.calls();
    assert_eq!(calls[0].1, "/api/material-lab/preview");
    assert_eq!(calls[1].1, "/api/material-lab/send");
    assert_eq!(calls[1].2["confirmed"], true);

    // 后端返回失败 → 工具失败并透传文案
    let backend = FakeBackend::new(vec![(
        400,
        serde_json::json!({"success": false, "result": "发送安全门禁未通过: SHA-256 不一致"}),
    )]);
    let gated = dispatch_tool_call_with(
        "select_calibration_cell_tool",
        &serde_json::json!({"calibration_id":"cal-1","cell_number":6}),
        false,
        Some(&backend),
    );
    assert!(!gated.success);
    assert!(gated.result.contains("SHA-256"));

    // material_params get → GET recommendation；save → manage
    let backend = FakeBackend::new(vec![
        (
            200,
            serde_json::json!({"success": true, "result": {"material": "椴木", "laser_max_power": 420}}),
        ),
        (
            200,
            serde_json::json!({"success": true, "result": {"materials": {}}}),
        ),
    ]);
    let got = dispatch_tool_call_with(
        "material_params_tool",
        &serde_json::json!({"action":"get","material":"椴木","thickness_mm":3}),
        false,
        Some(&backend),
    );
    assert!(got.success);
    let saved = dispatch_tool_call_with(
        "material_params_tool",
        &serde_json::json!({"action":"save","material":"椴木","thickness_mm":3,"laser_max_power":500,"feed_rate":1200}),
        false,
        Some(&backend),
    );
    assert!(saved.success);
    let calls = backend.calls();
    assert_eq!(calls[0].0, "GET");
    assert!(
        calls[0]
            .1
            .starts_with("/api/ui/material-recommendation?material=%E6%A4%B4%E6%9C%A8")
    );
    assert_eq!(calls[1].1, "/api/material-lab/manage");
    assert_eq!(calls[1].2["action"], "save");
}

#[test]
fn feedback_uses_shared_backend_without_silent_local_defaults() {
    let args = serde_json::json!({"task_id":"wf-1","feedback_text":"有点太焦了"});
    assert!(!dispatch_tool_call("refine_laser_params_from_feedback_tool", &args, false).success);
    let backend = FakeBackend::new(vec![(
        400,
        serde_json::json!({"success":false,"result":"原任务参数缺失"}),
    )]);
    assert!(
        !dispatch_tool_call_with(
            "refine_laser_params_from_feedback_tool",
            &args,
            false,
            Some(&backend)
        )
        .success
    );
    assert_eq!(backend.calls()[0].1, "/api/feedback");
    assert_eq!(backend.calls()[0].2, args);
    let tools = tools_list_json(false);
    let refine = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "refine_laser_params_from_feedback_tool")
        .unwrap();
    assert!(
        refine["inputSchema"]["properties"]["laser_max_power"]
            .get("default")
            .is_none()
    );
    assert_eq!(
        refine["inputSchema"]["required"],
        serde_json::json!(["task_id", "feedback_text"])
    );
}

#[test]
fn feedback_adoption_only_requests_new_preview() {
    let backend = FakeBackend::new(vec![(
        200,
        serde_json::json!({"success":true,"result":{"adopted_workflow_id":"wf-2"}}),
    )]);
    let result = dispatch_tool_call_with(
        "regenerate_text_laser_task_tool",
        &serde_json::json!({
            "task_id":"wf-1","feedback_id":"fb-1","confirmed":true
        }),
        false,
        Some(&backend),
    );
    assert!(result.success);
    assert_eq!(backend.calls().len(), 1);
    assert_eq!(backend.calls()[0].1, "/api/feedback/accept");
    assert_eq!(backend.calls()[0].2["workflow_id"], "wf-1");
}

#[test]
fn endpoint_placeholder_is_treated_as_unconfigured() {
    assert_eq!(
        redact_endpoint("wss://api.example/mcp/?token=abc"),
        "wss://api.example/mcp/?token=***"
    );
    // 直接测试判定逻辑（不依赖进程环境变量）
    for bad in [
        "",
        "wss://api.xiaozhi.me/mcp/?token=<your-token>",
        "http://x",
    ] {
        let value = bad.trim();
        let invalid = value.is_empty()
            || value.contains('<')
            || !(value.starts_with("ws://") || value.starts_with("wss://"));
        assert!(invalid, "{bad}");
    }
}

/// 本地 WebSocket 服务端：接收 initialize / tools/call，校验桥接真实往返，然后关闭。
#[test]
fn bridge_round_trips_json_rpc_over_loopback_websocket() {
    let _telemetry_lock = ENDPOINT_TELEMETRY_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        ws.send(tungstenite::Message::Text(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#.into(),
        ))
        .unwrap();
        let init: serde_json::Value =
            serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
        ws.send(tungstenite::Message::Text(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.into(),
        ))
        .unwrap();
        ws.send(tungstenite::Message::Text(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"laser_workflow_tool","arguments":{"action":"status","workflow_id":"wf-1"}}}"#.into())).unwrap();
        let call: serde_json::Value =
            serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
        // 桥连接期间遥测必须显示已连接（总状态与本接入点）
        let snapshot = global_mcp_telemetry().snapshot();
        let link = snapshot
            .endpoints
            .iter()
            .find(|link| link.id == "test-roundtrip")
            .cloned();
        let connected_while_open =
            snapshot.connected && link.as_ref().is_some_and(|link| link.connected);
        let _ = ws.close(None);
        (init, call, connected_while_open)
    });
    let backend: SharedBackend = Arc::new(FakeBackend::new(vec![(
        200,
        serde_json::json!({"success": true, "result": {"workflow_id": "wf-1", "status": "completed"}}),
    )]));
    let shutdown = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(BridgeStats::default());
    let endpoint = format!("ws://127.0.0.1:{port}/mcp/?token=test");
    let shutdown_for_bridge = shutdown.clone();
    let stats_for_bridge = stats.clone();
    let bridge = std::thread::spawn(move || {
        run_bridge(
            "test-roundtrip",
            &endpoint,
            Some(backend),
            false,
            shutdown_for_bridge,
            stats_for_bridge,
        )
    });
    let (init, call, connected_while_open) = server.join().unwrap();
    shutdown.store(true, Ordering::SeqCst);
    bridge.join().unwrap();
    assert!(
        connected_while_open,
        "连接期间 telemetry.connected 必须为真"
    );
    // 其他测试的桥可能同时在线，这里只看本接入点
    let after = global_mcp_telemetry().snapshot();
    let link = after
        .endpoints
        .iter()
        .find(|link| link.id == "test-roundtrip")
        .unwrap();
    assert!(!link.connected, "桥退出后本接入点必须为断开");
    assert_eq!(
        link.endpoint,
        format!("ws://127.0.0.1:{port}/mcp/?token=***"),
        "遥测里的地址必须脱敏"
    );
    assert_eq!(init["result"]["serverInfo"]["name"], SERVER_NAME);
    assert_eq!(call["id"], 2);
    assert_eq!(call["result"]["isError"], false);
    assert!(
        call["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("completed")
    );
    assert_eq!(stats.connections.load(Ordering::Relaxed), 1);
    assert_eq!(stats.messages_in.load(Ordering::Relaxed), 3);
    assert_eq!(stats.messages_out.load(Ordering::Relaxed), 2);
}

/// 关闭了部分工具的假后端（NV8）。使用独立遥测实例，避免并行测试互相污染全局工具计数。
struct SwitchBackend {
    inner: FakeBackend,
    disabled: Vec<&'static str>,
    telemetry: laser_contracts::McpTelemetry,
}
impl HostBackend for SwitchBackend {
    fn request(
        &self,
        method: &str,
        route: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, serde_json::Value), String> {
        self.inner.request(method, route, body)
    }
    fn tool_disabled(&self, name: &str) -> bool {
        self.disabled.contains(&name)
    }
    fn audit_client(&self) -> String {
        "mcp-switch-test".into()
    }
    fn mcp_telemetry(&self) -> &laser_contracts::McpTelemetry {
        &self.telemetry
    }
}

/// NV8：关闭的工具不出现在 tools/list，调用被拒绝且不触达后端；停止类工具即使后端报告关闭也照常可用；
/// 每次调用按结果计入统计。
#[test]
fn disabled_tools_are_neither_listed_nor_callable() {
    let backend = SwitchBackend {
        inner: FakeBackend::new(vec![
            (
                200,
                serde_json::json!({"success": true, "result": "急停 0x18 已发送"}),
            ),
            (
                200,
                serde_json::json!({"success": true, "result": {"materials": {}}}),
            ),
        ]),
        disabled: vec!["material_params_tool", "laser_safe_action_tool"],
        telemetry: laser_contracts::McpTelemetry::new(),
    };
    let list = handle_json_rpc_with(
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        false,
        Some(&backend),
    )
    .unwrap();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 12, "{names:?}");
    assert!(!names.contains(&"material_params_tool") && !names.contains(&"laser_safe_action_tool"));

    let call = handle_json_rpc_with(
        &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"material_params_tool","arguments":{"action":"list"}}}),
        false,
        Some(&backend),
    )
    .unwrap();
    assert_eq!(call["result"]["isError"], true);
    assert!(
        call["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("tool_disabled")
    );
    assert!(backend.inner.calls().is_empty(), "关闭的工具不得触达后端");
    let after = backend.telemetry.snapshot().tool_stats["material_params_tool"].clone();
    assert_eq!(after.calls, 1);
    assert_eq!(after.denied, 1);
    assert_eq!(after.failures, 0, "被开关拒绝不算失败");

    // 停止类工具不能关闭：即使后端报告关闭，调用照常转发
    assert!(!tool_can_be_disabled("laser_safe_action_tool"));
    let stop = dispatch_tool_call_with(
        "laser_safe_action_tool",
        &serde_json::json!({"action": "emergency_stop"}),
        false,
        Some(&backend),
    );
    assert!(stop.success, "{stop:?}");
    assert_eq!(backend.inner.calls()[0].1, "/api/safe-action");
    let stat = backend.telemetry.snapshot().tool_stats["laser_safe_action_tool"].clone();
    assert_eq!(stat.calls, 1);

    // 未知工具名统一计入“未知工具”，不按名字新增条目
    let made_up = dispatch_tool_call_with(
        "made_up_tool_for_stats",
        &serde_json::json!({}),
        false,
        Some(&backend),
    );
    assert!(!made_up.success);
    let stats = backend.telemetry.snapshot().tool_stats;
    assert!(!stats.contains_key("made_up_tool_for_stats"));
    let unknown = &stats["（未知工具）"];
    assert_eq!(unknown.calls, 1, "未知工具计一次调用");
    assert_eq!(unknown.failures, 1, "未知工具计一次失败");
}

/// NV8：桥定时发 Ping；对方长时间没有任何数据（连 Pong 都没有）时判定断线并自动重连，
/// 断线原因记入该接入点的状态。
#[test]
fn bridge_sends_heartbeats_and_reconnects_when_the_endpoint_goes_silent() {
    let _telemetry_lock = ENDPOINT_TELEMETRY_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut first = tungstenite::accept(stream).unwrap();
        let heartbeat = first.read().unwrap();
        // 收到 Ping 后不再读写：Pong 不会发出，桥应在空闲上限后断开并重连
        let (stream, _) = listener.accept().unwrap();
        let second = tungstenite::accept(stream).unwrap();
        (heartbeat.is_ping(), first, second)
    });
    let shutdown = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(BridgeStats::default());
    let timing = BridgeTiming {
        heartbeat: Duration::from_millis(200),
        idle_limit: Duration::from_secs(1),
        stable: Duration::from_secs(60),
        first_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_secs(1),
        poll: Duration::from_millis(100),
    };
    let endpoint = format!("ws://127.0.0.1:{port}/mcp/?token=heartbeat-secret");
    let (shutdown_for_bridge, stats_for_bridge) = (shutdown.clone(), stats.clone());
    let bridge = std::thread::spawn(move || {
        run_bridge_timed(
            "test-heartbeat",
            &endpoint,
            None,
            false,
            &shutdown_for_bridge,
            &stats_for_bridge,
            timing,
        )
    });
    let (got_ping, _first, _second) = server.join().unwrap();
    assert!(got_ping, "连接后应在心跳间隔内收到 Ping");
    // 第二次连接已建立，说明空闲判定触发了重连；此时上一次的断线原因已记录过
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while stats.connections.load(Ordering::Relaxed) < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(stats.connections.load(Ordering::Relaxed), 2);
    let events = global_mcp_telemetry().snapshot().recent_events.join("\n");
    assert!(events.contains("（test-heartbeat）"), "{events}");
    shutdown.store(true, Ordering::SeqCst);
    bridge.join().unwrap();
    let link = global_mcp_telemetry()
        .snapshot()
        .endpoints
        .into_iter()
        .find(|link| link.id == "test-heartbeat")
        .unwrap();
    assert!(!link.connected);
    assert_eq!(link.connections, 2);
    assert!(!link.endpoint.contains("heartbeat-secret"));
}

/// 连接失败的原因记入接入点状态，且不含 token。
#[test]
fn connection_failures_are_recorded_without_the_token() {
    let _telemetry_lock = ENDPOINT_TELEMETRY_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use std::net::TcpListener;
    // 先占用再释放一个端口，得到一个大概率没人监听的地址
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let shutdown = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(BridgeStats::default());
    let timing = BridgeTiming {
        first_backoff: Duration::from_millis(50),
        ..BridgeTiming::default()
    };
    global_mcp_telemetry().register_endpoints(&[(
        "test-refused".into(),
        format!("ws://127.0.0.1:{port}/mcp/?token=***"),
    )]);
    let endpoint = format!("ws://127.0.0.1:{port}/mcp/?token=refused-secret");
    let (shutdown_for_bridge, stats_for_bridge) = (shutdown.clone(), stats.clone());
    let bridge = std::thread::spawn(move || {
        run_bridge_timed(
            "test-refused",
            &endpoint,
            None,
            false,
            &shutdown_for_bridge,
            &stats_for_bridge,
            timing,
        )
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut error = None;
    while error.is_none() && std::time::Instant::now() < deadline {
        error = global_mcp_telemetry()
            .snapshot()
            .endpoints
            .into_iter()
            .find(|link| link.id == "test-refused")
            .and_then(|link| link.last_error);
        std::thread::sleep(Duration::from_millis(20));
    }
    shutdown.store(true, Ordering::SeqCst);
    bridge.join().unwrap();
    let error = error.expect("连接失败应记录原因");
    assert!(!error.contains("refused-secret"), "{error}");
    assert_eq!(stats.connections.load(Ordering::Relaxed), 0);
    assert_eq!(
        scrub_token("bad url wss://x/?token=abc&k=1", "wss://x/?token=abc&k=1"),
        "bad url wss://x/?token=***&k=***"
    );
    let complex = "wss://fakeuser:fakepass@example.invalid/mcp?k=fake1&token=fake2#frag";
    let scrubbed_complex = scrub_token(
        &format!("dial {complex} user=fakeuser pass=fakepass k=fake1 t=fake2"),
        complex,
    );
    for secret in ["fakeuser", "fakepass", "fake1", "fake2", "frag"] {
        assert!(
            !scrubbed_complex.contains(secret),
            "{secret} leaked in {scrubbed_complex}"
        );
    }
    assert!(scrubbed_complex.contains("wss://***@example.invalid/mcp?k=***&token=***#***"));
}

/// NV8：接入点身份只取标识类字段与有效期，不是 JWT 的地址返回 None。
#[test]
fn endpoint_identity_reads_only_identity_fields_from_the_token() {
    use base64::Engine;
    let encode = |value: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
    };
    let header = encode(serde_json::json!({"alg": "HS256", "typ": "JWT"}));
    let payload = encode(serde_json::json!({
        "userId": 42, "agentId": "agent-7", "endpointId": "ep_1", "purpose": "mcp-endpoint",
        "iat": 1_700_000_000, "exp": 1_700_000_100, "role": "admin", "nested": {"id": 1}
    }));
    let endpoint = format!("wss://api.example.test/mcp/?token={header}.{payload}.signature&x=1");
    let identity = endpoint_identity(&endpoint).unwrap();
    let fields = identity["fields"].as_object().unwrap();
    assert_eq!(fields["agentId"], "agent-7");
    assert_eq!(fields["userId"], "42");
    assert_eq!(fields["endpointId"], "ep_1");
    assert_eq!(fields["purpose"], "mcp-endpoint");
    assert!(
        !fields.contains_key("role") && !fields.contains_key("nested"),
        "{fields:?}"
    );
    assert_eq!(identity["expires_at"], 1_700_000_100.0);
    assert_eq!(identity["expired"], true);
    for endpoint in [
        "wss://api.example.test/mcp/?token=plain-token",
        "wss://api.example.test/mcp/",
        "wss://x/?token=a.%%%.c",
    ] {
        assert!(endpoint_identity(endpoint).is_none(), "{endpoint}");
    }
}

#[test]
fn disabled_gate_never_blocks_r2_workflow_cancel() {
    let backend = SwitchBackend {
        inner: FakeBackend::new(vec![(
            200,
            serde_json::json!({"success": true, "result": {"workflow_id": "wf-9", "status": "cancelled"}}),
        )]),
        disabled: vec!["laser_workflow_tool"],
        telemetry: laser_contracts::McpTelemetry::new(),
    };
    // 非停止类调用被关闭开关拦截
    let blocked = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action": "status", "workflow_id": "wf-9"}),
        false,
        Some(&backend),
    );
    assert!(!blocked.success);
    assert!(
        blocked
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("tool_disabled")
    );
    assert!(backend.inner.calls().is_empty());

    // R2 取消动作即使 laser_workflow_tool 被关闭也照常放行
    let cancel = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action": "cancel", "workflow_id": "wf-9"}),
        false,
        Some(&backend),
    );
    assert!(cancel.success, "{cancel:?}");
    let calls = backend.inner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, "/api/workflow");
    assert_eq!(calls[0].2["action"], "cancel");
}

#[test]
fn recommend_forwards_to_backend_and_resume_delete_enforce_confirmed() {
    let backend = FakeBackend::new(vec![
        (
            200,
            serde_json::json!({
                "success": true,
                "result": {
                    "material": "特制亚克力",
                    "thickness_mm": 4.0,
                    "matched_thickness_mm": 4.0,
                    "laser_mode": "cut",
                    "laser_max_power": 230,
                    "feed_rate": 240,
                    "passes": 2
                }
            }),
        ),
        (
            200,
            serde_json::json!({"success": true, "result": {"workflow_id": "wf-2", "status": "sending"}}),
        ),
        (
            400,
            serde_json::json!({"success": false, "result": "删除当前参数需要 confirmed=true"}),
        ),
    ]);
    let rec = dispatch_tool_call_with(
        "recommend_laser_params_tool",
        &serde_json::json!({
            "material": "特制亚克力",
            "thickness_mm": 4.0,
            "laser_mode": "cut",
            "power_scale": 255.0
        }),
        false,
        Some(&backend),
    );
    assert!(rec.success, "{rec:?}");
    assert!(
        rec.result.contains("推荐参数") && rec.result.contains("特制亚克力"),
        "{rec:?}"
    );

    // 未确认 resume 被拦截，不触达后端
    let unconfirmed_resume = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action": "resume", "workflow_id": "wf-2"}),
        false,
        Some(&backend),
    );
    assert!(!unconfirmed_resume.success);
    assert_eq!(backend.calls().len(), 1);

    // 确认后的 resume 携带连接参数转发
    let ok_resume = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action": "resume", "workflow_id": "wf-2", "confirmed": true, "host": "10.0.0.8"}),
        false,
        Some(&backend),
    );
    assert!(ok_resume.success, "{ok_resume:?}");

    // delete 未传 confirmed 时不得自动注入 confirmed=true
    let _ = dispatch_tool_call_with(
        "material_params_tool",
        &serde_json::json!({"action": "delete", "material": "特制亚克力"}),
        false,
        Some(&backend),
    );
    let calls = backend.calls();
    assert_eq!(calls.len(), 3);
    assert!(calls[0].1.contains("power_scale=255"));
    assert_eq!(calls[1].2["network_host"], "10.0.0.8");
    assert!(calls[2].2.get("confirmed").is_none());
}

/// 2026-11-06：AI 生成文字任务后只停在预览，`confirmed + send_after_generate` 不再顺带发送，
/// 结果提示模型等用户在电脑预览窗确认。
#[test]
fn ai_generation_stops_at_the_desktop_preview_even_when_asked_to_send() {
    let ok = (
        200,
        serde_json::json!({"success": true, "result": {"workflow_id": "wf-ai", "status": "preview_ready"}}),
    );
    let backend = FakeBackend::new(vec![ok.clone(), ok]);
    let result = dispatch_tool_call_with(
        "generate_text_laser_task_tool",
        &serde_json::json!({"text": "生日快乐", "material": "椴木", "thickness_mm": 3, "confirmed": true, "send_after_generate": true, "host": "10.0.0.8"}),
        false,
        Some(&backend),
    );
    assert!(result.success, "{result:?}");
    assert!(
        result.result.contains("同意开刻") && result.result.contains("没有直接开始加工"),
        "{}",
        result.result
    );
    let preview = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action": "preview", "text": "MOSS", "material": "椴木", "thickness_mm": 3}),
        false,
        Some(&backend),
    );
    assert!(preview.result.contains("同意开刻"), "{}", preview.result);
    let calls = backend.calls();
    assert_eq!(calls.len(), 2);
    assert!(
        calls.iter().all(|call| call.1 == "/api/text-task/generate"),
        "{calls:?}"
    );
}

#[test]
fn connection_probe_tools_route_through_backend_busy_protection() {
    let busy_ok = (
        200,
        serde_json::json!({
            "success": true,
            "result": {
                "connected": true,
                "serial_connected": true,
                "network_connected": false,
                "attempts": [{"mode": "serial", "target": "COM3@115200", "connected": true, "laser_mode_ok": true, "summary": "设备正在执行任务 wf-busy"}],
                "summary": "设备正在执行任务 wf-busy"
            }
        }),
    );
    let offline = (
        200,
        serde_json::json!({
            "success": true,
            "result": {
                "connected": false,
                "serial_connected": false,
                "network_connected": false,
                "attempts": [{"mode": "network", "target": "10.0.0.9:23", "connected": false, "laser_mode_ok": false, "summary": "网络 连接失败: timeout"}],
                "summary": "网络 连接失败: timeout"
            }
        }),
    );
    let backend = FakeBackend::new(vec![busy_ok.clone(), busy_ok, offline]);

    let r1 = dispatch_tool_call_with(
        "check_laser_connection_tool",
        &serde_json::json!({"port": "COM3", "timeout": 1.5}),
        false,
        Some(&backend),
    );
    assert!(r1.success, "{r1:?}");
    assert!(r1.result.contains("wf-busy"), "{}", r1.result);

    let r2 = dispatch_tool_call_with(
        "laser_grbl_tool",
        &serde_json::json!({"action": "status", "port": "COM3"}),
        false,
        Some(&backend),
    );
    assert!(r2.success, "{r2:?}");
    assert!(r2.result.contains("wf-busy"), "{}", r2.result);

    let r3 = dispatch_tool_call_with(
        "laser_network_grbl_tool",
        &serde_json::json!({"action": "probe", "host": "10.0.0.9"}),
        false,
        Some(&backend),
    );
    assert!(
        !r3.success,
        "disconnected probe must report success=false: {r3:?}"
    );
    assert!(r3.result.contains("连接失败"), "{}", r3.result);

    let calls = backend.calls();
    assert_eq!(calls.len(), 3);
    assert!(calls.iter().all(|c| c.1 == "/api/connection/check"));
    assert_eq!(calls[0].2["network_timeout"], 1.5);
    assert_eq!(calls[0].2["auto_fallback"], false);
    assert_eq!(calls[1].2["connection_mode"], "serial");
    assert_eq!(calls[1].2["auto_fallback"], false);
    assert_eq!(calls[2].2["connection_mode"], "network");
    assert_eq!(calls[2].2["auto_fallback"], false);
}

#[test]
fn voice_mcp_tools_surface_preview_and_send_warnings_in_summaries() {
    let backend = FakeBackend::new(vec![
        (
            200,
            serde_json::json!({
                "success": true,
                "result": {
                    "workflow_id": "wf-warn",
                    "status": "preview_ready",
                    "artifacts": {
                        "actual_params": {
                            "laser_max_power": 500.0,
                            "power_scale": 1000.0,
                            "feed_rate": 3600.0,
                            "passes": 1
                        },
                        "warnings": [
                            "进给速度 3600 mm/min 超过固件最大速度 2000 mm/min，控制器将自动降速并拉长实际用时"
                        ]
                    }
                }
            }),
        ),
        (
            200,
            serde_json::json!({
                "success": true,
                "result": {
                    "workflow_id": "wf-warn",
                    "status": "sending",
                    "send_result": {
                        "lines_sent": 0,
                        "total_lines": 120,
                        "warnings": [
                            "软限位未开启（$20=0），超行程时控制器不会自动拦截",
                            "按当前机械坐标 MPos=(85.0, 20.0) 估算，X 轴末端 115.0 mm 可能超出行程 100.0 mm"
                        ]
                    }
                }
            }),
        ),
    ]);

    let preview = dispatch_tool_call_with(
        "generate_text_laser_task_tool",
        &serde_json::json!({"text": "注意安全", "material": "椴木", "thickness_mm": 3}),
        false,
        Some(&backend),
    );
    assert!(preview.success, "{preview:?}");
    assert!(
        preview
            .result
            .contains("注意：进给速度 3600 mm/min 超过固件最大速度 2000 mm/min"),
        "{}",
        preview.result
    );

    let send = dispatch_tool_call_with(
        "laser_workflow_tool",
        &serde_json::json!({"action": "confirm_send", "workflow_id": "wf-warn", "confirmed": true, "host": "10.0.0.5"}),
        false,
        Some(&backend),
    );
    assert!(send.success, "{send:?}");
    assert!(
        send.result.contains("⚠️")
            && send.result.contains("软限位未开启")
            && send.result.contains("机械坐标 MPos"),
        "{}",
        send.result
    );
}
