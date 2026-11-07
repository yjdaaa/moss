//! NV19-08 desktop host over an in-memory database, a temporary settings file and a
//! temporary folder. Synthetic devices only; no window, network or model.
use super::*;

const DEVICE: &str = "aa:bb:cc:dd:ee:ff";

struct Fixture {
    host: Arc<ContextHost>,
    db: Arc<Mutex<RuntimeDb>>,
    settings: PathBuf,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
    {
        let db = db.lock().unwrap();
        db.voice_device_seen(DEVICE, "client-1", "MOSS-550W", "1.2.0", "192.0.2.10", 1.0)
            .unwrap();
        db.set_voice_device_status(DEVICE, laser_runtime::VOICE_DEVICE_ALLOWED, 2.0)
            .unwrap();
        db.voice_device_seen("11:22:33:44:55:66", "client-2", "", "", "192.0.2.11", 1.0)
            .unwrap();
        db.set_voice_device_status(
            "11:22:33:44:55:66",
            laser_runtime::VOICE_DEVICE_ALLOWED,
            2.0,
        )
        .unwrap();
    }
    let host = ContextHost::new(db.clone(), settings.clone());
    Fixture {
        host,
        db,
        settings,
        _dir: dir,
    }
}

fn call(f: &Fixture, input: Value) -> Result<Value, CenterError> {
    f.host.manage(&input, || None)
}

fn device_scope() -> Value {
    json!({"kind": "device", "id": DEVICE})
}

#[test]
fn nv19_everything_starts_off_and_extraction_needs_explicit_consent() {
    let f = fixture();
    let status = call(&f, json!({"action": "status"})).unwrap();
    assert_eq!(
        (
            status["retrieval"].as_bool(),
            status["extraction"].as_bool()
        ),
        (Some(false), Some(false))
    );
    assert!(status["project"].is_null());
    assert_eq!(status["devices"].as_array().unwrap().len(), 2);
    assert!(!f.host.retriever.enabled() && !f.host.extractor.enabled());

    assert_eq!(
        call(&f, json!({"action": "set_options", "extraction": true}))
            .unwrap_err()
            .code,
        "context_confirm_required"
    );
    assert!(
        !moss_voice::settings::load(&f.settings)
            .builtin_context
            .extraction,
        "未确认时什么都不写"
    );
    call(
        &f,
        json!({"action": "set_options", "retrieval": true, "extraction": true, "confirmed": true}),
    )
    .unwrap();
    assert!(f.host.retriever.enabled() && f.host.extractor.enabled());
    assert!(
        moss_voice::settings::load(&f.settings)
            .builtin_context
            .extraction
    );
    call(
        &f,
        json!({"action": "set_options", "retrieval": false, "extraction": false}),
    )
    .unwrap();
    assert!(
        !f.host.retriever.enabled() && !f.host.extractor.enabled(),
        "关闭不需要再次确认"
    );

    // A fresh host reads the saved switches.
    call(&f, json!({"action": "set_options", "retrieval": true})).unwrap();
    assert!(
        ContextHost::new(f.db.clone(), f.settings.clone())
            .retriever
            .enabled()
    );
    assert_eq!(
        call(&f, json!({"action": "no_such_action"}))
            .unwrap_err()
            .code,
        "context_invalid"
    );
}

#[test]
fn nv19_memory_crud_search_import_and_export_through_the_host() {
    let f = fixture();
    let created = call(&f, json!({"action": "create", "scope": device_scope(), "category": "preference", "content": "回答时先说预计时长", "confirmed": true})).unwrap();
    assert_eq!(created["verification"], "confirmed");
    let id = created["id"].as_str().unwrap().to_owned();
    let listed = call(&f, json!({"action": "list", "scope": device_scope()})).unwrap();
    assert_eq!(listed["rows"].as_array().unwrap().len(), 1);
    let found = call(
        &f,
        json!({"action": "list", "scope": device_scope(), "query": "预计时长"}),
    )
    .unwrap();
    assert_eq!(found["rows"][0]["id"], id.as_str(), "私有窗口按 FTS5 检索");
    assert_eq!(call(&f, json!({"action": "list", "scope": {"kind": "device", "id": "99:99:99:99:99:99"}, "query": "预计时长"})).unwrap_err().code, "context_access_denied");

    let edited = call(
        &f,
        json!({"action": "edit", "id": id, "revision": 1, "content": "回答时先说步骤"}),
    )
    .unwrap();
    assert_eq!(edited["revision"], 2);
    assert_eq!(
        call(
            &f,
            json!({"action": "edit", "id": id, "revision": 1, "content": "迟到"})
        )
        .unwrap_err()
        .code,
        "context_conflict"
    );
    assert_eq!(call(&f, json!({"action": "create", "scope": device_scope(), "category": "preference", "content": "发送 G1 X10 Y10 S1000"})).unwrap_err().code, "context_content_sensitive");

    let exported = call(&f, json!({"action": "export", "scope": device_scope()})).unwrap();
    assert_eq!(exported["memories"].as_array().unwrap().len(), 1);
    call(
        &f,
        json!({"action": "verify", "id": id, "revision": 2, "to": "invalidated"}),
    )
    .unwrap();
    assert_eq!(
        call(&f, json!({"action": "delete", "id": id, "revision": 3})).unwrap()["deleted"],
        id.as_str()
    );
    assert!(
        call(&f, json!({"action": "list", "scope": device_scope()})).unwrap()["rows"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // NV10 summary → candidates once; the NV10 summary itself is untouched.
    {
        let db = f.db.lock().unwrap();
        db.ensure_voice_memory(DEVICE, 1.0).unwrap();
        db.update_voice_memory_if_revision(DEVICE, 0, "喜欢简短回答\n周末不加工", 2.0)
            .unwrap();
    }
    let status = call(&f, json!({"action": "status"})).unwrap();
    assert_eq!(status["legacy_summaries"], json!([DEVICE]));
    assert_eq!(
        call(&f, json!({"action": "import_legacy", "device_id": DEVICE})).unwrap()["created"],
        1
    );
    assert_eq!(
        call(&f, json!({"action": "import_legacy", "device_id": DEVICE})).unwrap()["skipped"],
        json!(["already_imported"])
    );
    assert_eq!(
        f.db.lock()
            .unwrap()
            .voice_memory(DEVICE)
            .unwrap()
            .unwrap()
            .summary,
        "喜欢简短回答\n周末不加工"
    );

    assert_eq!(
        call(&f, json!({"action": "clear", "scope": device_scope()}))
            .unwrap_err()
            .code,
        "context_confirm_required"
    );
    assert_eq!(
        call(
            &f,
            json!({"action": "clear", "scope": device_scope(), "confirmed": true})
        )
        .unwrap()["cleared"],
        1
    );
}

#[test]
fn nv19_revoked_device_and_switched_project_make_old_scopes_stale() {
    let f = fixture();
    let project = call(&f, json!({"action": "set_project", "name": "椴木杯垫"})).unwrap();
    let project_id = project["project"]["id"].as_str().unwrap().to_owned();
    let project_scope = json!({"kind": "project", "id": project_id});
    call(&f, json!({"action": "create", "scope": project_scope, "category": "task_context", "content": "周五交付样品"})).unwrap();

    let voice = |device: &str| {
        VoiceAccess(f.host.trust.clone()).access(
            &ToolContext {
                device_id: device.into(),
                session_id: "s".into(),
            },
            None,
        )
    };
    let (access, device) = voice(DEVICE).unwrap();
    assert_eq!(
        access.scopes().len(),
        3,
        "语音得到设备、当前项目与独立本地知识库工作区"
    );
    assert_eq!(device.unwrap().firmware(), Some("1.2.0"));
    assert!(
        voice("11:22:33:44:55:66").unwrap().1.is_none(),
        "没有登记固件的设备不给机型信息"
    );
    assert!(
        voice("00:00:00:00:00:00").is_none(),
        "未允许的设备没有任何范围"
    );

    // Switching the project: the old project is no longer readable.
    call(&f, json!({"action": "set_project", "name": "另一个项目"})).unwrap();
    assert_eq!(
        call(&f, json!({"action": "list", "scope": project_scope}))
            .unwrap_err()
            .code,
        "context_access_denied"
    );
    call(&f, json!({"action": "clear_project"})).unwrap();
    assert!(call(&f, json!({"action": "status"})).unwrap()["project"].is_null());

    // Revoking the device: the voice access disappears and its grant generation changes.
    f.db.lock()
        .unwrap()
        .set_voice_device_status(DEVICE, laser_runtime::VOICE_DEVICE_REVOKED, 3.0)
        .unwrap();
    f.host.device_changed(DEVICE);
    assert!(voice(DEVICE).is_none());
    assert_eq!(
        call(&f, json!({"action": "list", "scope": device_scope()}))
            .unwrap_err()
            .code,
        "context_access_denied"
    );
}

#[test]
fn nv19_knowledge_experience_and_resume_through_the_host() {
    let f = fixture();
    let docs = tempfile::tempdir().unwrap();
    std::fs::write(
        docs.path().join("皮带.md"),
        "# 皮带张紧\n线条断续时检查皮带是否松动，拧紧张紧螺丝后恢复。\n",
    )
    .unwrap();
    let added = f.host.add_folder(docs.path()).unwrap();
    assert_eq!(added["files"], 1);
    assert_eq!(added["enabled"], true, "新挂载的知识库默认启用");
    let roots = call(&f, json!({"action": "knowledge_list"})).unwrap();
    assert_eq!(roots["roots"][0]["files"], 1);
    assert_eq!(roots["roots"][0]["enabled"], true);
    let root_id = roots["roots"][0]["id"].as_str().unwrap().to_owned();
    call(&f, json!({"action": "knowledge_reindex", "id": root_id})).unwrap();

    // 独立知识库：即使个人记忆开关为 false，勾选启用的知识库依然可被语音对话检索；取消勾选立即停止注入。
    let (retrieval, _) = f.host.hooks();
    let (access, device) = retrieval
        .access
        .access(
            &ToolContext {
                device_id: DEVICE.into(),
                session_id: "s".into(),
            },
            None,
        )
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let block = runtime
        .block_on(
            retrieval
                .retriever
                .prompt_for(access.clone(), "线条断续怎么办", device.clone()),
        )
        .unwrap();
    assert!(block.contains("资料检索结果") && block.contains("拧紧"));
    assert!(
        !block.contains(docs.path().to_str().unwrap()),
        "本机路径不进提示词"
    );

    let rev_before_toggle =
        call(&f, json!({"action": "knowledge_list"})).unwrap()["roots"][0]["revision"]
            .as_i64()
            .unwrap();
    let toggled_off = call(&f, json!({"action": "knowledge_toggle", "id": root_id, "revision": rev_before_toggle, "enabled": false})).unwrap();
    assert_eq!(toggled_off["enabled"], false);
    assert!(
        runtime
            .block_on(retrieval.retriever.prompt_for(
                access.clone(),
                "线条断续怎么办",
                device.clone()
            ))
            .is_none(),
        "取消勾选后不再注入该知识库"
    );
    let toggled_on = call(&f, json!({"action": "knowledge_toggle", "id": root_id, "revision": toggled_off["revision"], "enabled": true})).unwrap();
    assert_eq!(toggled_on["enabled"], true);
    assert!(
        runtime
            .block_on(
                retrieval
                    .retriever
                    .prompt_for(access, "线条断续怎么办", device)
            )
            .is_some(),
        "重新勾选后立即恢复注入"
    );
    let revision = call(&f, json!({"action": "knowledge_list"})).unwrap()["roots"][0]["revision"]
        .as_i64()
        .unwrap();
    call(
        &f,
        json!({"action": "knowledge_remove", "id": root_id, "revision": revision}),
    )
    .unwrap();
    assert!(docs.path().join("皮带.md").exists());

    assert_eq!(call(&f, json!({"action": "experience_record", "device_id": "11:22:33:44:55:66", "symptom": "边缘发黑", "conclusion": "降功率"})).unwrap_err().code, "context_applicability_required");
    let recorded = call(&f, json!({"action": "experience_record", "device_id": DEVICE, "symptom": "边缘发黑", "checks": ["清洁镜片"], "conclusion": "功率降到 60% 后改善"})).unwrap();
    assert_eq!(recorded["row"]["verification"], "candidate");
    let experience_id = recorded["row"]["id"].as_str().unwrap().to_owned();
    let confirmed = call(
        &f,
        json!({"action": "verify", "id": experience_id, "revision": 1, "to": "confirmed"}),
    )
    .unwrap();
    assert!(
        confirmed["content"]
            .as_str()
            .unwrap()
            .ends_with("证据：用户确认已解决"),
        "经验确认时写入证据"
    );
    let recall = call(
        &f,
        json!({"action": "experience_recall", "device_id": DEVICE, "symptom": "切割边缘发黑"}),
    )
    .unwrap();
    assert!(
        recall["text"]
            .as_str()
            .unwrap()
            .contains("[已验证·MOSS-550W 1.2.0]")
    );

    call(&f, json!({"action": "set_project", "name": "维护手册"})).unwrap();
    assert!(call(&f, json!({"action": "resume_get"})).unwrap().is_null());
    f.db.lock().unwrap().connection.execute_batch(
        "INSERT INTO approvals(approval_id,status,kind,target_record,digest,summary,source,created_at,expires_at,updated_at) VALUES ('apr-old','expired','framing','wf','d','走边框','voice',1,121,130);"
    ).unwrap();
    let note = json!({"decisions": ["统一 3 毫米"], "todos": ["重新走边框 [approval:apr-old]"], "evidence": [], "unverified": []});
    let saved = call(&f, json!({"action": "resume_save", "note": note})).unwrap();
    assert_eq!(
        call(&f, json!({"action": "resume_save", "note": note}))
            .unwrap_err()
            .code,
        "context_conflict"
    );
    call(&f, json!({"action": "resume_save", "note": note, "expected": {"id": saved["id"], "revision": saved["revision"]}})).unwrap();
    let resume = call(&f, json!({"action": "resume_get"})).unwrap();
    assert_eq!(resume["revision"], 2);
    assert!(
        resume["text"]
            .as_str()
            .unwrap()
            .contains("（审批 apr-old：已撤销或过期，不会重放）")
    );
    let status: String =
        f.db.lock()
            .unwrap()
            .connection
            .query_row(
                "SELECT status FROM approvals WHERE approval_id='apr-old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(status, "expired", "恢复不改审批");
}

#[test]
fn nv19_deep_search_needs_a_configured_model() {
    let f = fixture();
    assert_eq!(
        call(&f, json!({"action": "deep_search", "query": "边缘发黑"}))
            .unwrap_err()
            .code,
        "context_llm_missing"
    );
}

#[test]
fn nv19_calibrated_speakers_connect_to_voice_access_and_private_owner() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
    {
        let db = db.lock().unwrap();
        db.voice_device_seen(DEVICE, "client-1", "MOSS-550W", "1.2.0", "192.0.2.10", 1.0)
            .unwrap();
        db.set_voice_device_status(DEVICE, laser_runtime::VOICE_DEVICE_ALLOWED, 2.0)
            .unwrap();
    }

    // 1. 未标定时：即使传入 PersonScope，VoiceAccess 也不放行个人范围
    let active = Arc::new(Mutex::new(Vec::<(String, String, i64)>::new()));
    let active_reader = active.clone();
    let host = ContextHost::with_speakers(
        db.clone(),
        settings.clone(),
        Arc::new(move || active_reader.lock().unwrap().clone()),
    );
    let alice = PersonScope::candidate("spk_alice", 2, 1).unwrap();
    let ctx = ToolContext {
        device_id: DEVICE.into(),
        session_id: "s-1".into(),
    };
    let (uncalibrated, _) = VoiceAccess(host.trust.clone())
        .access(&ctx, Some(&alice))
        .unwrap();
    assert!(
        uncalibrated.scope_of(ScopeKind::Person).is_none(),
        "未完成标定时不得开放按人范围"
    );

    // 2. 完成标定并开启 builtin_person_memory_enabled 后：语音回合与私有管理窗口均打通 Person 范围
    *active.lock().unwrap() = vec![("spk_alice".into(), "Alice".into(), 2)];
    let mut voice_settings = moss_voice::settings::load(&settings);
    voice_settings.builtin_person_memory_enabled = true;
    moss_voice::settings::save(&settings, &voice_settings).unwrap();

    let (calibrated, _) = VoiceAccess(host.trust.clone())
        .access(&ctx, Some(&alice))
        .unwrap();
    assert_eq!(
        calibrated.scope_of(ScopeKind::Person).unwrap().key().id(),
        "spk_alice"
    );

    let status = host.manage(&json!({"action": "status"}), || None).unwrap();
    assert_eq!(status["persons"][0]["id"], "spk_alice");

    let person_scope = json!({"kind": "person", "id": "spk_alice"});
    let created = host
        .manage(
            &json!({"action": "create", "scope": person_scope, "category": "preference", "content": "单位默认用毫米", "confirmed": true}),
            || None,
        )
        .unwrap();
    assert_eq!(created["scope"]["kind"], "person");

    // 3. 删除声纹或标定失效后：Person 范围立即失效，不可再越权读取
    active.lock().unwrap().clear();
    host.person_changed("spk_alice");
    assert_eq!(
        host.manage(&json!({"action": "list", "scope": person_scope}), || None)
            .unwrap_err()
            .code,
        "context_access_denied"
    );
}

#[test]
fn nv19_personal_mode_isolates_device_preferences_while_keeping_hardware_facts() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
    {
        let db = db.lock().unwrap();
        db.voice_device_seen(DEVICE, "client-1", "MOSS-550W", "1.2.0", "192.0.2.10", 1.0)
            .unwrap();
        db.set_voice_device_status(DEVICE, laser_runtime::VOICE_DEVICE_ALLOWED, 2.0)
            .unwrap();
    }

    let active = Arc::new(Mutex::new(vec![
        ("spk_alice".to_owned(), "Alice".to_owned(), 2_i64),
        ("spk_bob".to_owned(), "Bob".to_owned(), 1_i64),
    ]));
    let active_reader = active.clone();
    let host = ContextHost::with_speakers(
        db.clone(),
        settings.clone(),
        Arc::new(move || active_reader.lock().unwrap().clone()),
    );

    // 在设备范围写入：1) 旧共享偏好；2) 设备硬件事实；3) 故障排查经验
    host.manage(
        &json!({"action": "create", "scope": device_scope(), "category": "preference", "content": "共享设备旧偏好回答一律用英文", "confirmed": true}),
        || None,
    )
    .unwrap();
    host.manage(
        &json!({"action": "create", "scope": device_scope(), "category": "device_fact", "device_id": DEVICE, "content": "激光模组型号为五百五十瓦蓝光头", "confirmed": true}),
        || None,
    )
    .unwrap();
    let exp = host
        .manage(
            &json!({"action": "experience_record", "device_id": DEVICE, "symptom": "边缘发黑", "checks": ["清洁镜片"], "conclusion": "功率降到 60% 后改善", "confirmed": true}),
            || None,
        )
        .unwrap();
    assert_eq!(exp["row"]["verification"], "confirmed");

    // 开启按人记忆与 NV19 检索，并为 Alice 写入个人偏好
    moss_voice::settings::update_voice(&settings, |v| {
        v.builtin_person_memory_enabled = true;
        Ok(())
    })
    .unwrap();
    host.manage(&json!({"action": "set_options", "retrieval": true}), || {
        None
    })
    .unwrap();
    host.manage(
        &json!({"action": "create", "scope": {"kind": "person", "id": "spk_alice"}, "category": "preference", "content": "个人专属偏好回答一律用中文毫米", "confirmed": true}),
        || None,
    )
    .unwrap();

    let (retrieval, _) = host.hooks();
    let ctx = ToolContext {
        device_id: DEVICE.into(),
        session_id: "s-personal".into(),
    };
    let alice = PersonScope::candidate("spk_alice", 2, 1).unwrap();
    let bob = PersonScope::candidate("spk_bob", 1, 1).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // 1. 未知说话人（person = None）：绝不回退设备范围 Preference，但硬件 DeviceFact / TroubleshootingExperience 仍可读
    let (unknown_access, unknown_dev) = retrieval.access.access(&ctx, None).unwrap();
    assert!(unknown_access.personal_mode());
    assert!(
        runtime
            .block_on(retrieval.retriever.prompt_for(
                unknown_access.clone(),
                "回答一律用什么语言",
                unknown_dev.clone()
            ))
            .is_none(),
        "未知说话人绝不能读到设备范围共享偏好"
    );
    let fact_block = runtime
        .block_on(retrieval.retriever.prompt_for(
            unknown_access.clone(),
            "激光模组型号是什么",
            unknown_dev.clone(),
        ))
        .unwrap();
    assert!(
        fact_block.contains("五百五十瓦蓝光头"),
        "硬件事实仍对设备生效: {fact_block}"
    );
    let exp_block = runtime
        .block_on(
            retrieval
                .retriever
                .prompt_for(unknown_access, "边缘发黑怎么处理", unknown_dev),
        )
        .unwrap();
    assert!(
        exp_block.contains("清洁镜片"),
        "故障经验仍对设备生效: {exp_block}"
    );

    // 2. 已识别说话人 Alice：只命中个人范围偏好，不混入设备范围旧共享偏好；硬件事实正常命中
    let (alice_access, alice_dev) = retrieval.access.access(&ctx, Some(&alice)).unwrap();
    assert!(alice_access.personal_mode());
    let alice_pref = runtime
        .block_on(retrieval.retriever.prompt_for(
            alice_access.clone(),
            "回答一律用什么",
            alice_dev.clone(),
        ))
        .unwrap();
    assert!(
        alice_pref.contains("中文毫米") && !alice_pref.contains("用英文"),
        "{alice_pref}"
    );
    let alice_fact = runtime
        .block_on(
            retrieval
                .retriever
                .prompt_for(alice_access, "激光模组型号是什么", alice_dev),
        )
        .unwrap();
    assert!(alice_fact.contains("五百五十瓦蓝光头"));

    // 3. 另一位说话人 Bob：既看不到 Alice 的偏好，也看不到设备范围旧共享偏好
    let (bob_access, bob_dev) = retrieval.access.access(&ctx, Some(&bob)).unwrap();
    assert!(
        runtime
            .block_on(
                retrieval
                    .retriever
                    .prompt_for(bob_access, "回答一律用什么", bob_dev)
            )
            .is_none()
    );

    // 4. 换机未装声纹模型/标定列表为空但开关仍开时：依然保持个人隔离，不回退设备偏好
    active.lock().unwrap().clear();
    let (degraded_access, degraded_dev) = retrieval.access.access(&ctx, Some(&alice)).unwrap();
    assert!(
        degraded_access.personal_mode() && degraded_access.scope_of(ScopeKind::Person).is_none()
    );
    assert!(
        runtime
            .block_on(retrieval.retriever.prompt_for(
                degraded_access,
                "回答一律用什么语言",
                degraded_dev
            ))
            .is_none()
    );
}
