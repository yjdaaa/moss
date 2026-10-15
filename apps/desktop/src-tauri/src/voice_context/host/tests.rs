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
        db.voice_device_seen(DEVICE, "client-1", "MOSS-550W", "1.2.0", "192.0.2.10", 1.0).unwrap();
        db.set_voice_device_status(DEVICE, laser_runtime::VOICE_DEVICE_ALLOWED, 2.0).unwrap();
        db.voice_device_seen("11:22:33:44:55:66", "client-2", "", "", "192.0.2.11", 1.0).unwrap();
        db.set_voice_device_status("11:22:33:44:55:66", laser_runtime::VOICE_DEVICE_ALLOWED, 2.0).unwrap();
    }
    let host = ContextHost::new(db.clone(), settings.clone());
    Fixture { host, db, settings, _dir: dir }
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
    assert_eq!((status["retrieval"].as_bool(), status["extraction"].as_bool()), (Some(false), Some(false)));
    assert!(status["project"].is_null());
    assert_eq!(status["devices"].as_array().unwrap().len(), 2);
    assert!(!f.host.retriever.enabled() && !f.host.extractor.enabled());

    assert_eq!(call(&f, json!({"action": "set_options", "extraction": true})).unwrap_err().code, "context_confirm_required");
    assert!(!moss_voice::settings::load(&f.settings).builtin_context.extraction, "未确认时什么都不写");
    call(&f, json!({"action": "set_options", "retrieval": true, "extraction": true, "confirmed": true})).unwrap();
    assert!(f.host.retriever.enabled() && f.host.extractor.enabled());
    assert_eq!(moss_voice::settings::load(&f.settings).builtin_context.extraction, true);
    call(&f, json!({"action": "set_options", "retrieval": false, "extraction": false})).unwrap();
    assert!(!f.host.retriever.enabled() && !f.host.extractor.enabled(), "关闭不需要再次确认");

    // A fresh host reads the saved switches.
    call(&f, json!({"action": "set_options", "retrieval": true})).unwrap();
    assert!(ContextHost::new(f.db.clone(), f.settings.clone()).retriever.enabled());
    assert_eq!(call(&f, json!({"action": "no_such_action"})).unwrap_err().code, "context_invalid");
}

#[test]
fn nv19_memory_crud_search_import_and_export_through_the_host() {
    let f = fixture();
    let created = call(&f, json!({"action": "create", "scope": device_scope(), "category": "preference", "content": "回答时先说预计时长", "confirmed": true})).unwrap();
    assert_eq!(created["verification"], "confirmed");
    let id = created["id"].as_str().unwrap().to_owned();
    let listed = call(&f, json!({"action": "list", "scope": device_scope()})).unwrap();
    assert_eq!(listed["rows"].as_array().unwrap().len(), 1);
    let found = call(&f, json!({"action": "list", "scope": device_scope(), "query": "预计时长"})).unwrap();
    assert_eq!(found["rows"][0]["id"], id.as_str(), "私有窗口按 FTS5 检索");
    assert_eq!(call(&f, json!({"action": "list", "scope": {"kind": "device", "id": "99:99:99:99:99:99"}, "query": "预计时长"})).unwrap_err().code, "context_access_denied");

    let edited = call(&f, json!({"action": "edit", "id": id, "revision": 1, "content": "回答时先说步骤"})).unwrap();
    assert_eq!(edited["revision"], 2);
    assert_eq!(call(&f, json!({"action": "edit", "id": id, "revision": 1, "content": "迟到"})).unwrap_err().code, "context_conflict");
    assert_eq!(call(&f, json!({"action": "create", "scope": device_scope(), "category": "preference", "content": "发送 G1 X10 Y10 S1000"})).unwrap_err().code, "context_content_sensitive");

    let exported = call(&f, json!({"action": "export", "scope": device_scope()})).unwrap();
    assert_eq!(exported["memories"].as_array().unwrap().len(), 1);
    call(&f, json!({"action": "verify", "id": id, "revision": 2, "to": "invalidated"})).unwrap();
    assert_eq!(call(&f, json!({"action": "delete", "id": id, "revision": 3})).unwrap()["deleted"], id.as_str());
    assert!(call(&f, json!({"action": "list", "scope": device_scope()})).unwrap()["rows"].as_array().unwrap().is_empty());

    // NV10 summary → candidates once; the NV10 summary itself is untouched.
    {
        let db = f.db.lock().unwrap();
        db.ensure_voice_memory(DEVICE, 1.0).unwrap();
        db.update_voice_memory_if_revision(DEVICE, 0, "喜欢简短回答\n周末不加工", 2.0).unwrap();
    }
    let status = call(&f, json!({"action": "status"})).unwrap();
    assert_eq!(status["legacy_summaries"], json!([DEVICE]));
    assert_eq!(call(&f, json!({"action": "import_legacy", "device_id": DEVICE})).unwrap()["created"], 1);
    assert_eq!(call(&f, json!({"action": "import_legacy", "device_id": DEVICE})).unwrap()["skipped"], json!(["already_imported"]));
    assert_eq!(f.db.lock().unwrap().voice_memory(DEVICE).unwrap().unwrap().summary, "喜欢简短回答\n周末不加工");

    assert_eq!(call(&f, json!({"action": "clear", "scope": device_scope()})).unwrap_err().code, "context_confirm_required");
    assert_eq!(call(&f, json!({"action": "clear", "scope": device_scope(), "confirmed": true})).unwrap()["cleared"], 1);
}

#[test]
fn nv19_revoked_device_and_switched_project_make_old_scopes_stale() {
    let f = fixture();
    let project = call(&f, json!({"action": "set_project", "name": "椴木杯垫"})).unwrap();
    let project_id = project["project"]["id"].as_str().unwrap().to_owned();
    let project_scope = json!({"kind": "project", "id": project_id});
    call(&f, json!({"action": "create", "scope": project_scope, "category": "task_context", "content": "周五交付样品"})).unwrap();

    let voice = |device: &str| VoiceAccess(f.host.trust.clone()).access(&ToolContext { device_id: device.into(), session_id: "s".into() }, None);
    let (access, device) = voice(DEVICE).unwrap();
    assert_eq!(access.scopes().len(), 2, "语音得到设备和当前项目");
    assert_eq!(device.unwrap().firmware(), Some("1.2.0"));
    assert!(voice("11:22:33:44:55:66").unwrap().1.is_none(), "没有登记固件的设备不给机型信息");
    assert!(voice("00:00:00:00:00:00").is_none(), "未允许的设备没有任何范围");

    // Switching the project: the old project is no longer readable.
    call(&f, json!({"action": "set_project", "name": "另一个项目"})).unwrap();
    assert_eq!(call(&f, json!({"action": "list", "scope": project_scope})).unwrap_err().code, "context_access_denied");
    call(&f, json!({"action": "clear_project"})).unwrap();
    assert!(call(&f, json!({"action": "status"})).unwrap()["project"].is_null());

    // Revoking the device: the voice access disappears and its grant generation changes.
    f.db.lock().unwrap().set_voice_device_status(DEVICE, laser_runtime::VOICE_DEVICE_REVOKED, 3.0).unwrap();
    f.host.device_changed(DEVICE);
    assert!(voice(DEVICE).is_none());
    assert_eq!(call(&f, json!({"action": "list", "scope": device_scope()})).unwrap_err().code, "context_access_denied");
}

#[test]
fn nv19_knowledge_experience_and_resume_through_the_host() {
    let f = fixture();
    let docs = tempfile::tempdir().unwrap();
    std::fs::write(docs.path().join("皮带.md"), "# 皮带张紧\n线条断续时检查皮带是否松动，拧紧张紧螺丝后恢复。\n").unwrap();
    assert_eq!(f.host.add_folder(docs.path()).unwrap_err().code, "context_no_project", "资料必须挂在当前项目下");
    call(&f, json!({"action": "set_project", "name": "维护手册"})).unwrap();
    let added = f.host.add_folder(docs.path()).unwrap();
    assert_eq!(added["files"], 1);
    let roots = call(&f, json!({"action": "knowledge_list"})).unwrap();
    assert_eq!(roots["roots"][0]["files"], 1);
    let root_id = roots["roots"][0]["id"].as_str().unwrap().to_owned();
    call(&f, json!({"action": "knowledge_reindex", "id": root_id})).unwrap();

    // Voice retrieval with both switches: memory hook sees the knowledge base.
    call(&f, json!({"action": "set_options", "retrieval": true})).unwrap();
    let (retrieval, _) = f.host.hooks();
    let (access, device) = retrieval.access.access(&ToolContext { device_id: DEVICE.into(), session_id: "s".into() }, None).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let block = runtime.block_on(retrieval.retriever.prompt_for(access, "线条断续怎么办", device)).unwrap();
    assert!(block.contains("资料检索结果") && block.contains("拧紧"));
    assert!(!block.contains(docs.path().to_str().unwrap()), "本机路径不进提示词");

    let revision = call(&f, json!({"action": "knowledge_list"})).unwrap()["roots"][0]["revision"].as_i64().unwrap();
    call(&f, json!({"action": "knowledge_remove", "id": root_id, "revision": revision})).unwrap();
    assert!(docs.path().join("皮带.md").exists());

    assert_eq!(call(&f, json!({"action": "experience_record", "device_id": "11:22:33:44:55:66", "symptom": "边缘发黑", "conclusion": "降功率"})).unwrap_err().code, "context_applicability_required");
    let recorded = call(&f, json!({"action": "experience_record", "device_id": DEVICE, "symptom": "边缘发黑", "checks": ["清洁镜片"], "conclusion": "功率降到 60% 后改善"})).unwrap();
    assert_eq!(recorded["row"]["verification"], "candidate");
    let experience_id = recorded["row"]["id"].as_str().unwrap().to_owned();
    let confirmed = call(&f, json!({"action": "verify", "id": experience_id, "revision": 1, "to": "confirmed"})).unwrap();
    assert!(confirmed["content"].as_str().unwrap().ends_with("证据：用户确认已解决"), "经验确认时写入证据");
    let recall = call(&f, json!({"action": "experience_recall", "device_id": DEVICE, "symptom": "切割边缘发黑"})).unwrap();
    assert!(recall["text"].as_str().unwrap().contains("[已验证·MOSS-550W 1.2.0]"));

    assert!(call(&f, json!({"action": "resume_get"})).unwrap().is_null());
    f.db.lock().unwrap().connection.execute_batch(
        "INSERT INTO approvals(approval_id,status,kind,target_record,digest,summary,source,created_at,expires_at,updated_at) VALUES ('apr-old','expired','framing','wf','d','走边框','voice',1,121,130);"
    ).unwrap();
    let note = json!({"decisions": ["统一 3 毫米"], "todos": ["重新走边框 [approval:apr-old]"], "evidence": [], "unverified": []});
    let saved = call(&f, json!({"action": "resume_save", "note": note})).unwrap();
    assert_eq!(call(&f, json!({"action": "resume_save", "note": note})).unwrap_err().code, "context_conflict");
    call(&f, json!({"action": "resume_save", "note": note, "expected": {"id": saved["id"], "revision": saved["revision"]}})).unwrap();
    let resume = call(&f, json!({"action": "resume_get"})).unwrap();
    assert_eq!(resume["revision"], 2);
    assert!(resume["text"].as_str().unwrap().contains("（审批 apr-old：已撤销或过期，不会重放）"));
    let status: String = f.db.lock().unwrap().connection.query_row("SELECT status FROM approvals WHERE approval_id='apr-old'", [], |r| r.get(0)).unwrap();
    assert_eq!(status, "expired", "恢复不改审批");
}

#[test]
fn nv19_deep_search_needs_a_configured_model() {
    let f = fixture();
    assert_eq!(call(&f, json!({"action": "deep_search", "query": "边缘发黑"})).unwrap_err().code, "context_llm_missing");
}
