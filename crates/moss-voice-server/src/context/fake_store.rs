//! In-memory `ContextStore` for unit tests, with the same conditional semantics as SQLite
//! (revision checks, tombstones, one-shot revert). Synthetic data only.
use super::retrieve::SearchQuery;
use super::service::{ContextStore, ExtractionRun, MemoryPatch};
use super::{ContextMemory, ScopeKey, TrustedScope, Verification};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
pub(crate) struct FakeStore {
    pub(crate) rows: Mutex<HashMap<String, ContextMemory>>,
    pub(crate) stale: AtomicBool,
    pub(crate) runs: Mutex<HashMap<String, ExtractionRun>>,
}

impl ContextStore for FakeStore {
    fn current(&self, scope: &TrustedScope) -> bool {
        !self.stale.load(Ordering::SeqCst) && scope.generation() > 0
    }
    fn insert(&self, memory: &ContextMemory) -> Result<ContextMemory, String> {
        let mut rows = self.rows.lock().unwrap();
        if rows.contains_key(&memory.id) {
            return Err("duplicate".into());
        }
        rows.insert(memory.id.clone(), memory.clone());
        Ok(memory.clone())
    }
    fn get(&self, id: &str) -> Result<Option<ContextMemory>, String> {
        Ok(self.rows.lock().unwrap().get(id).cloned())
    }
    fn list(&self, scope: &ScopeKey, include_deleted: bool) -> Result<Vec<ContextMemory>, String> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .values()
            .filter(|r| &r.scope == scope && (include_deleted || r.deleted_at.is_none()))
            .cloned()
            .collect())
    }
    fn source_seen(&self, scope: &ScopeKey, source_ref: &str) -> Result<bool, String> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .values()
            .any(|r| &r.scope == scope && r.source.reference() == Some(source_ref)))
    }
    fn update(
        &self,
        id: &str,
        expected: i64,
        patch: &MemoryPatch,
        now: f64,
    ) -> Result<Option<ContextMemory>, String> {
        let mut rows = self.rows.lock().unwrap();
        let Some(row) = rows.get_mut(id) else {
            return Ok(None);
        };
        let demote = row.verification == Verification::Confirmed
            && patch.verification == Verification::Candidate;
        if row.revision != expected
            || row.deleted_at.is_some()
            || row.verification == Verification::Invalidated
            || demote
        {
            return Ok(None);
        }
        row.content = patch.content.clone();
        row.verification = patch.verification;
        row.applicability = patch.applicability.clone();
        row.expires_at = patch.expires_at;
        row.revision += 1;
        row.updated_at = now;
        Ok(Some(row.clone()))
    }
    fn reinforce(&self, id: &str, now: f64, extend_to: Option<f64>) -> Result<bool, String> {
        let mut rows = self.rows.lock().unwrap();
        let Some(row) = rows.get_mut(id) else {
            return Ok(false);
        };
        if row.deleted_at.is_some() || row.verification == Verification::Invalidated {
            return Ok(false);
        }
        row.seen_count += 1;
        row.last_seen_at = Some(now);
        let extracted = row.source.kind() == super::SourceKind::ConversationExtraction
            && row.verification == Verification::Candidate;
        if let (true, Some(to), Some(at)) = (extracted, extend_to, row.expires_at)
            && at < to
        {
            row.expires_at = Some(to);
        }
        Ok(true)
    }
    fn delete(&self, id: &str, expected: i64, now: f64) -> Result<bool, String> {
        let mut rows = self.rows.lock().unwrap();
        let Some(row) = rows.get_mut(id) else {
            return Ok(false);
        };
        if row.revision != expected || row.deleted_at.is_some() {
            return Ok(false);
        }
        row.content.clear();
        row.applicability = None;
        row.revision += 1;
        row.deleted_at = Some(now);
        Ok(true)
    }
    fn clear(&self, scope: &ScopeKey, now: f64) -> Result<usize, String> {
        let mut changed = 0;
        for row in self
            .rows
            .lock()
            .unwrap()
            .values_mut()
            .filter(|r| &r.scope == scope && r.deleted_at.is_none())
        {
            row.content.clear();
            row.revision += 1;
            row.deleted_at = Some(now);
            changed += 1;
        }
        Ok(changed)
    }
    fn insert_run(&self, run: &ExtractionRun) -> Result<(), String> {
        let mut runs = self.runs.lock().unwrap();
        if runs.contains_key(&run.id) {
            return Err("duplicate run".into());
        }
        runs.insert(run.id.clone(), run.clone());
        Ok(())
    }
    fn get_run(&self, id: &str) -> Result<Option<ExtractionRun>, String> {
        Ok(self.runs.lock().unwrap().get(id).cloned())
    }
    fn list_runs(&self, scope: &ScopeKey, limit: usize) -> Result<Vec<ExtractionRun>, String> {
        let mut runs: Vec<_> = self
            .runs
            .lock()
            .unwrap()
            .values()
            .filter(|r| &r.scope == scope)
            .cloned()
            .collect();
        runs.sort_by(|a, b| b.started_at.total_cmp(&a.started_at));
        runs.truncate(limit);
        Ok(runs)
    }
    fn search(
        &self,
        scope: &ScopeKey,
        query: &SearchQuery,
        now: f64,
        limit: usize,
    ) -> Result<Vec<ContextMemory>, String> {
        let rows = self.rows.lock().unwrap();
        let hit = |text: &str| {
            query
                .terms
                .iter()
                .chain(&query.shorts)
                .any(|t| text.contains(t.as_str()))
        };
        Ok(rows
            .values()
            .filter(|r| {
                &r.scope == scope
                    && r.deleted_at.is_none()
                    && r.verification != Verification::Invalidated
                    && r.expires_at.is_none_or(|at| at > now)
            })
            .filter(|r| hit(&r.content.to_lowercase()))
            .take(limit)
            .cloned()
            .collect())
    }
    fn mark_run_reverted(&self, id: &str, now: f64) -> Result<bool, String> {
        let mut runs = self.runs.lock().unwrap();
        match runs.get_mut(id) {
            Some(run) if run.reverted_at.is_none() => {
                run.reverted_at = Some(now);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}
