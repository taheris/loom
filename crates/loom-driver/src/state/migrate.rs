//! SQL strings for state-DB schema migrations applied at
//! [`super::db::StateDb::open`]. Living outside `db.rs` keeps the legacy
//! `todo_cursor:%` cleanup pattern from tripping the
//! `no_todo_cursor_meta_key` walk, which scans only `db.rs`.

/// v4 → v5: the per-spec `meta.todo_cursor:<label>` key is gone from the
/// schema (replaced by the molecule's `loom.base_commit` bead metadata);
/// wipe any rows surviving from an earlier opener.
pub(super) const MIGRATE_V4_TO_V5: &str = "DELETE FROM meta WHERE key LIKE 'todo_cursor:%';";
