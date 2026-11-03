//! 对话消息（OpenAI `chat/completions` 格式）与历史截断。

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON 文本（模型原样给出，可能不合法，由执行方解析）
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_type")]
    pub kind: String,
    pub function: FunctionCall,
}

fn function_type() -> String {
    "function".into()
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: function_type(),
            function: FunctionCall { name: name.into(), arguments: arguments.into() },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    /// 助手消息只有工具调用时为 `null`
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// 2026-10-16：多模态图片（`data:<mime>;base64,...`）。
    /// 只有用户在视觉卡片里明确打开“把照片留在对话上下文里”才会有值；
    /// 只在会话内存里，随历史截断丢弃，不落盘、不进日志。序列化成 OpenAI 的
    /// `content` parts 数组（本类型只用于请求，不回读多模态消息）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

impl Serialize for ChatMessage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("role", &self.role)?;
        if self.images.is_empty() {
            map.serialize_entry("content", &self.content)?;
        } else {
            let mut parts: Vec<Value> = Vec::with_capacity(self.images.len() + 1);
            if let Some(text) = self.content.as_deref().filter(|text| !text.trim().is_empty()) {
                parts.push(serde_json::json!({"type": "text", "text": text}));
            }
            for url in &self.images {
                parts.push(serde_json::json!({"type": "image_url", "image_url": {"url": url}}));
            }
            map.serialize_entry("content", &parts)?;
        }
        if !self.tool_calls.is_empty() {
            map.serialize_entry("tool_calls", &self.tool_calls)?;
        }
        if let Some(id) = &self.tool_call_id {
            map.serialize_entry("tool_call_id", id)?;
        }
        map.end()
    }
}

impl ChatMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: Role::System, content: Some(text.into()), tool_calls: Vec::new(), tool_call_id: None, images: Vec::new() }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, content: Some(text.into()), tool_calls: Vec::new(), tool_call_id: None, images: Vec::new() }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: Some(text.into()), tool_calls: Vec::new(), tool_call_id: None, images: Vec::new() }
    }

    pub fn assistant_tools(text: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self { role: Role::Assistant, content: text.filter(|text| !text.is_empty()), tool_calls, tool_call_id: None, images: Vec::new() }
    }

    pub fn tool(call_id: impl Into<String>, result: impl Into<String>) -> Self {
        Self { role: Role::Tool, content: Some(result.into()), tool_calls: Vec::new(), tool_call_id: Some(call_id.into()), images: Vec::new() }
    }

    /// 带一张图片的用户消息（data URL）。只在“把照片留在对话上下文里”打开时由服务端构造。
    pub fn photo(text: impl Into<String>, data_url: impl Into<String>) -> Self {
        Self { role: Role::User, content: Some(text.into()), tool_calls: Vec::new(), tool_call_id: None, images: vec![data_url.into()] }
    }

    /// 估算长度：中文约 1 字 1 token，取字符数作为保守估计；一张照片按 1500 token 计。
    pub fn weight(&self) -> usize {
        self.content.as_deref().map(|text| text.chars().count()).unwrap_or(0)
            + self
                .tool_calls
                .iter()
                .map(|call| call.function.name.chars().count() + call.function.arguments.chars().count())
                .sum::<usize>()
            + self.images.len() * 1500
            + 4
    }
}

/// 工具定义（发给模型的 `tools`）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolSpec {
    pub fn to_openai(&self) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {"name": self.name, "description": self.description, "parameters": self.parameters}
        })
    }
}

/// 按长度预算截断历史（不含系统提示）：从最早的消息开始整轮丢弃，
/// 保证剩下的历史以用户消息开头，工具调用与其结果不会被拆开；最后一轮总是保留。
pub fn truncate_history(history: &mut Vec<ChatMessage>, budget: usize) {
    let total = |messages: &[ChatMessage]| messages.iter().map(ChatMessage::weight).sum::<usize>();
    while history.len() > 1 && total(history) > budget {
        // 下一轮的起点：第一条之后的下一条用户消息
        let Some(next_user) = history.iter().skip(1).position(|message| message.role == Role::User) else { break };
        history.drain(..next_user + 1);
    }
    // 开头若不是用户消息（例如残留的工具结果），丢到第一条用户消息为止
    if let Some(first_user) = history.iter().position(|message| message.role == Role::User) {
        history.drain(..first_user);
    }
}

/// 折叠早于最近 `keep_recent_user_turns` 轮用户发言之前的冗长工具详情，保留对话原文与配对 ID。
/// 0ms 本地执行，避免多轮查状态/搜网页后大段过期 JSON 拖慢后续轮次首字延迟。
pub fn compact_stale_tool_results(
    history: &[ChatMessage],
    keep_recent_user_turns: usize,
) -> Vec<ChatMessage> {
    let user_positions: Vec<usize> = history
        .iter()
        .enumerate()
        .filter_map(|(idx, msg)| (msg.role == Role::User).then_some(idx))
        .collect();
    let cutoff = if keep_recent_user_turns == 0 {
        history.len()
    } else if user_positions.len() > keep_recent_user_turns {
        user_positions[user_positions.len() - keep_recent_user_turns]
    } else {
        0
    };
    history
        .iter()
        .enumerate()
        .map(|(idx, msg)| {
            if idx >= cutoff || msg.role != Role::Tool {
                return msg.clone();
            }
            let Some(content) = msg.content.as_deref() else {
                return msg.clone();
            };
            if content.chars().count() <= 200 {
                return msg.clone();
            }
            let compacted = if let Ok(mut value) = serde_json::from_str::<Value>(content)
                && let Some(obj) = value.as_object_mut()
                && obj.remove("detail").is_some()
            {
                value.to_string()
            } else {
                let mut head: String = content.chars().take(160).collect();
                head.push_str("…（旧轮次详情已折叠）");
                head
            };
            let mut next = msg.clone();
            next.content = Some(compacted);
            next
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(index: usize, with_tool: bool) -> Vec<ChatMessage> {
        let mut messages = vec![ChatMessage::user(format!("问题{index}{}", "字".repeat(20)))];
        if with_tool {
            messages.push(ChatMessage::assistant_tools(None, vec![ToolCall::new(format!("c{index}"), "get_time", "{}")]));
            messages.push(ChatMessage::tool(format!("c{index}"), "12:00"));
        }
        messages.push(ChatMessage::assistant(format!("回答{index}{}", "字".repeat(20))));
        messages
    }

    #[test]
    fn serializes_like_openai() {
        let call = ChatMessage::assistant_tools(Some(String::new()), vec![ToolCall::new("c1", "f", r#"{"a":1}"#)]);
        let value = serde_json::to_value(&call).unwrap();
        assert_eq!(value["role"], "assistant");
        assert!(value["content"].is_null());
        assert_eq!(value["tool_calls"][0]["type"], "function");
        assert_eq!(value["tool_calls"][0]["function"]["arguments"], r#"{"a":1}"#);
        let tool = serde_json::to_value(ChatMessage::tool("c1", "ok")).unwrap();
        assert_eq!(tool["tool_call_id"], "c1");
        let user = serde_json::to_value(ChatMessage::user("hi")).unwrap();
        assert!(user.get("tool_calls").is_none() && user.get("tool_call_id").is_none());
        let spec = ToolSpec { name: "f".into(), description: "d".into(), parameters: serde_json::json!({"type":"object"}) };
        assert_eq!(spec.to_openai()["function"]["name"], "f");
    }

    #[test]
    fn drops_whole_old_rounds_and_keeps_tool_pairs() {
        let mut history: Vec<ChatMessage> = (0..6).flat_map(|index| round(index, index % 2 == 0)).collect();
        let full: usize = history.iter().map(ChatMessage::weight).sum();
        truncate_history(&mut history, full / 2);
        assert!(history.iter().map(ChatMessage::weight).sum::<usize>() <= full / 2);
        assert_eq!(history[0].role, Role::User, "截断后以用户消息开头");
        // 每个工具结果前面都有对应的工具调用
        for (index, message) in history.iter().enumerate() {
            if message.role == Role::Tool {
                let id = message.tool_call_id.as_deref().unwrap();
                assert!(history[..index].iter().any(|m| m.tool_calls.iter().any(|call| call.id == id)));
            }
        }
        assert_eq!(history.last().unwrap().content.as_deref().unwrap(), format!("回答5{}", "字".repeat(20)));
    }

    #[test]
    fn last_round_is_always_kept() {
        let mut history = round(0, true);
        truncate_history(&mut history, 1);
        assert_eq!(history.len(), 4);
        let mut empty: Vec<ChatMessage> = Vec::new();
        truncate_history(&mut empty, 10);
        assert!(empty.is_empty());
    }

    #[test]
    fn photo_message_uses_content_parts_and_weights_more() {
        let photo = ChatMessage::photo("（用户刚用摄像头拍了照片）", "data:image/jpeg;base64,QUJD");
        let value = serde_json::to_value(&photo).unwrap();
        assert_eq!(value["role"], "user");
        assert_eq!(value["content"][0]["type"], "text");
        assert_eq!(value["content"][1]["type"], "image_url");
        assert_eq!(value["content"][1]["image_url"]["url"], "data:image/jpeg;base64,QUJD");
        // 普通文字消息仍是字符串 content，不带 parts
        let text = serde_json::to_value(ChatMessage::user("你好")).unwrap();
        assert_eq!(text["content"], "你好");
        // 图片按 1500 token 估算，截断时优先丢旧照片
        assert!(photo.weight() >= 1500);
        assert!(ChatMessage::user("你好").weight() < 100);
        let mut history = vec![photo, ChatMessage::assistant("这是刚才那张照片")];
        truncate_history(&mut history, 1);
        assert_eq!(history.len(), 2, "最后一轮总是保留");
    }

    #[test]
    fn compacts_only_stale_tool_details_without_breaking_pairs_or_user_text() {
        let big_json = serde_json::json!({
            "success": true,
            "result": "查询成功",
            "detail": "详".repeat(800),
        })
        .to_string();
        let mut history = Vec::new();
        for idx in 0..3 {
            history.push(ChatMessage::user(format!("第{idx}句用户原话")));
            history.push(ChatMessage::assistant_tools(
                None,
                vec![ToolCall::new(format!("call-{idx}"), "laser_workflow_tool", "{}")],
            ));
            history.push(ChatMessage::tool(format!("call-{idx}"), big_json.clone()));
            history.push(ChatMessage::assistant(format!("第{idx}句助手回答")));
        }
        let compacted = compact_stale_tool_results(&history, 2);
        assert_eq!(compacted.len(), history.len());
        // 第 0 轮早于最近 2 轮：detail 被剥掉，但 call_id 与 result 摘要完整保留
        let old_tool: Value = serde_json::from_str(compacted[2].content.as_deref().unwrap()).unwrap();
        assert_eq!(old_tool["result"], "查询成功");
        assert!(old_tool.get("detail").is_none());
        assert_eq!(compacted[2].tool_call_id.as_deref(), Some("call-0"));
        // 最近 2 轮（idx 1、2）完整保留 detail，所有用户/助手原文不变
        let recent_tool: Value = serde_json::from_str(compacted[6].content.as_deref().unwrap()).unwrap();
        assert!(recent_tool["detail"].as_str().unwrap().chars().count() == 800);
        assert_eq!(compacted[0].content.as_deref(), Some("第0句用户原话"));
        assert_eq!(compacted[3].content.as_deref(), Some("第0句助手回答"));
    }
}
