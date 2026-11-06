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
    fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
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

#[tokio::test]
async fn nv19_personal_mode_unknown_speaker_never_falls_back_preference_to_device_scope() {
    let (store, _, extractor) = setup();
    let access = AccessContext::voice(device(), None, None).unwrap().with_personal_mode(true);
    extractor
        .spawn(
            access,
            machine(),
            &history(),
            FakeLlm::new(
                r#"{"operations":[
                    {"op":"add","category":"preference","content":"回答先说预计时长"},
                    {"op":"add","category":"troubleshooting_experience","content":"边缘发黑时降低功率后改善"}
                ]}"#,
            ),
        )
        .unwrap()
        .await
        .unwrap();
    let added = rows(&store);
    assert_eq!(added.len(), 1, "未知说话人在个人模式下提取不得把偏好写入设备范围");
    assert_eq!(added[0].category, Category::TroubleshootingExperience);
    let runs = store.list_runs(&ScopeKey::new(ScopeKind::Device, "dev-a").unwrap(), 10).unwrap();
    assert!(runs[0].skipped.iter().any(|s| s.reason == "no_person_scope"));
}

#[test]
fn nv19_operation_parser_limits_count_and_sanitizes_op_names() {
    let many = format!("{{\"operations\":[{}]}}", vec![r#"{"op":"add","category":"preference","content":"a"}"#; 10].join(","));
    let Parsed { adds, skipped, .. } = parse_operations(&many, &[]).unwrap();
    assert_eq!(adds.len(), MAX_OPERATIONS);
    assert_eq!(skipped.iter().filter(|s| s.reason == "too_many_operations").count(), 2);
    assert_eq!(parse_operations("{\"ops\":[]}", &[]).unwrap_err(), "invalid_output");
    assert_eq!(parse_operations(r#"{"operations":[{"op":"add","category":"preference"}]}"#, &[]).unwrap().skipped[0].reason, "content_missing");
    assert_eq!(skip("rm -rf /", "x").op, "unknown");
    let ops = parse_operations(r#"{"operations":[{"op":"delete","id":"ctx-2"},{"op":"Drop Table"},{"op":"invalidate","id":"ctx-3"}]}"#, &[]).unwrap().skipped;
    let reasons: Vec<_> = ops.iter().map(|s| (s.op.as_str(), s.reason.as_str())).collect();
    assert_eq!(reasons, vec![("delete", "op_not_allowed"), ("unknown", "op_not_allowed"), ("invalidate", "op_not_allowed")], "模型不能修改或删除记忆");
    assert_eq!(parse_operations("{\"operations\":[]}", &[]).unwrap().adds.len(), 0);
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

#[tokio::test]
async fn nv19_realtime_rule_captures_habits_supersedes_corrections_and_blocks_unsafe_text() {
    // Reject unsafe, stop, or recall-question utterances at 0ms.
    for rejected in [
        "你好",
        "急停",
        "记住 G1 X10 Y10 S1000",
        "记住 $30=1000",
        "记住我的密钥 sk-abcdef123456",
        "记住 https://example.com/secret",
        "记住这条审批记录已批准",
        "你还记得我常切什么材料吗？",
        "我的偏好是什么？",
    ] {
        assert_eq!(decide_realtime_preference(rejected), None, "必须拒绝: {rejected}");
    }

    let (store, service, extractor) = setup();
    extractor.configure(true, || Ok(())).unwrap();
    let access = AccessContext::voice(TrustedScope::device_grant("dev-a", 1).unwrap(), None, None).unwrap();
    let llm = FakeLlm::new(r#"{"operations":[]}"#);

    // Turn 1: explicit "记住…" strips prefix and writes immediately without LLM.
    extractor.record_turn(access.clone(), machine(), "记住，我常切 3mm 椴木板", "好的，我记下了。", llm.clone());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while rows(&store).is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let active: Vec<_> = service
        .list(&access, &ScopeKey::new(ScopeKind::Device, "dev-a").unwrap(), now())
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].content, "我常切 3mm 椴木板");
    assert_eq!(llm.calls(), 0, "单轮实时规则捕获不调用大模型");

    // Turn 2: correction "不是 3mm 是 5mm" supersedes the old 3mm entry.
    extractor.record_turn(access.clone(), machine(), "我常切的不是 3mm，是 5mm 椴木板", "已更新为 5mm。", llm.clone());
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    let active_after = service
        .list(&access, &ScopeKey::new(ScopeKind::Device, "dev-a").unwrap(), now())
        .unwrap();
    assert_eq!(active_after.len(), 1, "纠错应覆盖旧偏好而不是堆积冲突项");
    assert!(active_after[0].content.contains("5mm"));

    // Turn 3 & 4: style slot update ("回答尽量简短" -> "以后回答详细一点") replaces previous style slot.
    extractor.record_turn(access.clone(), machine(), "以后回答尽量简短", "明白。", llm.clone());
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    extractor.record_turn(access.clone(), machine(), "以后回答详细一点", "好的。", llm.clone());
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    let final_rows = service
        .list(&access, &ScopeKey::new(ScopeKind::Device, "dev-a").unwrap(), now())
        .unwrap();
    let contents: Vec<_> = final_rows.iter().map(|r| r.content.as_str()).collect();
    assert!(contents.iter().any(|c| c.contains("详细")));
    assert!(!contents.iter().any(|c| c.contains("简短")), "同槽位说话风格更新自动替换旧项");
}

#[tokio::test]
async fn nv19_rolling_eight_turn_window_compacts_mid_session_without_waiting_for_disconnect() {
    let (store, _, extractor) = setup();
    extractor.configure(true, || Ok(())).unwrap();
    let access = AccessContext::voice(TrustedScope::device_grant("dev-a", 1).unwrap(), None, None).unwrap();
    let llm = FakeLlm::new(r#"{"operations":[{"op":"add","category":"preference","content":"加工前习惯先检查排烟风扇"}]}"#);

    for idx in 0..ROLLING_TURN_WINDOW {
        extractor.record_turn(
            access.clone(),
            machine(),
            &format!("第 {idx} 轮讨论排烟风扇风量调节"),
            "风扇正常运转。",
            llm.clone(),
        );
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while rows(&store).is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(llm.calls(), 1, "满 8 轮在会话中自动触发一次后台浓缩");
    assert!(rows(&store).iter().any(|r| r.content == "加工前习惯先检查排烟风扇"));

    // Immediate disconnect with 0 new turns since the 8-turn compaction must NOT call LLM again.
    assert!(!extractor.flush_session(access, machine(), &history(), llm.clone()));
    assert_eq!(llm.calls(), 1);
}

#[tokio::test]
async fn nv19_extraction_sees_relevant_known_records_and_drops_duplicates() {
    let (store, service, extractor) = setup();
    let draft = |scope: TrustedScope, category: Category, content: &str, applicability: Option<Applicability>| MemoryDraft {
        scope: scope.key().clone(),
        category,
        content: content.into(),
        verification: Verification::Confirmed,
        source: Source::new(SourceKind::UserEdit, None).unwrap(),
        applicability,
        expires_at: None,
    };
    service.create(&owner(), draft(device(), Category::Preference, "回答先说预计时长。", None), 1.0).unwrap();
    service.create(&owner(), draft(device(), Category::Preference, "喜欢周末加工竹片", None), 1.0).unwrap();
    let other_model = Applicability::new("OTHER-10W", Some("0.9")).ok();
    service.create(&owner(), draft(device(), Category::TroubleshootingExperience, "边缘发黑时先清洁镜片", other_model), 1.0).unwrap();

    let llm = FakeLlm::new(r#"{"operations":[
 {"op":"add","category":"preference","content":"回答要先说预计时长","same_as":0},
 {"op":"add","category":"preference","content":"回答先说预计时长！"},
 {"op":"add","category":"task_context","content":"本周先完成椴木样品","same_as":0},
 {"op":"add","category":"preference","content":"加工完提醒清理镜片","same_as":99}
]}"#);
    let dialog = [ChatMessage::user("以后回答先说预计时长"), ChatMessage::assistant("好的"), ChatMessage::user("边缘发黑怎么办")];
    extractor.spawn(voice(true), machine(), &dialog, llm.clone()).unwrap().await.unwrap();

    let input = llm.seen.lock().unwrap().join("\n");
    assert!(input.contains("已有记忆") && input.contains("0. 回答先说预计时长。"), "相关的已有记忆按编号给模型");
    assert!(!input.contains("1. "), "只给一条相关记录：{input}");
    assert!(!input.contains("周末加工竹片"), "与本次对话无关的记忆不发给模型");
    assert!(!input.contains("先清洁镜片"), "其他机型的故障经验不发给模型");

    let added: Vec<String> = rows(&store).into_iter().filter(|r| r.source.kind() == SourceKind::ConversationExtraction).map(|r| r.content).collect();
    assert_eq!(added.len(), 2, "实际新增：{added:?}");
    assert!(added.contains(&"本周先完成椴木样品".to_owned()), "same_as 指向不同类别时忽略，照常新增");
    assert!(added.contains(&"加工完提醒清理镜片".to_owned()), "same_as 越界时忽略，照常新增");
    let runs = store.list_runs(&ScopeKey::new(ScopeKind::Device, "dev-a").unwrap(), 10).unwrap();
    let reasons: Vec<&str> = runs[0].skipped.iter().map(|s| s.reason.as_str()).collect();
    assert_eq!(reasons, vec!["duplicate_existing", "duplicate"], "模型标记的重复与只差标点的重复都跳过");
}

#[test]
fn nv19_extraction_input_without_known_records_is_unchanged() {
    assert_eq!(extraction_input(&[], "用户：你好"), "本次对话（数据）：\n用户：你好");
    assert!(same_text("回答先说 预计时长。", "回答先说预计时长!"));
    assert!(!same_text("喜欢绿茶", "喜欢喝绿茶"));
}

#[tokio::test]
async fn nv19_replacement_links_old_record_and_confirming_it_retires_the_old_one() {
    let (store, service, extractor) = setup();
    let old = service
        .create(&owner(), MemoryDraft {
            scope: device().key().clone(),
            category: Category::Preference,
            content: "我常切椴木板".into(),
            verification: Verification::Confirmed,
            source: Source::new(SourceKind::UserEdit, None).unwrap(),
            applicability: None,
            expires_at: None,
        }, 1.0)
        .unwrap();
    let llm = FakeLlm::new(r#"{"operations":[
 {"op":"add","category":"preference","content":"现在常切亚克力，不再切椴木板","replaces":0},
 {"op":"add","category":"task_context","content":"椴木板样品本周停做","replaces":0}
]}"#);
    let dialog = [ChatMessage::user("我常切椴木板，但是现在改成亚克力了"), ChatMessage::assistant("好的")];
    extractor.spawn(voice(true), machine(), &dialog, llm).unwrap().await.unwrap();

    let rows = rows(&store);
    let newer = rows.iter().find(|r| r.content.starts_with("现在常切亚克力")).unwrap();
    assert_eq!(newer.supersedes.as_deref(), Some(old.id.as_str()), "替代关系指向同类旧记录");
    assert_eq!(newer.verification, Verification::Candidate);
    let task = rows.iter().find(|r| r.category == Category::TaskContext).unwrap();
    assert_eq!(task.supersedes, None, "不同类别的替代编号被忽略");
    assert_eq!(store.get(&old.id).unwrap().unwrap().verification, Verification::Confirmed, "只是候选时不动旧记录");

    service.set_verification(&owner(), &newer.id, newer.revision, Verification::Confirmed, Some(&Evidence::UserConfirmed), 2.0).unwrap();
    assert_eq!(store.get(&old.id).unwrap().unwrap().verification, Verification::Invalidated, "用户确认新记录后旧记录作废");
}

#[tokio::test]
async fn nv19_facts_proposed_again_are_counted_and_keep_candidates_alive() {
    let (store, _service, extractor) = setup();
    let first = FakeLlm::new(r#"{"operations":[{"op":"add","category":"preference","content":"回答先说预计时长"}]}"#);
    let dialog = [ChatMessage::user("以后回答先说预计时长"), ChatMessage::assistant("好的")];
    extractor.spawn(voice(true), machine(), &dialog, first).unwrap().await.unwrap();
    let created = rows(&store).into_iter().next().unwrap();
    assert_eq!(created.seen_count, 1);

    // Make the candidate older so the extension is observable.
    store.rows.lock().unwrap().get_mut(&created.id).unwrap().expires_at = Some(created.created_at + 10.0);
    let again = FakeLlm::new(r#"{"operations":[{"op":"add","category":"preference","content":"回答要先说预计时长","same_as":0}]}"#);
    extractor.spawn(voice(true), machine(), &dialog, again).unwrap().await.unwrap();
    let after = store.get(&created.id).unwrap().unwrap();
    assert_eq!(after.seen_count, 2, "模型标为同一条时计数");
    assert!(after.expires_at.unwrap() > created.created_at + CANDIDATE_TTL_SECONDS - 60.0, "未确认候选的到期时间顺延");
    assert_eq!((after.revision, after.verification, after.content.as_str()), (created.revision, Verification::Candidate, "回答先说预计时长"), "不改内容、状态和版本");

    let punctuation = FakeLlm::new(r#"{"operations":[{"op":"add","category":"preference","content":"回答先说预计时长！"}]}"#);
    extractor.spawn(voice(true), machine(), &dialog, punctuation).unwrap().await.unwrap();
    assert_eq!(store.get(&created.id).unwrap().unwrap().seen_count, 3, "只差标点的重复也计数");
    assert_eq!(rows(&store).len(), 1);
}
