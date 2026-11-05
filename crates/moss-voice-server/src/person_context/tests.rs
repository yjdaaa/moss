use super::*;
use moss_voice_providers::{LlmReply, ProviderError, ToolSpec};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
pub(crate) struct Resolver {
    pub allowed: AtomicBool,
    pub calls: AtomicUsize,
    pub delay: Duration,
    pub replies: Mutex<std::collections::VecDeque<Option<String>>>,
}
impl Resolver {
    pub fn new(replies: &[Option<&str>]) -> Arc<Self> {
        Arc::new(Self {
            allowed: AtomicBool::new(true),
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            replies: Mutex::new(replies.iter().map(|v| v.map(str::to_owned)).collect()),
        })
    }
}
impl IdentityResolver for Resolver {
    fn identify<'a>(
        &'a self,
        _: &'a ToolContext,
        _: &'a [i16],
        _: u32,
        request: IdentityRequest,
    ) -> BoxFuture<'a, Option<PersonScope>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(request.utterance_generation > 0);
            tokio::time::sleep(self.delay).await;
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .flatten()
                .and_then(|id| PersonScope::candidate(&id, 1, request.directory_generation))
        })
    }
    fn permitted(&self, _: &ToolContext) -> bool {
        self.allowed.load(Ordering::SeqCst)
    }
    fn device_allowed(&self, _: &str) -> bool {
        self.allowed.load(Ordering::SeqCst)
    }
    fn current(&self, scope: &PersonScope) -> bool {
        scope.revision == 1 && ["a", "b"].contains(&scope.id.as_str())
    }
}
#[derive(Default)]
pub(crate) struct Store {
    pub rows: Mutex<HashMap<String, PersonSummary>>,
    pub loads: AtomicUsize,
    pub writes: AtomicUsize,
}
impl Store {
    pub fn seeded() -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(HashMap::from([
                (
                    "a".into(),
                    PersonSummary {
                        text: "甲喜欢茶".into(),
                        revision: 0,
                    },
                ),
                (
                    "b".into(),
                    PersonSummary {
                        text: "乙喜欢咖啡".into(),
                        revision: 0,
                    },
                ),
            ])),
            ..Default::default()
        })
    }
}
impl PersonMemoryStore for Store {
    fn load(&self, scope: &PersonScope) -> Result<Option<PersonSummary>, String> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        Ok(self.rows.lock().unwrap().get(scope.id()).cloned())
    }
    fn update(&self, scope: &PersonScope, expected: i64, text: &str) -> Result<bool, String> {
        let mut rows = self.rows.lock().unwrap();
        let Some(row) = rows.get_mut(scope.id()) else {
            return Ok(false);
        };
        if row.revision != expected {
            return Ok(false);
        }
        row.text = text.into();
        row.revision += 1;
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }
}
fn context() -> ToolContext {
    ToolContext {
        device_id: "synthetic-device".into(),
        session_id: "synthetic-session".into(),
    }
}
struct SummaryLlm {
    started: Arc<AtomicBool>,
    release: Arc<tokio::sync::Notify>,
}
impl Llm for SummaryLlm {
    fn chat<'a>(
        &'a self,
        _: &'a [ChatMessage],
        tools: &'a [ToolSpec],
        _: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> BoxFuture<'a, Result<LlmReply, ProviderError>> {
        Box::pin(async move {
            assert!(tools.is_empty());
            self.started.store(true, Ordering::SeqCst);
            self.release.notified().await;
            Ok(LlmReply {
                text: "新的合成偏好摘要".into(),
                tool_calls: vec![],
            })
        })
    }
}
#[tokio::test]
async fn a_b_unknown_histories_and_summaries_never_cross() {
    let resolver = Resolver::new(&[Some("a"), Some("b"), None]);
    let store = Store::seeded();
    let service = PersonContext::new(resolver, store.clone());
    let ctx = context();
    let a = service
        .start(&ctx, &vec![200; 32000], 16000)
        .finish()
        .await
        .unwrap();
    service.commit(
        &ctx,
        Some(&a),
        vec![
            ChatMessage::user("甲原话"),
            ChatMessage::assistant("甲回应"),
        ],
        4000,
    );
    let b = service
        .start(&ctx, &vec![200; 32000], 16000)
        .finish()
        .await
        .unwrap();
    assert!(service.history(&ctx, Some(&b)).is_empty());
    assert_eq!(service.history(&ctx, Some(&a)).len(), 2);
    let prompt = service.prompt_for(&ctx, Some(&b)).await.unwrap();
    assert!(prompt.contains("乙喜欢咖啡"));
    assert!(!prompt.contains("甲"));
    let loads = store.loads.load(Ordering::SeqCst);
    assert!(
        service
            .start(&ctx, &vec![200; 32000], 16000)
            .finish()
            .await
            .is_none()
    );
    assert!(service.history(&ctx, None).is_empty());
    assert!(service.prompt_for(&ctx, None).await.is_none());
    service.commit(&ctx, None, vec![ChatMessage::user("未知者")], 4000);
    assert_eq!(store.loads.load(Ordering::SeqCst), loads);
    assert_eq!(service.history(&ctx, Some(&a)).len(), 2);
}
#[tokio::test]
async fn delayed_identity_is_rejected_after_cancel_device_reallow_or_directory_change() {
    let resolver = Arc::new(Resolver {
        delay: Duration::from_millis(80),
        allowed: AtomicBool::new(true),
        calls: AtomicUsize::new(0),
        replies: Mutex::new(std::collections::VecDeque::from([
            Some("a".into()),
            Some("a".into()),
        ])),
    });
    let service = PersonContext::new(resolver.clone(), Store::seeded());
    let ctx = context();
    let task = service.start(&ctx, &vec![200; 32000], 16000);
    tokio::task::yield_now().await;
    service
        .device_changed(&ctx.device_id, || {
            resolver.allowed.store(false, Ordering::SeqCst);
            resolver.allowed.store(true, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
    assert!(task.finish().await.is_none());
    let task = service.start(&ctx, &vec![200; 32000], 16000);
    tokio::task::yield_now().await;
    service.directory_changed();
    assert!(task.finish().await.is_none());
    let task = service.start(&ctx, &vec![200; 32000], 16000);
    drop(task);
    tokio::task::yield_now().await;
    assert!(service.history(&ctx, None).is_empty());
}
#[tokio::test]
async fn deletion_conflict_has_no_effect_but_success_blocks_late_summary() {
    let resolver = Resolver::new(&[Some("a")]);
    let store = Store::seeded();
    let service = PersonContext::new(resolver, store.clone());
    let ctx = context();
    let a = service
        .start(&ctx, &vec![200; 32000], 16000)
        .finish()
        .await
        .unwrap();
    service.commit(&ctx, Some(&a), vec![ChatMessage::user("我偏好茶")], 4000);
    assert!(!service.person_deleted("a", || Ok(false)).unwrap());
    assert_eq!(service.history(&ctx, Some(&a)).len(), 1);
    let started = Arc::new(AtomicBool::new(false));
    let release = Arc::new(tokio::sync::Notify::new());
    assert!(service.queue_summary(
        &ctx,
        &a,
        &service.history(&ctx, Some(&a)),
        Arc::new(SummaryLlm {
            started: started.clone(),
            release: release.clone()
        })
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        while !started.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        service
            .person_deleted("a", || {
                store.rows.lock().unwrap().remove("a");
                Ok(true)
            })
            .unwrap()
    );
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        while service.state.lock().unwrap().in_flight.contains("a") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(store.writes.load(Ordering::SeqCst), 0);
    assert!(service.history(&ctx, Some(&a)).is_empty());
    assert!(service.prompt_for(&ctx, Some(&a)).await.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn abort_before_first_poll_releases_device_and_worker_slots() {
    let resolver = Resolver::new(&[Some("a")]);
    let service = PersonContext::new(resolver.clone(), Store::seeded());
    let ctx = context();
    drop(service.start(&ctx, &vec![200; 32000], 16000));
    tokio::time::timeout(Duration::from_secs(2), async {
        while service
            .state
            .lock()
            .unwrap()
            .identifying
            .contains(&ctx.device_id)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    assert_eq!(service.identify_workers.available_permits(), 2);
    assert!(
        service
            .start(&ctx, &vec![200; 32000], 16000)
            .finish()
            .await
            .is_some()
    );
}

#[tokio::test]
async fn resolved_scope_cannot_be_reused_after_regrant_or_on_another_session() {
    let resolver = Resolver::new(&[Some("a")]);
    let store = Store::seeded();
    let service = PersonContext::new(resolver.clone(), store.clone());
    let ctx = context();
    let a = service
        .start(&ctx, &vec![200; 32000], 16000)
        .finish()
        .await
        .unwrap();
    let other = ToolContext {
        session_id: "another-session".into(),
        ..ctx.clone()
    };
    assert!(!service.permitted(&other, Some(&a)));
    service.commit(&other, Some(&a), vec![ChatMessage::user("另一会话")], 4000);
    assert!(service.history(&other, Some(&a)).is_empty());
    service
        .device_changed(&ctx.device_id, || {
            resolver.allowed.store(false, Ordering::SeqCst);
            resolver.allowed.store(true, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
    assert!(!service.permitted(&ctx, Some(&a)));
    assert!(service.prompt_for(&ctx, Some(&a)).await.is_none());
    service.commit(&ctx, Some(&a), vec![ChatMessage::user("过期语句")], 4000);
    assert!(service.history(&ctx, Some(&a)).is_empty());
    assert_eq!(store.loads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn queued_summary_revalidates_before_model_and_manual_edit_wins_cas() {
    let resolver = Resolver::new(&[Some("a")]);
    let store = Store::seeded();
    let service = PersonContext::new(resolver.clone(), store.clone());
    let ctx = context();
    let a = service
        .start(&ctx, &vec![200; 32000], 16000)
        .finish()
        .await
        .unwrap();
    let held = service.workers.clone().acquire_many_owned(2).await.unwrap();
    let started = Arc::new(AtomicBool::new(false));
    let release = Arc::new(tokio::sync::Notify::new());
    let llm = Arc::new(SummaryLlm {
        started: started.clone(),
        release: release.clone(),
    });
    assert!(service.queue_summary(&ctx, &a, &[ChatMessage::user("合成偏好")], llm));
    service.device_changed(&ctx.device_id, || Ok(())).unwrap();
    drop(held);
    tokio::time::timeout(Duration::from_secs(2), async {
        while service.state.lock().unwrap().in_flight.contains("a") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!started.load(Ordering::SeqCst));
    assert_eq!(store.writes.load(Ordering::SeqCst), 0);
    let scope = PersonScope::candidate("a", 1, service.generation()).unwrap();
    assert!(
        service
            .operator_edit(&scope, 0, "操作者明确保存的摘要")
            .unwrap()
    );
    assert!(!service.operator_edit(&scope, 0, "过期摘要").unwrap());
    assert_eq!(store.rows.lock().unwrap()["a"].text, "操作者明确保存的摘要");
}

#[test]
fn embedding_threshold_margin_bad_templates_and_nonfinite_fail_closed() {
    let mut a = vec![0.; 192];
    a[0] = 1.;
    let mut b = vec![0.; 192];
    b[1] = 1.;
    let templates = vec![
        SpeakerTemplate {
            id: "a".into(),
            revision: 1,
            embedding: a.clone(),
        },
        SpeakerTemplate {
            id: "b".into(),
            revision: 1,
            embedding: b.clone(),
        },
    ];
    assert_eq!(
        match_embedding(&a, &templates, 1, 0.7, 0.05).unwrap().id(),
        "a"
    );
    assert_eq!(
        match_embedding(&b, &templates, 1, 0.7, 0.05).unwrap().id(),
        "b"
    );
    a[1] = 1.;
    assert!(match_embedding(&a, &templates, 1, 0.7, 0.05).is_none());
    for input in [
        vec![f32::NAN; 192],
        vec![0.; 192],
        vec![-1.; 192],
        vec![1.; 191],
    ] {
        assert!(match_embedding(&input, &templates, 1, 0.7, 0.05).is_none());
    }
    assert!(match_embedding(&b, &templates, 1, f32::NAN, 0.05).is_none());
    assert!(
        match_embedding(
            &b,
            &[SpeakerTemplate {
                id: "a".into(),
                revision: 1,
                embedding: vec![0.; 192]
            }],
            1,
            0.7,
            0.05
        )
        .is_none()
    );
}

fn calibration_vector(main: usize, noise: usize, weight: f32) -> Vec<f32> {
    let mut v = vec![0.0f32; 192];
    v[main] = 1.0;
    v[noise] = weight;
    v
}

fn calibration_templates() -> Vec<SpeakerTemplate> {
    ["a", "b"].iter().enumerate().map(|(i, id)| SpeakerTemplate { id: (*id).into(), revision: 1, embedding: calibration_vector(i, 100, 0.0) }).collect()
}

/// 简易标定（2026-10-14）：两人各三条测试音能区分时给出阈值；有人被认成别人、人数或条数不够都不通过。
#[test]
fn calibration_derives_threshold_and_rejects_confusable_or_incomplete_sets() {
    let templates = calibration_templates();
    let sample = |id: &str, main: usize, noise: usize, weight: f32| CalibrationSample { speaker_id: id.into(), embedding: calibration_vector(main, noise, weight) };
    let good: Vec<_> = (0..3).flat_map(|k| [sample("a", 0, 10 + k, 0.3), sample("b", 1, 20 + k, 0.3)]).collect();
    let report = calibrate(&templates, &good).unwrap();
    assert_eq!(report.people, 2);
    assert_eq!((report.genuine_trials, report.genuine_accepted, report.false_accepts), (6, 6, 0));
    assert!(report.threshold > report.max_impostor_score && report.threshold < 0.95, "{report:?}");
    assert!(report.threshold >= 0.5, "两人互不相像时阈值也不低于下限，防止把陌生人认成已登记的人: {report:?}");
    // 标定出的阈值用于正式匹配时同样认对
    let probe = calibration_vector(0, 50, 0.3);
    assert_eq!(match_embedding(&probe, &templates, 1, report.threshold, report.margin).unwrap().id(), "a");

    // 缺人或缺条数
    assert!(calibrate(&templates[..1], &good).is_err(), "至少两人");
    assert!(calibrate(&templates, &good[..5]).is_err(), "每人至少三条");
    // b 的一条测试音其实更像 a：阈值会被抬到认不出本人，或直接认错 → 不通过
    let mut confusable = good.clone_like();
    confusable[1] = CalibrationSample { speaker_id: "b".into(), embedding: calibration_vector(0, 1, 0.2) };
    assert!(calibrate(&templates, &confusable).is_err());
}

trait CloneLike { fn clone_like(&self) -> Vec<CalibrationSample>; }
impl CloneLike for Vec<CalibrationSample> {
    fn clone_like(&self) -> Vec<CalibrationSample> {
        self.iter().map(|s| CalibrationSample { speaker_id: s.speaker_id.clone(), embedding: s.embedding.clone() }).collect()
    }
}

#[tokio::test]
async fn short_vs_overlong_duration_split_and_timeout_aborts_worker_immediately() {
    let resolver = Arc::new(Resolver {
        delay: Duration::from_secs(10),
        allowed: AtomicBool::new(true),
        calls: AtomicUsize::new(0),
        replies: Mutex::new(std::collections::VecDeque::from([Some("a".into())])),
    });
    let service = PersonContext::new(resolver.clone(), Store::seeded());
    let ctx = context();

    // <2s 返回 SkippedShortDuration；>10s 必须返回 Denied（绝不继承上一轮身份）
    assert_eq!(
        service
            .start(&ctx, &vec![200; 16000], 16000)
            .finish_outcome()
            .await,
        IdentityOutcome::SkippedShortDuration
    );
    assert_eq!(
        service
            .start(&ctx, &vec![200; 16000 * 11], 16000)
            .finish_outcome()
            .await,
        IdentityOutcome::Denied
    );
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);

    // finish_outcome 超时（1.5s < 10s）后 Drop 必须立即 abort 后台任务并释放并发槽位
    let outcome = service
        .start(&ctx, &vec![200; 32000], 16000)
        .finish_outcome()
        .await;
    assert_eq!(outcome, IdentityOutcome::Resolved(None));
    tokio::time::timeout(Duration::from_millis(500), async {
        while service.identify_workers.available_permits() < 2
            || service.state.lock().unwrap().identifying.contains(&ctx.device_id)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timeout in finish_outcome must abort spawned task and release permits promptly");
}
