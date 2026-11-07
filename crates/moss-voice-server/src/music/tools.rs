//! NV14 opt-in read-only name-to-track selection. The model can never supply
//! a path, directory, URL or arbitrary filesystem handle.
use super::index::{MusicChoice, MusicIndex};
use crate::{
    conversation::{ToolContext, ToolExecutor, ToolOutcome},
    tools::ToolHostResult,
};
use moss_voice_providers::{BoxFuture, ToolSpec};
use serde_json::{Value, json};
use std::sync::Arc;

const PLAY: &str = "play_music";
pub struct MusicTools {
    underlying: Arc<dyn ToolExecutor>,
    index: Arc<MusicIndex>,
}
impl MusicTools {
    pub fn new(underlying: Arc<dyn ToolExecutor>, index: Arc<MusicIndex>) -> Self {
        Self { underlying, index }
    }
}
impl ToolExecutor for MusicTools {
    fn tools(&self) -> Vec<ToolSpec> {
        let mut list = self.underlying.tools();
        list.retain(|tool| tool.name != PLAY);
        list.push(ToolSpec {
            name:PLAY.into(),
            description:"播放用户明确配置的本地音乐目录中的一首歌曲。按歌名匹配或随机选择，遇到重名时先向用户澄清。不接受路径或网址。".into(),
            parameters:json!({
                "type":"object","properties":{
                    "query":{"type":"string","description":"歌名，随机播放时省略"},
                    "random":{"type":"boolean","description":"设为 true 随机播放"}
                },"additionalProperties":false
            }),
        });
        list
    }
    fn call_raw<'a>(
        &'a self,
        context: &'a ToolContext,
        name: &'a str,
        args: &'a str,
    ) -> BoxFuture<'a, ToolHostResult> {
        if name == PLAY {
            return Box::pin(async {
                ToolHostResult {
                    success: false,
                    result: "音乐只能经设备授权会话播放".into(),
                    detail: None,
                }
            });
        }
        self.underlying.call_raw(context, name, args)
    }
    fn call<'a>(
        &'a self,
        context: &'a ToolContext,
        name: &'a str,
        args: &'a str,
    ) -> BoxFuture<'a, ToolOutcome> {
        if name != PLAY {
            return self.underlying.call(context, name, args);
        }
        Box::pin(async move {
            let Ok(value) = serde_json::from_str::<Value>(args) else {
                return ToolOutcome::Reply("无效音乐请求".into());
            };
            let Some(fields) = value.as_object() else {
                return ToolOutcome::Reply("音乐请求参数必须为对象".into());
            };
            if fields.keys().any(|key| key != "query" && key != "random")
                || fields.get("random").is_some_and(|v| !v.is_boolean())
                || fields.get("query").is_some_and(|v| !v.is_string())
            {
                return ToolOutcome::Reply("音乐请求不能包含路径、网址或其他字段".into());
            }
            let random = fields
                .get("random")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let query = fields.get("query").and_then(Value::as_str);
            match self.index.choose(query, random) {
                Ok(MusicChoice::Selected(track)) => ToolOutcome::PlayMusic {
                    track_id: track.track_id,
                    title: track.title,
                },
                Ok(MusicChoice::Ambiguous(items)) => ToolOutcome::Reply(
                    json!({
                        "success":false,"message":"找到多首同名或相似歌曲，请按候选编号澄清；未选择前不会播放",
                        "candidates":items.iter().enumerate().map(|(index,item)|json!({"number":index+1,"title":item.title,"track_id":item.track_id})).collect::<Vec<_>>()
                    })
                    .to_string(),
                ),
                Ok(MusicChoice::Missing) => ToolOutcome::Reply("本地音乐目录里没有找到歌曲".into()),
                Err(_) => ToolOutcome::Reply("无效的歌名或随机播放参数".into()),
            }
        })
    }
    fn recent_workflows<'a>(&'a self, context: &'a ToolContext) -> BoxFuture<'a, Option<Value>> {
        self.underlying.recent_workflows(context)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::NoTools;
    use std::fs;
    #[test]
    fn model_only_selects_indexed_track_and_unknown_fields_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["晨歌.wav", "晨歌.mp3", "夜雨.wav"] {
            fs::write(temp.path().join(name), b"fake").unwrap();
        }
        let tools = MusicTools::new(
            Arc::new(NoTools),
            Arc::new(MusicIndex::scan(temp.path()).unwrap()),
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let ctx = ToolContext {
            device_id: "fake".into(),
            session_id: "fake".into(),
        };
        assert!(tools.tools().iter().any(|tool| tool.name == PLAY));
        let selected = rt.block_on(tools.call(&ctx, PLAY, r#"{"query":"夜雨"}"#));
        assert!(matches!(selected, ToolOutcome::PlayMusic { .. }));
        let ambiguous = rt.block_on(tools.call(&ctx, PLAY, r#"{"query":"晨歌"}"#));
        let ToolOutcome::Reply(text) = ambiguous else {
            panic!("duplicates must be ambiguous");
        };
        assert!(text.contains("晨歌") && !text.contains(temp.path().to_string_lossy().as_ref()));
        for input in [
            r#"{"path":"C:\\Windows\\x"}"#,
            r#"{"url":"https://evil.example"}"#,
            r#"{"random":true,"query":"夜雨"}"#,
            r#"{"query":"../secrets"}"#,
        ] {
            assert!(matches!(
                rt.block_on(tools.call(&ctx, PLAY, input)),
                ToolOutcome::Reply(_)
            ));
        }
        assert!(!rt.block_on(tools.call_raw(&ctx, PLAY, "{}")).success);
    }

    #[test]
    fn desktop_wrapper_stack_forwards_recent_workflows_for_job_done_announcement() {
        use crate::home_tools::{HomeAvailability, HomeHost, HomeTools};
        use crate::plugins::runtime::{CombinedTools, InfoTools, PluginHost};
        use std::sync::Mutex;

        struct LeafTools(Mutex<Value>);
        impl ToolExecutor for LeafTools {
            fn tools(&self) -> Vec<ToolSpec> {
                Vec::new()
            }
            fn call<'a>(
                &'a self,
                _: &'a ToolContext,
                _: &'a str,
                _: &'a str,
            ) -> BoxFuture<'a, ToolOutcome> {
                Box::pin(async { ToolOutcome::Reply("{}".into()) })
            }
            fn recent_workflows<'a>(&'a self, _: &'a ToolContext) -> BoxFuture<'a, Option<Value>> {
                let snapshot = self.0.lock().unwrap().clone();
                Box::pin(async move { Some(snapshot) })
            }
        }
        struct InertPlugins;
        impl PluginHost for InertPlugins {
            fn config(&self) -> Value {
                json!({})
            }
            fn secret(&self, _: &str) -> Option<String> {
                None
            }
            fn permitted(&self, _: &ToolContext) -> bool {
                false
            }
        }
        struct InertHome;
        impl HomeHost for InertHome {
            fn availability(&self) -> HomeAvailability {
                HomeAvailability {
                    read: false,
                    control: false,
                }
            }
            fn permitted(&self, _: &ToolContext) -> bool {
                false
            }
            fn invoke(&self, _: &ToolContext, _: &str, _: &Value) -> Value {
                json!({})
            }
        }

        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("夜雨.wav"), b"fake").unwrap();
        let leaf = Arc::new(LeafTools(Mutex::new(json!({
            "workflows": [{"workflow_id": "wf-wrap-1", "status": "sending", "send_result": {"lines_sent": 10, "total_lines": 50}}]
        }))));
        let combined: Arc<dyn ToolExecutor> = Arc::new(CombinedTools {
            original: leaf.clone(),
            info: Arc::new(InfoTools::new(Arc::new(InertPlugins)).unwrap()),
        });
        let home: Arc<dyn ToolExecutor> = Arc::new(HomeTools {
            original: combined,
            home: Arc::new(InertHome),
        });
        let full_stack: Arc<dyn ToolExecutor> = Arc::new(MusicTools::new(
            home,
            Arc::new(MusicIndex::scan(temp.path()).unwrap()),
        ));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let ctx = ToolContext {
            device_id: "dev-1".into(),
            session_id: "sess-1".into(),
        };
        let mut poller = crate::jobs::JobWatcher::new();
        assert!(
            rt.block_on(poller.poll(full_stack.as_ref(), &ctx))
                .is_empty(),
            "首次轮询建立基线"
        );

        *leaf.0.lock().unwrap() = json!({
            "workflows": [{"workflow_id": "wf-wrap-1", "status": "completed", "send_result": {"lines_sent": 50, "total_lines": 50}}]
        });
        let ended = rt.block_on(poller.poll(full_stack.as_ref(), &ctx));
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].workflow_id, "wf-wrap-1");
        assert!(ended[0].speech().contains("50"));
    }
}
