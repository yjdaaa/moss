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
