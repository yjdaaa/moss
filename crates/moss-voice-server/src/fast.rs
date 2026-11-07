//! NV8a：整句固定意图，任何模糊或带额外内容的话都留给正常对话。
use serde_json::Value;

pub fn normalized(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// 只收会被理解为“立即停止”的说法；“暂停”的本意是可恢复的暂停，不能映射为不可恢复的急停（0x18 复位），
/// 交给正常对话处理。
pub fn is_stop(text: &str) -> bool {
    let text = normalized(text);
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
    words.iter().any(|word| {
        text == *word
            || ["吧", "啊", "了", "一下", "快"]
                .iter()
                .any(|end| text == format!("{word}{end}"))
    })
}

/// 纯思考气音或无语义口头碎片（0ms 本地静默跳过，不打扰用户继续组织语言）。
/// 注意：绝不拦截“嗯/好/好的/对/确认/开始/停”等可能承担确认、回答或控制语义的短词。
pub fn is_noise_filler(text: &str) -> bool {
    matches!(
        normalized(text).as_str(),
        "呃" | "呃呃" | "额" | "额额" | "唔" | "唔唔"
    )
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
        | "雕刻机状态"
        | "激光机状态" => ReadIntent::Status,
        "连接状态" | "设备连接了吗" | "连接了吗" | "机器连上了吗" | "设备连上了吗" | "连上了吗"
        | "连上设备了吗" | "连上机器了吗" | "机器在线吗" | "设备在线吗" => {
            ReadIntent::Connection
        }
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
    fn stops_are_whole_phrases_and_negations_are_not_stops() {
        for text in ["停！", "急停", "停止吧", "关掉激光"] {
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
}
