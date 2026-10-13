//! NV8a：整句固定意图，任何模糊或带额外内容的话都留给正常对话。
use serde_json::Value;

pub fn normalized(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).collect()
}

pub fn is_stop(text: &str) -> bool {
    let text = normalized(text);
    let words = ["停", "停止", "停下", "停机", "急停", "快停", "别打了", "关掉激光", "暂停"];
    words.iter().any(|word| text == *word || ["吧", "啊", "了", "一下", "快"].iter().any(|end| text == format!("{word}{end}")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadIntent { Status, Connection, Progress, Remaining, Material }

pub fn read_intent(text: &str) -> Option<ReadIntent> {
    Some(match normalized(text).as_str() {
        "查询状态" | "设备状态" | "现在什么状态" | "机器状态" => ReadIntent::Status,
        "连接状态" | "设备连接了吗" | "连接了吗" | "机器连上了吗" => ReadIntent::Connection,
        "加工进度" | "当前进度" | "进度怎么样" | "加工到哪了" => ReadIntent::Progress,
        "还要多久" | "剩余时间" | "还剩多久" => ReadIntent::Remaining,
        "当前材料" | "现在用什么材料" | "材料参数" | "当前材料参数" => ReadIntent::Material,
        _ => return None,
    })
}

pub fn body(result: &crate::tools::ToolHostResult) -> Value {
    result.detail.as_deref().and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.get("result").cloned()).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stops_are_whole_phrases_and_negations_are_not_stops() {
        for text in ["停！", "急停", "停止吧", "关掉激光", "暂停一下"] { assert!(is_stop(text), "{text}"); }
        for text in ["不要停止", "别停", "停止之后怎么办", "如何停止", "我想知道机器停止的原因", "", "停止机器"] { assert!(!is_stop(text), "{text}"); }
        assert_eq!(read_intent("当前进度？"), Some(ReadIntent::Progress));
        assert_eq!(read_intent("当前进度怎么样，顺便开始加工"), None);
    }
}
