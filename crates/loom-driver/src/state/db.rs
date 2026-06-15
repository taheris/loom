use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::bd::BdError;
use crate::identifier::{MoleculeId, SpecLabel};

use super::error::CacheError;

const SCHEMA_VERSION: &str = "8";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS specs (
    label     TEXT PRIMARY KEY,
    spec_path TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS spec_epics (
    spec_label  TEXT PRIMARY KEY REFERENCES specs(label),
    epic_id     TEXT NOT NULL,
    todo_cursor TEXT
);
CREATE TABLE IF NOT EXISTS work_epics (
    epic_id          TEXT PRIMARY KEY,
    todo_head        TEXT,
    todo_fingerprint TEXT,
    is_active        INTEGER NOT NULL DEFAULT 0,
    iteration_count  INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS companions (
    spec_label     TEXT NOT NULL REFERENCES specs(label),
    companion_path TEXT NOT NULL,
    PRIMARY KEY (spec_label, companion_path)
);
CREATE TABLE IF NOT EXISTS notes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    spec_label TEXT NOT NULL REFERENCES specs(label) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    text       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_notes_spec_kind ON notes(spec_label, kind);
CREATE TABLE IF NOT EXISTS criterion_status (
    spec_label        TEXT NOT NULL REFERENCES specs(label),
    criterion_id      TEXT NOT NULL,
    annotation_json   TEXT NOT NULL,
    result            TEXT NOT NULL,
    last_timestamp_ms INTEGER,
    last_commit       TEXT,
    evidence          TEXT,
    PRIMARY KEY (spec_label, criterion_id)
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

const DROP_AND_RECREATE: &str = "
DROP TABLE IF EXISTS criterion_status;
DROP TABLE IF EXISTS notes;
DROP TABLE IF EXISTS companions;
DROP TABLE IF EXISTS work_epics;
DROP TABLE IF EXISTS spec_epics;
DROP TABLE IF EXISTS molecules;
DROP TABLE IF EXISTS current_molecule;
DROP TABLE IF EXISTS specs;
DROP TABLE IF EXISTS meta;
";

/// Owned handle to the SQLite cache database. Wraps the connection in a
/// `Mutex` so the type is `Send + Sync`; the underlying `rusqlite::Connection`
/// is `!Sync`.
pub struct CacheDb {
    conn: Mutex<Connection>,
}

/// One row of the `specs` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecRow {
    pub label: SpecLabel,
    pub spec_path: String,
}

/// Cached mirror of a durable per-spec epic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecEpicRow {
    pub spec_label: SpecLabel,
    pub epic_id: MoleculeId,
    pub todo_cursor: Option<String>,
}

/// Cached mirror of a pending or active work epic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkEpicRow {
    pub epic_id: MoleculeId,
    pub todo_head: Option<String>,
    pub todo_fingerprint: Option<String>,
    pub is_active: bool,
    pub iteration_count: u32,
}

/// Cached verifier evidence for one success criterion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionEvidenceRow {
    pub spec_label: SpecLabel,
    pub criterion_id: String,
    pub annotation_json: String,
    pub result: String,
    pub last_timestamp_ms: Option<i64>,
    pub last_commit: Option<String>,
    pub evidence: Option<String>,
}

/// One row of the `notes` table. `kind` is free-form (default
/// `implementation`); `created_at_ms` is unix-epoch milliseconds for
/// chronological ordering on `list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRow {
    pub id: i64,
    pub spec_label: String,
    pub kind: String,
    pub text: String,
    pub created_at_ms: i64,
}

/// Bead-metadata writer injected into
/// [`CacheDb::consume_notes_and_refresh_base_commit`]. The callback
/// receives the molecule id whose epic carries `loom.base_commit` and
/// the new commit value; failure surfaces as
/// [`CacheError::BdUpdate`](super::error::CacheError::BdUpdate) and
/// rolls back the SQLite writes that share the gate's transaction.
pub type BdUpdateFn = Box<dyn Fn(&MoleculeId, &str) -> Result<(), BdError>>;

/// Compatibility projection for older work-epic call sites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoleculeRow {
    pub id: MoleculeId,
    pub spec_label: SpecLabel,
    pub base_commit: Option<String>,
    pub iteration_count: u32,
}

impl CacheDb {
    /// Open or create a cache DB at `path`, applying schema migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CacheError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).map_err(|source| CacheError::OpenDb {
            path: path.to_path_buf(),
            source,
        })?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA)?;
        apply_migrations(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Delete the file at `path` (if any) and re-open with a fresh schema.
    /// Used by `loom init --rebuild` to recover from a corrupted DB file.
    pub fn recreate(path: impl AsRef<Path>) -> Result<Self, CacheError> {
        let path = path.as_ref();
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Self::open(path)
    }

    /// Look up a single spec row by label.
    pub fn spec(&self, label: &SpecLabel) -> Result<SpecRow, CacheError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT label, spec_path FROM specs WHERE label = ?1",
            params![label.as_str()],
            row_to_spec,
        )
        .optional()?
        .ok_or_else(|| CacheError::SpecNotFound {
            label: label.to_string(),
        })
    }

    /// Look up a cached work-epic projection by id, or `None` when no row matches.
    pub fn molecule(&self, id: &MoleculeId) -> Result<Option<MoleculeRow>, CacheError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT w.epic_id, s.spec_label, w.todo_head, w.iteration_count
             FROM work_epics w
             JOIN spec_epics s ON s.epic_id = w.epic_id
             WHERE w.epic_id = ?1",
            params![id.as_str()],
            row_to_molecule,
        )
        .optional()?
        .transpose()
    }

    /// Read the cached work-epic projection associated with a spec, if any.
    pub fn molecule_for_spec(&self, label: &SpecLabel) -> Result<Option<MoleculeRow>, CacheError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT w.epic_id, s.spec_label, w.todo_head, w.iteration_count
             FROM spec_epics s
             JOIN work_epics w ON w.epic_id = s.epic_id
             WHERE s.spec_label = ?1",
            params![label.as_str()],
            row_to_molecule,
        )
        .optional()?
        .transpose()
    }

    /// Insert or update an indexed spec row.
    pub fn upsert_spec(&self, label: &SpecLabel, spec_path: &str) -> Result<(), CacheError> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO specs(label, spec_path) VALUES (?1, ?2)
             ON CONFLICT(label) DO UPDATE SET spec_path = excluded.spec_path",
            params![label.as_str(), spec_path],
        )?;
        Ok(())
    }

    /// Insert or update a cached spec-epic mirror row.
    pub fn upsert_spec_epic(&self, row: &SpecEpicRow) -> Result<(), CacheError> {
        self.ensure_spec_row(&row.spec_label)?;
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO spec_epics(spec_label, epic_id, todo_cursor) VALUES (?1, ?2, ?3)
             ON CONFLICT(spec_label) DO UPDATE SET
                 epic_id = excluded.epic_id,
                 todo_cursor = excluded.todo_cursor",
            params![
                row.spec_label.as_str(),
                row.epic_id.as_str(),
                row.todo_cursor.as_deref(),
            ],
        )?;
        Ok(())
    }

    /// Read a cached spec-epic mirror row.
    pub fn spec_epic(&self, label: &SpecLabel) -> Result<Option<SpecEpicRow>, CacheError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT spec_label, epic_id, todo_cursor FROM spec_epics WHERE spec_label = ?1",
            params![label.as_str()],
            row_to_spec_epic,
        )
        .optional()?
        .transpose()
    }

    /// Insert or update a cached work-epic mirror row.
    pub fn upsert_work_epic(&self, row: &WorkEpicRow) -> Result<(), CacheError> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO work_epics(epic_id, todo_head, todo_fingerprint, is_active, iteration_count)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(epic_id) DO UPDATE SET
                 todo_head = excluded.todo_head,
                 todo_fingerprint = excluded.todo_fingerprint,
                 is_active = excluded.is_active,
                 iteration_count = excluded.iteration_count",
            params![
                row.epic_id.as_str(),
                row.todo_head.as_deref(),
                row.todo_fingerprint.as_deref(),
                if row.is_active { 1_i64 } else { 0_i64 },
                i64::from(row.iteration_count),
            ],
        )?;
        Ok(())
    }

    /// Read a cached work-epic mirror row.
    pub fn work_epic(&self, epic_id: &MoleculeId) -> Result<Option<WorkEpicRow>, CacheError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT epic_id, todo_head, todo_fingerprint, is_active, iteration_count
             FROM work_epics WHERE epic_id = ?1",
            params![epic_id.as_str()],
            row_to_work_epic,
        )
        .optional()?
        .transpose()
    }

    /// List cached work epics sorted by id.
    pub fn work_epics(&self) -> Result<Vec<WorkEpicRow>, CacheError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT epic_id, todo_head, todo_fingerprint, is_active, iteration_count
             FROM work_epics ORDER BY epic_id",
        )?;
        let rows = stmt
            .query_map([], row_to_work_epic)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().collect()
    }

    /// Insert or update cached criterion evidence.
    pub fn upsert_criterion_evidence(&self, row: &CriterionEvidenceRow) -> Result<(), CacheError> {
        self.ensure_spec_row(&row.spec_label)?;
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO criterion_status(
                 spec_label, criterion_id, annotation_json, result,
                 last_timestamp_ms, last_commit, evidence
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(spec_label, criterion_id) DO UPDATE SET
                 annotation_json = excluded.annotation_json,
                 result = excluded.result,
                 last_timestamp_ms = excluded.last_timestamp_ms,
                 last_commit = excluded.last_commit,
                 evidence = excluded.evidence",
            params![
                row.spec_label.as_str(),
                row.criterion_id.as_str(),
                row.annotation_json.as_str(),
                row.result.as_str(),
                row.last_timestamp_ms,
                row.last_commit.as_deref(),
                row.evidence.as_deref(),
            ],
        )?;
        Ok(())
    }

    /// Read cached criterion evidence for one spec.
    pub fn criterion_evidence_for_spec(
        &self,
        label: &SpecLabel,
    ) -> Result<Vec<CriterionEvidenceRow>, CacheError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT spec_label, criterion_id, annotation_json, result,
                    last_timestamp_ms, last_commit, evidence
             FROM criterion_status WHERE spec_label = ?1 ORDER BY criterion_id",
        )?;
        let rows = stmt
            .query_map(params![label.as_str()], row_to_criterion_evidence)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().collect()
    }

    /// Replace the companion rows for `label` with `paths`. Inserts a `specs`
    /// row for `label` if none exists yet so a fresh `loom plan` cycle does
    /// not fail before `loom todo` populates the rest of the row.
    ///
    /// Used by `loom plan` after the interactive interview exits to land the
    /// declared `## Companions` paths in the cache DB without rebuilding the
    /// whole schema.
    pub fn replace_companions(
        &self,
        label: &SpecLabel,
        paths: &[String],
    ) -> Result<(), CacheError> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO specs(label, spec_path) VALUES (?1, ?2)",
            params![label.as_str(), default_spec_path(label)],
        )?;
        conn.execute(
            "DELETE FROM companions WHERE spec_label = ?1",
            params![label.as_str()],
        )?;
        for path in paths {
            conn.execute(
                "INSERT OR IGNORE INTO companions(spec_label, companion_path)
                 VALUES (?1, ?2)",
                params![label.as_str(), path],
            )?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // `notes` table CRUD. Backs the `loom note` CLI.
    // -----------------------------------------------------------------

    /// Atomically replace every note for `(spec_label, kind)` with the
    /// supplied set. Performs `DELETE` + N `INSERT` in a single tx so a
    /// partial failure leaves the prior set intact.
    pub fn notes_set(
        &self,
        spec_label: &SpecLabel,
        kind: &str,
        notes: &[String],
        created_at_ms: i64,
    ) -> Result<(), CacheError> {
        self.ensure_spec_row(spec_label)?;
        let mut conn = self.conn.lock().map_err(|_| CacheError::Poisoned)?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM notes WHERE spec_label = ?1 AND kind = ?2",
            params![spec_label.as_str(), kind],
        )?;
        for text in notes {
            tx.execute(
                "INSERT INTO notes(spec_label, kind, text, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![spec_label.as_str(), kind, text.as_str(), created_at_ms],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Append a single note. Returns its row id.
    pub fn notes_add(
        &self,
        spec_label: &SpecLabel,
        kind: &str,
        text: &str,
        created_at_ms: i64,
    ) -> Result<i64, CacheError> {
        self.ensure_spec_row(spec_label)?;
        let conn = self.conn.lock().map_err(|_| CacheError::Poisoned)?;
        conn.execute(
            "INSERT INTO notes(spec_label, kind, text, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![spec_label.as_str(), kind, text, created_at_ms],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Delete every note for `(spec_label, kind)`. Pass `kind = None`
    /// to clear all kinds.
    pub fn notes_clear(
        &self,
        spec_label: &SpecLabel,
        kind: Option<&str>,
    ) -> Result<(), CacheError> {
        let conn = self.conn.lock().map_err(|_| CacheError::Poisoned)?;
        if let Some(k) = kind {
            conn.execute(
                "DELETE FROM notes WHERE spec_label = ?1 AND kind = ?2",
                params![spec_label.as_str(), k],
            )?;
        } else {
            conn.execute(
                "DELETE FROM notes WHERE spec_label = ?1",
                params![spec_label.as_str()],
            )?;
        }
        Ok(())
    }

    /// List notes by `(spec_label, kind)`. `spec_label = None` widens
    /// to all specs; `kind = None` widens to all kinds. Always ordered
    /// by `id` ascending (chronological).
    pub fn notes_list(
        &self,
        spec_label: Option<&SpecLabel>,
        kind: Option<&str>,
    ) -> Result<Vec<NoteRow>, CacheError> {
        let conn = self.conn.lock().map_err(|_| CacheError::Poisoned)?;
        let (sql, args) = match (spec_label, kind) {
            (Some(label), Some(k)) => (
                "SELECT id, spec_label, kind, text, created_at FROM notes \
                 WHERE spec_label = ?1 AND kind = ?2 ORDER BY id ASC",
                vec![label.as_str().to_string(), k.to_string()],
            ),
            (Some(label), None) => (
                "SELECT id, spec_label, kind, text, created_at FROM notes \
                 WHERE spec_label = ?1 ORDER BY id ASC",
                vec![label.as_str().to_string()],
            ),
            (None, Some(k)) => (
                "SELECT id, spec_label, kind, text, created_at FROM notes \
                 WHERE kind = ?1 ORDER BY id ASC",
                vec![k.to_string()],
            ),
            (None, None) => (
                "SELECT id, spec_label, kind, text, created_at FROM notes \
                 ORDER BY id ASC",
                vec![],
            ),
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args), |row| {
                Ok(NoteRow {
                    id: row.get(0)?,
                    spec_label: row.get::<_, String>(1)?,
                    kind: row.get(2)?,
                    text: row.get(3)?,
                    created_at_ms: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Productive-completion gate: delete every implementation-kind note
    /// for `label`, advance the local `molecules.base_commit` cache for
    /// `mol_id` to `new_base_commit`, and run the durable bead-metadata
    /// write through `bd_update` — all under one SQLite transaction. A
    /// closure failure (the bead-metadata write) propagates as
    /// [`CacheError::BdUpdate`] and the transaction rolls back so the
    /// local cache stays aligned with the pre-write Beads state.
    pub fn consume_notes_and_refresh_base_commit(
        &self,
        label: &SpecLabel,
        mol_id: &MoleculeId,
        new_base_commit: &str,
        bd_update: BdUpdateFn,
    ) -> Result<(), CacheError> {
        let mut conn = self.conn.lock().map_err(|_| CacheError::Poisoned)?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM notes WHERE spec_label = ?1 AND kind = 'implementation'",
            params![label.as_str()],
        )?;
        tx.execute(
            "UPDATE work_epics SET todo_head = ?1 WHERE epic_id = ?2",
            params![new_base_commit, mol_id.as_str()],
        )?;
        bd_update(mol_id, new_base_commit)?;
        tx.commit()?;
        Ok(())
    }

    /// Remove a single note by its row id.
    pub fn notes_rm(&self, id: i64) -> Result<(), CacheError> {
        let conn = self.conn.lock().map_err(|_| CacheError::Poisoned)?;
        let n = conn.execute("DELETE FROM notes WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(CacheError::SpecNotFound {
                label: format!("note id {id}"),
            });
        }
        Ok(())
    }

    /// Ensure a `specs` row exists for `label` — the foreign-key
    /// constraint on `notes.spec_label` requires it. Idempotent.
    fn ensure_spec_row(&self, label: &SpecLabel) -> Result<(), CacheError> {
        let conn = self.conn.lock().map_err(|_| CacheError::Poisoned)?;
        conn.execute(
            "INSERT OR IGNORE INTO specs(label, spec_path) VALUES (?1, ?2)",
            params![label.as_str(), default_spec_path(label)],
        )?;
        Ok(())
    }

    /// Read all companion paths recorded for `label` (sorted for determinism).
    pub fn companions(&self, label: &SpecLabel) -> Result<Vec<String>, CacheError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT companion_path FROM companions
             WHERE spec_label = ?1 ORDER BY companion_path",
        )?;
        let rows = stmt.query_map(params![label.as_str()], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Increment the iteration counter for `mol_id` and return the new value.
    pub fn increment_iteration(&self, mol_id: &MoleculeId) -> Result<u32, CacheError> {
        let conn = self.lock_conn()?;
        let updated = conn.execute(
            "UPDATE work_epics SET iteration_count = iteration_count + 1 WHERE epic_id = ?1",
            params![mol_id.as_str()],
        )?;
        if updated == 0 {
            return Err(CacheError::SpecNotFound {
                label: mol_id.to_string(),
            });
        }
        let count: i64 = conn.query_row(
            "SELECT iteration_count FROM work_epics WHERE epic_id = ?1",
            params![mol_id.as_str()],
            |r| r.get(0),
        )?;
        Ok(count.max(0) as u32)
    }

    /// Set the iteration counter for `mol_id` to `value`. Errors if no row
    /// matches (consistent with [`Self::increment_iteration`]).
    pub fn set_iteration(&self, mol_id: &MoleculeId, value: u32) -> Result<(), CacheError> {
        let conn = self.lock_conn()?;
        let updated = conn.execute(
            "UPDATE work_epics SET iteration_count = ?1 WHERE epic_id = ?2",
            params![value, mol_id.as_str()],
        )?;
        if updated == 0 {
            return Err(CacheError::SpecNotFound {
                label: mol_id.to_string(),
            });
        }
        Ok(())
    }

    /// Reset the iteration counter for `mol_id` to zero.
    pub fn reset_iteration(&self, mol_id: &MoleculeId) -> Result<(), CacheError> {
        self.set_iteration(mol_id, 0)
    }

    /// Borrow the underlying connection for code inside `state/` only.
    pub(super) fn with_conn<R>(
        &self,
        f: impl FnOnce(&Connection) -> Result<R, CacheError>,
    ) -> Result<R, CacheError> {
        let conn = self.lock_conn()?;
        f(&conn)
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, CacheError> {
        self.conn.lock().map_err(|_| CacheError::Poisoned)
    }
}

pub(super) fn drop_and_recreate(conn: &Connection) -> Result<(), CacheError> {
    conn.execute_batch(DROP_AND_RECREATE)?;
    conn.execute_batch(SCHEMA)?;
    write_schema_version(conn, SCHEMA_VERSION)?;
    Ok(())
}

fn apply_migrations(conn: &Connection) -> Result<(), CacheError> {
    let from = read_schema_version(conn)?;
    match from.as_deref() {
        None => write_schema_version(conn, SCHEMA_VERSION)?,
        Some(SCHEMA_VERSION) => {}
        Some("1" | "2" | "3" | "4" | "5" | "6" | "7") => drop_and_recreate(conn)?,
        Some(other) => {
            return Err(CacheError::UnknownSchemaVersion {
                version: other.to_string(),
            });
        }
    }
    Ok(())
}

fn read_schema_version(conn: &Connection) -> Result<Option<String>, CacheError> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(value)
}

fn write_schema_version(conn: &Connection, version: &str) -> Result<(), CacheError> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![version],
    )?;
    Ok(())
}

fn row_to_spec(row: &rusqlite::Row<'_>) -> rusqlite::Result<SpecRow> {
    let label: String = row.get(0)?;
    let spec_path: String = row.get(1)?;
    Ok(SpecRow {
        label: SpecLabel::new(label),
        spec_path,
    })
}

fn row_to_spec_epic(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<SpecEpicRow, CacheError>> {
    let spec_label: String = row.get(0)?;
    let epic_id: String = row.get(1)?;
    let todo_cursor: Option<String> = row.get(2)?;
    Ok(Ok(SpecEpicRow {
        spec_label: SpecLabel::new(spec_label),
        epic_id: MoleculeId::new(epic_id),
        todo_cursor,
    }))
}

fn row_to_work_epic(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<WorkEpicRow, CacheError>> {
    let epic_id: String = row.get(0)?;
    let todo_head: Option<String> = row.get(1)?;
    let todo_fingerprint: Option<String> = row.get(2)?;
    let is_active: i64 = row.get(3)?;
    let iteration_count: i64 = row.get(4)?;
    Ok(Ok(WorkEpicRow {
        epic_id: MoleculeId::new(epic_id),
        todo_head,
        todo_fingerprint,
        is_active: is_active != 0,
        iteration_count: iteration_count.max(0) as u32,
    }))
}

fn row_to_criterion_evidence(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<CriterionEvidenceRow, CacheError>> {
    let spec_label: String = row.get(0)?;
    Ok(Ok(CriterionEvidenceRow {
        spec_label: SpecLabel::new(spec_label),
        criterion_id: row.get(1)?,
        annotation_json: row.get(2)?,
        result: row.get(3)?,
        last_timestamp_ms: row.get(4)?,
        last_commit: row.get(5)?,
        evidence: row.get(6)?,
    }))
}

fn row_to_molecule(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<MoleculeRow, CacheError>> {
    let id: String = row.get(0)?;
    let spec_label: String = row.get(1)?;
    let base_commit: Option<String> = row.get(2)?;
    let iteration_count: i64 = row.get(3)?;
    Ok(Ok(MoleculeRow {
        id: MoleculeId::new(id),
        spec_label: SpecLabel::new(spec_label),
        base_commit,
        iteration_count: iteration_count.max(0) as u32,
    }))
}

fn default_spec_path(label: &SpecLabel) -> String {
    format!("specs/{}.md", label.as_str())
}
