//! NV19-02 schema v10: migration, rollback, CHECK constraints and conditional CRUD.
//! In-memory SQLite with synthetic rows only.
use laser_runtime::db::{RuntimeDb, SCHEMA_VERSION};
use laser_runtime::{ContextExtractionRunRow, ContextMemoryPatch, ContextMemoryRow};

fn row(id: &str, scope_kind: &str, scope_id: &str) -> ContextMemoryRow {
    ContextMemoryRow {
        id: id.into(),
        scope_kind: scope_kind.into(),
        scope_id: scope_id.into(),
        category: "preference".into(),
        content: "喜欢先听到预计时长".into(),
        verification: "candidate".into(),
        source_kind: "user_edit".into(),
        source_ref: None,
        device_model: None,
        firmware: None,
        revision: 1,
        created_at: 10.0,
        updated_at: 10.0,
        expires_at: None,
        deleted_at: None,
    }
}

fn patch(content: &str, verification: &str) -> ContextMemoryPatch {
    ContextMemoryPatch { content: content.into(), verification: verification.into(), device_model: None, firmware: None, expires_at: None }
}

fn count(db: &RuntimeDb, sql: &str) -> i64 {
    db.connection.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn nv19_v10_upgrades_v9_database_and_keeps_nv10_summaries() {
    assert_eq!(SCHEMA_VERSION, 12);
    let db = RuntimeDb::open_memory().unwrap();
    db.connection
        .execute_batch("DROP TABLE context_knowledge_fts; DROP TABLE context_knowledge_roots; DROP TABLE context_knowledge_dirs; DROP TABLE context_knowledge_files; DROP TABLE context_memories_fts; DROP TABLE context_memories; DROP TABLE context_extraction_runs; DELETE FROM schema_version WHERE version>=10;")
        .unwrap();
    db.ensure_voice_memory("aa:bb:cc:dd:ee:ff", 1.0).unwrap();
    db.update_voice_memory_if_revision("aa:bb:cc:dd:ee:ff", 0, "喜欢简短回答", 2.0).unwrap().unwrap();
    assert_eq!(db.schema_version().unwrap(), 9);

    db.migrate().unwrap();
    assert_eq!(db.schema_version().unwrap(), 12);
    let legacy = db.voice_memory("aa:bb:cc:dd:ee:ff").unwrap().unwrap();
    assert_eq!((legacy.summary.as_str(), legacy.revision), ("喜欢简短回答", 1), "NV10 摘要原样保留");
    assert_eq!(count(&db, "SELECT COUNT(*) FROM context_memories"), 0, "迁移不自动搬迁旧摘要");

    db.migrate().unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM schema_version WHERE version=10"), 1, "重复迁移幂等");
}

#[test]
fn nv19_v10_failure_rolls_back_and_retry_succeeds() {
    let db = RuntimeDb::open_memory().unwrap();
    db.connection
        .execute_batch(
            "DROP TABLE context_knowledge_fts; DROP TABLE context_knowledge_roots; DROP TABLE context_knowledge_dirs; DROP TABLE context_knowledge_files; DROP TABLE context_memories_fts; DROP TABLE context_memories; DROP TABLE context_extraction_runs; DELETE FROM schema_version WHERE version>=10;
             CREATE TABLE context_extraction_runs(unexpected TEXT);",
        )
        .unwrap();
    assert!(db.migrate().is_err());
    assert_eq!(db.schema_version().unwrap(), 9, "失败的迁移不得写入版本号");
    assert_eq!(count(&db, "SELECT COUNT(*) FROM sqlite_master WHERE name='context_memories'"), 0, "同一事务内先建的表也回滚");

    db.connection.execute_batch("DROP TABLE context_extraction_runs;").unwrap();
    db.migrate().unwrap();
    assert_eq!(db.schema_version().unwrap(), 12);
}

#[test]
fn nv19_check_constraints_reject_values_outside_the_contract() {
    let db = RuntimeDb::open_memory().unwrap();
    let mut bad = Vec::new();
    for (field, value) in [("category", "skill"), ("scope_kind", "global"), ("verification", "verified"), ("source_kind", "model")] {
        let mut item = row(&format!("bad-{field}"), "device", "dev-a");
        match field {
            "category" => item.category = value.into(),
            "scope_kind" => item.scope_kind = value.into(),
            "verification" => item.verification = value.into(),
            _ => item.source_kind = value.into(),
        }
        bad.push(item);
    }
    let mut empty = row("empty", "device", "dev-a");
    empty.content = "   ".into();
    bad.push(empty);
    let mut long = row("long", "device", "dev-a");
    long.content = "字".repeat(1001);
    bad.push(long);
    let mut firmware_only = row("fw", "device", "dev-a");
    firmware_only.firmware = Some("1.2.0".into());
    bad.push(firmware_only);
    let mut no_scope = row("noscope", "device", "");
    no_scope.scope_id = String::new();
    bad.push(no_scope);
    for item in bad {
        assert!(db.insert_context_memory(&item).is_err(), "应拒绝 {}", item.id);
    }
    let mut reborn = row("reborn", "device", "dev-a");
    reborn.revision = 2;
    assert!(db.insert_context_memory(&reborn).is_err(), "新条目只能从 revision 1 开始");
    reborn.revision = 1;
    reborn.deleted_at = Some(1.0);
    assert!(db.insert_context_memory(&reborn).is_err(), "不能直接写入已删除条目");
    // Raw SQL bypassing the API still cannot store a tombstone with content.
    assert!(db
        .connection
        .execute(
            "INSERT INTO context_memories(id,scope_kind,scope_id,category,content,verification,source_kind,revision,created_at,updated_at,deleted_at)
             VALUES('raw','device','dev-a','preference','残留','candidate','user_edit',1,1,1,1)",
            [],
        )
        .is_err());
    assert_eq!(count(&db, "SELECT COUNT(*) FROM context_memories"), 0);
}

#[test]
fn nv19_crud_is_scoped_conditional_and_deletion_cannot_be_undone_by_late_writes() {
    let db = RuntimeDb::open_memory().unwrap();
    let created = db.insert_context_memory(&row("m1", "device", "dev-a")).unwrap();
    assert_eq!(created.revision, 1);
    let mut fact = row("m2", "device", "dev-a");
    fact.category = "device_fact".into();
    fact.verification = "confirmed".into();
    fact.device_model = Some("MOSS-550W".into());
    fact.firmware = Some("1.2.0".into());
    db.insert_context_memory(&fact).unwrap();
    db.insert_context_memory(&row("m3", "device", "dev-b")).unwrap();
    db.insert_context_memory(&row("m4", "workspace", "dev-a")).unwrap();
    assert!(db.insert_context_memory(&row("m1", "device", "dev-a")).is_err(), "id 唯一");

    let listed: Vec<_> = db.list_context_memories("device", "dev-a", false, 100).unwrap().into_iter().map(|r| r.id).collect();
    assert_eq!(listed.len(), 2);
    assert!(listed.contains(&"m1".to_string()) && listed.contains(&"m2".to_string()), "只列出本范围，同 id 不同 kind 不混入");

    // Conditional update: the first writer wins, a stale writer gets None.
    let edited = db.update_context_memory_if_revision("m1", 1, &patch("改为喜欢详细回答", "confirmed"), 20.0).unwrap().unwrap();
    assert_eq!((edited.revision, edited.verification.as_str(), edited.updated_at), (2, "confirmed", 20.0));
    assert!(db.update_context_memory_if_revision("m1", 1, &patch("迟到的旧结果", "candidate"), 21.0).unwrap().is_none());
    assert!(db.update_context_memory_if_revision("m1", 2, &patch("降级", "candidate"), 21.0).unwrap().is_none(), "已确认不能降回候选");
    assert_eq!(db.context_memory("m1").unwrap().unwrap().content, "改为喜欢详细回答");

    let invalidated = db.update_context_memory_if_revision("m1", 2, &patch("改为喜欢详细回答", "invalidated"), 22.0).unwrap().unwrap();
    assert!(db.update_context_memory_if_revision("m1", invalidated.revision, &patch("复活", "confirmed"), 23.0).unwrap().is_none(), "作废为终态");

    // Deletion leaves an empty tombstone; late writes with the old revision fail.
    assert!(!db.delete_context_memory_if_revision("m2", 9, 30.0).unwrap());
    assert!(db.delete_context_memory_if_revision("m2", 1, 30.0).unwrap());
    let tomb = db.context_memory("m2").unwrap().unwrap();
    assert_eq!((tomb.content.as_str(), tomb.device_model.as_deref(), tomb.revision, tomb.deleted_at), ("", None, 2, Some(30.0)));
    assert!(db.update_context_memory_if_revision("m2", 1, &patch("迟到", "confirmed"), 31.0).unwrap().is_none());
    assert!(db.update_context_memory_if_revision("m2", 2, &patch("复活", "confirmed"), 31.0).unwrap().is_none(), "已删除不能再编辑");
    assert!(!db.delete_context_memory_if_revision("m2", 2, 31.0).unwrap());
    assert_eq!(db.list_context_memories("device", "dev-a", false, 100).unwrap().len(), 1);
    assert_eq!(db.list_context_memories("device", "dev-a", true, 100).unwrap().len(), 2);

    assert!(db.update_context_memory_if_revision("m3", 0, &patch("x", "candidate"), 1.0).is_err());
    assert!(db.update_context_memory_if_revision("m3", 1, &patch("x", "candidate"), f64::NAN).is_err());
}

#[test]
fn nv19_clear_scope_and_source_tombstones() {
    let db = RuntimeDb::open_memory().unwrap();
    let mut imported = row("i1", "device", "dev-a");
    imported.source_kind = "legacy_nv10_import".into();
    imported.source_ref = Some("nv10:r1-616162".into());
    db.insert_context_memory(&imported).unwrap();
    db.insert_context_memory(&row("i2", "device", "dev-a")).unwrap();
    db.insert_context_memory(&row("other", "device", "dev-b")).unwrap();

    assert!(db.context_source_seen("device", "dev-a", "nv10:r1-616162").unwrap());
    assert!(!db.context_source_seen("device", "dev-b", "nv10:r1-616162").unwrap());
    assert_eq!(db.clear_context_scope("device", "dev-a", 50.0).unwrap(), 2);
    assert_eq!(db.clear_context_scope("device", "dev-a", 51.0).unwrap(), 0, "重复清空不再改动");
    assert!(db.context_source_seen("device", "dev-a", "nv10:r1-616162").unwrap(), "删除后仍记得来源，不会被重新导入");
    assert_eq!(count(&db, "SELECT COUNT(*) FROM context_memories WHERE scope_id='dev-a' AND content<>''"), 0, "清空后不留内容");
    assert_eq!(db.list_context_memories("device", "dev-b", false, 100).unwrap().len(), 1, "其他范围不受影响");
    assert!(db.clear_context_scope("device", "dev-b", f64::INFINITY).is_err());
}

#[test]
fn nv19_extraction_manifests_are_json_arrays_scoped_and_revert_once() {
    let db = RuntimeDb::open_memory().unwrap();
    let run = |id: &str, scope: &str, started: f64| ContextExtractionRunRow {
        id: id.into(),
        scope_kind: "device".into(),
        scope_id: scope.into(),
        started_at: started,
        finished_at: Some(started + 1.0),
        adds_json: r#"[{"id":"ctx-1","revision":1}]"#.into(),
        updates_json: "[]".into(),
        deletes_json: "[]".into(),
        skipped_json: r#"[{"op":"update","reason":"op_not_allowed"}]"#.into(),
        reverted_at: None,
    };
    db.insert_context_extraction_run(&run("run-1", "dev-a", 1.0)).unwrap();
    db.insert_context_extraction_run(&run("run-2", "dev-a", 2.0)).unwrap();
    db.insert_context_extraction_run(&run("run-3", "dev-b", 3.0)).unwrap();
    assert!(db.insert_context_extraction_run(&run("run-1", "dev-a", 4.0)).is_err(), "id 唯一");
    for bad in ["{}", "not json", &format!("[\"{}\"]", "x".repeat(17 * 1024))] {
        let mut item = run("run-bad", "dev-a", 5.0);
        item.skipped_json = bad.to_string();
        assert!(db.insert_context_extraction_run(&item).is_err(), "清单必须是有界 JSON 数组");
    }
    let mut reverted = run("run-r", "dev-a", 6.0);
    reverted.reverted_at = Some(6.0);
    assert!(db.insert_context_extraction_run(&reverted).is_err(), "不能直接写入已撤销清单");
    let mut wrong_scope = run("run-s", "dev-a", 7.0);
    wrong_scope.scope_kind = "global".into();
    assert!(db.insert_context_extraction_run(&wrong_scope).is_err());

    let ids: Vec<_> = db.list_context_extraction_runs("device", "dev-a", 10).unwrap().into_iter().map(|r| r.id).collect();
    assert_eq!(ids, vec!["run-2", "run-1"], "只列本范围，新的在前");
    assert!(db.mark_context_extraction_reverted("run-1", 8.0).unwrap());
    assert!(!db.mark_context_extraction_reverted("run-1", 9.0).unwrap(), "只能撤销一次");
    assert!(!db.mark_context_extraction_reverted("missing", 9.0).unwrap());
    assert_eq!(db.context_extraction_run("run-1").unwrap().unwrap().reverted_at, Some(8.0));
}

#[test]
fn nv19_fts_index_follows_inserts_edits_deletes_and_filters_state() {
    let db = RuntimeDb::open_memory().unwrap();
    let mut a = row("a", "device", "dev-a");
    a.content = "边缘发黑时降低功率后改善".into();
    db.insert_context_memory(&a).unwrap();
    let mut b = row("b", "device", "dev-a");
    b.content = "喜欢先听到预计时长".into();
    db.insert_context_memory(&b).unwrap();
    let mut other = row("c", "device", "dev-b");
    other.content = "边缘发黑时降低功率后改善".into();
    db.insert_context_memory(&other).unwrap();
    let mut expired = row("d", "device", "dev-a");
    expired.content = "边缘发黑已过期的记录".into();
    expired.expires_at = Some(50.0);
    db.insert_context_memory(&expired).unwrap();

    let ids = |rows: Vec<ContextMemoryRow>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();
    let terms = |list: &[&str]| list.iter().map(|t| t.to_string()).collect::<Vec<_>>();
    assert_eq!(ids(db.match_context_memories("device", "dev-a", &terms(&["降低功率"]), 100.0, 10).unwrap()), vec!["a"], "只查本范围，过期的不返回");
    assert_eq!(ids(db.match_context_memories("device", "dev-a", &terms(&["边缘发", "预计时"]), 10.0, 10).unwrap()).len(), 3, "OR 组合，到期前仍可见");
    assert_eq!(ids(db.like_context_memories("device", "dev-a", &terms(&["功率"]), 100.0, 10).unwrap()), vec!["a"], "两字词走 LIKE");
    assert_eq!(ids(db.like_context_memories("device", "dev-a", &terms(&["镜片", "时长"]), 100.0, 10).unwrap()), vec!["b"], "多个短词按 OR 组合");
    assert!(db.like_context_memories("device", "dev-a", &terms(&["100%"]), 100.0, 10).unwrap().is_empty(), "LIKE 通配符被转义");
    assert!(db.like_context_memories("device", "dev-a", &terms(&["_"]), 100.0, 10).unwrap().is_empty(), "下划线不是通配符");
    assert!(db.like_context_memories("device", "dev-a", &[], 100.0, 10).is_err());
    assert!(db.match_context_memories("device", "dev-a", &terms(&["功率"]), 100.0, 10).is_err(), "少于三个字不能走 FTS");
    assert!(db.match_context_memories("device", "dev-a", &terms(&["\" OR content:*"]), 100.0, 10).unwrap().is_empty(), "检索词按短语引用，不能注入 FTS 语法");

    // Edit re-indexes; delete (tombstone) and invalidation drop the row from results.
    db.update_context_memory_if_revision("b", 1, &patch("喜欢详细的加工说明", "candidate"), 20.0).unwrap().unwrap();
    assert!(db.match_context_memories("device", "dev-a", &terms(&["预计时"]), 100.0, 10).unwrap().is_empty(), "旧内容不再命中");
    assert_eq!(ids(db.match_context_memories("device", "dev-a", &terms(&["加工说"]), 100.0, 10).unwrap()), vec!["b"]);
    db.update_context_memory_if_revision("b", 2, &patch("喜欢详细的加工说明", "invalidated"), 21.0).unwrap().unwrap();
    assert!(db.match_context_memories("device", "dev-a", &terms(&["加工说"]), 100.0, 10).unwrap().is_empty(), "作废的不返回");
    db.delete_context_memory_if_revision("a", 1, 30.0).unwrap();
    assert!(db.match_context_memories("device", "dev-a", &terms(&["降低功率"]), 100.0, 10).unwrap().is_empty());
    let indexed: i64 = db.connection.query_row("SELECT COUNT(*) FROM context_memories_fts WHERE context_memories_fts MATCH '\"降低功率\"'", [], |r| r.get(0)).unwrap();
    assert_eq!(indexed, 1, "删除后索引里只剩另一范围的那条");
}

mod knowledge {
    use laser_runtime::db::RuntimeDb;
    use laser_runtime::{KnowledgeDirRow, KnowledgeFileRow, KnowledgeRootRow};

    fn root(id: &str, scope: &str, path: &str) -> KnowledgeRootRow {
        KnowledgeRootRow { id: id.into(), scope_kind: "project".into(), scope_id: scope.into(), label: "说明书".into(), root_path: path.into(), revision: 1, created_at: 1.0, indexed_at: None }
    }
    fn dir(id: &str, root: &str, rel: &str, l0: &str) -> KnowledgeDirRow {
        KnowledgeDirRow { id: id.into(), root_id: root.into(), rel_dir: rel.into(), l0: l0.into(), l1: format!("{l0}。详细概览"), file_count: 1 }
    }
    fn file(id: &str, root: &str, dir: &str, rel: &str) -> KnowledgeFileRow {
        KnowledgeFileRow { id: id.into(), root_id: root.into(), dir_id: dir.into(), rel_path: rel.into(), sha256: "a".repeat(64), size: 10, mtime: 1.0 }
    }
    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn nv19_knowledge_index_replaces_atomically_and_deletes_completely() {
        let db = RuntimeDb::open_memory().unwrap();
        db.insert_knowledge_root(&root("r1", "p1", "D:/docs")).unwrap();
        db.insert_knowledge_root(&root("r2", "p2", "D:/other")).unwrap();
        assert!(db.insert_knowledge_root(&root("r3", "p1", "D:/docs")).is_err(), "同一范围同一目录只登记一次");
        let mut bad = root("r4", "p1", "D:/x");
        bad.scope_kind = "device".into();
        assert!(db.insert_knowledge_root(&bad).is_err(), "资料只属于项目或工作区");

        let dirs = vec![dir("d1", "r1", "", "激光对焦步骤与镜片清洁"), dir("d2", "r1", "维护", "皮带张紧与导轨润滑")];
        let files = vec![file("f1", "r1", "d1", "对焦.md"), file("f2", "r1", "d2", "维护/皮带.md")];
        assert!(db.replace_knowledge_index("r1", 1, &dirs, &files, 2.0).unwrap());
        assert!(!db.replace_knowledge_index("r1", 1, &dirs, &files, 3.0).unwrap(), "旧版本的重建失败");
        assert_eq!(db.knowledge_root("r1").unwrap().unwrap().revision, 2);
        db.replace_knowledge_index("r2", 1, &[dir("d9", "r2", "", "皮带张紧另一个项目")], &[file("f9", "r2", "d9", "a.md")], 2.0).unwrap();

        let hit = |terms: &[&str], shorts: &[&str]| db.search_knowledge_dirs("project", "p1", &strings(terms), &strings(shorts), 10).unwrap().into_iter().map(|d| d.id).collect::<Vec<_>>();
        assert_eq!(hit(&["皮带张"], &[]), vec!["d2"], "只查本范围");
        assert_eq!(hit(&["详细概"], &[]).len(), 2, "L1 也参与检索");
        assert_eq!(hit(&[], &["镜片"]), vec!["d1"]);
        assert!(db.search_knowledge_dirs("project", "p1", &strings(&["皮带"]), &[], 10).is_err());

        // Reindex after a file was removed: its directory summary disappears from search.
        assert!(db.replace_knowledge_index("r1", 2, &dirs[..1], &files[..1], 4.0).unwrap());
        assert!(hit(&["皮带张"], &[]).is_empty(), "删除的文件不再命中");
        assert_eq!(db.knowledge_files("d2").unwrap().len(), 0);
        assert!(db.replace_knowledge_index("r1", 3, &dirs[..1], &[file("fx", "r1", "missing", "x.md")], 5.0).is_err(), "文件必须属于本次的目录");

        assert!(!db.delete_knowledge_root_if_revision("r1", 1).unwrap());
        assert!(db.delete_knowledge_root_if_revision("r1", 3).unwrap());
        assert!(hit(&["对焦步"], &[]).is_empty() && db.knowledge_dirs("r1").unwrap().is_empty() && db.knowledge_files("d1").unwrap().is_empty());
        let indexed: i64 = db.connection.query_row("SELECT COUNT(*) FROM context_knowledge_fts WHERE context_knowledge_fts MATCH '\"对焦步\"'", [], |r| r.get(0)).unwrap();
        assert_eq!(indexed, 0, "全文索引一并清掉");
        assert_eq!(db.knowledge_roots("project", "p2").unwrap().len(), 1, "其他项目不受影响");
    }

    #[test]
    fn nv19_knowledge_summary_lengths_are_bounded() {
        let db = RuntimeDb::open_memory().unwrap();
        db.insert_knowledge_root(&root("r1", "p1", "D:/docs")).unwrap();
        let mut long = dir("d1", "r1", "", "x");
        long.l0 = "字".repeat(257);
        assert!(db.replace_knowledge_index("r1", 1, &[long], &[], 2.0).is_err(), "L0 最多 256 字");
        assert_eq!(db.knowledge_root("r1").unwrap().unwrap().revision, 1, "失败时整次回滚");
        let mut ok = dir("d1", "r1", "", &"字".repeat(256));
        ok.l1 = "字".repeat(4000);
        assert!(db.replace_knowledge_index("r1", 1, &[ok], &[], 2.0).unwrap());
    }
}
