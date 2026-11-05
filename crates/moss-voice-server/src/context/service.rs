//! NV19-02 trusted CRUD over long-term context. Every call re-checks the caller's scopes
//! with the host (`ContextStore::current`) and applies the NV19-01 contracts before the store
//! is touched. Management actions (edit, confirm, delete, clear, legacy import, export) are
//! private-window only; the voice channel may only create extraction candidates and read.
use super::{
    check_content, transition, AccessContext, Applicability, Category, Channel, ContextError, ContextMemory, Evidence, MemoryDraft,
    ScopeKey, ScopeKind, Source, SourceKind, TrustedScope, Verification, MAX_CONTENT_CHARS,
};
use super::retrieve::SearchQuery;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

/// Fields a user may change after creation. Scope, category and source are fixed.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryPatch {
    pub content: String,
    pub verification: Verification,
    pub applicability: Option<Applicability>,
    pub expires_at: Option<f64>,
}

/// One record added by an extraction run, with the revision it was created at.
#[derive(Debug, Clone, PartialEq)]
pub struct RunAdd {
    pub id: String,
    pub revision: i64,
}

/// A proposed operation that was not applied, with its reason code. No content is kept.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSkip {
    pub op: String,
    pub reason: String,
}

/// NV19-03 change manifest. Extraction only ever adds candidates, so `updates` and
/// `deletes` are always empty (stored as `[]`); ids and reason codes only, never content.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractionRun {
    pub id: String,
    pub scope: ScopeKey,
    pub started_at: f64,
    pub finished_at: Option<f64>,
    pub adds: Vec<RunAdd>,
    pub skipped: Vec<RunSkip>,
    pub reverted_at: Option<f64>,
}

impl ExtractionRun {
    pub fn adds_json(&self) -> String {
        serde_json::Value::Array(self.adds.iter().map(|a| serde_json::json!({"id": a.id, "revision": a.revision})).collect()).to_string()
    }

    pub fn skipped_json(&self) -> String {
        serde_json::Value::Array(self.skipped.iter().map(|s| serde_json::json!({"op": s.op, "reason": s.reason})).collect()).to_string()
    }

    /// Rebuild from stored JSON columns; anything unexpected fails closed.
    #[allow(clippy::too_many_arguments)]
    pub fn from_json(id: &str, scope: ScopeKey, started_at: f64, finished_at: Option<f64>, adds: &str, updates: &str, deletes: &str, skipped: &str, reverted_at: Option<f64>) -> Result<Self, String> {
        let bad = || "提取清单格式无效".to_owned();
        let array = |text: &str| serde_json::from_str::<Vec<serde_json::Value>>(text).map_err(|_| bad());
        if !array(updates)?.is_empty() || !array(deletes)?.is_empty() {
            return Err(bad());
        }
        let adds = array(adds)?
            .iter()
            .map(|v| Some(RunAdd { id: v.get("id")?.as_str()?.to_owned(), revision: v.get("revision")?.as_i64()? }))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(bad)?;
        let skipped = array(skipped)?
            .iter()
            .map(|v| Some(RunSkip { op: v.get("op")?.as_str()?.to_owned(), reason: v.get("reason")?.as_str()?.to_owned() }))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(bad)?;
        Ok(Self { id: id.into(), scope, started_at, finished_at, adds, skipped, reverted_at })
    }
}

/// Outcome of reverting a whole extraction run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RevertReport {
    pub removed: usize,
    /// Records the user already edited, confirmed or deleted; left as they are.
    pub kept: Vec<String>,
}

/// Implemented by the desktop host over `laser_runtime::RuntimeDb` (schema v10).
pub trait ContextStore: Send + Sync {
    /// Host re-verification: the device is still allowed with the same grant generation,
    /// the speaker template is unchanged, the desktop selection is still current.
    fn current(&self, scope: &TrustedScope) -> bool;
    fn insert(&self, memory: &ContextMemory) -> Result<ContextMemory, String>;
    fn get(&self, id: &str) -> Result<Option<ContextMemory>, String>;
    fn list(&self, scope: &ScopeKey, include_deleted: bool) -> Result<Vec<ContextMemory>, String>;
    /// Includes deleted tombstones, so deleted imports are not recreated.
    fn source_seen(&self, scope: &ScopeKey, source_ref: &str) -> Result<bool, String>;
    /// `None`: revision conflict, or the row was deleted/invalidated meanwhile.
    fn update(&self, id: &str, expected_revision: i64, patch: &MemoryPatch, now: f64) -> Result<Option<ContextMemory>, String>;
    fn delete(&self, id: &str, expected_revision: i64, now: f64) -> Result<bool, String>;
    fn clear(&self, scope: &ScopeKey, now: f64) -> Result<usize, String>;
    fn insert_run(&self, run: &ExtractionRun) -> Result<(), String>;
    fn get_run(&self, id: &str) -> Result<Option<ExtractionRun>, String>;
    fn list_runs(&self, scope: &ScopeKey, limit: usize) -> Result<Vec<ExtractionRun>, String>;
    /// One-shot; `false` if already reverted or missing.
    fn mark_run_reverted(&self, id: &str, now: f64) -> Result<bool, String>;
    /// NV19-04: live, non-invalidated, unexpired rows of one scope containing any of
    /// `query.terms` (FTS5 trigram, each 3 chars) or any of `query.shorts` (substring).
    fn search(&self, scope: &ScopeKey, query: &SearchQuery, now: f64, limit: usize) -> Result<Vec<ContextMemory>, String>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServiceError {
    Invalid(ContextError),
    /// Someone else changed the record first; reload and retry.
    Conflict,
    NotFound,
    /// The device was revoked, the speaker re-enrolled or the selection switched.
    ScopeStale,
    Store(String),
}

impl ServiceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(error) => error.code(),
            Self::Conflict => "context_conflict",
            Self::NotFound => "context_not_found",
            Self::ScopeStale => "context_scope_stale",
            Self::Store(_) => "context_store_failed",
        }
    }
}

impl From<ContextError> for ServiceError {
    fn from(error: ContextError) -> Self {
        Self::Invalid(error)
    }
}

/// Result of importing one NV10 summary. The original summary is never changed.
#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    pub created: Vec<ContextMemory>,
    /// Reason codes for skipped parts (e.g. `already_imported`, `context_content_sensitive`).
    pub skipped: Vec<&'static str>,
}

pub struct ContextService {
    store: Arc<dyn ContextStore>,
    /// Serializes read-check-write sequences within this process; the store's revision
    /// checks protect against everything else.
    gate: Mutex<()>,
}

fn new_id() -> Result<String, ServiceError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| ServiceError::Store("无法生成记忆编号".into()))?;
    Ok(format!("ctx-{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

/// Split at line boundaries into pieces of at most `MAX_CONTENT_CHARS`.
fn chunks(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let line: String = line.chars().take(MAX_CONTENT_CHARS).collect();
        if !current.is_empty() && current.chars().count() + 1 + line.chars().count() > MAX_CONTENT_CHARS {
            parts.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(&line);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

impl ContextService {
    pub fn new(store: Arc<dyn ContextStore>) -> Arc<Self> {
        Arc::new(Self { store, gate: Mutex::new(()) })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ()>, ServiceError> {
        self.gate.lock().map_err(|_| ServiceError::Store("记忆管理锁不可用".into()))
    }

    fn store<T>(result: Result<T, String>) -> Result<T, ServiceError> {
        result.map_err(ServiceError::Store)
    }

    /// The caller must hold the scope and the host must still vouch for it.
    fn verified<'a>(&self, access: &'a AccessContext, key: &ScopeKey) -> Result<&'a TrustedScope, ServiceError> {
        let scope = access.trusted(key).ok_or(ContextError::AccessDenied)?;
        if self.store.current(scope) { Ok(scope) } else { Err(ServiceError::ScopeStale) }
    }

    fn private_only(access: &AccessContext) -> Result<(), ServiceError> {
        if access.channel() == Channel::DesktopPrivate { Ok(()) } else { Err(ContextError::AccessDenied.into()) }
    }

    /// Load a live record the caller may manage, checking the expected revision.
    fn managed(&self, access: &AccessContext, id: &str, expected_revision: i64) -> Result<ContextMemory, ServiceError> {
        Self::private_only(access)?;
        let record = Self::store(self.store.get(id))?.ok_or(ServiceError::NotFound)?;
        if record.deleted_at.is_some() {
            return Err(ServiceError::NotFound);
        }
        self.verified(access, &record.scope)?;
        if record.revision != expected_revision {
            return Err(ServiceError::Conflict);
        }
        Ok(record)
    }

    pub fn create(&self, access: &AccessContext, draft: MemoryDraft, now: f64) -> Result<ContextMemory, ServiceError> {
        draft.validate(access, now)?;
        let _gate = self.lock()?;
        self.verified(access, &draft.scope)?;
        let memory = ContextMemory {
            id: new_id()?,
            scope: draft.scope,
            category: draft.category,
            content: draft.content,
            verification: draft.verification,
            source: draft.source,
            applicability: draft.applicability,
            revision: 1,
            created_at: now,
            updated_at: now,
            expires_at: draft.expires_at,
            deleted_at: None,
        };
        Self::store(self.store.insert(&memory))
    }

    /// Voice sees only active records; the private window also sees expired and invalidated
    /// ones so they can be reviewed. Deleted tombstones are never returned.
    pub fn list(&self, access: &AccessContext, scope: &ScopeKey, now: f64) -> Result<Vec<ContextMemory>, ServiceError> {
        self.verified(access, scope)?;
        let rows = Self::store(self.store.list(scope, false))?;
        Ok(rows
            .into_iter()
            .filter(|row| &row.scope == scope && row.deleted_at.is_none())
            .filter(|row| access.channel() == Channel::DesktopPrivate || row.readable(access, now))
            .collect())
    }

    /// The user rewrites content / applicability / expiry; verification is unchanged.
    pub fn edit(&self, access: &AccessContext, id: &str, expected_revision: i64, content: &str, applicability: Option<Applicability>, expires_at: Option<f64>, now: f64) -> Result<ContextMemory, ServiceError> {
        let _gate = self.lock()?;
        let record = self.managed(access, id, expected_revision)?;
        if record.verification == Verification::Invalidated {
            return Err(ContextError::TransitionInvalid.into());
        }
        check_content(content)?;
        if record.category.needs_applicability() && applicability.is_none() {
            return Err(ContextError::ApplicabilityRequired.into());
        }
        if expires_at.is_some_and(|at| !at.is_finite() || at <= now) {
            return Err(ContextError::ExpiryInvalid.into());
        }
        let patch = MemoryPatch { content: content.into(), verification: record.verification, applicability, expires_at };
        Self::store(self.store.update(id, expected_revision, &patch, now))?.ok_or(ServiceError::Conflict)
    }

    /// Confirm needs evidence; invalidation is always allowed and terminal.
    pub fn set_verification(&self, access: &AccessContext, id: &str, expected_revision: i64, to: Verification, evidence: Option<&Evidence>, now: f64) -> Result<ContextMemory, ServiceError> {
        let _gate = self.lock()?;
        let record = self.managed(access, id, expected_revision)?;
        transition(record.verification, to, evidence)?;
        let patch = MemoryPatch { content: record.content, verification: to, applicability: record.applicability, expires_at: record.expires_at };
        Self::store(self.store.update(id, expected_revision, &patch, now))?.ok_or(ServiceError::Conflict)
    }

    pub fn delete(&self, access: &AccessContext, id: &str, expected_revision: i64, now: f64) -> Result<(), ServiceError> {
        let _gate = self.lock()?;
        self.managed(access, id, expected_revision)?;
        if Self::store(self.store.delete(id, expected_revision, now))? { Ok(()) } else { Err(ServiceError::Conflict) }
    }

    /// Voice-channel conflict/correction superseding: soft-delete an older `Preference` record
    /// in the same verified scope when the user states an updated or corrected habit.
    pub(crate) fn supersede_voice_preference(&self, access: &AccessContext, id: &str, expected_revision: i64, now: f64) -> Result<bool, ServiceError> {
        if access.channel() != Channel::Voice {
            return Err(ContextError::AccessDenied.into());
        }
        let _gate = self.lock()?;
        let Some(record) = Self::store(self.store.get(id))? else { return Ok(false) };
        if record.deleted_at.is_some() || record.category != Category::Preference {
            return Ok(false);
        }
        self.verified(access, &record.scope)?;
        if record.revision != expected_revision {
            return Ok(false);
        }
        Self::store(self.store.delete(id, expected_revision, now))
    }

    /// Up to 2 active speaking-style / address preferences in the caller's active preference scope.
    pub fn style_preferences(&self, access: &AccessContext, now: f64) -> Vec<ContextMemory> {
        let target = if access.personal_mode() {
            access.scope_of(ScopeKind::Person)
        } else {
            access.scope_of(ScopeKind::Person).or_else(|| access.scope_of(ScopeKind::Device))
        };
        let Some(scope) = target else { return Vec::new() };
        let Ok(mut rows) = self.list(access, scope.key(), now) else { return Vec::new() };
        rows.retain(|r| r.category == Category::Preference && r.readable(access, now) && super::retrieve::is_global_style_preference(&r.content));
        rows.sort_by(|a, b| {
            (b.verification == Verification::Confirmed)
                .cmp(&(a.verification == Verification::Confirmed))
                .then_with(|| b.updated_at.total_cmp(&a.updated_at))
        });
        rows.truncate(2);
        rows
    }

    pub fn clear(&self, access: &AccessContext, scope: &ScopeKey, now: f64) -> Result<usize, ServiceError> {
        Self::private_only(access)?;
        let _gate = self.lock()?;
        self.verified(access, scope)?;
        Self::store(self.store.clear(scope, now))
    }

    /// Turn one NV10 per-device summary into `preference` candidates. Idempotent per
    /// (device, NV10 revision), including after the user deleted the imported rows.
    pub fn import_legacy(&self, access: &AccessContext, device_id: &str, summary: &str, nv10_revision: i64, now: f64) -> Result<ImportReport, ServiceError> {
        Self::private_only(access)?;
        let scope = ScopeKey::new(ScopeKind::Device, device_id)?;
        let digest: String = Sha256::digest(device_id.as_bytes()).iter().take(12).map(|b| format!("{b:02x}")).collect();
        let reference = format!("nv10:r{nv10_revision}-{digest}");
        let source = Source::new(SourceKind::LegacyNv10Import, Some(&reference))?;
        let _gate = self.lock()?;
        self.verified(access, &scope)?;
        let mut report = ImportReport::default();
        if nv10_revision < 1 || summary.trim().is_empty() {
            report.skipped.push("empty_summary");
            return Ok(report);
        }
        if Self::store(self.store.source_seen(&scope, &reference))? {
            report.skipped.push("already_imported");
            return Ok(report);
        }
        // Filter line by line first, so one sensitive line does not drop its neighbours.
        let mut kept = Vec::new();
        for line in summary.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let clipped: String = line.chars().take(MAX_CONTENT_CHARS).collect();
            match check_content(&clipped) {
                Ok(()) => kept.push(clipped),
                Err(error) => report.skipped.push(error.code()),
            }
        }
        for content in chunks(&kept.join("
")) {
            let draft = MemoryDraft {
                scope: scope.clone(),
                category: Category::Preference,
                content,
                verification: Verification::Candidate,
                source: source.clone(),
                applicability: None,
                expires_at: None,
            };
            match draft.validate(access, now) {
                Ok(()) => {
                    let memory = ContextMemory {
                        id: new_id()?,
                        scope: draft.scope,
                        category: draft.category,
                        content: draft.content,
                        verification: draft.verification,
                        source: draft.source,
                        applicability: None,
                        revision: 1,
                        created_at: now,
                        updated_at: now,
                        expires_at: None,
                        deleted_at: None,
                    };
                    report.created.push(Self::store(self.store.insert(&memory))?);
                }
                Err(error) => report.skipped.push(error.code()),
            }
        }
        Ok(report)
    }

    pub(crate) fn verify_for_search(&self, access: &AccessContext, key: &ScopeKey) -> Result<(), ServiceError> {
        self.verified(access, key).map(|_| ())
    }

    pub(crate) fn search_store(&self, key: &ScopeKey, query: &SearchQuery, now: f64) -> Result<Vec<ContextMemory>, ServiceError> {
        Self::store(self.store.search(key, query, now, super::retrieve::CANDIDATES_PER_SCOPE))
    }

    /// Manifests of one scope, newest first (private window).
    pub fn list_extractions(&self, access: &AccessContext, scope: &ScopeKey, limit: usize) -> Result<Vec<ExtractionRun>, ServiceError> {
        Self::private_only(access)?;
        self.verified(access, scope)?;
        Self::store(self.store.list_runs(scope, limit.min(200)))
    }

    /// Undo a whole extraction run: remove each added candidate that is still exactly as the
    /// run left it. Anything the user has since edited, confirmed or deleted is kept as is.
    pub fn revert_extraction(&self, access: &AccessContext, run_id: &str, now: f64) -> Result<RevertReport, ServiceError> {
        Self::private_only(access)?;
        let _gate = self.lock()?;
        let run = Self::store(self.store.get_run(run_id))?.ok_or(ServiceError::NotFound)?;
        self.verified(access, &run.scope)?;
        if run.reverted_at.is_some() || !Self::store(self.store.mark_run_reverted(run_id, now))? {
            return Err(ServiceError::Conflict);
        }
        let mut report = RevertReport::default();
        for add in &run.adds {
            let current = Self::store(self.store.get(&add.id))?;
            // Candidates may live in the device, person or project scope of the session; each
            // must still be held by this window and vouched for by the host.
            let untouched = current.as_ref().is_some_and(|row| row.deleted_at.is_none() && row.revision == add.revision && self.verified(access, &row.scope).is_ok());
            if untouched && Self::store(self.store.delete(&add.id, add.revision, now))? {
                report.removed += 1;
            } else {
                report.kept.push(add.id.clone());
            }
        }
        Ok(report)
    }

    /// Commit one extraction run from the voice channel: every candidate goes through
    /// `create` (contracts + host scope check); the manifest is written last.
    pub(crate) fn commit_extraction(&self, access: &AccessContext, run: &mut ExtractionRun, drafts: Vec<MemoryDraft>, now: f64) -> Result<(), ServiceError> {
        let mut stopped = None;
        for draft in drafts {
            if stopped.is_some() {
                run.skipped.push(RunSkip { op: "add".into(), reason: "run_stopped".into() });
                continue;
            }
            match self.create(access, draft, now) {
                Ok(row) => run.adds.push(RunAdd { id: row.id, revision: row.revision }),
                Err(ServiceError::Invalid(error)) => run.skipped.push(RunSkip { op: "add".into(), reason: error.code().into() }),
                // Scope went stale or the store failed mid-run: stop adding, but still write
                // the manifest so whatever was added stays revertible.
                Err(other) => {
                    run.skipped.push(RunSkip { op: "add".into(), reason: other.code().into() });
                    stopped = Some(other);
                }
            }
        }
        run.finished_at = Some(now);
        Self::store(self.store.insert_run(run))?;
        stopped.map_or(Ok(()), Err)
    }

    /// Private backup of one scope as plain JSON (no tombstones). Never sent to a model.
    pub fn export(&self, access: &AccessContext, scope: &ScopeKey, now: f64) -> Result<serde_json::Value, ServiceError> {
        Self::private_only(access)?;
        let rows = self.list(access, scope, now)?;
        Ok(serde_json::Value::Array(
            rows.iter()
                .map(|row| {
                    serde_json::json!({
                        "id": row.id,
                        "scope_kind": row.scope.kind().as_str(),
                        "scope_id": row.scope.id(),
                        "category": row.category.as_str(),
                        "content": row.content,
                        "verification": row.verification.as_str(),
                        "source_kind": row.source.kind().as_str(),
                        "source_ref": row.source.reference(),
                        "device_model": row.applicability.as_ref().map(Applicability::device_model),
                        "firmware": row.applicability.as_ref().and_then(Applicability::firmware),
                        "revision": row.revision,
                        "created_at": row.created_at,
                        "updated_at": row.updated_at,
                        "expires_at": row.expires_at,
                    })
                })
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests;
