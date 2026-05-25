//! Shared types and infrastructure for the Loom agent driver.
//!
//! Holds newtype identifiers, the agent backend trait surface, the SQLite
//! state store, the typed `bd` CLI wrapper, and per-spec advisory locking.
//! Subsequent issues populate each module; this crate currently exposes the
//! module skeleton only.

pub mod agent;
pub mod bd;
pub mod clock;
pub mod config;
pub mod git;
pub mod lock;
pub mod logging;
pub mod markdown;
pub mod profile_manifest;
pub mod scratch;
pub mod state;
pub mod testing;

/// Re-export of the identifier newtypes that now live in `loom-events`.
/// Existing call sites (`use loom_driver::identifier::BeadId`) continue
/// to work; new code should depend on `loom-events` directly.
pub use loom_events::identifier;
