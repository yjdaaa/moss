//! NV8a：整句固定意图，任何模糊或带额外内容的话都留给正常对话。
use serde_json::Value;

pub fn normalized(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// 只收会被理解为“立即停止”的说法；“暂停”的本意是可恢复的暂停，不能映射为不可恢复的急停（0x18 复位），
/// 交给正常对话处理。
pub fn is_stop(text: &str) -> bool {
    let text = normalized(text);
    if text.is_empty() {
        return false;
    }
    let words = [
        "停",
        "停止",
        "停下",
        "停机",
        "急停",
        "快停",
        "别打了",
        "关掉激光",
    ];
    let matches_word = |candidate: &str| {
        words.iter().any(|word| {
            candidate.strip_prefix(word).is_some_and(|rest| {
                rest.is_empty() || rest == *word || rest.strip_prefix(word) == Some(*word)
            })
        })
    };
    matches_word(&text)
        || ["吧", "啊", "了", "一下", "快"]
            .iter()
            .any(|end| text.strip_suffix(end).is_some_and(matches_word))
}

/// 纯思考气音或无语义口头碎片（0ms 本地静默跳过，不打扰用户继续组织语言）。
/// 注意：绝不拦截“嗯/好/好的/对/确认/开始/停”等可能承担确认、回答或控制语义的短词。
pub fn is_noise_filler(text: &str) -> bool {
    matches!(
        normalized(text).as_str(),
        "呃" | "呃呃" | "额" | "额额" | "唔" | "唔唔"
    )
}

/// 附和（backchannel）：对方说话时随口应的一声，不是要抢话。只收不承担确认 / 控制语义的说法——
/// “确认”“开始”“停”“好的开始”之类一律不算，必须照常处理。
pub fn is_backchannel(text: &str) -> bool {
    matches!(
        normalized(text).as_str(),
        "嗯" | "嗯嗯"
            | "嗯嗯嗯"
            | "哦"
            | "哦哦"
            | "噢"
            | "啊"
            | "哎"
            | "呃"
            | "额"
            | "对"
            | "对对"
            | "对对对"
            | "对的"
            | "是"
            | "是的"
            | "是啊"
            | "好"
            | "好的"
            | "好好"
            | "行"
            | "嗯好"
            | "嗯对"
            | "没错"
            | "明白"
            | "明白了"
            | "知道了"
            | "这样啊"
            | "原来如此"
            | "哈哈"
            | "哈哈哈"
            | "呵呵"
    )
}

/// 这句话听起来说完了没有（决定判停后还要等多久才开口，见 [`endpoint_wait_ms`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completeness {
    /// 明显说完了：停止 / 退出类整句、疑问句、以句末语气词收尾
    Complete,
    /// 看不出来
    Unknown,
    /// 明显没说完：停在连词、介词、“把”“调到”这类后面必须接内容的词上
    Unfinished,
}

/// 按识别出的文字判断一句话是否说完。只看结尾的词，不做语义理解；拿不准一律 `Unknown`。
/// 识别结果自带的句号不可靠（整段识别会给半句话也加句号），所以先去掉标点再看结尾。
pub fn completeness(text: &str) -> Completeness {
    // 后面必须接内容的结尾（长的在前）
    const OPEN_ENDS: &[&str] = &[
        "也就是说",
        "换句话说",
        "因为",
        "所以",
        "但是",
        "可是",
        "不过",
        "然后",
        "接着",
        "就是",
        "比如",
        "例如",
        "如果",
        "要是",
        "假如",
        "虽然",
        "而且",
        "并且",
        "以及",
        "还有",
        "或者",
        "还是",
        "那个",
        "帮我把",
        "调到",
        "调成",
        "设为",
        "设成",
        "改成",
        "换成",
        "变成",
        "等于",
        "大概",
        "大约",
        "差不多",
        // 单字只收思考气音：“和 / 在 / 给 / 比 / 向 / 额”等单字结尾分不清是介词还是词的一部分
        //（“暖和”“我在”“交给你”“对比”“方向”“余额”），按没说完处理会让完整的话多等约 0.9 秒
        "嗯",
        "呃",
    ];
    // 以“的”收尾的应答（不是“红色的……”那种没说完的定语）
    const CLOSED_DE: &[&str] = &[
        "是的",
        "好的",
        "对的",
        "行的",
        "可以的",
        "真的",
        "别的",
        "一样的",
    ];
    const CLOSED_ENDS: &[&str] = &[
        "吗",
        "呢",
        "吧",
        "了",
        "啊",
        "呀",
        "嘛",
        "啦",
        "哦",
        "呗",
        "么",
        "什么意思",
        "啥意思",
        "怎么办",
        "怎么弄",
        "多少钱",
        "在哪里",
        "在哪儿",
    ];
    let core = normalized(text);
    if core.is_empty() {
        return Completeness::Unknown;
    }
    if is_stop(text) {
        return Completeness::Complete;
    }
    let chars = core.chars().count();
    if let Some(open) = OPEN_ENDS.iter().find(|end| core.ends_with(**end)) {
        // 整句只有一个单字时多半是在回答（“给”“在”），不算没说完；思考气音除外
        let open_chars = open.chars().count();
        if chars > open_chars || open_chars >= 2 || matches!(*open, "嗯" | "呃") {
            return Completeness::Unfinished;
        }
    }
    if core.ends_with('的') && chars >= 5 && !CLOSED_DE.iter().any(|end| core.ends_with(end)) {
        return Completeness::Unfinished;
    }
    let question = text.trim_end().ends_with(['？', '?']);
    if question || CLOSED_ENDS.iter().any(|end| core.ends_with(end)) {
        return Completeness::Complete;
    }
    Completeness::Unknown
}

/// 从用户停止出声算起，等多久才认定这句话说完（毫秒）。
/// 说完了就马上回应；拿不准按原先的 400 ms；明显没说完多等一会儿，不抢话。
pub fn endpoint_wait_ms(text: &str) -> u64 {
    match completeness(text) {
        Completeness::Complete => 250,
        Completeness::Unknown => 400,
        Completeness::Unfinished => 1300,
    }
}

/// 结合 Smart Turn v3 尾音语义判停概率调整停顿等待时长；停止指令永远 0 ms 放行。
pub fn endpoint_wait_ms_with_prosody(text: &str, smart_turn_prob: Option<f32>) -> u64 {
    if is_stop(text) {
        return 0;
    }
    let base = endpoint_wait_ms(text);
    let Some(prob) = smart_turn_prob.filter(|p| p.is_finite()) else {
        return base;
    };
    if prob < 0.5 {
        base.max(1250)
    } else if prob >= 0.80 && completeness(text) == Completeness::Unknown {
        250
    } else {
        base
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadIntent {
    Status,
    Connection,
    Progress,
    Remaining,
    Material,
}

fn strip_polite_wrappers(mut s: &str) -> &str {
    const PREFIXES: &[&str] = &[
        "请问一下",
        "请问",
        "麻烦帮我",
        "麻烦",
        "你帮我",
        "帮我",
        "给我",
        "我想知道",
        "我想看下",
        "我想看一下",
        "看一下",
        "看下",
        "看看",
        "查一下",
        "查下",
        "查查",
        "问一下",
        "问下",
        "播报一下",
        "说一下",
    ];
    const SUFFIXES: &[&str] = &["一下", "呀", "啊", "吧", "呢", "啦", "哦", "呗"];
    for _ in 0..2 {
        for prefix in PREFIXES {
            if let Some(rest) = s.strip_prefix(prefix)
                && !rest.is_empty()
            {
                s = rest;
                break;
            }
        }
    }
    for _ in 0..2 {
        for suffix in SUFFIXES {
            if let Some(rest) = s.strip_suffix(suffix)
                && !rest.is_empty()
            {
                s = rest;
                break;
            }
        }
    }
    s
}

fn match_core_read(core: &str) -> Option<ReadIntent> {
    Some(match core {
        "查询状态"
        | "设备状态"
        | "现在什么状态"
        | "什么状态"
        | "机器状态"
        | "当前状态"
        | "机器现在什么状态"
        | "设备现在什么状态"
        | "激光机现在什么状态"
        | "雕刻机现在什么状态"
        | "激光机当前状态"
        | "雕刻机当前状态"
        | "雕刻机状态"
        | "激光机状态" => ReadIntent::Status,
        "连接状态"
        | "设备连接了吗"
        | "连接了吗"
        | "机器连上了吗"
        | "设备连上了吗"
        | "激光机连上了吗"
        | "雕刻机连上了吗"
        | "连上了吗"
        | "连上设备了吗"
        | "连上机器了吗"
        | "机器在线吗"
        | "设备在线吗"
        | "激光机在线吗"
        | "雕刻机在线吗" => ReadIntent::Connection,
        "加工进度"
        | "当前进度"
        | "进度怎么样"
        | "进度如何"
        | "进度多少"
        | "当前加工进度"
        | "加工到哪了"
        | "雕到哪了"
        | "打到哪了"
        | "刻到哪了"
        | "雕到百分之几了"
        | "打到百分之几了"
        | "现在进度怎么样" => ReadIntent::Progress,
        "还要多久" | "剩余时间" | "还剩多久" | "现在还要多久" | "大概还要多久" | "还要雕多久"
        | "还要打多久" | "还要刻多久" | "还有多久雕完" | "还有多久打完" => {
            ReadIntent::Remaining
        }
        "当前材料"
        | "现在用什么材料"
        | "用什么材料"
        | "用的什么材料"
        | "材料参数"
        | "当前材料参数"
        | "现在是什么材料" => ReadIntent::Material,
        _ => return None,
    })
}

/// 判断文本在剥离句末结束标点后是否仍为单句（不含逗号、分号、句中问号/感叹号或换行等复合分句符）。
pub fn is_single_clause(text: &str) -> bool {
    let trimmed = text
        .trim()
        .trim_end_matches(['。', '？', '?', '！', '!', ' ', '\t']);
    !trimmed.chars().any(|c| {
        matches!(
            c,
            '，' | ',' | '；' | ';' | '。' | '？' | '?' | '！' | '!' | '\n' | '\r'
        )
    })
}

pub fn read_intent(text: &str) -> Option<ReadIntent> {
    // 含分句标点说明可能携带复合意图（如“当前进度怎么样，顺便开始加工”或“当前进度？顺便开灯”），留给完整对话路径
    if !is_single_clause(text) {
        return None;
    }
    let clean = normalized(text);
    match_core_read(&clean).or_else(|| match_core_read(strip_polite_wrappers(&clean)))
}

pub fn body(result: &crate::tools::ToolHostResult) -> Value {
    result
        .detail
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.get("result").cloned())
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backchannels_never_include_confirm_or_control_words() {
        for text in ["嗯。", "对对对", "好的", "哈哈哈", "知道了"] {
            assert!(is_backchannel(text), "{text}");
        }
        for text in [
            "确认",
            "开始",
            "停",
            "好的开始",
            "可以",
            "不对",
            "换一个",
            "等一下",
        ] {
            assert!(!is_backchannel(text), "{text}");
        }
    }

    #[test]
    fn completeness_only_trusts_clear_endings() {
        use Completeness::*;
        for (text, expected) in [
            ("停", Complete),
            ("现在几点了？", Complete),
            ("帮我看看机器连上了吗", Complete),
            ("好了", Complete),
            ("什么意思", Complete),
            ("这是什么意思", Complete),
            ("接下来怎么办", Complete),
            ("这个怎么弄", Complete),
            ("开始", Unknown),
            ("确认", Unknown),
            ("是的", Unknown),
            ("好的。", Unknown),
            ("给", Unknown),
            ("椴木三毫米。", Unknown),
            ("帮我把功率调到", Unfinished),
            ("我想刻一个圆形然后", Unfinished),
            ("把那个红色的。", Unfinished),
            ("功率八十，速度大概", Unfinished),
            ("嗯", Unfinished),
            // 以“在 / 和 / 额 / 比 / 向 / 给”结尾的完整说法不能被当成没说完
            ("我在", Unknown),
            ("今天挺暖和", Unknown),
            ("查一下余额", Unknown),
            ("帮我对比", Unknown),
            ("调一下方向", Unknown),
            ("交给你", Unknown),
            ("", Unknown),
        ] {
            assert_eq!(completeness(text), expected, "{text}");
        }
        assert!(endpoint_wait_ms("好了") < endpoint_wait_ms("开始"));
        assert!(endpoint_wait_ms("开始") < endpoint_wait_ms("帮我把"));
    }
    #[test]
    fn stops_are_whole_phrases_and_negations_are_not_stops() {
        for text in [
            "停！",
            "停停",
            "停停停",
            "停停吧",
            "停下停下",
            "急停",
            "急停急停",
            "快停快停",
            "停止吧",
            "停止停止",
            "关掉激光",
        ] {
            assert!(is_stop(text), "{text}");
        }
        for text in [
            "不要停止",
            "别停",
            "停止之后怎么办",
            "如何停止",
            "我想知道机器停止的原因",
            "",
            "停止机器",
            "暂停",
            "暂停一下",
        ] {
            assert!(!is_stop(text), "{text}");
        }
        assert_eq!(read_intent("当前进度？"), Some(ReadIntent::Progress));
        assert_eq!(read_intent("当前进度怎么样，顺便开始加工"), None);
    }

    #[test]
    fn polite_colloquial_queries_route_locally_while_confirmations_and_compounds_stay_safe() {
        assert_eq!(
            read_intent("帮我看一下当前进度呀？"),
            Some(ReadIntent::Progress)
        );
        assert_eq!(
            read_intent("请问机器连上了吗"),
            Some(ReadIntent::Connection)
        );
        assert_eq!(
            read_intent("麻烦查下还要雕多久吧"),
            Some(ReadIntent::Remaining)
        );
        assert_eq!(
            read_intent("看看机器现在什么状态"),
            Some(ReadIntent::Status)
        );
        assert_eq!(read_intent("激光机现在什么状态"), Some(ReadIntent::Status));
        assert_eq!(
            read_intent("雕刻机现在什么状态？"),
            Some(ReadIntent::Status)
        );
        assert_eq!(read_intent("激光机连上了吗"), Some(ReadIntent::Connection));
        assert_eq!(read_intent("雕刻机在线吗"), Some(ReadIntent::Connection));
        assert_eq!(
            read_intent("看下现在用什么材料呢"),
            Some(ReadIntent::Material)
        );
        assert_eq!(read_intent("当前进度怎么样，顺便开始加工"), None);
        assert_eq!(read_intent("现在进度怎么样？顺便把灯打开"), None);
        assert_eq!(read_intent("帮我把功率调到400"), None);
        assert!(is_noise_filler("呃…"));
        assert!(is_noise_filler("额额"));
        for word in [
            "嗯", "嗯哼", "好", "好的", "对", "确认", "开始", "停", "你好",
        ] {
            assert!(!is_noise_filler(word), "{word} must not be swallowed");
        }
    }

    #[test]
    fn prosody_wait_extends_unfinished_and_never_delays_stops() {
        for stop in ["急停", "停一下", "快停"] {
            assert_eq!(endpoint_wait_ms_with_prosody(stop, None), 0);
            assert_eq!(endpoint_wait_ms_with_prosody(stop, Some(0.05)), 0);
            assert_eq!(endpoint_wait_ms_with_prosody(stop, Some(0.95)), 0);
        }
        assert_eq!(endpoint_wait_ms_with_prosody("好了", Some(0.20)), 1250);
        assert_eq!(endpoint_wait_ms_with_prosody("二十瓦", Some(0.20)), 1250);
        assert_eq!(endpoint_wait_ms_with_prosody("帮我把", Some(0.20)), 1300);

        assert_eq!(endpoint_wait_ms_with_prosody("二十瓦", Some(0.90)), 250);
        assert_eq!(endpoint_wait_ms_with_prosody("帮我把", Some(0.90)), 1300);

        assert_eq!(
            endpoint_wait_ms_with_prosody("二十瓦", None),
            endpoint_wait_ms("二十瓦")
        );
        assert_eq!(
            endpoint_wait_ms_with_prosody("二十瓦", Some(f32::NAN)),
            endpoint_wait_ms("二十瓦")
        );
    }
}
