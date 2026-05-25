//! `loom use <label>` — set the active spec via the state DB.
//!
//! Acquires the per-spec lock for `label` (per the `Concurrency & Locking`
//! lock matrix in `specs/harness.md`), opens the state DB, and writes
//! `current_spec`. Round-trips with [`super::status::load`].

use std::path::Path;
use std::time::Duration;

use displaydoc::Display;
use thiserror::Error;

use loom_driver::identifier::SpecLabel;
use loom_driver::lock::{LockError, LockManager};
use loom_driver::state::{StateDb, StateError};

/// Default timeout used by [`run`]. Mirrors `LockManager::acquire_spec`'s
/// 5-second wait so the binary surfaces `SpecBusy` after the same delay as
/// every other spec-scoped command.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Failures raised by [`run`].
#[derive(Debug, Display, Error)]
pub enum UseError {
    /// lock acquisition failed
    Lock(#[from] LockError),

    /// state-db operation failed
    State(#[from] StateError),
}

/// Acquire `<label>.lock` (waiting up to [`DEFAULT_LOCK_TIMEOUT`]) and
/// persist `current_spec = label` in the state DB. `db_path` is typically
/// `<workspace>/.wrapix/loom/state.db`; the caller is responsible for
/// ensuring [`super::init::run`] has populated it.
pub fn run(workspace: &Path, label: &SpecLabel, db_path: &Path) -> Result<(), UseError> {
    run_with_timeout(workspace, label, db_path, DEFAULT_LOCK_TIMEOUT)
}

/// Same as [`run`] with an explicit lock-wait timeout. Tests use this to
/// keep the contention path fast.
pub fn run_with_timeout(
    workspace: &Path,
    label: &SpecLabel,
    db_path: &Path,
    timeout: Duration,
) -> Result<(), UseError> {
    let lock_mgr = LockManager::new(workspace)?;
    let _guard = lock_mgr.acquire_spec_with_timeout(label, timeout)?;
    let db = StateDb::open(db_path)?;
    let _ = db.spec(label)?;
    db.set_current_spec(label)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use loom_driver::state::{ActiveMolecule, StateError};

    fn db_path(workspace: &std::path::Path) -> std::path::PathBuf {
        workspace.join(".wrapix/loom/state.db")
    }

    fn seed_spec(workspace: &std::path::Path, label: &str) -> Result<StateDb> {
        let specs_dir = workspace.join("specs");
        std::fs::create_dir_all(&specs_dir)?;
        std::fs::write(specs_dir.join(format!("{label}.md")), "# x\n")?;
        let db = StateDb::open(db_path(workspace))?;
        db.rebuild(workspace, &[] as &[ActiveMolecule])?;
        Ok(db)
    }

    #[test]
    fn use_round_trips_with_status_load() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let _seed = seed_spec(dir.path(), "harness")?;
        run(
            dir.path(),
            &SpecLabel::new("harness"),
            &db_path(dir.path()),
        )?;

        let db = StateDb::open(db_path(dir.path()))?;
        let current = db
            .current_spec()?
            .ok_or_else(|| anyhow::anyhow!("current_spec must be set"))?;
        assert_eq!(current.as_str(), "harness");
        Ok(())
    }

    #[test]
    fn use_acquires_per_spec_lock() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let _seed = seed_spec(dir.path(), "alpha")?;
        let mgr = LockManager::new(dir.path())?;
        let _hold = mgr.acquire_spec(&SpecLabel::new("alpha"))?;

        match run_with_timeout(
            dir.path(),
            &SpecLabel::new("alpha"),
            &db_path(dir.path()),
            Duration::from_millis(100),
        ) {
            Err(UseError::Lock(LockError::SpecBusy { label })) => {
                assert_eq!(label, "alpha");
                Ok(())
            }
            other => Err(anyhow::anyhow!("expected SpecBusy, got {other:?}")),
        }
    }

    /// Spec contract (`specs/tests.md` § Auxiliary commands):
    /// `loom use <unknown>` errors when no `specs` row matches the label.
    /// Without this guard the meta table would happily point at a spec that
    /// has never been registered, leaving `loom status` to display a label
    /// that nothing else in the workspace knows about.
    #[test]
    fn use_unknown_spec_errors_with_spec_not_found() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let _seed = StateDb::open(db_path(dir.path()))?;
        match run(
            dir.path(),
            &SpecLabel::new("ghost"),
            &db_path(dir.path()),
        ) {
            Err(UseError::State(StateError::SpecNotFound { label })) => {
                assert_eq!(label, "ghost");
            }
            other => return Err(anyhow::anyhow!("expected SpecNotFound, got {other:?}")),
        }

        let db = StateDb::open(db_path(dir.path()))?;
        assert!(
            db.current_spec()?.is_none(),
            "current_spec must not be written for an unknown label",
        );
        Ok(())
    }
}
