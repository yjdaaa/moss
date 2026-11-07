//! NV19-02 service rules over an in-memory fake store with the same conditional semantics
//! as SQLite (revision CAS, tombstones). Synthetic ids only.
use super::*;
use crate::context::fake_store::FakeStore;
use crate::context::{RecordState, ScopeOrigin};
use crate::person_context::PersonScope;
use std::sync::atomic::Ordering;

const NOW: f64 = 1_790_000_000.0;

fn setup() -> (Arc<FakeStore>, Arc<ContextService>) {
    let store = Arc::new(FakeStore::default());
    (store.clone(), ContextService::new(store))
}

fn device(id: &str) -> TrustedScope {
    TrustedScope::device_grant(id, 1).unwrap()
}

fn owner() -> AccessContext {
    AccessContext::desktop_private(vec![
        device("dev-a"),
        TrustedScope::desktop_selection(ScopeKind::Project, "p1", 1).unwrap(),
    ])
}

fn voice() -> AccessContext {
    AccessContext::voice(
        device("dev-a"),
        None,
        Some(TrustedScope::desktop_selection(ScopeKind::Project, "p1", 1).unwrap()),
    )
    .unwrap()
}

fn dev() -> ScopeKey {
    ScopeKey::new(ScopeKind::Device, "dev-a").unwrap()
}

fn draft(
    category: Category,
    kind: SourceKind,
    reference: Option<&str>,
    verification: Verification,
) -> MemoryDraft {
    let scope = if category.allowed_scopes().contains(&ScopeKind::Device) {
        dev()
    } else {
        ScopeKey::new(ScopeKind::Project, "p1").unwrap()
    };
    MemoryDraft {
        scope,
        category,
        content: "喜欢先听到预计时长".into(),
        verification,
        source: Source::new(kind, reference).unwrap(),
        applicability: category
            .needs_applicability()
            .then(|| Applicability::new("MOSS-550W", None).unwrap()),
        expires_at: None,
    }
}

fn user_pref() -> MemoryDraft {
    draft(
        Category::Preference,
        SourceKind::UserEdit,
        None,
        Verification::Candidate,
    )
}

#[test]
fn nv19_create_assigns_fresh_ids_and_rechecks_the_host_scope() {
    let (store, service) = setup();
    let a = service.create(&owner(), user_pref(), NOW).unwrap();
    let b = service.create(&owner(), user_pref(), NOW).unwrap();
    assert!(a.id.starts_with("ctx-") && a.id.len() == 36 && a.id != b.id);
    assert_eq!((a.revision, a.created_at, a.deleted_at), (1, NOW, None));

    let other = AccessContext::desktop_private(vec![device("dev-b")]);
    assert_eq!(
        service.create(&other, user_pref(), NOW).unwrap_err(),
        ServiceError::Invalid(ContextError::AccessDenied)
    );
    assert_eq!(
        service.list(&other, &dev(), NOW).unwrap_err(),
        ServiceError::Invalid(ContextError::AccessDenied)
    );

    // Device revoked / speaker re-enrolled / selection switched after the request began.
    store.stale.store(true, Ordering::SeqCst);
    assert_eq!(
        service.create(&owner(), user_pref(), NOW).unwrap_err(),
        ServiceError::ScopeStale
    );
    assert_eq!(
        service.list(&voice(), &dev(), NOW).unwrap_err(),
        ServiceError::ScopeStale
    );
    assert_eq!(
        service.list(&owner(), &dev(), NOW).unwrap_err().code(),
        "context_scope_stale"
    );
}

#[test]
fn nv19_voice_channel_creates_candidates_and_reads_only_active_records() {
    let (_, service) = setup();
    let extracted = service
        .create(
            &voice(),
            draft(
                Category::TaskContext,
                SourceKind::ConversationExtraction,
                Some("extract:run-1"),
                Verification::Candidate,
            ),
            NOW,
        )
        .unwrap();
    assert_eq!(extracted.verification, Verification::Candidate);
    assert_eq!(
        service
            .create(
                &voice(),
                draft(
                    Category::Preference,
                    SourceKind::ConversationExtraction,
                    Some("extract:run-1"),
                    Verification::Confirmed
                ),
                NOW
            )
            .unwrap_err(),
        ServiceError::Invalid(ContextError::VerificationNotAllowed)
    );

    let keep = service.create(&owner(), user_pref(), NOW).unwrap();
    let mut expiring = user_pref();
    expiring.expires_at = Some(NOW + 5.0);
    service.create(&owner(), expiring, NOW).unwrap();
    let dropped = service.create(&owner(), user_pref(), NOW).unwrap();
    service
        .set_verification(
            &owner(),
            &dropped.id,
            1,
            Verification::Invalidated,
            None,
            NOW,
        )
        .unwrap();

    let later = NOW + 10.0;
    let voice_ids: Vec<_> = service
        .list(&voice(), &dev(), later)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(voice_ids, vec![keep.id.clone()], "语音只看有效记录");
    let all = service.list(&owner(), &dev(), later).unwrap();
    assert_eq!(all.len(), 3, "私有窗口能看到过期与作废记录以便管理");
    assert!(all.iter().any(|r| r.state(later) == RecordState::Expired));

    // Management is private-window only.
    for result in [
        service
            .edit(&voice(), &keep.id, 1, "改", None, None, NOW)
            .map(|_| ()),
        service
            .set_verification(
                &voice(),
                &keep.id,
                1,
                Verification::Confirmed,
                Some(&Evidence::UserConfirmed),
                NOW,
            )
            .map(|_| ()),
        service.delete(&voice(), &keep.id, 1, NOW),
        service.clear(&voice(), &dev(), NOW).map(|_| ()),
        service
            .import_legacy(&voice(), "dev-a", "喜欢简短", 1, NOW)
            .map(|_| ()),
        service.export(&voice(), &dev(), NOW).map(|_| ()),
    ] {
        assert_eq!(
            result.unwrap_err(),
            ServiceError::Invalid(ContextError::AccessDenied)
        );
    }
}

#[test]
fn nv19_edit_and_verification_use_revision_checks_and_evidence() {
    let (_, service) = setup();
    let owner = owner();
    let item = service.create(&owner, user_pref(), NOW).unwrap();
    assert_eq!(
        service
            .edit(&owner, &item.id, 2, "改", None, None, NOW)
            .unwrap_err(),
        ServiceError::Conflict
    );
    assert_eq!(
        service
            .edit(&owner, &item.id, 1, "我的 api_key 是 x", None, None, NOW)
            .unwrap_err(),
        ServiceError::Invalid(ContextError::ContentSensitive)
    );
    assert_eq!(
        service
            .edit(&owner, &item.id, 1, "改", None, Some(NOW), NOW)
            .unwrap_err(),
        ServiceError::Invalid(ContextError::ExpiryInvalid)
    );
    let edited = service
        .edit(&owner, &item.id, 1, "喜欢详细回答", None, None, NOW + 1.0)
        .unwrap();
    assert_eq!(
        (
            edited.revision,
            edited.content.as_str(),
            edited.verification
        ),
        (2, "喜欢详细回答", Verification::Candidate)
    );
    assert_eq!(
        service
            .edit(&owner, "missing", 1, "x", None, None, NOW)
            .unwrap_err(),
        ServiceError::NotFound
    );

    assert_eq!(
        service
            .set_verification(&owner, &item.id, 2, Verification::Confirmed, None, NOW)
            .unwrap_err(),
        ServiceError::Invalid(ContextError::TransitionInvalid),
        "没有证据不能确认"
    );
    let confirmed = service
        .set_verification(
            &owner,
            &item.id,
            2,
            Verification::Confirmed,
            Some(&Evidence::UserConfirmed),
            NOW,
        )
        .unwrap();
    assert_eq!(
        (confirmed.verification, confirmed.content.as_str()),
        (Verification::Confirmed, "喜欢详细回答")
    );
    let invalid = service
        .set_verification(&owner, &item.id, 3, Verification::Invalidated, None, NOW)
        .unwrap();
    assert_eq!(
        service
            .edit(&owner, &item.id, invalid.revision, "复活", None, None, NOW)
            .unwrap_err(),
        ServiceError::Invalid(ContextError::TransitionInvalid)
    );

    let mut fact = draft(
        Category::DeviceFact,
        SourceKind::UserEdit,
        None,
        Verification::Confirmed,
    );
    fact.content = "主板为 ESP32".into();
    let fact = service.create(&owner, fact, NOW).unwrap();
    assert_eq!(
        service
            .edit(&owner, &fact.id, 1, "主板为 ESP32-S3", None, None, NOW)
            .unwrap_err(),
        ServiceError::Invalid(ContextError::ApplicabilityRequired)
    );
    let pinned = Applicability::new("MOSS-550W", Some("1.2.0")).unwrap();
    assert_eq!(
        service
            .edit(
                &owner,
                &fact.id,
                1,
                "主板为 ESP32-S3",
                Some(pinned.clone()),
                None,
                NOW
            )
            .unwrap()
            .applicability,
        Some(pinned)
    );
}

#[test]
fn nv19_deletion_and_clear_cannot_be_undone_by_late_work() {
    let (store, service) = setup();
    let owner = owner();
    let item = service.create(&owner, user_pref(), NOW).unwrap();
    service.delete(&owner, &item.id, 1, NOW).unwrap();
    assert_eq!(
        service.delete(&owner, &item.id, 1, NOW).unwrap_err(),
        ServiceError::NotFound
    );
    assert_eq!(
        service
            .edit(&owner, &item.id, 2, "复活", None, None, NOW)
            .unwrap_err(),
        ServiceError::NotFound
    );
    // A background task that read revision 1 before the delete cannot write it back.
    let late = MemoryPatch {
        content: "迟到".into(),
        verification: Verification::Candidate,
        applicability: None,
        expires_at: None,
    };
    assert!(store.update(&item.id, 1, &late, NOW).unwrap().is_none());
    assert!(service.list(&owner, &dev(), NOW).unwrap().is_empty());

    service.create(&owner, user_pref(), NOW).unwrap();
    service.create(&owner, user_pref(), NOW).unwrap();
    service
        .create(
            &owner,
            draft(
                Category::TaskContext,
                SourceKind::UserEdit,
                None,
                Verification::Candidate,
            ),
            NOW,
        )
        .unwrap();
    assert_eq!(service.clear(&owner, &dev(), NOW).unwrap(), 2);
    assert!(service.list(&owner, &dev(), NOW).unwrap().is_empty());
    assert_eq!(
        service
            .list(
                &owner,
                &ScopeKey::new(ScopeKind::Project, "p1").unwrap(),
                NOW
            )
            .unwrap()
            .len(),
        1,
        "只清空选中的范围"
    );
}

#[test]
fn nv19_legacy_import_creates_candidates_once_and_never_resurrects_deleted_rows() {
    let (_, service) = setup();
    let owner = owner();
    let line = "喜欢先听到预计时长再开始。".repeat(30);
    let summary = format!("{line}\n{line}\n{line}\n我的 api_key 是 sk-test");
    assert!(summary.chars().count() > MAX_CONTENT_CHARS);
    let report = service
        .import_legacy(&owner, "dev-a", &summary, 3, NOW)
        .unwrap();
    assert!(report.created.len() >= 2, "超长摘要按行切成多条");
    assert!(
        report
            .created
            .iter()
            .all(|r| r.verification == Verification::Candidate
                && r.category == Category::Preference
                && r.content.chars().count() <= MAX_CONTENT_CHARS)
    );
    assert!(report.created.iter().all(|r| {
        r.source.kind() == SourceKind::LegacyNv10Import
            && r.source
                .reference()
                .is_some_and(|s| s.starts_with("nv10:r3-"))
    }));
    assert!(
        report.skipped.contains(&"context_content_sensitive"),
        "敏感片段不导入"
    );

    let again = service
        .import_legacy(&owner, "dev-a", &summary, 3, NOW)
        .unwrap();
    assert!(again.created.is_empty() && again.skipped == vec!["already_imported"]);
    for row in &report.created {
        service.delete(&owner, &row.id, row.revision, NOW).unwrap();
    }
    let after_delete = service
        .import_legacy(&owner, "dev-a", &summary, 3, NOW)
        .unwrap();
    assert!(after_delete.created.is_empty(), "删掉的导入不会被重新导入");
    assert_eq!(
        service
            .import_legacy(&owner, "dev-a", "喜欢简短回答", 4, NOW)
            .unwrap()
            .created
            .len(),
        1,
        "NV10 新版本可再导入"
    );
    assert_eq!(
        service
            .import_legacy(&owner, "dev-a", "  ", 5, NOW)
            .unwrap()
            .skipped,
        vec!["empty_summary"]
    );
    assert_eq!(
        service
            .import_legacy(&owner, "dev-b", "喜欢简短", 1, NOW)
            .unwrap_err(),
        ServiceError::Invalid(ContextError::AccessDenied)
    );
}

#[test]
fn nv19_export_is_private_json_without_tombstones() {
    let (_, service) = setup();
    let owner = owner();
    let kept = service.create(&owner, user_pref(), NOW).unwrap();
    let gone = service.create(&owner, user_pref(), NOW).unwrap();
    service.delete(&owner, &gone.id, 1, NOW).unwrap();
    let exported = service.export(&owner, &dev(), NOW).unwrap();
    let rows = exported.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], kept.id.as_str());
    assert_eq!(rows[0]["scope_kind"], "device");
    assert_eq!(rows[0]["category"], "preference");
    assert_eq!(rows[0]["verification"], "candidate");
}

#[test]
fn nv19_person_scope_only_from_calibrated_speaker() {
    let (_, service) = setup();
    let person =
        TrustedScope::calibrated_person(&PersonScope::candidate("alice", 1, 1).unwrap()).unwrap();
    assert_eq!(person.origin(), ScopeOrigin::CalibratedSpeaker);
    let with_person = AccessContext::voice(device("dev-a"), Some(person), None).unwrap();
    let mut pref = draft(
        Category::Preference,
        SourceKind::ConversationExtraction,
        Some("extract:r"),
        Verification::Candidate,
    );
    pref.scope = ScopeKey::new(ScopeKind::Person, "alice").unwrap();
    assert!(service.create(&with_person, pref.clone(), NOW).is_ok());
    assert_eq!(
        service.create(&voice(), pref, NOW).unwrap_err(),
        ServiceError::Invalid(ContextError::AccessDenied),
        "未识别说话人时不能写个人记忆"
    );
}

#[test]
fn nv19_service_error_codes() {
    assert_eq!(ServiceError::Conflict.code(), "context_conflict");
    assert_eq!(ServiceError::NotFound.code(), "context_not_found");
    assert_eq!(ServiceError::ScopeStale.code(), "context_scope_stale");
    assert_eq!(
        ServiceError::Store("x".into()).code(),
        "context_store_failed"
    );
    assert_eq!(
        ServiceError::Invalid(ContextError::AccessDenied).code(),
        "context_access_denied"
    );
    assert_eq!(chunks("a\n\n b \nc"), vec!["a\nb\nc".to_string()]);
    assert_eq!(
        chunks(&"字".repeat(MAX_CONTENT_CHARS + 5)).len(),
        1,
        "单行超长时截断到上限"
    );
}
