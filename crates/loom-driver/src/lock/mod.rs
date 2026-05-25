//! Per-spec and workspace advisory locking via `flock(2)`.
//!
//! Concurrent `loom` invocations on the same workspace are explicitly allowed
//! (see *Concurrency & Locking* in `specs/harness.md`). The lock model is
//! per-spec exclusive locks — `<label>.lock` per spec — plus a single
//! `workspace.lock` held only during destructive state rebuild (`loom init`,
//! `loom init --rebuild`).
//!
//! All locks are POSIX advisory locks acquired via `fd-lock` (which wraps
//! `flock(2)`). The kernel releases them on process exit or crash, so there
//! are no stale locks to clean up.
//!
//! Lock files live under `$XDG_STATE_HOME/loom/locks/<workspace-basename>/`
//! (default `~/.local/state/loom/locks/<basename>/`) — outside the workspace
//! bind-mount so a bead container cannot `rm` them out from under the host
//! driver. The reserved label `workspace` cannot be used as a spec label so
//! that `workspace.lock` never collides with a `<label>.lock`. Read-only
//! commands (`status`, `logs`, `spec`) acquire no lock and are unaffected
//! by an active hold.

mod error;
mod manager;

pub use error::LockError;
pub use manager::{LockGuard, LockManager, RESERVED_WORKSPACE_LABEL};
