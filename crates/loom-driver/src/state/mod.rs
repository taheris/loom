//! SQLite cache store backing `.loom/cache.db`.
//!
//! The schema is owned by `loom-driver` and migrated on `CacheDb::open`. All
//! raw SQL is confined to this module; callers see a typed Rust surface
//! (`CacheDb` plus the row structs returned by its accessors).
//!
//! The cache DB is reconstructable from spec files on disk and active beads
//! via [`CacheDb::rebuild`]; iteration counters reset to 0. Notes are owned
//! by the `loom note` CLI and live in the SQLite `notes` table; there
//! is no markdown source of truth for them anymore.

mod companions;
mod db;
mod error;
mod rebuild;

pub use companions::parse_companions;
pub use db::{
    BdUpdateFn, CacheDb, CriterionEvidenceRow, MoleculeRow, NoteRow, SpecEpicRow, SpecRow,
    WorkEpicRow,
};
pub use error::CacheError;
pub use rebuild::{ActiveMolecule, RebuildReport};
