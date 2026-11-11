//! NV19-08 desktop wiring for long-term context (内置语音技术书 §15, 实施细则 §11).
//!
//! Two commands, both only from the trusted MOSS main window (Tauri ACL in
//! `capabilities/voice-private-main.json` plus `require_private_settings_window`; the separate
//! packaged private window was removed on 2026-11-07, n67):
//! `voice_context_manage` dispatches on `action`; `voice_context_pick_folder` opens the folder
//! dialog and indexes the chosen folder for the current project (the path never returns to JS).
//! Retrieval and extraction are off until the user switches them on.

use serde_json::Value;

type CmdResult = Result<Value, Value>;

#[cfg(desktop)]
pub(crate) mod host {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use laser_runtime::{
        ContextExtractionRunRow, ContextMemoryPatch, ContextMemoryRow, KnowledgeChunkRow,
        KnowledgeDirRow, KnowledgeFileRow, KnowledgeRootRow, RuntimeDb,
    };
    use moss_voice::CenterError;
    use moss_voice::settings::BuiltinContext;
    use moss_voice_providers::Llm;
    use moss_voice_server::ToolContext;
    use moss_voice_server::context::experience::{Experience, format_experience};
    use moss_voice_server::context::extract::{ContextExtraction, ContextExtractor};
    use moss_voice_server::context::knowledge::{
        INDEPENDENT_KB_SCOPE_ID, KnowledgeChunk, KnowledgeChunkHit, KnowledgeDir, KnowledgeError,
        KnowledgeFile, KnowledgeRoot, KnowledgeService, KnowledgeStore, independent_kb_scope,
    };
    use moss_voice_server::context::resume::{
        RefKind, RefState, Reference, ResumeFacts, ResumeNote, format_resume,
    };
    use moss_voice_server::context::retrieve::{
        ContextAccessProvider, ContextRetrieval, ContextRetriever, SearchQuery, fast_query,
    };
    use moss_voice_server::context::service::{
        ContextService, ContextStore, ExtractionRun, MemoryPatch, ServiceError,
    };
    use moss_voice_server::context::{
        AccessContext, Applicability, Category, ContextError, ContextMemory, Evidence, MemoryDraft,
        RecordState, ScopeKey, ScopeKind, Source, SourceKind, StoredMemory, TrustedScope,
        Verification,
    };
    use moss_voice_server::person_context::PersonScope;
    use serde_json::{Value, json};

    fn now() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }

    fn err(error: impl std::fmt::Debug) -> String {
        format!("数据库操作失败：{error:?}")
    }

    /// Calibrated speaker directory query `(id, name, revision)`; returns empty when
    /// speaker calibration is absent or stale.
    pub(crate) type CalibratedSpeakersFn =
        Arc<dyn Fn() -> Vec<(String, String, i64)> + Send + Sync>;

    /// What the host vouches for right now: allowed devices (by grant generation),
    /// calibrated speakers (by speaker revision), and the current project (by switch generation).
    struct Trust {
        db: Arc<Mutex<RuntimeDb>>,
        settings_file: PathBuf,
        speakers: CalibratedSpeakersFn,
    }

    impl Trust {
        fn context(&self) -> BuiltinContext {
            moss_voice::settings::load(&self.settings_file).builtin_context
        }

        fn device_generation(&self, device_id: &str) -> Option<u64> {
            let db = self.db.lock().ok()?;
            let row = db.voice_device(device_id).ok()??;
            row.is_allowed()
                .then_some(row.decided_at?.to_bits())
                .filter(|g| *g != 0)
        }

        fn current(&self, scope: &TrustedScope) -> bool {
            let key = scope.key();
            match key.kind() {
                ScopeKind::Device => self.device_generation(key.id()) == Some(scope.generation()),
                ScopeKind::Project => {
                    let context = self.context();
                    !context.project_id.is_empty()
                        && context.project_id == key.id()
                        && context.project_generation == scope.generation()
                }
                ScopeKind::Person => self
                    .person_scope(key.id())
                    .is_some_and(|live| live.generation() == scope.generation()),
                ScopeKind::Workspace => {
                    key.id() == INDEPENDENT_KB_SCOPE_ID && scope.generation() == 1
                }
            }
        }

        fn person_scope(&self, person_id: &str) -> Option<TrustedScope> {
            let (_, _, revision) = (self.speakers)()
                .into_iter()
                .find(|(id, _, rev)| id == person_id && *rev > 0)?;
            let candidate = PersonScope::candidate(person_id, revision, 1)?;
            TrustedScope::calibrated_person(&candidate).ok()
        }

        fn voice_person(&self, person: Option<&PersonScope>) -> Option<TrustedScope> {
            if !moss_voice::settings::load(&self.settings_file).builtin_person_memory_enabled {
                return None;
            }
            let scope = person?;
            let matched = (self.speakers)()
                .into_iter()
                .any(|(id, _, rev)| id == scope.id() && rev == scope.revision());
            matched
                .then(|| TrustedScope::calibrated_person(scope).ok())
                .flatten()
        }

        fn project(&self) -> Option<TrustedScope> {
            let context = self.context();
            (!context.project_id.is_empty())
                .then(|| {
                    TrustedScope::desktop_selection(
                        ScopeKind::Project,
                        &context.project_id,
                        context.project_generation,
                    )
                    .ok()
                })
                .flatten()
        }

        fn workspace_kb(&self) -> Option<TrustedScope> {
            TrustedScope::desktop_selection(ScopeKind::Workspace, INDEPENDENT_KB_SCOPE_ID, 1).ok()
        }

        fn device(&self, device_id: &str) -> Option<TrustedScope> {
            TrustedScope::device_grant(device_id, self.device_generation(device_id)?).ok()
        }

        /// Recorded model and firmware of a device; both must be known.
        fn applicability(&self, device_id: &str) -> Option<Applicability> {
            let db = self.db.lock().ok()?;
            let row = db.voice_device(device_id).ok()??;
            Applicability::new(&row.model, Some(&row.firmware_version)).ok()
        }

        /// Private window: every allowed device, calibrated speakers, plus the current project.
        fn owner(&self) -> AccessContext {
            let ids: Vec<String> = self
                .db
                .lock()
                .ok()
                .and_then(|db| db.voice_devices().ok())
                .unwrap_or_default()
                .into_iter()
                .filter(|d| d.is_allowed())
                .map(|d| d.device_id)
                .collect();
            let mut scopes: Vec<TrustedScope> =
                ids.iter().filter_map(|id| self.device(id)).collect();
            scopes.extend(
                (self.speakers)()
                    .into_iter()
                    .filter_map(|(id, _, _)| self.person_scope(&id)),
            );
            scopes.extend(self.project());
            scopes.extend(self.workspace_kb());
            AccessContext::desktop_private(scopes)
        }
    }

    // ---------- stores over schema v12 ----------

    struct DbContextStore(Arc<Trust>);

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
            supersedes: row.supersedes.as_deref(),
            seen_count: row.seen_count,
            last_seen_at: row.last_seen_at,
        })
        .map_err(|e| e.code().to_owned())
    }

    fn decode_run(row: ContextExtractionRunRow) -> Result<ExtractionRun, String> {
        let scope =
            ScopeKey::parse(&row.scope_kind, &row.scope_id).map_err(|e| e.code().to_owned())?;
        ExtractionRun::from_json(
            &row.id,
            scope,
            row.started_at,
            row.finished_at,
            &row.adds_json,
            &row.updates_json,
            &row.deletes_json,
            &row.skipped_json,
            row.reverted_at,
        )
    }

    impl DbContextStore {
        fn db(&self) -> Result<std::sync::MutexGuard<'_, RuntimeDb>, String> {
            self.0.db.lock().map_err(|_| "数据库锁失败".to_owned())
        }
    }

    impl ContextStore for DbContextStore {
        fn current(&self, scope: &TrustedScope) -> bool {
            self.0.current(scope)
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
                firmware: m
                    .applicability
                    .as_ref()
                    .and_then(|a| a.firmware())
                    .map(Into::into),
                revision: m.revision,
                created_at: m.created_at,
                updated_at: m.updated_at,
                expires_at: m.expires_at,
                deleted_at: m.deleted_at,
                supersedes: m.supersedes.clone(),
                seen_count: 1,
                last_seen_at: None,
            };
            decode(self.db()?.insert_context_memory(&row).map_err(err)?)
        }
        fn get(&self, id: &str) -> Result<Option<ContextMemory>, String> {
            self.db()?
                .context_memory(id)
                .map_err(err)?
                .map(decode)
                .transpose()
        }
        fn list(
            &self,
            scope: &ScopeKey,
            include_deleted: bool,
        ) -> Result<Vec<ContextMemory>, String> {
            self.db()?
                .list_context_memories(scope.kind().as_str(), scope.id(), include_deleted, 500)
                .map_err(err)?
                .into_iter()
                .map(decode)
                .collect()
        }
        fn source_seen(&self, scope: &ScopeKey, source_ref: &str) -> Result<bool, String> {
            self.db()?
                .context_source_seen(scope.kind().as_str(), scope.id(), source_ref)
                .map_err(err)
        }
        fn update(
            &self,
            id: &str,
            expected: i64,
            patch: &MemoryPatch,
            now: f64,
        ) -> Result<Option<ContextMemory>, String> {
            let row = ContextMemoryPatch {
                content: patch.content.clone(),
                verification: patch.verification.as_str().into(),
                device_model: patch
                    .applicability
                    .as_ref()
                    .map(|a| a.device_model().into()),
                firmware: patch
                    .applicability
                    .as_ref()
                    .and_then(|a| a.firmware())
                    .map(Into::into),
                expires_at: patch.expires_at,
            };
            self.db()?
                .update_context_memory_if_revision(id, expected, &row, now)
                .map_err(err)?
                .map(decode)
                .transpose()
        }
        fn delete(&self, id: &str, expected: i64, now: f64) -> Result<bool, String> {
            self.db()?
                .delete_context_memory_if_revision(id, expected, now)
                .map_err(err)
        }
        fn reinforce(&self, id: &str, now: f64, extend_to: Option<f64>) -> Result<bool, String> {
            self.db()?
                .reinforce_context_memory(id, now, extend_to)
                .map_err(err)
        }
        fn clear(&self, scope: &ScopeKey, now: f64) -> Result<usize, String> {
            self.db()?
                .clear_context_scope(scope.kind().as_str(), scope.id(), now)
                .map_err(err)
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
            self.db()?.insert_context_extraction_run(&row).map_err(err)
        }
        fn get_run(&self, id: &str) -> Result<Option<ExtractionRun>, String> {
            self.db()?
                .context_extraction_run(id)
                .map_err(err)?
                .map(decode_run)
                .transpose()
        }
        fn list_runs(&self, scope: &ScopeKey, limit: usize) -> Result<Vec<ExtractionRun>, String> {
            self.db()?
                .list_context_extraction_runs(scope.kind().as_str(), scope.id(), limit)
                .map_err(err)?
                .into_iter()
                .map(decode_run)
                .collect()
        }
        fn mark_run_reverted(&self, id: &str, now: f64) -> Result<bool, String> {
            self.db()?
                .mark_context_extraction_reverted(id, now)
                .map_err(err)
        }
        fn search(
            &self,
            scope: &ScopeKey,
            query: &SearchQuery,
            now: f64,
            limit: usize,
        ) -> Result<Vec<ContextMemory>, String> {
            let db = self.db()?;
            let mut rows = Vec::new();
            if !query.terms.is_empty() {
                rows.extend(
                    db.match_context_memories(
                        scope.kind().as_str(),
                        scope.id(),
                        &query.terms,
                        now,
                        limit,
                    )
                    .map_err(err)?,
                );
            }
            if !query.shorts.is_empty() {
                for row in db
                    .like_context_memories(
                        scope.kind().as_str(),
                        scope.id(),
                        &query.shorts,
                        now,
                        limit,
                    )
                    .map_err(err)?
                {
                    if !rows.iter().any(|seen: &ContextMemoryRow| seen.id == row.id) {
                        rows.push(row);
                    }
                }
            }
            rows.into_iter().map(decode).collect()
        }
    }

    struct DbKnowledgeStore(Arc<Trust>);

    fn root_from(row: KnowledgeRootRow) -> Result<KnowledgeRoot, String> {
        Ok(KnowledgeRoot {
            scope: ScopeKey::parse(&row.scope_kind, &row.scope_id)
                .map_err(|e| e.code().to_owned())?,
            id: row.id,
            label: row.label,
            root_path: PathBuf::from(row.root_path),
            revision: row.revision,
            indexed_at: row.indexed_at,
            enabled: row.enabled,
        })
    }

    impl KnowledgeStore for DbKnowledgeStore {
        fn current(&self, scope: &TrustedScope) -> bool {
            self.0.current(scope)
        }
        fn insert_root(&self, root: &KnowledgeRoot, now: f64) -> Result<(), String> {
            let row = KnowledgeRootRow {
                id: root.id.clone(),
                scope_kind: root.scope.kind().as_str().into(),
                scope_id: root.scope.id().into(),
                label: root.label.clone(),
                root_path: root
                    .root_path
                    .to_str()
                    .ok_or("资料目录路径包含不支持的字符")?
                    .into(),
                revision: 1,
                created_at: now,
                indexed_at: None,
                enabled: root.enabled,
            };
            self.0
                .db
                .lock()
                .map_err(err)?
                .insert_knowledge_root(&row)
                .map_err(err)
        }
        fn root(&self, id: &str) -> Result<Option<KnowledgeRoot>, String> {
            self.0
                .db
                .lock()
                .map_err(err)?
                .knowledge_root(id)
                .map_err(err)?
                .map(root_from)
                .transpose()
        }
        fn roots(&self, scope: &ScopeKey) -> Result<Vec<KnowledgeRoot>, String> {
            self.0
                .db
                .lock()
                .map_err(err)?
                .knowledge_roots(scope.kind().as_str(), scope.id())
                .map_err(err)?
                .into_iter()
                .map(root_from)
                .collect()
        }
        fn all_roots(&self) -> Result<Vec<KnowledgeRoot>, String> {
            self.0
                .db
                .lock()
                .map_err(err)?
                .all_knowledge_roots()
                .map_err(err)?
                .into_iter()
                .map(root_from)
                .collect()
        }
        fn set_root_enabled(&self, id: &str, expected: i64, enabled: bool) -> Result<bool, String> {
            self.0
                .db
                .lock()
                .map_err(err)?
                .set_knowledge_root_enabled(id, expected, enabled)
                .map_err(err)
        }
        fn replace(
            &self,
            root_id: &str,
            expected: i64,
            dirs: &[KnowledgeDir],
            files: &[KnowledgeFile],
            chunks: &[KnowledgeChunk],
            now: f64,
        ) -> Result<bool, String> {
            let dirs: Vec<_> = dirs
                .iter()
                .map(|d| KnowledgeDirRow {
                    id: d.id.clone(),
                    root_id: d.root_id.clone(),
                    rel_dir: d.rel_dir.clone(),
                    l0: d.l0.clone(),
                    l1: d.l1.clone(),
                    file_count: d.file_count,
                })
                .collect();
            let files: Vec<_> = files
                .iter()
                .map(|f| KnowledgeFileRow {
                    id: f.id.clone(),
                    root_id: f.root_id.clone(),
                    dir_id: f.dir_id.clone(),
                    rel_path: f.rel_path.clone(),
                    sha256: f.sha256.clone(),
                    size: f.size,
                    mtime: f.mtime,
                })
                .collect();
            let chunks: Vec<_> = chunks
                .iter()
                .map(|c| KnowledgeChunkRow {
                    file_id: c.file_id.clone(),
                    ord: c.ord,
                    text: c.text.clone(),
                })
                .collect();
            self.0
                .db
                .lock()
                .map_err(err)?
                .replace_knowledge_index_with_chunks(root_id, expected, &dirs, &files, &chunks, now)
                .map_err(err)
        }
        fn delete_root(&self, id: &str, expected: i64) -> Result<bool, String> {
            self.0
                .db
                .lock()
                .map_err(err)?
                .delete_knowledge_root_if_revision(id, expected)
                .map_err(err)
        }
        fn search_chunks(
            &self,
            scope: &ScopeKey,
            query: &SearchQuery,
            limit: usize,
        ) -> Result<Vec<KnowledgeChunkHit>, String> {
            Ok(self
                .0
                .db
                .lock()
                .map_err(err)?
                .search_knowledge_chunks(scope.kind().as_str(), scope.id(), &query.terms, limit)
                .map_err(err)?
                .into_iter()
                .map(|h| KnowledgeChunkHit {
                    dir: KnowledgeDir {
                        id: h.dir.id,
                        root_id: h.dir.root_id,
                        rel_dir: h.dir.rel_dir,
                        l0: h.dir.l0,
                        l1: h.dir.l1,
                        file_count: h.dir.file_count,
                    },
                    file_id: h.file_id,
                    ord: h.ord,
                })
                .collect())
        }
        fn search_dirs(
            &self,
            scope: &ScopeKey,
            query: &SearchQuery,
            limit: usize,
        ) -> Result<Vec<KnowledgeDir>, String> {
            Ok(self
                .0
                .db
                .lock()
                .map_err(err)?
                .search_knowledge_dirs(
                    scope.kind().as_str(),
                    scope.id(),
                    &query.terms,
                    &query.shorts,
                    limit,
                )
                .map_err(err)?
                .into_iter()
                .map(|d| KnowledgeDir {
                    id: d.id,
                    root_id: d.root_id,
                    rel_dir: d.rel_dir,
                    l0: d.l0,
                    l1: d.l1,
                    file_count: d.file_count,
                })
                .collect())
        }
        fn files(&self, dir_id: &str) -> Result<Vec<KnowledgeFile>, String> {
            Ok(self
                .0
                .db
                .lock()
                .map_err(err)?
                .knowledge_files(dir_id)
                .map_err(err)?
                .into_iter()
                .map(|f| KnowledgeFile {
                    id: f.id,
                    root_id: f.root_id,
                    dir_id: f.dir_id,
                    rel_path: f.rel_path,
                    sha256: f.sha256,
                    size: f.size,
                    mtime: f.mtime,
                })
                .collect())
        }
    }

    /// Read-only SELECTs over approvals / workflows & calibrations (with jobs fallback) / AgentRun records (NV19-07 mapping).
    struct DbResumeFacts(Arc<Trust>);

    pub(crate) fn query_job_ref_state(
        db: &RuntimeDb,
        id: &str,
    ) -> Option<(String, Option<String>)> {
        let row = |sql: &str| -> Option<(String, Option<String>)> {
            db.connection
                .query_row(sql, [id], |r| Ok((r.get(0)?, r.get(1)?)))
                .ok()
        };
        row("SELECT status, json_extract(payload,'$.send_result.phase') FROM workflows WHERE workflow_id=?1")
            .or_else(|| {
                row("SELECT status, json_extract(payload,'$.send_result.phase') FROM calibrations WHERE calibration_id=?1")
            })
            .or_else(|| row("SELECT status, NULL FROM jobs WHERE job_id=?1"))
    }

    impl ResumeFacts for DbResumeFacts {
        fn state(&self, reference: &Reference) -> RefState {
            let Ok(db) = self.0.db.lock() else {
                return RefState::Unverifiable;
            };
            let row = |sql: &str| -> Option<(String, Option<String>)> {
                db.connection
                    .query_row(sql, [&reference.id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .ok()
            };
            let found = match reference.kind {
                RefKind::Approval => row(
                    "SELECT status, json_extract(outcome,'$.phase') FROM approvals WHERE approval_id=?1",
                ),
                RefKind::Job => query_job_ref_state(&db, &reference.id),
                RefKind::AgentRun => row("SELECT status, NULL FROM voice_agent_runs WHERE id=?1"),
                RefKind::Test => return RefState::Unverifiable,
            };
            let Some((status, phase)) = found else {
                return RefState::Missing;
            };
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
                (
                    _,
                    "running" | "pending" | "queued" | "sending" | "processing" | "preview_ready"
                    | "prepared",
                    _,
                ) => RefState::InProgress,
                _ => RefState::Unverifiable,
            }
        }
    }

    /// Voice turns: the allowed device, the current project and the device's model/firmware.
    struct VoiceAccess(Arc<Trust>);

    impl ContextAccessProvider for VoiceAccess {
        fn access(
            &self,
            context: &ToolContext,
            person: Option<&PersonScope>,
        ) -> Option<(AccessContext, Option<Applicability>)> {
            let device = self.0.device(&context.device_id)?;
            let personal_mode =
                moss_voice::settings::load(&self.0.settings_file).builtin_person_memory_enabled;
            let mut access =
                AccessContext::voice(device, self.0.voice_person(person), self.0.project())
                    .ok()?
                    .with_personal_mode(personal_mode);
            if let Some(kb) = self.0.workspace_kb() {
                access = access.with_workspace(kb).ok()?;
            }
            Some((access, self.0.applicability(&context.device_id)))
        }
    }

    // ---------- host ----------

    pub(crate) struct ContextHost {
        trust: Arc<Trust>,
        store: Arc<DbContextStore>,
        pub(crate) service: Arc<ContextService>,
        pub(crate) knowledge: Arc<KnowledgeService>,
        pub(crate) extractor: Arc<ContextExtractor>,
        pub(crate) retriever: Arc<ContextRetriever>,
        settings_lock: Mutex<()>,
    }

    fn fail(code: &'static str, status: u16, message: &str) -> CenterError {
        CenterError::new(code, status, message)
    }

    fn context_message(error: ContextError) -> &'static str {
        match error {
            ContextError::ScopeInvalid => "记忆范围无效",
            ContextError::CategoryNotInScope => "这类记忆不能放在所选范围",
            ContextError::AccessDenied => "没有这个范围的权限（设备未允许或项目未选中）",
            ContextError::SourceNotAllowed => "这种来源不能写这类记忆",
            ContextError::VerificationNotAllowed => "这条记忆不能直接标为已确认",
            ContextError::ContentInvalid => "内容为空、过长或格式不对",
            ContextError::ContentSensitive => "内容含密钥、网址、审批或机器指令，不能保存",
            ContextError::SourceRefInvalid => "来源引用无效",
            ContextError::ApplicabilityRequired => "需要写明机型和固件版本",
            ContextError::ApplicabilityInvalid => "机型或固件版本无效",
            ContextError::ExpiryInvalid => "到期时间无效",
            ContextError::TransitionInvalid => "状态不能这样改（确认需要证据，作废后不能恢复）",
        }
    }

    fn service_error(error: ServiceError) -> CenterError {
        match error {
            ServiceError::Invalid(inner) => fail(inner.code(), 400, context_message(inner)),
            ServiceError::Conflict => fail("context_conflict", 409, "记忆已被修改，请刷新后再试"),
            ServiceError::NotFound => fail("context_not_found", 404, "记忆不存在或已删除"),
            ServiceError::ScopeStale => fail(
                "context_scope_stale",
                409,
                "设备授权或当前项目已变化，请刷新",
            ),
            ServiceError::Store(_) => fail("context_store_failed", 500, "记忆存储失败"),
        }
    }

    fn knowledge_error(error: KnowledgeError) -> CenterError {
        match error {
            KnowledgeError::InvalidRoot => {
                fail("knowledge_root_invalid", 400, "只能选择本机上的普通文件夹")
            }
            KnowledgeError::Unavailable => fail("knowledge_unavailable", 409, "资料目录无法读取"),
            KnowledgeError::UnsafePath => fail(
                "knowledge_unsafe_path",
                400,
                "该文件夹是系统、用户根目录、敏感目录，或包含链接 / 目录联接",
            ),
            KnowledgeError::LimitExceeded => fail(
                "knowledge_limit_exceeded",
                400,
                "资料过多：最多 1000 个文件、32 MiB、6 层目录",
            ),
            KnowledgeError::Context(inner) => fail(inner.code(), 400, context_message(inner)),
            KnowledgeError::ScopeStale => {
                fail("context_scope_stale", 409, "当前项目已变化，请刷新")
            }
            KnowledgeError::Conflict => {
                fail("knowledge_conflict", 409, "资料目录已被修改，请刷新后再试")
            }
            KnowledgeError::NotFound => fail("knowledge_not_found", 404, "资料目录不存在"),
            KnowledgeError::Store(_) => fail(
                "knowledge_store_failed",
                500,
                "资料索引存储失败（同一目录不能重复添加）",
            ),
        }
    }

    fn bad_input() -> CenterError {
        fail("context_invalid", 400, "长期记忆操作格式无效")
    }

    fn text<'a>(input: &'a Value, key: &str) -> Result<&'a str, CenterError> {
        input.get(key).and_then(Value::as_str).ok_or_else(bad_input)
    }

    fn int(input: &Value, key: &str) -> Result<i64, CenterError> {
        input.get(key).and_then(Value::as_i64).ok_or_else(bad_input)
    }

    fn scope_of(input: &Value) -> Result<ScopeKey, CenterError> {
        let scope = input.get("scope").ok_or_else(bad_input)?;
        ScopeKey::parse(text(scope, "kind")?, text(scope, "id")?)
            .map_err(|e| fail(e.code(), 400, context_message(e)))
    }

    fn strings(input: &Value, key: &str) -> Vec<String> {
        input
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn row_json(row: &ContextMemory, at: f64) -> Value {
        let state = match row.state(at) {
            RecordState::Active(_) => "active",
            RecordState::Expired => "expired",
            RecordState::Invalidated => "invalidated",
            RecordState::Deleted => "deleted",
        };
        json!({
            "id": row.id, "scope": {"kind": row.scope.kind().as_str(), "id": row.scope.id()},
            "category": row.category.as_str(), "content": row.content, "verification": row.verification.as_str(),
            "source_kind": row.source.kind().as_str(), "source_ref": row.source.reference(),
            "device_model": row.applicability.as_ref().map(|a| a.device_model()), "firmware": row.applicability.as_ref().and_then(|a| a.firmware()),
            "revision": row.revision, "created_at": row.created_at, "updated_at": row.updated_at, "expires_at": row.expires_at, "state": state,
        })
    }

    impl ContextHost {
        #[cfg(test)]
        pub(crate) fn new(db: Arc<Mutex<RuntimeDb>>, settings_file: PathBuf) -> Arc<Self> {
            Self::with_speakers(db, settings_file, Arc::new(Vec::new))
        }

        pub(crate) fn with_speakers(
            db: Arc<Mutex<RuntimeDb>>,
            settings_file: PathBuf,
            speakers: CalibratedSpeakersFn,
        ) -> Arc<Self> {
            let trust = Arc::new(Trust {
                db,
                settings_file,
                speakers,
            });
            let store = Arc::new(DbContextStore(trust.clone()));
            let service = ContextService::new(store.clone());
            let knowledge = KnowledgeService::new(Arc::new(DbKnowledgeStore(trust.clone())));
            let extractor = ContextExtractor::new(service.clone());
            let retriever = ContextRetriever::new(service.clone());
            retriever.set_knowledge(knowledge.clone());
            let context = trust.context();
            let _ = extractor.configure(context.extraction, || Ok(()));
            retriever.set_enabled(context.retrieval);
            Arc::new(Self {
                trust,
                store,
                service,
                knowledge,
                extractor,
                retriever,
                settings_lock: Mutex::new(()),
            })
        }

        /// Hooks for the conversation; both stay inert while their switch is off.
        pub(crate) fn hooks(&self) -> (ContextRetrieval, ContextExtraction) {
            let access: Arc<dyn ContextAccessProvider> = Arc::new(VoiceAccess(self.trust.clone()));
            (
                ContextRetrieval {
                    retriever: self.retriever.clone(),
                    access: access.clone(),
                },
                ContextExtraction {
                    extractor: self.extractor.clone(),
                    access,
                },
            )
        }

        /// Device allowed / revoked: in-flight extraction for it is discarded.
        pub(crate) fn device_changed(&self, device_id: &str) {
            if let Ok(key) = ScopeKey::new(ScopeKind::Device, device_id) {
                let _ = self.extractor.scope_changed(&key, || Ok::<_, ()>(()));
            }
        }

        /// Speaker deleted or re-enrolled: in-flight extraction for that person is discarded.
        pub(crate) fn person_changed(&self, person_id: &str) {
            if let Ok(key) = ScopeKey::new(ScopeKind::Person, person_id) {
                let _ = self.extractor.scope_changed(&key, || Ok::<_, ()>(()));
            }
        }

        /// Calibration reset or personal mode toggled: invalidate in-flight extractions for all speakers.
        pub(crate) fn speakers_invalidated(&self) {
            let ids: Vec<String> = self
                .trust
                .db
                .lock()
                .ok()
                .and_then(|db| db.list_voice_speakers().ok())
                .unwrap_or_default()
                .into_iter()
                .map(|r| r.id)
                .collect();
            for id in ids {
                self.person_changed(&id);
            }
        }

        fn save_context(
            &self,
            change: impl FnOnce(&mut BuiltinContext),
        ) -> Result<BuiltinContext, CenterError> {
            let _guard = self
                .settings_lock
                .lock()
                .map_err(|_| fail("context_settings_failed", 500, "设置锁不可用"))?;
            moss_voice::settings::update_voice(&self.trust.settings_file, |settings| {
                change(&mut settings.builtin_context);
                Ok(settings.builtin_context.clone())
            })
            .map_err(|e| fail("context_settings_failed", 500, &e))
        }

        fn project_key(&self) -> Result<ScopeKey, CenterError> {
            self.trust
                .project()
                .map(|s| s.key().clone())
                .ok_or_else(|| fail("context_no_project", 409, "请先设置当前项目"))
        }

        pub(crate) fn add_folder(&self, path: &std::path::Path) -> Result<Value, CenterError> {
            let scope = independent_kb_scope();
            let (root, report) = self
                .knowledge
                .add_root(&self.trust.owner(), &scope, path, now())
                .map_err(knowledge_error)?;
            Ok(json!({
                "id": root.id, "label": root.label, "enabled": root.enabled,
                "revision": root.revision, "indexed_at": root.indexed_at,
                "directories": report.directories, "files": report.files, "chunks": report.chunks, "skipped": report.skipped
            }))
        }

        pub(crate) fn manage(
            &self,
            input: &Value,
            llm: impl FnOnce() -> Option<Arc<dyn Llm>>,
        ) -> Result<Value, CenterError> {
            let action = text(input, "action")?;
            let owner = self.trust.owner();
            let at = now();
            match action {
                "status" => self.status(&owner, at),
                "set_options" => {
                    let current = self.trust.context();
                    let retrieval = input
                        .get("retrieval")
                        .and_then(Value::as_bool)
                        .unwrap_or(current.retrieval);
                    let extraction = input
                        .get("extraction")
                        .and_then(Value::as_bool)
                        .unwrap_or(current.extraction);
                    if extraction
                        && !current.extraction
                        && input.get("confirmed").and_then(Value::as_bool) != Some(true)
                    {
                        return Err(fail(
                            "context_confirm_required",
                            400,
                            "开启自动提取前需要确认：对话文字会发给你配置的大模型",
                        ));
                    }
                    let mut saved = None;
                    self.extractor
                        .configure(extraction, || {
                            saved = Some(
                                self.save_context(|c| {
                                    c.retrieval = retrieval;
                                    c.extraction = extraction;
                                })
                                .map_err(|e| e.message)?,
                            );
                            Ok(())
                        })
                        .map_err(|e| fail("context_settings_failed", 500, &e))?;
                    self.retriever.set_enabled(retrieval);
                    if extraction {
                        self.auto_import_legacy_summaries(&owner, at);
                    }
                    let saved =
                        saved.ok_or_else(|| fail("context_settings_failed", 500, "设置未保存"))?;
                    Ok(json!({"retrieval": saved.retrieval, "extraction": saved.extraction}))
                }
                "set_project" => {
                    let name: String = text(input, "name")?.trim().chars().take(40).collect();
                    if name.is_empty() || name.chars().any(char::is_control) {
                        return Err(bad_input());
                    }
                    let mut bytes = [0u8; 8];
                    getrandom::fill(&mut bytes)
                        .map_err(|_| fail("context_settings_failed", 500, "无法生成项目编号"))?;
                    let id = format!(
                        "p-{}",
                        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
                    );
                    self.switch_project(Some((id, name)))
                }
                "clear_project" => self.switch_project(None),
                "list" => {
                    let scope = scope_of(input)?;
                    let category = input
                        .get("category")
                        .and_then(Value::as_str)
                        .and_then(Category::parse);
                    let query = input
                        .get("query")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|q| !q.is_empty());
                    let rows: Vec<ContextMemory> = match query {
                        Some(q) => {
                            let Some(query) = fast_query(q) else {
                                return Ok(json!({"rows": []}));
                            };
                            let held = owner.trusted(&scope).cloned().ok_or_else(|| {
                                service_error(ServiceError::Invalid(ContextError::AccessDenied))
                            })?;
                            let scoped = AccessContext::desktop_private(vec![held]);
                            self.service
                                .search(&scoped, &query, None, at)
                                .map_err(service_error)?
                                .into_iter()
                                .map(|(r, _)| r)
                                .collect()
                        }
                        None => self
                            .service
                            .list(&owner, &scope, at)
                            .map_err(service_error)?,
                    };
                    let rows: Vec<Value> = rows
                        .iter()
                        .filter(|r| category.is_none_or(|c| r.category == c))
                        .map(|r| row_json(r, at))
                        .collect();
                    Ok(json!({"rows": rows}))
                }
                "list_all" => {
                    let query = input
                        .get("query")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|q| !q.is_empty());
                    let parsed_query = query.and_then(fast_query);
                    if query.is_some() && parsed_query.is_none() {
                        return Ok(json!({"rows": []}));
                    }
                    let devices_meta: Vec<(String, String)> = self
                        .trust
                        .db
                        .lock()
                        .ok()
                        .and_then(|db| db.voice_devices().ok())
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|d| d.is_allowed())
                        .map(|d| {
                            (
                                d.device_id.clone(),
                                if d.name.trim().is_empty() {
                                    d.device_id
                                } else {
                                    d.name
                                },
                            )
                        })
                        .collect();
                    let speakers_meta = (self.trust.speakers)();
                    let project_name = self.trust.context().project_name;
                    let mut collected: Vec<ContextMemory> = Vec::new();
                    for trusted in owner.scopes() {
                        let key = trusted.key();
                        if key.kind() == ScopeKind::Workspace {
                            continue;
                        }
                        let found = match &parsed_query {
                            Some(q) => {
                                let scoped = AccessContext::desktop_private(vec![trusted.clone()]);
                                self.service
                                    .search(&scoped, q, None, at)
                                    .map(|hits| hits.into_iter().map(|(r, _)| r).collect())
                                    .unwrap_or_default()
                            }
                            None => self.service.list(&owner, key, at).unwrap_or_default(),
                        };
                        for row in found {
                            if matches!(row.state(at), RecordState::Active(_))
                                && !collected.iter().any(|seen| seen.id == row.id)
                            {
                                collected.push(row);
                            }
                        }
                    }
                    collected.sort_by(|a, b| b.updated_at.total_cmp(&a.updated_at));
                    let rows: Vec<Value> = collected
                        .iter()
                        .map(|r| {
                            let mut v = row_json(r, at);
                            let label = match r.scope.kind() {
                                ScopeKind::Person => speakers_meta
                                    .iter()
                                    .find(|(id, _, _)| id == r.scope.id())
                                    .map(|(_, name, _)| name.clone())
                                    .unwrap_or_else(|| r.scope.id().to_owned()),
                                ScopeKind::Device => devices_meta
                                    .iter()
                                    .find(|(id, _)| id == r.scope.id())
                                    .map(|(_, name)| name.clone())
                                    .unwrap_or_else(|| r.scope.id().to_owned()),
                                ScopeKind::Project => {
                                    if project_name.is_empty() {
                                        r.scope.id().to_owned()
                                    } else {
                                        project_name.clone()
                                    }
                                }
                                ScopeKind::Workspace => "通用".into(),
                            };
                            if let Some(obj) = v.as_object_mut() {
                                obj.insert("scope_label".into(), Value::String(label));
                            }
                            v
                        })
                        .collect();
                    Ok(json!({"rows": rows}))
                }
                "quick_create" => {
                    let content = text(input, "content")?.trim().to_owned();
                    let personal_on = moss_voice::settings::load(&self.trust.settings_file)
                        .builtin_person_memory_enabled;
                    let target = if input.get("scope").is_some() {
                        scope_of(input)?
                    } else if personal_on
                        && let Some(person) = owner
                            .scopes()
                            .iter()
                            .find(|s| s.key().kind() == ScopeKind::Person)
                    {
                        person.key().clone()
                    } else if let Some(dev) = owner
                        .scopes()
                        .iter()
                        .find(|s| s.key().kind() == ScopeKind::Device)
                    {
                        dev.key().clone()
                    } else {
                        return Err(fail(
                            "context_no_device",
                            409,
                            "请先在“服务”标签允许一台小智设备接入后再添加记忆",
                        ));
                    };
                    let draft = MemoryDraft {
                        scope: target,
                        category: Category::Preference,
                        content,
                        verification: Verification::Confirmed,
                        source: Source::new(SourceKind::UserEdit, None)
                            .map_err(|e| fail(e.code(), 400, context_message(e)))?,
                        applicability: None,
                        expires_at: None,
                    };
                    let row = self
                        .service
                        .create(&owner, draft, at)
                        .map_err(service_error)?;
                    Ok(row_json(&row, at))
                }
                "clear_all" => {
                    if input.get("confirmed").and_then(Value::as_bool) != Some(true) {
                        return Err(fail("context_confirm_required", 400, "清空前需要确认"));
                    }
                    let mut cleared = 0usize;
                    for trusted in owner.scopes() {
                        let key = trusted.key().clone();
                        if key.kind() == ScopeKind::Workspace {
                            continue;
                        }
                        if let Ok(n) = self
                            .extractor
                            .scope_changed(&key, || self.service.clear(&owner, &key, at))
                        {
                            cleared += n;
                        }
                    }
                    Ok(json!({"cleared": cleared}))
                }
                "deep_search" => {
                    let question = text(input, "query")?;
                    let llm = llm().ok_or_else(|| {
                        fail(
                            "context_llm_missing",
                            409,
                            "深度检索需要先配置内置服务的大模型",
                        )
                    })?;
                    let rows = tauri::async_runtime::block_on(
                        self.retriever.deep_search(owner, question, llm, None),
                    )
                    .map_err(service_error)?;
                    Ok(json!({"rows": rows.iter().map(|r| row_json(r, at)).collect::<Vec<_>>()}))
                }
                "create" => {
                    let scope = scope_of(input)?;
                    let category =
                        Category::parse(text(input, "category")?).ok_or_else(bad_input)?;
                    let applicability = self.applicability_input(input)?;
                    let confirmed = input.get("confirmed").and_then(Value::as_bool) == Some(true);
                    let draft = MemoryDraft {
                        scope,
                        category,
                        content: text(input, "content")?.trim().to_owned(),
                        verification: if confirmed {
                            Verification::Confirmed
                        } else {
                            Verification::Candidate
                        },
                        source: Source::new(SourceKind::UserEdit, None)
                            .map_err(|e| fail(e.code(), 400, context_message(e)))?,
                        applicability,
                        expires_at: input.get("expires_at").and_then(Value::as_f64),
                    };
                    let row = self
                        .service
                        .create(&owner, draft, at)
                        .map_err(service_error)?;
                    Ok(row_json(&row, at))
                }
                "edit" => {
                    let applicability = self.applicability_input(input)?;
                    let row = self
                        .service
                        .edit(
                            &owner,
                            text(input, "id")?,
                            int(input, "revision")?,
                            text(input, "content")?.trim(),
                            applicability,
                            input.get("expires_at").and_then(Value::as_f64),
                            at,
                        )
                        .map_err(service_error)?;
                    Ok(row_json(&row, at))
                }
                "verify" => {
                    let id = text(input, "id")?;
                    let revision = int(input, "revision")?;
                    let row = match text(input, "to")? {
                        "confirmed" => {
                            let category = self.store.get(id).ok().flatten().map(|r| r.category);
                            if category == Some(Category::TroubleshootingExperience) {
                                self.service.confirm_experience(
                                    &owner,
                                    id,
                                    revision,
                                    &Evidence::UserConfirmed,
                                    at,
                                )
                            } else {
                                self.service.set_verification(
                                    &owner,
                                    id,
                                    revision,
                                    Verification::Confirmed,
                                    Some(&Evidence::UserConfirmed),
                                    at,
                                )
                            }
                        }
                        "invalidated" => self.service.set_verification(
                            &owner,
                            id,
                            revision,
                            Verification::Invalidated,
                            None,
                            at,
                        ),
                        _ => return Err(bad_input()),
                    }
                    .map_err(service_error)?;
                    Ok(row_json(&row, at))
                }
                "delete" => {
                    let id = text(input, "id")?;
                    let revision = int(input, "revision")?;
                    let scope = self
                        .store
                        .get(id)
                        .ok()
                        .flatten()
                        .map(|r| r.scope)
                        .ok_or_else(|| fail("context_not_found", 404, "记忆不存在或已删除"))?;
                    self.extractor
                        .scope_changed(&scope, || self.service.delete(&owner, id, revision, at))
                        .map_err(service_error)?;
                    Ok(json!({"deleted": id}))
                }
                "clear" => {
                    let scope = scope_of(input)?;
                    if input.get("confirmed").and_then(Value::as_bool) != Some(true) {
                        return Err(fail("context_confirm_required", 400, "清空前需要确认"));
                    }
                    let cleared = self
                        .extractor
                        .scope_changed(&scope, || self.service.clear(&owner, &scope, at))
                        .map_err(service_error)?;
                    Ok(json!({"cleared": cleared}))
                }
                "import_legacy" => {
                    let device_id = text(input, "device_id")?;
                    let legacy = self
                        .trust
                        .db
                        .lock()
                        .map_err(|_| fail("context_store_failed", 500, "数据库锁失败"))?
                        .voice_memory(device_id)
                        .ok()
                        .flatten();
                    let Some(legacy) = legacy else {
                        return Ok(json!({"created": 0, "skipped": ["empty_summary"]}));
                    };
                    let report = self
                        .service
                        .import_legacy(&owner, device_id, &legacy.summary, legacy.revision, at)
                        .map_err(service_error)?;
                    Ok(json!({"created": report.created.len(), "skipped": report.skipped}))
                }
                "export" => {
                    let scope = scope_of(input)?;
                    Ok(
                        json!({"memories": self.service.export(&owner, &scope, at).map_err(service_error)?}),
                    )
                }
                "extractions" => {
                    let scope = scope_of(input)?;
                    let runs = self
                        .service
                        .list_extractions(&owner, &scope, 50)
                        .map_err(service_error)?;
                    Ok(json!({"runs": runs.iter().map(|r| json!({
                        "id": r.id, "started_at": r.started_at, "finished_at": r.finished_at, "reverted_at": r.reverted_at,
                        "added": r.adds.len(), "skipped": r.skipped.iter().map(|s| json!({"op": s.op, "reason": s.reason})).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>()}))
                }
                "revert" => {
                    let report = self
                        .service
                        .revert_extraction(&owner, text(input, "run_id")?, at)
                        .map_err(service_error)?;
                    Ok(json!({"removed": report.removed, "kept": report.kept}))
                }
                "knowledge_list" => {
                    let roots = self.knowledge.all_roots(&owner).map_err(knowledge_error)?;
                    let db = self
                        .trust
                        .db
                        .lock()
                        .map_err(|_| fail("context_store_failed", 500, "数据库锁失败"))?;
                    Ok(json!({"roots": roots.iter().map(|r| {
                        let dirs = db.knowledge_dirs(&r.id).unwrap_or_default();
                        json!({"id": r.id, "label": r.label, "enabled": r.enabled, "revision": r.revision, "indexed_at": r.indexed_at,
                               "directories": dirs.len(), "files": dirs.iter().map(|d| d.file_count).sum::<i64>()})
                    }).collect::<Vec<_>>()}))
                }
                "knowledge_toggle" => {
                    let enabled = input
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .ok_or_else(bad_input)?;
                    let root = self
                        .knowledge
                        .set_enabled(&owner, text(input, "id")?, int(input, "revision")?, enabled)
                        .map_err(knowledge_error)?;
                    let db = self
                        .trust
                        .db
                        .lock()
                        .map_err(|_| fail("context_store_failed", 500, "数据库锁失败"))?;
                    let dirs = db.knowledge_dirs(&root.id).unwrap_or_default();
                    Ok(json!({
                        "id": root.id, "label": root.label, "enabled": root.enabled, "revision": root.revision,
                        "indexed_at": root.indexed_at, "directories": dirs.len(),
                        "files": dirs.iter().map(|d| d.file_count).sum::<i64>()
                    }))
                }
                "knowledge_reindex" => {
                    let report = self
                        .knowledge
                        .reindex(&owner, text(input, "id")?, at)
                        .map_err(knowledge_error)?;
                    Ok(
                        json!({"directories": report.directories, "files": report.files, "chunks": report.chunks, "skipped": report.skipped}),
                    )
                }
                "knowledge_remove" => {
                    self.knowledge
                        .remove_root(&owner, text(input, "id")?, int(input, "revision")?)
                        .map_err(knowledge_error)?;
                    Ok(json!({"removed": true}))
                }
                "experience_record" => {
                    let device_id = text(input, "device_id")?;
                    let device = self.trust.applicability(device_id).ok_or_else(|| {
                        fail(
                            "context_applicability_required",
                            400,
                            "该设备没有登记机型和固件版本，不能记录经验",
                        )
                    })?;
                    let experience = Experience {
                        symptom: text(input, "symptom")?.into(),
                        checks: strings(input, "checks"),
                        conclusion: text(input, "conclusion")?.into(),
                        evidence: vec![],
                    };
                    let evidence = (input.get("confirmed").and_then(Value::as_bool) == Some(true))
                        .then_some(Evidence::UserConfirmed);
                    let scope = ScopeKey::new(ScopeKind::Device, device_id)
                        .map_err(|e| fail(e.code(), 400, context_message(e)))?;
                    let recorded = self
                        .service
                        .record_experience(&owner, &scope, &device, experience, evidence, at)
                        .map_err(service_error)?;
                    Ok(
                        json!({"row": row_json(&recorded.memory, at), "similar": recorded.similar.iter().map(|s| s.id.clone()).collect::<Vec<_>>()}),
                    )
                }
                "experience_recall" => {
                    let device_id = text(input, "device_id")?;
                    let device = self.trust.applicability(device_id);
                    let found = self
                        .service
                        .recall_experience(&owner, text(input, "symptom")?, device.as_ref(), at)
                        .map_err(service_error)?;
                    Ok(json!({"text": format_experience(&found)}))
                }
                "resume_get" => {
                    let scope = self.project_key()?;
                    let current = self
                        .service
                        .list(&owner, &scope, at)
                        .map_err(service_error)?
                        .into_iter()
                        .filter(|r| {
                            r.source.kind() == SourceKind::UserEdit
                                && r.verification == Verification::Confirmed
                        })
                        .filter_map(|r| ResumeNote::parse(&r.content).map(|n| (r, n)))
                        .max_by(|a, b| a.0.updated_at.total_cmp(&b.0.updated_at));
                    let view = self
                        .service
                        .resume(&owner, &scope, &DbResumeFacts(self.trust.clone()), at)
                        .map_err(service_error)?;
                    Ok(match current {
                        Some((row, note)) => json!({
                            "id": row.id, "revision": row.revision, "updated_at": row.updated_at,
                            "note": {"decisions": note.decisions, "todos": note.todos, "evidence": note.evidence, "unverified": note.unverified},
                            "text": view.as_ref().map(format_resume),
                        }),
                        None => Value::Null,
                    })
                }
                "resume_save" => {
                    let scope = self.project_key()?;
                    let note = input.get("note").ok_or_else(bad_input)?;
                    let note = ResumeNote {
                        decisions: strings(note, "decisions"),
                        todos: strings(note, "todos"),
                        evidence: strings(note, "evidence"),
                        unverified: strings(note, "unverified"),
                    };
                    let expected = match input.get("expected") {
                        Some(Value::Object(e)) => Some((
                            e.get("id")
                                .and_then(Value::as_str)
                                .ok_or_else(bad_input)?
                                .to_owned(),
                            e.get("revision")
                                .and_then(Value::as_i64)
                                .ok_or_else(bad_input)?,
                        )),
                        _ => None,
                    };
                    let row = self
                        .service
                        .save_resume(
                            &owner,
                            &scope,
                            &note,
                            expected.as_ref().map(|(id, rev)| (id.as_str(), *rev)),
                            at,
                        )
                        .map_err(service_error)?;
                    Ok(json!({"id": row.id, "revision": row.revision}))
                }
                _ => Err(bad_input()),
            }
        }

        fn switch_project(&self, next: Option<(String, String)>) -> Result<Value, CenterError> {
            let old = self.trust.project().map(|s| s.key().clone());
            let change = |c: &mut BuiltinContext| {
                let (id, name) = next.clone().unwrap_or_default();
                c.project_id = id;
                c.project_name = name;
                c.project_generation = c.project_generation.wrapping_add(1).max(1);
            };
            let saved = match &old {
                // Discard in-flight extraction for the old project atomically with the switch.
                Some(key) => self
                    .extractor
                    .scope_changed(key, || self.save_context(change))?,
                None => self.save_context(change)?,
            };
            Ok(
                json!({"project": (!saved.project_id.is_empty()).then(|| json!({"id": saved.project_id, "name": saved.project_name}))}),
            )
        }

        fn applicability_input(&self, input: &Value) -> Result<Option<Applicability>, CenterError> {
            match input.get("device_id").and_then(Value::as_str) {
                Some(device_id) => self
                    .trust
                    .applicability(device_id)
                    .map(Some)
                    .ok_or_else(|| {
                        fail(
                            "context_applicability_required",
                            400,
                            "该设备没有登记机型和固件版本",
                        )
                    }),
                None => Ok(None),
            }
        }

        fn status(&self, owner: &AccessContext, at: f64) -> Result<Value, CenterError> {
            let context = self.trust.context();
            let devices = self
                .trust
                .db
                .lock()
                .map_err(|_| fail("context_store_failed", 500, "数据库锁失败"))?
                .voice_devices()
                .unwrap_or_default();
            let mut scopes = Vec::new();
            for scope in owner
                .scopes()
                .iter()
                .filter(|s| s.key().kind() != ScopeKind::Workspace)
            {
                let rows = self
                    .service
                    .list(owner, scope.key(), at)
                    .unwrap_or_default();
                let live: Vec<_> = rows
                    .iter()
                    .filter(|r| matches!(r.state(at), RecordState::Active(_)))
                    .collect();
                let chars: usize = rows.iter().map(|r| r.content.chars().count()).sum();
                scopes.push(json!({
                    "kind": scope.key().kind().as_str(), "id": scope.key().id(),
                    "total": rows.len(), "active": live.len(),
                    "candidates": live.iter().filter(|r| r.verification == Verification::Candidate).count(), "chars": chars,
                }));
            }
            let legacy: Vec<String> = {
                let db = self
                    .trust
                    .db
                    .lock()
                    .map_err(|_| fail("context_store_failed", 500, "数据库锁失败"))?;
                devices
                    .iter()
                    .filter(|d| d.is_allowed())
                    .filter(|d| {
                        db.voice_memory(&d.device_id)
                            .ok()
                            .flatten()
                            .is_some_and(|m| !m.summary.trim().is_empty())
                    })
                    .map(|d| d.device_id.clone())
                    .collect()
            };
            let persons: Vec<Value> = (self.trust.speakers)()
                .into_iter()
                .map(|(id, name, revision)| json!({"id": id, "name": name, "revision": revision}))
                .collect();
            Ok(json!({
                "retrieval": context.retrieval, "extraction": context.extraction,
                "project": (!context.project_id.is_empty()).then(|| json!({"id": context.project_id, "name": context.project_name})),
                "devices": devices.iter().filter(|d| d.is_allowed()).map(|d| json!({"id": d.device_id, "name": d.name, "model": d.model, "firmware": d.firmware_version})).collect::<Vec<_>>(),
                "persons": persons,
                "scopes": scopes, "legacy_summaries": legacy,
            }))
        }

        fn auto_import_legacy_summaries(&self, owner: &AccessContext, at: f64) {
            let legacy_rows: Vec<(String, String, i64)> = {
                let Ok(db) = self.trust.db.lock() else { return };
                let devices = db.voice_devices().unwrap_or_default();
                devices
                    .into_iter()
                    .filter(|d| d.is_allowed())
                    .filter_map(|d| {
                        let m = db.voice_memory(&d.device_id).ok().flatten()?;
                        (!m.summary.trim().is_empty()).then_some((
                            d.device_id,
                            m.summary,
                            m.revision,
                        ))
                    })
                    .collect()
            };
            for (device_id, summary, revision) in legacy_rows {
                let _ = self
                    .service
                    .import_legacy(owner, &device_id, &summary, revision, at);
            }
        }
    }

    #[cfg(test)]
    mod tests;
}

#[cfg(desktop)]
use crate::voice_builtin::require_private_settings_window;

#[tauri::command]
pub async fn voice_context_manage(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    input: Value,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = crate::voice_builtin::context_manage(app, input).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, input));
    result
}

/// Opens only after an explicit click in the private window; indexes the chosen folder for the
/// current project. The absolute path stays on the native side.
#[tauri::command]
pub async fn voice_context_pick_folder(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> CmdResult {
    #[cfg(desktop)]
    require_private_settings_window(&window)?;
    #[cfg(desktop)]
    let result = {
        let picker = app.clone();
        let picked = tauri::async_runtime::spawn_blocking(move || {
            use tauri_plugin_dialog::DialogExt;
            picker
                .dialog()
                .file()
                .set_title("选择资料文件夹（只读取文本文件）")
                .blocking_pick_folder()
                .and_then(|p| p.into_path().ok())
        })
        .await
        .map_err(
            |_| serde_json::json!({"code": "voice_internal", "message": "选择资料文件夹失败"}),
        )?;
        match picked {
            None => Ok(Value::Null),
            Some(path) => crate::voice_builtin::context_add_folder(app, path).await,
        }
    };
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

#[cfg(mobile)]
fn desktop_only<T>(_: T) -> CmdResult {
    Err(
        serde_json::json!({"code": "voice_desktop_only", "message": "长期记忆管理仅在 MOSS 桌面端可用"}),
    )
}

/// Commands the private page calls (build.rs registration and capability tests check this).
#[cfg(test)]
pub const COMMANDS: [&str; 2] = ["voice_context_manage", "voice_context_pick_folder"];

#[cfg(all(test, desktop))]
mod job_facts_tests {
    #[test]
    fn query_job_ref_state_reads_workflows_and_calibrations_before_legacy_jobs() {
        let db = laser_runtime::RuntimeDb::open_memory().unwrap();
        db.connection
            .execute_batch(
                "INSERT INTO workflows(workflow_id,status,created_at,updated_at,payload) VALUES
                   ('wf-real','sending',1,1,'{\"workflow_id\":\"wf-real\",\"status\":\"sending\",\"send_result\":{\"phase\":\"streaming\"}}');
                 INSERT INTO calibrations(calibration_id,status,created_at,updated_at,payload) VALUES
                   ('cal-real','completed',1,1,'{\"calibration_id\":\"cal-real\",\"status\":\"completed\"}');",
            )
            .unwrap();
        assert_eq!(
            super::host::query_job_ref_state(&db, "wf-real"),
            Some(("sending".to_owned(), Some("streaming".to_owned())))
        );
        assert_eq!(
            super::host::query_job_ref_state(&db, "cal-real"),
            Some(("completed".to_owned(), None))
        );
        assert_eq!(super::host::query_job_ref_state(&db, "missing"), None);
    }
}
