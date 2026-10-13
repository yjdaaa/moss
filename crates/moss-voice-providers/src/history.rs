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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    /// 助手消息只有工具调用时为 `null`
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: Role::System, content: Some(text.into()), tool_calls: Vec::new(), tool_call_id: None }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, content: Some(text.into()), tool_calls: Vec::new(), tool_call_id: None }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: Some(text.into()), tool_calls: Vec::new(), tool_call_id: None }
    }

    pub fn assistant_tools(text: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self { role: Role::Assistant, content: text.filter(|text| !text.is_empty()), tool_calls, tool_call_id: None }
    }

    pub fn tool(call_id: impl Into<String>, result: impl Into<String>) -> Self {
        Self { role: Role::Tool, content: Some(result.into()), tool_calls: Vec::new(), tool_call_id: Some(call_id.into()) }
    }

    /// 估算长度：中文约 1 字 1 token，取字符数作为保守估计。
    pub fn weight(&self) -> usize {
        self.content.as_deref().map(|text| text.chars().count()).unwrap_or(0)
            + self
                .tool_calls
                .iter()
                .map(|call| call.function.name.chars().count() + call.function.arguments.chars().count())
                .sum::<usize>()
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
}
