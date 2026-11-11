//! NV19-02 long-term context rows (`context_memories`, schema v10).
//! Field rules are validated upstream by `moss-voice-server::context`; the CHECK
//! constraints and conditional updates here are the last line of defence.
//! NV10 `voice_memory` is never read, moved or modified by this module.
use crate::db::{RuntimeDb, SchemaError};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextMemoryRow {
    pub id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub category: String,
    pub content: String,
    pub verification: String,
    pub source_kind: String,
    pub source_ref: Option<String>,
    pub device_model: Option<String>,
    pub firmware: Option<String>,
    pub revision: i64,
    pub created_at: f64,
    pub updated_at: f64,
    pub expires_at: Option<f64>,
    pub deleted_at: Option<f64>,
    /// v17: id of the older record (same scope and category) this one replaces once confirmed.
    pub supersedes: Option<String>,
    /// v17: how often the same fact was proposed again; new rows always start at 1.
    pub seen_count: i64,
    pub last_seen_at: Option<f64>,
}

/// User-editable fields. Scope, category and source never change after creation.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextMemoryPatch {
    pub content: String,
    pub verification: String,
    pub device_model: Option<String>,
    pub firmware: Option<String>,
    pub expires_at: Option<f64>,
}

const COLUMNS: &str = "id,scope_kind,scope_id,category,content,verification,source_kind,source_ref,device_model,firmware,revision,created_at,updated_at,expires_at,deleted_at,supersedes,seen_count,last_seen_at";
const MAX_LIST: usize = 500;

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContextMemoryRow> {
    Ok(ContextMemoryRow {
        id: row.get(0)?,
        scope_kind: row.get(1)?,
        scope_id: row.get(2)?,
        category: row.get(3)?,
        content: row.get(4)?,
        verification: row.get(5)?,
        source_kind: row.get(6)?,
        source_ref: row.get(7)?,
        device_model: row.get(8)?,
        firmware: row.get(9)?,
        revision: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        expires_at: row.get(13)?,
        deleted_at: row.get(14)?,
        supersedes: row.get(15)?,
        seen_count: row.get(16)?,
        last_seen_at: row.get(17)?,
    })
}

fn invalid(message: &str) -> SchemaError {
    SchemaError::InvalidRecord(message.into())
}

impl RuntimeDb {
    /// New rows always start at revision 1, seen once, and are never born deleted.
    pub fn insert_context_memory(
        &self,
        row: &ContextMemoryRow,
    ) -> Result<ContextMemoryRow, SchemaError> {
        if row.revision != 1
            || row.seen_count != 1
            || row.last_seen_at.is_some()
            || row.deleted_at.is_some()
            || !row.created_at.is_finite()
            || row.updated_at != row.created_at
            || row.expires_at.is_some_and(|at| !at.is_finite())
        {
            return Err(invalid("记忆条目无效"));
        }
        self.connection.execute(
            &format!("INSERT INTO context_memories({COLUMNS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1,?11,?11,?12,NULL,?13,1,NULL)"),
            params![
                row.id, row.scope_kind, row.scope_id, row.category, row.content, row.verification,
                row.source_kind, row.source_ref, row.device_model, row.firmware, row.created_at, row.expires_at,
                row.supersedes
            ],
        )?;
        self.context_memory(&row.id)?
            .ok_or_else(|| invalid("记忆条目写入失败"))
    }

    pub fn context_memory(&self, id: &str) -> Result<Option<ContextMemoryRow>, SchemaError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM context_memories WHERE id=?1"
        ))?;
        statement
            .query_row([id], from_row)
            .optional()
            .map_err(SchemaError::from)
    }

    /// One scope only, newest first. Deleted tombstones are listed only when asked.
    pub fn list_context_memories(
        &self,
        scope_kind: &str,
        scope_id: &str,
        include_deleted: bool,
        limit: usize,
    ) -> Result<Vec<ContextMemoryRow>, SchemaError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM context_memories WHERE scope_kind=?1 AND scope_id=?2 AND (?3 OR deleted_at IS NULL) ORDER BY updated_at DESC,id LIMIT ?4"
        ))?;
        let rows = statement.query_map(
            params![
                scope_kind,
                scope_id,
                include_deleted,
                limit.min(MAX_LIST) as i64
            ],
            from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Whether a source reference was ever recorded in this scope, tombstones included,
    /// so a deleted import or extraction is not silently recreated.
    pub fn context_source_seen(
        &self,
        scope_kind: &str,
        scope_id: &str,
        source_ref: &str,
    ) -> Result<bool, SchemaError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT 1 FROM context_memories WHERE scope_kind=?1 AND scope_id=?2 AND source_ref=?3 LIMIT 1",
        )?;
        let found: Option<i64> = statement
            .query_row(params![scope_kind, scope_id, source_ref], |row| row.get(0))
            .optional()?;
        Ok(found.is_some())
    }

    /// Conditional edit of a live, non-invalidated row. Invalidated is terminal and a
    /// confirmed row is never demoted to candidate. `None` means a revision conflict
    /// (or the row was deleted/invalidated meanwhile); nothing is written.
    pub fn update_context_memory_if_revision(
        &self,
        id: &str,
        expected_revision: i64,
        patch: &ContextMemoryPatch,
        now: f64,
    ) -> Result<Option<ContextMemoryRow>, SchemaError> {
        if expected_revision < 1
            || !now.is_finite()
            || patch.expires_at.is_some_and(|at| !at.is_finite())
        {
            return Err(invalid("记忆条目或版本无效"));
        }
        let changed = self.connection.execute(
            "UPDATE context_memories SET content=?3,verification=?4,device_model=?5,firmware=?6,expires_at=?7,revision=revision+1,updated_at=?8
             WHERE id=?1 AND revision=?2 AND deleted_at IS NULL AND verification<>'invalidated'
               AND NOT (verification='confirmed' AND ?4='candidate')",
            params![id, expected_revision, patch.content, patch.verification, patch.device_model, patch.firmware, patch.expires_at, now],
        )?;
        if changed == 1 {
            self.context_memory(id)
        } else {
            Ok(None)
        }
    }

    /// Deletion keeps an empty tombstone (id, scope, source reference, revision) so late
    /// background work fails its revision check. The content itself is gone.
    pub fn delete_context_memory_if_revision(
        &self,
        id: &str,
        expected_revision: i64,
        now: f64,
    ) -> Result<bool, SchemaError> {
        if expected_revision < 1 || !now.is_finite() {
            return Err(invalid("记忆条目或版本无效"));
        }
        Ok(self.connection.execute(
            "UPDATE context_memories SET content='',device_model=NULL,firmware=NULL,expires_at=NULL,revision=revision+1,updated_at=?3,deleted_at=?3
             WHERE id=?1 AND revision=?2 AND deleted_at IS NULL",
            params![id, expected_revision, now],
        )? == 1)
    }

    /// v17: the same fact was proposed again. Counts it and, for an unreviewed extracted
    /// candidate whose expiry is earlier than `extend_to`, pushes the expiry out. Content,
    /// verification and revision are untouched, so user edits and run reverts keep working.
    pub fn reinforce_context_memory(
        &self,
        id: &str,
        now: f64,
        extend_to: Option<f64>,
    ) -> Result<bool, SchemaError> {
        if !now.is_finite() || extend_to.is_some_and(|at| !at.is_finite()) {
            return Err(invalid("时间无效"));
        }
        Ok(self.connection.execute(
            "UPDATE context_memories SET seen_count=seen_count+1, last_seen_at=?2,
               expires_at=CASE WHEN ?3 IS NOT NULL AND verification='candidate' AND source_kind='conversation_extraction'
                                AND expires_at IS NOT NULL AND expires_at<?3 THEN ?3 ELSE expires_at END
             WHERE id=?1 AND deleted_at IS NULL AND verification<>'invalidated'",
            params![id, now, extend_to],
        )? == 1)
    }

    /// Clear every live row in one scope (tombstones, same as single deletion).
    pub fn clear_context_scope(
        &self,
        scope_kind: &str,
        scope_id: &str,
        now: f64,
    ) -> Result<usize, SchemaError> {
        if !now.is_finite() {
            return Err(invalid("时间无效"));
        }
        Ok(self.connection.execute(
            "UPDATE context_memories SET content='',device_model=NULL,firmware=NULL,expires_at=NULL,revision=revision+1,updated_at=?3,deleted_at=?3
             WHERE scope_kind=?1 AND scope_id=?2 AND deleted_at IS NULL",
            params![scope_kind, scope_id, now],
        )?)
    }
}

/// NV19-03 extraction manifest. Holds record ids and reason codes only — never memory
/// content — so deleting a memory leaves no copy behind in its manifest.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextExtractionRunRow {
    pub id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub started_at: f64,
    pub finished_at: Option<f64>,
    pub adds_json: String,
    pub updates_json: String,
    pub deletes_json: String,
    pub skipped_json: String,
    pub reverted_at: Option<f64>,
}

const RUN_COLUMNS: &str = "id,scope_kind,scope_id,started_at,finished_at,adds_json,updates_json,deletes_json,skipped_json,reverted_at";
const MAX_MANIFEST: usize = 16 * 1024;

fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContextExtractionRunRow> {
    Ok(ContextExtractionRunRow {
        id: row.get(0)?,
        scope_kind: row.get(1)?,
        scope_id: row.get(2)?,
        started_at: row.get(3)?,
        finished_at: row.get(4)?,
        adds_json: row.get(5)?,
        updates_json: row.get(6)?,
        deletes_json: row.get(7)?,
        skipped_json: row.get(8)?,
        reverted_at: row.get(9)?,
    })
}

impl RuntimeDb {
    pub fn insert_context_extraction_run(
        &self,
        run: &ContextExtractionRunRow,
    ) -> Result<(), SchemaError> {
        let json_ok = |text: &str| {
            text.len() <= MAX_MANIFEST
                && serde_json::from_str::<serde_json::Value>(text).is_ok_and(|v| v.is_array())
        };
        if run.id.is_empty()
            || run.id.len() > 64
            || run.reverted_at.is_some()
            || !run.started_at.is_finite()
            || ![
                &run.adds_json,
                &run.updates_json,
                &run.deletes_json,
                &run.skipped_json,
            ]
            .iter()
            .all(|text| json_ok(text))
        {
            return Err(invalid("提取清单无效"));
        }
        self.connection.execute(
            &format!("INSERT INTO context_extraction_runs({RUN_COLUMNS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,NULL)"),
            params![run.id, run.scope_kind, run.scope_id, run.started_at, run.finished_at, run.adds_json, run.updates_json, run.deletes_json, run.skipped_json],
        )?;
        Ok(())
    }

    pub fn context_extraction_run(
        &self,
        id: &str,
    ) -> Result<Option<ContextExtractionRunRow>, SchemaError> {
        self.connection
            .query_row(
                &format!("SELECT {RUN_COLUMNS} FROM context_extraction_runs WHERE id=?1"),
                [id],
                run_from_row,
            )
            .optional()
            .map_err(SchemaError::from)
    }

    pub fn list_context_extraction_runs(
        &self,
        scope_kind: &str,
        scope_id: &str,
        limit: usize,
    ) -> Result<Vec<ContextExtractionRunRow>, SchemaError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM context_extraction_runs WHERE scope_kind=?1 AND scope_id=?2 ORDER BY started_at DESC,id LIMIT ?3"
        ))?;
        let rows = statement.query_map(
            params![scope_kind, scope_id, limit.min(MAX_LIST) as i64],
            run_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// One-shot: a manifest can be reverted only once.
    pub fn mark_context_extraction_reverted(
        &self,
        id: &str,
        now: f64,
    ) -> Result<bool, SchemaError> {
        if !now.is_finite() {
            return Err(invalid("时间无效"));
        }
        Ok(self.connection.execute(
            "UPDATE context_extraction_runs SET reverted_at=?2 WHERE id=?1 AND reverted_at IS NULL",
            params![id, now],
        )? == 1)
    }
}

/// Live, non-invalidated, unexpired rows of one scope. Shared by both search paths.
const SEARCHABLE: &str = "m.scope_kind=?1 AND m.scope_id=?2 AND m.deleted_at IS NULL AND m.verification<>'invalidated' AND (m.expires_at IS NULL OR m.expires_at>?3)";

impl RuntimeDb {
    /// NV19-04 full-text search (schema v11, FTS5 trigram). Each term must be at least three
    /// characters; terms are quoted as phrases and OR-ed, best bm25 first.
    pub fn match_context_memories(
        &self,
        scope_kind: &str,
        scope_id: &str,
        terms: &[String],
        now: f64,
        limit: usize,
    ) -> Result<Vec<ContextMemoryRow>, SchemaError> {
        if terms.is_empty()
            || terms.len() > 64
            || terms
                .iter()
                .any(|t| t.chars().count() < 3 || t.chars().count() > 64)
            || !now.is_finite()
        {
            return Err(invalid("检索词无效"));
        }
        let query = terms
            .iter()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        let columns = COLUMNS
            .split(',')
            .map(|c| format!("m.{c}"))
            .collect::<Vec<_>>()
            .join(",");
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {columns} FROM context_memories_fts f JOIN context_memories m ON m.rowid=f.rowid
             WHERE context_memories_fts MATCH ?4 AND {SEARCHABLE} ORDER BY bm25(context_memories_fts) LIMIT ?5"
        ))?;
        let rows = statement.query_map(
            params![scope_kind, scope_id, now, query, limit.min(MAX_LIST) as i64],
            from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Substring search for short terms (one or two characters), which trigram cannot index.
    /// Rows containing any of `needles` (OR), newest first.
    pub fn like_context_memories(
        &self,
        scope_kind: &str,
        scope_id: &str,
        needles: &[String],
        now: f64,
        limit: usize,
    ) -> Result<Vec<ContextMemoryRow>, SchemaError> {
        if needles.is_empty()
            || needles.len() > 32
            || needles
                .iter()
                .any(|n| n.trim().is_empty() || n.chars().count() > 64)
            || !now.is_finite()
        {
            return Err(invalid("检索词无效"));
        }
        // `!` as the LIKE escape character: user text cannot act as a wildcard.
        let patterns: Vec<String> = needles
            .iter()
            .map(|n| {
                format!(
                    "%{}%",
                    n.replace('!', "!!").replace('%', "!%").replace('_', "!_")
                )
            })
            .collect();
        let any = (0..patterns.len())
            .map(|i| format!("m.content LIKE ?{} ESCAPE '!'", i + 5))
            .collect::<Vec<_>>()
            .join(" OR ");
        let columns = COLUMNS
            .split(',')
            .map(|c| format!("m.{c}"))
            .collect::<Vec<_>>()
            .join(",");
        let mut statement = self.connection.prepare(&format!(
            "SELECT {columns} FROM context_memories m WHERE ({any}) AND {SEARCHABLE} ORDER BY m.updated_at DESC,m.id LIMIT ?4"
        ))?;
        let limit = limit.min(MAX_LIST) as i64;
        let mut values: Vec<&dyn rusqlite::ToSql> = vec![&scope_kind, &scope_id, &now, &limit];
        values.extend(patterns.iter().map(|p| p as &dyn rusqlite::ToSql));
        let rows = statement.query_map(values.as_slice(), from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
