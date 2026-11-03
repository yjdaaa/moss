use laser_runtime::db::{RuntimeDb, SCHEMA_VERSION};

#[test]
fn nv16_enrollment_is_atomic_bounded_private_and_deleted_with_memory(){
    let db=RuntimeDb::open_memory().unwrap();
    let clips=vec![vec![1.;192];3];
    let row=db.enroll_voice_speaker("server-id","合成人物","campplus_sv_zh_cn_16k_common",&clips,1.).unwrap();
    assert_eq!(row.dimension,192);
    let json=serde_json::to_value(&row).unwrap();
    assert!(json.get("embedding").is_none());
    assert_eq!(db.voice_person_summary(&row.id).unwrap(),Some(("".into(),0)));
    assert!(db.enroll_voice_speaker("other","合成人物","campplus_sv_zh_cn_16k_common",&clips,2.).is_err());
    for bad in [vec![vec![0.;192];3],vec![vec![f32::NAN;192];3],vec![vec![1.;191];3],vec![vec![1.;192];2],vec![vec![1.;192],vec![-1.;192],vec![1.;192]]] {
        assert!(db.enroll_voice_speaker("invalid","invalid","campplus_sv_zh_cn_16k_common",&bad,2.).is_err());
    }
    assert_eq!(db.list_voice_speakers().unwrap().len(),1);
    let templates=db.native_voice_speaker_templates().unwrap();assert_eq!(templates.len(),1);assert_eq!(templates[0].embedding.len(),192);
    assert!(!db.delete_voice_speaker_if_revision("server-id",2).unwrap());
    assert!(db.delete_voice_speaker_if_revision("server-id",1).unwrap());
    assert!(db.voice_person_summary("server-id").unwrap().is_none());
    assert!(db.list_voice_speakers().unwrap().is_empty());
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn home_token_is_private_versioned_and_revocation_invalidates_approvals() {
    let db=RuntimeDb::open_memory().unwrap();
    assert_eq!(db.home_token_status().unwrap(),(false,0));
    assert_eq!(db.update_home_token_if_revision(0,Some("synthetic_long_lived_test_token_without_whitespace"),1.).unwrap(),Some(1));
    assert_eq!(db.home_token_status().unwrap(),(true,1));
    let stored=db.get_secret("voice.homeassistant.token").unwrap().unwrap();
    #[cfg(windows)]
    assert!(stored.starts_with("dpapi-user-v1:"));
    #[cfg(target_os = "macos")]
    assert!(stored.starts_with("keychain-user-v1:"));
    assert!(!stored.contains("synthetic_long_lived"));
    assert_eq!(db.home_token_for_connection().unwrap().as_deref(),Some("synthetic_long_lived_test_token_without_whitespace"));
    assert_eq!(db.update_home_token_if_revision(0,None,2.).unwrap(),None);
    assert_eq!(db.home_token_status().unwrap(),(true,1));
    db.connection.execute(
        "INSERT INTO approvals(approval_id,status,kind,target_record,digest,summary,source,created_at,expires_at,updated_at) VALUES('synthetic-ha','pending','home_control','light.desk','fake','fake approval','voice',1,30,1)",[],
    ).unwrap();
    assert_eq!(db.update_home_token_if_revision(1,None,2.).unwrap(),Some(2));
    assert_eq!(db.home_token_status().unwrap(),(false,2));
    assert!(db.get_secret("voice.homeassistant.token").unwrap().is_none());
    let state:String=db.connection.query_row("SELECT status FROM approvals WHERE approval_id='synthetic-ha'",[],|row|row.get(0)).unwrap();
    assert_eq!(state,"expired");

    assert!(db.update_home_token_if_revision(2,Some("short"),3.).is_err());
    assert_eq!(db.home_token_status().unwrap(),(false,2));
    assert_eq!(db.update_home_token_if_revision(2,Some("synthetic_second_long_lived_test_token"),3.).unwrap(),Some(3));
    assert_eq!(db.home_token_status().unwrap(),(true,3));
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn nv15_legacy_plaintext_migration_is_atomic_and_one_time() {
    let db=RuntimeDb::open_memory().unwrap();
    db.set_secret("voice.homeassistant.token","synthetic_legacy_long_lived_token",1.).unwrap();
    assert!(db.home_token_status().is_err());
    assert!(db.home_token_for_connection().is_err());
    assert!(db.migrate_legacy_home_token(2.).unwrap());
    assert_eq!(db.home_token_status().unwrap(),(true,1));
    assert_eq!(db.home_token_for_connection().unwrap().as_deref(),Some("synthetic_legacy_long_lived_token"));
    assert!(!db.migrate_legacy_home_token(3.).unwrap());
    assert_eq!(db.home_token_status().unwrap(),(true,1));
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn nv15_backup_does_not_persist_plaintext_and_two_connections_cannot_reuse_old_revision(){
    let dir=tempfile::tempdir().unwrap();
    let file=dir.path().join("synthetic.sqlite");
    let original="synthetic_ha_not_a_real_bearer_token_abcdefghijklmnopqrstuvwxyz";
    let db1=RuntimeDb::open(&file).unwrap();
    let db2=RuntimeDb::open(&file).unwrap();
    assert_eq!(db1.update_home_token_if_revision(0,Some(original),1.).unwrap(),Some(1));
    assert_eq!(db2.home_token_status().unwrap(),(true,1));
    assert_eq!(db2.update_home_token_if_revision(0,None,2.).unwrap(),None);
    assert_eq!(db1.update_home_token_if_revision(1,None,3.).unwrap(),Some(2));
    assert_eq!(db2.update_home_token_if_revision(1,Some(original),4.).unwrap(),None);
    assert_eq!(db2.home_token_status().unwrap(),(false,2));
    // The original database plus its live WAL must never contain the new
    // token as a plaintext byte string. This does not cleanse old backups.
    for suffix in ["","-wal","-shm"] {
        let path=std::path::PathBuf::from(format!("{}{}",file.display(),suffix));
        if let Ok(bytes)=std::fs::read(path){
            assert!(!bytes.windows(original.len()).any(|chunk|chunk==original.as_bytes()));
        }
    }
    // A raw copied database is not a portable credential backup. Key
    // ownership is tied to Windows DPAPI current-user context.
    assert!(db2.home_token_for_connection().unwrap().is_none());
}


#[cfg(any(windows, target_os = "macos"))]
#[test]
fn nv15_token_rotation_revoke_wipes_all_voice_grants_and_approval_pending() {
    let db = RuntimeDb::open_memory().unwrap();
    db.update_home_token_if_revision(0,
        Some("nv15_synthetic_token_for_revocation_test_abcdef"),1.0)
        .unwrap().unwrap();
    db.set_secret("voice.homeassistant.voice_read_consent", "1:1", 1.).unwrap();
    db.set_secret("voice.homeassistant.voice_control_consent", "1:1:synthetic", 1.).unwrap();
    db.connection.execute(
        "INSERT INTO approvals(approval_id,status,kind,target_record,digest,summary,source,created_at,expires_at,updated_at) VALUES('nv15-new','approved','home_control','light.desk','fake','fake approval','voice',1,30,1)",[],
    ).unwrap();
    assert_eq!(db.update_home_token_if_revision(1,
        Some("nv15_synthetic_replaced_token_abcdefxyz"),2.0).unwrap(),Some(2));
    for name in ["voice.homeassistant.voice_read_consent",
                 "voice.homeassistant.voice_control_consent"] {
        assert_eq!(db.get_secret(name).unwrap(),None,"old grant survived token rotation: {name}");
    }
    let status: String=db.connection.query_row(
        "SELECT status FROM approvals WHERE approval_id='nv15-new'",[],|r|r.get(0)
    ).unwrap();
    assert_eq!(status,"expired");
    // Reentering the original token cannot resurrect either stale grant.
    db.update_home_token_if_revision(2,
        Some("nv15_synthetic_token_for_revocation_test_abcdef"),3.).unwrap().unwrap();
    assert_eq!(db.get_secret("voice.homeassistant.voice_control_consent").unwrap(),None);
    assert_eq!(db.get_secret("voice.homeassistant.voice_read_consent").unwrap(),None);
    db.set_secret("voice.homeassistant.voice_read_consent","3:3",4.).unwrap();
    assert_eq!(db.update_home_token_if_revision(3,None,5.).unwrap(),Some(4));
    assert_eq!(db.get_secret("voice.homeassistant.voice_read_consent").unwrap(),None);
    assert_eq!(db.home_token_status().unwrap(),(false,4));
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn nv15_concurrent_token_cas_has_only_one_winner_across_connections() {
    use std::{sync::{Arc,Barrier},thread};
    let dir=tempfile::tempdir().unwrap();
    let path=dir.path().join("parallel.sqlite");
    let db=RuntimeDb::open(&path).unwrap();
    assert_eq!(db.update_home_token_if_revision(0,
        Some("nv15_concurrent_old_synthetic_token_abc"),1.).unwrap(),Some(1));
    drop(db);
    let ready=Arc::new(Barrier::new(3));
    let handles=(0..2).map(|i|{
        let path=path.clone();
        let ready=ready.clone();
        thread::spawn(move || {
            let db=RuntimeDb::open(&path).unwrap();
            ready.wait();
            db.update_home_token_if_revision(1,Some(&format!(
                "nv15_concurrent_new_synthetic_token_{i}_abcdef")),2. + i as f64)
        })
    }).collect::<Vec<_>>();
    ready.wait();
    let result=handles.into_iter().map(|h|h.join().unwrap().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(result.iter().filter(|v|v.is_some()).count(),1);
    let db=RuntimeDb::open(&path).unwrap();
    assert_eq!(db.home_token_status().unwrap(),(true,2));
    assert!(db.home_token_for_connection().unwrap().unwrap()
        .starts_with("nv15_concurrent_new_synthetic_token_"));
}

#[test]
fn nv15_foreign_dpapi_blob_recovery_requires_revision_and_never_decrypts_old_secret() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("copied-from-other-account.sqlite");
    let primary = RuntimeDb::open(&path).unwrap();
    // A synthetically invalid DPAPI envelope models a backup from a user
    // that cannot decrypt the original credential; no real key is read.
    primary.set_secret("voice.homeassistant.token","dpapi-user-v1:bad0beef",1.).unwrap();
    primary.set_secret("voice.homeassistant.token_revision","7",1.).unwrap();
    primary.set_secret("voice.homeassistant.voice_read_consent","3:7",1.).unwrap();
    primary.set_secret("voice.homeassistant.voice_control_consent","3:7:fake",1.).unwrap();
    primary.connection.execute(
        "INSERT INTO approvals(approval_id,status,kind,target_record,digest,summary,source,created_at,expires_at,updated_at) VALUES('foreign-approval','approved','home_control','light.desk','fake','fake approval','voice',1,30,1)",[]
    ).unwrap();
    assert!(primary.home_token_status().is_err());
    assert!(primary.home_token_for_connection().is_err());
    assert_eq!(primary.home_token_recovery_revision().unwrap(),7);
    let secondary = RuntimeDb::open(&path).unwrap();
    assert_eq!(secondary.update_home_token_if_revision(6,None,2.).unwrap(),None,
        "stale restore must never revoke a newer credential");
    assert_eq!(secondary.update_home_token_if_revision(7,None,3.).unwrap(),Some(8));
    assert_eq!(primary.home_token_status().unwrap(),(false,8));
    assert_eq!(primary.home_token_for_connection().unwrap(),None);
    for secret in ["voice.homeassistant.token",
                   "voice.homeassistant.voice_read_consent",
                   "voice.homeassistant.voice_control_consent"] {
        assert_eq!(primary.get_secret(secret).unwrap(),None);
    }
    let state:String=primary.connection.query_row(
        "SELECT status FROM approvals WHERE approval_id='foreign-approval'",[],|r|r.get(0)
    ).unwrap();
    assert_eq!(state,"expired");
    #[cfg(any(windows, target_os = "macos"))]
    assert_eq!(primary.update_home_token_if_revision(7,
        Some("synthetic_old_token_must_not_revive_this_approval"),4.).unwrap(),None);
    #[cfg(not(any(windows, target_os = "macos")))]
    assert!(primary.update_home_token_if_revision(7,
        Some("synthetic_old_token_must_not_revive_this_approval"),4.).is_err());
    assert_eq!(primary.home_token_status().unwrap(),(false,8));
}

#[test]
fn nv15_corrupted_credential_revision_disables_even_backup_recovery() {
    let db=RuntimeDb::open_memory().unwrap();
    db.set_secret("voice.homeassistant.token_revision","broken",1.).unwrap();
    db.set_secret("voice.homeassistant.token","dpapi-user-v1:bad0beef",1.).unwrap();
    assert!(db.home_token_status().is_err());
    assert!(db.home_token_recovery_revision().is_err());
    assert!(db.update_home_token_if_revision(0,None,2.).is_err());
    assert!(db.home_token_for_connection().is_err());
}

#[test]
fn nv16_person_memory_revision_unknown_and_deletion_are_isolated() {
    let db=RuntimeDb::open_memory().unwrap();
    for (id,name) in [("person-a","甲"),("person-b","乙")] {
        db.connection.execute(
           "INSERT INTO voice_speakers(id,name,model_id,dimension,embedding,revision,created_at) VALUES(?1,?2,'synthetic',64,?3,1,0)",
           rusqlite::params![id,name,vec![0_u8;256]],
        ).unwrap();
        db.connection.execute("INSERT INTO voice_person_memory(speaker_id,updated_at) VALUES(?1,0)",[id]).unwrap();
    }
    assert_eq!(db.voice_person_summary("unknown").unwrap(),None);
    assert!(!db.update_voice_person_summary_if_revision("unknown",0,"fake",1.).unwrap());
    assert!(db.update_voice_person_summary_if_revision("person-a",0,"personal",1.).unwrap());
    assert!(!db.update_voice_person_summary_if_revision("person-a",0,"stale",2.).unwrap());
    assert_eq!(db.voice_person_summary("person-a").unwrap(),Some(("personal".into(),1)));
    assert_eq!(db.voice_person_summary("person-b").unwrap(),Some(("".into(),0)));
    assert!(!db.delete_voice_speaker_if_revision("person-a",2).unwrap());
    assert!(db.delete_voice_speaker_if_revision("person-a",1).unwrap());
    assert_eq!(db.voice_person_summary("person-a").unwrap(),None);
    assert_eq!(db.voice_person_summary("person-b").unwrap(),Some(("".into(),0)));
}

#[test]
fn firmware_download_revision_and_transition_cannot_be_replayed() {
    let db = RuntimeDb::open_memory().unwrap();
    db.connection.execute(
        "INSERT INTO voice_firmware_delivery(delivery_id,device_id,artifact_sha256,updated_at) VALUES('d','device',?1,0)",
        rusqlite::params!["a".repeat(64)],
    ).unwrap();
    assert!(db.update_firmware_download_if_revision("d",0,"downloading",1.).unwrap());
    assert!(!db.update_firmware_download_if_revision("d",0,"downloaded",2.).unwrap());
    assert!(!db.update_firmware_download_if_revision("d",1,"pending",2.).is_ok());
    assert!(db.update_firmware_download_if_revision("d",1,"unknown",3.).unwrap());
    assert!(!db.update_firmware_download_if_revision("d",2,"downloading",4.).unwrap());
    assert!(!db.update_firmware_download_if_revision("other",2,"downloaded",4.).unwrap());
    let state: (String,i64)=db.connection.query_row(
       "SELECT download_status,revision FROM voice_firmware_delivery WHERE delivery_id='d'",[],|r|Ok((r.get(0)?,r.get(1)?))
    ).unwrap();
    assert_eq!(state,("unknown".into(),2));
}

#[test]
fn nv16_nv17_sqlite_migration_and_failclosed_restart() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("runtime.sqlite");
    {
        let db = RuntimeDb::open(&file).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        db.connection.execute(
            "INSERT INTO voice_speakers(id,name,model_id,dimension,embedding,revision,created_at) VALUES('test-person','甲','synthetic',64,?1,1,1.0)",
            rusqlite::params![vec![0_u8; 256]],
        ).unwrap();
        db.connection.execute(
            "INSERT INTO voice_person_memory(speaker_id,summary,revision,updated_at) VALUES('test-person','private',1,1.0)",
            [],
        ).unwrap();
        db.connection.execute(
            "INSERT INTO voice_firmware_delivery(delivery_id,device_id,artifact_sha256,download_status,updated_at) VALUES('test-transfer','test-device',?1,'downloading',1.0)",
            rusqlite::params!["a".repeat(64)],
        ).unwrap();
    }
    let db = RuntimeDb::open(&file).unwrap();
    assert_eq!(db.recover_firmware_downloads(2.0).unwrap(), 1);
    assert_eq!(db.recover_firmware_downloads(3.0).unwrap(), 0);
    let state: (String,String,i64) = db.connection.query_row(
        "SELECT download_status,device_report_status,revision FROM voice_firmware_delivery WHERE delivery_id='test-transfer'",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).unwrap();
    assert_eq!(state,("unknown".into(),"not_reported".into(),1));
    let deliveries=db.list_firmware_deliveries().unwrap();
    assert_eq!(deliveries.len(),1);
    assert_eq!(deliveries[0].download_status,"unknown");
    assert_eq!(deliveries[0].device_report_status,"not_reported");
    let summary: String = db.connection.query_row(
        "SELECT summary FROM voice_person_memory WHERE speaker_id='test-person'",[],|r|r.get(0)
    ).unwrap();
    assert_eq!(summary,"private");
    db.connection.execute("DELETE FROM voice_person_memory WHERE speaker_id='test-person'",[]).unwrap();
    let remaining: i64=db.connection.query_row("SELECT count(*) FROM voice_person_memory",[],|r|r.get(0)).unwrap();
    assert_eq!(remaining,0);
}

#[test]
fn device_snapshot_keeps_raw_json_and_rejects_unknown_or_oversized(){
    let db=RuntimeDb::open_memory().unwrap();
    assert_eq!(SCHEMA_VERSION,15);
    db.voice_device_seen("dev","client","lichuang-dev","2.5.0","192.168.1.9",1.).unwrap();
    assert!(db.voice_device_snapshot("dev","info",r#"{"application":{"version":"2.5.0"}}"#,2.).unwrap());
    assert!(db.voice_device_snapshot("dev","hello",r#"{"aec":true}"#,3.).unwrap());
    assert!(db.voice_device_snapshot("dev","status",r#"{"volume":50}"#,4.).unwrap());
    let row=db.voice_device("dev").unwrap().unwrap();
    assert_eq!(row.info_json.as_deref(),Some(r#"{"application":{"version":"2.5.0"}}"#));
    assert_eq!(row.hello_json.as_deref(),Some(r#"{"aec":true}"#));
    assert_eq!(row.status_json.as_deref(),Some(r#"{"volume":50}"#));
    assert_eq!((row.info_at,row.hello_at,row.status_at),(Some(2.),Some(3.),Some(4.)));
    // 未知种类、超长内容与未登记设备都不写库，也不影响已有快照
    assert!(!db.voice_device_snapshot("dev","name","x",5.).unwrap());
    let long=format!(r#"{{"a":"{}"}}"#,"x".repeat(9000));
    assert!(!db.voice_device_snapshot("dev","info",&long,6.).unwrap());
    assert!(!db.voice_device_snapshot("missing","info","{}",7.).unwrap());
    assert_eq!(db.voice_device("dev").unwrap().unwrap().info_at,Some(2.));
}

#[test]
fn listen_mode_snapshot_is_the_device_aec_evidence(){
    let db=RuntimeDb::open_memory().unwrap();
    assert_eq!(SCHEMA_VERSION,15);
    db.voice_device_seen("dev","client","lichuang-dev","2.5.0","1.2.3.4",1.).unwrap();
    assert!(db.voice_device_snapshot("dev","listen",r#"{"mode":"realtime"}"#,2.).unwrap());
    let row=db.voice_device("dev").unwrap().unwrap();
    assert_eq!(row.listen_json.as_deref(),Some(r#"{"mode":"realtime"}"#));
    assert_eq!(row.listen_at,Some(2.));
    // 只保留最近一次：设备双击关掉 AEC 后退回 auto 会覆盖
    assert!(db.voice_device_snapshot("dev","listen",r#"{"mode":"auto"}"#,3.).unwrap());
    assert_eq!(db.voice_device("dev").unwrap().unwrap().listen_json.as_deref(),Some(r#"{"mode":"auto"}"#));
    assert_eq!(db.voice_device("dev").unwrap().unwrap().listen_at,Some(3.));
}
