//! 基本对话轮次：逐句播报、工具往返、退出与再见、打断合并、半句等待与空闲收尾。
use super::*;

#[test]
fn chat_mode_requires_conversation() {
    let config = ServerConfig {
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        ws_port: 0,
        http_port: 0,
        mode: ServerMode::Chat,
        ..ServerConfig::default()
    };
    assert!(
        VoiceServer::start(config, Arc::new(FakeRegistry::default()))
            .err()
            .unwrap()
            .contains("对话模式")
    );
}

#[test]
fn answer_is_spoken_sentence_by_sentence_with_synced_text_and_history() {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            say(&["你好，", "我是 MOSS。", "很高兴见到你。"]),
            say(&["记得。"]),
        ])),
        ..FakeLlm::default()
    };
    let (server, registry, fakes) = start_chat(
        llm,
        &["今天天气怎么样", "你还记得吗"],
        Duration::from_secs(120),
    );
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 1000);
    let items = collect_items(&mut device);
    assert_eq!(
        texts(&items),
        vec![
            "听:今天天气怎么样",
            "tts:start",
            "句:你好，我是 MOSS。",
            "句:很高兴见到你。",
            "tts:stop"
        ]
    );
    // 每句的文字紧挨着这句的第一帧：两句文字之间有音频帧，第一帧在第一句文字之后
    let positions: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| matches!(item, Item::Text(value) if value["state"] == "sentence_start"))
        .map(|(index, _)| index)
        .collect();
    let first_frame = items
        .iter()
        .position(|item| matches!(item, Item::Frame))
        .unwrap();
    assert!(first_frame == positions[0] + 1);
    for pair in positions.windows(2) {
        assert!(
            items[pair[0]..pair[1]]
                .iter()
                .filter(|item| matches!(item, Item::Frame))
                .count()
                >= 4,
            "{pair:?}"
        );
    }
    // 2 句合并后各 300 ms → 24 kHz 下 10 帧
    assert_eq!(
        items
            .iter()
            .filter(|item| matches!(item, Item::Frame))
            .count(),
        10
    );
    assert_eq!(
        *fakes.tts.said.lock().unwrap(),
        vec!["你好，我是 MOSS。", "很高兴见到你。"]
    );
    // 1000 ms 在设备端编成 17 个 60 ms 帧（最后一帧补零），识别收到全部 17 × 960 个样本
    assert_eq!(
        fakes.asr.heard.lock().unwrap()[0],
        17 * 960,
        "识别收到完整的手动录音"
    );

    speak_manual(&mut device, 1, 600);
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["听:你还记得吗", "tts:start", "句:记得。", "tts:stop"]
    );
    let seen = fakes.llm.seen.lock().unwrap();
    let (second, tools) = &seen[1];
    assert_eq!(
        *tools, 2,
        "工具定义随请求发送（含宿主保留的 end_conversation）"
    );
    let roles: Vec<Role> = second.iter().map(|message| message.role).collect();
    assert_eq!(
        roles,
        vec![Role::System, Role::User, Role::Assistant, Role::User]
    );
    assert_eq!(
        second[2].content.as_deref(),
        Some("你好，我是 MOSS。很高兴见到你。")
    );
    let events = server.status().events;
    assert!(
        events
            .iter()
            .any(|event| event.text.contains("识别完成：7 字"))
    );
    assert!(
        events
            .iter()
            .any(|event| event.text.starts_with("首句延迟："))
    );
    assert!(
        !events.iter().any(|event| event.text.contains("天气")),
        "识别文字不写入事件"
    );
}

#[test]
fn tool_results_go_back_to_the_model_and_speak_outcome_skips_it() {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            call("get_time"),
            say(&["现在十二点。"]),
            call("announce"),
        ])),
        ..FakeLlm::default()
    };
    let (server, registry, fakes) =
        start_chat(llm, &["几点了", "播报一下"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["听:几点了", "tts:start", "句:现在十二点。", "tts:stop"]
    );
    {
        let seen = fakes.llm.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        let second = &seen[1].0;
        let call_message = &second[second.len() - 2];
        assert_eq!(call_message.tool_calls[0].function.name, "get_time");
        let result = second.last().unwrap();
        assert_eq!(
            (
                result.role,
                result.content.as_deref(),
                result.tool_call_id.as_deref()
            ),
            (Role::Tool, Some("12:00"), Some("call-get_time"))
        );
    }
    assert_eq!(
        fakes.tools.calls.lock().unwrap()[0],
        ("get_time".to_owned(), DEVICE.to_owned()),
        "工具收到设备身份"
    );

    // “直接播报”的结果不再询问模型
    speak_manual(&mut device, 1, 600);
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["听:播报一下", "tts:start", "句:已经播报", "tts:stop"]
    );
    assert_eq!(fakes.llm.seen.lock().unwrap().len(), 3);
}

#[test]
fn endless_tool_calls_stop_after_five_rounds() {
    let llm = FakeLlm {
        repeat: Some(call("get_time")),
        ..FakeLlm::default()
    };
    let (server, registry, fakes) = start_chat(llm, &["一直查"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let spoken = texts(&collect_items(&mut device));
    let spoken_lines: String = spoken
        .iter()
        .filter_map(|item| item.strip_prefix("句:"))
        .collect();
    assert_eq!(spoken_lines, "工具调用次数超限，换个说法试试");
    assert_eq!(
        fakes.llm.seen.lock().unwrap().len(),
        6,
        "5 轮工具 + 第 6 次回复仍要调用工具时停止"
    );
    assert_eq!(fakes.tools.calls.lock().unwrap().len(), 5);
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.contains("超过 5 轮"))
    );
}

#[test]
fn model_errors_and_failed_sentences_are_handled() {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            Round::Fail("大模型返回 HTTP 401，密钥无效或没有权限"),
            say(&["第一句。", "这句失败。", "第三句。"]),
        ])),
        ..FakeLlm::default()
    };
    let (server, registry, _fakes) =
        start_chat(llm, &["你好", "再试一次"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    // 错误提示按具体故障类别播报大白话原因；超过设备一行（320 px、20 px 字号约 14 个汉字）时在标点处分段随音频显示
    let first_turn = texts(&collect_items(&mut device));
    assert_eq!(first_turn.first().map(String::as_str), Some("听:你好"));
    assert_eq!(first_turn.get(1).map(String::as_str), Some("tts:start"));
    assert_eq!(first_turn.last().map(String::as_str), Some("tts:stop"));
    let first_spoken: String = first_turn
        .iter()
        .filter_map(|item| item.strip_prefix("句:"))
        .collect();
    assert_eq!(first_spoken, "大模型密钥无效或没权限");
    assert!(
        server.status().events.iter().any(|event| event
            .text
            .contains("对话出错：大模型返回 HTTP 401")
            && event.level == crate::EventLevel::Error
            && event.category == crate::DiagCategory::Model),
        "对话失败记为错误级别并带分类"
    );
    // 合成失败的句子跳过，其余照常
    speak_manual(&mut device, 1, 600);
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "听:再试一次",
            "tts:start",
            "句:第一句。",
            "句:第三句。",
            "tts:stop"
        ]
    );
}

/// 退出词前后带称呼或语气词也算整句退出；否定句、带其他内容的话留给大模型。
#[test]
fn exit_phrases_tolerate_address_and_particles_but_not_other_content() {
    for (heard, exits) in [
        ("退下吧。", true),
        ("退下退下", true),
        ("退下退下退下", true),
        ("再见再见！", true),
        ("小智退下退下吧", true),
        ("小智，退下", true),
        ("好了你退下吧", true),
        ("那你先去休息吧", true),
        ("没事了", true),
        // 整链实测：SenseVoice 常把“退下”听成“退一下”
        ("退一下。", true),
        ("小智退一下吧", true),
        ("往后退一下", false),
        ("不要退下", false),
        ("退出登录", false),
        ("帮我退下刀头", false),
    ] {
        let (server, registry, fakes) =
            start_chat(FakeLlm::default(), &[heard], Duration::from_secs(120));
        let mut device = chat_device(&server, &registry, 1);
        speak_manual(&mut device, 1, 600);
        let _ = collect_items(&mut device);
        assert_eq!(fakes.llm.seen.lock().unwrap().is_empty(), exits, "{heard}");
        if exits {
            expect_closed(&mut device);
        }
    }
}

#[test]
fn spoken_errors_explain_category_in_plain_chinese_without_leaking_secrets() {
    use crate::conversation::{DEFAULT_ERROR_REPLY, ErrorStage, spoken_error_reply};
    use moss_voice_providers::DiagCategory;

    let llm_403 = ProviderError::with_category(
        DiagCategory::Auth,
        "大模型返回 HTTP 403，密钥无效或没有权限",
    )
    .detail("服务方原话：Error 403: not subscribed sk-secret-123456 https://api.example.com/v1");
    let spoken = spoken_error_reply(DEFAULT_ERROR_REPLY, ErrorStage::Llm, &llm_403);
    assert_eq!(spoken, "大模型密钥无效或没权限");
    assert!(!spoken.contains("sk-") && !spoken.contains("http") && !spoken.contains("403"));

    let llm_429 = ProviderError::with_category(
        DiagCategory::Quota,
        "大模型返回 HTTP 429，请求太频繁或额度不足",
    );
    assert_eq!(
        spoken_error_reply(DEFAULT_ERROR_REPLY, ErrorStage::Llm, &llm_429),
        "大模型额度用完或请求太快"
    );

    let llm_timeout = ProviderError::with_category(
        DiagCategory::Network,
        "大模型请求超时：对方服务器太久没有回应",
    );
    assert_eq!(
        spoken_error_reply(DEFAULT_ERROR_REPLY, ErrorStage::Llm, &llm_timeout),
        "连接大模型超时"
    );

    let asr_auth = ProviderError::with_category(
        DiagCategory::Auth,
        "语音识别返回 HTTP 401，密钥无效或没有权限",
    );
    assert_eq!(
        spoken_error_reply(DEFAULT_ERROR_REPLY, ErrorStage::Asr, &asr_auth),
        "语音识别密钥无效或没权限"
    );

    // 调用方显式自定义 error_reply 时优先保留自定义话术
    assert_eq!(
        spoken_error_reply("自定义故障提示", ErrorStage::Llm, &llm_403),
        "自定义故障提示"
    );
}

#[test]
fn network_error_replies_are_prewarmed_and_read_failures_never_speak_raw_tool_text() {
    use crate::conversation::{
        ASR_OFFLINE_REPLY, ASR_TIMEOUT_REPLY, DEFAULT_ERROR_REPLY, LLM_OFFLINE_REPLY,
        LLM_TIMEOUT_REPLY, read_failure_speech,
    };
    let conversation = |error_reply: &str| {
        Conversation::new(
            Arc::new(FakeAsr::default()),
            Arc::new(FakeLlm::default()),
            Arc::new(FakeTts::default()),
            Arc::new(FakeTools::default()),
            ConversationSettings {
                error_reply: error_reply.into(),
                ..ConversationSettings::default()
            },
        )
        .fixed_phrases()
    };
    // 断网时云端合成也连不上：断网 / 超时类原因必须启动时就预合成，默认设置下不再念的笼统话术不占缓存
    let phrases = conversation(DEFAULT_ERROR_REPLY);
    for reply in [
        LLM_TIMEOUT_REPLY,
        LLM_OFFLINE_REPLY,
        ASR_TIMEOUT_REPLY,
        ASR_OFFLINE_REPLY,
    ] {
        assert!(phrases.iter().any(|p| p == reply), "{reply}");
    }
    assert!(!phrases.iter().any(|p| p == DEFAULT_ERROR_REPLY));
    assert!(phrases.len() < 32, "固定话术不能挤满 32 条合成缓存");
    // 自定义话术时照旧只预合成自定义的那句
    let custom = conversation("自定义故障提示");
    assert!(custom.iter().any(|p| p == "自定义故障提示"));
    assert!(!custom.iter().any(|p| p == LLM_OFFLINE_REPLY));

    for (raw, expect) in [
        ("工具执行超时", "查询超时，请稍后再问"),
        ("语音设备授权已失效，请重新连接", "设备授权失效，请重新连接"),
        ("服务正在关闭，已拒绝新的工具调用", "MOSS 正在关闭"),
        ("设备未就绪（当前状态: Alarm）", "激光机还没就绪"),
        (
            "设备连接失败 (<device-ip>): Connection refused (os error 61)",
            "激光机没连上，请检查连接",
        ),
        (
            "laser_workflow_tool 调用 moss-host 失败: error sending request",
            "查询失败，详情看桌面端",
        ),
    ] {
        let spoken = read_failure_speech(raw, "查询失败");
        assert_eq!(spoken, expect, "{raw}");
        assert!(!spoken.contains("device-ip") && !spoken.contains("error"));
    }
    assert_eq!(
        read_failure_speech("参数校验失败: thickness_mm 必填", "材料参数查询失败"),
        "材料参数查询失败，详情看桌面端"
    );
}

#[test]
fn llm_failure_after_preamble_and_tool_call_still_speaks_concise_error() {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            Round::Reply {
                deltas: vec!["好，我看下。"],
                calls: vec![ToolCall::new("call-get_time", "get_time", "{}")],
                delay_ms: 0,
            },
            Round::Fail("大模型返回 HTTP 429，请求太频繁或额度不足"),
        ])),
        ..FakeLlm::default()
    };
    let (server, registry, _fakes) = start_chat(llm, &["现在几点"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let spoken: String = texts(&collect_items(&mut device))
        .iter()
        .filter_map(|item| item.strip_prefix("句:"))
        .collect();
    assert_eq!(spoken, "好，我看下。大模型额度用完或请求太快");
}

/// 没命中退出词时，大模型调用宿主保留的 end_conversation：说完告别语后结束对话，同轮其他工具不执行。
#[test]
fn model_can_end_the_conversation_with_the_reserved_tool() {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([Round::Reply {
            deltas: Vec::new(),
            calls: vec![
                ToolCall::new(
                    "call-exit",
                    "end_conversation",
                    r#"{"farewell":"好的，您忙"}"#,
                ),
                ToolCall::new("call-other", "announce", "{}"),
            ],
            delay_ms: 0,
        }])),
        ..FakeLlm::default()
    };
    let (server, registry, fakes) = start_chat(llm, &["我先去忙别的了"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "听:我先去忙别的了",
            "tts:start",
            "句:好的，您忙",
            "tts:stop"
        ]
    );
    expect_closed(&mut device);
    assert_eq!(fakes.llm.seen.lock().unwrap().len(), 1, "不再回问大模型");
    assert!(
        fakes.tools.calls.lock().unwrap().is_empty(),
        "同轮其他工具不执行"
    );
    drop(server);
}

/// 官方固件收到 tts start 后要经主任务调度才切到说话状态，此前到达的音频帧会被丢弃：
/// 首帧音频不得早于保护间隔。
#[test]
fn first_audio_frame_waits_for_the_device_to_enter_speaking() {
    let (server, registry, _fakes) =
        start_chat(FakeLlm::default(), &["你好"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let mut started = None;
    let gap = loop {
        match device.read().unwrap() {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                if value["type"] == "tts" && value["state"] == "start" {
                    started = Some(Instant::now());
                }
            }
            Message::Binary(_) => break started.expect("音频帧先于 tts start").elapsed(),
            _ => {}
        }
    };
    assert!(
        gap >= Duration::from_millis(150),
        "首帧距 tts start 仅 {gap:?}"
    );
    drop(server);
}

#[test]
fn exit_word_says_goodbye_and_closes() {
    let (server, registry, fakes) =
        start_chat(FakeLlm::default(), &["再见！"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let goodbye = ConversationSettings::default().goodbye;
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "听:再见！".to_owned(),
            "tts:start".into(),
            format!("句:{goodbye}"),
            "tts:stop".into()
        ]
    );
    expect_closed(&mut device);
    assert!(
        fakes.llm.seen.lock().unwrap().is_empty(),
        "退出词不经过大模型"
    );
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.contains("用户说了退出词"))
    );
}

/// “退一下”= 道别后回到待命（2026-11-15 用户确认）：这一场对话的语音通道关掉，设备回到待命、
/// 重新监听唤醒词；再喊唤醒词（设备重新连上来）就是新的一场对话，照常回答。
#[test]
fn saying_tui_yi_xia_goes_to_standby_and_the_wake_word_starts_a_new_conversation() {
    let (server, registry, fakes) = start_chat(
        FakeLlm::default(),
        &["退一下。", "现在几点"],
        Duration::from_secs(120),
    );
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let goodbye = ConversationSettings::default().goodbye;
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "听:退一下。".to_owned(),
            "tts:start".into(),
            format!("句:{goodbye}"),
            "tts:stop".into()
        ]
    );
    // 说完再见断开：固件收到后回到待命（不再“聆听中”），只等唤醒词
    expect_closed(&mut device);
    assert!(
        fakes.llm.seen.lock().unwrap().is_empty(),
        "退出词不经过大模型"
    );
    // 喊唤醒词：设备重新连上来开始新的一场，正常对话
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["听:现在几点", "tts:start", "句:好的。", "tts:stop"]
    );
    let seen = fakes.llm.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(
        !seen[0].0.iter().any(|message| message
            .content
            .as_deref()
            .is_some_and(|text| text.contains("退一下"))),
        "新的一场不带上一场的对话"
    );
}

#[test]
fn abort_cancels_turn_and_keeps_partial_history() {
    let slow = Round::Reply {
        deltas: vec!["第一句。", "第二句。", "第三句。", "第四句。", "第五句。"],
        calls: Vec::new(),
        delay_ms: 400,
    };
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([slow, say(&["好。"])])),
        ..FakeLlm::default()
    };
    let (server, registry, fakes) =
        start_chat(llm, &["讲个长故事", "算了"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 2);
    speak_manual(&mut device, 2, 600);
    // 等到第一帧音频后打断
    loop {
        if let Message::Binary(_) = device.read().unwrap() {
            break;
        }
    }
    send_json(
        &mut device,
        r#"{"type":"abort","reason":"wake_word_detected"}"#,
    );
    loop {
        if let Message::Text(text) = device.read().unwrap()
            && text.as_str().contains(r#""state":"stop""#)
        {
            break;
        }
    }
    device
        .get_mut()
        .set_read_timeout(Some(Duration::from_millis(1500)))
        .unwrap();
    match device.read() {
        Ok(Message::Binary(_)) => panic!("打断后仍在发送音频"),
        Ok(Message::Text(text)) => panic!("打断后仍有消息：{text}"),
        _ => {}
    }
    device
        .get_mut()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert!(
        fakes.tts.said.lock().unwrap().len() < 5,
        "打断后不再合成后面的句子"
    );
    speak_manual(&mut device, 2, 600);
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["听:算了", "tts:start", "句:好。", "tts:stop"]
    );
    let seen = fakes.llm.seen.lock().unwrap();
    let history = &seen[1].0;
    assert_eq!(history[1].content.as_deref(), Some("讲个长故事"));
    assert_eq!(history[2].role, Role::Assistant);
    // 刚收到第一帧就打断：设备里还缓冲着那几帧、用户几乎什么都没听到（固件收到 abort 立即停播），
    // 已发出的“第一句”记为没说出口的，后面还没合成的句子不出现（n100）
    let reply = history[2].content.as_deref().unwrap();
    let (heard, unsaid) = reply.split_once("【被打断，没说出口的：").expect(reply);
    assert!(!heard.contains("第一句"), "{reply}");
    assert!(unsaid.contains("句。"), "{reply}");
    assert!(!reply.contains("第三句"), "{reply}");
}

/// 话说一半停顿被判停后，在小智开口前接着说（官方服务端思考期间照常收音）：
/// 放弃还没开口的回答，前半句与后半句拼成一段重新识别，前半句不单独进历史。
#[test]
fn speaking_on_while_thinking_merges_both_halves_into_one_turn() {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            Round::Reply {
                deltas: vec!["只听到半句的回答。"],
                calls: Vec::new(),
                delay_ms: 3000,
            },
            say(&["完整的回答。"]),
        ])),
        ..FakeLlm::default()
    };
    let (server, registry, fakes) = start_chat(
        llm,
        &["帮我把功率", "帮我把功率调到八十"],
        Duration::from_secs(120),
    );
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut first = tone(UP, 300.0, 600, 8000.0);
    first.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&first));
    assert_eq!(read_json(&mut device)["text"], "帮我把功率");
    // 大模型还在想：设备仍在聆听状态并继续上传，用户接着把话说完
    let mut second = tone(UP, 300.0, 600, 8000.0);
    second.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&second));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "听:帮我把功率调到八十",
            "tts:start",
            "句:完整的回答。",
            "tts:stop"
        ]
    );
    let heard = fakes.asr.heard.lock().unwrap().clone();
    assert_eq!(heard.len(), 2);
    assert!(
        heard[1] > heard[0] + UP as usize / 2,
        "第二次识别的是前半句 + 后半句：{heard:?}"
    );
    let seen = fakes.llm.seen.lock().unwrap();
    let users: Vec<_> = seen
        .last()
        .unwrap()
        .0
        .iter()
        .filter(|message| message.role == Role::User)
        .filter_map(|message| message.content.clone())
        .collect();
    assert_eq!(users, vec!["帮我把功率调到八十"], "前半句不单独进历史");
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.starts_with("思考中用户接着说"))
    );
}

/// 抢跑：静音 250 ms 就开始识别与请求大模型，但这句话明显没说完时先扣住——不开口、不执行工具；
/// 用户接着说完，合并重来，被放弃的那一轮里大模型发起的工具调用一次都没执行。
#[test]
fn unfinished_sentence_is_held_and_its_tool_call_never_runs_when_the_user_continues() {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([call("get_time"), say(&["调好了。"])])),
        ..FakeLlm::default()
    };
    let (server, registry, fakes) = start_chat(
        llm,
        &["帮我把功率调到", "帮我把功率调到八十"],
        Duration::from_secs(120),
    );
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut half = tone(UP, 300.0, 600, 8000.0);
    half.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&half));
    assert_eq!(read_json(&mut device)["text"], "帮我把功率调到");
    // 大模型瞬间就要调工具，但这句话停在“调到”上：闸门没放行
    std::thread::sleep(Duration::from_millis(300));
    assert!(fakes.tools.calls.lock().unwrap().is_empty());
    send_audio(&mut device, 1, &uplink(&half));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "听:帮我把功率调到八十",
            "tts:start",
            "句:调好了。",
            "tts:stop"
        ]
    );
    assert!(
        fakes.tools.calls.lock().unwrap().is_empty(),
        "被放弃那一轮的工具调用没有执行"
    );
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.contains("像是还没说完"))
    );
}

/// 抢跑扣住时大模型调用了保留工具 `end_conversation`：在闸门放行前不能发告别语音或结束会话；
/// 用户紧接着说完后，被放弃那一轮的 `end_conversation` 不会生效。
#[test]
fn held_turn_gates_end_conversation_until_release() {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            call("end_conversation"),
            say(&["灯已关。"]),
        ])),
        ..FakeLlm::default()
    };
    let (server, registry, fakes) = start_chat(
        llm,
        &["不用了帮我把", "不用了帮我把灯关掉", "现在几点"],
        Duration::from_secs(120),
    );
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut half = tone(UP, 300.0, 600, 8000.0);
    half.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&half));
    assert_eq!(read_json(&mut device)["text"], "不用了帮我把");
    // 大模型立刻返回了 end_conversation，但句子停在“把”上：闸门未放行前绝不合成告别语也不关会话
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        fakes.tts.said.lock().unwrap().is_empty(),
        "未放行前不能由 end_conversation 触发告别语合成"
    );
    send_audio(&mut device, 1, &uplink(&half));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "听:不用了帮我把灯关掉",
            "tts:start",
            "句:灯已关。",
            "tts:stop"
        ]
    );
    assert_eq!(fakes.tts.said.lock().unwrap().as_slice(), ["灯已关。"]);
    // 会话没有被上一轮作废的 end_conversation 关闭，还能继续下一轮对话
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut next = tone(UP, 300.0, 600, 8000.0);
    next.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&next));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["听:现在几点", "tts:start", "句:好的。", "tts:stop"]
    );
}

/// 明显没说完但用户确实不说了：到点照常回应，工具只执行一次；明显说完的句子不额外等待。
#[test]
fn held_turn_is_released_on_time_and_complete_sentences_are_not_held() {
    for (heard, min_ms, max_ms) in [("帮我把功率调到", 900, 2500), ("现在几点了", 0, 700)]
    {
        let llm = FakeLlm {
            rounds: Mutex::new(VecDeque::from([call("get_time"), say(&["十二点。"])])),
            ..FakeLlm::default()
        };
        let (server, registry, fakes) = start_chat(llm, &[heard], Duration::from_secs(120));
        let mut device = chat_device(&server, &registry, 1);
        send_json(
            &mut device,
            r#"{"type":"listen","state":"start","mode":"auto"}"#,
        );
        let mut pcm = tone(UP, 300.0, 600, 8000.0);
        pcm.extend(silence(UP, 600));
        send_audio(&mut device, 1, &uplink(&pcm));
        assert_eq!(read_json(&mut device)["text"], heard);
        let heard_at = Instant::now();
        loop {
            if let Message::Text(text) = device.read().unwrap()
                && text.as_str().contains("\"start\"")
                && text.as_str().contains("\"tts\"")
            {
                break;
            }
        }
        let waited = heard_at.elapsed().as_millis();
        assert!(
            (min_ms..=max_ms).contains(&waited),
            "{heard}：识别完到开口 {waited} ms"
        );
        let _ = collect_items(&mut device);
        assert_eq!(fakes.tools.calls.lock().unwrap().len(), 1, "{heard}");
        drop(server);
    }
}

/// 这一轮已经开始执行工具时，用户接着说话不会让整轮重来（工具可能有副作用）。
#[test]
fn speaking_on_while_a_tool_is_running_does_not_restart_the_turn() {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            call("get_time"),
            Round::Reply {
                deltas: vec!["现在十二点。"],
                calls: Vec::new(),
                delay_ms: 1500,
            },
        ])),
        ..FakeLlm::default()
    };
    let (server, registry, fakes) = start_chat(llm, &["几点了"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut first = tone(UP, 300.0, 600, 8000.0);
    first.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&first));
    assert_eq!(read_json(&mut device)["text"], "几点了");
    std::thread::sleep(Duration::from_millis(300));
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 600, 8000.0)));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["tts:start", "句:现在十二点。", "tts:stop"]
    );
    assert_eq!(fakes.asr.heard.lock().unwrap().len(), 1, "没有重新识别");
    assert_eq!(fakes.tools.calls.lock().unwrap().len(), 1, "工具只执行一次");
    drop(server);
}

#[test]
fn empty_transcript_keeps_listening_in_auto_mode() {
    let (server, registry, fakes) =
        start_chat(FakeLlm::default(), &["", "开始"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut pcm = tone(UP, 300.0, 600, 8000.0);
    pcm.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&pcm));
    // 第一次识别为空：不回复，继续拾音；不发 listen start 再说一次
    std::thread::sleep(Duration::from_millis(300));
    send_audio(&mut device, 1, &uplink(&pcm));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["听:开始", "tts:start", "句:好的。", "tts:stop"]
    );
    assert_eq!(fakes.asr.heard.lock().unwrap().len(), 2);
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.contains("没有识别出文字"))
    );
}

#[test]
fn wake_word_gets_cached_greeting_then_listens() {
    let (server, registry, fakes) =
        start_chat(FakeLlm::default(), &["开灯"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    for round in 0..2 {
        // 固件：检测到唤醒词后先发 detect，紧接着发 listen start（自动模式）
        send_json(
            &mut device,
            r#"{"type":"listen","state":"detect","text":"你好小智"}"#,
        );
        send_json(
            &mut device,
            r#"{"type":"listen","state":"start","mode":"auto"}"#,
        );
        assert_eq!(
            texts(&collect_items(&mut device)),
            vec!["tts:start", "句:我在", "tts:stop"],
            "第 {round} 次唤醒"
        );
    }
    assert_eq!(
        fakes
            .tts
            .said
            .lock()
            .unwrap()
            .iter()
            .filter(|text| *text == "我在")
            .count(),
        1,
        "问候语只合成一次"
    );
    // 问候播完后已在拾音：直接说话即可
    let mut pcm = tone(UP, 300.0, 600, 8000.0);
    pcm.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&pcm));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["听:开灯", "tts:start", "句:好的。", "tts:stop"]
    );
}

#[test]
fn idle_session_says_goodbye_then_closes() {
    let (server, registry, _fakes) =
        start_chat(FakeLlm::default(), &[], Duration::from_millis(800));
    let mut device = chat_device(&server, &registry, 1);
    let started = Instant::now();
    let goodbye = ConversationSettings::default().goodbye;
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "tts:start".to_owned(),
            format!("句:{goodbye}"),
            "tts:stop".into()
        ]
    );
    assert!(started.elapsed() >= Duration::from_millis(700));
    expect_closed(&mut device);
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.contains("没有对话，已说再见"))
    );
}
