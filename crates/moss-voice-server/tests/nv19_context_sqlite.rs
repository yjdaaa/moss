//! NV19-02 end to end over real SQLite (schema v10): a reference `ContextStore` adapter that
//! the desktop host will mirror, driven through `ContextService`. In-memory database only.
use laser_runtime::{ContextExtractionRunRow, ContextMemoryPatch, ContextMemoryRow, RuntimeDb};
use moss_voice_server::context::retrieve::SearchQuery;
use moss_voice_server::context::service::{ContextService, ContextStore, ExtractionRun, MemoryPatch, RunAdd, RunSkip, ServiceError};
use moss_voice_server::context::{
    AccessContext, Applicability, Category, ContextError, ContextMemory, Evidence, MemoryDraft, ScopeKey, ScopeKind, Source, SourceKind,
    StoredMemory, TrustedScope, Verification,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const NOW: f64 = 1_790_000_000.0;

struct DbStore {
    db: Mutex<RuntimeDb>,
    revoked: AtomicBool,
}

fn decode(row: ContextMemoryRow) -> Result<ContextMemory, String> {
    ContextMemory::from_stored(StoredMemory {
        id: &row.id,
        scope_kind: &row.scope_kind,
        scope_id: &row.scope_id,
        category: &row.category,
        content: &row.content,
        verification: &row.verification,
        source_kind: &row.source_kind,
        source_ref: row.source_ref.as_deref(),
        device_model: row.device_model.as_deref(),
        firmware: row.firmware.as_deref(),
        revision: row.revision,
        created_at: row.created_at,
        updated_at: row.updated_at,
        expires_at: row.expires_at,
        deleted_at: row.deleted_at,
    })
    .map_err(|e| e.code().to_owned())
}

fn err(e: laser_runtime::SchemaError) -> String {
    format!("{e:?}")
}

impl ContextStore for DbStore {
    fn current(&self, scope: &TrustedScope) -> bool {
        !(self.revoked.load(Ordering::SeqCst) && scope.key().kind() == ScopeKind::Device)
    }
    fn insert(&self, m: &ContextMemory) -> Result<ContextMemory, String> {
        let row = ContextMemoryRow {
            id: m.id.clone(),
            scope_kind: m.scope.kind().as_str().into(),
            scope_id: m.scope.id().into(),
            category: m.category.as_str().into(),
            content: m.content.clone(),
            verification: m.verification.as_str().into(),
            source_kind: m.source.kind().as_str().into(),
            source_ref: m.source.reference().map(Into::into),
            device_model: m.applicability.as_ref().map(|a| a.device_model().into()),
            firmware: m.applicability.as_ref().and_then(|a| a.firmware()).map(Into::into),
            revision: m.revision,
            created_at: m.created_at,
            updated_at: m.updated_at,
            expires_at: m.expires_at,
            deleted_at: m.deleted_at,
        };
        decode(self.db.lock().unwrap().insert_context_memory(&row).map_err(err)?)
    }
    fn get(&self, id: &str) -> Result<Option<ContextMemory>, String> {
        self.db.lock().unwrap().context_memory(id).map_err(err)?.map(decode).transpose()
    }
    fn list(&self, scope: &ScopeKey, include_deleted: bool) -> Result<Vec<ContextMemory>, String> {
        self.db.lock().unwrap().list_context_memories(scope.kind().as_str(), scope.id(), include_deleted, 500).map_err(err)?.into_iter().map(decode).collect()
    }
    fn source_seen(&self, scope: &ScopeKey, source_ref: &str) -> Result<bool, String> {
        self.db.lock().unwrap().context_source_seen(scope.kind().as_str(), scope.id(), source_ref).map_err(err)
    }
    fn update(&self, id: &str, expected: i64, patch: &MemoryPatch, now: f64) -> Result<Option<ContextMemory>, String> {
        let row = ContextMemoryPatch {
            content: patch.content.clone(),
            verification: patch.verification.as_str().into(),
            device_model: patch.applicability.as_ref().map(|a| a.device_model().into()),
            firmware: patch.applicability.as_ref().and_then(|a| a.firmware()).map(Into::into),
            expires_at: patch.expires_at,
        };
        self.db.lock().unwrap().update_context_memory_if_revision(id, expected, &row, now).map_err(err)?.map(decode).transpose()
    }
    fn delete(&self, id: &str, expected: i64, now: f64) -> Result<bool, String> {
        self.db.lock().unwrap().delete_context_memory_if_revision(id, expected, now).map_err(err)
    }
    fn clear(&self, scope: &ScopeKey, now: f64) -> Result<usize, String> {
        self.db.lock().unwrap().clear_context_scope(scope.kind().as_str(), scope.id(), now).map_err(err)
    }
    fn insert_run(&self, run: &ExtractionRun) -> Result<(), String> {
        let row = ContextExtractionRunRow {
            id: run.id.clone(),
            scope_kind: run.scope.kind().as_str().into(),
            scope_id: run.scope.id().into(),
            started_at: run.started_at,
            finished_at: run.finished_at,
            adds_json: run.adds_json(),
            updates_json: "[]".into(),
            deletes_json: "[]".into(),
            skipped_json: run.skipped_json(),
            reverted_at: None,
        };
        self.db.lock().unwrap().insert_context_extraction_run(&row).map_err(err)
    }
    fn get_run(&self, id: &str) -> Result<Option<ExtractionRun>, String> {
        self.db.lock().unwrap().context_extraction_run(id).map_err(err)?.map(decode_run).transpose()
    }
    fn list_runs(&self, scope: &ScopeKey, limit: usize) -> Result<Vec<ExtractionRun>, String> {
        self.db.lock().unwrap().list_context_extraction_runs(scope.kind().as_str(), scope.id(), limit).map_err(err)?.into_iter().map(decode_run).collect()
    }
    fn mark_run_reverted(&self, id: &str, now: f64) -> Result<bool, String> {
        self.db.lock().unwrap().mark_context_extraction_reverted(id, now).map_err(err)
    }
    fn search(&self, scope: &ScopeKey, query: &SearchQuery, now: f64, limit: usize) -> Result<Vec<ContextMemory>, String> {
        // FTS5 for three-character windows, LIKE for two-character pieces; union by id.
        let db = self.db.lock().unwrap();
        let mut rows = Vec::new();
        if !query.terms.is_empty() {
            rows.extend(db.match_context_memories(scope.kind().as_str(), scope.id(), &query.terms, now, limit).map_err(err)?);
        }
        if !query.shorts.is_empty() {
            for row in db.like_context_memories(scope.kind().as_str(), scope.id(), &query.shorts, now, limit).map_err(err)? {
                if !rows.iter().any(|seen: &ContextMemoryRow| seen.id == row.id) {
                    rows.push(row);
                }
            }
        }
        rows.into_iter().map(decode).collect()
    }
}

fn decode_run(row: ContextExtractionRunRow) -> Result<ExtractionRun, String> {
    let scope = ScopeKey::parse(&row.scope_kind, &row.scope_id).map_err(|e| e.code().to_owned())?;
    ExtractionRun::from_json(&row.id, scope, row.started_at, row.finished_at, &row.adds_json, &row.updates_json, &row.deletes_json, &row.skipped_json, row.reverted_at)
}

fn setup() -> (Arc<DbStore>, Arc<ContextService>) {
    let store = Arc::new(DbStore { db: Mutex::new(RuntimeDb::open_memory().unwrap()), revoked: AtomicBool::new(false) });
    (store.clone(), ContextService::new(store))
}

fn owner() -> AccessContext {
    AccessContext::desktop_private(vec![
        TrustedScope::device_grant("aa:bb:cc:dd:ee:ff", 1).unwrap(),
        TrustedScope::desktop_selection(ScopeKind::Workspace, "w1", 1).unwrap(),
        TrustedScope::desktop_selection(ScopeKind::Project, "p1", 1).unwrap(),
    ])
}

fn device_key() -> ScopeKey {
    ScopeKey::new(ScopeKind::Device, "aa:bb:cc:dd:ee:ff").unwrap()
}

#[test]
fn nv19_every_category_and_source_name_round_trips_through_schema_v10() {
    let (_, service) = setup();
    let owner = owner();
    let sources = [
        (SourceKind::UserEdit, None),
        (SourceKind::LegacyNv10Import, Some("nv10:r1-abc")),
        (SourceKind::DocumentIndex, Some("doc:sha256-1")),
        (SourceKind::TrustedTest, Some("test:run-1")),
        (SourceKind::AgentRunRecord, Some("agent_run:7")),
    ];
    let mut created = 0;
    for category in Category::ALL {
        for (kind, reference) in sources {
            if !kind.allowed_categories().contains(&category) {
                continue;
            }
            let scope_kind = category.allowed_scopes()[0];
            let scope = match scope_kind {
                ScopeKind::Device => device_key(),
                other => ScopeKey::new(other, if other == ScopeKind::Project { "p1" } else { "w1" }).unwrap(),
            };
            let draft = MemoryDraft {
                scope,
                category,
                content: "合成测试内容".into(),
                verification: if kind.may_create_confirmed() { Verification::Confirmed } else { Verification::Candidate },
                source: Source::new(kind, reference).unwrap(),
                applicability: category.needs_applicability().then(|| Applicability::new("MOSS-550W", Some("1.2.0")).unwrap()),
                expires_at: Some(NOW + 60.0),
            };
            let row = service.create(&owner, draft.clone(), NOW).unwrap();
            assert_eq!((row.category, row.verification, row.source.clone(), row.applicability.clone()), (category, draft.verification, draft.source, draft.applicability));
            created += 1;
        }
    }
    assert!(created >= 8, "每种来源允许的类别都能写入并读回");
}

#[test]
fn nv19_service_over_sqlite_keeps_revisions_tombstones_and_scope_checks() {
    let (store, service) = setup();
    let owner = owner();
    let draft = MemoryDraft {
        scope: device_key(),
        category: Category::Preference,
        content: "喜欢先听到预计时长".into(),
        verification: Verification::Candidate,
        source: Source::new(SourceKind::UserEdit, None).unwrap(),
        applicability: None,
        expires_at: None,
    };
    let item = service.create(&owner, draft.clone(), NOW).unwrap();
    let edited = service.edit(&owner, &item.id, 1, "喜欢详细回答", None, None, NOW + 1.0).unwrap();
    assert_eq!(edited.revision, 2);
    assert_eq!(service.edit(&owner, &item.id, 1, "迟到", None, None, NOW + 2.0).unwrap_err(), ServiceError::Conflict);
    let confirmed = service.set_verification(&owner, &item.id, 2, Verification::Confirmed, Some(&Evidence::UserConfirmed), NOW + 3.0).unwrap();
    assert_eq!(confirmed.verification, Verification::Confirmed);

    service.delete(&owner, &item.id, confirmed.revision, NOW + 4.0).unwrap();
    let late = MemoryPatch { content: "复活".into(), verification: Verification::Confirmed, applicability: None, expires_at: None };
    assert!(store.update(&item.id, confirmed.revision, &late, NOW + 5.0).unwrap().is_none(), "删除后旧版本的后台写入失败");
    let tomb = store.get(&item.id).unwrap().unwrap();
    assert!(tomb.content.is_empty() && tomb.deleted_at.is_some());
    assert!(service.list(&owner, &device_key(), NOW).unwrap().is_empty());

    // Legacy import is idempotent across deletion on the real store.
    let report = service.import_legacy(&owner, "aa:bb:cc:dd:ee:ff", "喜欢简短回答\n周末不加工", 2, NOW).unwrap();
    assert_eq!(report.created.len(), 1);
    service.clear(&owner, &device_key(), NOW + 6.0).unwrap();
    assert_eq!(service.import_legacy(&owner, "aa:bb:cc:dd:ee:ff", "喜欢简短回答\n周末不加工", 2, NOW).unwrap().skipped, vec!["already_imported"]);

    // Device revoked between requests: the host check fails before the store is touched.
    store.revoked.store(true, Ordering::SeqCst);
    assert_eq!(service.create(&owner, draft, NOW).unwrap_err(), ServiceError::ScopeStale);
    let workspace = ScopeKey::new(ScopeKind::Workspace, "w1").unwrap();
    assert!(service.list(&owner, &workspace, NOW).is_ok(), "撤销设备不影响工作区范围");
    assert_eq!(
        service.list(&AccessContext::desktop_private(vec![]), &workspace, NOW).unwrap_err(),
        ServiceError::Invalid(ContextError::AccessDenied)
    );
}

#[test]
fn nv19_extraction_manifest_round_trips_and_reverts_once_over_sqlite() {
    let (store, service) = setup();
    let owner = owner();
    let voice = AccessContext::voice(TrustedScope::device_grant("aa:bb:cc:dd:ee:ff", 1).unwrap(), None, None).unwrap();
    let draft = MemoryDraft {
        scope: device_key(),
        category: Category::Preference,
        content: "回答先说预计时长".into(),
        verification: Verification::Candidate,
        source: Source::new(SourceKind::ConversationExtraction, Some("extract:run-abc")).unwrap(),
        applicability: None,
        expires_at: Some(NOW + 3600.0),
    };
    let row = service.create(&voice, draft, NOW).unwrap();
    let run = ExtractionRun {
        id: "run-abc".into(),
        scope: device_key(),
        started_at: NOW,
        finished_at: Some(NOW + 1.0),
        adds: vec![RunAdd { id: row.id.clone(), revision: 1 }],
        skipped: vec![RunSkip { op: "update".into(), reason: "op_not_allowed".into() }],
        reverted_at: None,
    };
    store.insert_run(&run).unwrap();
    assert_eq!(store.get_run("run-abc").unwrap().unwrap(), run, "清单经 SQLite 往返不变");

    let report = service.revert_extraction(&owner, "run-abc", NOW + 2.0).unwrap();
    assert_eq!((report.removed, report.kept.len()), (1, 0));
    assert!(store.get(&row.id).unwrap().unwrap().deleted_at.is_some());
    assert_eq!(service.revert_extraction(&owner, "run-abc", NOW + 3.0).unwrap_err(), ServiceError::Conflict);
}

/// NV19-04 fixed Chinese retrieval set over real FTS5 trigram. Expected answers are fixed in
/// advance; the synonym case documents a known fast-mode miss instead of hiding it.
#[test]
fn nv19_fixed_chinese_retrieval_set_over_fts5() {
    use moss_voice_server::context::retrieve::fast_query;
    let (_, service) = setup();
    let owner = AccessContext::desktop_private(vec![
        TrustedScope::device_grant("aa:bb:cc:dd:ee:ff", 1).unwrap(),
        TrustedScope::device_grant("11:22:33:44:55:66", 1).unwrap(),
        TrustedScope::desktop_selection(ScopeKind::Project, "p1", 1).unwrap(),
    ]);
    let m550 = || Some(Applicability::new("MOSS-550W", Some("1.2.0")).unwrap());
    let rows = [
        ("A", ScopeKind::Device, "aa:bb:cc:dd:ee:ff", Category::TroubleshootingExperience, "边缘发黑时把功率降到百分之六十后改善", Verification::Confirmed, m550(), None),
        ("B", ScopeKind::Device, "aa:bb:cc:dd:ee:ff", Category::TroubleshootingExperience, "雕刻线条断续是皮带松了，拧紧后恢复", Verification::Confirmed, m550(), None),
        ("C", ScopeKind::Device, "aa:bb:cc:dd:ee:ff", Category::Preference, "回答时先说预计时长再说步骤", Verification::Confirmed, None, None),
        ("D", ScopeKind::Device, "aa:bb:cc:dd:ee:ff", Category::Preference, "喜欢用椴木板做样品", Verification::Candidate, None, None),
        ("E", ScopeKind::Device, "aa:bb:cc:dd:ee:ff", Category::Preference, "排风扇噪音大时提醒我关窗", Verification::Confirmed, None, Some(NOW - 1.0)),
        ("F", ScopeKind::Device, "11:22:33:44:55:66", Category::Preference, "另一台机器：回答时先说预计时长", Verification::Confirmed, None, None),
        ("G", ScopeKind::Project, "p1", Category::TaskContext, "本周先完成椴木杯垫样品，周五交付", Verification::Confirmed, None, None),
        ("H", ScopeKind::Device, "aa:bb:cc:dd:ee:ff", Category::TroubleshootingExperience, "边缘发黑要更换镜片", Verification::Confirmed, Some(Applicability::new("MOSS-550W", Some("2.0.0")).unwrap()), None),
    ];
    let mut ids = std::collections::HashMap::new();
    for (name, kind, scope, category, content, verification, applicability, expires) in rows {
        let draft = MemoryDraft {
            scope: ScopeKey::new(kind, scope).unwrap(),
            category,
            content: content.into(),
            verification,
            source: Source::new(SourceKind::UserEdit, None).unwrap(),
            applicability,
            expires_at: expires,
        };
        // Expired rows are created valid, then the clock moves past them.
        let created_at = if expires.is_some() { NOW - 10.0 } else { NOW };
        ids.insert(service.create(&owner, draft, created_at).unwrap().id, name);
    }
    let device_a = AccessContext::voice(TrustedScope::device_grant("aa:bb:cc:dd:ee:ff", 1).unwrap(), None, Some(TrustedScope::desktop_selection(ScopeKind::Project, "p1", 1).unwrap())).unwrap();
    let current = Applicability::new("MOSS-550W", Some("1.2.0")).unwrap();
    // Top-1 must be one of the listed names; an empty list means nothing may be returned.
    let cases: [(&str, &[&str]); 10] = [
        ("上次边缘发黑是怎么解决的", &["A"]),
        ("线条断断续续怎么办", &["B"]),
        ("我喜欢回答先说什么", &["C"]),
        ("椴木", &["D", "G"]),
        ("这周的样品什么时候交付", &["G"]),
        ("排风扇噪音", &[]),
        ("边缘烧焦了怎么处理", &[]),
        ("你好啊今天天气不错", &[]),
        ("皮带", &["B"]),
        ("预计时长", &["C"]),
    ];
    let mut correct = 0;
    for (question, expected) in cases {
        let query = fast_query(question).unwrap();
        let hits = service.search(&device_a, &query, Some(&current), NOW).unwrap();
        let names: Vec<&str> = hits.iter().map(|(row, _)| ids[&row.id]).collect();
        assert!(!names.contains(&"F"), "{question}: 不得读到另一台设备");
        assert!(!names.contains(&"E"), "{question}: 过期记录不得返回");
        assert!(!names.contains(&"H"), "{question}: 固件不符的经验不得返回");
        match names.first() {
            Some(top) => assert!(expected.contains(top), "{question}: {names:?}"),
            None => assert!(expected.is_empty(), "{question}: 应命中 {expected:?}"),
        }
        correct += 1;
    }
    println!("NV19_FIXED_SET top1={correct}/{} (含“边缘烧焦”同义说法在快速模式下按预期查不到)", cases.len());
}

#[test]
fn nv19_retrieval_latency_over_500_rows_is_recorded() {
    use moss_voice_server::context::retrieve::fast_query;
    let (_, service) = setup();
    let owner = owner();
    let words = ["边缘", "功率", "皮带", "镜片", "椴木", "亚克力", "排风", "对焦", "速度", "样品"];
    for i in 0..500 {
        let content = format!("第{i}条：{}和{}的经验记录，编号{}", words[i % 10], words[(i * 7 + 3) % 10], i * 13);
        let draft = MemoryDraft {
            scope: device_key(),
            category: Category::Preference,
            content,
            verification: Verification::Confirmed,
            source: Source::new(SourceKind::UserEdit, None).unwrap(),
            applicability: None,
            expires_at: None,
        };
        service.create(&owner, draft, NOW).unwrap();
    }
    let voice = AccessContext::voice(TrustedScope::device_grant("aa:bb:cc:dd:ee:ff", 1).unwrap(), None, None).unwrap();
    let mut samples = Vec::new();
    for question in ["边缘和功率有什么经验", "皮带松了怎么办", "椴木样品", "亚克力对焦", "排风速度"].iter().cycle().take(100) {
        let query = fast_query(question).unwrap();
        let started = std::time::Instant::now();
        let _ = service.search(&voice, &query, None, NOW).unwrap();
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    // Recorded, not asserted: wall-clock limits would make the test depend on machine load.
    println!("NV19_LATENCY rows=500 p50={:.2}ms p95={:.2}ms", samples[49], samples[94]);
}

mod knowledge_sqlite {
    //! NV19-05 reference `KnowledgeStore` over schema v12, driven through `KnowledgeService`
    //! with a temporary folder.
    use super::*;
    use laser_runtime::{KnowledgeDirRow, KnowledgeFileRow, KnowledgeRootRow};
    use moss_voice_server::context::knowledge::{format_knowledge, KnowledgeDir, KnowledgeError, KnowledgeFile, KnowledgeRoot, KnowledgeService, KnowledgeStore};
    use moss_voice_server::context::retrieve::fast_query;
    use std::path::PathBuf;

    struct DbKnowledge(Mutex<RuntimeDb>);

    fn root_from(row: KnowledgeRootRow) -> Result<KnowledgeRoot, String> {
        Ok(KnowledgeRoot {
            scope: ScopeKey::parse(&row.scope_kind, &row.scope_id).map_err(|e| e.code().to_owned())?,
            id: row.id,
            label: row.label,
            root_path: PathBuf::from(row.root_path),
            revision: row.revision,
            indexed_at: row.indexed_at,
            enabled: row.enabled,
        })
    }
    fn dir_from(d: KnowledgeDirRow) -> KnowledgeDir {
        KnowledgeDir { id: d.id, root_id: d.root_id, rel_dir: d.rel_dir, l0: d.l0, l1: d.l1, file_count: d.file_count }
    }

    impl KnowledgeStore for DbKnowledge {
        fn current(&self, _scope: &TrustedScope) -> bool {
            true
        }
        fn insert_root(&self, root: &KnowledgeRoot, now: f64) -> Result<(), String> {
            let row = KnowledgeRootRow {
                id: root.id.clone(),
                scope_kind: root.scope.kind().as_str().into(),
                scope_id: root.scope.id().into(),
                label: root.label.clone(),
                root_path: root.root_path.to_str().ok_or("path")?.into(),
                revision: 1,
                created_at: now,
                indexed_at: None,
                enabled: root.enabled,
            };
            self.0.lock().unwrap().insert_knowledge_root(&row).map_err(err)
        }
        fn root(&self, id: &str) -> Result<Option<KnowledgeRoot>, String> {
            self.0.lock().unwrap().knowledge_root(id).map_err(err)?.map(root_from).transpose()
        }
        fn roots(&self, scope: &ScopeKey) -> Result<Vec<KnowledgeRoot>, String> {
            self.0.lock().unwrap().knowledge_roots(scope.kind().as_str(), scope.id()).map_err(err)?.into_iter().map(root_from).collect()
        }
        fn all_roots(&self) -> Result<Vec<KnowledgeRoot>, String> {
            self.0.lock().unwrap().all_knowledge_roots().map_err(err)?.into_iter().map(root_from).collect()
        }
        fn set_root_enabled(&self, id: &str, expected: i64, enabled: bool) -> Result<bool, String> {
            self.0.lock().unwrap().set_knowledge_root_enabled(id, expected, enabled).map_err(err)
        }
        fn replace(&self, root_id: &str, expected: i64, dirs: &[KnowledgeDir], files: &[KnowledgeFile], now: f64) -> Result<bool, String> {
            let dirs: Vec<_> = dirs.iter().map(|d| KnowledgeDirRow { id: d.id.clone(), root_id: d.root_id.clone(), rel_dir: d.rel_dir.clone(), l0: d.l0.clone(), l1: d.l1.clone(), file_count: d.file_count }).collect();
            let files: Vec<_> = files
                .iter()
                .map(|f| KnowledgeFileRow { id: f.id.clone(), root_id: f.root_id.clone(), dir_id: f.dir_id.clone(), rel_path: f.rel_path.clone(), sha256: f.sha256.clone(), size: f.size, mtime: f.mtime })
                .collect();
            self.0.lock().unwrap().replace_knowledge_index(root_id, expected, &dirs, &files, now).map_err(err)
        }
        fn delete_root(&self, id: &str, expected: i64) -> Result<bool, String> {
            self.0.lock().unwrap().delete_knowledge_root_if_revision(id, expected).map_err(err)
        }
        fn search_dirs(&self, scope: &ScopeKey, query: &SearchQuery, limit: usize) -> Result<Vec<KnowledgeDir>, String> {
            Ok(self.0.lock().unwrap().search_knowledge_dirs(scope.kind().as_str(), scope.id(), &query.terms, &query.shorts, limit).map_err(err)?.into_iter().map(dir_from).collect())
        }
        fn files(&self, dir_id: &str) -> Result<Vec<KnowledgeFile>, String> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .knowledge_files(dir_id)
                .map_err(err)?
                .into_iter()
                .map(|f| KnowledgeFile { id: f.id, root_id: f.root_id, dir_id: f.dir_id, rel_path: f.rel_path, sha256: f.sha256, size: f.size, mtime: f.mtime })
                .collect())
        }
    }

    #[test]
    fn nv19_knowledge_base_over_sqlite_indexes_searches_rebuilds_and_forgets() {
        let temp = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| {
            let path = temp.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("对焦.md", "# 激光对焦\n\n先把焦距规放在材料上，降低激光头直到刚好接触。\n");
        write("维护/皮带.md", "# 皮带张紧\n线条断续时检查皮带是否松动，拧紧张紧螺丝后恢复。\n");
        let service = KnowledgeService::new(Arc::new(DbKnowledge(Mutex::new(RuntimeDb::open_memory().unwrap()))));
        let project = ScopeKey::new(ScopeKind::Project, "p1").unwrap();
        let owner = AccessContext::desktop_private(vec![TrustedScope::desktop_selection(ScopeKind::Project, "p1", 1).unwrap()]);

        let (root, report) = service.add_root(&owner, &project, temp.path(), NOW).unwrap();
        assert_eq!((report.directories, report.files), (2, 2));
        assert_eq!(service.add_root(&owner, &project, temp.path(), NOW).unwrap_err().code(), "knowledge_store_failed", "同一目录不能重复登记");

        let hits = service.search(&owner, &fast_query("线条断续怎么办").unwrap()).unwrap();
        assert_eq!(hits[0].rel_dir, "维护");
        let block = format_knowledge(&hits).unwrap();
        assert!(block.contains("原文摘录") && block.contains("拧紧"));
        assert!(!block.contains(temp.path().to_str().unwrap()), "本机绝对路径不进提示词");

        std::fs::remove_file(temp.path().join("维护/皮带.md")).unwrap();
        service.reindex(&owner, &root.id, NOW + 1.0).unwrap();
        assert!(service.search(&owner, &fast_query("线条断续怎么办").unwrap()).unwrap().is_empty());
        assert_eq!(service.search(&owner, &fast_query("怎么降低激光头").unwrap()).unwrap().len(), 1);

        let current = service.roots(&owner, &project).unwrap().remove(0);
        assert_eq!(service.remove_root(&owner, &root.id, 1).unwrap_err(), KnowledgeError::Conflict);
        service.remove_root(&owner, &root.id, current.revision).unwrap();
        assert!(service.search(&owner, &fast_query("怎么降低激光头").unwrap()).unwrap().is_empty());
        assert!(service.roots(&owner, &project).unwrap().is_empty());
    }
}

mod resume_sqlite {
    //! NV19-07 reference `ResumeFacts` over the real approvals / jobs / voice_agent_runs tables.
    //! Read-only SELECTs; the test also proves resuming changes none of those rows.
    use super::*;
    use laser_runtime::AgentRunRecord;
    use moss_voice_server::context::resume::{format_resume, RefKind, RefState, Reference, ResumeFacts, ResumeNote};

    struct DbFacts<'a>(&'a RuntimeDb);

    impl ResumeFacts for DbFacts<'_> {
        fn state(&self, reference: &Reference) -> RefState {
            let row = |sql: &str| -> Option<(String, Option<String>)> {
                self.0.connection.query_row(sql, [&reference.id], |r| Ok((r.get(0)?, r.get(1)?))).ok()
            };
            let found = match reference.kind {
                RefKind::Approval => row("SELECT status, json_extract(outcome,'$.phase') FROM approvals WHERE approval_id=?1"),
                RefKind::Job => row("SELECT status, NULL FROM jobs WHERE job_id=?1"),
                RefKind::AgentRun => row("SELECT status, NULL FROM voice_agent_runs WHERE id=?1"),
                RefKind::Test => return RefState::Unverifiable,
            };
            let Some((status, phase)) = found else { return RefState::Missing };
            match (reference.kind, status.as_str(), phase.as_deref()) {
                (RefKind::Approval, "pending" | "approved", _) => RefState::InProgress,
                (RefKind::Approval, "expired" | "rejected" | "cancelled", _) => RefState::Revoked,
                (RefKind::Approval, "executed", Some("unknown")) => RefState::ResultUnknown,
                (RefKind::Approval, "executed", Some("failed")) => RefState::Failed,
                (RefKind::Approval, "executed", Some("cancelled")) => RefState::Stopped,
                (RefKind::Approval, "executed", _) => RefState::Done,
                (_, "completed", _) => RefState::Done,
                (_, "failed" | "timed_out", _) => RefState::Failed,
                (_, "cancelled", _) => RefState::Stopped,
                (_, "result_unknown", _) => RefState::ResultUnknown,
                (_, "running" | "pending" | "queued", _) => RefState::InProgress,
                _ => RefState::Unverifiable,
            }
        }
    }

    fn snapshot(db: &RuntimeDb) -> String {
        let mut out = String::new();
        for sql in ["SELECT approval_id||status||updated_at FROM approvals ORDER BY 1", "SELECT job_id||status FROM jobs ORDER BY 1", "SELECT id||status FROM voice_agent_runs ORDER BY 1"] {
            let mut statement = db.connection.prepare(sql).unwrap();
            for row in statement.query_map([], |r| r.get::<_, String>(0)).unwrap() {
                out.push_str(&row.unwrap());
                out.push('\n');
            }
        }
        out
    }

    #[test]
    fn nv19_resume_over_real_records_reports_without_replaying() {
        let facts_db = RuntimeDb::open_memory().unwrap();
        facts_db
            .connection
            .execute_batch(
                "INSERT INTO approvals(approval_id,status,kind,target_record,digest,summary,source,created_at,expires_at,updated_at) VALUES
                   ('apr-old','expired','framing','wf','d','走边框','voice',1,121,130),
                   ('apr-wait','pending','framing','wf','d','走边框','voice',1,9999999999,1);
                 INSERT INTO approvals(approval_id,status,kind,target_record,digest,summary,source,created_at,expires_at,updated_at,outcome) VALUES
                   ('apr-unknown','executed','engrave','wf','d','雕刻','voice',1,121,5,'{\"phase\":\"unknown\"}');
                 INSERT INTO jobs(job_id,status,created_at,payload) VALUES ('j-done','completed',1,'{}'),('j-stop','cancelled',1,'{}');",
            )
            .unwrap();
        let run = AgentRunRecord {
            id: "run-9".into(),
            approval_id: "apr-agent".into(),
            provider: "codex".into(),
            workspace: "w".into(),
            task_digest: "a".repeat(64),
            config_revision: "b".repeat(64),
            status: "running".into(),
            output: String::new(),
            truncated: false,
            started_at: 1.0,
            finished_at: None,
            error_code: None,
        };
        facts_db.insert_agent_run(&run).unwrap();
        facts_db.recover_agent_runs(2.0).unwrap();
        let before = snapshot(&facts_db);

        let (_, service) = setup();
        let project = ScopeKey::new(ScopeKind::Project, "p1").unwrap();
        let owner = AccessContext::desktop_private(vec![TrustedScope::desktop_selection(ScopeKind::Project, "p1", 1).unwrap()]);
        let note = ResumeNote {
            decisions: vec!["统一用 3 毫米椴木".into()],
            todos: vec!["等批准后走边框 [approval:apr-wait]".into(), "确认雕刻是否完成 [approval:apr-unknown]".into(), "看 Agent 输出 [agent_run:run-9]".into(), "继续中断的任务 [job:j-stop]".into()],
            evidence: vec!["样品完成 [job:j-done]".into(), "旧的走边框 [approval:apr-old]".into(), "不存在的任务 [job:j-none]".into()],
            unverified: vec![],
        };
        service.save_resume(&owner, &project, &note, None, NOW).unwrap();
        let view = service.resume(&owner, &project, &DbFacts(&facts_db), NOW).unwrap().unwrap();
        let state = |items: &[moss_voice_server::context::resume::CheckedItem], id: &str| items.iter().flat_map(|i| i.states.iter()).find(|(r, _)| r.id == id).unwrap().1;
        assert_eq!(state(&view.todos, "apr-wait"), RefState::InProgress);
        assert_eq!(state(&view.todos, "apr-unknown"), RefState::ResultUnknown);
        assert_eq!(state(&view.todos, "run-9"), RefState::ResultUnknown, "重启后中断的 Agent 运行是结果未知");
        assert_eq!(state(&view.todos, "j-stop"), RefState::Stopped);
        assert_eq!(view.evidence.len(), 1);
        assert_eq!(state(&view.unverified, "apr-old"), RefState::Revoked);
        assert_eq!(state(&view.unverified, "j-none"), RefState::Missing);
        assert!(format_resume(&view).contains("不会自动重试"));
        assert_eq!(snapshot(&facts_db), before, "恢复只读：审批、任务和 Agent 记录一行都没变");
    }
}
