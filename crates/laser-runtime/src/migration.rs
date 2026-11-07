use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::db::MaterialConflictPolicy;
use crate::store::{RecordKind, RuntimeStore, SqliteStore, StoredRecord};
use crate::{RuntimeDb, RuntimeRoot, SchemaError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    Keep,
    Overwrite,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationCounts {
    pub added: u64,
    pub skipped: u64,
    pub conflicts: u64,
    pub stale: u64,
    pub uploads: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationReport {
    pub dry_run: bool,
    pub counts: MigrationCounts,
    pub errors: Vec<String>,
    pub stale_paths: Vec<String>,
    pub material_source: Option<String>,
    pub tables: std::collections::BTreeMap<String, MigrationCounts>,
}

#[derive(Debug)]
pub enum MigrationError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Sql(rusqlite::Error),
    Schema(SchemaError),
    InvalidArgument(String),
}

impl From<std::io::Error> for MigrationError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<serde_json::Error> for MigrationError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
impl From<rusqlite::Error> for MigrationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}
impl From<SchemaError> for MigrationError {
    fn from(error: SchemaError) -> Self {
        Self::Schema(error)
    }
}

#[derive(Debug, Clone)]
pub struct MigrationOptions {
    pub source_root: PathBuf,
    pub source_roots: Vec<PathBuf>,
    pub target_root: PathBuf,
    pub historical_roots: Vec<PathBuf>,
    pub dry_run: bool,
    pub on_conflict: ConflictPolicy,
}

pub fn run_migration(
    options: &MigrationOptions,
) -> Result<(MigrationReport, PathBuf), MigrationError> {
    if !options.source_root.is_dir() {
        return Err(MigrationError::InvalidArgument(format!(
            "旧根目录不存在：{}",
            options.source_root.display()
        )));
    }
    let source_roots = if options.source_roots.is_empty() {
        vec![options.source_root.clone()]
    } else {
        options.source_roots.clone()
    };
    let source_canonical = std::fs::canonicalize(&options.source_root)?;
    let target_canonical = if options.target_root.exists() {
        std::fs::canonicalize(&options.target_root)?
    } else {
        options.target_root.clone()
    };
    if target_canonical == source_canonical || target_canonical.starts_with(&source_canonical) {
        return Err(MigrationError::InvalidArgument(
            "目标 RuntimeRoot 不得等于旧根或位于旧根内部".into(),
        ));
    }
    let mut report = MigrationReport {
        dry_run: options.dry_run,
        ..MigrationReport::default()
    };
    let material_path = source_roots
        .iter()
        .flat_map(|root| {
            [
                root.join(".runtime").join("lasergrbl_materials.json"),
                root.join(".lasergrbl_materials.json"),
            ]
        })
        .find(|path| path.is_file());
    let material_value: Option<Value> = if let Some(path) = &material_path {
        report.material_source = Some(path.display().to_string());
        let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        if let Err(error) = normalize_material_version(value.get("version")) {
            report.errors.push(format!("{}: {}", path.display(), error));
            None
        } else {
            Some(value)
        }
    } else {
        report.material_source = Some("preset_only".to_string());
        None
    };

    let mut upload_entries = Vec::new();
    for source_root in &source_roots {
        let mut upload_roots = vec![
            source_root.join(".runtime").join("uploads"),
            source_root.join(".runtime").join("laser_web_uploads"),
            source_root.join("laser_web_uploads"),
            source_root.join("out"),
        ];
        upload_roots.dedup();
        for root in upload_roots {
            upload_entries.extend(scan_uploads(&root, &mut report)?);
        }
    }

    let records = collect_records(&source_roots)?;
    let target_db_path = options.target_root.join("moss.sqlite");
    let target_read = if target_db_path.is_file() {
        Some(rusqlite::Connection::open_with_flags(
            &target_db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?)
    } else {
        None
    };
    let runtime = if !options.dry_run {
        Some(
            RuntimeRoot::resolve(Some(&options.target_root), None).map_err(|error| {
                MigrationError::InvalidArgument(format!("运行目录不可用：{error:?}"))
            })?,
        )
    } else {
        None
    };
    let db = if let Some(runtime) = runtime.as_ref() {
        Some(RuntimeDb::open(&runtime.database)?)
    } else {
        None
    };
    if let Some(db) = db.as_ref() {
        for (source, hash, size) in &upload_entries {
            let relative_name = upload_name(source, hash, &runtime.as_ref().unwrap().uploads)?;
            let destination = runtime.as_ref().unwrap().uploads.join(&relative_name);
            if !destination.exists() {
                std::fs::copy(source, &destination)?;
                report.counts.added += 1;
            } else {
                report.counts.skipped += 1;
            }
            let id = hex::encode(Sha256::digest(source.to_string_lossy().as_bytes()));
            db.connection.execute(
                "INSERT OR IGNORE INTO uploads(upload_id,path,sha256,size_bytes,created_at) VALUES(?1,?2,?3,?4,unixepoch())",
                rusqlite::params![id, relative_name, hash, *size as i64],
            )?;
        }
    }

    let mat_policy = match options.on_conflict {
        ConflictPolicy::Keep => MaterialConflictPolicy::Keep,
        ConflictPolicy::Overwrite => MaterialConflictPolicy::Overwrite,
    };
    if let Some(val) = &material_value {
        let now = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        if options.dry_run {
            let mem_db = RuntimeDb::open_memory()?;
            let counts = mem_db.import_material_json(val, mat_policy, now)?;
            let entry = report.tables.entry("materials".into()).or_default();
            entry.added = counts.added;
            entry.skipped = counts.skipped;
            entry.conflicts = counts.conflicts;
        } else if let Some(db) = db.as_ref() {
            let counts = db.import_material_json(val, mat_policy, now)?;
            let entry = report.tables.entry("materials".into()).or_default();
            entry.added = counts.added;
            entry.skipped = counts.skipped;
            entry.conflicts = counts.conflicts;
        }
    }
    let historical_roots = if options.historical_roots.is_empty() {
        source_roots.clone()
    } else {
        options.historical_roots.clone()
    };
    for (kind, mut record) in records {
        remap_record_paths(
            kind,
            &mut record.payload,
            &historical_roots,
            &options.target_root,
            &mut report,
        );
        let table_name = format!("{kind:?}").to_lowercase();
        let (action, changed) = existing_record_action(target_read.as_ref(), &record)?;
        let counts = report.tables.entry(table_name).or_default();
        match action.as_str() {
            "skip" => counts.skipped += 1,
            "conflict" => {
                counts.conflicts += 1;
                if options.on_conflict == ConflictPolicy::Keep {
                    continue;
                }
            }
            _ => counts.added += 1,
        }
        if !options.dry_run {
            if let Some(db) = db.as_ref() {
                let store = SqliteStore::new(db);
                store.put(&changed)?;
            }
        }
    }
    let report_path = options.target_root.join(format!(
        "migration-{}.json",
        chrono::Utc::now().format("%Y%m%d%H%M%S")
    ));
    std::fs::create_dir_all(&options.target_root)?;
    std::fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    Ok((report, report_path))
}

fn collect_records(
    source_roots: &[PathBuf],
) -> Result<Vec<(RecordKind, StoredRecord)>, MigrationError> {
    let mut result = Vec::new();
    for source_root in source_roots {
        let roots = [
            (
                RecordKind::Workflow,
                vec![source_root.join(".runtime/laser_workflows")],
            ),
            (
                RecordKind::TextTask,
                vec![
                    source_root.join(".runtime/lasergrbl_text_tasks"),
                    source_root.join(".lasergrbl_text_tasks"),
                ],
            ),
            (
                RecordKind::Job,
                vec![
                    source_root.join(".runtime/lasergrbl_jobs"),
                    source_root.join(".runtime/lasergrbl_network_jobs"),
                ],
            ),
            (
                RecordKind::Calibration,
                vec![source_root.join(".runtime/lasergrbl_calibrations")],
            ),
        ];
        for (kind, directories) in roots {
            for directory in directories {
                if !directory.is_dir() {
                    continue;
                }
                for entry in walk_json_files(&directory)? {
                    let payload: Value = serde_json::from_slice(&std::fs::read(&entry)?)?;
                    let id = record_id(kind, &payload, &entry);
                    let mut payload = payload;
                    normalize_old_job(kind, &mut payload);
                    result.push((
                        kind,
                        StoredRecord {
                            kind,
                            id,
                            status: payload
                                .get("status")
                                .and_then(Value::as_str)
                                .map(String::from),
                            payload,
                        },
                    ));
                }
            }
        }
    }
    Ok(result)
}

fn upload_name(
    source: &Path,
    hash: &str,
    destination_root: &Path,
) -> Result<String, MigrationError> {
    let name = source
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("upload");
    let candidate = destination_root.join(name);
    if !candidate.exists()
        || sha256_file(&candidate)
            .map(|value| value == hash)
            .unwrap_or(false)
    {
        return Ok(name.to_string());
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("upload");
    let extension = path
        .extension()
        .and_then(|v| v.to_str())
        .map(|v| format!(".{v}"))
        .unwrap_or_default();
    Ok(format!("{stem}--{}{extension}", &hash[..8.min(hash.len())]))
}

fn walk_json_files(root: &Path) -> Result<Vec<PathBuf>, MigrationError> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk_json_files(&path)?);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            files.push(path);
        }
    }
    Ok(files)
}

fn record_id(kind: RecordKind, payload: &Value, path: &Path) -> String {
    let key = match kind {
        RecordKind::Workflow => "workflow_id",
        RecordKind::TextTask => "task_id",
        RecordKind::Job => "job_id",
        RecordKind::Calibration => "calibration_id",
    };
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| path.file_stem().and_then(|v| v.to_str()).map(str::to_owned))
        .unwrap_or_else(|| hex::encode(Sha256::digest(path.to_string_lossy().as_bytes())))
}

fn normalize_old_job(kind: RecordKind, payload: &mut Value) {
    if kind != RecordKind::Job {
        return;
    }
    let status = payload.get("status").and_then(Value::as_str).unwrap_or("");
    if matches!(status, "pending" | "running" | "queued")
        || payload
            .get("expected_gcode_sha256")
            .and_then(Value::as_str)
            .is_none()
    {
        if let Some(object) = payload.as_object_mut() {
            object.insert("status".into(), Value::String("failed".into()));
            let result = object
                .entry("result")
                .or_insert_with(|| serde_json::json!({}));
            if let Some(result) = result.as_object_mut() {
                result.insert(
                    "detail".into(),
                    Value::String("旧系统未完成任务，迁移时安全终止".into()),
                );
            }
        }
    }
}

fn existing_record_action(
    connection: Option<&rusqlite::Connection>,
    record: &StoredRecord,
) -> Result<(String, StoredRecord), MigrationError> {
    let Some(connection) = connection else {
        return Ok(("add".into(), record.clone()));
    };
    let (table, key) = record.kind.table();
    let raw: Option<String> = connection
        .query_row(
            &format!("SELECT payload FROM {table} WHERE {key}=?1"),
            [&record.id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(("add".into(), record.clone()));
    };
    let existing: Value = serde_json::from_str(&raw)?;
    if existing == record.payload {
        Ok(("skip".into(), record.clone()))
    } else {
        Ok(("conflict".into(), record.clone()))
    }
}

pub fn normalize_material_version(value: Option<&Value>) -> Result<i64, String> {
    let Some(value) = value else {
        return Ok(1);
    };
    match value {
        Value::Number(number) if number.as_i64() == Some(1) => Ok(1),
        Value::Number(number) if number.as_f64() == Some(1.0) => Ok(1),
        Value::String(text) if text.trim() == "1" => Ok(1),
        _ => Err("材料库 version 无法归一化为 1".to_string()),
    }
}

pub fn remap_path(raw: &Path, historical_roots: &[PathBuf]) -> PathBuf {
    let mut mapped = raw.to_path_buf();
    for root in historical_roots {
        if let Ok(relative) = mapped.strip_prefix(root) {
            mapped = root.join(relative);
            break;
        }
    }
    for root in historical_roots {
        let old = root.join(".lasergrbl_text_tasks");
        if let Ok(relative) = mapped.strip_prefix(&old) {
            return root
                .join(".runtime")
                .join("lasergrbl_text_tasks")
                .join(relative);
        }
    }
    mapped
}

pub fn remap_path_to(raw: &Path, historical_roots: &[PathBuf], target_root: &Path) -> PathBuf {
    for root in historical_roots {
        if let Ok(relative) = raw.strip_prefix(root) {
            let candidate = target_root.join(relative);
            let old_layout = target_root.join(".lasergrbl_text_tasks");
            if let Ok(task_relative) = candidate.strip_prefix(&old_layout) {
                return target_root
                    .join(".runtime")
                    .join("lasergrbl_text_tasks")
                    .join(task_relative);
            }
            return candidate;
        }
    }
    raw.to_path_buf()
}

pub fn remap_record_paths(
    kind: RecordKind,
    payload: &mut Value,
    historical_roots: &[PathBuf],
    target_root: &Path,
    report: &mut MigrationReport,
) {
    let mut is_stale = false;
    let mut remap_field = |path_str: &str| -> (String, bool) {
        let raw_path = Path::new(path_str);
        let remapped_abs = remap_path_to(raw_path, historical_roots, target_root);
        let exists = remapped_abs.exists();
        let outside_root = remapped_abs.strip_prefix(target_root).is_err();
        let rel_path = if let Ok(rel) = remapped_abs.strip_prefix(target_root) {
            rel.to_string_lossy().replace('\\', "/")
        } else {
            // 路径脱敏：若跨盘符或无法相对化，绝不将外部宿主绝对路径落库，提取文件名脱敏
            let filename = remapped_abs
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown_file".to_string());
            format!("external/{filename}")
        };
        // RuntimeRoot 之外的文件运行时无法解析，一律视为 stale（db-schema §1 第 5 条）
        let stale = !exists || outside_root;
        if stale {
            report.stale_paths.push(rel_path.clone());
        }
        (rel_path, stale)
    };

    match kind {
        RecordKind::Workflow => {
            if let Some(artifacts) = payload.get_mut("artifacts").and_then(Value::as_object_mut) {
                for key in ["gcode_file", "image_file", "preview_file", "task_file"] {
                    if let Some(val) = artifacts.get_mut(key) {
                        if let Some(s) = val.as_str() {
                            if !s.is_empty() {
                                let (remapped, stale) = remap_field(s);
                                *val = Value::String(remapped);
                                if stale {
                                    is_stale = true;
                                }
                            }
                        }
                    }
                }
            }
        }
        RecordKind::TextTask => {
            if let Some(task_dir) = payload.get_mut("task_dir") {
                if let Some(s) = task_dir.as_str() {
                    if !s.is_empty() {
                        let (remapped, stale) = remap_field(s);
                        *task_dir = Value::String(remapped);
                        if stale {
                            is_stale = true;
                        }
                    }
                }
            }
            if let Some(attempts) = payload.get_mut("attempts").and_then(Value::as_array_mut) {
                for attempt in attempts {
                    if let Some(att_obj) = attempt.as_object_mut() {
                        for key in ["gcode_file", "image_file", "preview_file"] {
                            if let Some(val) = att_obj.get_mut(key) {
                                if let Some(s) = val.as_str() {
                                    if !s.is_empty() {
                                        let (remapped, stale) = remap_field(s);
                                        *val = Value::String(remapped);
                                        if stale {
                                            is_stale = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        RecordKind::Job => {
            if let Some(prepared) = payload.get_mut("prepared").and_then(Value::as_object_mut) {
                if let Some(val) = prepared.get_mut("gcode_file") {
                    if let Some(s) = val.as_str() {
                        if !s.is_empty() {
                            let (remapped, stale) = remap_field(s);
                            *val = Value::String(remapped);
                            if stale {
                                is_stale = true;
                            }
                        }
                    }
                }
            }
        }
        RecordKind::Calibration => {
            for key in ["gcode_file", "preview_file"] {
                if let Some(val) = payload.get_mut(key) {
                    if let Some(s) = val.as_str() {
                        if !s.is_empty() {
                            let (remapped, stale) = remap_field(s);
                            *val = Value::String(remapped);
                            if stale {
                                is_stale = true;
                            }
                        }
                    }
                }
            }
        }
    }

    if is_stale {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("stale".into(), Value::Bool(true));
        }
        report.counts.stale += 1;
    }
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let digest = Sha256::digest(bytes);
    Ok(hex::encode(digest))
}

pub fn scan_uploads(
    root: &Path,
    report: &mut MigrationReport,
) -> std::io::Result<Vec<(PathBuf, String, u64)>> {
    let mut entries = Vec::new();
    if !root.exists() {
        return Ok(entries);
    }
    for item in std::fs::read_dir(root)? {
        let item = item?;
        let path = item.path();
        if item.file_type()?.is_symlink() {
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let metadata = item.metadata()?;
        let hash = sha256_file(&path)?;
        entries.push((path, hash, metadata.len()));
        report.counts.uploads += 1;
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_normalization_is_strict_about_unknown_values() {
        assert_eq!(normalize_material_version(None).unwrap(), 1);
        assert_eq!(
            normalize_material_version(Some(&serde_json::json!(1))).unwrap(),
            1
        );
        assert_eq!(
            normalize_material_version(Some(&serde_json::json!(1.0))).unwrap(),
            1
        );
        assert_eq!(
            normalize_material_version(Some(&serde_json::json!(" 1 "))).unwrap(),
            1
        );
        assert!(normalize_material_version(Some(&serde_json::json!(2))).is_err());
        assert!(normalize_material_version(Some(&serde_json::json!("1.1"))).is_err());
    }

    #[test]
    fn old_task_layout_is_remapped_without_writing() {
        let root = PathBuf::from(r"C:\old");
        let source = root
            .join(".lasergrbl_text_tasks")
            .join("a")
            .join("task.json");
        let mapped = remap_path(&source, std::slice::from_ref(&root));
        assert_eq!(
            mapped,
            root.join(".runtime")
                .join("lasergrbl_text_tasks")
                .join("a")
                .join("task.json")
        );
    }

    #[test]
    fn remap_record_paths_marks_stale_and_rewrites_relative() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let old_gcode = source.path().join("existing.gcode");
        std::fs::write(&old_gcode, b"G0 X0").unwrap();

        // Create existing file in target at the remapped destination
        let target_gcode = target.path().join("existing.gcode");
        std::fs::write(&target_gcode, b"G0 X0").unwrap();

        let mut payload = serde_json::json!({
            "workflow_id": "wf_1",
            "artifacts": {
                "gcode_file": old_gcode.to_string_lossy().to_string(),
                "image_file": source.path().join("missing.png").to_string_lossy().to_string()
            }
        });

        let mut report = MigrationReport::default();
        remap_record_paths(
            RecordKind::Workflow,
            &mut payload,
            &[source.path().to_path_buf()],
            target.path(),
            &mut report,
        );

        assert_eq!(report.counts.stale, 1);
        assert_eq!(report.stale_paths.len(), 1);
        assert_eq!(payload["stale"], true);
        assert_eq!(payload["artifacts"]["gcode_file"], "existing.gcode");
        assert_eq!(payload["artifacts"]["image_file"], "missing.png");
    }

    #[test]
    fn remap_record_paths_never_leaks_absolute_paths_outside_target() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let foreign_gcode = foreign.path().join("secret-host-path.gcode");
        std::fs::write(&foreign_gcode, b"G0 X0").unwrap();
        let mut payload = serde_json::json!({
            "workflow_id": "wf_foreign",
            "artifacts": { "gcode_file": foreign_gcode.to_string_lossy().to_string() }
        });
        let mut report = MigrationReport::default();
        remap_record_paths(
            RecordKind::Workflow,
            &mut payload,
            &[source.path().to_path_buf()],
            target.path(),
            &mut report,
        );
        let rewritten = payload["artifacts"]["gcode_file"].as_str().unwrap();
        assert_eq!(rewritten, "external/secret-host-path.gcode");
        assert!(!std::path::Path::new(rewritten).is_absolute());
        assert!(
            !rewritten.contains(':'),
            "must not carry a drive letter or host prefix"
        );
        assert_eq!(
            payload["stale"], true,
            "files outside RuntimeRoot must be marked stale"
        );
        assert_eq!(report.counts.stale, 1);
        assert!(
            report
                .stale_paths
                .iter()
                .all(|p| !std::path::Path::new(p).is_absolute())
        );
    }

    #[test]
    fn upload_scan_reports_sha256() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("a.bin"), b"abc").unwrap();
        let mut report = MigrationReport::default();
        let entries = scan_uploads(temp.path(), &mut report).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].1,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(report.counts.uploads, 1);
    }

    #[test]
    fn migration_imports_records_and_is_idempotent() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let workflow_dir = source.path().join(".runtime/laser_workflows");
        let task_dir = source.path().join(".runtime/lasergrbl_text_tasks/task_1");
        let job_dir = source.path().join(".runtime/lasergrbl_jobs");
        let calibration_dir = source.path().join(".runtime/lasergrbl_calibrations");
        for path in [&workflow_dir, &task_dir, &job_dir, &calibration_dir] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(
            workflow_dir.join("wf_1.json"),
            serde_json::to_vec(&serde_json::json!({
                "workflow_id":"wf_1","status":"preview_ready","source_type":"image",
                "input":{"material":"椴木","thickness_mm":"3"},
                "created_at":1,"updated_at":2
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            task_dir.join("task.json"),
            serde_json::to_vec(&serde_json::json!({
                "task_id":"task_1","status":"prepared","text":"Hi","material":"椴木",
                "thickness_mm":"3","attempts":[],"created_at":1,"updated_at":2
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            job_dir.join("job_1.json"),
            serde_json::to_vec(&serde_json::json!({
                "job_id":"job_1","status":"completed","expected_gcode_sha256":"abc",
                "transport":"serial","created_at":1,"updated_at":2
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            job_dir.join("job_pending.json"),
            serde_json::to_vec(&serde_json::json!({
                "job_id":"job_pending","status":"pending",
                "transport":"serial","created_at":1,"updated_at":2
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            calibration_dir.join("cal_1.json"),
            serde_json::to_vec(&serde_json::json!({
                "calibration_id":"cal_1","status":"prepared","material":"椴木",
                "thickness_mm":"3","created_at":1,"updated_at":2
            }))
            .unwrap(),
        )
        .unwrap();
        let options = MigrationOptions {
            source_root: source.path().to_path_buf(),
            source_roots: vec![source.path().to_path_buf()],
            target_root: target.path().to_path_buf(),
            historical_roots: vec![source.path().to_path_buf()],
            dry_run: true,
            on_conflict: ConflictPolicy::Keep,
        };
        let (dry, _) = run_migration(&options).unwrap();
        assert_eq!(dry.counts.added, 0);
        assert!(!target.path().join("moss.sqlite").exists());
        let mut options = options;
        options.dry_run = false;
        let (first, _) = run_migration(&options).unwrap();
        assert_eq!(first.tables.values().map(|v| v.added).sum::<u64>(), 5);
        let (second, _) = run_migration(&options).unwrap();
        assert_eq!(second.tables.values().map(|v| v.skipped).sum::<u64>(), 5);
        let db = RuntimeDb::open(target.path().join("moss.sqlite")).unwrap();
        for (table, count) in [
            ("workflows", 1),
            ("text_tasks", 1),
            ("jobs", 2),
            ("calibrations", 1),
        ] {
            let actual: i64 = db
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(actual, count);
        }
        let pending_status: String = db
            .connection
            .query_row(
                "SELECT status FROM jobs WHERE job_id='job_pending'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending_status, "failed");
    }
}
