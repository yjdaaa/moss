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
    let (runtime, work_db) = if options.dry_run {
        (None, open_dry_run_db(&target_db_path)?)
    } else {
        let runtime = RuntimeRoot::resolve(Some(&options.target_root), None).map_err(|error| {
            MigrationError::InvalidArgument(format!("运行目录不可用：{error:?}"))
        })?;
        let db = RuntimeDb::open(&runtime.database)?;
        (
            Some(runtime),
            WorkingDb {
                db,
                _temp_dir: None,
            },
        )
    };
    if let Some(runtime) = runtime.as_ref() {
        for (source, hash, size) in &upload_entries {
            let relative_name = upload_name(source, hash, &runtime.uploads)?;
            let destination = runtime.uploads.join(&relative_name);
            if !destination.exists() {
                std::fs::copy(source, &destination)?;
                report.counts.added += 1;
            } else {
                report.counts.skipped += 1;
            }
            let id = hex::encode(Sha256::digest(source.to_string_lossy().as_bytes()));
            work_db.db.connection.execute(
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
        let counts = work_db.db.import_material_json(val, mat_policy, now)?;
        let entry = report.tables.entry("materials".into()).or_default();
        entry.added = counts.added;
        entry.skipped = counts.skipped;
        entry.conflicts = counts.conflicts;
    }
    let historical_roots = if options.historical_roots.is_empty() {
        source_roots.clone()
    } else {
        options.historical_roots.clone()
    };
    let store = SqliteStore::new(&work_db.db);
    for (kind, mut record) in records {
        remap_record_paths(
            kind,
            &mut record.payload,
            &historical_roots,
            &options.target_root,
            &mut report,
        );
        let table_name = format!("{kind:?}").to_lowercase();
        let action = existing_record_action(&work_db.db.connection, &record)?;
        let counts = report.tables.entry(table_name).or_default();
        match action {
            RecordAction::Skip => {
                counts.skipped += 1;
                continue;
            }
            RecordAction::Conflict => {
                counts.conflicts += 1;
                if options.on_conflict == ConflictPolicy::Keep {
                    continue;
                }
            }
            RecordAction::Add => counts.added += 1,
        }
        store.put(&record)?;
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
    files.sort();
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
    if (matches!(status, "pending" | "running" | "queued")
        || payload
            .get("expected_gcode_sha256")
            .and_then(Value::as_str)
            .is_none())
        && let Some(object) = payload.as_object_mut()
    {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordAction {
    Add,
    Skip,
    Conflict,
}

const SNAPSHOT_ROOT: &str = ".moss-migration-dryrun";
const SNAPSHOT_PREFIX: &str = "moss-migration-dryrun-";
const SNAPSHOT_MARKER: &str = ".moss-snapshot";

struct TempSnapshotDir {
    path: PathBuf,
    // 直到关闭快照并清理目录才释放，避免并行试跑误删活跃快照。
    _lock: std::fs::File,
}

fn private_dir_builder() -> std::fs::DirBuilder {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
}

impl TempSnapshotDir {
    fn create(base: &Path) -> Result<Self, MigrationError> {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        match private_dir_builder().create(base) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(base)?;
                if !metadata.is_dir() {
                    return Err(MigrationError::InvalidArgument("临时快照根目录无效".into()));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o077 != 0 {
                        return Err(MigrationError::InvalidArgument(
                            "临时快照根目录可被其他用户访问".into(),
                        ));
                    }
                }
            }
            Err(error) => return Err(error.into()),
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(base.join("cleanup.lock"))?;
        lock.lock()?;
        for entry in std::fs::read_dir(base)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(suffix) = name
                .to_str()
                .and_then(|name| name.strip_prefix(SNAPSHOT_PREFIX))
            else {
                continue;
            };
            let mut parts = suffix.split('-');
            if parts
                .by_ref()
                .take(3)
                .filter(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                .count()
                == 3
                && parts.next().is_none()
                && entry.file_type()?.is_dir()
                && std::fs::symlink_metadata(entry.path().join(SNAPSHOT_MARKER))
                    .is_ok_and(|meta| meta.is_file())
            {
                std::fs::remove_dir_all(entry.path())?;
            }
        }

        let pid = std::process::id();
        for _ in 0..32 {
            let seq = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let candidate = base.join(format!("{SNAPSHOT_PREFIX}{pid}-{nanos}-{seq}"));
            match private_dir_builder().create(&candidate) {
                Ok(()) => {
                    let temp_dir = Self {
                        path: candidate,
                        _lock: lock,
                    };
                    std::fs::File::create(temp_dir.path.join(SNAPSHOT_MARKER))?;
                    return Ok(temp_dir);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(MigrationError::Io(error)),
            }
        }
        Err(MigrationError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "无法创建临时快照目录",
        )))
    }
}

impl Drop for TempSnapshotDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct WorkingDb {
    // 必须先释放 db（关闭 SQLite 句柄），再清理临时快照目录。
    db: RuntimeDb,
    _temp_dir: Option<TempSnapshotDir>,
}

fn vacuum_readonly_snapshot(
    target_db_path: &Path,
    snapshot_path: &Path,
) -> Result<(), MigrationError> {
    let flags =
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let source = rusqlite::Connection::open_with_flags(target_db_path, flags)?;
    source.busy_timeout(std::time::Duration::from_secs(5))?;
    let snapshot_str = snapshot_path
        .to_str()
        .ok_or_else(|| MigrationError::InvalidArgument("临时快照路径包含非 UTF-8 字符".into()))?;
    source.execute("VACUUM INTO ?1", [snapshot_str])?;
    Ok(())
}

fn open_dry_run_db(target_db_path: &Path) -> Result<WorkingDb, MigrationError> {
    let base = target_db_path.with_file_name(SNAPSHOT_ROOT);
    if !target_db_path.is_file() {
        // 数据库被移走也要清掉上一轮崩溃遗留的快照。
        if base.exists() {
            TempSnapshotDir::create(&base)?;
        }
        return Ok(WorkingDb {
            db: RuntimeDb::open_memory()?,
            _temp_dir: None,
        });
    }
    let temp_dir = TempSnapshotDir::create(&base)?;
    let snapshot_path = temp_dir.path.join("snapshot.sqlite");
    vacuum_readonly_snapshot(target_db_path, &snapshot_path).map_err(|error| {
        MigrationError::InvalidArgument(format!(
            "无法通过只读 VACUUM INTO 获取目标库一致性快照（{}）：{error:?}",
            target_db_path.display()
        ))
    })?;
    let db = RuntimeDb::open(&snapshot_path)?;

    Ok(WorkingDb {
        db,
        _temp_dir: Some(temp_dir),
    })
}

fn existing_record_action(
    connection: &rusqlite::Connection,
    record: &StoredRecord,
) -> Result<RecordAction, MigrationError> {
    let (table, key) = record.kind.table();
    let raw: Option<String> = connection
        .query_row(
            &format!("SELECT payload FROM {table} WHERE {key}=?1"),
            [&record.id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(RecordAction::Add);
    };
    let existing: Value = serde_json::from_str(&raw)?;
    if existing == record.payload {
        Ok(RecordAction::Skip)
    } else {
        Ok(RecordAction::Conflict)
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

    // 字符串路径字段原地改写为 RuntimeRoot 相对路径；空串与非字符串保持原样
    let mut remap = |val: &mut Value| {
        if let Some(s) = val.as_str()
            && !s.is_empty()
        {
            let (remapped, stale) = remap_field(s);
            *val = Value::String(remapped);
            is_stale |= stale;
        }
    };
    match kind {
        RecordKind::Workflow => {
            if let Some(artifacts) = payload.get_mut("artifacts").and_then(Value::as_object_mut) {
                for key in ["gcode_file", "image_file", "preview_file", "task_file"] {
                    if let Some(val) = artifacts.get_mut(key) {
                        remap(val);
                    }
                }
            }
        }
        RecordKind::TextTask => {
            if let Some(val) = payload.get_mut("task_dir") {
                remap(val);
            }
            if let Some(attempts) = payload.get_mut("attempts").and_then(Value::as_array_mut) {
                for att_obj in attempts.iter_mut().filter_map(Value::as_object_mut) {
                    for key in ["gcode_file", "image_file", "preview_file"] {
                        if let Some(val) = att_obj.get_mut(key) {
                            remap(val);
                        }
                    }
                }
            }
        }
        RecordKind::Job => {
            if let Some(prepared) = payload.get_mut("prepared").and_then(Value::as_object_mut)
                && let Some(val) = prepared.get_mut("gcode_file")
            {
                remap(val);
            }
        }
        RecordKind::Calibration => {
            for key in ["gcode_file", "preview_file"] {
                if let Some(val) = payload.get_mut(key) {
                    remap(val);
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
    fn dry_run_keeps_snapshot_private_and_removes_orphans() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("moss.sqlite");
        let db = RuntimeDb::open(&target).unwrap();
        db.connection
            .execute("CREATE TABLE dry_run_probe (id INT)", [])
            .unwrap();
        db.connection
            .execute("INSERT INTO dry_run_probe VALUES (42)", [])
            .unwrap();
        let base = root.path().join(SNAPSHOT_ROOT);
        let stale = base.join("moss-migration-dryrun-1-2-3");
        private_dir_builder().create(&base).unwrap();
        std::fs::create_dir(&stale).unwrap();
        std::fs::File::create(stale.join(SNAPSHOT_MARKER)).unwrap();
        std::fs::write(stale.join("snapshot.sqlite"), b"stale snapshot").unwrap();
        let unrelated = base.join("moss-migration-dryrun-1-2-4");
        std::fs::create_dir(&unrelated).unwrap();

        let snapshot = open_dry_run_db(&target).unwrap();
        let path = snapshot._temp_dir.as_ref().unwrap().path.clone();
        assert_eq!(path.parent(), Some(base.as_path()));
        assert!(!stale.exists());
        assert!(unrelated.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for dir in [&base, &path] {
                assert_eq!(
                    std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
        }
        let value: i64 = snapshot
            .db
            .connection
            .query_row("SELECT id FROM dry_run_probe", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, 42);
        drop(snapshot);
        assert!(!path.exists());
    }

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

    #[test]
    fn same_batch_duplicate_records_respect_keep_and_overwrite_with_dry_run() {
        let source_a = tempfile::tempdir().unwrap();
        let source_b = tempfile::tempdir().unwrap();
        let wf_dir_a = source_a.path().join(".runtime/laser_workflows");
        let wf_dir_b = source_b.path().join(".runtime/laser_workflows");
        std::fs::create_dir_all(&wf_dir_a).unwrap();
        std::fs::create_dir_all(&wf_dir_b).unwrap();

        let write_wf = |dir: &Path, filename: &str, id: &str, status: &str, updated_at: i64| {
            std::fs::write(
                dir.join(filename),
                serde_json::to_vec(&serde_json::json!({
                    "workflow_id": id,
                    "status": status,
                    "source_type": "image",
                    "input": {"material": "椴木", "thickness_mm": "3"},
                    "created_at": 1,
                    "updated_at": updated_at
                }))
                .unwrap(),
            )
            .unwrap();
        };

        // 同批同目录内重复不同内容 + 跨 source_roots 重复相同内容
        write_wf(&wf_dir_a, "01_wf_diff.json", "wf_diff", "preview_ready", 10);
        write_wf(&wf_dir_a, "02_wf_diff.json", "wf_diff", "completed", 20);
        write_wf(&wf_dir_a, "wf_same.json", "wf_same", "preview_ready", 10);
        write_wf(&wf_dir_b, "wf_same.json", "wf_same", "preview_ready", 10);

        let target_keep = tempfile::tempdir().unwrap();
        let mut keep_opts = MigrationOptions {
            source_root: source_a.path().to_path_buf(),
            source_roots: vec![source_a.path().to_path_buf(), source_b.path().to_path_buf()],
            target_root: target_keep.path().to_path_buf(),
            historical_roots: vec![source_a.path().to_path_buf(), source_b.path().to_path_buf()],
            dry_run: true,
            on_conflict: ConflictPolicy::Keep,
        };

        let (dry_keep, _) = run_migration(&keep_opts).unwrap();
        assert!(!target_keep.path().join("moss.sqlite").exists());
        assert!(!target_keep.path().join("moss.lock").exists());
        assert_eq!(
            dry_keep.tables.get("workflow"),
            Some(&MigrationCounts {
                added: 2,
                skipped: 1,
                conflicts: 1,
                stale: 0,
                uploads: 0,
            })
        );

        keep_opts.dry_run = false;
        let (first_keep, _) = run_migration(&keep_opts).unwrap();
        assert_eq!(
            first_keep.tables.get("workflow"),
            dry_keep.tables.get("workflow")
        );

        let keep_db = RuntimeDb::open(target_keep.path().join("moss.sqlite")).unwrap();
        let kept_status: String = keep_db
            .connection
            .query_row(
                "SELECT status FROM workflows WHERE workflow_id='wf_diff'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kept_status, "preview_ready");
        drop(keep_db);

        let (second_keep, _) = run_migration(&keep_opts).unwrap();
        assert_eq!(
            second_keep.tables.get("workflow"),
            Some(&MigrationCounts {
                added: 0,
                skipped: 3,
                conflicts: 1,
                stale: 0,
                uploads: 0,
            })
        );

        let target_overwrite = tempfile::tempdir().unwrap();
        let mut overwrite_opts = MigrationOptions {
            target_root: target_overwrite.path().to_path_buf(),
            dry_run: true,
            on_conflict: ConflictPolicy::Overwrite,
            ..keep_opts.clone()
        };
        let (dry_overwrite, _) = run_migration(&overwrite_opts).unwrap();
        assert!(!target_overwrite.path().join("moss.sqlite").exists());
        assert_eq!(
            dry_overwrite.tables.get("workflow"),
            Some(&MigrationCounts {
                added: 2,
                skipped: 1,
                conflicts: 1,
                stale: 0,
                uploads: 0,
            })
        );

        overwrite_opts.dry_run = false;
        let (first_overwrite, _) = run_migration(&overwrite_opts).unwrap();
        assert_eq!(
            first_overwrite.tables.get("workflow"),
            dry_overwrite.tables.get("workflow")
        );
        let overwrite_db = RuntimeDb::open(target_overwrite.path().join("moss.sqlite")).unwrap();
        let overwritten_status: String = overwrite_db
            .connection
            .query_row(
                "SELECT status FROM workflows WHERE workflow_id='wf_diff'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(overwritten_status, "completed");
        drop(overwrite_db);

        let (second_overwrite, _) = run_migration(&overwrite_opts).unwrap();
        assert_eq!(
            second_overwrite.tables.get("workflow"),
            Some(&MigrationCounts {
                added: 0,
                skipped: 2,
                conflicts: 2,
                stale: 0,
                uploads: 0,
            })
        );
    }

    #[test]
    fn dry_run_uses_target_snapshot_with_wal_and_custom_materials_without_touching_target() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let runtime_src = source.path().join(".runtime");
        let wf_dir = runtime_src.join("laser_workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();

        let target_db_path = target.path().join("moss.sqlite");
        let target_db = RuntimeDb::open(&target_db_path).unwrap();
        // 禁止自动 checkpoint，确保后续写入留存在 WAL 中由试跑快照读取。
        target_db
            .connection
            .pragma_update(None, "wal_autocheckpoint", 0)
            .unwrap();
        target_db
            .import_material_json(
                &serde_json::json!({
                    "version": 1,
                    "materials": {
                        "自定义桦木": {
                            "aliases": [],
                            "thicknesses": {
                                "3": {
                                    "cut": {
                                        "laser_min_power": 0,
                                        "laser_max_power": 750,
                                        "feed_rate": 420,
                                        "travel_rate": 3000,
                                        "threshold": -1,
                                        "passes": 1,
                                        "pixel_size_mm": 0.1
                                    }
                                }
                            }
                        },
                        "椴木": {
                            "aliases": [],
                            "thicknesses": {
                                "3": {
                                    "cut": {
                                        "laser_min_power": 0,
                                        "laser_max_power": 910,
                                        "feed_rate": 310,
                                        "travel_rate": 3000,
                                        "threshold": -1,
                                        "passes": 1,
                                        "pixel_size_mm": 0.1
                                    }
                                }
                            }
                        }
                    }
                }),
                MaterialConflictPolicy::Overwrite,
                100.0,
            )
            .unwrap();
        let store = SqliteStore::new(&target_db);
        store
            .put(&StoredRecord {
                kind: RecordKind::Workflow,
                id: "wf_existing".into(),
                status: Some("completed".into()),
                payload: serde_json::json!({
                    "workflow_id": "wf_existing",
                    "status": "completed",
                    "source_type": "image",
                    "input": {"material": "自定义桦木", "thickness_mm": "3"},
                    "created_at": 1,
                    "updated_at": 5
                }),
            })
            .unwrap();

        let wal_len = target
            .path()
            .join("moss.sqlite-wal")
            .metadata()
            .unwrap()
            .len();
        assert!(wal_len > 0, "fixture 写入应保留在 WAL 中");

        std::fs::write(
            runtime_src.join("lasergrbl_materials.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "materials": {
                    "自定义桦木": {
                        "aliases": [],
                        "thicknesses": {
                            "3": {
                                "cut": {
                                    "laser_min_power": 0,
                                    "laser_max_power": 750,
                                    "feed_rate": 420,
                                    "travel_rate": 3000,
                                    "threshold": -1,
                                    "passes": 1,
                                    "pixel_size_mm": 0.1
                                }
                            }
                        }
                    },
                    "椴木": {
                        "aliases": [],
                        "thicknesses": {
                            "3": {
                                "cut": {
                                    "laser_min_power": 0,
                                    "laser_max_power": 800,
                                    "feed_rate": 300,
                                    "travel_rate": 3000,
                                    "threshold": -1,
                                    "passes": 1,
                                    "pixel_size_mm": 0.1
                                }
                            }
                        }
                    },
                    "胡桃木": {
                        "aliases": [],
                        "thicknesses": {
                            "2": {
                                "cut": {
                                    "laser_min_power": 0,
                                    "laser_max_power": 650,
                                    "feed_rate": 500,
                                    "travel_rate": 3000,
                                    "threshold": -1,
                                    "passes": 1,
                                    "pixel_size_mm": 0.1
                                }
                            }
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        std::fs::write(
            wf_dir.join("wf_existing.json"),
            serde_json::to_vec(&serde_json::json!({
                "workflow_id": "wf_existing",
                "status": "completed",
                "source_type": "image",
                "input": {"material": "自定义桦木", "thickness_mm": "3"},
                "created_at": 1,
                "updated_at": 5
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            wf_dir.join("01_wf_batch.json"),
            serde_json::to_vec(&serde_json::json!({
                "workflow_id": "wf_batch",
                "status": "preview_ready",
                "source_type": "image",
                "input": {"material": "胡桃木", "thickness_mm": "2"},
                "created_at": 2,
                "updated_at": 6
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            wf_dir.join("02_wf_batch.json"),
            serde_json::to_vec(&serde_json::json!({
                "workflow_id": "wf_batch",
                "status": "failed",
                "source_type": "image",
                "input": {"material": "胡桃木", "thickness_mm": "2"},
                "created_at": 2,
                "updated_at": 7
            }))
            .unwrap(),
        )
        .unwrap();

        let mut options = MigrationOptions {
            source_root: source.path().to_path_buf(),
            source_roots: vec![source.path().to_path_buf()],
            target_root: target.path().to_path_buf(),
            historical_roots: vec![source.path().to_path_buf()],
            dry_run: true,
            on_conflict: ConflictPolicy::Keep,
        };

        let (dry, _) = run_migration(&options).unwrap();
        assert_eq!(
            dry.tables.get("materials"),
            Some(&MigrationCounts {
                added: 1,
                skipped: 1,
                conflicts: 1,
                stale: 0,
                uploads: 0,
            })
        );
        assert_eq!(
            dry.tables.get("workflow"),
            Some(&MigrationCounts {
                added: 1,
                skipped: 1,
                conflicts: 1,
                stale: 0,
                uploads: 0,
            })
        );

        // dry-run 不得修改真实库内容或创建运行目录锁/子目录
        assert!(!target.path().join("moss.lock").exists());
        assert!(!target.path().join("uploads").exists());
        let walnut_count: i64 = target_db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM materials WHERE name='胡桃木'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(walnut_count, 0);
        let basswood_power: i32 = target_db
            .connection
            .query_row(
                "SELECT laser_max_power FROM material_params
                 WHERE material_name='椴木' AND thickness_key='3' AND laser_mode='cut' AND engraving_mode=''",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(basswood_power, 910);
        let batch_count: i64 = target_db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM workflows WHERE workflow_id='wf_batch'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(batch_count, 0);

        drop(target_db);

        let (dry_closed, _) = run_migration(&options).unwrap();
        assert_eq!(
            dry_closed.tables.get("materials"),
            dry.tables.get("materials")
        );
        assert_eq!(
            dry_closed.tables.get("workflow"),
            dry.tables.get("workflow")
        );

        options.dry_run = false;
        let (real, _) = run_migration(&options).unwrap();
        assert_eq!(real.tables.get("materials"), dry.tables.get("materials"));
        assert_eq!(real.tables.get("workflow"), dry.tables.get("workflow"));

        let verify_db = RuntimeDb::open(&target_db_path).unwrap();
        let walnut_after: i64 = verify_db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM materials WHERE name='胡桃木'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(walnut_after, 1);
        let basswood_after: i32 = verify_db
            .connection
            .query_row(
                "SELECT laser_max_power FROM material_params
                 WHERE material_name='椴木' AND thickness_key='3' AND laser_mode='cut' AND engraving_mode=''",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(basswood_after, 910);
        let batch_status: String = verify_db
            .connection
            .query_row(
                "SELECT status FROM workflows WHERE workflow_id='wf_batch'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(batch_status, "preview_ready");
    }

    #[test]
    fn dry_run_fails_explicitly_when_readonly_vacuum_snapshot_fails() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let bad_db = target.path().join("moss.sqlite");
        std::fs::write(&bad_db, b"not a valid sqlite database").unwrap();

        let options = MigrationOptions {
            source_root: source.path().to_path_buf(),
            source_roots: vec![source.path().to_path_buf()],
            target_root: target.path().to_path_buf(),
            historical_roots: vec![source.path().to_path_buf()],
            dry_run: true,
            on_conflict: ConflictPolicy::Keep,
        };
        let err = run_migration(&options).unwrap_err();
        match err {
            MigrationError::InvalidArgument(message) => {
                assert!(
                    message.contains("VACUUM INTO"),
                    "unexpected error: {message}"
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(&bad_db).unwrap(),
            b"not a valid sqlite database"
        );
    }
}
