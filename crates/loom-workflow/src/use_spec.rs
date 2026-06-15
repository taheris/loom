//! `loom use <label>` — validate a spec label without mutating workflow state.

use std::path::Path;
use std::time::Duration;

use displaydoc::Display;
use thiserror::Error;

use loom_driver::identifier::SpecLabel;
use loom_driver::lock::{LockError, LockManager, PhaseLock};
use loom_driver::state::{CacheDb, CacheError};

/// Default timeout used by [`run`].
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Failures raised by [`run`].
#[derive(Debug, Display, Error)]
pub enum UseError {
    /// lock acquisition failed
    Lock(#[from] LockError),

    /// cache-db operation failed
    State(#[from] CacheError),
}

/// Acquire the planning lock and validate that `label` exists in the cache.
pub fn run(workspace: &Path, label: &SpecLabel, db_path: &Path) -> Result<(), UseError> {
    run_with_timeout(workspace, label, db_path, DEFAULT_LOCK_TIMEOUT)
}

/// Same as [`run`] with an explicit lock-wait timeout.
pub fn run_with_timeout(
    workspace: &Path,
    label: &SpecLabel,
    db_path: &Path,
    timeout: Duration,
) -> Result<(), UseError> {
    let lock_mgr = LockManager::new(workspace)?;
    let _guard = lock_mgr.acquire_phase_with_timeout(PhaseLock::Planning, timeout)?;
    let db = CacheDb::open(db_path)?;
    let _ = db.spec(label)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use loom_driver::state::{ActiveMolecule, CacheError};

    fn db_path(workspace: &std::path::Path) -> std::path::PathBuf {
        workspace.join(".loom/cache.db")
    }

    fn seed_spec(workspace: &std::path::Path, label: &str) -> Result<CacheDb> {
        let specs_dir = workspace.join("specs");
        std::fs::create_dir_all(&specs_dir)?;
        std::fs::write(specs_dir.join(format!("{label}.md")), "# x\n")?;
        let db = CacheDb::open(db_path(workspace))?;
        db.rebuild(workspace, &[] as &[ActiveMolecule])?;
        Ok(db)
    }

    #[test]
    fn use_existing_spec_validates_without_persisting_selection() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let _seed = seed_spec(dir.path(), "harness")?;
        run(dir.path(), &SpecLabel::new("harness"), &db_path(dir.path()))?;
        Ok(())
    }

    #[test]
    fn use_acquires_planning_lock() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let _seed = seed_spec(dir.path(), "alpha")?;
        let mgr = LockManager::new(dir.path())?;
        let _hold = mgr.acquire_planning()?;

        match run_with_timeout(
            dir.path(),
            &SpecLabel::new("alpha"),
            &db_path(dir.path()),
            Duration::from_millis(100),
        ) {
            Err(UseError::Lock(LockError::PhaseBusy { phase })) => {
                assert_eq!(phase, PhaseLock::Planning);
                Ok(())
            }
            other => Err(anyhow::anyhow!("expected PhaseBusy(plan), got {other:?}")),
        }
    }

    #[test]
    fn use_unknown_spec_errors_with_spec_not_found() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let _seed = CacheDb::open(db_path(dir.path()))?;
        match run(dir.path(), &SpecLabel::new("ghost"), &db_path(dir.path())) {
            Err(UseError::State(CacheError::SpecNotFound { label })) => {
                assert_eq!(label, "ghost");
            }
            other => return Err(anyhow::anyhow!("expected SpecNotFound, got {other:?}")),
        }
        Ok(())
    }
}
