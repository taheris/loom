//! SQL strings for state-DB schema migrations applied at
//! [`super::db::StateDb::open`]. Living outside `db.rs` keeps the legacy
//! `todo_cursor:%` cleanup pattern from tripping the
//! `no_todo_cursor_meta_key` walk, which scans only `db.rs`.

/// v4 → v5: the per-spec `meta.todo_cursor:<label>` key is gone from the
/// schema (replaced by the molecule's `loom.base_commit` bead metadata);
/// wipe any rows surviving from an earlier opener.
pub(super) const MIGRATE_V4_TO_V5: &str = "DELETE FROM meta WHERE key LIKE 'todo_cursor:%';";

/// v5 → v6: introduces `current_molecule(spec_label, epic_id)` — the
/// per-spec pointer to the active epic, replacing `bd list
/// --label=loom:active` lookups.
pub(super) const MIGRATE_V5_TO_V6: &str = "
CREATE TABLE IF NOT EXISTS current_molecule (
    spec_label TEXT PRIMARY KEY REFERENCES specs(label) ON DELETE CASCADE,
    epic_id    TEXT NOT NULL
);
";
