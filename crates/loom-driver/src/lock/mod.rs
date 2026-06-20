//! Phase/work-root and workspace advisory locking via `flock(2)`.
//!
//! Concurrent `loom` invocations on the same workspace are explicitly allowed
//! (see *Concurrency & Locking* in `specs/harness.md`). The lock model is
//! phase locks (`plan.lock`, `todo.lock`), per-work-root locks
//! (`<bead-or-epic-id>.lock`), and `workspace.lock` for `loom init`.
//!
//! All locks are POSIX advisory locks acquired via `flock(2)`. The kernel
//! releases them on process exit or crash, so there
//! are no stale locks to clean up.
//!
//! Lock files live under `$XDG_STATE_HOME/loom/locks/<workspace-basename>/`
//! (default `~/.local/state/loom/locks/<basename>/`) — outside the workspace
//! bind-mount so a bead container cannot `rm` them out from under the host
//! driver. Read-only commands (`status`, `logs`, `spec`) acquire no lock and
//! are unaffected by an active hold.

mod error;
mod manager;

pub use error::{LockError, PhaseLock};
pub use manager::{LockGuard, LockManager};
