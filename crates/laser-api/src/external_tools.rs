//! Host-owned third-party MCP integration. No HTTP configuration or arbitrary-command route.
use crate::approval::ApprovalError;
use laser_contracts::{ApprovalRecord, CallerContext, ToolCallResult};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};

pub trait ExternalTools: Send + Sync {
    fn tools(&self) -> Value;
    fn call(&self, context: &CallerContext, name: &str, arguments: &Value) -> ToolCallResult;
    fn current_digest(&self, record: &ApprovalRecord) -> Result<String, ApprovalError>;
    fn execute(&self, record: &ApprovalRecord) -> (u16, Value);
}
impl crate::ApiService {
    /// Trusted desktop bootstrap only; the backend holds a Weak<ApiService> to avoid a cycle.
    pub fn set_external_tools(&self, tools: Arc<dyn ExternalTools>) -> Result<(), String> {
        self.external_tools
            .set(tools)
            .map_err(|_| "第三方 MCP 已初始化".into())
    }
    pub fn external_tools_list(&self) -> Value {
        self.external_tools
            .get()
            .map(|tools| tools.tools())
            .unwrap_or_else(|| json!({"tools":[]}))
    }
    pub fn call_external_tool(
        &self,
        context: &CallerContext,
        name: &str,
        arguments: &Value,
    ) -> ToolCallResult {
        if !matches!(context, CallerContext::Voice { .. }) || !self.voice_caller_allowed(context) {
            return ToolCallResult::err("第三方工具只接受已允许的内置语音会话");
        }
        self.external_tools
            .get()
            .map(|tools| tools.call(context, name, arguments))
            .unwrap_or_else(|| ToolCallResult::err("第三方 MCP 未启用"))
    }
    pub fn external_voice_permit(
        &self,
        context: &CallerContext,
    ) -> Result<Arc<AtomicBool>, ApprovalError> {
        if !self.voice_caller_allowed(context) {
            return Err(ApprovalError::Expired);
        }
        self.voice_permit(context)
            .map_err(|_| ApprovalError::Expired)?
            .ok_or(ApprovalError::Expired)
    }
}

/// Bounded display only. Execution always uses the exact hidden request and its digest.
pub fn parameter_preview(value: &Value) -> Value {
    fn visit(value: &Value, depth: usize, budget: &mut usize) -> Value {
        if *budget == 0 || depth > 5 {
            return json!("[已截断]");
        }
        *budget -= 1;
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .take(24)
                    .map(|(key, value)| {
                        let lower = key.to_ascii_lowercase();
                        let sensitive = [
                            "key",
                            "token",
                            "secret",
                            "password",
                            "authorization",
                            "cookie",
                            "credential",
                        ]
                        .iter()
                        .any(|needle| lower.contains(needle));
                        (
                            key.chars().take(80).collect(),
                            if sensitive {
                                json!("[已隐藏]")
                            } else {
                                visit(value, depth + 1, budget)
                            },
                        )
                    })
                    .collect(),
            ),
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .take(10)
                    .map(|v| visit(v, depth + 1, budget))
                    .collect(),
            ),
            Value::String(text) => json!(
                text.chars()
                    .filter(|c| !c.is_control())
                    .take(160)
                    .collect::<String>()
            ),
            _ => value.clone(),
        }
    }
    visit(value, 0, &mut 64)
}
