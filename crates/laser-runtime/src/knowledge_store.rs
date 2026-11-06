//! NV19-05 layered knowledge base rows (schema v12). Only directory summaries (L0/L1) and
//! file fingerprints are stored; original text (L2) stays on disk and is read on demand by
//! `moss-voice-server::context::knowledge` after re-checking the fingerprint.
use crate::db::{RuntimeDb, SchemaError};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KnowledgeRootRow {
    pub id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub label: String,
    /// Canonical local path chosen by the user. Private: never sent to a model.
    pub root_path: String,
    pub revision: i64,
    pub created_at: f64,
    pub indexed_at: Option<f64>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KnowledgeDirRow {
    pub id: String,
    pub root_id: String,
    pub rel_dir: String,
    pub l0: String,
    pub l1: String,
    pub file_count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KnowledgeFileRow {
    pub id: String,
    pub root_id: String,
    pub dir_id: String,
    pub rel_path: String,
    pub sha256: String,
    pub size: i64,
    pub mtime: f64,
}

/// v17 passage to index. `text` goes only into the contentless FTS5 table (trigram index,
/// no positions); it is never stored as a column and cannot be selected back.
#[derive(Debug, Clone, PartialEq)]
pub struct KnowledgeChunkRow {
    pub file_id: String,
    pub ord: i64,
    pub text: String,
}

/// v17 passage hit: the directory row it belongs to, plus which passage of which file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KnowledgeChunkHitRow {
    pub dir: KnowledgeDirRow,
    pub file_id: String,
    pub ord: i64,
}

const ROOT_COLUMNS: &str = "id,scope_kind,scope_id,label,root_path,revision,created_at,indexed_at,enabled";
const DIR_COLUMNS: &str = "d.id,d.root_id,d.rel_dir,d.l0,d.l1,d.file_count";
const MAX_LIST: usize = 200;

fn root_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<KnowledgeRootRow> {
    Ok(KnowledgeRootRow {
        id: row.get(0)?,
        scope_kind: row.get(1)?,
        scope_id: row.get(2)?,
        label: row.get(3)?,
        root_path: row.get(4)?,
        revision: row.get(5)?,
        created_at: row.get(6)?,
        indexed_at: row.get(7)?,
        enabled: row.get::<_, i64>(8)? != 0,
    })
}

fn dir_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<KnowledgeDirRow> {
    Ok(KnowledgeDirRow { id: row.get(0)?, root_id: row.get(1)?, rel_dir: row.get(2)?, l0: row.get(3)?, l1: row.get(4)?, file_count: row.get(5)? })
}

fn invalid(message: &str) -> SchemaError {
    SchemaError::InvalidRecord(message.into())
}

impl RuntimeDb {
    /// New roots start at revision 1 and unindexed.
    pub fn insert_knowledge_root(&self, row: &KnowledgeRootRow) -> Result<(), SchemaError> {
        if row.id.is_empty() || row.revision != 1 || row.indexed_at.is_some() || !row.created_at.is_finite() {
            return Err(invalid("资料目录无效"));
        }
        self.connection.execute(
            &format!("INSERT INTO context_knowledge_roots({ROOT_COLUMNS}) VALUES(?1,?2,?3,?4,?5,1,?6,NULL,?7)"),
            params![row.id, row.scope_kind, row.scope_id, row.label, row.root_path, row.created_at, i64::from(row.enabled)],
        )?;
        Ok(())
    }

    pub fn knowledge_root(&self, id: &str) -> Result<Option<KnowledgeRootRow>, SchemaError> {
        self.connection
            .query_row(&format!("SELECT {ROOT_COLUMNS} FROM context_knowledge_roots WHERE id=?1"), [id], root_from_row)
            .optional()
            .map_err(SchemaError::from)
    }

    pub fn knowledge_roots(&self, scope_kind: &str, scope_id: &str) -> Result<Vec<KnowledgeRootRow>, SchemaError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {ROOT_COLUMNS} FROM context_knowledge_roots WHERE scope_kind=?1 AND scope_id=?2 ORDER BY created_at,id LIMIT {MAX_LIST}"
        ))?;
        let rows = statement.query_map(params![scope_kind, scope_id], root_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn all_knowledge_roots(&self) -> Result<Vec<KnowledgeRootRow>, SchemaError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {ROOT_COLUMNS} FROM context_knowledge_roots ORDER BY created_at,id LIMIT {MAX_LIST}"
        ))?;
        let rows = statement.query_map([], root_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn set_knowledge_root_enabled(&self, id: &str, expected_revision: i64, enabled: bool) -> Result<bool, SchemaError> {
        let updated = self.connection.execute(
            "UPDATE context_knowledge_roots SET enabled=?3, revision=revision+1 WHERE id=?1 AND revision=?2",
            params![id, expected_revision, i64::from(enabled)],
        )?;
        Ok(updated == 1)
    }

    /// Replace the whole index of one root atomically, only if the root is still at
    /// `expected_revision`. Removed or renamed files disappear with their old summaries.
    /// `false` means a conflict (another reindex or a deletion won); nothing is written.
    pub fn replace_knowledge_index(&self, root_id: &str, expected_revision: i64, dirs: &[KnowledgeDirRow], files: &[KnowledgeFileRow], now: f64) -> Result<bool, SchemaError> {
        self.replace_knowledge_index_with_chunks(root_id, expected_revision, dirs, files, &[], now)
    }

    /// Same as `replace_knowledge_index`, also replacing the root's passage index (v17).
    pub fn replace_knowledge_index_with_chunks(&self, root_id: &str, expected_revision: i64, dirs: &[KnowledgeDirRow], files: &[KnowledgeFileRow], chunks: &[KnowledgeChunkRow], now: f64) -> Result<bool, SchemaError> {
        if !now.is_finite() || dirs.iter().any(|d| d.root_id != root_id) || files.iter().any(|f| f.root_id != root_id || !dirs.iter().any(|d| d.id == f.dir_id)) {
            return Err(invalid("资料索引无效"));
        }
        let dir_of: std::collections::HashMap<&str, &str> = files.iter().map(|f| (f.id.as_str(), f.dir_id.as_str())).collect();
        if chunks.iter().any(|c| c.ord < 0 || c.text.trim().is_empty() || !dir_of.contains_key(c.file_id.as_str())) {
            return Err(invalid("资料段落无效"));
        }
        let tx = self.connection.unchecked_transaction()?;
        let bumped = tx.execute(
            "UPDATE context_knowledge_roots SET revision=revision+1, indexed_at=?3 WHERE id=?1 AND revision=?2",
            params![root_id, expected_revision, now],
        )?;
        if bumped != 1 {
            return Ok(false);
        }
        delete_chunks(&tx, root_id)?;
        tx.execute("DELETE FROM context_knowledge_files WHERE root_id=?1", [root_id])?;
        tx.execute("DELETE FROM context_knowledge_dirs WHERE root_id=?1", [root_id])?;
        for dir in dirs {
            tx.execute(
                "INSERT INTO context_knowledge_dirs(id,root_id,rel_dir,l0,l1,file_count) VALUES(?1,?2,?3,?4,?5,?6)",
                params![dir.id, dir.root_id, dir.rel_dir, dir.l0, dir.l1, dir.file_count],
            )?;
        }
        for file in files {
            tx.execute(
                "INSERT INTO context_knowledge_files(id,root_id,dir_id,rel_path,sha256,size,mtime) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![file.id, file.root_id, file.dir_id, file.rel_path, file.sha256, file.size, file.mtime],
            )?;
        }
        for chunk in chunks {
            tx.execute(
                "INSERT INTO context_knowledge_chunks(root_id,dir_id,file_id,ord) VALUES(?1,?2,?3,?4)",
                params![root_id, dir_of[chunk.file_id.as_str()], chunk.file_id, chunk.ord],
            )?;
            tx.execute("INSERT INTO context_knowledge_chunks_fts(rowid,text) VALUES(last_insert_rowid(),?1)", [&chunk.text])?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Remove a root with all its summaries and fingerprints (FTS entries go with them).
    pub fn delete_knowledge_root_if_revision(&self, id: &str, expected_revision: i64) -> Result<bool, SchemaError> {
        let tx = self.connection.unchecked_transaction()?;
        if tx.execute("DELETE FROM context_knowledge_roots WHERE id=?1 AND revision=?2", params![id, expected_revision])? != 1 {
            return Ok(false);
        }
        delete_chunks(&tx, id)?;
        tx.execute("DELETE FROM context_knowledge_files WHERE root_id=?1", [id])?;
        tx.execute("DELETE FROM context_knowledge_dirs WHERE root_id=?1", [id])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn knowledge_dirs(&self, root_id: &str) -> Result<Vec<KnowledgeDirRow>, SchemaError> {
        let mut statement = self.connection.prepare(&format!("SELECT {DIR_COLUMNS} FROM context_knowledge_dirs d WHERE d.root_id=?1 ORDER BY d.rel_dir"))?;
        let rows = statement.query_map([root_id], dir_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn knowledge_files(&self, dir_id: &str) -> Result<Vec<KnowledgeFileRow>, SchemaError> {
        let mut statement = self.connection.prepare("SELECT id,root_id,dir_id,rel_path,sha256,size,mtime FROM context_knowledge_files WHERE dir_id=?1 ORDER BY rel_path")?;
        let rows = statement.query_map([dir_id], |r| {
            Ok(KnowledgeFileRow { id: r.get(0)?, root_id: r.get(1)?, dir_id: r.get(2)?, rel_path: r.get(3)?, sha256: r.get(4)?, size: r.get(5)?, mtime: r.get(6)? })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Directory summaries of one scope whose L0 or L1 contains any three-character term
    /// (FTS5 trigram, phrase-quoted), and/or any short term (substring). Union, at most `limit`.
    pub fn search_knowledge_dirs(&self, scope_kind: &str, scope_id: &str, terms: &[String], shorts: &[String], limit: usize) -> Result<Vec<KnowledgeDirRow>, SchemaError> {
        if terms.len() > 64 || shorts.len() > 32 || terms.iter().any(|t| t.chars().count() < 3 || t.chars().count() > 64) || shorts.iter().any(|t| t.trim().is_empty() || t.chars().count() > 64) {
            return Err(invalid("检索词无效"));
        }
        let limit = limit.min(MAX_LIST) as i64;
        let mut found: Vec<KnowledgeDirRow> = Vec::new();
        if !terms.is_empty() {
            let query = terms.iter().map(|t| format!("\"{}\"", t.replace('"', "\"\""))).collect::<Vec<_>>().join(" OR ");
            let mut statement = self.connection.prepare(&format!(
                "SELECT {DIR_COLUMNS} FROM context_knowledge_fts f JOIN context_knowledge_dirs d ON d.rowid=f.rowid
                 JOIN context_knowledge_roots r ON r.id=d.root_id
                 WHERE context_knowledge_fts MATCH ?3 AND r.scope_kind=?1 AND r.scope_id=?2 AND r.enabled=1 ORDER BY bm25(context_knowledge_fts) LIMIT ?4"
            ))?;
            for row in statement.query_map(params![scope_kind, scope_id, query, limit], dir_from_row)? {
                found.push(row?);
            }
        }
        if !shorts.is_empty() {
            // `!` as the LIKE escape character: user text cannot act as a wildcard.
            let patterns: Vec<String> = shorts.iter().map(|n| format!("%{}%", n.replace('!', "!!").replace('%', "!%").replace('_', "!_"))).collect();
            let any = (0..patterns.len()).map(|i| format!("(d.l0 LIKE ?{0} ESCAPE '!' OR d.l1 LIKE ?{0} ESCAPE '!')", i + 4)).collect::<Vec<_>>().join(" OR ");
            let mut statement = self.connection.prepare(&format!(
                "SELECT {DIR_COLUMNS} FROM context_knowledge_dirs d JOIN context_knowledge_roots r ON r.id=d.root_id
                 WHERE ({any}) AND r.scope_kind=?1 AND r.scope_id=?2 AND r.enabled=1 ORDER BY d.rel_dir LIMIT ?3"
            ))?;
            let mut values: Vec<&dyn rusqlite::ToSql> = vec![&scope_kind, &scope_id, &limit];
            values.extend(patterns.iter().map(|p| p as &dyn rusqlite::ToSql));
            for row in statement.query_map(values.as_slice(), dir_from_row)? {
                let row = row?;
                if !found.iter().any(|seen| seen.id == row.id) {
                    found.push(row);
                }
            }
        }
        Ok(found)
    }
}

/// Passage index entries of one root, FTS rows first (contentless_delete allows plain DELETE).
fn delete_chunks(tx: &rusqlite::Transaction<'_>, root_id: &str) -> Result<(), SchemaError> {
    tx.execute("DELETE FROM context_knowledge_chunks_fts WHERE rowid IN (SELECT id FROM context_knowledge_chunks WHERE root_id=?1)", [root_id])?;
    tx.execute("DELETE FROM context_knowledge_chunks WHERE root_id=?1", [root_id])?;
    Ok(())
}

impl RuntimeDb {
    /// v17: passages of enabled roots in one scope matching any three-character term
    /// (phrase-quoted, OR-ed), best bm25 first. Only ids come back — never document text.
    pub fn search_knowledge_chunks(&self, scope_kind: &str, scope_id: &str, terms: &[String], limit: usize) -> Result<Vec<KnowledgeChunkHitRow>, SchemaError> {
        if terms.is_empty() || terms.len() > 64 || terms.iter().any(|t| t.chars().count() < 3 || t.chars().count() > 64) {
            return Err(invalid("检索词无效"));
        }
        let query = terms.iter().map(|t| format!("\"{}\"", t.replace('"', "\"\""))).collect::<Vec<_>>().join(" OR ");
        let mut statement = self.connection.prepare(&format!(
            "SELECT {DIR_COLUMNS}, c.file_id, c.ord FROM context_knowledge_chunks_fts f
             JOIN context_knowledge_chunks c ON c.id=f.rowid
             JOIN context_knowledge_dirs d ON d.id=c.dir_id
             JOIN context_knowledge_roots r ON r.id=c.root_id
             WHERE context_knowledge_chunks_fts MATCH ?3 AND r.scope_kind=?1 AND r.scope_id=?2 AND r.enabled=1
             ORDER BY bm25(context_knowledge_chunks_fts) LIMIT ?4"
        ))?;
        let rows = statement.query_map(params![scope_kind, scope_id, query, limit.min(MAX_LIST) as i64], |row| {
            Ok(KnowledgeChunkHitRow { dir: dir_from_row(row)?, file_id: row.get(6)?, ord: row.get(7)? })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
