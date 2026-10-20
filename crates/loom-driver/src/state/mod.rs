//! Transient local workflow state backing `.loom/state.db`.
//!
//! The schema is owned by `loom-driver` and migrated on `CacheDb::open`. All
//! raw SQL is confined to this module; callers see a typed Rust surface
//! (`CacheDb` plus the row structs returned by its accessors).
//!
//! The cache DB is reconstructable from spec files on disk and active beads
//! via [`CacheDb::rebuild`]; iteration counters reset to 0. Notes are owned
//! by the `loom note` CLI and live in the `SQLite` `notes` table; there
//! is no markdown source of truth for them anymore.

mod companions;
mod db;
mod error;
mod legacy;
mod rebuild;
mod record;

pub use companions::parse_companions;
pub use db::{CacheDb, CriterionEvidenceRow, NoteRow, SpecEpicRow, SpecRow, WorkEpicRow};
pub use error::CacheError;
pub use record::RecordKind;

/// The one local state database for an invocation's controlling workspace.
pub fn path(workspace: &std::path::Path) -> std::path::PathBuf {
    let root = if workspace
        .file_name()
        .is_some_and(|name| name == "integration")
        && workspace
            .parent()
            .is_some_and(|parent| parent.file_name().is_some_and(|name| name == ".loom"))
    {
        workspace
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap_or(workspace)
    } else {
        workspace
    };
    root.join(".loom/state.db")
}
pub use rebuild::{RebuildEpic, RebuildReport};
