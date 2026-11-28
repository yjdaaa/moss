//! 对话模式（NV3）：一段话 → 识别 → 大模型（流式，可调用工具）→ 切句 → 逐句合成 → 交给会话播放。
//!
//! 一轮对话（turn）在独立的 tokio 任务中运行，通过 [`TurnEvent`] 把识别结果、每句音频与结束信息送回会话；
//! 会话随时可以中止任务（设备打断）。规则取 xiaozhi-esp32-server `core/connection.py`（`7a260ee`）：
//! - 多个工具调用并行执行，各 30 秒超时；结果回填后再问模型，最多 5 轮；
//! - 工具可要求“直接播报”某段话而不再问模型；
//! - 退出词直接说再见并结束会话；历史按长度预算截断（esp32-server 没有上限）。
//!
//! 工具来源（NV4）：服务级的 [`ToolExecutor`]（MOSS 工具，见 [`crate::tools`]）与每台设备自带的 MCP 工具
//! （[`DeviceTools`]，经会话的 WebSocket 中继）。
//!
//! 流式厂商（NV7）：配置了 [`StreamingAsr`] 时会话在用户开始说话时就建立识别连接、边说边送音频，说完只等最后
//! 结果（失败时退回整段识别一次）；合成用 [`Tts::synthesize_stream`]，每句的第一段音频一到就交给会话播放。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use moss_voice_providers::text::{Segmenter, ThinkFilter, emotion_of, strip_think};
use moss_voice_providers::{
    Asr, AsrStream, BoxFuture, ChatMessage, Llm, Pcm, ProviderError, StreamingAsr, ToolCall,
    ToolSpec, Tts, compact_stale_tool_results, truncate_history,
};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio::time::Instant;

macro_rules! plain_head {
    () => {
        "你是 MOSS，运行在用户电脑上的激光工坊中文语音助手，通过小智设备与用户对话。\
说话像在工坊旁边搭手的熟人，自然松弛、直接说正事，别说“请说指令”“请下达指令”“随时待命”“有什么可以帮您”这类客服对讲机套话。"
    };
}

macro_rules! immutable_safety_rules {
    () => {
        "回答要简短、口语化，一般不超过三句话；不要使用 Markdown、列表和代码块，句子中间不要用表情符号，因为你的回答会被直接朗读出来。\
像面对面聊天那样说话：开口就直接回答结论，不要先复述问题、说客套话，也不要硬塞“好，”“嗯，”拆成两三字的碎顿；回答只有一句短话时一口气自然说完，有两句以上或句子较长时，第一小句在六到十二个字处自然停顿断开，方便先开口播报；数字和单位写成能直接念出来的样子，不用括号、斜杠和英文缩写；要列举几样东西时用逗号连成一句话；一次只问一个问题；\
内容比较多时先说最要紧的一两句，再问要不要接着听。\
用户的话是语音识别出来的，可能有同音错字或不完整，按最可能的意思理解，不要纠正用户的用词；确实听不明白就简短地请他再说一遍。\
可以在整段回答的最开头放一个表情（😊 开心、🤔 思考、😮 惊讶、😢 难过），它只用来切换小智屏幕上的表情、不会被读出来；没有合适的就不放。\
不知道的事情就直接说不知道，不要编造。\
PVC 与含氯材料一律拒绝加工，用户只说塑料或软卡时先问清具体材质再生成预览；生成预览后提醒用户核对机台上的实物材料与厚度。\
需要让激光机运动或出光（开始加工、发送任务、续雕、设定原点、走边框等）时：先复述材料、厚度、内容与功率速度次数，\
问用户“确认开始吗”，只有用户在下一句明确说“确认”或“开始”后，才能带 confirmed=true 调用工具；不要替用户确认，也不要猜参数。\
走边框只问“确认走边框吗”，一次确认只做用户确认的那一个动作，走完边框要开始加工得再问一次。\
用户说停、急停、取消时立即调用对应的停止工具，不需要确认。"
    };
}

pub(crate) const PLAIN_HEAD: &str = plain_head!();

/// 默认系统提示（NV4 接入雕刻工具时补充工具使用规则）。
pub const DEFAULT_SYSTEM_PROMPT: &str = concat!(plain_head!(), immutable_safety_rules!());

/// 不可被任何风格预设或自定义补充提示覆盖的底层语音与硬件安全铁律。
pub const IMMUTABLE_SAFETY_RULES: &str = immutable_safety_rules!();

/// 组合「内置角色开头 + 用户工坊背景补充 + 锁死在末尾的底层安全铁律」（只含内置角色；自建角色见 [`crate::persona`]）。
pub fn compose_persona_system_prompt(style: &str, custom_prompt: &str) -> String {
    crate::persona::PersonaConfig {
        active: style.trim().to_owned(),
        background: custom_prompt.to_owned(),
        custom: Vec::new(),
        risk_notes: false,
    }
    .system_prompt()
}

pub fn local_clock_line() -> String {
    use chrono::{Datelike, Local, Timelike, Weekday};
    let now = Local::now();
    let weekday = match now.weekday() {
        Weekday::Mon => "星期一",
        Weekday::Tue => "星期二",
        Weekday::Wed => "星期三",
        Weekday::Thu => "星期四",
        Weekday::Fri => "星期五",
        Weekday::Sat => "星期六",
        Weekday::Sun => "星期日",
    };
    format!(
        "当前本机时间：{:04}-{:02}-{:02}（{}）{:02}:{:02}。",
        now.year(),
        now.month(),
        now.day(),
        weekday,
        now.hour(),
        now.minute()
    )
}

pub fn build_turn_system_prompt(
    base_prompt: &str,
    include_clock: bool,
    ongoing_dialogue: bool,
) -> String {
    let mut out = String::with_capacity(base_prompt.len() + 160);
    out.push_str(base_prompt);
    if include_clock {
        out.push('\n');
        out.push_str(&local_clock_line());
    }
    if ongoing_dialogue {
        out.push_str("\n对话连贯约束：当前为同一场对话的后续轮次，除用户主动询问身份或称呼外，请直接切入回答，不要每句话开头重复机械喊出用户称呼。");
    }
    out
}

/// 0ms 本地 ASR 同音错词纠偏：急停/停止短令原样保护，其余按词长优先顺序替换。
pub fn apply_asr_corrections(raw: &str, rules: &[(String, String)]) -> String {
    if rules.is_empty() || raw.is_empty() || crate::fast::is_stop(raw) {
        return raw.to_owned();
    }
    let mut text = raw.to_owned();
    for (wrong, right) in rules {
        if !wrong.is_empty() && wrong != right && text.contains(wrong.as_str()) {
            text = text.replace(wrong.as_str(), right.as_str());
        }
    }
    text
}

/// 工具调用的上下文（NV4 用于构造 `CallerContext::Voice` 与审计）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolContext {
    pub device_id: String,
    pub session_id: String,
}

/// 工具执行结果的处理方式（esp32-server 的 `Action`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutcome {
    /// 结果回填给模型，由模型组织回答（`REQLLM`）
    Reply(String),
    /// 直接朗读这段话，不再询问模型（`RESPONSE`）；同时作为工具结果记入历史
    Speak(String),
    /// Trusted read-only music selection; only the device session may open it.
    PlayMusic { track_id: String, title: String },
}

/// 工具执行方。实现须线程安全；单次调用由对话层加 30 秒超时。
pub trait ToolExecutor: Send + Sync {
    fn tools(&self) -> Vec<ToolSpec>;
    /// 用户在第三方 MCP 设置里明确设为“允许”（免批准、确认过只读风险）的工具（n101）：
    /// 这类工具慢的时候可以像联网搜索一样转到后台查。默认不是；不根据服务端自己的 annotations 判断。
    fn trusted_read_only(&self, _name: &str) -> bool {
        false
    }
    fn call<'a>(
        &'a self,
        context: &'a ToolContext,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, ToolOutcome>;
    /// 固定路由读取完整结果，不能使用为模型截断的 JSON 详情。
    fn call_raw<'a>(
        &'a self,
        context: &'a ToolContext,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, crate::tools::ToolHostResult> {
        Box::pin(async move {
            let text = match self.call(context, name, arguments).await {
                ToolOutcome::Reply(s) | ToolOutcome::Speak(s) => s,
                ToolOutcome::PlayMusic { .. } => {
                    return crate::tools::ToolHostResult {
                        success: false,
                        result: "音乐必须通过设备会话播放".into(),
                        detail: None,
                    };
                }
            };
            let value: Value = serde_json::from_str(&text).unwrap_or_default();
            crate::tools::ToolHostResult {
                success: value["success"].as_bool().unwrap_or(false),
                result: value["result"]
                    .as_str()
                    .unwrap_or("工具未返回可验证的结果")
                    .to_owned(),
                detail: value["detail"].as_str().map(str::to_owned),
            }
        })
    }
    /// 只读读取最近的加工任务（加工结束播报轮询用）。
    /// 默认不支持：宿主没有提供“不计入工具调用统计”的只读通道时，不做任何轮询。
    fn recent_workflows<'a>(&'a self, _context: &'a ToolContext) -> BoxFuture<'a, Option<Value>> {
        Box::pin(async { None })
    }
    /// 只读查询当前是否有正在运行或未决的激光加工任务（VA2 #2 模拟轮次停止命令门禁专用）。
    /// 默认回退读取 `recent_workflows`，解析其中 `sending` / `active_stop_targets`。
    fn has_active_laser_job<'a>(&'a self, context: &'a ToolContext) -> BoxFuture<'a, Option<bool>> {
        Box::pin(async move {
            let recent = self.recent_workflows(context).await?;
            Some(crate::sim_guard::has_active_job_in_recent_workflows(
                &recent,
            ))
        })
    }
    /// n70 只读读取某个加工任务的刀路预览原图（开始雕刻后推到小智屏幕）。默认不支持。
    fn workflow_preview<'a>(
        &'a self,
        _context: &'a ToolContext,
        _workflow_id: &'a str,
    ) -> BoxFuture<'a, Option<Vec<u8>>> {
        Box::pin(async { None })
    }
}

/// 不提供任何工具（NV3 默认）。
pub struct NoTools;

impl ToolExecutor for NoTools {
    fn tools(&self) -> Vec<ToolSpec> {
        Vec::new()
    }

    fn call<'a>(
        &'a self,
        _: &'a ToolContext,
        name: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move { ToolOutcome::Reply(format!("工具 {name} 不存在")) })
    }
}

/// 语音急停快通道成功后的固定反馈话术
pub const STOP_OK_SPEECH: &str = "已发送停止请求，请确认设备状态。";
/// 语音急停快通道失败后的固定反馈话术
pub const STOP_ERR_SPEECH: &str = "停止请求发送失败，请使用设备急停按钮并检查连接。";
/// 慢速外部信息查询工具（天气/新闻/联网搜索/RAGFlow 知识库）触发时的过渡垫音
pub const SLOW_QUERY_FILLER: &str = "稍等，正在查询。";
/// 慢速 AI 生图/刀路预览生成工具触发时的过渡垫音（仅预览生成，不涉及硬件动作）。
/// 这类工具一定要几秒，拿到调用就先应一声（n113），所以写成口头应答而不是“稍等”。
pub const SLOW_GENERATE_FILLER: &str = "好，我生成下预览。";
/// n72 只读检查机器连接（串口 / 网络探测要几秒）时的过渡垫音（n113 起拿到调用就说）
pub const SLOW_CHECK_FILLER: &str = "好，我看下机器。";
/// 小智拍照看图（设备 MCP 的 `self.camera.take_photo`：拍照、上传、视觉模型回答，要几秒）时的过渡垫音（n113）
pub const SLOW_LOOK_FILLER: &str = "好，我看一眼。";
/// 小智固件的拍照工具在大模型工具清单里的名字（[`DeviceTools`] 加 `device__` 前缀、`.` 换成 `_`）
const DEVICE_CAMERA_TOOL: &str = "device__self_camera_take_photo";

/// 命令词纠正只用于这么短的识别结果（n101）：停止 / 退出命令本来就短，长句以识别为准。
const MAX_COMMAND_FUSION_CHARS: usize = 4;
/// 识别结果像被听错的命令、命令词结果还没到时最多等这么久（n101）；尾音送完后结果不会再来，
/// 最多只等到那时（[`TurnGate::command_hint_until`]，n115）
const COMMAND_HINT_WAIT: Duration = Duration::from_millis(500);
/// 尾音最后一段送进判停流后，命令词结果经辅助程序回来的余量
const COMMAND_HINT_IPC_MARGIN: Duration = Duration::from_millis(100);
/// 退出类“相同的字”只认这几个（n115）：“一 / 再”太常见，“第一个”“再来一次”不能因此等命令词结果；
/// 实测的听错说法（跪下、看一下、问一下、待见……）都含“下 / 见”
const EXIT_HINT_CHARS: &str = "退下见";

/// 识别结果像不像被听错的命令词（n101）：没识别出字；或者不超过 4 个字、且和这类命令词有相同的字
/// （噪声里实测的错法：“跪下 / 会一下 / 不理下 / 看一下”之于“退下 / 退一下”，“待见”之于“再见”，“天下”之于“停下”）；
/// 停止类再放过“停”的单个同音 / 近音字（“听 / 挺”）。“好的”“挺好”这类说法与命令词没有相同的字，命令词模型误报时也不会被改成命令。
/// 停止类会走急停，所以带否定或“暂 / 等”的说法（“别停”“不要停”“暂停”“等一下”）一律不纠正（n114）。
pub(crate) fn command_hint_fits(
    kind: moss_voice_providers::local::CommandKeyword,
    normalized: &str,
) -> bool {
    let length = normalized.chars().count();
    if length == 0 {
        return true;
    }
    if length > MAX_COMMAND_FUSION_CHARS {
        return false;
    }
    if kind == moss_voice_providers::local::CommandKeyword::Stop
        && normalized.chars().any(|c| "别不没勿莫甭暂等".contains(c))
    {
        return false;
    }
    // 单个字只放过“停”的同音 / 近音字：“好 / 嗯 / 对”这类单字回答不能被命令词模型误报改成急停
    if kind == moss_voice_providers::local::CommandKeyword::Stop
        && length == 1
        && normalized.chars().all(|c| "听挺亭庭婷廷艇定顶".contains(c))
    {
        return true;
    }
    moss_voice_providers::local::COMMAND_KEYWORDS
        .iter()
        .filter(|(_, _, of)| *of == kind)
        .any(|(_, label, _)| {
            label.chars().any(|c| {
                normalized.contains(c)
                    && (kind != moss_voice_providers::local::CommandKeyword::Exit
                        || EXIT_HINT_CHARS.contains(c))
            })
        })
}

/// 命令词模型命中后对识别文本做融合改判（供会话与全链路连贯试跑共用同一规则）：
/// 已是明确停止 / 退出意图时不改；仅在识别文本为空或 ≤4 字且符合同字 / 近音条件时改判为“停”或“退下”。
pub fn fuse_command_keyword_hint(
    kind: moss_voice_providers::local::CommandKeyword,
    text: &str,
) -> Option<&'static str> {
    if crate::fast::is_stop(text) {
        return None;
    }
    if is_default_exit_phrase(text) {
        return None;
    }
    let normalized = crate::fast::normalized(text);
    if !command_hint_fits(kind, &normalized) {
        return None;
    }
    Some(match kind {
        moss_voice_providers::local::CommandKeyword::Stop => "停",
        moss_voice_providers::local::CommandKeyword::Exit => "退下",
    })
}

/// 判断识别文本是否命中默认整句退出短令（如“退下 / 再见 / 退一下”，供会话与连贯试跑共用快路径判断）。
pub fn is_default_exit_phrase(text: &str) -> bool {
    let defaults = ConversationSettings::default();
    is_exit_phrase(text, &defaults.exit_words)
}

fn is_exit_phrase(text: &str, exit_words: &[String]) -> bool {
    const PREFIXES: &[&str] = &[
        "你好小智",
        "小智同学",
        "小智",
        "好了",
        "好的",
        "好吧",
        "行了",
        "可以了",
        "那就",
        "那你",
        "那",
        "你可以",
        "你先",
        "你就",
        "你",
        "先",
    ];
    const SUFFIXES: &[&str] = &["去吧", "吧", "了", "啊", "啦", "呀", "哦", "咯"];
    let normalized = crate::fast::normalized(text);
    let mut core = normalized.as_str();
    let matches = |core: &str| {
        !core.is_empty()
            && exit_words.iter().any(|word| {
                let w = crate::fast::normalized(word);
                !w.is_empty()
                    && core.strip_prefix(&w).is_some_and(|rest| {
                        rest.is_empty() || rest == w || rest.strip_prefix(&w) == Some(w.as_str())
                    })
            })
    };
    if matches(core) {
        return true;
    }
    for _ in 0..3 {
        if let Some(rest) = PREFIXES.iter().find_map(|p| core.strip_prefix(p)) {
            core = rest;
            if matches(core) {
                return true;
            }
        }
    }
    for _ in 0..2 {
        if let Some(rest) = SUFFIXES.iter().find_map(|p| core.strip_suffix(p)) {
            core = rest;
            if matches(core) {
                return true;
            }
        }
    }
    false
}

/// 只读慢查询转到后台时说的话（n100）
pub const BACKGROUND_QUERY_FILLER: &str = "我在后台查着，查好了告诉你。";
/// 转到后台的查询写给大模型的工具结果
const BACKGROUND_TOOL_RESULT: &str = "已转到后台查询，结果出来后会主动告诉用户";
/// 这一轮被打断时还没拿到结果的工具，补记进历史时写的结果（n114）：工具可能已经执行，大模型不能当它没发生
pub const INTERRUPTED_TOOL_RESULT: &str =
    "用户打断时这个工具已经开始执行，结果没有收到：不要直接再执行一遍，需要时先查状态再告诉用户";

/// 后台查询结果出来、会话主动播报时附在系统提示词后的说明（n100）。
const BACKGROUND_REPORT_GUIDE: &str = "现在是你主动开口：刚才转到后台查的结果出来了。用一两句口语告诉用户结论；用户这期间如果已经在聊别的，先说一句“刚才查的……有结果了”再说。";

/// 转到后台继续执行的只读慢查询（n100）。丢弃时中止（会话结束、过期）。
#[derive(Debug)]
pub struct BackgroundQuery {
    /// 用户当时问的话
    pub question: String,
    pub handle: tokio::task::JoinHandle<Vec<(ToolCall, ToolOutcome)>>,
}

impl Drop for BackgroundQuery {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// 查询结果整理成给大模型看的文字（每个工具一段，总长有上限）。
pub fn background_results_text(outcomes: &[(ToolCall, ToolOutcome)]) -> String {
    let mut text = String::new();
    for (call, outcome) in outcomes {
        let content = match outcome {
            ToolOutcome::Reply(content) | ToolOutcome::Speak(content) => content.as_str(),
            ToolOutcome::PlayMusic { .. } => continue,
        };
        text.push_str(&format!("[{}] {}\n", call.function.name, content));
    }
    text.chars().take(2000).collect()
}

/// 慢工具垫音的起说延迟：工具在这之内返回就不说垫音（快的时候不说废话，慢的时候不冷场）。
/// 只用于可能很快返回的查询类；一定要几秒的工具见 [`slow_tool_filler_delay`]。
pub const SLOW_TOOL_FILLER_DELAY: Duration = Duration::from_millis(1000);

/// 垫音什么时候说（n113）：生成预览、检查机器、拍照看图一定要几秒，`TurnGate` 放行、工具开跑的同时就说，
/// 像人听到吩咐先应一声“好”再动手，省掉原来干等的 1 秒；联网搜索、天气这类查询常常 1 秒内就回，
/// 仍等 [`SLOW_TOOL_FILLER_DELAY`] 还没回才说，快的时候不说废话。
pub fn slow_tool_filler_delay(filler: &str) -> Duration {
    if filler == SLOW_QUERY_FILLER {
        SLOW_TOOL_FILLER_DELAY
    } else {
        Duration::ZERO
    }
}

/// n72（2026-11-08 用户决定）：激光工具在场时附在每轮系统提示后的用法表，让模型按用户说法选对工具。
/// 只写工具怎么选，确认与停止规则仍以 [`IMMUTABLE_SAFETY_RULES`] 为准。
pub const LASER_TOOL_GUIDE: &str = "激光工具怎么选：\
问机器连没连、在干什么，用 check_laser_connection_tool。\
要刻字，先用 laser_workflow_tool 的 preview（带文字、材料、厚度，知道木牌尺寸就带上），预览会弹到电脑上；\
要刻圆、星星、爱心、边框这类图形，在同一个 preview 里带 shapes / border（坐标 mm，木牌中心为原点，y 正数在上），图形别和文字重叠，需要时用 text_box 给文字留位置；改图形大小位置就带改好的参数重新 preview。\
用户想先看看位置、走个边框，用 frame 带同一个 workflow_id，按预览范围空走一圈不出光，用户确认走边框后再带 confirmed=true；边框一开始走工具就会回话；途中用户说暂停用 laser_safe_action_tool 的 pause，说不走了、取消、别走了用 stop_framing，不要用 laser_workflow_tool 的 cancel（那会作废预览）；说继续先问“确认继续走边框吗”，同意后 resume 带 confirmed=true。走完请用户挪正材料，可以再走。\
用户核对后明确说确认开始，再用 confirm_send 带同一个 workflow_id。功率速度默认来自材料库，用户明确说了这次的功率、速度或遍数就放进 manual_params。\
问进度用 status，取消用 cancel，从中断处继续用 resume。用户说停或急停，立即用 laser_safe_action_tool 的 emergency_stop 并带 confirmed=true，不要先问。\
边刻边调快慢或功率，用 laser_safe_action_tool 的 feed_ 或 power_ 动作。查某种材料的参数用 material_params_tool 的 get。\
刻完用户说太焦、太浅、切不透，用 refine_laser_params_from_feedback_tool 给建议，用户同意后用 regenerate_text_laser_task_tool 采用并重新预览。\
不知道参数就用 run_calibration_grid_tool 刻测试矩阵，用户说第几格最好再用 select_calibration_cell_tool 保存；\
说线距测试就带 test_type=interval，想测遍数或线距就改 x_axis / y_axis。\
照片、图片语音拿不到文件，请用户在电脑工作台上传，转换方式（如 Jarvis）和快速 / 均衡 / 精细在那里选。\
用户没说串口号或 IP 就不要填；工具报错说缺连接时，再问用户。";

fn arg_flag_true(arguments: &Value, key: &str) -> bool {
    match arguments.get(key) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(text)) => text.trim().eq_ignore_ascii_case("true") || text.trim() == "1",
        Some(Value::Number(num)) => num.as_i64() == Some(1),
        _ => false,
    }
}

fn is_slow_query_call(call: &ToolCall) -> bool {
    matches!(
        call.function.name.as_str(),
        "web_search" | "search_rag" | "get_weather" | "get_news"
    )
}

fn is_slow_check_call(call: &ToolCall) -> bool {
    call.function.name == "check_laser_connection_tool"
}

fn is_slow_look_call(call: &ToolCall) -> bool {
    call.function.name == DEVICE_CAMERA_TOOL
}

fn is_slow_generate_preview_call(call: &ToolCall) -> bool {
    let name = call.function.name.as_str();
    match name {
        // 刻字预览与测试矩阵生成（都不动机器）；发送、续雕、开始刻矩阵不在此列
        "laser_workflow_tool" => {
            let parsed: Value =
                serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);
            parsed.get("action").and_then(Value::as_str) == Some("preview")
        }
        "run_calibration_grid_tool" => {
            let parsed: Value =
                serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);
            !(arg_flag_true(&parsed, "confirmed")
                && parsed
                    .get("calibration_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.trim().is_empty()))
        }
        "regenerate_text_laser_task_tool" => {
            let parsed: Value =
                serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);
            !parsed
                .get("feedback_id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.trim().is_empty())
        }
        _ => false,
    }
}

/// 判定本轮工具调用是否需要先行播放缓存过渡垫音：
/// - 仅当整批调用全部属于慢速查询或慢速预览生成时才返回垫音；
/// - 一旦包含任何控制、R3 审批、音乐或本地毫秒级快工具，一律返回 `None`。
pub fn slow_tool_filler(calls: &[ToolCall]) -> Option<&'static str> {
    if calls.is_empty() {
        return None;
    }
    let mut has_generate = false;
    let mut has_check = false;
    let mut has_look = false;
    for call in calls {
        if is_slow_generate_preview_call(call) {
            has_generate = true;
        } else if is_slow_check_call(call) {
            has_check = true;
        } else if is_slow_look_call(call) {
            has_look = true;
        } else if !is_slow_query_call(call) {
            return None;
        }
    }
    if has_generate {
        Some(SLOW_GENERATE_FILLER)
    } else if has_look {
        Some(SLOW_LOOK_FILLER)
    } else if has_check {
        Some(SLOW_CHECK_FILLER)
    } else {
        Some(SLOW_QUERY_FILLER)
    }
}

#[derive(Debug, Clone)]
pub struct ConversationSettings {
    pub system_prompt: String,
    /// 历史长度预算（字符数，约等于中文 token 数；不含系统提示）
    pub history_budget: usize,
    /// 说出其中之一（去掉标点后完全相同）即结束会话
    pub exit_words: Vec<String>,
    /// 设备检测到唤醒词时的回应；为空则不回应
    pub wake_reply: Option<String>,
    /// 退出与空闲断开前说的话
    pub goodbye: String,
    /// 识别或大模型出错时说的话
    pub error_reply: String,
    pub max_tool_rounds: usize,
    pub tool_timeout: Duration,
    pub read_only_shortcuts: bool,
    /// 加工结束后在空闲且设备在线时主动播报（2026-11-02 用户决定，语音中心可关）
    pub announce_job_done: bool,
    /// 旁人说话不接话（n100，默认开，语音中心可关）：给大模型 `stay_silent` 选项
    pub ignore_bystanders: bool,
    /// 本地判停流上加载了命令词检测（n101）：识别结果像被听错的命令时，稍等命令词结果
    pub command_keywords: bool,
    /// 加工发送期间是否将小智屏幕同步为实时加工副屏 HUD（随 `screen_preview_sync` 开关控制）
    pub screen_preview_sync: bool,
    pub model_labels: Vec<String>,
    /// 是否在每轮系统提示中注入当前本机日期、星期与时间（默认开）
    pub include_local_clock: bool,
    /// 0ms 本地 ASR 同音错词纠偏规则（按错词长度降序）
    pub asr_corrections: Vec<(String, String)>,
}

pub(crate) const DEFAULT_ERROR_REPLY: &str = "抱歉，我这边出了点问题，请稍后再试";
/// 断网 / 超时类报错话术：出这类错时云端合成多半也连不上，所以随固定话术在启动时预合成（见 `fixed_phrases`）。
pub(crate) const LLM_TIMEOUT_REPLY: &str = "连接大模型超时";
pub(crate) const LLM_OFFLINE_REPLY: &str = "连不上大模型，请检查网络";
pub(crate) const ASR_TIMEOUT_REPLY: &str = "语音识别超时";
pub(crate) const ASR_OFFLINE_REPLY: &str = "连不上语音识别，请检查网络";
const NETWORK_ERROR_REPLIES: [&str; 4] = [
    LLM_TIMEOUT_REPLY,
    LLM_OFFLINE_REPLY,
    ASR_TIMEOUT_REPLY,
    ASR_OFFLINE_REPLY,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorStage {
    Asr,
    Llm,
    ToolLimit,
}

/// 把识别、大模型或工具超限错误映射为简短直白的口语播报（闭集静态中文短句，绝不拼入密钥、地址、IP 或服务方英文原文）。
/// 调用方若自定义了非默认 `settings_reply`，优先保留自定义话术。
pub(crate) fn spoken_error_reply<'a>(
    settings_reply: &'a str,
    stage: ErrorStage,
    error: &ProviderError,
) -> &'a str {
    use moss_voice_providers::DiagCategory;
    if settings_reply != DEFAULT_ERROR_REPLY {
        return settings_reply;
    }
    if stage == ErrorStage::ToolLimit {
        return "工具调用次数超限，换个说法试试";
    }
    let msg = error.message.as_str();
    let has = |needles: &[&str]| needles.iter().any(|n| msg.contains(n));
    let uncategorized = matches!(error.category, DiagCategory::Model | DiagCategory::General);
    let is_auth = error.category == DiagCategory::Auth
        || (uncategorized && has(&["密钥", "权限", "HTTP 401", "HTTP 403"]));
    let is_quota = error.category == DiagCategory::Quota
        || (uncategorized && has(&["余额", "HTTP 402", "频繁", "额度", "HTTP 429"]));
    let is_config = error.category == DiagCategory::Config
        || (uncategorized
            && has(&[
                "地址",
                "模型名",
                "资源 ID",
                "参数",
                "HTTP 400",
                "HTTP 404",
                "HTTP 422",
                "跳转",
            ]));
    let is_network = error.category == DiagCategory::Network
        || (uncategorized
            && has(&[
                "超时",
                "无法连接",
                "网络",
                "域名",
                "连接中断",
                "连接中途断开",
            ]));

    match stage {
        ErrorStage::Llm => {
            if is_auth {
                "大模型密钥无效或没权限"
            } else if is_quota {
                if has(&["余额", "402"]) {
                    "大模型余额不足"
                } else {
                    "大模型额度用完或请求太快"
                }
            } else if is_config {
                "大模型地址或名称配错了"
            } else if is_network {
                if has(&["超时"]) {
                    LLM_TIMEOUT_REPLY
                } else {
                    LLM_OFFLINE_REPLY
                }
            } else {
                "大模型服务出错了"
            }
        }
        ErrorStage::Asr => {
            if is_auth {
                "语音识别密钥无效或没权限"
            } else if is_quota {
                "语音识别额度不足或太快"
            } else if is_config {
                "语音识别配置不对"
            } else if is_network {
                if has(&["超时"]) {
                    ASR_TIMEOUT_REPLY
                } else {
                    ASR_OFFLINE_REPLY
                }
            } else if has(&["本地", "辅助程序"]) {
                "本地语音识别出错了"
            } else {
                "语音识别出错了"
            }
        }
        ErrorStage::ToolLimit => "工具调用次数超限，换个说法试试",
    }
}

impl Default for ConversationSettings {
    fn default() -> Self {
        Self {
            system_prompt: DEFAULT_SYSTEM_PROMPT.into(),
            history_budget: 4000,
            exit_words: [
                "退出",
                "再见",
                "拜拜",
                "退下",
                // SenseVoice 常把短促的“退下”听成“退一下”（2026-11-15 整链实测）；单独说这三个字也是要小智退下
                "退一下",
                "休息",
                "去休息",
                "没事了",
                "不聊了",
                "关闭对话",
                "结束对话",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            wake_reply: Some("我在".into()),
            goodbye: "好的，再见".into(),
            error_reply: DEFAULT_ERROR_REPLY.into(),
            max_tool_rounds: 5,
            tool_timeout: Duration::from_secs(30),
            read_only_shortcuts: true,
            announce_job_done: true,
            ignore_bystanders: true,
            command_keywords: false,
            screen_preview_sync: true,
            model_labels: Vec::new(),
            include_local_clock: true,
            asr_corrections: Vec::new(),
        }
    }
}

/// n88 尾音语义判停提供方（如本地 Smart Turn v3）：评估截止停顿时刻最后不超过 8 秒音频的说完概率（`0.0..=1.0`）。
pub trait TurnEndPredictor: Send + Sync {
    fn predict_turn_end<'a>(
        &'a self,
        pcm: &'a [i16],
        sample_rate: u32,
    ) -> BoxFuture<'a, Option<f32>>;
}

/// 对话所需的模型与工具。整个服务共享一份，每台设备的历史在各自的会话里。
pub struct Conversation {
    pub asr: Arc<dyn Asr>,
    /// 流式识别（NV7）；有时会话边说边识别，`asr` 作为它失败时的退路
    pub streaming_asr: Option<Arc<dyn StreamingAsr>>,
    pub llm: Arc<dyn Llm>,
    pub tts: Arc<dyn Tts>,
    /// 主云端 TTS 失败且未产出本句音频时的本地降级 TTS（如本机已安装的 Kokoro / Matcha）。
    pub fallback_tts: Option<Arc<dyn Tts>>,
    pub tools: Arc<dyn ToolExecutor>,
    pub settings: ConversationSettings,
    pub memory: Option<Arc<crate::memory::MemoryService>>,
    pub speaker_enrollment: Option<Arc<crate::speaker_enrollment::SpeakerEnrollment>>,
    pub personal_context: Option<Arc<crate::person_context::PersonContext>>,
    /// NV19-04 长期记忆检索；宿主不接入时为空，NV10 摘要照常使用
    pub context_retrieval: Option<crate::context::retrieve::ContextRetrieval>,
    /// NV19-03 会话结束后的候选提取；宿主不接入时为空
    pub context_extraction: Option<crate::context::extract::ContextExtraction>,
    /// 可选 Jev 信息筛选与快答分流器；默认未挂载，开启后不携带任何动作授权。
    pub jev_filter: Option<Arc<crate::jev::VoiceJevFilter>>,
    /// n59 当前说话角色；挂载后每轮用它的提示词代替 `settings.system_prompt`（可被语音或界面热切换）
    pub persona: Option<Arc<crate::persona::PersonaSwitch>>,
    /// n88 尾音语义判停预测器；默认为空，开启后用于辅助评估用户停顿时是否意犹未尽。
    pub turn_predictor: Option<Arc<dyn TurnEndPredictor>>,
    /// n105 完整过程记录（默认关）：开着时每轮把大模型请求 / 回复、工具完整结果经 [`TurnEvent::Trace`]
    /// 交给会话，由会话带上设备、会话与轮次写入 `logs/process-trace/`
    pub process_trace: Option<laser_runtime::ProcessTrace>,
    /// 固定话术（唤醒回应、再见）的合成结果缓存
    cache: Mutex<Vec<(String, Pcm)>>,
    /// 缓存代次：热更换合成音色时递增，防止旧声线的在途预合成写回缓存
    cache_epoch: std::sync::atomic::AtomicU64,
}

impl fmt::Debug for Conversation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Conversation")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl Conversation {
    pub fn new(
        asr: Arc<dyn Asr>,
        llm: Arc<dyn Llm>,
        tts: Arc<dyn Tts>,
        tools: Arc<dyn ToolExecutor>,
        settings: ConversationSettings,
    ) -> Self {
        Self {
            asr,
            streaming_asr: None,
            llm,
            tts,
            fallback_tts: None,
            tools,
            settings,
            memory: None,
            speaker_enrollment: None,
            personal_context: None,
            context_retrieval: None,
            context_extraction: None,
            jev_filter: None,
            persona: None,
            turn_predictor: None,
            process_trace: None,
            cache: Mutex::new(Vec::new()),
            cache_epoch: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// 挂载 n88 尾音语义判停预测器（Smart Turn v3 等）。
    pub fn with_turn_predictor(mut self, predictor: Arc<dyn TurnEndPredictor>) -> Self {
        self.turn_predictor = Some(predictor);
        self
    }

    /// 挂载 n59 说话角色：提示词随语音或界面切换即时更新。
    pub fn with_persona(mut self, persona: Arc<crate::persona::PersonaSwitch>) -> Self {
        self.persona = Some(persona);
        self
    }

    /// 本轮的基础系统提示：挂了角色用角色的，否则用固定设置。
    pub fn system_prompt(&self) -> String {
        match &self.persona {
            Some(persona) => persona.system_prompt(),
            None => self.settings.system_prompt.clone(),
        }
    }

    pub fn with_memory(mut self, memory: Arc<crate::memory::MemoryService>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_speaker_enrollment(
        mut self,
        enrollment: Arc<crate::speaker_enrollment::SpeakerEnrollment>,
    ) -> Self {
        self.speaker_enrollment = Some(enrollment);
        self
    }
    pub fn with_personal_context(
        mut self,
        context: Arc<crate::person_context::PersonContext>,
    ) -> Self {
        self.personal_context = Some(context);
        self
    }
    pub fn with_context_retrieval(
        mut self,
        retrieval: crate::context::retrieve::ContextRetrieval,
    ) -> Self {
        self.context_retrieval = Some(retrieval);
        self
    }
    pub fn with_context_extraction(
        mut self,
        extraction: crate::context::extract::ContextExtraction,
    ) -> Self {
        self.context_extraction = Some(extraction);
        self
    }
    pub fn with_jev_filter(mut self, filter: Arc<crate::jev::VoiceJevFilter>) -> Self {
        self.jev_filter = Some(filter);
        self
    }

    /// 启用流式识别（NV7）。
    pub fn with_streaming_asr(mut self, streaming_asr: Arc<dyn StreamingAsr>) -> Self {
        self.streaming_asr = Some(streaming_asr);
        self
    }

    /// 配置主 TTS 失败时的本地降级 TTS（当句尚未产出音频即报错时自动降级，避免整轮静音）。
    pub fn with_fallback_tts(mut self, fallback_tts: Arc<dyn Tts>) -> Self {
        self.fallback_tts = Some(fallback_tts);
        self
    }
    /// 整句退出意图：去掉称呼、起头词与句尾语气词后与退出词（或其连说两三遍，如“退下退下”“再见再见”）完全相等
    /// （“小智退下吧”“好了你退下”算，“不要退下”“退出登录”不算，留给正常对话）。
    pub(crate) fn is_exit(&self, text: &str) -> bool {
        is_exit_phrase(text, &self.settings.exit_words)
    }

    async fn cached_speech(&self, text: &str) -> Result<Pcm, ProviderError> {
        if let Ok(cache) = self.cache.lock()
            && let Some((_, pcm)) = cache.iter().find(|(key, _)| key == text)
        {
            return Ok(pcm.clone());
        }
        let epoch_before = self.cache_epoch.load(std::sync::atomic::Ordering::Acquire);
        // 备用合成出的音色和主合成不同，只用这一次、不进缓存（n114）：启动预热时云端还连不上，
        // 否则固定话术会一直是本地音色，直到重启
        let (mut pcm, from_fallback) = match self.tts.synthesize(text).await {
            Ok(pcm) => (pcm, false),
            Err(primary_err) => {
                if let Some(fallback) = &self.fallback_tts {
                    (
                        fallback.synthesize(text).await.map_err(|_| primary_err)?,
                        true,
                    )
                } else {
                    return Err(primary_err);
                }
            }
        };
        crate::audio::trim_trailing_silence_i16(
            &mut pcm.samples,
            pcm.sample_rate,
            crate::audio::TRAILING_SILENCE_KEEP_MS,
            crate::audio::TRAILING_SILENCE_TRIGGER_MS,
        );
        crate::audio::fade_head_i16(
            &mut pcm.samples,
            pcm.sample_rate,
            crate::audio::BOUNDARY_FADE_MS,
        );
        crate::audio::fade_tail_i16(
            &mut pcm.samples,
            pcm.sample_rate,
            crate::audio::BOUNDARY_FADE_MS,
        );
        const MAX_SPEECH_CACHE_ENTRIES: usize = 32;
        if !from_fallback
            && self.cache_epoch.load(std::sync::atomic::Ordering::Acquire) == epoch_before
            && let Ok(mut cache) = self.cache.lock()
            && self.cache_epoch.load(std::sync::atomic::Ordering::Acquire) == epoch_before
            && !cache.iter().any(|(key, _)| key == text)
        {
            if cache.len() >= MAX_SPEECH_CACHE_ENTRIES {
                let pinned = self.fixed_phrases();
                let evict_idx = cache
                    .iter()
                    .position(|(key, _)| !pinned.iter().any(|p| p == key))
                    .unwrap_or(0);
                cache.remove(evict_idx);
            }
            cache.push((text.to_owned(), pcm.clone()));
        }
        Ok(pcm)
    }
    /// 返回需要预合成缓存的高频固定话术清单（去重、过滤空白）。
    pub fn fixed_phrases(&self) -> Vec<String> {
        let mut phrases = Vec::with_capacity(12);
        let mut push_unique = |text: &str| {
            let trimmed = text.trim();
            if !trimmed.is_empty() && !phrases.iter().any(|existing| existing == trimmed) {
                phrases.push(trimmed.to_owned());
            }
        };
        if let Some(wake) = self.settings.wake_reply.as_deref() {
            push_unique(wake);
        }
        push_unique(&self.settings.goodbye);
        if self.settings.error_reply == DEFAULT_ERROR_REPLY {
            // 默认设置下出错改说具体原因（n116），旧的笼统话术不会再念；断网类原因要预合成，断网时才念得出来
            for reply in NETWORK_ERROR_REPLIES {
                push_unique(reply);
            }
        } else {
            push_unique(&self.settings.error_reply);
        }
        push_unique(STOP_OK_SPEECH);
        push_unique(STOP_ERR_SPEECH);
        push_unique(crate::tools::APPROVAL_SPEECH);
        push_unique(crate::tools::SUBMITTED_APPROVAL_SPEECH);
        push_unique(SLOW_QUERY_FILLER);
        push_unique(SLOW_GENERATE_FILLER);
        push_unique(SLOW_CHECK_FILLER);
        push_unique(SLOW_LOOK_FILLER);
        push_unique(BACKGROUND_QUERY_FILLER);
        phrases
    }

    /// 清空固定话术内存缓存并递增代次（热更换本地克隆参考音色或台词时调用，n131）。
    pub fn invalidate_speech_cache(&self) {
        self.cache_epoch
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    /// 提前合成高频固定话术与慢工具过渡垫音并写入内存缓存，返回成功缓存的条数。
    /// 单条合成失败不会中断其余条目，后续实际触发时仍会按需重试合成。
    pub async fn prewarm_cache(&self) -> usize {
        let mut warmed = 0;
        for phrase in self.fixed_phrases() {
            if self.cached_speech(&phrase).await.is_ok() {
                warmed += 1;
            }
        }
        warmed
    }

    #[cfg(test)]
    pub(crate) async fn prewarm_or_speak_for_test(&self, text: &str) {
        let _ = self.cached_speech(text).await;
    }

    #[cfg(test)]
    pub(crate) fn speech_cache_len_for_test(&self) -> usize {
        self.cache.lock().map(|c| c.len()).unwrap_or(0)
    }
}

/// 还在合成、送往会话的这一轮回答（送去朗读的正文，是否已朗读过内容），见 [`Turn::generate_split`]（n115）
type PendingSpeech<'s> =
    std::pin::Pin<Box<dyn std::future::Future<Output = (String, bool)> + Send + 's>>;

/// 任务取消守卫：对话被打断丢弃时，自动 abort 后台正在预合成的 TTS 任务。
struct AbortOnDrop<T>(Option<tokio::task::JoinHandle<T>>);

impl<T> AbortOnDrop<T> {
    fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self(Some(handle))
    }
    async fn join(&mut self) -> Option<Result<T, tokio::task::JoinError>> {
        Some(self.0.take()?.await)
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

/// 一句已启动后台 TTS 合成的待播句子（用于二级流水线预取，消除句间 TTS 启动间隙）。
struct PreparedSentence {
    text: String,
    chunks_rx: tokio::sync::mpsc::UnboundedReceiver<Pcm>,
    handle: AbortOnDrop<Result<(), moss_voice_providers::ProviderError>>,
    /// 本轮第几句（完整过程记录里“第 N 句开始合成 / 合成出音频 / 合成失败”，n110）
    index: u32,
    began: Instant,
}

/// 设备自带 MCP 工具的一次调用：由对话任务发给会话，会话经 WebSocket 转给设备并把结果文字送回。
#[derive(Debug)]
pub struct DeviceCall {
    /// 设备上报的原始工具名
    pub name: String,
    pub arguments: Value,
    pub reply: oneshot::Sender<String>,
}

/// 设备自带的 MCP 工具（内置语音技术书 §5.3）。工具名加 `device__` 前缀，并把模型接口不接受的字符
/// （如固件工具名中的 `.`）换成 `_`；调用时换回原名。
#[derive(Clone)]
pub struct DeviceTools {
    specs: Vec<ToolSpec>,
    names: HashMap<String, String>,
    calls: UnboundedSender<DeviceCall>,
}

impl DeviceTools {
    /// `tools` 为设备 `tools/list` 返回的条目（`name`、`description`、`inputSchema`）。
    pub fn new(tools: &[Value], calls: UnboundedSender<DeviceCall>) -> Self {
        let mut specs = Vec::new();
        let mut names = HashMap::new();
        for tool in tools {
            let Some(original) = tool["name"].as_str().filter(|name| !name.is_empty()) else {
                continue;
            };
            let cleaned: String = original
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let name: String = format!("device__{cleaned}").chars().take(64).collect();
            if names.contains_key(&name) {
                // 清理后重名：后面的不登记（不静默覆盖）
                continue;
            }
            names.insert(name.clone(), original.to_owned());
            specs.push(ToolSpec {
                name,
                description: tool["description"].as_str().unwrap_or_default().to_owned(),
                parameters: tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type": "object"})),
            });
        }
        Self {
            specs,
            names,
            calls,
        }
    }

    pub fn specs(&self) -> &[ToolSpec] {
        &self.specs
    }

    fn original(&self, name: &str) -> Option<&str> {
        self.names.get(name).map(String::as_str)
    }

    async fn call(&self, original: &str, arguments: &str) -> ToolOutcome {
        let arguments = if arguments.trim().is_empty() {
            "{}"
        } else {
            arguments
        };
        let Ok(arguments) = serde_json::from_str::<Value>(arguments) else {
            return ToolOutcome::Reply(format!("调用设备工具 {original} 的参数不是合法的 JSON"));
        };
        let (reply, result) = oneshot::channel();
        if self
            .calls
            .send(DeviceCall {
                name: original.to_owned(),
                arguments,
                reply,
            })
            .is_err()
        {
            return ToolOutcome::Reply("设备已断开".into());
        }
        ToolOutcome::Reply(result.await.unwrap_or_else(|_| "设备没有返回结果".into()))
    }
}

/// 停止快路径的触发来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastStopSource {
    /// 边说边识别的中间结果整句命中停止词
    Interim,
    /// 思考中命令词模型听到“停”
    Keyword,
    /// 思考中判停模型没认出人声的那阵声音，单独识别出停止说法
    Recheck,
}

impl FastStopSource {
    /// 记录里的机器可读键。
    pub fn key(self) -> &'static str {
        match self {
            Self::Interim => "interim",
            Self::Keyword => "kws",
            Self::Recheck => "recheck",
        }
    }
}

/// 一轮对话的输入。
pub enum TurnInput {
    /// 流式中间结果命中后立即执行；该段识别流由会话丢弃，最终结果不会重复执行。
    FastStop {
        text: String,
        /// 是怎么听出“停”的（完整过程记录如实写明来源，n110）
        source: FastStopSource,
    },
    /// 用户说的一段话（PCM 与采样率）；`stream` 为说话期间已在进行的流式识别
    Utterance {
        pcm: Vec<i16>,
        sample_rate: u32,
        stream: Option<AsrStream>,
    },
    /// VA1 文字直通联调：跳过麦克风与识别，直接以“刚识别出的话”进入识别后的完整对话链路；
    /// `simulated == true` 时启用 VA2 模拟门禁（禁止物理控制/外部控制/长期记忆污染）。
    Text { text: String, simulated: bool },
    /// 固定话术（唤醒回应、空闲再见）；`close` 为真时说完结束会话
    Say { text: String, close: bool },
    /// 后台查询结果出来了：由大模型组织成一两句话主动告诉用户（n100）
    Report { question: String, results: String },
}

/// 一轮对话送回会话的事件。
#[derive(Debug)]
pub enum TurnEvent {
    /// 只读慢查询转到后台继续执行（这一轮随后结束），结果由会话等空闲时播报（n100）
    Background(BackgroundQuery),
    /// 大模型判断这句话不是对小智说的（调用了 `stay_silent`）：不回应，不记入历史（n100）
    NotForMe,
    /// Trusted Rust-only identity; `verified` is true ONLY when voiceprint matched on this turn's audio.
    PersonResolved {
        scope: Option<crate::person_context::PersonScope>,
        verified: bool,
    },
    /// 退出词已经确认，合成再见之前就关闭插话检测。
    Closing,
    /// VA3 本轮所走路径：`fast_stop` / `fast_read` / `fast_exit` / `llm` / `ignored` / `not_for_me`
    Route(&'static str),
    /// VA2 #2 模拟轮次在激光加工进行中跳过急停命令的记录原因
    StopSkipped(String),
    /// n105 完整过程记录的一条（只在开关打开时发送）：`llm_round` / `tool_call`
    Trace {
        kind: &'static str,
        data: Value,
    },
    /// 这一轮开始执行这批工具：此后用户接着说话不再整轮重来（工具可能有副作用，不能执行两遍）。
    /// 会话记下这批调用，这一轮被打断时补记进历史（n114）
    ToolsStarted(Vec<ToolCall>),
    /// 大模型的第一句交给合成了，音频马上就到（n115）：会话（已放行时）先发 `tts start`，让固件切到说话状态
    /// 要的保护间隔和合成时间重叠，不再等音频到了才发
    SpeechSoon,
    /// 上一批工具的结果（调用 id、写给大模型的结果文字），会话据此补记被打断那一轮的历史（n114）
    ToolsFinished(Vec<(String, String)>),
    Timing {
        stage: &'static str,
        ms: u64,
    },
    /// 可覆写的阶段耗时（例如多轮工具调用时不断更新最终 `llm_done` 时刻）。
    TimingOverwrite {
        stage: &'static str,
        ms: u64,
    },
    ToolTiming {
        name: String,
        ms: u64,
    },
    TextReply(String),
    /// 识别结果（会话发 `stt`）
    Heard(String),
    /// 尾音语义判停模型给出的说完概率（`0.0..=1.0`）
    SmartTurnProb(f32),
    /// 原始识别结果与最终文本（供默认关闭的仅内存排障流与完整过程记录使用）：`homophone` 为同音纠正后的文字，
    /// `keyword` 为命令词模型改判时听到的命令类别（n110：同音纠正与命令词改判分开记）
    RawHeard {
        raw: String,
        homophone: String,
        corrected: String,
        keyword: Option<moss_voice_providers::local::CommandKeyword>,
    },
    /// 工具调用详情摘要（含耗时与是否被模拟门禁拦截，供对话记录与 VA3 联调明细复用）
    ToolDebug {
        name: String,
        arguments: String,
        result: String,
        ms: u64,
        blocked: bool,
        block_reason: Option<String>,
    },
    /// 没有识别出文字：继续拾音
    Ignored,
    /// 模型回复中的第一个表情（会话发 `llm` 表情消息）
    Emotion(&'static str),
    /// 一句话的文字与（第一段）音频
    Speech {
        text: String,
        pcm: Pcm,
    },
    /// 同一句话后续到达的音频（流式合成）
    SpeechMore(Pcm),
    /// 当前这句话的音频分块已全部推入（供节奏泵在句间欠载时立即冲刷句尾残帧，而句中流式半帧不冲刷）
    SentenceEnd,
    /// 只有受信工具能产生此事件，模型参数永远不成为文件路径。
    PlayMusic {
        track_id: String,
        title: String,
    },
    /// 出错（记入事件；不含密钥与地址）。带诊断分类、建议与技术细节（2026-11-07 n68）。
    Failed(ProviderError),
    /// 出了问题但已自动补救（如流式识别失败改整段识别、主合成失败降级本地合成），记为警告。
    Warning(ProviderError),
    /// 结束：要追加到历史的消息；`close` 为真时播完后结束会话
    Done {
        messages: Vec<ChatMessage>,
        close: bool,
    },
}

#[derive(Clone)]
pub(crate) struct TurnTrace {
    pub trace: Option<laser_runtime::ProcessTrace>,
    pub flow: String,
    pub turn: u64,
}

impl TurnTrace {
    fn emit(&self, context: &ToolContext, kind: &'static str, data: Value) -> bool {
        let Some(ref trace) = self.trace else {
            return false;
        };
        if !trace.enabled() {
            return true;
        }
        crate::session::write_voice_trace(
            trace,
            &context.session_id,
            &context.device_id,
            &self.flow,
            self.turn,
            kind,
            data,
        );
        true
    }
}

struct Turn {
    started: Instant,
    measured: Mutex<std::collections::HashSet<&'static str>>,
    conversation: Arc<Conversation>,
    context: ToolContext,
    events: UnboundedSender<TurnEvent>,
    device: Option<DeviceTools>,
    _session_person_hint: Option<crate::person_context::PersonScope>,
    /// 抢跑闸门；没有闸门（`None`）时不等待。
    gate: Option<TurnGate>,
    /// 会话注入的轮次直写记录器（存在时在事件发生当刻直接落盘，不经 `events` 通道门禁积压或随打断丢弃）
    turn_trace: Option<TurnTrace>,
    /// 本轮已发出的大模型请求数（完整过程记录里的“第 N 轮”）
    llm_rounds: std::sync::atomic::AtomicU32,
    /// 本轮已开始合成的句子数（完整过程记录里的“第 N 句”）
    tts_sentences: std::sync::atomic::AtomicU32,
    /// 完整过程记录打开时：本轮可用工具的说明（工具名 → 说明），记工具调用时附上它的用途
    tool_descriptions: Mutex<HashMap<String, String>>,
    /// VA1/VA2 模拟联调轮次标记：开启时强制在工具执行入口拦截所有物理/外部/重启控制并跳过记忆提取
    simulated: bool,
    /// VC1 工具/急停执行当刻的实时测试时间窗检查（防止真实轮次思考期间中段开窗后绕过门禁）
    live_bench_window: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

/// 命令词类别的中文名（完整过程记录用）。
pub(crate) fn keyword_label(kind: moss_voice_providers::local::CommandKeyword) -> &'static str {
    match kind {
        moss_voice_providers::local::CommandKeyword::Stop => "停止",
        moss_voice_providers::local::CommandKeyword::Exit => "退出",
    }
}

impl Turn {
    /// 完整过程记录是否打开（开关每 2 秒最多读一次设置文件）。
    fn trace_on(&self) -> bool {
        if let Some(ref tt) = self.turn_trace {
            return tt
                .trace
                .as_ref()
                .is_some_and(laser_runtime::ProcessTrace::enabled);
        }
        self.conversation
            .process_trace
            .as_ref()
            .is_some_and(laser_runtime::ProcessTrace::enabled)
    }

    fn emit_trace(&self, kind: &'static str, data: Value) {
        if let Some(ref tt) = self.turn_trace {
            let _ = tt.emit(&self.context, kind, data);
            return;
        }
        if self.trace_on() {
            self.send(TurnEvent::Trace { kind, data });
        }
    }

    async fn wait_release(&self) {
        if let Some(mut release) = self.gate.as_ref().map(|gate| gate.release.clone())
            && release.wait_for(|released| *released).await.is_err()
        {
            // 会话已放弃这一轮（用户接着说了）：绝不能往下执行工具，等任务被中止
            std::future::pending::<()>().await;
        }
    }

    async fn fixed_tool(&self, name: &str, arguments: Value) -> crate::tools::ToolHostResult {
        let began = Instant::now();
        let arguments = arguments.to_string();
        let result = tokio::time::timeout(
            self.conversation.settings.tool_timeout,
            self.conversation
                .tools
                .call_raw(&self.context, name, &arguments),
        )
        .await
        .unwrap_or_else(|_| crate::tools::ToolHostResult {
            success: false,
            result: "工具执行超时".into(),
            detail: None,
        });
        let elapsed_ms = began.elapsed().as_millis() as u64;
        self.send(TurnEvent::ToolTiming {
            name: name.into(),
            ms: elapsed_ms,
        });
        if self.trace_on() {
            // 快路径直接调用的工具（不经大模型）：与大模型发起的工具调用一样记下参数、完整结果与耗时
            self.emit_trace(
                "tool_call",
                serde_json::json!({
                    "name": name,
                    "arguments": arguments,
                    "success": result.success,
                    "result_kind": "reply",
                    "result": result.result,
                    "elapsed_ms": elapsed_ms,
                    "fast_route": true,
                }),
            );
        }
        self.send(TurnEvent::ToolDebug {
            name: name.into(),
            arguments,
            result: result.result.clone(),
            ms: elapsed_ms,
            blocked: false,
            block_reason: None,
        });
        result
    }

    async fn read_reply(&self, intent: crate::fast::ReadIntent) -> String {
        use crate::fast::{ReadIntent, body};
        use serde_json::json;
        if matches!(intent, ReadIntent::Status | ReadIntent::Connection) {
            let (tool, args) = if intent == ReadIntent::Status {
                ("laser_safe_action_tool", json!({"action":"status"}))
            } else {
                ("check_laser_connection_tool", json!({}))
            };
            let result = self.fixed_tool(tool, args).await;
            return if result.success {
                format!("查询结果：{}", result.result)
            } else {
                read_failure_speech(&result.result, "查询失败")
            };
        }
        let recent = self
            .fixed_tool("laser_workflow_tool", json!({"action":"recent"}))
            .await;
        if !recent.success {
            return read_failure_speech(&recent.result, "查询失败");
        }
        let recent = body(&recent);
        let active: Vec<&Value> = recent["workflows"]
            .as_array()
            .map(|items| items.iter().filter(|v| v["status"] == "sending").collect())
            .unwrap_or_default();
        if active.is_empty() {
            // 没有在途任务：不再一律回“没有找到”。材料只在加工中有意义，
            // 进度与剩余时间改为按最近一条记录回答终态（2026-11-02 用户要求）。
            if intent == ReadIntent::Material {
                return "没有找到正在发送的加工任务，无法确定当前加工进度或材料。".into();
            }
            let latest = recent["workflows"].as_array().and_then(|items| {
                items
                    .iter()
                    .max_by_key(|record| record["updated_at"].as_u64().unwrap_or(0))
            });
            return match latest {
                Some(record) => terminal_reply(record),
                None => "还没有任何加工任务记录。".into(),
            };
        }
        if active.len() != 1 {
            return "找到多个正在发送的任务，无法确定你询问的是哪个，请说明任务。".into();
        }
        let Some(id) = active[0]["workflow_id"].as_str() else {
            return "任务记录缺少编号，无法读取当前进度。".into();
        };
        let result = self
            .fixed_tool(
                "laser_workflow_tool",
                json!({"action":"status","workflow_id":id}),
            )
            .await;
        if !result.success {
            return read_failure_speech(&result.result, "查询失败");
        }
        let record = body(&result);
        if intent == ReadIntent::Material {
            let input = &record["input"];
            let (Some(material), Some(thickness)) =
                (input["material"].as_str(), input["thickness_mm"].as_f64())
            else {
                return "当前任务没有完整的材料与厚度记录，无法确定材料参数。".into();
            };
            let mut args = json!({"action":"get","material":material,"thickness_mm":thickness});
            for key in ["laser_mode", "engraving_mode"] {
                if let Some(v) = input[key].as_str() {
                    args[key] = json!(v);
                }
            }
            let result = self.fixed_tool("material_params_tool", args).await;
            if !result.success {
                return read_failure_speech(&result.result, "材料参数查询失败");
            }
            let params = body(&result);
            let (Some(power), Some(speed), Some(passes)) = (
                params["laser_max_power"].as_f64(),
                params["feed_rate"].as_f64(),
                params["passes"].as_u64(),
            ) else {
                return format!("当前材料是{material}，厚度{thickness}毫米；参数记录不完整。");
            };
            return format!(
                "当前材料是{material}，厚度{thickness}毫米；材料库建议功率{power}，速度每分钟{speed}毫米，{passes}次。建议参数不代表设备当前设置。"
            );
        }
        let progress = &record["send_result"];
        let prefix = match (
            progress["lines_sent"].as_u64(),
            progress["total_lines"].as_u64(),
        ) {
            (Some(sent), Some(total)) if total > 0 && sent <= total => format!(
                "已发送{sent}行，共{total}行，发送进度百分之{}。这不是物理加工完成比例。",
                sent.saturating_mul(100) / total
            ),
            _ => "当前任务没有可信的发送进度数据。".into(),
        };
        if intent == ReadIntent::Remaining {
            format!("{prefix}目前没有可信的剩余加工时间估计。")
        } else {
            prefix
        }
    }

    async fn resolve_person(
        &self,
        identity: Option<crate::person_context::IdentityTask>,
    ) -> Result<Option<crate::person_context::PersonScope>, ()> {
        let Some(personal) = &self.conversation.personal_context else {
            return Ok(None);
        };
        let (scope, verified) = match identity {
            Some(task) => match task.finish_outcome().await {
                crate::person_context::IdentityOutcome::Resolved(Some(scope)) => {
                    (Some(scope), true)
                }
                crate::person_context::IdentityOutcome::Resolved(None)
                | crate::person_context::IdentityOutcome::SkippedShortDuration
                | crate::person_context::IdentityOutcome::Denied => (None, false),
            },
            None => (None, false),
        };
        if !personal.permitted(&self.context, scope.as_ref()) {
            return Err(());
        }
        self.send(TurnEvent::PersonResolved {
            scope: scope.clone(),
            verified,
        });
        Ok(scope)
    }

    async fn fast_answer(
        &self,
        text: &str,
        identity: &mut Option<crate::person_context::IdentityTask>,
        personal_prefix: &mut Vec<ChatMessage>,
    ) -> bool {
        let stop = crate::fast::is_stop(text);
        let read = self
            .conversation
            .settings
            .read_only_shortcuts
            .then(|| crate::fast::read_intent(text))
            .flatten();
        if !stop && read.is_none() {
            return false;
        }
        if stop {
            let _ = identity.take();
        } else if !self.simulated && self.resolve_person(identity.take()).await.is_err() {
            return true;
        }
        self.timing("fast_route");
        if stop {
            self.send(TurnEvent::Route("fast_stop"));
            let effective_simulated =
                self.simulated || self.live_bench_window.as_ref().is_some_and(|check| check());
            if effective_simulated {
                let active_job = self
                    .conversation
                    .tools
                    .has_active_laser_job(&self.context)
                    .await;
                if active_job != Some(false) {
                    let reason = "模拟轮次的停止命令未执行".to_owned();
                    self.send(TurnEvent::StopSkipped(reason.clone()));
                    self.emit_trace(
                        "simulated_stop_skipped",
                        serde_json::json!({
                            "reason": &reason,
                            "active_job": active_job,
                            "text": text,
                        }),
                    );
                    let reply = "当前有加工任务正在运行，模拟轮次的停止命令未执行。".to_owned();
                    self.speak_fixed(&reply).await;
                    let mut done_msgs =
                        if !self.simulated && self.conversation.personal_context.is_some() {
                            std::mem::take(personal_prefix)
                        } else {
                            Vec::new()
                        };
                    done_msgs.push(ChatMessage::user(text));
                    done_msgs.push(ChatMessage::assistant(reply));
                    self.send(TurnEvent::Done {
                        messages: done_msgs,
                        close: false,
                    });
                    return true;
                }
            }
        } else if read.is_some() {
            self.send(TurnEvent::Route("fast_read"));
        }
        let reply = if stop {
            let result = self
                .fixed_tool(
                    "laser_safe_action_tool",
                    serde_json::json!({"action":"emergency_stop","confirmed":true}),
                )
                .await;
            if result.success {
                STOP_OK_SPEECH.into()
            } else {
                STOP_ERR_SPEECH.into()
            }
        } else if let Some(read) = read {
            self.read_reply(read).await
        } else {
            return false;
        };
        if stop {
            self.speak_fixed(&reply).await;
        } else if !self.speak(reply.clone()).await {
            self.send(TurnEvent::TextReply(reply.clone()));
        }
        let mut done_msgs = if !self.simulated && self.conversation.personal_context.is_some() {
            std::mem::take(personal_prefix)
        } else {
            Vec::new()
        };
        done_msgs.push(ChatMessage::user(text));
        done_msgs.push(ChatMessage::assistant(reply));
        self.send(TurnEvent::Done {
            messages: done_msgs,
            close: false,
        });
        true
    }
    fn timing(&self, stage: &'static str) {
        if !self
            .measured
            .lock()
            .map(|mut stages| stages.insert(stage))
            .unwrap_or(false)
        {
            return;
        }
        self.send(TurnEvent::Timing {
            stage,
            ms: self.started.elapsed().as_millis() as u64,
        });
    }
    fn timing_overwrite(&self, stage: &'static str) {
        self.send(TurnEvent::TimingOverwrite {
            stage,
            ms: self.started.elapsed().as_millis() as u64,
        });
    }
    fn send(&self, event: TurnEvent) {
        let _ = self.events.send(event);
    }

    fn trace_smart_turn(&self, data: Value) {
        self.emit_trace("smart_turn", data);
    }

    async fn speak_fixed(&self, text: &str) -> bool {
        self.timing("first_synthesis_start");
        let index = self.next_sentence(text, true, false);
        let began = Instant::now();
        match self.conversation.cached_speech(text).await {
            Ok(pcm) => {
                self.timing("first_synthesis_ready");
                self.trace_tts_ready(index, began);
                self.send(TurnEvent::Speech {
                    text: text.to_owned(),
                    pcm,
                });
                self.send(TurnEvent::SentenceEnd);
                true
            }
            Err(error) => {
                self.trace_tts_failed(index, text, began, &error, false);
                self.send(TurnEvent::Failed(error));
                self.send(TurnEvent::TextReply(text.to_owned()));
                false
            }
        }
    }

    /// 新开始合成一句（`fixed` 为固定话术）：返回它在本轮是第几句；记录打开时写“开始合成第 N 句”。
    fn next_sentence(&self, text: &str, fixed: bool, fallback: bool) -> u32 {
        let index = self
            .tts_sentences
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        self.trace_tts_start(index, text, fixed, fallback);
        index
    }

    fn trace_tts_start(&self, index: u32, text: &str, fixed: bool, fallback: bool) {
        self.emit_trace(
            "tts_start",
            serde_json::json!({
                "index": index, "text": text, "fixed": fixed, "fallback": fallback,
            }),
        );
    }

    fn trace_tts_ready(&self, index: u32, began: Instant) {
        self.emit_trace(
            "tts_ready",
            serde_json::json!({
                "index": index,
                "elapsed_ms": began.elapsed().as_millis() as u64,
            }),
        );
    }

    fn trace_tts_failed(
        &self,
        index: u32,
        text: &str,
        began: Instant,
        error: &ProviderError,
        fallback: bool,
    ) {
        self.emit_trace(
            "tts_failed",
            serde_json::json!({
                "index": index,
                "text": text,
                "error": error.to_string(),
                "elapsed_ms": began.elapsed().as_millis() as u64,
                "fallback": fallback,
            }),
        );
    }

    /// 合成并送出一句话：第一段音频带文字（会话据此发 `sentence_start`），之后的分段直接追加。
    /// 返回是否送出过音频（中途失败时已送出的部分照常播放）。
    /// `fallback_of` 为主合成失败后用本地合成重来的那一句的编号（不重计首句耗时，沿用原编号）。
    fn start_sentence_tts_with(
        &self,
        tts: Arc<dyn Tts>,
        text: String,
        fallback_of: Option<u32>,
    ) -> PreparedSentence {
        let index = match fallback_of {
            Some(index) => {
                self.trace_tts_start(index, &text, false, true);
                index
            }
            None => {
                self.timing("first_synthesis_start");
                self.next_sentence(&text, false, false)
            }
        };
        let (chunks_tx, chunks_rx) = unbounded_channel::<Pcm>();
        let input = text.clone();
        let handle = tokio::spawn(async move { tts.synthesize_stream(&input, chunks_tx).await });
        PreparedSentence {
            text,
            chunks_rx,
            handle: AbortOnDrop::new(handle),
            index,
            began: Instant::now(),
        }
    }

    fn start_sentence_tts(&self, text: String) -> PreparedSentence {
        self.start_sentence_tts_with(self.conversation.tts.clone(), text, None)
    }

    /// 消费一句已启动的 TTS 音频流（首尾 12ms 线性淡入淡出 + 句尾冗余静音裁剪）；
    /// 若传入 `sentence_rx` 与 `lookahead`，则在等待当前句音频块的同时后台并发启动下一句 TTS 预取。
    async fn drain_prepared_sentence(
        &self,
        mut current: PreparedSentence,
        mut sentence_rx: Option<&mut tokio::sync::mpsc::Receiver<String>>,
        mut lookahead: Option<&mut Option<PreparedSentence>>,
        sentence_rx_closed: &mut bool,
        allow_fallback: bool,
    ) -> bool {
        let (index, began) = (current.index, current.began);
        let sentence = current.text.clone();
        let (clause_keep_ms, clause_trigger_ms) = crate::audio::clause_silence_bounds_ms(&sentence);
        let mut pending_text = Some(current.text);
        let mut held_tail: Vec<i16> = Vec::new();
        let mut sample_rate: u32 = 0;
        let mut head_faded = false;
        let mut saw_active = false;
        let mut emitted_trailing_silent: usize = 0;

        loop {
            let can_prefetch =
                lookahead.as_ref().is_some_and(|slot| slot.is_none()) && !*sentence_rx_closed;
            let maybe_pcm = if can_prefetch && let Some(rx) = sentence_rx.as_deref_mut() {
                tokio::select! {
                    biased;
                    pcm = current.chunks_rx.recv() => pcm,
                    next = rx.recv() => {
                        match next {
                            Some(next_sentence) => {
                                if let Some(slot) = lookahead.as_deref_mut() {
                                    *slot = Some(self.start_sentence_tts(next_sentence));
                                }
                            }
                            None => {
                                *sentence_rx_closed = true;
                            }
                        }
                        continue;
                    }
                }
            } else {
                current.chunks_rx.recv().await
            };

            let Some(pcm) = maybe_pcm else {
                break;
            };
            if pcm.samples.is_empty() || pcm.sample_rate == 0 {
                continue;
            };
            sample_rate = pcm.sample_rate;
            let fade_samples = ((u64::from(sample_rate)
                * u64::from(crate::audio::BOUNDARY_FADE_MS))
                / 1000) as usize;
            let keep_samples =
                ((u64::from(sample_rate) * u64::from(clause_keep_ms)) / 1000) as usize;
            let max_silence_hold = ((u64::from(sample_rate)
                * u64::from(crate::audio::MAX_STREAM_SILENCE_HOLD_MS))
                / 1000) as usize;
            let mut combined = if held_tail.is_empty() {
                pcm.samples
            } else {
                let mut buf = std::mem::take(&mut held_tail);
                buf.extend_from_slice(&pcm.samples);
                buf
            };
            let trailing_silent = crate::audio::trailing_silence_samples(&combined);
            // 全部有声采样及累计至多 128ms（CLAUSE_SILENCE_KEEP_MS - 12ms）自然气口立即放行，不卡住有声字尾；
            // 仅暂存 12ms 淡出尾窗与超出 keep_samples 的多余静音（已收口整句流不限长度，实时流式上限 1000ms），
            // 在整句真正结束时一次性裁掉句尾超长死寂。
            let total_trailing_silent = if trailing_silent < combined.len() {
                trailing_silent
            } else if saw_active {
                emitted_trailing_silent.saturating_add(combined.len())
            } else {
                0
            };
            let excess_silent = total_trailing_silent.saturating_sub(keep_samples);
            let capped_excess = if current.chunks_rx.is_closed() && current.chunks_rx.is_empty() {
                excess_silent
            } else {
                excess_silent.min(max_silence_hold)
            };
            let hold_samples = fade_samples.saturating_add(capped_excess);
            if hold_samples > 0 {
                if combined.len() <= hold_samples {
                    held_tail = combined;
                    continue;
                }
                if !head_faded {
                    crate::audio::fade_head_i16(
                        &mut combined,
                        sample_rate,
                        crate::audio::BOUNDARY_FADE_MS,
                    );
                    head_faded = true;
                }
                let split_at = combined.len() - hold_samples;
                held_tail = combined.split_off(split_at);
                let emitted_tail_silent = crate::audio::trailing_silence_samples(&combined);
                if emitted_tail_silent < combined.len() {
                    saw_active = true;
                    emitted_trailing_silent = emitted_tail_silent;
                } else if saw_active {
                    emitted_trailing_silent =
                        emitted_trailing_silent.saturating_add(combined.len());
                }
                let emit_pcm = Pcm {
                    samples: combined,
                    sample_rate,
                };
                match pending_text.take() {
                    Some(text) => {
                        self.timing("first_synthesis_ready");
                        self.trace_tts_ready(index, began);
                        self.send(TurnEvent::Speech {
                            text,
                            pcm: emit_pcm,
                        });
                    }
                    None => self.send(TurnEvent::SpeechMore(emit_pcm)),
                }
            } else {
                if !head_faded {
                    crate::audio::fade_head_i16(
                        &mut combined,
                        sample_rate,
                        crate::audio::BOUNDARY_FADE_MS,
                    );
                    head_faded = true;
                }
                let emitted_tail_silent = crate::audio::trailing_silence_samples(&combined);
                if emitted_tail_silent < combined.len() {
                    saw_active = true;
                    emitted_trailing_silent = emitted_tail_silent;
                } else if saw_active {
                    emitted_trailing_silent =
                        emitted_trailing_silent.saturating_add(combined.len());
                }
                let emit_pcm = Pcm {
                    samples: combined,
                    sample_rate,
                };
                match pending_text.take() {
                    Some(text) => {
                        self.timing("first_synthesis_ready");
                        self.trace_tts_ready(index, began);
                        self.send(TurnEvent::Speech {
                            text,
                            pcm: emit_pcm,
                        });
                    }
                    None => self.send(TurnEvent::SpeechMore(emit_pcm)),
                }
            }
        }

        if !held_tail.is_empty() && sample_rate > 0 {
            let tail_silent = crate::audio::trailing_silence_samples(&held_tail);
            if tail_silent < held_tail.len() {
                crate::audio::trim_trailing_silence_i16(
                    &mut held_tail,
                    sample_rate,
                    clause_keep_ms,
                    clause_trigger_ms,
                );
            } else if saw_active {
                let trigger_samples =
                    ((u64::from(sample_rate) * u64::from(clause_trigger_ms)) / 1000) as usize;
                let keep_samples =
                    ((u64::from(sample_rate) * u64::from(clause_keep_ms)) / 1000) as usize;
                let fade_samples = ((u64::from(sample_rate)
                    * u64::from(crate::audio::BOUNDARY_FADE_MS))
                    / 1000) as usize;
                if emitted_trailing_silent.saturating_add(held_tail.len()) > trigger_samples {
                    let remain_keep = keep_samples.saturating_sub(emitted_trailing_silent);
                    let target_len = remain_keep.max(fade_samples).min(held_tail.len());
                    held_tail.truncate(target_len);
                }
            }
            if !held_tail.is_empty() {
                if !head_faded {
                    crate::audio::fade_head_i16(
                        &mut held_tail,
                        sample_rate,
                        crate::audio::BOUNDARY_FADE_MS,
                    );
                }
                crate::audio::fade_tail_i16(
                    &mut held_tail,
                    sample_rate,
                    crate::audio::BOUNDARY_FADE_MS,
                );
                let tail_pcm = Pcm {
                    samples: held_tail,
                    sample_rate,
                };
                match pending_text.take() {
                    Some(text) => {
                        self.timing("first_synthesis_ready");
                        self.trace_tts_ready(index, began);
                        self.send(TurnEvent::Speech {
                            text,
                            pcm: tail_pcm,
                        });
                    }
                    None => self.send(TurnEvent::SpeechMore(tail_pcm)),
                }
            }
        }

        if let Some(Ok(Err(error))) = current.handle.join().await {
            let will_fallback = allow_fallback
                && self.conversation.fallback_tts.is_some()
                && pending_text.is_some();
            self.trace_tts_failed(index, &sentence, began, &error, will_fallback);
            if allow_fallback
                && let Some(fallback) = self.conversation.fallback_tts.clone()
                && let Some(retry_text) = pending_text.take()
            {
                self.send(TurnEvent::Warning(
                    error
                        .clone()
                        .prefixed("主语音合成失败，本句已自动降级为本地语音合成："),
                ));
                let fb_prepared = self.start_sentence_tts_with(fallback, retry_text, Some(index));
                let mut fb_closed = true;
                return Box::pin(self.drain_prepared_sentence(
                    fb_prepared,
                    None,
                    None,
                    &mut fb_closed,
                    false,
                ))
                .await;
            }
            self.send(TurnEvent::Failed(error));
            if let Some(text) = pending_text.take() {
                // 这句一点声音都没合成出来、也没有备用合成：至少把文字给设备（与 speak_fixed / fast_answer 一致，n114），
                // 不让用户什么都收不到，历史里却记着说过
                self.send(TurnEvent::TextReply(text));
                return false;
            }
        }
        let emitted = pending_text.is_none();
        if emitted {
            self.send(TurnEvent::SentenceEnd);
        }
        emitted
    }

    async fn speak(&self, text: String) -> bool {
        let prepared = self.start_sentence_tts(text);
        let mut closed = true;
        self.drain_prepared_sentence(prepared, None, None, &mut closed, true)
            .await
    }

    /// 识别用户的话：有流式识别时取它的结果，失败则用整段音频再识别一次。
    async fn transcribe(
        &self,
        pcm: &[i16],
        sample_rate: u32,
        stream: Option<AsrStream>,
    ) -> Result<String, ProviderError> {
        if let Some(stream) = stream {
            let denoise = stream.denoise_slot();
            let finished = stream.finish().await;
            // 只在流式识别成功时记（失败改整段识别时这一路的降噪情况与最终文字无关）；会话在中间结果时已取走的不重复记
            if finished.is_ok()
                && self.trace_on()
                && let Some(report) = denoise.lock().ok().and_then(|mut slot| slot.take())
            {
                self.emit_trace("denoise", crate::audio::denoise_trace("识别", &report));
            }
            match finished {
                Ok(text) => return Ok(text),
                Err(error) => self.send(TurnEvent::Warning(
                    error.prefixed("流式识别失败，改用整段识别："),
                )),
            }
        }
        self.conversation.asr.transcribe(pcm, sample_rate).await
    }

    /// 让模型生成一轮回复，边生成边切句合成，等句子都合成完才返回。返回（回复或错误，是否已朗读过内容，
    /// 送去朗读的正文）。正文已去掉 `<think>` 思考内容（n114）；大模型中途出错时，调用方用它把已经说出去的半句记进历史。
    async fn generate(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        emotion_sent: &mut bool,
    ) -> (
        Result<moss_voice_providers::LlmReply, ProviderError>,
        bool,
        String,
    ) {
        let (reply, speaking) = self.generate_split(messages, tools, emotion_sent).await;
        let (streamed, spoke) = speaking.await;
        (reply, spoke, streamed)
    }

    /// 同 [`Self::generate`]，但大模型一回完就返回（n115）：还没合成完的句子作为 [`PendingSpeech`] 交给调用方，
    /// 回复带工具调用时可以先开跑工具、再一起等它，不用等开场白合成完。调用方必须在这一轮发出下一件事
    /// （下一次请求大模型、固定话术、结束）之前等完它。
    async fn generate_split<'s>(
        &'s self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        emotion_sent: &'s mut bool,
    ) -> (
        Result<moss_voice_providers::LlmReply, ProviderError>,
        PendingSpeech<'s>,
    ) {
        let (delta_tx, mut delta_rx) = unbounded_channel::<String>();
        let llm = self.conversation.llm.clone();
        let round = self
            .llm_rounds
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let trace_on = self.trace_on();
        if trace_on {
            // 请求发出时就记：发给模型的完整内容（系统提示词、历史、本句、工具定义）
            self.emit_trace(
                "llm_request",
                serde_json::json!({
                    "round": round,
                    "message_count": messages.len(),
                    "tool_count": tools.len(),
                    "messages": messages,
                    "tools": tools.iter().map(ToolSpec::to_openai).collect::<Vec<_>>(),
                }),
            );
        }
        let trace_began = Instant::now();
        // 这一轮大模型吐出第一个字的时刻（毫秒，从请求发出算起）；没吐字为 u64::MAX
        let first_delta_ms = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
        let first_delta = &*first_delta_ms;
        let speaking_first_delta = first_delta_ms.clone();
        let generation = async move {
            let reply = llm.chat(messages, tools, delta_tx).await;
            self.timing_overwrite("llm_done");
            if trace_on {
                // 回复一完整就记（不等合成播完），耗时只含大模型本身
                let first = first_delta.load(std::sync::atomic::Ordering::SeqCst);
                self.emit_trace(
                    "llm_round",
                    serde_json::json!({
                        "round": round,
                        "first_token_ms": (first != u64::MAX).then_some(first),
                        "elapsed_ms": trace_began.elapsed().as_millis() as u64,
                        "message_count": messages.len(),
                        "reply_text": reply.as_ref().ok().map(|r| r.text.clone()),
                        "reply_tool_calls": reply.as_ref().ok().map(|r| r.tool_calls.clone()),
                        "error": reply.as_ref().err().map(ToString::to_string),
                    }),
                );
            }
            reply
        };
        let speaking = async move {
            let first_delta = &*speaking_first_delta;
            let (sentence_tx, mut sentence_rx) = tokio::sync::mpsc::channel::<String>(4);
            let feeder = async {
                let sentence_tx = sentence_tx;
                let mut segmenter = Segmenter::new();
                let mut think = ThinkFilter::default();
                let mut streamed = String::new();
                let mut announced = false;
                while let Some(raw) = delta_rx.recv().await {
                    self.timing("first_llm_delta");
                    let _ = first_delta.compare_exchange(
                        u64::MAX,
                        trace_began.elapsed().as_millis() as u64,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    let delta = think.push(&raw);
                    if delta.is_empty() {
                        continue;
                    }
                    streamed.push_str(&delta);
                    if !*emotion_sent && let Some(emotion) = emotion_of(&delta) {
                        *emotion_sent = true;
                        self.send(TurnEvent::Emotion(emotion));
                    }
                    for sentence in segmenter.push(&delta, Instant::now().into_std()) {
                        self.timing("first_sentence");
                        if !std::mem::replace(&mut announced, true) {
                            self.send(TurnEvent::SpeechSoon);
                        }
                        if sentence_tx.send(sentence).await.is_err() {
                            return streamed;
                        }
                    }
                }
                let tail = think.finish();
                if !tail.is_empty() {
                    streamed.push_str(&tail);
                    for sentence in segmenter.push(&tail, Instant::now().into_std()) {
                        if sentence_tx.send(sentence).await.is_err() {
                            return streamed;
                        }
                    }
                }
                if let Some(rest) = segmenter.finish() {
                    self.timing("first_sentence");
                    if !announced {
                        self.send(TurnEvent::SpeechSoon);
                    }
                    let _ = sentence_tx.send(rest).await;
                }
                streamed
            };
            let tts_consumer = async {
                let mut spoke = false;
                let mut lookahead: Option<PreparedSentence> = None;
                let mut rx_closed = false;
                loop {
                    let next_prepared = if let Some(prefetched) = lookahead.take() {
                        Some(prefetched)
                    } else if rx_closed {
                        None
                    } else {
                        match sentence_rx.recv().await {
                            Some(sentence) => Some(self.start_sentence_tts(sentence)),
                            None => {
                                rx_closed = true;
                                None
                            }
                        }
                    };
                    let Some(current) = next_prepared else {
                        break;
                    };
                    spoke |= self
                        .drain_prepared_sentence(
                            current,
                            Some(&mut sentence_rx),
                            Some(&mut lookahead),
                            &mut rx_closed,
                            true,
                        )
                        .await;
                }
                spoke
            };
            tokio::join!(feeder, tts_consumer)
        };
        let mut speaking: PendingSpeech<'s> = Box::pin(speaking);
        tokio::pin!(generation);
        let mut finished = None;
        let reply = loop {
            tokio::select! {
                reply = &mut generation => break reply,
                out = &mut speaking, if finished.is_none() => finished = Some(out),
            }
        };
        let reply = reply.map(|mut reply| {
            reply.text = strip_think(&reply.text);
            reply
        });
        let rest = match finished {
            Some(out) => Box::pin(std::future::ready(out)),
            None => speaking,
        };
        (reply, rest)
    }

    async fn run_tools(&self, calls: &[ToolCall]) -> Vec<(ToolCall, ToolOutcome)> {
        self.tools_future(calls.to_vec()).await
    }

    /// 立即开始执行这批工具，返回一个不借用本轮的等待结果的 future（可以交给后台任务继续等）。
    fn tools_future(
        &self,
        calls: Vec<ToolCall>,
    ) -> impl std::future::Future<Output = Vec<(ToolCall, ToolOutcome)>> + Send + 'static {
        let mut set = tokio::task::JoinSet::new();
        let trace_on = self.trace_on();
        for (index, call) in calls.iter().cloned().enumerate() {
            let description = if trace_on {
                self.tool_descriptions
                    .lock()
                    .ok()
                    .and_then(|map| map.get(&call.function.name).cloned())
            } else {
                None
            };
            let tools = self.conversation.tools.clone();
            let device = self.device.clone();
            let context = self.context.clone();
            let timeout = self.conversation.settings.tool_timeout;
            let events = self.events.clone();
            let turn_trace = self.turn_trace.clone();
            let simulated = self.simulated;
            let live_bench_window = self.live_bench_window.clone();
            set.spawn(async move {
                let began = Instant::now();
                let device_target = device
                    .as_ref()
                    .filter(|_| call.function.name != "play_music")
                    .and_then(|device| {
                        device
                            .original(&call.function.name)
                            .map(|name| (device, name))
                    });
                let effective_simulated =
                    simulated || live_bench_window.as_ref().is_some_and(|check| check());
                let blocked_reason = if effective_simulated {
                    let trusted_ro = tools.trusted_read_only(&call.function.name);
                    crate::sim_guard::check_simulated_tool(
                        &call.function.name,
                        device_target.map(|(_, original)| original),
                        &call.function.arguments,
                        trusted_ro,
                    )
                    .err()
                } else {
                    None
                };
                let outcome = if let Some(ref reason) = blocked_reason {
                    ToolOutcome::Reply(crate::sim_guard::blocked_tool_reply(
                        &call.function.name,
                        reason,
                    ))
                } else {
                    let work = async {
                        match device_target {
                            Some((device, original)) => {
                                device.call(original, &call.function.arguments).await
                            }
                            None => {
                                tools
                                    .call(&context, &call.function.name, &call.function.arguments)
                                    .await
                            }
                        }
                    };
                    tokio::time::timeout(timeout, work)
                        .await
                        .unwrap_or_else(|_| {
                            ToolOutcome::Reply(format!("工具 {} 执行超时", call.function.name))
                        })
                };
                let elapsed_ms = began.elapsed().as_millis() as u64;
                let _ = events.send(TurnEvent::ToolTiming {
                    name: call.function.name.clone(),
                    ms: elapsed_ms,
                });
                let out_preview = match &outcome {
                    ToolOutcome::Reply(s) | ToolOutcome::Speak(s) => s.chars().take(1200).collect(),
                    ToolOutcome::PlayMusic { title, .. } => format!("播放本地音乐：{title}"),
                };
                let _ = events.send(TurnEvent::ToolDebug {
                    name: call.function.name.clone(),
                    arguments: call.function.arguments.chars().take(1200).collect(),
                    result: out_preview,
                    ms: elapsed_ms,
                    blocked: blocked_reason.is_some(),
                    block_reason: blocked_reason.clone(),
                });
                if trace_on {
                    let (result_kind, result) = match &outcome {
                        ToolOutcome::Reply(s) => ("reply", s.clone()),
                        ToolOutcome::Speak(s) => ("speak", s.clone()),
                        ToolOutcome::PlayMusic { title, .. } => ("play_music", title.clone()),
                    };
                    let data = serde_json::json!({
                        "name": call.function.name,
                        "description": description,
                        "call_id": call.id,
                        "arguments": call.function.arguments,
                        "result_kind": result_kind,
                        "result": result,
                        "elapsed_ms": elapsed_ms,
                        "blocked": blocked_reason.is_some(),
                        "block_reason": blocked_reason,
                    });
                    if let Some(ref tt) = turn_trace {
                        let _ = tt.emit(&context, "tool_call", data);
                    } else {
                        let _ = events.send(TurnEvent::Trace {
                            kind: "tool_call",
                            data,
                        });
                    }
                }
                (index, call.id, outcome)
            });
        }
        async move {
            let mut outcomes: Vec<Option<ToolOutcome>> = vec![None; calls.len()];
            while let Some(joined) = set.join_next().await {
                if let Ok((index, _, outcome)) = joined {
                    outcomes[index] = Some(outcome);
                }
            }
            // 工具任务 panic 时没有结果：补“执行失败”，保证每个调用都有对应结果（否则模型接口会拒绝下一轮请求）
            calls
                .into_iter()
                .zip(outcomes)
                .map(|(call, outcome)| {
                    let outcome = outcome.unwrap_or_else(|| {
                        ToolOutcome::Reply(format!("工具 {} 执行失败", call.function.name))
                    });
                    (call, outcome)
                })
                .collect()
        }
    }

    /// 命令词模型听到了停止 / 退出命令、通用识别却没认出来（噪声里常把“退下”听成“问一下”、“停”听成别的单字）：
    /// 识别结果很短（不超过 4 个字，含没识别出字）时按命令词纠正；长句一律以识别为准，不让命令词模型盖过整句话。
    /// 返回（最终文字，命令词模型改判时听到的命令类别）。
    async fn fuse_command_hint(
        &self,
        text: String,
    ) -> (String, Option<moss_voice_providers::local::CommandKeyword>) {
        use moss_voice_providers::local::CommandKeyword;
        let Some(gate) = self.gate.as_ref() else {
            return (text, None);
        };
        if crate::fast::is_stop(&text) || self.conversation.is_exit(&text) {
            return (text, None);
        }
        let normalized = crate::fast::normalized(&text);
        let read = || gate.command_hint.lock().ok().and_then(|slot| *slot);
        let mut hint = read();
        // 命令词模型要在词说完后再听一小段才出结果，常比识别晚到：识别结果像被听错的命令时最多等一会儿
        // （“好的”这类不像命令的不等，不拖慢正常对话）
        if hint.is_none()
            && self.conversation.settings.command_keywords
            && (command_hint_fits(CommandKeyword::Stop, &normalized)
                || command_hint_fits(CommandKeyword::Exit, &normalized))
        {
            let mut deadline = std::time::Instant::now() + COMMAND_HINT_WAIT;
            if let Some(tail_end) = gate.command_hint_until {
                deadline = deadline.min(tail_end + COMMAND_HINT_IPC_MARGIN);
            }
            while hint.is_none() && std::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
                hint = read();
            }
        }
        let Some(kind) = hint else {
            return (text, None);
        };
        if !command_hint_fits(kind, &normalized) {
            return (text, None);
        }
        let fused = match kind {
            CommandKeyword::Stop => "停",
            CommandKeyword::Exit => "退下",
        };
        (fused.into(), Some(kind))
    }

    /// 后台查询结果出来后的主动播报：不给工具，只让大模型把结果说成一两句话。
    async fn report(&self, question: String, results: String, history: Vec<ChatMessage>) {
        let settings = &self.conversation.settings;
        let mut system = build_turn_system_prompt(
            &self.conversation.system_prompt(),
            settings.include_local_clock,
            true,
        );
        system.push('\n');
        system.push_str(BACKGROUND_REPORT_GUIDE);
        let mut messages = vec![ChatMessage::system(system)];
        messages.extend(compact_stale_tool_results(&history, 2));
        messages.push(ChatMessage::system(format!(
            "后台查询完成。用户之前问：{question}\n查询结果：\n{results}"
        )));
        let mut emotion_sent = false;
        let (reply, spoke, streamed) = self.generate(&messages, &[], &mut emotion_sent).await;
        let messages = match reply {
            Ok(reply) if !reply.text.trim().is_empty() => vec![ChatMessage::assistant(reply.text)],
            Ok(_) => Vec::new(),
            Err(error) => {
                let reply_text = spoken_error_reply(&settings.error_reply, ErrorStage::Llm, &error);
                self.send(TurnEvent::Failed(error));
                self.speak_fixed(reply_text).await;
                if spoke && !streamed.trim().is_empty() {
                    vec![ChatMessage::assistant(streamed)]
                } else {
                    Vec::new()
                }
            }
        };
        self.send(TurnEvent::Done {
            messages,
            close: false,
        });
    }

    async fn answer(
        &self,
        pcm: Vec<i16>,
        sample_rate: u32,
        stream: Option<AsrStream>,
        history: Vec<ChatMessage>,
    ) {
        let settings = &self.conversation.settings;
        let mut smart_turn_task = if self
            .gate
            .as_ref()
            .is_some_and(|g| g.speculative && !g.hold_llm)
            && !pcm.is_empty()
            && sample_rate > 0
            && let Some(predictor) = self.conversation.turn_predictor.clone()
        {
            let max_samples = (sample_rate as usize).saturating_mul(8);
            let tail = pcm[pcm.len().saturating_sub(max_samples)..].to_vec();
            // 推理耗时在任务里量（不含识别与命令词等待），随结果一起交回
            Some(tokio::spawn(async move {
                let began = Instant::now();
                let prob = predictor.predict_turn_end(&tail, sample_rate).await;
                (prob, began.elapsed().as_millis() as u64)
            }))
        } else {
            None
        };
        let identity = if self.simulated {
            None
        } else {
            self.conversation
                .personal_context
                .as_ref()
                .map(|context| context.start(&self.context, &pcm, sample_rate))
        };
        let text = match self.transcribe(&pcm, sample_rate, stream).await {
            Ok(text) => text,
            Err(error) => {
                if let Some(task) = smart_turn_task.take() {
                    task.abort();
                }
                let reply_text = spoken_error_reply(&settings.error_reply, ErrorStage::Asr, &error);
                self.send(TurnEvent::Failed(error));
                self.speak_fixed(reply_text).await;
                self.send(TurnEvent::Done {
                    messages: Vec::new(),
                    close: false,
                });
                return;
            }
        };
        self.timing("final_asr");
        self.answer_from_text(text, history, smart_turn_task, identity)
            .await;
    }

    /// 识别出最终文字（或 VA1 文字直通注入）之后的统一对话链路：
    /// 同音纠偏 → 命令词融合 → 噪声过滤 → 下发 `stt` → 快路径（停止 / 只读 / 退出）→ Jev 筛选 → 大模型与工具 → TTS。
    async fn answer_from_text(
        &self,
        raw_text: String,
        mut history: Vec<ChatMessage>,
        mut smart_turn_task: Option<tokio::task::JoinHandle<(Option<f32>, u64)>>,
        mut identity: Option<crate::person_context::IdentityTask>,
    ) {
        let settings = &self.conversation.settings;
        let homophone = apply_asr_corrections(&raw_text, &settings.asr_corrections);
        let (text, keyword) = self.fuse_command_hint(homophone.clone()).await;
        if !moss_voice_providers::text::speakable(&text) || crate::fast::is_noise_filler(&text) {
            if let Some(task) = smart_turn_task.take() {
                task.abort();
            }
            if self.trace_on() {
                // 被丢掉的识别结果也如实记下原文和原因（不进识别结果那一步，不交给大模型）
                let reason = if moss_voice_providers::text::speakable(&text) {
                    "只是语气词或噪声词"
                } else {
                    "没有可读的文字"
                };
                self.emit_trace(
                    "asr_ignored",
                    serde_json::json!({
                        "raw": raw_text,
                        "homophone": homophone,
                        "corrected": text,
                        "reason": reason,
                    }),
                );
            }
            self.send(TurnEvent::Route("ignored"));
            self.send(TurnEvent::Ignored);
            return;
        }
        if crate::fast::is_stop(&text) || self.conversation.is_exit(&text) {
            let had_task = smart_turn_task.take().inspect(|t| t.abort()).is_some();
            if had_task || self.conversation.turn_predictor.is_some() {
                self.trace_smart_turn(serde_json::json!({
                    "reason": "是停止 / 退出说法，不等语义判停、立即处理",
                }));
            }
        } else if let Some(mut task) = smart_turn_task.take() {
            const SMART_TURN_BUDGET: Duration = Duration::from_millis(250);
            let waiting = Instant::now();
            match tokio::time::timeout(SMART_TURN_BUDGET, &mut task).await {
                Ok(Ok((Some(prob), inference_ms))) if prob.is_finite() => {
                    let prob = prob.clamp(0.0, 1.0);
                    self.timing("smart_turn");
                    self.trace_smart_turn(serde_json::json!({
                        "probability": prob,
                        "elapsed_ms": inference_ms,
                        "waited_after_asr_ms": waiting.elapsed().as_millis() as u64,
                    }));
                    self.send(TurnEvent::SmartTurnProb(prob));
                }
                outcome => {
                    task.abort();
                    let (reason, inference_ms) = match outcome {
                        Err(_) => (
                            format!(
                                "识别完后再等 {} ms 仍没算完，超出时间预算已放弃",
                                SMART_TURN_BUDGET.as_millis()
                            ),
                            None,
                        ),
                        Ok(Ok((Some(_), ms))) => {
                            ("模型给出的概率不是有效数字".to_owned(), Some(ms))
                        }
                        Ok(Ok((None, ms))) => ("模型没给出结果（出错或超时）".to_owned(), Some(ms)),
                        Ok(Err(_)) => ("判停任务异常结束".to_owned(), None),
                    };
                    self.trace_smart_turn(serde_json::json!({
                        "reason": reason,
                        "elapsed_ms": inference_ms,
                        "waited_after_asr_ms": waiting.elapsed().as_millis() as u64,
                        "budget_ms": SMART_TURN_BUDGET.as_millis() as u64,
                    }));
                }
            }
        } else if self.conversation.turn_predictor.is_some() {
            let reason = if self.gate.as_ref().is_some_and(|g| g.hold_llm) {
                "插话待判轮次，不跑尾音语义判停"
            } else {
                "非静音判停抢跑（按键说话或设备已结束拾音），不用语义判停"
            };
            self.trace_smart_turn(serde_json::json!({"reason": reason}));
        }
        if self.trace_on() {
            // 识别原文 → 同音纠正 → 命令词改判，在发生当刻先于后续 Jev / 大模型请求写入（n110）
            self.emit_trace(
                "asr",
                serde_json::json!({
                    "raw": &raw_text,
                    "homophone": &homophone,
                    "corrected": &text,
                    "kws_fused": keyword.is_some(),
                    "kws_keyword": keyword.map(keyword_label),
                }),
            );
        }
        self.send(TurnEvent::RawHeard {
            raw: raw_text,
            homophone,
            corrected: text.clone(),
            keyword,
        });
        self.send(TurnEvent::Heard(text.clone()));
        if self.fast_answer(&text, &mut identity, &mut history).await {
            return;
        }
        if self.conversation.is_exit(&text) {
            self.send(TurnEvent::Route("fast_exit"));
            self.send(TurnEvent::Closing);
            self.speak_fixed(&settings.goodbye).await;
            self.send(TurnEvent::Done {
                messages: Vec::new(),
                close: true,
            });
            return;
        }
        if let Some(gate) = self.gate.as_ref().filter(|gate| gate.hold_llm) {
            // 这一轮是对正在播的回答的插话：会话确认它不是附和之前不请求大模型；
            // 确认后被打断的回答才进历史，用会话给的最新历史
            self.wait_release().await;
            if let Some(fresh) = gate
                .fresh_history
                .lock()
                .ok()
                .and_then(|mut slot| slot.take())
            {
                history = fresh;
            }
        }
        // Stop and exit shortcuts above never wait for speaker work. Unknown
        // identity has no personal history or fallback device summary.
        // 模拟轮次没有真人录音，绝不关联声纹身份或读取/污染按人记忆，仅沿用当前会话内存历史支持多轮追问。
        let scope = if self.simulated {
            let _ = identity.take();
            None
        } else {
            let Ok(scope) = self.resolve_person(identity).await else {
                return;
            };
            scope
        };
        let (history, personal_turn_prefix) = if self.simulated {
            (history, Vec::new())
        } else if let Some(personal) = &self.conversation.personal_context {
            (personal.history(&self.context, scope.as_ref()), history)
        } else {
            (history, Vec::new())
        };
        let tool_executor = self.conversation.tools.clone();
        let mut tools = tokio::task::spawn_blocking(move || tool_executor.tools())
            .await
            .unwrap_or_default();
        if let Some(device) = &self.device {
            // Reserved play_music is always owned by the trusted desktop index,
            // never by an untrusted device MCP advertisement.
            tools.extend(
                device
                    .specs()
                    .iter()
                    .filter(|spec| spec.name != "play_music")
                    .cloned(),
            );
        }
        let mut messages = Vec::with_capacity(history.len() + 4);
        let mut system = build_turn_system_prompt(
            &self.conversation.system_prompt(),
            settings.include_local_clock,
            !history.is_empty(),
        );
        // n100：最近被打断过，说明标记的含义
        if history.iter().rev().take(8).any(|message| {
            message.role == moss_voice_providers::Role::Assistant
                && message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains(INTERRUPTED_MARK))
        }) {
            system.push('\n');
            system.push_str(INTERRUPTED_GUIDE);
        }
        // n72：激光工具在场时附上“说法 → 工具”用法表
        if tools.iter().any(|tool| tool.name == "laser_workflow_tool") {
            system.push('\n');
            system.push_str(LASER_TOOL_GUIDE);
        }
        messages.push(ChatMessage::system(system));

        // n57: once structured memory retrieval is on, the old NV10 per-device summary (imported
        // as candidates when the switch was turned on) is no longer injected next to it.
        let nv10_summary = !self
            .conversation
            .context_retrieval
            .as_ref()
            .is_some_and(|retrieval| retrieval.retriever.enabled());
        let active_jev = self
            .conversation
            .jev_filter
            .as_ref()
            .filter(|f| f.options().active());

        if let Some(jev) = active_jev {
            // 先取本地 SQLite 粗召回候选（~2ms，受 FAST_TIMEOUT 约束），使同一次 Jev 扇出顺手完成记忆撞词过滤；
            // 同时与个人/设备摘要读取并发执行，不产生额外串行等待。
            let retrieved = match &self.conversation.context_retrieval {
                Some(retrieval) => match retrieval.access.access(&self.context, scope.as_ref()) {
                    Some((access, device)) => {
                        retrieval
                            .retriever
                            .retrieve_candidates(access, &text, device)
                            .await
                    }
                    None => None,
                },
                None => None,
            };
            let mem_texts = retrieved
                .as_ref()
                .map(|r| r.candidate_texts())
                .unwrap_or_default();
            let summary_fut = async {
                if let Some(personal) = &self.conversation.personal_context {
                    personal.prompt_for(&self.context, scope.as_ref()).await
                } else if let Some(memory) =
                    self.conversation.memory.as_ref().filter(|_| nv10_summary)
                {
                    memory.prompt_for(&self.context.device_id).await
                } else {
                    None
                }
            };
            let jev_fut = jev.evaluate_turn(&text, &history, &tools, &mem_texts);
            let (summary_block, decision) = tokio::join!(summary_fut, jev_fut);
            if decision.applied {
                self.timing("jev_filter");
            }
            if self.trace_on() {
                let kept = decision.filter_tools(&tools, &text, &history);
                self.emit_trace(
                    "jev",
                    serde_json::json!({
                        "applied": decision.applied,
                        "cached": decision.cached,
                        "unfinished": decision.unfinished,
                        "fast_read": decision.fast_read.as_ref().map(|intent| format!("{intent:?}")),
                        "allowed_groups": decision.allowed_groups.as_ref().map(|groups| {
                            groups.iter().map(|g| format!("{g:?}")).collect::<Vec<_>>()
                        }),
                        "keep_memories": decision.keep_memories,
                        "tools_before": tools.len(),
                        "tools_after": kept.len(),
                    }),
                );
            }
            if settings.read_only_shortcuts
                && let Some(intent) = decision.fast_read
            {
                self.send(TurnEvent::Route("fast_read"));
                self.timing("fast_route");
                let reply = self.read_reply(intent).await;
                if !self.speak(reply.clone()).await {
                    self.send(TurnEvent::TextReply(reply.clone()));
                }
                let mut done_msgs = personal_turn_prefix;
                done_msgs.push(ChatMessage::user(text));
                done_msgs.push(ChatMessage::assistant(reply));
                self.send(TurnEvent::Done {
                    messages: done_msgs,
                    close: false,
                });
                return;
            }
            tools = decision.filter_tools(&tools, &text, &history);
            if let Some(context) = summary_block {
                messages.push(ChatMessage::system(context));
            }
            if let Some(candidates) = retrieved
                && let Some(context) = candidates.render(decision.keep_memories.as_deref())
            {
                messages.push(ChatMessage::system(context));
            }
        } else {
            if let Some(personal) = &self.conversation.personal_context {
                if let Some(context) = personal.prompt_for(&self.context, scope.as_ref()).await {
                    messages.push(ChatMessage::system(context));
                }
            } else if let Some(memory) = self.conversation.memory.as_ref().filter(|_| nv10_summary)
                && let Some(context) = memory.prompt_for(&self.context.device_id).await
            {
                messages.push(ChatMessage::system(context));
            }
            // NV19-04: bounded, labelled read-only data; skipped for small talk; never blocks
            // beyond its own short timeout and adds nothing on any failure.
            if let Some(retrieval) = &self.conversation.context_retrieval
                && let Some((access, device)) =
                    retrieval.access.access(&self.context, scope.as_ref())
                && let Some(context) = retrieval.retriever.prompt_for(access, &text, device).await
            {
                messages.push(ChatMessage::system(context));
            }
        }
        messages.extend(compact_stale_tool_results(&history, 2));
        let first_new = messages.len();
        messages.extend(personal_turn_prefix);
        let question = text.clone();
        messages.push(ChatMessage::user(text));
        // 保留名 end_conversation 始终由宿主自己持有（官方服务端默认加载的 handle_exit_intent 同理）：
        // 用户换着说法表示聊完了（没命中整句退出词）时由模型调用，说完告别语后结束这一轮对话
        tools.retain(|spec| spec.name != END_CONVERSATION_TOOL && spec.name != STAY_SILENT_TOOL);
        tools.push(end_conversation_tool());
        if self.gate.as_ref().is_some_and(|gate| gate.allow_silent) {
            tools.push(stay_silent_tool());
        }
        if self.trace_on()
            && let Ok(mut map) = self.tool_descriptions.lock()
        {
            map.extend(
                tools
                    .iter()
                    .map(|spec| (spec.name.clone(), spec.description.clone())),
            );
        }
        self.send(TurnEvent::Route("llm"));
        let mut emotion_sent = false;
        let mut spoke_any = false;
        let mut filler_spoken = false;
        let mut rounds = 0;
        loop {
            if !self.simulated
                && self
                    .conversation
                    .personal_context
                    .as_ref()
                    .is_some_and(|personal| !personal.permitted(&self.context, scope.as_ref()))
            {
                self.send(TurnEvent::Done {
                    messages: vec![],
                    close: false,
                });
                return;
            }
            let (reply, speaking) = self
                .generate_split(&messages, &tools, &mut emotion_sent)
                .await;
            // 回复里既有要说的开场白又有工具调用：工具不等开场白合成完就开跑，执行工具时再一起等（n115）。
            // 有开场白就算这一轮已经开口（原先合成完才知道；只有开场白整句合成失败时两者不同）
            let overlap = reply.as_ref().is_ok_and(|reply| {
                !reply.tool_calls.is_empty() && moss_voice_providers::text::speakable(&reply.text)
            });
            let mut pending = None;
            let (spoke, streamed) = if overlap {
                pending = Some(speaking);
                (true, String::new())
            } else {
                let (streamed, spoke) = speaking.await;
                (spoke, streamed)
            };
            spoke_any |= spoke;
            let mut reply = match reply {
                Ok(reply) => reply,
                Err(error) => {
                    if spoke && !streamed.trim().is_empty() {
                        // 大模型说到一半出错：已经说出去的半句照样记进历史（n114），下一轮它才知道自己说过什么
                        messages.push(ChatMessage::assistant(streamed));
                    }
                    let reply_text =
                        spoken_error_reply(&settings.error_reply, ErrorStage::Llm, &error);
                    self.send(TurnEvent::Failed(error));
                    self.speak_fixed(reply_text).await;
                    break;
                }
            };
            if reply.tool_calls.is_empty() {
                messages.push(ChatMessage::assistant(reply.text));
                break;
            }
            rounds += 1;
            messages.push(ChatMessage::assistant_tools(
                Some(reply.text),
                reply.tool_calls.clone(),
            ));
            // 抢跑：这一轮可能在用户话还没说完时就开始了，工具调用与退出工具都可能有副作用，等会话确认说完再执行
            self.wait_release().await;
            // 不正常执行工具的几种情况（超过轮数、不接话、结束对话、点歌混调）：先把开场白说完，与原先一致
            let special = rounds > settings.max_tool_rounds
                || reply.tool_calls.iter().any(|call| {
                    call.function.name == STAY_SILENT_TOOL
                        || call.function.name == END_CONVERSATION_TOOL
                })
                || (reply.tool_calls.len() != 1
                    && reply
                        .tool_calls
                        .iter()
                        .any(|call| call.function.name == "play_music"));
            if special && let Some(speech) = pending.take() {
                speech.await;
            }
            if rounds > settings.max_tool_rounds {
                // 不执行这一轮的工具；补上结果，保证历史中每个调用都有对应结果
                for call in &reply.tool_calls {
                    messages.push(ChatMessage::tool(
                        call.id.clone(),
                        "工具调用轮数超过上限，未执行",
                    ));
                }
                let error = ProviderError::with_category(
                    moss_voice_providers::DiagCategory::Model,
                    format!("工具调用超过 {} 轮，已停止", settings.max_tool_rounds),
                )
                .hint("大模型反复调用工具没有收尾：换个说法再试，或换一个更擅长工具调用的模型");
                let reply_text =
                    spoken_error_reply(&settings.error_reply, ErrorStage::ToolLimit, &error);
                self.send(TurnEvent::Failed(error));
                self.speak_fixed(reply_text).await;
                break;
            }
            if reply
                .tool_calls
                .iter()
                .any(|call| call.function.name == STAY_SILENT_TOOL)
            {
                // 大模型判断这句不是对小智说的：只有它是这一轮的第一个决定、而且是唯一的调用时才当真——
                // 还没出声（没说话、没放过渡语）、没执行过工具，会话据此不回应或让暂停的回答接着说。
                // 混着别的工具（比如急停）时不当真：给它配一个结果，其余工具照常执行；
                // 已经出过声或执行过工具时，当这一轮正常说完（会话照常发 tts stop、记历史）。
                if rounds == 1 && !spoke_any && !filler_spoken && reply.tool_calls.len() == 1 {
                    self.send(TurnEvent::NotForMe);
                    return;
                }
                for call in reply
                    .tool_calls
                    .iter()
                    .filter(|call| call.function.name == STAY_SILENT_TOOL)
                {
                    messages.push(ChatMessage::tool(
                        call.id.clone(),
                        "未执行：这句话需要正常处理",
                    ));
                }
                reply
                    .tool_calls
                    .retain(|call| call.function.name != STAY_SILENT_TOOL);
                if reply.tool_calls.is_empty() {
                    break;
                }
            }
            if let Some(exit) = reply
                .tool_calls
                .iter()
                .find(|call| call.function.name == END_CONVERSATION_TOOL)
            {
                // 同一轮里的其他工具一律不执行（可能有副作用），补上结果保证历史配对
                for call in &reply.tool_calls {
                    messages.push(ChatMessage::tool(call.id.clone(), "对话已结束"));
                }
                self.send(TurnEvent::Closing);
                if !spoke_any {
                    let farewell = serde_json::from_str::<Value>(&exit.function.arguments)
                        .ok()
                        .and_then(|args| {
                            args["farewell"].as_str().map(str::trim).map(str::to_owned)
                        })
                        .filter(|text| !text.is_empty() && text.chars().count() <= 40)
                        .unwrap_or_else(|| settings.goodbye.clone());
                    self.speak_fixed(&farewell).await;
                }
                self.send(TurnEvent::Done {
                    messages: messages.split_off(first_new),
                    close: true,
                });
                return;
            }
            // Never mix a music playback request with any other tool in the
            // same model turn; other tools could have side effects.
            if reply
                .tool_calls
                .iter()
                .any(|call| call.function.name == "play_music")
                && reply.tool_calls.len() != 1
            {
                self.send(TurnEvent::Failed(ProviderError::new(
                    "音乐播放不能和其他工具同时执行",
                )));
                self.speak_fixed("点歌请单独说，不要同时做别的操作").await;
                self.send(TurnEvent::Done {
                    messages: Vec::new(),
                    close: false,
                });
                return;
            }
            if let Some(gate) = &self.gate
                && !gate.tool_latch.enter_tools()
            {
                // 会话已把这一轮作废、合并到新一轮重来：一个工具都不能执行，等任务被中止
                std::future::pending::<()>().await;
            }
            self.send(TurnEvent::ToolsStarted(reply.tool_calls.clone()));
            let mut direct = Vec::new();
            let mut music = None;
            let background = self.gate.as_ref().is_some_and(|gate| gate.allow_background)
                && reply.tool_calls.iter().all(|call| {
                    is_slow_query_call(call)
                        || (call.function.name.contains("__")
                            && self
                                .conversation
                                .tools
                                .trusted_read_only(&call.function.name))
                });
            let tool_outcomes = if !spoke_any && !filler_spoken && rounds == 1 && background {
                // 只读慢查询（联网搜索、天气、新闻、知识库）：1 秒内返回就照常回答；
                // 再慢就交给后台继续查，这一轮先说一句就结束，用户可以接着聊，查好了会话再主动播报（n100）
                let mut running = BackgroundQuery {
                    question: question.clone(),
                    handle: tokio::spawn(self.tools_future(reply.tool_calls.clone())),
                };
                match tokio::time::timeout(SLOW_TOOL_FILLER_DELAY, &mut running.handle).await {
                    Ok(outcomes) => outcomes.unwrap_or_else(|_| {
                        reply
                            .tool_calls
                            .iter()
                            .cloned()
                            .map(|call| {
                                let failed = format!("工具 {} 执行失败", call.function.name);
                                (call, ToolOutcome::Reply(failed))
                            })
                            .collect()
                    }),
                    Err(_) => {
                        self.send(TurnEvent::Background(running));
                        self.send(TurnEvent::ToolsFinished(
                            reply
                                .tool_calls
                                .iter()
                                .map(|call| (call.id.clone(), BACKGROUND_TOOL_RESULT.to_owned()))
                                .collect(),
                        ));
                        self.speak_fixed(BACKGROUND_QUERY_FILLER).await;
                        for call in &reply.tool_calls {
                            messages
                                .push(ChatMessage::tool(call.id.clone(), BACKGROUND_TOOL_RESULT));
                        }
                        messages.push(ChatMessage::assistant(BACKGROUND_QUERY_FILLER));
                        break;
                    }
                }
            } else if !spoke_any
                && !filler_spoken
                && rounds == 1
                && let Some(filler) = slow_tool_filler(&reply.tool_calls)
            {
                let running = self.run_tools(&reply.tool_calls);
                tokio::pin!(running);
                let delay = slow_tool_filler_delay(filler);
                let early = if delay.is_zero() {
                    None
                } else {
                    tokio::time::timeout(delay, &mut running).await.ok()
                };
                match early {
                    Some(outcomes) => outcomes,
                    None => {
                        // 一定要几秒的工具：闸门已放行，边应一声边跑工具（n113）
                        filler_spoken = true;
                        let (_, outcomes) = tokio::join!(self.speak_fixed(filler), running);
                        outcomes
                    }
                }
            } else if let Some(speech) = pending.take() {
                // 开场白边合成边播，工具同时跑（n115）
                let (_, outcomes) = tokio::join!(speech, self.run_tools(&reply.tool_calls));
                outcomes
            } else {
                self.run_tools(&reply.tool_calls).await
            };
            debug_assert!(pending.is_none(), "开场白必须在执行工具时等完");
            let mut finished = Vec::with_capacity(tool_outcomes.len());
            for (call, outcome) in tool_outcomes {
                let content = match outcome {
                    ToolOutcome::Reply(content) => content,
                    ToolOutcome::Speak(content) => {
                        direct.push(content.clone());
                        content
                    }
                    ToolOutcome::PlayMusic { track_id, title } => {
                        music = Some((track_id, title));
                        "歌曲已由设备会话接管，等待开始播放".into()
                    }
                };
                finished.push((call.id.clone(), content.clone()));
                messages.push(ChatMessage::tool(call.id, content));
            }
            self.send(TurnEvent::ToolsFinished(finished));
            if let Some((track_id, title)) = music {
                self.send(TurnEvent::PlayMusic { track_id, title });
                // Music is an independent playback source, not a spoken LLM answer.
                // Do not copy tool parameters/paths into persistent dialog history.
                self.send(TurnEvent::Done {
                    messages: Vec::new(),
                    close: false,
                });
                return;
            }
            if !direct.is_empty() {
                let spoken = direct.join("。");
                for sentence in direct {
                    self.speak_fixed(&sentence).await;
                }
                messages.push(ChatMessage::assistant(spoken));
                break;
            }
        }
        self.send(TurnEvent::Done {
            messages: messages.split_off(first_new),
            close: false,
        });
    }
}

/// 历史里标记“被打断、没说出口”的前缀（会话打断时写入，见 `session::heard`，n100）。
pub(crate) const INTERRUPTED_MARK: &str = "【被打断";

/// 历史里出现被打断标记时附在系统提示词后的说明：让大模型知道哪些话用户没听到，话题回来时能接上。
const INTERRUPTED_GUIDE: &str = "被打断约定：历史里助手话后面的【被打断，没说出口的：……】是你上次被用户打断时还没说出来的内容，用户没有听到。先回应用户新说的话；等新话题说完，或用户问“刚才说到哪了”“你接着说”，再自然地把没说完的接上（换个说法，不重复已经说过的）。回答里不要出现【】标记。";

const END_CONVERSATION_TOOL: &str = "end_conversation";
/// 保留名：这句话不是对小智说的（旁人说话、打电话、自言自语、电视声），不回应（n100）
const STAY_SILENT_TOOL: &str = "stay_silent";

fn stay_silent_tool() -> ToolSpec {
    ToolSpec {
        name: STAY_SILENT_TOOL.into(),
        description: "这句话明显不是对你说的时调用，调用后你不出声、不回应：例如用户在跟身边的人说话、在打电话、在自言自语，或者是旁边电视、别人的声音。只要有可能是在问你、叫你做事、回答你的问题，就不要调用，正常回答。调用时不要同时输出任何文字。"
            .into(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }
}

fn end_conversation_tool() -> ToolSpec {
    ToolSpec {
        name: END_CONVERSATION_TOOL.into(),
        description: "结束本次语音对话并让设备回到待命。仅当用户明确表示聊完了、要你退下、去休息、不用了、告别时调用；用户只是说“停”“等一下”或还在提问时不要调用。"
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "farewell": {"type": "string", "description": "一句简短的告别语，不超过 15 个字"}
            }
        }),
    }
}

/// 没有在途任务时，用最近一条记录回答“上一个任务怎么样了”。
/// 只说记录里已有的行数，不编造物理加工进度或剩余时间。
/// 本地只读快答里工具失败时的口语播报（n116）：只说闭集短句，不念工具原文——原文可能很长，
/// 还可能带设备地址、串口或服务方英文报错（如“设备连接失败 (<device-ip>): …”）；原文照旧记在工具调试事件里。
pub(crate) fn read_failure_speech(raw: &str, prefix: &str) -> String {
    let has = |needles: &[&str]| needles.iter().any(|n| raw.contains(n));
    if has(&["超时"]) {
        "查询超时，请稍后再问".into()
    } else if has(&["授权"]) {
        "设备授权失效，请重新连接".into()
    } else if has(&["正在关闭"]) {
        "MOSS 正在关闭".into()
    } else if has(&["未就绪"]) {
        "激光机还没就绪".into()
    } else if has(&["未连接", "连接失败", "无法连接", "断开", "探测目标"]) {
        "激光机没连上，请检查连接".into()
    } else {
        format!("{prefix}，详情看桌面端")
    }
}

fn terminal_reply(record: &Value) -> String {
    let send = &record["send_result"];
    let sent = send["lines_sent"].as_u64();
    let total = send["total_lines"].as_u64();
    match record["status"].as_str().unwrap_or("") {
        "completed" => match total {
            Some(total) if total > 0 => format!("上一个任务已经完成，共 {total} 行。"),
            _ => "上一个任务已经完成。".into(),
        },
        "cancelled" => match sent {
            Some(sent) if sent > 0 => format!("上一个任务已经取消，停在第 {sent} 行。"),
            _ => "上一个任务已经取消。".into(),
        },
        "failed" => match sent {
            Some(sent) if sent > 0 => {
                format!("上一个任务失败了，停在第 {sent} 行，请检查设备。")
            }
            _ => "上一个任务失败了，请检查设备。".into(),
        },
        "preview_ready" | "preview" | "planned" | "confirmed" => {
            "现在没有正在加工的任务，最近一条任务还没开始发送。".into()
        }
        _ => "现在没有正在加工的任务。".into(),
    }
}

/// 启动一轮对话。返回的任务句柄由会话持有，打断时中止。
pub fn spawn_turn(
    conversation: Arc<Conversation>,
    context: ToolContext,
    input: TurnInput,
    history: Vec<ChatMessage>,
    device: Option<DeviceTools>,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::UnboundedReceiver<TurnEvent>,
) {
    spawn_turn_with_person_hint(conversation, context, input, history, device, None)
}

pub fn spawn_turn_with_person_hint(
    conversation: Arc<Conversation>,
    context: ToolContext,
    input: TurnInput,
    history: Vec<ChatMessage>,
    device: Option<DeviceTools>,
    session_person_hint: Option<crate::person_context::PersonScope>,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::UnboundedReceiver<TurnEvent>,
) {
    spawn_turn_gated(
        conversation,
        context,
        input,
        history,
        device,
        session_person_hint,
        None,
    )
}

/// 抢跑闸门：会话在确认用户这句话说完之前先让对话任务开跑，用它控制哪些事必须等确认。
#[derive(Clone)]
pub struct TurnGate {
    /// 变为 `true` 即放行；发送端被丢弃表示这一轮已作废，任务原地等待被中止。
    /// 放行后会话仍可把它置回 `false`（插话时暂停回答）：任务在下一批工具调用前会停下来等
    pub release: tokio::sync::watch::Receiver<bool>,
    /// 这一轮“开始执行工具”与“被会话作废重来”只能发生其一（见 [`ToolLatch`]）
    pub tool_latch: Arc<ToolLatch>,
    /// 这一轮是在静音判停时抢跑开始的（只有这种轮次才需要语义判停辅助决定还要等多久）
    pub speculative: bool,
    /// 为真时连大模型请求也等放行（对正在播的回答的插话：先只做识别）；
    /// 为假时只有大模型发起的工具调用等放行
    pub hold_llm: bool,
    /// `hold_llm` 放行时会话给的最新历史（被打断的回答此时才进历史）
    pub fresh_history: Arc<Mutex<Option<Vec<ChatMessage>>>>,
    /// 自动 / 实时拾音（不是按键说话）：给大模型“这句不是对我说的”的选项（[`STAY_SILENT_TOOL`]，n100）
    pub allow_silent: bool,
    /// 会话能接收后台查询结果并主动播报（[`TurnEvent::Background`]，n100）
    pub allow_background: bool,
    /// 命令词检测模型在这句话里听到的停止 / 退出命令词（协议 7，n101）：通用识别把它听成了
    /// 别的短话（如“问一下”“跪下”）时，按命令词纠正。判停流是异步的、这一轮还可能抢跑开始，
    /// 命令词结果常比“这段话结束”晚一点到，会话收到后补写进来；任务在识别完成后才读
    pub command_hint: Arc<Mutex<Option<moss_voice_providers::local::CommandKeyword>>>,
    /// 这句话的尾音什么时候送完（之后命令词结果不会再来）；没有尾音窗口时为空（n115）
    pub command_hint_until: Option<std::time::Instant>,
}

/// 一轮对话的工具执行闩：任务开始执行工具与会话把这一轮作废重来（思考中用户接着说）互斥，
/// 由原子比较交换决出先后——否则会话可能在任务已经发起工具调用的瞬间把它中止，再让合并后的新一轮
/// 把同一个工具执行第二遍。状态只会从“未定”走向“已开始执行工具”或“已作废”，不会回退。
#[derive(Debug, Default)]
pub struct ToolLatch(std::sync::atomic::AtomicU8);

const LATCH_OPEN: u8 = 0;
const LATCH_TOOLS_STARTED: u8 = 1;
const LATCH_ABANDONED: u8 = 2;

impl ToolLatch {
    /// 任务调用工具前：成功（或之前已开始过）返回 `true`；这一轮已被作废返回 `false`。
    pub fn enter_tools(&self) -> bool {
        use std::sync::atomic::Ordering;
        match self.0.compare_exchange(
            LATCH_OPEN,
            LATCH_TOOLS_STARTED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(state) => state == LATCH_TOOLS_STARTED,
        }
    }

    /// 会话要把这一轮作废重来：工具还没开始执行才成功。
    pub fn try_abandon(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.0
            .compare_exchange(
                LATCH_OPEN,
                LATCH_ABANDONED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub fn tools_started(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire) == LATCH_TOOLS_STARTED
    }
}

/// 同 [`spawn_turn_with_person_hint`]，另带抢跑闸门。
pub fn spawn_turn_gated(
    conversation: Arc<Conversation>,
    context: ToolContext,
    input: TurnInput,
    history: Vec<ChatMessage>,
    device: Option<DeviceTools>,
    session_person_hint: Option<crate::person_context::PersonScope>,
    gate: Option<TurnGate>,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::UnboundedReceiver<TurnEvent>,
) {
    let simulated = matches!(
        input,
        TurnInput::Text {
            simulated: true,
            ..
        }
    );
    spawn_turn_traced(
        conversation,
        context,
        input,
        history,
        device,
        session_person_hint,
        gate,
        None,
        simulated,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_turn_traced(
    conversation: Arc<Conversation>,
    context: ToolContext,
    input: TurnInput,
    history: Vec<ChatMessage>,
    device: Option<DeviceTools>,
    session_person_hint: Option<crate::person_context::PersonScope>,
    gate: Option<TurnGate>,
    turn_trace: Option<TurnTrace>,
    simulated: bool,
    live_bench_window: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::UnboundedReceiver<TurnEvent>,
) {
    let (events, receiver) = unbounded_channel();
    let simulated = simulated
        || matches!(
            input,
            TurnInput::Text {
                simulated: true,
                ..
            }
        );
    let turn = Turn {
        conversation,
        context,
        events,
        device,
        _session_person_hint: session_person_hint,
        gate,
        turn_trace,
        started: Instant::now(),
        measured: Mutex::default(),
        llm_rounds: Default::default(),
        tts_sentences: Default::default(),
        tool_descriptions: Mutex::default(),
        simulated,
        live_bench_window,
    };
    let handle = tokio::spawn(async move {
        match input {
            TurnInput::FastStop { text, .. } => {
                let mut no_identity = None;
                let mut no_prefix = Vec::new();
                let corrected =
                    apply_asr_corrections(&text, &turn.conversation.settings.asr_corrections);
                turn.send(TurnEvent::RawHeard {
                    raw: text,
                    homophone: corrected.clone(),
                    corrected: corrected.clone(),
                    keyword: None,
                });
                turn.send(TurnEvent::Heard(corrected.clone()));
                turn.fast_answer(&corrected, &mut no_identity, &mut no_prefix)
                    .await;
            }
            TurnInput::Utterance {
                pcm,
                sample_rate,
                stream,
            } => turn.answer(pcm, sample_rate, stream, history).await,
            TurnInput::Text { text, .. } => turn.answer_from_text(text, history, None, None).await,
            TurnInput::Report { question, results } => {
                turn.report(question, results, history).await
            }
            TurnInput::Say { text, close } => {
                turn.speak_fixed(&text).await;
                turn.send(TurnEvent::Done {
                    messages: Vec::new(),
                    close,
                });
            }
        }
    });
    (handle, receiver)
}

/// 把一轮的新消息追加到历史并截断。
pub fn append_history(history: &mut Vec<ChatMessage>, messages: Vec<ChatMessage>, budget: usize) {
    history.extend(messages);
    truncate_history(history, budget);
}

#[cfg(test)]
mod tool_latch_tests {
    use super::ToolLatch;

    /// “开始执行工具”与“作废重来”只能有一个成功，且不会回退。
    #[test]
    fn tool_start_and_abandon_are_mutually_exclusive() {
        let latch = ToolLatch::default();
        assert!(latch.enter_tools());
        assert!(latch.tools_started());
        assert!(!latch.try_abandon(), "已开始执行工具就不能再作废重来");
        assert!(latch.enter_tools(), "同一轮后续的工具轮次照常执行");

        let latch = ToolLatch::default();
        assert!(latch.try_abandon());
        assert!(!latch.enter_tools(), "作废之后一个工具都不能执行");
        assert!(!latch.tools_started());

        // 两边同时抢：恰好一方成功
        for _ in 0..200 {
            let latch = std::sync::Arc::new(ToolLatch::default());
            let other = latch.clone();
            let task = std::thread::spawn(move || other.enter_tools());
            let abandoned = latch.try_abandon();
            let entered = task.join().unwrap();
            assert!(
                abandoned != entered,
                "abandoned={abandoned} entered={entered}"
            );
        }
    }
}
