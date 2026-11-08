//! MOSS 工具接入对话（内置语音技术书 §5.2，NV4）。
//!
//! 本 crate 不依赖 laser-*：宿主（桌面端）实现 [`ToolHost`]，以 `CallerContext::Voice` 调用
//! `laser_mcp::dispatch_tool_call_with`（`laser_host::voice_tool_call`）。安全判定全部在执行层：
//! R4 与未分级工具拒绝，R3 只生成桌面审批。这里只负责：
//! - 把 MCP 工具清单转成模型的工具定义；
//! - 在阻塞线程里执行同步的工具调用；
//! - **结果为 `approval_required` 时直接朗读固定话术**，不交给模型改写，避免模型说成“已经开始加工”；
//! - 其他结果截断后交给模型组织回答。

use std::sync::Arc;

use moss_voice_providers::{BoxFuture, ToolSpec};
use serde_json::{Value, json};

use crate::conversation::{ToolContext, ToolExecutor, ToolOutcome};

/// 交给模型的工具结果最长字符数（避免把完整的作业详情塞进上下文）
const MAX_RESULT_CHARS: usize = 1500;

/// R3 生成审批后朗读的话（后端未给出 speech 时使用）
pub const APPROVAL_SPEECH: &str = "已发到电脑，请在电脑上的 MOSS 里确认后开始。";
/// 后端 R3 审批默认返回的固定朗读话术
pub const SUBMITTED_APPROVAL_SPEECH: &str = "已提交，请在电脑上的 MOSS 里确认后开始。";

/// 一次工具调用的结果（与 MCP 工具结果相同的三个字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolHostResult {
    pub success: bool,
    pub result: String,
    pub detail: Option<String>,
}

/// 宿主提供的工具调用面。`call` 是同步的，会在阻塞线程中执行。
pub trait ToolHost: Send + Sync {
    /// MCP `tools/list` 格式：`{"tools":[{"name","description","inputSchema"}]}`
    fn list(&self) -> Value;
    fn call(&self, context: &ToolContext, name: &str, arguments: &Value) -> ToolHostResult;
    /// 只读读取最近的加工任务（加工结束播报轮询）。默认不支持，返回 `None` 即不轮询。
    /// 这条通道**不走工具分发**，因此不计入工具调用统计，也不对模型暴露。
    fn recent_workflows(&self, _device_id: &str, _session_id: &str) -> Option<Value> {
        None
    }
    /// n70 只读读取某个加工任务的刀路预览原图（PNG / JPEG 字节），同样不走工具分发。默认不支持。
    fn workflow_preview(
        &self,
        _device_id: &str,
        _session_id: &str,
        _workflow_id: &str,
    ) -> Option<Vec<u8>> {
        None
    }
}

/// 把宿主的工具交给对话。工具清单每轮重新读取：用户在语音中心关闭或打开工具（NV8）后，下一轮对话即生效。
pub struct HostTools {
    host: Arc<dyn ToolHost>,
}

impl HostTools {
    pub fn new(host: Arc<dyn ToolHost>) -> Self {
        Self { host }
    }

    fn specs(&self) -> Vec<ToolSpec> {
        self.host.list()["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| {
                        Some(ToolSpec {
                            name: tool["name"].as_str()?.to_owned(),
                            description: tool["description"]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned(),
                            parameters: tool
                                .get("inputSchema")
                                .cloned()
                                .unwrap_or_else(|| json!({"type": "object"})),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// 结果是否为“已生成桌面审批”；是则返回要朗读的话。
pub fn approval_speech(result: &ToolHostResult) -> Option<String> {
    let body: Value = serde_json::from_str(result.detail.as_deref()?).ok()?;
    let inner = body.get("result")?;
    (inner.get("status")?.as_str()? == "approval_required").then(|| {
        inner
            .get("speech")
            .and_then(Value::as_str)
            .filter(|speech| !speech.is_empty())
            .unwrap_or(APPROVAL_SPEECH)
            .to_owned()
    })
}

fn truncate(text: &str) -> String {
    if text.chars().count() <= MAX_RESULT_CHARS {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(MAX_RESULT_CHARS).collect();
    short.push_str("…（已截断）");
    short
}

/// 模型看到的工具结果：成败、摘要与（截断后的）详情。
pub fn outcome_for_model(result: &ToolHostResult) -> String {
    let mut value = json!({"success": result.success, "result": result.result});
    if let Some(detail) = result.detail.as_deref().filter(|detail| !detail.is_empty()) {
        value["detail"] = Value::String(truncate(detail));
    }
    value.to_string()
}

impl ToolExecutor for HostTools {
    fn call_raw<'a>(
        &'a self,
        context: &'a ToolContext,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, ToolHostResult> {
        Box::pin(async move {
            let Ok(arguments) = serde_json::from_str::<Value>(arguments) else {
                return ToolHostResult {
                    success: false,
                    result: "工具参数无效".into(),
                    detail: None,
                };
            };
            let host = self.host.clone();
            let context = context.clone();
            let name = name.to_owned();
            tokio::task::spawn_blocking(move || host.call(&context, &name, &arguments))
                .await
                .unwrap_or_else(|_| ToolHostResult {
                    success: false,
                    result: "工具执行失败".into(),
                    detail: None,
                })
        })
    }
    fn recent_workflows<'a>(&'a self, context: &'a ToolContext) -> BoxFuture<'a, Option<Value>> {
        Box::pin(async move {
            let host = self.host.clone();
            let device = context.device_id.clone();
            let session = context.session_id.clone();
            tokio::task::spawn_blocking(move || host.recent_workflows(&device, &session))
                .await
                .ok()
                .flatten()
        })
    }
    fn workflow_preview<'a>(
        &'a self,
        context: &'a ToolContext,
        workflow_id: &'a str,
    ) -> BoxFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let host = self.host.clone();
            let device = context.device_id.clone();
            let session = context.session_id.clone();
            let workflow = workflow_id.to_owned();
            tokio::task::spawn_blocking(move || host.workflow_preview(&device, &session, &workflow))
                .await
                .ok()
                .flatten()
        })
    }
    fn tools(&self) -> Vec<ToolSpec> {
        self.specs()
    }

    fn call<'a>(
        &'a self,
        context: &'a ToolContext,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let arguments = if arguments.trim().is_empty() {
                "{}"
            } else {
                arguments
            };
            let Ok(arguments) = serde_json::from_str::<Value>(arguments) else {
                return ToolOutcome::Reply(format!(
                    "调用 {name} 的参数不是合法的 JSON，请重新生成参数"
                ));
            };
            let host = self.host.clone();
            let context = context.clone();
            let name_owned = name.to_owned();
            let joined =
                tokio::task::spawn_blocking(move || host.call(&context, &name_owned, &arguments))
                    .await;
            let result = match joined {
                Ok(result) => result,
                Err(_) => return ToolOutcome::Reply(format!("工具 {name} 执行失败")),
            };
            match approval_speech(&result) {
                Some(speech) => ToolOutcome::Speak(speech),
                None => ToolOutcome::Reply(outcome_for_model(&result)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct FakeHost {
        calls: Mutex<Vec<(String, Value, String)>>,
    }

    impl ToolHost for FakeHost {
        fn list(&self) -> Value {
            json!({"tools": [
                {"name": "laser_workflow_tool", "description": "加工流程", "inputSchema": {"type": "object", "properties": {"action": {"type": "string"}}}},
                {"name": "no_schema_tool"},
                {"description": "缺名字的条目被忽略"}
            ]})
        }

        fn call(&self, context: &ToolContext, name: &str, arguments: &Value) -> ToolHostResult {
            self.calls.lock().unwrap().push((
                name.to_owned(),
                arguments.clone(),
                context.device_id.clone(),
            ));
            match arguments["action"].as_str() {
                Some("confirm_send") => ToolHostResult {
                    success: true,
                    result: "已提交".into(),
                    detail: Some(json!({"success": true, "result": {"status": "approval_required", "approval_id": "apr_1", "speech": "已提交，请在电脑上的 MOSS 里确认后开始。"}}).to_string()),
                },
                Some("big") => ToolHostResult { success: true, result: "完成".into(), detail: Some("字".repeat(5000)) },
                _ => ToolHostResult { success: false, result: "未找到该任务".into(), detail: None },
            }
        }
    }

    fn context() -> ToolContext {
        ToolContext {
            device_id: "aa:bb".into(),
            session_id: "s1".into(),
        }
    }

    #[tokio::test]
    async fn approval_results_are_spoken_verbatim_and_others_go_to_the_model() {
        let host = Arc::new(FakeHost {
            calls: Mutex::default(),
        });
        let tools = HostTools::new(host.clone());
        let specs = tools.tools();
        assert_eq!(specs.len(), 2);
        assert_eq!(
            specs[0].parameters["properties"]["action"]["type"],
            "string"
        );
        assert_eq!(specs[1].parameters, json!({"type": "object"}));

        let outcome = tools
            .call(
                &context(),
                "laser_workflow_tool",
                r#"{"action":"confirm_send","confirmed":true}"#,
            )
            .await;
        assert_eq!(
            outcome,
            ToolOutcome::Speak("已提交，请在电脑上的 MOSS 里确认后开始。".into())
        );
        let outcome = tools
            .call(&context(), "laser_workflow_tool", r#"{"action":"status"}"#)
            .await;
        let ToolOutcome::Reply(text) = outcome else {
            panic!()
        };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            (value["success"].as_bool(), value["result"].as_str()),
            (Some(false), Some("未找到该任务"))
        );
        let ToolOutcome::Reply(text) = tools
            .call(&context(), "laser_workflow_tool", r#"{"action":"big"}"#)
            .await
        else {
            panic!()
        };
        assert!(text.chars().count() < 1700 && text.contains("已截断"));
        // 参数不合法时不调用宿主
        let ToolOutcome::Reply(text) = tools
            .call(&context(), "laser_workflow_tool", "{not json")
            .await
        else {
            panic!()
        };
        assert!(text.contains("不是合法的 JSON"));
        {
            let calls = host.calls.lock().unwrap();
            assert_eq!(calls.len(), 3);
            assert_eq!(calls[0].2, "aa:bb", "宿主收到设备身份");
        }
        // 空参数按 {} 处理
        tools.call(&context(), "laser_workflow_tool", "").await;
        assert_eq!(host.calls.lock().unwrap()[3].1, json!({}));
    }

    #[test]
    fn approval_detection_needs_the_status_field() {
        let plain = ToolHostResult {
            success: true,
            result: "ok".into(),
            detail: Some(r#"{"result":{"status":"sending"}}"#.into()),
        };
        assert_eq!(approval_speech(&plain), None);
        let no_speech = ToolHostResult {
            success: true,
            result: "ok".into(),
            detail: Some(r#"{"result":{"status":"approval_required"}}"#.into()),
        };
        assert_eq!(
            approval_speech(&no_speech).as_deref(),
            Some(APPROVAL_SPEECH)
        );
        assert_eq!(
            approval_speech(&ToolHostResult {
                success: true,
                result: String::new(),
                detail: Some("not json".into())
            }),
            None
        );
    }

    /// NV8：工具清单每轮重新读取，宿主关闭工具后下一轮对话就看不到它。
    #[test]
    fn tool_list_is_read_again_every_turn() {
        struct SwitchHost(std::sync::atomic::AtomicBool);
        impl ToolHost for SwitchHost {
            fn list(&self) -> Value {
                if self.0.load(std::sync::atomic::Ordering::SeqCst) {
                    json!({"tools": [{"name": "a_tool"}, {"name": "b_tool"}]})
                } else {
                    json!({"tools": [{"name": "a_tool"}]})
                }
            }
            fn call(&self, _: &ToolContext, _: &str, _: &Value) -> ToolHostResult {
                ToolHostResult {
                    success: true,
                    result: String::new(),
                    detail: None,
                }
            }
        }
        let host = Arc::new(SwitchHost(std::sync::atomic::AtomicBool::new(true)));
        let tools = HostTools::new(host.clone());
        assert_eq!(tools.tools().len(), 2);
        host.0.store(false, std::sync::atomic::Ordering::SeqCst);
        let names: Vec<String> = tools.tools().into_iter().map(|spec| spec.name).collect();
        assert_eq!(names, ["a_tool"]);
    }
}
