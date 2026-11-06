//! NV19-01 boundary matrix and source contract. Synthetic ids only; no storage or network.
use super::*;

const NOW: f64 = 1_790_000_000.0;

fn device(id: &str) -> TrustedScope {
    TrustedScope::device_grant(id, 3).unwrap()
}

fn person(id: &str) -> TrustedScope {
    TrustedScope::calibrated_person(&PersonScope::candidate(id, 2, 7).unwrap()).unwrap()
}

fn project(id: &str) -> TrustedScope {
    TrustedScope::desktop_selection(ScopeKind::Project, id, 1).unwrap()
}

fn key(kind: ScopeKind, id: &str) -> ScopeKey {
    ScopeKey::new(kind, id).unwrap()
}

fn draft(scope: ScopeKey, category: Category, source: Source, verification: Verification) -> MemoryDraft {
    let applicability = category.needs_applicability().then(|| Applicability::new("MOSS-550W", Some("1.2.0")).unwrap());
    MemoryDraft { scope, category, content: "喜欢先听到预计时长".into(), verification, source, applicability, expires_at: None }
}

fn extraction() -> Source {
    Source::new(SourceKind::ConversationExtraction, Some("extract:run-1")).unwrap()
}

fn user() -> Source {
    Source::new(SourceKind::UserEdit, None).unwrap()
}

fn record(scope: ScopeKey, category: Category, verification: Verification) -> ContextMemory {
    let draft = draft(scope, category, user(), verification);
    ContextMemory {
        id: "m1".into(),
        scope: draft.scope,
        category,
        content: draft.content,
        verification,
        source: draft.source,
        applicability: draft.applicability,
        revision: 1,
        created_at: NOW - 10.0,
        updated_at: NOW - 10.0,
        expires_at: None,
        deleted_at: None,
        supersedes: None,
        seen_count: 1,
        last_seen_at: None,
    }
}

#[test]
fn nv19_names_round_trip_and_unknown_values_are_rejected() {
    for category in Category::ALL {
        assert_eq!(Category::parse(category.as_str()), Some(category));
    }
    for kind in ScopeKind::ALL {
        assert_eq!(ScopeKind::parse(kind.as_str()), Some(kind));
    }
    for source in SourceKind::ALL {
        assert_eq!(SourceKind::parse(source.as_str()), Some(source));
    }
    for state in [Verification::Candidate, Verification::Confirmed, Verification::Invalidated] {
        assert_eq!(Verification::parse(state.as_str()), Some(state));
    }
    assert_eq!(Category::parse("skill"), None);
    assert_eq!(ScopeKind::parse("global"), None);
    assert_eq!(SourceKind::parse("model_said_so"), None);
    assert_eq!(Verification::parse("verified"), None);
}

#[test]
fn nv19_scope_ids_and_trusted_constructors_reject_forged_or_empty_values() {
    assert!(ScopeKey::new(ScopeKind::Device, "aa:bb:cc:dd:ee:ff").is_ok());
    assert!(ScopeKey::new(ScopeKind::Project, "aa:bb").is_err(), "冒号只允许出现在设备号");
    for bad in ["", "../x", "a b", "a/b", "名字", &"x".repeat(65)] {
        assert_eq!(ScopeKey::new(ScopeKind::Workspace, bad), Err(ContextError::ScopeInvalid), "{bad}");
    }
    assert!(ScopeKey::parse("everyone", "x").is_err());
    assert!(TrustedScope::device_grant("dev-a", 0).is_err(), "未授权设备没有授权代次");
    assert!(TrustedScope::desktop_selection(ScopeKind::Device, "dev-a", 1).is_err(), "桌面选择只能产生项目或工作区");
    assert!(TrustedScope::desktop_selection(ScopeKind::Project, "p1", 0).is_err());
    assert_eq!(person("alice").origin(), ScopeOrigin::CalibratedSpeaker);
    assert_eq!(device("dev-a").generation(), 3);
}

#[test]
fn nv19_voice_context_slots_must_match_their_trusted_origin() {
    assert!(AccessContext::voice(project("p1"), None, None).is_err(), "项目不能冒充设备");
    assert!(AccessContext::voice(device("dev-a"), Some(device("dev-b")), None).is_err(), "设备不能冒充个人");
    assert!(AccessContext::voice(device("dev-a"), None, Some(person("alice"))).is_err(), "个人不能冒充桌面选择");
    assert!(AccessContext::voice(device("dev-a"), Some(person("alice")), Some(project("p1"))).is_ok());
}

#[test]
fn nv19_read_matrix_is_exact_with_no_cross_scope_or_fallback() {
    let a = AccessContext::voice(device("dev-a"), None, None).unwrap();
    let a_alice = AccessContext::voice(device("dev-a"), Some(person("alice")), Some(project("p1"))).unwrap();
    let cases = [
        (key(ScopeKind::Device, "dev-a"), true, true),
        (key(ScopeKind::Device, "dev-b"), false, false),
        (key(ScopeKind::Person, "alice"), false, true),
        (key(ScopeKind::Person, "bob"), false, false),
        (key(ScopeKind::Project, "p1"), false, true),
        (key(ScopeKind::Project, "p2"), false, false),
        // Same id, different kind: a workspace never stands in for a project.
        (key(ScopeKind::Workspace, "p1"), false, false),
    ];
    for (scope, device_only, with_person) in cases {
        assert_eq!(a.can_read(&scope), device_only, "仅设备：{scope:?}");
        assert_eq!(a_alice.can_read(&scope), with_person, "设备+个人+项目：{scope:?}");
    }
    // Unknown speaker: person records are unreachable, not replaced by someone else's.
    let unknown = AccessContext::voice(device("dev-a"), None, None).unwrap();
    assert!(!record(key(ScopeKind::Person, "alice"), Category::Preference, Verification::Confirmed).readable(&unknown, NOW));
}

#[test]
fn nv19_storage_matrix_binds_categories_to_scopes() {
    let expected = [
        (Category::Preference, vec![ScopeKind::Device, ScopeKind::Person]),
        (Category::DeviceFact, vec![ScopeKind::Device]),
        (Category::TechnicalNote, vec![ScopeKind::Project, ScopeKind::Workspace]),
        (Category::TaskContext, vec![ScopeKind::Project, ScopeKind::Workspace]),
        (Category::TroubleshootingExperience, vec![ScopeKind::Device, ScopeKind::Workspace]),
    ];
    let owner = AccessContext::desktop_private(vec![
        device("dev-a"),
        person("alice"),
        project("p1"),
        TrustedScope::desktop_selection(ScopeKind::Workspace, "w1", 1).unwrap(),
    ]);
    let ids = [(ScopeKind::Device, "dev-a"), (ScopeKind::Person, "alice"), (ScopeKind::Project, "p1"), (ScopeKind::Workspace, "w1")];
    for (category, allowed) in expected {
        assert_eq!(category.allowed_scopes(), allowed.as_slice());
        for (kind, id) in ids {
            let result = draft(key(kind, id), category, user(), Verification::Candidate).validate(&owner, NOW);
            if allowed.contains(&kind) {
                assert_eq!(result, Ok(()), "{category:?} 应可存于 {kind:?}");
            } else {
                assert_eq!(result, Err(ContextError::CategoryNotInScope), "{category:?} 不应存于 {kind:?}");
            }
        }
    }
}

#[test]
fn nv19_source_contract_limits_channel_categories_and_initial_state() {
    let voice = AccessContext::voice(device("dev-a"), None, Some(project("p1"))).unwrap();
    let owner = AccessContext::desktop_private(vec![device("dev-a"), project("p1")]);
    let dev = key(ScopeKind::Device, "dev-a");
    let p1 = key(ScopeKind::Project, "p1");

    // Extraction: voice channel only, candidate only, never device facts or documents.
    assert_eq!(draft(dev.clone(), Category::Preference, extraction(), Verification::Candidate).validate(&voice, NOW), Ok(()));
    assert_eq!(draft(p1.clone(), Category::TaskContext, extraction(), Verification::Candidate).validate(&voice, NOW), Ok(()));
    assert_eq!(draft(dev.clone(), Category::Preference, extraction(), Verification::Confirmed).validate(&voice, NOW), Err(ContextError::VerificationNotAllowed));
    assert_eq!(draft(dev.clone(), Category::DeviceFact, extraction(), Verification::Candidate).validate(&voice, NOW), Err(ContextError::SourceNotAllowed));
    assert_eq!(draft(dev.clone(), Category::Preference, extraction(), Verification::Candidate).validate(&owner, NOW), Err(ContextError::AccessDenied));
    // The voice channel cannot pretend to be the user editing in the private window.
    assert_eq!(draft(dev.clone(), Category::Preference, user(), Verification::Confirmed).validate(&voice, NOW), Err(ContextError::AccessDenied));

    let legacy = Source::new(SourceKind::LegacyNv10Import, Some("nv10:aa-bb")).unwrap();
    assert_eq!(draft(dev.clone(), Category::Preference, legacy.clone(), Verification::Candidate).validate(&owner, NOW), Ok(()));
    assert_eq!(draft(dev.clone(), Category::Preference, legacy, Verification::Confirmed).validate(&owner, NOW), Err(ContextError::VerificationNotAllowed), "旧摘要导入只生成候选");

    let doc = Source::new(SourceKind::DocumentIndex, Some("doc:sha256-ab12")).unwrap();
    assert_eq!(draft(p1.clone(), Category::TechnicalNote, doc.clone(), Verification::Candidate).validate(&owner, NOW), Ok(()));
    assert_eq!(draft(p1.clone(), Category::TaskContext, doc, Verification::Candidate).validate(&owner, NOW), Err(ContextError::SourceNotAllowed));

    let test = Source::new(SourceKind::TrustedTest, Some("test:run-42")).unwrap();
    assert_eq!(draft(dev.clone(), Category::TroubleshootingExperience, test, Verification::Confirmed).validate(&owner, NOW), Ok(()));
    let agent = Source::new(SourceKind::AgentRunRecord, Some("agent_run:17")).unwrap();
    assert_eq!(draft(p1, Category::TaskContext, agent.clone(), Verification::Confirmed).validate(&owner, NOW), Err(ContextError::VerificationNotAllowed));
    assert_eq!(draft(dev, Category::Preference, agent, Verification::Candidate).validate(&owner, NOW), Err(ContextError::SourceNotAllowed));
}

#[test]
fn nv19_source_references_are_internal_tokens_only() {
    assert!(Source::new(SourceKind::UserEdit, None).is_ok());
    assert_eq!(Source::new(SourceKind::UserEdit, Some("doc:x")), Err(ContextError::SourceRefInvalid));
    assert_eq!(Source::new(SourceKind::DocumentIndex, None), Err(ContextError::SourceRefInvalid));
    for bad in ["test:run-1", "doc:", "doc:C:\\Users\\me\\a.pdf", "doc:/etc/passwd", "doc:https://x.test/?token=1", "doc:sk-abcdef", "doc:a b", &format!("doc:{}", "a".repeat(97))] {
        assert_eq!(Source::new(SourceKind::DocumentIndex, Some(bad)), Err(ContextError::SourceRefInvalid), "{bad}");
    }
    assert_eq!(Source::new(SourceKind::DocumentIndex, Some("doc:sha256.ab_12-x")).unwrap().reference(), Some("doc:sha256.ab_12-x"));
}

#[test]
fn nv19_content_rejects_secrets_machine_data_and_oversize() {
    let owner = AccessContext::desktop_private(vec![device("dev-a")]);
    let mut item = draft(key(ScopeKind::Device, "dev-a"), Category::Preference, user(), Verification::Candidate);
    for (content, error) in [
        ("   ", ContextError::ContentInvalid),
        ("a\u{0007}b", ContextError::ContentInvalid),
        ("我的 api_key 是 x", ContextError::ContentSensitive),
        ("审批记录：已批准", ContextError::ContentSensitive),
        ("先走一遍\nG1 X10 Y5 F1200", ContextError::ContentSensitive),
        ("$30=1000", ContextError::ContentSensitive),
        ("先试一下再发送 G1 X10 Y5 F1200 看看", ContextError::ContentSensitive),
        ("改成$30=1000就好", ContextError::ContentSensitive),
        ("激光开 M3 S800 再走边框", ContextError::ContentSensitive),
    ] {
        item.content = content.into();
        assert_eq!(item.validate(&owner, NOW), Err(error), "{content:?}");
    }
    item.content = "字".repeat(MAX_CONTENT_CHARS + 1);
    assert_eq!(item.validate(&owner, NOW), Err(ContextError::ContentInvalid));
    item.content = "字".repeat(MAX_CONTENT_CHARS);
    assert_eq!(item.validate(&owner, NOW), Ok(()));
    for plain in ["喜欢 G 系列命名\n用户说 $ 符号表示价格", "M3 螺丝拧紧后再试", "用 G2 镜头拍 X 光片", "价格 $30 左右"] {
        item.content = plain.into();
        assert_eq!(item.validate(&owner, NOW), Ok(()), "普通文字不应误判为机器数据：{plain}");
    }
}

#[test]
fn nv19_expiry_and_applicability_rules() {
    let owner = AccessContext::desktop_private(vec![device("dev-a")]);
    let mut fact = draft(key(ScopeKind::Device, "dev-a"), Category::DeviceFact, user(), Verification::Confirmed);
    fact.applicability = None;
    assert_eq!(fact.validate(&owner, NOW), Err(ContextError::ApplicabilityRequired));
    fact.applicability = Some(Applicability::new("MOSS-550W", None).unwrap());
    fact.expires_at = Some(NOW);
    assert_eq!(fact.validate(&owner, NOW), Err(ContextError::ExpiryInvalid));
    fact.expires_at = Some(f64::NAN);
    assert_eq!(fact.validate(&owner, NOW), Err(ContextError::ExpiryInvalid));
    fact.expires_at = Some(NOW + 1.0);
    assert_eq!(fact.validate(&owner, NOW), Ok(()));
    assert!(Applicability::new(" ", None).is_err());
    assert!(Applicability::new("m", Some("")).is_err());

    let pinned = Applicability::new("MOSS-550W", Some("1.2.0")).unwrap();
    assert!(pinned.matches("moss-550w", Some(" 1.2.0 ")));
    assert!(!pinned.matches("MOSS-550W", Some("1.3.0")), "过期固件不套用");
    assert!(!pinned.matches("MOSS-550W", None), "当前固件未知时不套用");
    assert!(!pinned.matches("OTHER", Some("1.2.0")), "错机型不套用");
    assert!(Applicability::new("MOSS-550W", None).unwrap().matches("MOSS-550W", None));
}

#[test]
fn nv19_verification_transitions_need_evidence_and_invalidated_is_terminal() {
    use Verification::*;
    let test = Evidence::TrustedTest(Source::new(SourceKind::TrustedTest, Some("test:run-1")).unwrap());
    let fake = Evidence::TrustedTest(Source::new(SourceKind::UserEdit, None).unwrap());
    assert_eq!(transition(Candidate, Confirmed, None), Err(ContextError::TransitionInvalid), "模型同意不是证据");
    assert_eq!(transition(Candidate, Confirmed, Some(&Evidence::UserConfirmed)), Ok(()));
    assert_eq!(transition(Candidate, Confirmed, Some(&test)), Ok(()));
    assert_eq!(transition(Candidate, Confirmed, Some(&fake)), Err(ContextError::TransitionInvalid));
    assert_eq!(transition(Candidate, Invalidated, None), Ok(()));
    assert_eq!(transition(Confirmed, Invalidated, None), Ok(()));
    for to in [Candidate, Confirmed, Invalidated] {
        assert_eq!(transition(Invalidated, to, Some(&Evidence::UserConfirmed)), Err(ContextError::TransitionInvalid));
    }
    assert_eq!(transition(Confirmed, Candidate, None), Err(ContextError::TransitionInvalid));
    assert_eq!(transition(Candidate, Candidate, None), Err(ContextError::TransitionInvalid));
}

#[test]
fn nv19_record_state_and_fact_usage() {
    let access = AccessContext::voice(device("dev-a"), None, None).unwrap();
    let dev = key(ScopeKind::Device, "dev-a");
    let current = Some(("MOSS-550W", Some("1.2.0")));

    let confirmed = record(dev.clone(), Category::TroubleshootingExperience, Verification::Confirmed);
    assert_eq!(confirmed.state(NOW), RecordState::Active(Verification::Confirmed));
    assert!(confirmed.usable_as_fact(&access, NOW, current));
    assert!(!confirmed.usable_as_fact(&access, NOW, Some(("MOSS-550W", Some("2.0.0")))), "固件不符");
    assert!(!confirmed.usable_as_fact(&access, NOW, None), "未知设备不套用硬件经验");

    let candidate = record(dev.clone(), Category::TroubleshootingExperience, Verification::Candidate);
    assert!(candidate.readable(&access, NOW), "候选可读，但须由调用方标为未确认");
    assert!(!candidate.usable_as_fact(&access, NOW, current), "候选不能冒充已验证");

    let mut expired = confirmed.clone();
    expired.expires_at = Some(NOW);
    assert_eq!(expired.state(NOW), RecordState::Expired);
    assert!(!expired.readable(&access, NOW));

    let mut invalid = confirmed.clone();
    invalid.verification = Verification::Invalidated;
    assert_eq!(invalid.state(NOW), RecordState::Invalidated);

    let mut deleted = invalid;
    deleted.deleted_at = Some(NOW - 1.0);
    assert_eq!(deleted.state(NOW), RecordState::Deleted, "删除优先于其他状态");

    let other = record(key(ScopeKind::Device, "dev-b"), Category::Preference, Verification::Confirmed);
    assert!(!other.readable(&access, NOW) && !other.usable_as_fact(&access, NOW, current));
    let preference = record(dev, Category::Preference, Verification::Confirmed);
    assert!(preference.usable_as_fact(&access, NOW, None), "偏好不需要设备型号");
}

#[test]
fn nv19_error_codes_are_stable_and_unique() {
    use ContextError::*;
    let all = [ScopeInvalid, CategoryNotInScope, AccessDenied, SourceNotAllowed, VerificationNotAllowed, ContentInvalid, ContentSensitive, SourceRefInvalid, ApplicabilityRequired, ApplicabilityInvalid, ExpiryInvalid, TransitionInvalid];
    let codes: std::collections::HashSet<_> = all.iter().map(|error| error.code()).collect();
    assert_eq!(codes.len(), all.len());
    assert!(codes.iter().all(|code| code.starts_with("context_")));
}
