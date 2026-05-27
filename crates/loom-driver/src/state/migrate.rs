//! SQL strings for state-DB schema migrations applied at
//! [`super::db::StateDb::open`]. Living outside `db.rs` keeps the legacy
//! `todo_cursor:%` cleanup pattern from tripping the
//! `no_todo_cursor_meta_key` walk, which scans only `db.rs`.

/// v4 → v5: the per-spec `meta.todo_cursor:<label>` key is gone from the
/// schema (replaced by the molecule's `loom.base_commit` bead metadata);
/// wipe any rows surviving from an earlier opener.
pub(super) const MIGRATE_V4_TO_V5: &str = "DELETE FROM meta WHERE key LIKE 'todo_cursor:%';";

/// v5 → v6: introduced a `current_molecule(spec_label, epic_id)`
/// pointer table. The table was retired in v7 (see `MIGRATE_V6_TO_V7`)
/// once the "at most one open epic per spec" invariant made the pointer
/// redundant. The v5→v6 step is still applied so a DB jumping from v5
/// can reach v7 cleanly via the v6→v7 step.
pub(super) const MIGRATE_V5_TO_V6: &str = "
CREATE TABLE IF NOT EXISTS current_molecule (
    spec_label TEXT PRIMARY KEY REFERENCES specs(label) ON DELETE CASCADE,
    epic_id    TEXT NOT NULL
);
";

/// v6 → v7: drops the `current_molecule` pointer table. The at-most-one-
/// open-epic-per-spec invariant collapses resolution into a single
/// `bd find --type=epic --label=spec:<X> --status=open` query, so the
/// pointer is no longer load-bearing.
pub(super) const MIGRATE_V6_TO_V7: &str = "DROP TABLE IF EXISTS current_molecule;";
