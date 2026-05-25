//! SQLite state store backing `.wrapix/loom/state.db`.
//!
//! The schema is owned by `loom-driver` and migrated on `StateDb::open`. All
//! raw SQL is confined to this module; callers see a typed Rust surface
//! (`StateDb` plus the row structs returned by its accessors).
//!
//! The state DB is reconstructable from spec files on disk and active beads
//! via [`StateDb::rebuild`]; iteration counters reset to 0. Notes are owned
//! by the `loom note` CLI and live in the SQLite `notes` table; there
//! is no markdown source of truth for them anymore.

mod companions;
mod db;
mod error;
mod migrate;
mod rebuild;

pub use companions::parse_companions;
pub use db::{BdUpdateFn, MoleculeRow, NoteRow, SpecRow, StateDb};
pub use error::StateError;
pub use rebuild::{ActiveMolecule, RebuildReport};
