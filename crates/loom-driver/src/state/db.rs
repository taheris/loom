use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::bd::BdError;
use crate::identifier::{MoleculeId, SpecLabel};

use super::error::StateError;
use super::migrate::MIGRATE_V4_TO_V5;

const SCHEMA_VERSION: &str = "5";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS specs (
    label TEXT PRIMARY KEY
);
CREATE TABLE IF NOT EXISTS molecules (
    id              TEXT PRIMARY KEY,
    spec_label      TEXT NOT NULL REFERENCES specs(label),
    base_commit     TEXT,
    iteration_count INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS companions (
    spec_label     TEXT NOT NULL REFERENCES specs(label),
    companion_path TEXT NOT NULL,
    PRIMARY KEY (spec_label, companion_path)
);
-- `kind` lets one bead carry multiple categories of notes (default
-- `implementation`); the `loom note` CLI is the only writer.
CREATE TABLE IF NOT EXISTS notes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    spec_label TEXT NOT NULL REFERENCES specs(label) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    text       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_notes_spec_kind ON notes(spec_label, kind);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- meta rows: current_spec, schema_version
";

const MIGRATE_V1_TO_V2: &str = "ALTER TABLE specs DROP COLUMN spec_path;";
const MIGRATE_V2_TO_V3: &str = "
CREATE TABLE IF NOT EXISTS notes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    spec_label TEXT NOT NULL REFERENCES specs(label) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    text       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_notes_spec_kind ON notes(spec_label, kind);
";
const MIGRATE_V3_TO_V4: &str = "ALTER TABLE specs DROP COLUMN implementation_notes;";

const DROP_AND_RECREATE: &str = "
DROP TABLE IF EXISTS notes;
DROP TABLE IF EXISTS companions;
DROP TABLE IF EXISTS molecules;
DROP TABLE IF EXISTS specs;
DROP TABLE IF EXISTS meta;
";

/// Owned handle to the SQLite state database. Wraps the connection in a
/// `Mutex` so the type is `Send + Sync`; the underlying `rusqlite::Connection`
/// is `!Sync`.
pub struct StateDb {
    conn: Mutex<Connection>,
}

/// One row of the `specs` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecRow {
    pub label: SpecLabel,
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
/// [`StateDb::consume_notes_and_refresh_base_commit`]. The callback
/// receives the molecule id whose epic carries `loom.base_commit` and
/// the new commit value; failure surfaces as
/// [`StateError::BdUpdate`](super::error::StateError::BdUpdate) and
/// rolls back the SQLite writes that share the gate's transaction.
pub type BdUpdateFn = Box<dyn Fn(&MoleculeId, &str) -> Result<(), BdError>>;

/// One row of the `molecules` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoleculeRow {
    pub id: MoleculeId,
    pub spec_label: SpecLabel,
    pub base_commit: Option<String>,
    pub iteration_count: u32,
}

impl StateDb {
    /// Open or create a state DB at `path`, applying schema migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StateError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).map_err(|source| StateError::OpenDb {
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
    pub fn recreate(path: impl AsRef<Path>) -> Result<Self, StateError> {
        let path = path.as_ref();
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Self::open(path)
    }

    /// Look up a single spec row by label.
    pub fn spec(&self, label: &SpecLabel) -> Result<SpecRow, StateError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT label FROM specs WHERE label = ?1",
            params![label.as_str()],
            row_to_spec,
        )
        .optional()?
        .ok_or_else(|| StateError::SpecNotFound {
            label: label.to_string(),
        })
    }

    /// Most recent active molecule for the given spec, or `None` if there is
    /// no active molecule. Resolves ties with `ORDER BY id ASC`.
    pub fn active_molecule(&self, label: &SpecLabel) -> Result<Option<MoleculeRow>, StateError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT id, spec_label, base_commit, iteration_count
             FROM molecules WHERE spec_label = ?1 ORDER BY id ASC LIMIT 1",
            params![label.as_str()],
            row_to_molecule,
        )
        .optional()?
        .transpose()
    }

    /// Read the `current_spec` meta row.
    pub fn current_spec(&self) -> Result<Option<SpecLabel>, StateError> {
        let conn = self.lock_conn()?;
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'current_spec'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(value.map(SpecLabel::new))
    }

    /// Write the `current_spec` meta row.
    pub fn set_current_spec(&self, label: &SpecLabel) -> Result<(), StateError> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('current_spec', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![label.as_str()],
        )?;
        Ok(())
    }

    /// Replace the companion rows for `label` with `paths`. Inserts a `specs`
    /// row for `label` if none exists yet so a fresh `loom plan` cycle does
    /// not fail before `loom todo` populates the rest of the row.
    ///
    /// Used by `loom plan` after the interactive interview exits to land the
    /// declared `## Companions` paths in the state DB without rebuilding the
    /// whole schema.
    pub fn replace_companions(
        &self,
        label: &SpecLabel,
        paths: &[String],
    ) -> Result<(), StateError> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO specs(label) VALUES (?1)",
            params![label.as_str()],
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
    ) -> Result<(), StateError> {
        self.ensure_spec_row(spec_label)?;
        let mut conn = self.conn.lock().map_err(|_| StateError::Poisoned)?;
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
    ) -> Result<i64, StateError> {
        self.ensure_spec_row(spec_label)?;
        let conn = self.conn.lock().map_err(|_| StateError::Poisoned)?;
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
    ) -> Result<(), StateError> {
        let conn = self.conn.lock().map_err(|_| StateError::Poisoned)?;
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
    ) -> Result<Vec<NoteRow>, StateError> {
        let conn = self.conn.lock().map_err(|_| StateError::Poisoned)?;
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
    /// [`StateError::BdUpdate`] and the transaction rolls back so the
    /// local cache stays aligned with the pre-write Beads state.
    pub fn consume_notes_and_refresh_base_commit(
        &self,
        label: &SpecLabel,
        mol_id: &MoleculeId,
        new_base_commit: &str,
        bd_update: BdUpdateFn,
    ) -> Result<(), StateError> {
        let mut conn = self.conn.lock().map_err(|_| StateError::Poisoned)?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM notes WHERE spec_label = ?1 AND kind = 'implementation'",
            params![label.as_str()],
        )?;
        tx.execute(
            "UPDATE molecules SET base_commit = ?1 WHERE id = ?2",
            params![new_base_commit, mol_id.as_str()],
        )?;
        bd_update(mol_id, new_base_commit)?;
        tx.commit()?;
        Ok(())
    }

    /// Remove a single note by its row id.
    pub fn notes_rm(&self, id: i64) -> Result<(), StateError> {
        let conn = self.conn.lock().map_err(|_| StateError::Poisoned)?;
        let n = conn.execute("DELETE FROM notes WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(StateError::SpecNotFound {
                label: format!("note id {id}"),
            });
        }
        Ok(())
    }

    /// Ensure a `specs` row exists for `label` — the foreign-key
    /// constraint on `notes.spec_label` requires it. Idempotent.
    fn ensure_spec_row(&self, label: &SpecLabel) -> Result<(), StateError> {
        let conn = self.conn.lock().map_err(|_| StateError::Poisoned)?;
        conn.execute(
            "INSERT OR IGNORE INTO specs(label) VALUES (?1)",
            params![label.as_str()],
        )?;
        Ok(())
    }

    /// Read all companion paths recorded for `label` (sorted for determinism).
    pub fn companions(&self, label: &SpecLabel) -> Result<Vec<String>, StateError> {
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
    pub fn increment_iteration(&self, mol_id: &MoleculeId) -> Result<u32, StateError> {
        let conn = self.lock_conn()?;
        let updated = conn.execute(
            "UPDATE molecules SET iteration_count = iteration_count + 1 WHERE id = ?1",
            params![mol_id.as_str()],
        )?;
        if updated == 0 {
            return Err(StateError::SpecNotFound {
                label: mol_id.to_string(),
            });
        }
        let count: i64 = conn.query_row(
            "SELECT iteration_count FROM molecules WHERE id = ?1",
            params![mol_id.as_str()],
            |r| r.get(0),
        )?;
        Ok(count.max(0) as u32)
    }

    /// Set the iteration counter for `mol_id` to `value`. Errors if no row
    /// matches (consistent with [`Self::increment_iteration`]).
    pub fn set_iteration(&self, mol_id: &MoleculeId, value: u32) -> Result<(), StateError> {
        let conn = self.lock_conn()?;
        let updated = conn.execute(
            "UPDATE molecules SET iteration_count = ?1 WHERE id = ?2",
            params![value, mol_id.as_str()],
        )?;
        if updated == 0 {
            return Err(StateError::SpecNotFound {
                label: mol_id.to_string(),
            });
        }
        Ok(())
    }

    /// Reset the iteration counter for `mol_id` to zero.
    pub fn reset_iteration(&self, mol_id: &MoleculeId) -> Result<(), StateError> {
        self.set_iteration(mol_id, 0)
    }

    /// Borrow the underlying connection for code inside `state/` only.
    pub(super) fn with_conn<R>(
        &self,
        f: impl FnOnce(&Connection) -> Result<R, StateError>,
    ) -> Result<R, StateError> {
        let conn = self.lock_conn()?;
        f(&conn)
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StateError> {
        self.conn.lock().map_err(|_| StateError::Poisoned)
    }
}

pub(super) fn drop_and_recreate(conn: &Connection) -> Result<(), StateError> {
    conn.execute_batch(DROP_AND_RECREATE)?;
    conn.execute_batch(SCHEMA)?;
    write_schema_version(conn, SCHEMA_VERSION)?;
    Ok(())
}

fn apply_migrations(conn: &Connection) -> Result<(), StateError> {
    let from = read_schema_version(conn)?;
    match from.as_deref() {
        None => write_schema_version(conn, SCHEMA_VERSION)?,
        Some("1") => {
            conn.execute_batch(MIGRATE_V1_TO_V2)?;
            conn.execute_batch(MIGRATE_V2_TO_V3)?;
            conn.execute_batch(MIGRATE_V3_TO_V4)?;
            conn.execute_batch(MIGRATE_V4_TO_V5)?;
            write_schema_version(conn, SCHEMA_VERSION)?;
        }
        Some("2") => {
            conn.execute_batch(MIGRATE_V2_TO_V3)?;
            conn.execute_batch(MIGRATE_V3_TO_V4)?;
            conn.execute_batch(MIGRATE_V4_TO_V5)?;
            write_schema_version(conn, SCHEMA_VERSION)?;
        }
        Some("3") => {
            conn.execute_batch(MIGRATE_V3_TO_V4)?;
            conn.execute_batch(MIGRATE_V4_TO_V5)?;
            write_schema_version(conn, SCHEMA_VERSION)?;
        }
        Some("4") => {
            conn.execute_batch(MIGRATE_V4_TO_V5)?;
            write_schema_version(conn, SCHEMA_VERSION)?;
        }
        Some("5") => {}
        Some(other) => {
            return Err(StateError::UnknownSchemaVersion {
                version: other.to_string(),
            });
        }
    }
    Ok(())
}

fn read_schema_version(conn: &Connection) -> Result<Option<String>, StateError> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(value)
}

fn write_schema_version(conn: &Connection, version: &str) -> Result<(), StateError> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![version],
    )?;
    Ok(())
}

fn row_to_spec(row: &rusqlite::Row<'_>) -> rusqlite::Result<SpecRow> {
    let label: String = row.get(0)?;
    Ok(SpecRow {
        label: SpecLabel::new(label),
    })
}

fn row_to_molecule(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<MoleculeRow, StateError>> {
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
