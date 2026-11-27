//! VA1/VA2/VA3/VA6 真小智“文字直通”联调通道、模拟安全门禁、停止快路径、记忆隔离与本机调试入口测试。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;
use crate::debug_api::{ProcessTraceControl, ProcessTraceToggleResult};
use crate::memory::{MemoryRecord, MemoryService, VoiceMemoryStore};

struct SimTestTools {
    calls: Mutex<Vec<(String, String)>>,
    active_laser_job: AtomicBool,
    strict_mode: AtomicBool,
}

impl Default for SimTestTools {
    fn default() -> Self {
        Self {
            calls: Mutex::default(),
            active_laser_job: AtomicBool::new(false),
            strict_mode: AtomicBool::new(false),
        }
    }
}

impl ToolExecutor for SimTestTools {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "laser_workflow_tool".into(),
                description: "激光加工任务".into(),
                parameters: serde_json::json!({"type":"object"}),
            },
            ToolSpec {
                name: "laser_safe_action_tool".into(),
                description: "激光安全控制".into(),
                parameters: serde_json::json!({"type":"object"}),
            },
            ToolSpec {
                name: "hass_set_state".into(),
                description: "家居控制".into(),
                parameters: serde_json::json!({"type":"object"}),
            },
            ToolSpec {
                name: "agent_codex".into(),
                description: "本机 Agent".into(),
                parameters: serde_json::json!({"type":"object"}),
            },
        ]
    }

    fn call<'a>(
        &'a self,
        _: &'a ToolContext,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_owned(), arguments.to_owned()));
            if self.strict_mode.load(Ordering::Acquire)
                && (name == "laser_workflow_tool" && arguments.contains("confirm_send")
                    || name == "laser_safe_action_tool" && !arguments.contains("status"))
            {
                return ToolOutcome::Speak(
                    "已发送桌面审批，请用户在电脑上批准（不应在模拟轮次触达）".into(),
                );
            }
            ToolOutcome::Reply(
                serde_json::json!({
                    "success": true,
                    "result": format!("ok:{name}"),
                })
                .to_string(),
            )
        })
    }

    fn has_active_laser_job<'a>(&'a self, _: &'a ToolContext) -> BoxFuture<'a, Option<bool>> {
        Box::pin(async move { Some(self.active_laser_job.load(Ordering::Acquire)) })
    }
}

/// VA2 #1 + VA1 + VA3：模拟轮次拒绝发送/移动/倍率/家居/Agent（即使严格模式开启也不进审批），
/// 放行预览生成；完整链路下发 `stt` 并产出 VA3 完整明细，对话记录带 `simulated: true`。
#[test]
fn simulated_turn_blocks_physical_tools_even_in_strict_mode_and_allows_preview() {
    let tools = Arc::new(SimTestTools::default());
    tools.strict_mode.store(true, Ordering::Release);
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            Round::Reply {
                deltas: vec![],
                calls: vec![
                    ToolCall::new(
                        "c-send",
                        "laser_workflow_tool",
                        r#"{"action":"confirm_send","workflow_id":"wf-1","confirmed":true}"#,
                    ),
                    ToolCall::new(
                        "c-move",
                        "laser_safe_action_tool",
                        r#"{"action":"absolute_move","x_mm":20,"y_mm":20,"confirmed":true}"#,
                    ),
                    ToolCall::new(
                        "c-override",
                        "laser_safe_action_tool",
                        r#"{"action":"feed_plus_10"}"#,
                    ),
                    ToolCall::new(
                        "c-home",
                        "hass_set_state",
                        r#"{"entity_id":"light.studio","state":"on"}"#,
                    ),
                    ToolCall::new("c-agent", "agent_codex", r#"{"prompt":"echo hi"}"#),
                    ToolCall::new(
                        "c-preview",
                        "laser_workflow_tool",
                        r#"{"action":"preview","text":"新年快乐","material":"椴木","thickness_mm":3}"#,
                    ),
                ],
                delay_ms: 0,
            },
            say(&["模拟轮次已拒绝所有物理操作，仅为你生成了木牌雕刻预览。"]),
        ])),
        ..Default::default()
    });
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        llm.clone(),
        Arc::new(FakeTts::default()),
        tools.clone(),
        ConversationSettings::default(),
    );
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            idle_timeout: Duration::from_secs(60),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    server.set_debug_enabled(true);

    let mut device = chat_device(&server, &registry, 1);
    wait_until("设备会话上线", || {
        !server.active_device_sessions().is_empty()
    });

    let ack = server
        .inject_text(DEVICE, "帮我生成预览并直接开始雕刻", true)
        .unwrap();
    assert!(ack.simulated);
    assert_eq!(ack.device_id, DEVICE);

    let items = collect_items(&mut device);
    let seq = texts(&items);
    assert!(
        seq.first()
            .is_some_and(|s| s == "听:帮我生成预览并直接开始雕刻"),
        "必须先下发 stt 字幕让设备立刻显示注入文字：{seq:?}"
    );
    assert!(seq.iter().any(|s| s.contains("仅为你生成了木牌雕刻预览")));
    assert!(items.iter().any(|i| matches!(i, Item::Frame)));

    wait_until("轮次明细落定", || {
        server
            .turn_detail(&ack.turn_id)
            .is_some_and(|d| d.status == "done")
    });

    // 只有 preview 真正进入了 ToolExecutor；所有物理/家居/Agent 工具一次都没触达（也不进严格模式审批）
    let executed = tools.calls.lock().unwrap().clone();
    assert_eq!(executed.len(), 1, "只应执行 preview 一次：{executed:?}");
    assert_eq!(executed[0].0, "laser_workflow_tool");
    assert!(executed[0].1.contains("\"preview\""));

    // 第二轮大模型拿到了如实拒绝的原因 JSON
    let seen = llm.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let second_msgs = &seen[1].0;
    let blocked_msgs: Vec<_> = second_msgs
        .iter()
        .filter(|m| {
            m.role == Role::Tool
                && m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("simulated_turn_blocked"))
        })
        .collect();
    assert_eq!(
        blocked_msgs.len(),
        5,
        "5 个危险工具都应把模拟门禁拒绝原因如实返回给大模型"
    );

    // VA3 完整明细校验
    let detail = server.turn_detail(&ack.turn_id).unwrap();
    assert!(detail.simulated);
    assert_eq!(detail.route, "llm");
    assert_eq!(detail.injected_text, "帮我生成预览并直接开始雕刻");
    assert_eq!(detail.corrected_text, "帮我生成预览并直接开始雕刻");
    assert!(detail.stages_ms.contains_key("first_llm_delta"));
    assert!(detail.stages_ms.contains_key("llm_done"));
    assert!(detail.stages_ms.contains_key("first_synthesis_ready"));
    assert!(detail.stages_ms.contains_key("first_audio_frame"));
    assert!(detail.stages_ms.contains_key("turn_end"));
    assert_eq!(detail.tools.len(), 6);
    assert_eq!(detail.tools.iter().filter(|t| t.blocked).count(), 5);
    assert_eq!(detail.tools.iter().filter(|t| !t.blocked).count(), 1);
    assert!(detail.reply.contains("仅为你生成了木牌雕刻预览"));
    assert!(detail.spoken.contains("仅为你生成了木牌雕刻预览"));
    assert!(detail.playback.frames > 0);

    // VA2 #3 对话记录带模拟标记
    let traces = server.status().debug_traces;
    assert_eq!(traces.len(), 1);
    assert!(traces[0].simulated, "对话记录必须带模拟标记");
    assert_eq!(traces[0].tools.iter().filter(|t| t.blocked).count(), 5);

    server.stop();
}

/// VA2 #2：模拟轮次里“停 / 停止”——当前有激光加工任务时绝不发急停，只记“模拟轮次的停止命令未执行”；
/// 确认没有加工任务时按原快路径执行以便测出响应。
#[test]
fn simulated_stop_fast_path_skips_estop_when_laser_job_active_and_runs_when_idle() {
    let tools = Arc::new(SimTestTools::default());
    tools.active_laser_job.store(true, Ordering::Release);
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        Arc::new(FakeLlm::default()),
        Arc::new(FakeTts::default()),
        tools.clone(),
        ConversationSettings::default(),
    );
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            idle_timeout: Duration::from_secs(60),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    wait_until("设备会话上线", || {
        !server.active_device_sessions().is_empty()
    });

    // 1. 有加工任务运行中：注入“停”绝不调用 emergency_stop
    let ack1 = server.inject_text(DEVICE, "停", true).unwrap();
    let items1 = collect_items(&mut device);
    let seq1 = texts(&items1);
    assert!(
        seq1.iter().any(|s| s.contains("模拟轮次的停止命令未执行")),
        "{seq1:?}"
    );
    wait_until("第 1 轮完成", || {
        server
            .turn_detail(&ack1.turn_id)
            .is_some_and(|d| d.status == "done")
    });
    assert!(
        tools.calls.lock().unwrap().is_empty(),
        "有加工任务时模拟停止绝不能向激光机发急停"
    );
    let detail1 = server.turn_detail(&ack1.turn_id).unwrap();
    assert_eq!(detail1.route, "fast_stop");
    assert_eq!(
        detail1.stop_skipped_reason.as_deref(),
        Some("模拟轮次的停止命令未执行")
    );

    // 2. 确认无加工任务：按原停止快路径执行（仅调用 mock，绝不触碰真实硬件）
    tools.active_laser_job.store(false, Ordering::Release);
    wait_until("回到空闲", || {
        server
            .active_device_sessions()
            .first()
            .is_some_and(|s| s.state == "idle")
    });
    let ack2 = server.inject_text(DEVICE, "停止", true).unwrap();
    let _items2 = collect_items(&mut device);
    wait_until("第 2 轮完成", || {
        server
            .turn_detail(&ack2.turn_id)
            .is_some_and(|d| d.status == "done")
    });
    let calls = tools.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "laser_safe_action_tool");
    assert!(calls[0].1.contains("\"emergency_stop\""));
    let detail2 = server.turn_detail(&ack2.turn_id).unwrap();
    assert_eq!(detail2.route, "fast_stop");
    assert_eq!(detail2.stop_skipped_reason, None);

    server.stop();
}

/// VA2 #3：模拟轮次保留会话内多轮上下文（支持追问改参数），但绝不触发长期记忆摘要或结构化上下文提取。
#[test]
fn simulated_turns_support_multi_turn_followup_but_skip_memory_extraction() {
    struct CountingMemoryStore {
        writes: AtomicUsize,
    }
    impl VoiceMemoryStore for CountingMemoryStore {
        fn allowed(&self, _: &str) -> Result<bool, String> {
            Ok(true)
        }
        fn load(&self, _: &str) -> Result<Option<MemoryRecord>, String> {
            Ok(None)
        }
        fn ensure(&self, device_id: &str) -> Result<MemoryRecord, String> {
            Ok(MemoryRecord {
                device_id: device_id.into(),
                summary: String::new(),
                revision: 0,
                updated_at: 0.0,
            })
        }
        fn update(
            &self,
            device_id: &str,
            revision: i64,
            summary: &str,
        ) -> Result<Option<MemoryRecord>, String> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(Some(MemoryRecord {
                device_id: device_id.into(),
                summary: summary.into(),
                revision: revision + 1,
                updated_at: 1.0,
            }))
        }
        fn list(&self) -> Result<Vec<MemoryRecord>, String> {
            Ok(Vec::new())
        }
    }

    let mem_store = Arc::new(CountingMemoryStore {
        writes: AtomicUsize::new(0),
    });
    let memory = MemoryService::new(mem_store.clone());
    memory.configure(true, || Ok(())).unwrap();

    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            say(&["已按新年快乐生成木牌预览。"]),
            say(&["好的，已把文字改成福气满满并重新生成预览。"]),
        ])),
        ..Default::default()
    });
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        llm.clone(),
        Arc::new(FakeTts::default()),
        Arc::new(SimTestTools::default()),
        ConversationSettings::default(),
    )
    .with_memory(memory);

    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            idle_timeout: Duration::from_secs(60),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    wait_until("设备会话上线", || {
        !server.active_device_sessions().is_empty()
    });

    let ack1 = server
        .inject_text(DEVICE, "生成新年快乐木牌预览", true)
        .unwrap();
    let _ = collect_items(&mut device);
    wait_until("第 1 轮完成且空闲", || {
        server
            .turn_detail(&ack1.turn_id)
            .is_some_and(|d| d.status == "done")
            && server
                .active_device_sessions()
                .first()
                .is_some_and(|s| s.state == "idle")
    });

    let ack2 = server
        .inject_text(DEVICE, "把刚才的文字改成福气满满", true)
        .unwrap();
    let _ = collect_items(&mut device);
    wait_until("第 2 轮完成", || {
        server
            .turn_detail(&ack2.turn_id)
            .is_some_and(|d| d.status == "done")
    });

    // 验证多轮追问生效：第 2 轮发给大模型的历史包含第 1 轮对话
    let seen = llm.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(
        seen[1]
            .0
            .iter()
            .any(|m| m.content.as_deref().is_some_and(|c| c.contains("新年快乐"))),
        "同一模拟会话内的多轮追问应保留上下文"
    );

    // 断开会话触发 conversation_closed，等待短暂时间确认长期记忆写入次数恒为 0
    let _ = device.close(None);
    wait_until("会话关闭", || {
        server.active_device_sessions().is_empty()
    });
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(
        mem_store.writes.load(Ordering::SeqCst),
        0,
        "模拟轮次绝不能写入长期记忆"
    );

    server.stop();
}

/// VA2 #4–#5 + VA3：本机 127.0.0.1 调试 HTTP 入口门禁（鉴权、关闭、过期、禁跨源、禁未授权路由）与完整联调流程。
#[test]
fn debug_http_api_enforces_auth_ttl_disable_and_completes_full_turn() {
    struct FakeTraceControl {
        enabled: AtomicBool,
        flushes: AtomicUsize,
    }
    impl ProcessTraceControl for FakeTraceControl {
        fn set_enabled(&self, enabled: bool) -> Result<ProcessTraceToggleResult, String> {
            let previous_enabled = self.enabled.swap(enabled, Ordering::SeqCst);
            Ok(ProcessTraceToggleResult {
                enabled,
                previous_enabled,
                trace_dir_rel: "logs/process-trace".into(),
                using_custom_dir: false,
            })
        }
        fn flush(&self) {
            self.flushes.fetch_add(1, Ordering::SeqCst);
        }
    }

    let trace_ctrl = Arc::new(FakeTraceControl {
        enabled: AtomicBool::new(false),
        flushes: AtomicUsize::new(0),
    });
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&["已查询三毫米椴木的雕刻参数。"])])),
        ..Default::default()
    });
    let settings = ConversationSettings {
        asr_corrections: vec![("段木".into(), "椴木".into())],
        ..Default::default()
    };
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        llm,
        Arc::new(FakeTts::default()),
        Arc::new(SimTestTools::default()),
        settings,
    );
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            idle_timeout: Duration::from_secs(60),
            active_laser_job: Some(Arc::new(|| false)),
            debug_process_trace: Some(trace_ctrl.clone()),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();

    let token = "test-debug-token-0123456789abcdef0123456789abcdef";
    let now = crate::unix_now();
    let port = server
        .enable_debug_api(0, token.into(), now, now + 3600, None)
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();

    // 1. 缺令牌 / 错令牌 / 带浏览器 Origin / 越界路由均被拒绝
    assert_eq!(
        client
            .get(format!("{base}/debug/v1/devices"))
            .send()
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(format!("{base}/debug/v1/devices"))
            .header("Authorization", "Bearer wrong-token")
            .send()
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(format!("{base}/debug/v1/devices"))
            .header("Authorization", format!("Bearer {token}"))
            .header("Origin", "http://localhost:5173")
            .send()
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .post(format!("{base}/debug/v1/tools/call"))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({"name": "laser_safe_action_tool"}))
            .send()
            .unwrap()
            .status(),
        404
    );

    // 2. 无在线设备时列表为空，注入返回 404
    let empty_list: Value = client
        .get(format!("{base}/debug/v1/devices"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(empty_list["devices"].as_array().unwrap().len(), 0);
    assert_eq!(empty_list["active_laser_job"], false);
    assert_eq!(
        client
            .post(format!("{base}/debug/v1/devices/{DEVICE}/inject"))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({"text": "你好"}))
            .send()
            .unwrap()
            .status(),
        404
    );

    // 3. 上线一台设备并走完过程记录开关 + 文字直通注入 + 明细查询（含同音纠偏）
    let mut device = chat_device(&server, &registry, 1);
    wait_until("设备会话上线", || {
        !server.active_device_sessions().is_empty()
    });

    let pt_on: Value = client
        .post(format!("{base}/debug/v1/process-trace"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"enabled": true}))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(pt_on["previous_enabled"], false);
    assert_eq!(pt_on["enabled"], true);
    assert!(trace_ctrl.enabled.load(Ordering::SeqCst));

    let inject_res = client
        .post(format!("{base}/debug/v1/devices/{DEVICE}/inject"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"text": "帮我查三毫米段木参数"}))
        .send()
        .unwrap();
    assert_eq!(inject_res.status(), 202);
    let ack: Value = inject_res.json().unwrap();
    let turn_id = ack["turn_id"].as_str().unwrap().to_owned();
    assert_eq!(ack["simulated"], true);

    let items = collect_items(&mut device);
    assert!(items.iter().any(|i| matches!(i, Item::Frame)));
    wait_until("轮次完成", || {
        server
            .turn_detail(&turn_id)
            .is_some_and(|d| d.status == "done")
    });

    let detail: Value = client
        .get(format!("{base}/debug/v1/turns/{turn_id}"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(detail["simulated"], true);
    assert_eq!(detail["injected_text"], "帮我查三毫米段木参数");
    assert_eq!(detail["corrected_text"], "帮我查三毫米椴木参数");
    assert_eq!(detail["route"], "llm");
    assert!(detail["playback"]["frames"].as_u64().unwrap() > 0);
    assert_eq!(detail["device_online"], true);

    let pt_off: Value = client
        .post(format!("{base}/debug/v1/process-trace"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"enabled": false, "flush": true}))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(pt_off["enabled"], false);
    assert!(trace_ctrl.flushes.load(Ordering::SeqCst) >= 1);

    // 4. 过期令牌自动作废（401 token_expired）
    let _ = server
        .enable_debug_api(port, token.into(), now - 90_000, now - 1, None)
        .unwrap();
    let expired_res = client
        .get(format!("{base}/debug/v1/devices"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .unwrap();
    assert_eq!(expired_res.status(), 401);
    let expired_body: Value = expired_res.json().unwrap();
    assert_eq!(expired_body["error"], "token_expired");

    // 5. 关闭开关后立即停止入口，原令牌无法再访问
    server.disable_debug_api();
    assert!(server.debug_api_port().is_none());

    server.stop();
}

/// VB1/VB4 端到端验证：开启识别语料采集后，真实小智语音轮次保存 16 kHz 单声道 WAV 与识别元数据，
/// 而 VA 模拟轮次（文字直通）一律不保存。
#[test]
fn asr_corpus_captures_real_utterance_and_skips_simulated_turn() {
    let temp = tempfile::tempdir().unwrap();
    let corpus = Arc::new(crate::AsrCorpusStore::new(
        temp.path().join("asr-corpus"),
        crate::AsrCorpusConfig {
            enabled: true,
            max_items: 500,
            max_days: 7,
        },
    ));
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("三毫米段木刻字".into())])),
        ..Default::default()
    });
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            say(&["好的，收到三毫米椴木刻字。"]),
            say(&["模拟轮次已收到。"]),
        ])),
        ..Default::default()
    });
    let settings = ConversationSettings {
        asr_corrections: vec![("段木".into(), "椴木".into())],
        ..Default::default()
    };
    let conversation = Conversation::new(
        asr,
        llm,
        Arc::new(FakeTts::default()),
        Arc::new(SimTestTools::default()),
        settings,
    );
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            asr_corpus: Some(corpus.clone()),
            idle_timeout: Duration::from_secs(60),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    wait_until("设备会话上线", || {
        !server.active_device_sessions().is_empty()
    });

    // 1. 真实语音轮次（手动拾音送入上行音频）应写入 1 条 16 kHz WAV + JSON
    speak_manual(&mut device, 1, 500);
    let _ = collect_items(&mut device);
    wait_until("真实语音语料落盘", || {
        corpus.list().map(|v| v.len()).unwrap_or(0) == 1
    });
    let items = corpus.list().unwrap();
    assert_eq!(items.len(), 1);
    assert!(!items[0].meta.simulated);
    assert_eq!(items[0].meta.sample_rate, 16_000);
    assert_eq!(items[0].meta.raw_text, "三毫米段木刻字");
    assert_eq!(items[0].meta.corrected_text, "三毫米椴木刻字");

    // 2. VA 模拟文字直通轮次绝不能写入语料库
    let ack = server
        .inject_text(DEVICE, "帮我查激光机状态", true)
        .unwrap();
    let _ = collect_items(&mut device);
    wait_until("模拟轮次结束", || {
        server
            .turn_detail(&ack.turn_id)
            .is_some_and(|d| d.status == "done")
    });
    assert_eq!(
        corpus.list().unwrap().len(),
        1,
        "模拟轮次不得保存进识别语料"
    );

    server.stop();
}

/// VC1 核心安全测试：打开测试时间窗后，真实音频轮次的发送与移动被模拟门禁拒绝、有加工任务时停止快路径不发急停、
/// 对话记录标为“模拟（整体测试）”、真实外放录音仍可落盘到 asr-corpus；主动关闭时间窗后立刻恢复普通轮次并记录事件。
#[test]
fn vc1_bench_window_locks_real_audio_turns_skips_estop_and_restores_after_close() {
    let temp = tempfile::tempdir().unwrap();
    let corpus = Arc::new(crate::AsrCorpusStore::new(
        temp.path().join("asr-corpus"),
        crate::AsrCorpusConfig {
            enabled: true,
            max_items: 500,
            max_days: 7,
        },
    ));
    let tools = Arc::new(SimTestTools::default());
    tools.strict_mode.store(true, Ordering::Release);
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([
            Ok("直接开始雕刻并移动激光头".into()),
            Ok("停".into()),
            Ok("请移动激光头到原点附近".into()),
        ])),
        ..Default::default()
    });
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            // 第 1 轮（窗口内真实音频）：模型尝试调用 send + move + preview
            Round::Reply {
                deltas: vec![],
                calls: vec![
                    ToolCall::new(
                        "bw-send",
                        "laser_workflow_tool",
                        r#"{"action":"confirm_send","workflow_id":"wf-1","confirmed":true}"#,
                    ),
                    ToolCall::new(
                        "bw-move",
                        "laser_safe_action_tool",
                        r#"{"action":"absolute_move","x_mm":15,"y_mm":15,"confirmed":true}"#,
                    ),
                    ToolCall::new(
                        "bw-preview",
                        "laser_workflow_tool",
                        r#"{"action":"preview","text":"测试铭牌","material":"椴木","thickness_mm":3}"#,
                    ),
                ],
                delay_ms: 0,
            },
            say(&["测试时间窗内已拒绝发送与移动，只生成了预览。"]),
            // 第 3 轮（关闭时间窗后的普通真实音频）：允许执行正常工具调用
            Round::Reply {
                deltas: vec![],
                calls: vec![ToolCall::new(
                    "normal-move",
                    "laser_safe_action_tool",
                    r#"{"action":"absolute_move","x_mm":10,"y_mm":10,"confirmed":true}"#,
                )],
                delay_ms: 0,
            },
            say(&["已恢复普通轮次并执行移动请求。"]),
        ])),
        ..Default::default()
    });
    let conversation = Conversation::new(
        asr,
        llm,
        Arc::new(FakeTts::default()),
        tools.clone(),
        ConversationSettings::default(),
    );
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            asr_corpus: Some(corpus.clone()),
            idle_timeout: Duration::from_secs(60),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    server.set_debug_enabled(true);

    let token = "vc1-test-token-0123456789abcdef0123456789abcdef";
    let now = crate::unix_now();
    let port = server
        .enable_debug_api(0, token.into(), now, now + 3600, None)
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();

    let mut device = chat_device(&server, &registry, 1);
    wait_until("设备会话上线", || {
        !server.active_device_sessions().is_empty()
    });

    // 参数边界校验：max_minutes 为 0 或超过 60 必须 400 拒绝
    assert_eq!(
        client
            .post(format!("{base}/debug/v1/bench-window"))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({"device_id": DEVICE, "action": "open", "max_minutes": 0}))
            .send()
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        client
            .post(format!("{base}/debug/v1/bench-window"))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({"device_id": DEVICE, "action": "open", "max_minutes": 61}))
            .send()
            .unwrap()
            .status(),
        400
    );

    // 1. 打开测试时间窗
    let open_res = client
        .post(format!("{base}/debug/v1/bench-window"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"device_id": DEVICE, "action": "open", "max_minutes": 30}))
        .send()
        .unwrap();
    assert_eq!(open_res.status(), 200);
    let open_body: Value = open_res.json().unwrap();
    assert_eq!(open_body["bench_window"]["active"], true);
    assert_eq!(open_body["bench_window"]["max_minutes"], 30);

    let dev_list: Value = client
        .get(format!("{base}/debug/v1/devices"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(dev_list["devices"][0]["bench_window"]["active"], true);

    // 2. 时间窗内真实音频轮次：发送与移动全部被 sim_guard 拒绝，仅预览放行
    speak_manual(&mut device, 1, 500);
    let _ = collect_items(&mut device);
    wait_until("窗口内第 1 轮完成", || {
        server
            .turn_details_for_device(DEVICE, 5)
            .first()
            .is_some_and(|d| d.turn == 1 && d.status == "done")
    });
    {
        let executed = tools.calls.lock().unwrap();
        assert_eq!(executed.len(), 1, "时间窗内只能放行 preview：{executed:?}");
        assert!(executed[0].1.contains("\"preview\""));
    }
    let d1 = server.turn_details_for_device(DEVICE, 5).remove(0);
    assert!(d1.simulated && d1.bench_window);
    assert_eq!(d1.simulated_label.as_deref(), Some("模拟（整体测试）"));
    assert!(d1.speech_began_at_ms.is_some());
    assert!(d1.speech_end_at_ms.is_some());
    assert!(d1.first_audio_frame_at_ms.is_some());
    assert!(d1.corpus_date.is_some() && d1.corpus_turn_id.is_some());
    assert_eq!(d1.tools.iter().filter(|t| t.blocked).count(), 2);

    // 对话记录面板标为 bench_window = true（前端渲染“模拟（整体测试）”）
    let traces = server.status().debug_traces;
    assert_eq!(traces.len(), 1);
    assert!(traces[0].simulated && traces[0].bench_window);

    // VC3 #7：外放真实麦克风语音在时间窗内仍可保存进 asr-corpus 供自动标注
    wait_until("时间窗内真实音频写入 asr-corpus", || {
        corpus.list().map(|v| v.len()).unwrap_or(0) == 1
    });
    assert!(corpus.list().unwrap()[0].meta.bench_window);

    // 3. 时间窗内有激光任务在跑时，真实音频说“停”绝不发急停
    tools.active_laser_job.store(true, Ordering::Release);
    speak_manual(&mut device, 1, 500);
    let items_stop = collect_items(&mut device);
    assert!(
        texts(&items_stop)
            .iter()
            .any(|s| s.contains("模拟轮次的停止命令未执行")),
        "{:?}",
        texts(&items_stop)
    );
    wait_until("窗口内第 2 轮完成", || {
        server
            .turn_details_for_device(DEVICE, 5)
            .first()
            .is_some_and(|d| d.turn == 2 && d.status == "done")
    });
    assert!(
        !tools
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, a)| a.contains("emergency_stop")),
        "时间窗内有加工任务时停止快路径绝不能发急停"
    );

    // 4. 主动关闭测试时间窗 -> 写入关闭事件，立刻恢复普通轮次
    let close_res = client
        .post(format!("{base}/debug/v1/bench-window"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"device_id": DEVICE, "action": "close"}))
        .send()
        .unwrap();
    assert_eq!(close_res.status(), 200);
    assert!(server.bench_windows().is_empty());
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|e| e.text.contains("测试时间窗已关闭（脚本主动关闭）")),
        "关闭时间窗必须写入诊断事件"
    );

    // 关闭后再次真实语音触发移动：应恢复正常（进入 ToolExecutor，非模拟轮次）
    tools.active_laser_job.store(false, Ordering::Release);
    tools.strict_mode.store(false, Ordering::Release);
    speak_manual(&mut device, 1, 500);
    let _ = collect_items(&mut device);
    wait_until("关闭窗口后第 3 轮完成", || {
        server
            .turn_details_for_device(DEVICE, 5)
            .first()
            .is_some_and(|d| d.turn == 3 && d.status == "done")
    });
    let d3 = server.turn_details_for_device(DEVICE, 5).remove(0);
    assert!(!d3.simulated && !d3.bench_window);
    assert_eq!(d3.simulated_label, None);
    assert!(
        tools
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(n, a)| n == "laser_safe_action_tool" && a.contains("absolute_move")),
        "关闭时间窗后普通轮次应恢复正常工具执行"
    );

    server.stop();
}

/// VC1 自动关闭全覆盖：到达最长时长自动关闭、设备断开自动关闭、关闭调试开关自动关闭、令牌失效自动关闭，且不污染长期记忆。
#[test]
fn vc1_bench_window_auto_closes_on_timeout_disconnect_disable_and_token_expiry() {
    struct CountingMemoryStore {
        writes: AtomicUsize,
    }
    impl VoiceMemoryStore for CountingMemoryStore {
        fn allowed(&self, _: &str) -> Result<bool, String> {
            Ok(true)
        }
        fn load(&self, _: &str) -> Result<Option<MemoryRecord>, String> {
            Ok(None)
        }
        fn ensure(&self, device_id: &str) -> Result<MemoryRecord, String> {
            Ok(MemoryRecord {
                device_id: device_id.into(),
                summary: String::new(),
                revision: 0,
                updated_at: 0.0,
            })
        }
        fn update(
            &self,
            device_id: &str,
            revision: i64,
            summary: &str,
        ) -> Result<Option<MemoryRecord>, String> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(Some(MemoryRecord {
                device_id: device_id.into(),
                summary: summary.into(),
                revision: revision + 1,
                updated_at: 1.0,
            }))
        }
        fn list(&self) -> Result<Vec<MemoryRecord>, String> {
            Ok(Vec::new())
        }
    }

    let mem_store = Arc::new(CountingMemoryStore {
        writes: AtomicUsize::new(0),
    });
    let memory = MemoryService::new(mem_store.clone());
    memory.configure(true, || Ok(())).unwrap();

    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("时间窗内聊聊椴木".into())])),
        ..Default::default()
    });
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&["好的，已记录椴木参数仅限本轮。"])])),
        ..Default::default()
    });
    let conversation = Conversation::new(
        asr,
        llm,
        Arc::new(FakeTts::default()),
        Arc::new(SimTestTools::default()),
        ConversationSettings::default(),
    )
    .with_memory(memory);

    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            idle_timeout: Duration::from_secs(60),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();

    let token = "vc1-autoclose-token-0123456789abcdef0123456789";
    let now = crate::unix_now();
    let port = server
        .enable_debug_api(0, token.into(), now, now + 3600, None)
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();

    let mut device = chat_device(&server, &registry, 1);
    wait_until("设备 1 上线", || {
        !server.active_device_sessions().is_empty()
    });

    // A. 超时自动关闭（120 ms 测试时长）
    server
        .shared
        .open_bench_window_for_test(DEVICE, Duration::from_millis(120))
        .unwrap();
    assert_eq!(server.bench_windows().len(), 1);
    wait_until("测试时间窗超时自动关闭", || {
        server.bench_windows().is_empty()
    });
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|e| e.text.contains("测试时间窗已关闭（到达最长时长）")),
        "超时关闭应写事件"
    );

    // B. 时间窗内跑完一轮真实音频后设备断开：自动关闭且不写长期记忆
    server.open_bench_window(DEVICE, 30).unwrap();
    speak_manual(&mut device, 1, 500);
    let _ = collect_items(&mut device);
    wait_until("窗口内轮次完成", || {
        server
            .turn_details_for_device(DEVICE, 5)
            .first()
            .is_some_and(|d| d.status == "done")
    });
    let _ = device.close(None);
    wait_until("断开后会话与时间窗关闭", || {
        server.active_device_sessions().is_empty() && server.bench_windows().is_empty()
    });
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(
        mem_store.writes.load(Ordering::SeqCst),
        0,
        "时间窗内真实音频轮次绝不能写入长期记忆"
    );
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|e| e.text.contains("测试时间窗已关闭（设备断开）")),
        "设备断开后必须自动关闭时间窗并写事件"
    );

    // C. 重连后打开时间窗，关闭调试开关 -> 自动关闭时间窗
    let _device2 = chat_device(&server, &registry, 1);
    wait_until("设备 2 上线", || {
        !server.active_device_sessions().is_empty()
    });
    server.open_bench_window(DEVICE, 30).unwrap();
    assert_eq!(server.bench_windows().len(), 1);
    server.disable_debug_api();
    assert!(server.bench_windows().is_empty());
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|e| e.text.contains("测试时间窗已关闭（调试开关已关闭）")),
        "关闭调试开关必须立即关闭时间窗"
    );

    // D. 重新开启调试入口并打开时间窗，令牌过期 -> 拒绝访问并自动关闭时间窗
    let port2 = server
        .enable_debug_api(0, token.into(), now, now + 3600, None)
        .unwrap();
    let base2 = format!("http://127.0.0.1:{port2}");
    server.open_bench_window(DEVICE, 30).unwrap();
    assert_eq!(server.bench_windows().len(), 1);
    server.shared.debug_gate.force_expire_for_test();
    let expired_res = client
        .post(format!("{base2}/debug/v1/bench-window"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"device_id": DEVICE, "action": "open", "max_minutes": 10}))
        .send()
        .unwrap();
    assert_eq!(expired_res.status(), 401);
    let _ = base;
    wait_until("令牌失效自动关闭时间窗", || {
        server.bench_windows().is_empty()
    });
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|e| e.text.contains("测试时间窗已关闭（调试令牌已失效）")),
        "令牌失效必须自动关闭时间窗并写事件"
    );

    server.stop();
}

/// VC2 + VC5：数字音频注入仅允许在测试时间窗内使用（窗外 403）、新接口全部经过鉴权与 Origin 校验，
/// 且数字音频注入走完整 VAD/ASR/对话链路但不落盘进 asr-corpus。
#[test]
fn vc2_digital_audio_inject_requires_bench_window_enforces_auth_and_skips_asr_corpus() {
    fn encode_base64_for_test(bytes: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0];
            let b1 = *chunk.get(1).unwrap_or(&0);
            let b2 = *chunk.get(2).unwrap_or(&0);
            out.push(TABLE[(b0 >> 2) as usize] as char);
            out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
            if chunk.len() > 1 {
                out.push(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(TABLE[(b2 & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }

    let temp = tempfile::tempdir().unwrap();
    let corpus = Arc::new(crate::AsrCorpusStore::new(
        temp.path().join("asr-corpus"),
        crate::AsrCorpusConfig {
            enabled: true,
            max_items: 500,
            max_days: 7,
        },
    ));
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("三毫米椴木参数".into())])),
        ..Default::default()
    });
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&["数字对照轮次收到三毫米椴木参数。"])])),
        ..Default::default()
    });
    let conversation = Conversation::new(
        asr.clone(),
        llm,
        Arc::new(FakeTts::default()),
        Arc::new(SimTestTools::default()),
        ConversationSettings::default(),
    );
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            asr_corpus: Some(corpus.clone()),
            idle_timeout: Duration::from_secs(60),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();

    let token = "vc2-digital-token-0123456789abcdef0123456789abcd";
    let now = crate::unix_now();
    let port = server
        .enable_debug_api(0, token.into(), now, now + 3600, None)
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();

    let mut device = chat_device(&server, &registry, 1);
    wait_until("设备上线", || {
        !server.active_device_sessions().is_empty()
    });

    // 合成 0.36 秒 440 Hz 16 kHz PCM
    let samples = tone(16_000, 440.0, 360, 8000.0);
    let pcm_bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let pcm_b64 = encode_base64_for_test(&pcm_bytes);

    // 1. 新接口全部经过鉴权与 Origin 拦截
    assert_eq!(
        client
            .post(format!("{base}/debug/v1/bench-window"))
            .json(&serde_json::json!({"device_id": DEVICE, "action": "open"}))
            .send()
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(format!("{base}/debug/v1/devices/{DEVICE}/inject-audio"))
            .header("Authorization", format!("Bearer {token}"))
            .header("Origin", "http://localhost:5173")
            .json(&serde_json::json!({"pcm_base64": pcm_b64}))
            .send()
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(format!("{base}/debug/v1/devices/{DEVICE}/turns"))
            .send()
            .unwrap()
            .status(),
        401
    );

    // 2. 未打开测试时间窗时调用 inject-audio 必须 403 bench_window_required
    let no_win_res = client
        .post(format!("{base}/debug/v1/devices/{DEVICE}/inject-audio"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"pcm_base64": pcm_b64, "sample_rate": 16000, "frame_ms": 60}))
        .send()
        .unwrap();
    assert_eq!(no_win_res.status(), 403);
    let no_win_body: Value = no_win_res.json().unwrap();
    assert_eq!(no_win_body["error"], "bench_window_required");

    // 3. 打开测试时间窗后允许数字音频对照注入，并走完真实 VAD/ASR/TTS 链路
    let open_res = client
        .post(format!("{base}/debug/v1/bench-window"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"device_id": DEVICE, "action": "open", "max_minutes": 10}))
        .send()
        .unwrap();
    assert_eq!(open_res.status(), 200);

    let inject_res = client
        .post(format!("{base}/debug/v1/devices/{DEVICE}/inject-audio"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({
            "pcm_base64": pcm_b64,
            "sample_rate": 16000,
            "frame_ms": 20,
            "lead_silence_ms": 60,
            "tail_silence_ms": 360
        }))
        .send()
        .unwrap();
    assert_eq!(inject_res.status(), 202);
    let ack: Value = inject_res.json().unwrap();
    let turn_id = ack["turn_id"].as_str().unwrap().to_owned();
    assert_eq!(ack["simulated"], true);

    let items = collect_items(&mut device);
    assert!(items.iter().any(|i| matches!(i, Item::Frame)));
    wait_until("数字对照轮次完成", || {
        server
            .turn_detail(&turn_id)
            .is_some_and(|d| d.status == "done")
    });

    let detail: Value = client
        .get(format!("{base}/debug/v1/turns/{turn_id}"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(detail["simulated"], true);
    assert_eq!(detail["bench_window"], true);
    assert_eq!(detail["digital_audio"], true);
    assert_eq!(detail["injected_text"], "三毫米椴木参数");
    assert!(detail["corpus_turn_id"].is_null());
    assert!(
        !asr.heard.lock().unwrap().is_empty(),
        "数字音频注入必须经过真实 ASR 识别"
    );

    // 设备轮次列表接口可读回该轮
    let turns_res: Value = client
        .get(format!("{base}/debug/v1/devices/{DEVICE}/turns?limit=10"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(turns_res["turns"][0]["turn_id"], turn_id);

    // 数字对照音频绝不进入 asr-corpus
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        corpus.list().unwrap().is_empty(),
        "服务端数字音频对照注入不得写入 asr-corpus"
    );

    server.stop();
}

/// 审查回归：Echo 模式（409）、撤销设备（403）、非回环 Host（DNS 重绑定 403）与同设备重连自动关闭旧会话时间窗。
#[test]
fn vc1_bench_window_enforces_chat_mode_revocation_host_and_session_binding_on_reconnect() {
    // 1. Echo 模式拒绝打开测试时间窗（409 echo_mode）
    let echo_registry = Arc::new(FakeRegistry::default());
    let echo_server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Echo,
            idle_timeout: Duration::from_secs(60),
            ..ServerConfig::default()
        },
        echo_registry.clone(),
    )
    .unwrap();
    let _echo_dev = chat_device(&echo_server, &echo_registry, 1);
    wait_until("Echo 设备上线", || {
        !echo_server.active_device_sessions().is_empty()
    });
    let echo_err = echo_server.open_bench_window(DEVICE, 10).unwrap_err();
    assert_eq!(echo_err.http_status, 409);
    assert_eq!(echo_err.code, "echo_mode");
    echo_server.stop();

    // 2. Chat 模式下校验：DNS 重绑定 Host 403、设备重连关闭旧窗、撤销设备拒绝开窗（403）
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("查询状态".into())])),
        ..Default::default()
    });
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&["重连后的普通轮次回复。"])])),
        ..Default::default()
    });
    let conversation = Conversation::new(
        asr,
        llm,
        Arc::new(FakeTts::default()),
        Arc::new(SimTestTools::default()),
        ConversationSettings::default(),
    );
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            idle_timeout: Duration::from_secs(60),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let token = "vc1-sec-token-0123456789abcdef0123456789abcdef";
    let now = crate::unix_now();
    let port = server
        .enable_debug_api(0, token.into(), now, now + 3600, None)
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();

    // 非回环 Host 头（DNS 重绑定防护）在解析请求体前返回 403 host_not_allowed
    let bad_host_res = client
        .post(format!("{base}/debug/v1/bench-window"))
        .header("Host", "evil.example:18009")
        .header("Authorization", format!("Bearer {token}"))
        .body("not-even-valid-json")
        .send()
        .unwrap();
    assert_eq!(bad_host_res.status(), 403);
    let bad_host_body: Value = bad_host_res.json().unwrap();
    assert_eq!(bad_host_body["error"], "host_not_allowed");

    let _dev1 = chat_device(&server, &registry, 1);
    wait_until("设备 1 上线", || {
        !server.active_device_sessions().is_empty()
    });
    let sess1 = server.active_device_sessions()[0].session_id.clone();
    let win = server.open_bench_window(DEVICE, 10).unwrap();
    assert_eq!(win.session_id, sess1);
    assert_eq!(server.bench_windows().len(), 1);

    // 同一 device_id 建立新连接（设备重连）：自动关闭旧会话时间窗，新会话不再处于时间窗内
    let mut dev2 = chat_device(&server, &registry, 1);
    wait_until("设备重连切换为新 session", || {
        server
            .active_device_sessions()
            .iter()
            .any(|s| s.device_id == DEVICE && s.session_id != sess1)
    });
    assert!(
        server.bench_windows().is_empty(),
        "同设备重连必须关闭旧会话时间窗，绝不延续到新会话"
    );
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|e| e.text.contains("测试时间窗已关闭（设备重连）")),
        "重连关窗应写入事件"
    );

    // 新会话发起普通语音轮次：不应被旧窗口锁为 simulated
    speak_manual(&mut dev2, 1, 500);
    let _ = collect_items(&mut dev2);
    wait_until("新会话第 1 轮完成", || {
        server
            .turn_details_for_device(DEVICE, 5)
            .first()
            .is_some_and(|d| d.status == "done")
    });
    let latest = server.turn_details_for_device(DEVICE, 5).remove(0);
    assert!(!latest.bench_window && !latest.simulated);

    // 撤销设备权限后，不能打开测试时间窗（403 device_revoked）
    registry.set(DEVICE, DeviceStatus::Revoked);
    let revoked_err = server.open_bench_window(DEVICE, 10).unwrap_err();
    assert_eq!(revoked_err.http_status, 403);
    assert_eq!(revoked_err.code, "device_revoked");

    server.stop();
}
