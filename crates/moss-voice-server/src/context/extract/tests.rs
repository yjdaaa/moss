//! NV19-03 extraction with a fake LLM and the in-memory store. No fixed sleeps: tests await
//! the spawned task and gate the fake model with `Notify`, so they do not depend on timing.
use super::*;
use crate::context::fake_store::FakeStore;
use crate::context::service::{ContextStore, ServiceError};
use crate::context::{ContextError, Evidence, TrustedScope};
use crate::person_context::PersonScope;
use moss_voice_providers::{BoxFuture, LlmReply, ProviderError, ToolCall, ToolSpec};
use std::sync::atomic::Ordering;
use tokio::sync::Notify;

struct FakeLlm {
    text: String,
    tool_call: bool,
    gate: Option<Arc<Notify>>,
    seen: Mutex<Vec<String>>,
}

impl FakeLlm {
    fn new(text: &str) -> Arc<Self> {
        Arc::new(Self { text: text.into(), tool_call: false, gate: None, seen: Mutex::new(Vec::new()) })
    }
    fn gated(text: &str, gate: Arc<Notify>) -> Arc<Self> {
        Arc::new(Self { text: text.into(), tool_call: false, gate: Some(gate), seen: Mutex::new(Vec::new()) })
    }
}

impl Llm for FakeLlm {
    fn chat<'a>(&'a self, messages: &'a [ChatMessage], tools: &'a [ToolSpec], _deltas: mpsc::UnboundedSender<String>) -> BoxFuture<'a, Result<LlmReply, ProviderError>> {
        Box::pin(async move {
            assert!(tools.is_empty(), "提取不能看到可调用的工具");
            assert_eq!(messages.len(), 2);
            self.seen.lock().unwrap().push(messages[1].content.clone().unwrap_or_default());
            if let Some(gate) = &self.gate {
                gate.notified().await;
            }
            let tool_calls = if self.tool_call { vec![ToolCall::new("call-1", "start_job", "{}")] } else { vec![] };
            Ok(LlmReply { text: self.text.clone(), tool_calls })
        })
    }
}

fn setup() -> (Arc<FakeStore>, Arc<ContextService>, Arc<ContextExtractor>) {
    let store = Arc::new(FakeStore::default());
    let service = ContextService::new(store.clone());
    let extractor = ContextExtractor::new(service.clone());
    extractor.configure(true, || Ok(())).unwrap();
    (store, service, extractor)
}

fn device() -> TrustedScope {
    TrustedScope::device_grant("dev-a", 1).unwrap()
}

fn project() -> TrustedScope {
    TrustedScope::desktop_selection(ScopeKind::Project, "p1", 1).unwrap()
}

fn voice(with_project: bool) -> AccessContext {
    AccessContext::voice(device(), None, with_project.then(project)).unwrap()
}

fn owner() -> AccessContext {
    AccessContext::desktop_private(vec![device(), project()])
}

fn history() -> Vec<ChatMessage> {
    vec![
        ChatMessage::user("以后回答先说预计时长"),
        ChatMessage::assistant("好的"),
        ChatMessage::user("我的密码是 hunter2"),
    ]
}

fn machine() -> Option<Applicability> {
    Some(Applicability::new("MOSS-550W", Some("1.2.0")).unwrap())
}

const FULL: &str = r#"```json
{"operations":[
 {"op":"add","category":"preference","content":"回答先说预计时长"},
 {"op":"add","category":"task_context","content":"本周先完成椴木样品"},
 {"op":"add","category":"troubleshooting_experience","content":"边缘发黑时降低功率后改善"},
 {"op":"add","category":"device_fact","content":"主板是 ESP32"},
 {"op":"add","category":"preference","content":"我的 api_key 是 x"},
 {"op":"update","id":"ctx-1","content":"改掉"},
 {"op":"add","category":"skill","content":"安装插件"},
 {"op":"add","category":"preference","content":"回答先说预计时长"}
]}
```"#;

fn rows(store: &FakeStore) -> Vec<crate::context::ContextMemory> {
    store.rows.lock().unwrap().values().cloned().collect()
}

#[tokio::test]
async fn nv19_extraction_is_off_by_default_and_voice_only() {
    let store = Arc::new(FakeStore::default());
    let extractor = ContextExtractor::new(ContextService::new(store.clone()));
    assert!(!extractor.enabled());
    assert!(!extractor.queue(voice(true), machine(), &history(), FakeLlm::new(FULL)), "默认关闭");
    extractor.configure(true, || Ok(())).unwrap();
    assert!(extractor.spawn(owner(), machine(), &history(), FakeLlm::new(FULL)).is_none(), "只处理语音会话");
    assert!(extractor.spawn(voice(true), machine(), &[ChatMessage::assistant("只有助手")], FakeLlm::new(FULL)).is_none(), "没有用户发言不提取");
    assert!(rows(&store).is_empty());
}

#[tokio::test]
async fn nv19_extraction_adds_only_valid_candidates_and_records_the_manifest() {
    let (store, _, extractor) = setup();
    let llm = FakeLlm::new(FULL);
    extractor.spawn(voice(true), machine(), &history(), llm.clone()).unwrap().await.unwrap();

    let input = llm.seen.lock().unwrap().join("\n");
    assert!(input.contains("预计时长") && !input.contains("hunter2"), "敏感行在交给模型前已去掉");

    let added = rows(&store);
    assert_eq!(added.len(), 3);
    for row in &added {
        assert_eq!(row.verification, Verification::Candidate);
        assert_eq!(row.source.kind(), SourceKind::ConversationExtraction);
        assert!(row.source.reference().is_some_and(|r| r.starts_with("extract:run-")));
        assert!(row.expires_at.is_some_and(|at| at > row.created_at + CANDIDATE_TTL_SECONDS - 1.0), "未确认的候选会到期");
    }
    let by_category = |c: Category| added.iter().find(|r| r.category == c).unwrap();
    assert_eq!(by_category(Category::Preference).scope.kind(), ScopeKind::Device);
    assert_eq!(by_category(Category::TaskContext).scope.kind(), ScopeKind::Project);
    assert_eq!(by_category(Category::TroubleshootingExperience).applicability, machine(), "故障经验使用宿主记录的机型");

    let runs = store.list_runs(&ScopeKey::new(ScopeKind::Device, "dev-a").unwrap(), 10).unwrap();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run.adds.len(), 3);
    let reasons: Vec<(&str, &str)> = run.skipped.iter().map(|s| (s.op.as_str(), s.reason.as_str())).collect();
    for expected in [
        ("add", "context_source_not_allowed"),
        ("add", "context_content_sensitive"),
        ("update", "op_not_allowed"),
        ("add", "category_unknown"),
        ("add", "duplicate"),
    ] {
        assert!(reasons.contains(&expected), "缺少 {expected:?}，实际 {reasons:?}");
    }
    assert!(!run.skipped_json().contains("api_key") && !run.adds_json().contains("预计"), "清单不保存内容");
    assert_eq!(run.skipped.len(), 5);
}

#[tokio::test]
async fn nv19_missing_scope_or_device_record_skips_instead_of_guessing() {
    let (store, _, extractor) = setup();
    extractor.spawn(voice(false), None, &history(), FakeLlm::new(FULL)).unwrap().await.unwrap();
    let added = rows(&store);
    assert_eq!(added.len(), 1, "只剩偏好");
    let run = &store.list_runs(&ScopeKey::new(ScopeKind::Device, "dev-a").unwrap(), 10).unwrap()[0];
    assert!(run.skipped.iter().any(|s| s.reason == "no_project_scope"));
    assert!(run.skipped.iter().any(|s| s.reason == "applicability_unknown"));
}

#[tokio::test]
async fn nv19_bad_output_and_tool_calls_add_nothing_but_are_recorded() {
    let (store, _, extractor) = setup();
    extractor.spawn(voice(true), machine(), &history(), FakeLlm::new("好的，我记住了")).unwrap().await.unwrap();
    let mut llm = FakeLlm { text: FULL.into(), tool_call: true, gate: None, seen: Mutex::new(Vec::new()) };
    llm.tool_call = true;
    extractor.spawn(voice(true), machine(), &history(), Arc::new(llm)).unwrap().await.unwrap();
    assert!(rows(&store).is_empty());
    let runs = store.list_runs(&ScopeKey::new(ScopeKind::Device, "dev-a").unwrap(), 10).unwrap();
    let reasons: Vec<_> = runs.iter().flat_map(|r| r.skipped.iter().map(|s| s.reason.clone())).collect();
    assert!(reasons.contains(&"invalid_output".to_string()) && reasons.contains(&"tool_calls_rejected".to_string()));
    assert!(runs.iter().all(|r| r.adds.is_empty()));
}

#[tokio::test]
async fn nv19_clear_disable_or_unique_in_flight_drop_stale_results() {
    let (store, _, extractor) = setup();
    let device_key = ScopeKey::new(ScopeKind::Device, "dev-a").unwrap();

    // Cleared while the model was still answering: nothing from the old run lands.
    let gate = Arc::new(Notify::new());
    let task = extractor.spawn(voice(true), machine(), &history(), FakeLlm::gated(FULL, gate.clone())).unwrap();
    assert!(extractor.spawn(voice(true), machine(), &history(), FakeLlm::new(FULL)).is_none(), "同一设备同时只有一个提取");
    extractor.scope_changed(&device_key, || Ok::<_, ()>(())).unwrap();
    gate.notify_one();
    task.await.unwrap();
    assert!(rows(&store).is_empty() && store.runs.lock().unwrap().is_empty());

    // Switched off while running.
    let gate = Arc::new(Notify::new());
    let task = extractor.spawn(voice(true), machine(), &history(), FakeLlm::gated(FULL, gate.clone())).unwrap();
    extractor.configure(false, || Ok(())).unwrap();
    gate.notify_one();
    task.await.unwrap();
    assert!(rows(&store).is_empty());

    // Project switched: the project epoch changes, the whole run is discarded.
    extractor.configure(true, || Ok(())).unwrap();
    let gate = Arc::new(Notify::new());
    let task = extractor.spawn(voice(true), machine(), &history(), FakeLlm::gated(FULL, gate.clone())).unwrap();
    extractor.scope_changed(&ScopeKey::new(ScopeKind::Project, "p1").unwrap(), || Ok::<_, ()>(())).unwrap();
    gate.notify_one();
    task.await.unwrap();
    assert!(rows(&store).is_empty());

    // After the old run finished, a new one is accepted again.
    extractor.spawn(voice(true), machine(), &history(), FakeLlm::new(FULL)).unwrap().await.unwrap();
    assert_eq!(rows(&store).len(), 3);
}

#[tokio::test]
async fn nv19_host_revocation_at_commit_writes_nothing_but_a_manifest() {
    let (store, _, extractor) = setup();
    store.stale.store(true, Ordering::SeqCst);
    extractor.spawn(voice(true), machine(), &history(), FakeLlm::new(FULL)).unwrap().await.unwrap();
    assert!(rows(&store).is_empty());
    let run = &store.list_runs(&ScopeKey::new(ScopeKind::Device, "dev-a").unwrap(), 10).unwrap()[0];
    assert!(run.adds.is_empty());
    assert!(run.skipped.iter().any(|s| s.reason == "context_scope_stale"));
}

#[tokio::test]
async fn nv19_revert_removes_untouched_candidates_once_and_keeps_user_changes() {
    let (store, service, extractor) = setup();
    extractor.spawn(voice(true), machine(), &history(), FakeLlm::new(FULL)).unwrap().await.unwrap();
    let device_key = ScopeKey::new(ScopeKind::Device, "dev-a").unwrap();
    let run = store.list_runs(&device_key, 10).unwrap().remove(0);
    let confirmed_id = rows(&store).into_iter().find(|r| r.category == Category::Preference).unwrap().id;
    service.set_verification(&owner(), &confirmed_id, 1, Verification::Confirmed, Some(&Evidence::UserConfirmed), 1.0).unwrap();

    assert_eq!(service.revert_extraction(&voice(true), &run.id, 2.0).unwrap_err(), ServiceError::Invalid(ContextError::AccessDenied));
    let report = service.revert_extraction(&owner(), &run.id, 2.0).unwrap();
    assert_eq!(report.removed, 2, "项目范围里的候选也能撤销");
    assert_eq!(report.kept, vec![confirmed_id.clone()], "用户已确认的保留");
    let live: Vec<_> = rows(&store).into_iter().filter(|r| r.deleted_at.is_none()).collect();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, confirmed_id);
    assert_eq!(service.revert_extraction(&owner(), &run.id, 3.0).unwrap_err(), ServiceError::Conflict, "只能撤销一次");
    assert_eq!(service.revert_extraction(&owner(), "run-missing", 3.0).unwrap_err(), ServiceError::NotFound);
    assert_eq!(service.list_extractions(&owner(), &device_key, 10).unwrap()[0].reverted_at, Some(2.0));
}

#[tokio::test]
async fn nv19_calibrated_speaker_preferences_go_to_the_person_scope() {
    let (store, _, extractor) = setup();
    let person = TrustedScope::calibrated_person(&PersonScope::candidate("alice", 1, 1).unwrap()).unwrap();
    let access = AccessContext::voice(device(), Some(person), None).unwrap();
    extractor.spawn(access, None, &history(), FakeLlm::new(r#"{"operations":[{"op":"add","category":"preference","content":"喜欢简短回答"}]}"#)).unwrap().await.unwrap();
    let added = rows(&store);
    assert_eq!(added.len(), 1);
    assert_eq!((added[0].scope.kind(), added[0].scope.id()), (ScopeKind::Person, "alice"));
}

#[test]
fn nv19_operation_parser_limits_count_and_sanitizes_op_names() {
    let many = format!("{{\"operations\":[{}]}}", vec![r#"{"op":"add","category":"preference","content":"a"}"#; 10].join(","));
    let (adds, skipped) = parse_operations(&many).unwrap();
    assert_eq!(adds.len(), MAX_OPERATIONS);
    assert_eq!(skipped.iter().filter(|s| s.reason == "too_many_operations").count(), 2);
    assert_eq!(parse_operations("{\"ops\":[]}").unwrap_err(), "invalid_output");
    assert_eq!(parse_operations(r#"{"operations":[{"op":"add","category":"preference"}]}"#).unwrap().1[0].reason, "content_missing");
    assert_eq!(skip("rm -rf /", "x").op, "unknown");
    let ops = parse_operations(r#"{"operations":[{"op":"delete","id":"ctx-2"},{"op":"Drop Table"},{"op":"invalidate","id":"ctx-3"}]}"#).unwrap().1;
    let reasons: Vec<_> = ops.iter().map(|s| (s.op.as_str(), s.reason.as_str())).collect();
    assert_eq!(reasons, vec![("delete", "op_not_allowed"), ("unknown", "op_not_allowed"), ("invalidate", "op_not_allowed")], "模型不能修改或删除记忆");
    assert_eq!(parse_operations("{\"operations\":[]}").unwrap().0.len(), 0);
}

#[tokio::test]
async fn nv19_session_close_hook_uses_host_access_and_respects_the_switch() {
    use crate::context::retrieve::ContextAccessProvider;
    use crate::conversation::ToolContext;
    struct Access(bool);
    impl ContextAccessProvider for Access {
        fn access(&self, context: &ToolContext, _: Option<&PersonScope>) -> Option<(AccessContext, Option<Applicability>)> {
            self.0.then(|| (AccessContext::voice(TrustedScope::device_grant(&context.device_id, 1).unwrap(), None, None).unwrap(), machine()))
        }
    }
    let (store, _, extractor) = setup();
    let context = ToolContext { device_id: "dev-a".into(), session_id: "s1".into() };
    let denied = ContextExtraction { extractor: extractor.clone(), access: Arc::new(Access(false)) };
    assert!(!denied.session_closed(&context, &history(), FakeLlm::new(FULL)), "宿主不给可信范围时不提取");
    extractor.configure(false, || Ok(())).unwrap();
    let hook = ContextExtraction { extractor: extractor.clone(), access: Arc::new(Access(true)) };
    assert!(!hook.session_closed(&context, &history(), FakeLlm::new(FULL)), "开关关闭时不提取");
    extractor.configure(true, || Ok(())).unwrap();
    assert!(hook.session_closed(&context, &history(), FakeLlm::new(FULL)));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while store.runs.lock().unwrap().is_empty() && tokio::time::Instant::now() < deadline {
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(store.runs.lock().unwrap().len(), 1);
    assert!(rows(&store).iter().all(|r| r.scope.id() == "dev-a"));
}
