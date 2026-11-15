//! n101：“旁人说话不接话”在真实大模型上的误判率（不在常规测试中运行，会调用云端接口、产生费用）。
//!
//! ```text
//! MOSS_EVAL_LLM_URL=<OpenAI 兼容地址> MOSS_EVAL_LLM_KEY=<token> MOSS_EVAL_LLM_MODEL=<模型> \
//! cargo test -p moss-voice-server real_llm_addressee_eval -- --ignored --nocapture
//! ```
//! 每句都放在“刚回答完一轮”的对话里，看大模型是回应还是调用 `stay_silent`。两类错误分开数：
//! 对小智说的却不回应（更伤，用户以为它没听见），以及旁人的话却接了。
use super::*;
use crate::conversation::{ToolLatch, TurnEvent, TurnGate, TurnInput, spawn_turn_gated};

/// （这句话，是不是对小智说的）
const LABELLED: &[(&str, bool)] = &[
    ("帮我看看激光机现在什么状态", true),
    ("功率调到百分之六十", true),
    ("现在几点了", true),
    ("你好", true),
    ("再说一遍", true),
    ("刚才说到哪了", true),
    ("那椴木要切几遍", true),
    ("嗯，开始吧", true),
    ("对，就这个", true),
    ("好的谢谢", true),
    ("讲个笑话", true),
    ("这个参数会不会太焦", true),
    ("你觉得呢", true),
    ("那换成亚克力呢", true),
    ("小智，帮我把速度调慢一点", true),
    ("老婆，把螺丝刀递给我", false),
    ("喂，你好，我现在不方便，晚点回你", false),
    ("宝贝，作业写完了吗", false),
    ("妈，饭做好了没", false),
    ("这个快递就放门口吧", false),
    ("你先去吃饭吧，我等会儿", false),
    ("观众朋友们大家好，欢迎收看今天的新闻联播", false),
    ("张三，你明天几点来厂里", false),
    ("把那个箱子搬过来", false),
    ("师傅，这个线是不是接反了", false),
];

#[test]
#[ignore]
fn real_llm_addressee_eval() {
    let (Ok(url), Ok(key), Ok(model)) = (
        std::env::var("MOSS_EVAL_LLM_URL"),
        std::env::var("MOSS_EVAL_LLM_KEY"),
        std::env::var("MOSS_EVAL_LLM_MODEL"),
    ) else {
        panic!("需要 MOSS_EVAL_LLM_URL / MOSS_EVAL_LLM_KEY / MOSS_EVAL_LLM_MODEL");
    };
    let llm = Arc::new(
        moss_voice_providers::openai::OpenAiLlm::new(&url, &key, &model)
            .unwrap()
            .with_fast_reply(true),
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let history = vec![
        ChatMessage::user("帮我看看激光机现在什么状态"),
        ChatMessage::assistant("激光机现在空闲，已经连上了。要我先帮你对一下焦吗？"),
    ];
    let (mut ignored_me, mut answered_others) = (Vec::new(), Vec::new());
    for (text, to_me) in LABELLED {
        let asr = Arc::new(FakeAsr {
            replies: Mutex::new(VecDeque::from([Ok((*text).to_owned())])),
            ..Default::default()
        });
        let conversation = Arc::new(Conversation::new(
            asr,
            llm.clone(),
            Arc::new(FakeTts::default()),
            Arc::new(FakeTools::default()),
            ConversationSettings::default(),
        ));
        let (_release, gate) = tokio::sync::watch::channel(true);
        let silent = runtime.block_on(async {
            let (handle, mut events) = spawn_turn_gated(
                conversation,
                ToolContext {
                    device_id: "eval".into(),
                    session_id: "eval".into(),
                },
                TurnInput::Utterance {
                    pcm: vec![0; 1600],
                    sample_rate: UP,
                    stream: None,
                },
                history.clone(),
                None,
                None,
                Some(TurnGate {
                    release: gate,
                    tool_latch: Arc::new(ToolLatch::default()),
                    speculative: false,
                    hold_llm: false,
                    fresh_history: Arc::default(),
                    allow_silent: true,
                    allow_background: false,
                    command_hint: Arc::default(),
                }),
            );
            let mut silent = None;
            while let Some(event) = events.recv().await {
                match event {
                    TurnEvent::NotForMe => silent = Some(true),
                    TurnEvent::Speech { .. } | TurnEvent::TextReply(_) => {
                        silent.get_or_insert(false);
                    }
                    TurnEvent::Failed(error) => panic!("大模型请求失败：{}", error.message),
                    _ => {}
                }
            }
            let _ = handle.await;
            silent.unwrap_or(false)
        });
        println!("{} {text}", if silent { "不回应" } else { "回应  " });
        match (to_me, silent) {
            (true, true) => ignored_me.push(*text),
            (false, false) => answered_others.push(*text),
            _ => {}
        }
    }
    let mine = LABELLED.iter().filter(|(_, to_me)| *to_me).count();
    let others = LABELLED.len() - mine;
    println!(
        "对小智说的却不回应：{}/{mine} {ignored_me:?}\n旁人的话却接了：{}/{others} {answered_others:?}",
        ignored_me.len(),
        answered_others.len()
    );
}
