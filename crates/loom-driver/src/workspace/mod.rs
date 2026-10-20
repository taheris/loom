//! Checked invocation-local host checkout context; no local file is permanent authority.

pub mod external;
mod launch;
mod verification;
pub use verification::{
    PublicationRebase, VerificationContinuation, VerificationFailure, has_verification_state,
};

use std::path::{Path, PathBuf};

use displaydoc::Display;
use rustix::fs::{Dev, Stat};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::identifier::BeadId;
use crate::scratch::directory::Directory;

pub use launch::validate_spawn;

#[derive(Debug, Display, Error)]
pub enum Error {
    /// workspace filesystem operation failed
    Io(#[from] std::io::Error),
    /// {operation} failed for bead {bead} at {path}; work and candidate pins retained; recover available Git inputs and require fresh verification
    VerificationFilesystem {
        operation: &'static str,
        path: PathBuf,
        bead: BeadId,
        #[source]
        source: std::io::Error,
    },
    /// external workspace membership subprocess failed
    ExternalProcess(#[from] crate::process::Error),
    /// external workspace membership rejected ({status}): {detail}
    ExternalRejected {
        status: std::process::ExitStatus,
        detail: String,
    },
    /// local workspace state operation failed
    State(#[from] crate::state::CacheError),
    /// workspace context is unavailable or changed: {reason}
    Association { reason: &'static str },
    /// worker mount would expose host authority or override workspace isolation: {path}
    Mount { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: Dev,
    inode: u64,
}

impl Identity {
    const fn from_stat(stat: &Stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Checkout {
    path: PathBuf,
    directory: Identity,
    git: Identity,
}

impl Checkout {
    fn read(path: &Path) -> Result<Self, Error> {
        let path = std::path::absolute(path)?;
        if disposable(&path) {
            return Err(Error::Association {
                reason: "worker checkouts cannot select host workflow state",
            });
        }
        Self::read_participant(&path)
    }

    fn read_participant(path: &Path) -> Result<Self, Error> {
        let path = std::path::absolute(path)?;
        let directory = Directory::open(&path)?;
        let git = directory.child(Path::new(".git"))?;
        Ok(Self {
            path,
            directory: Identity::from_stat(&directory.stat()?),
            git: Identity::from_stat(&git.stat()?),
        })
    }
}

/// Host context resolved from the actual invocation, not a worker's serialized claim.
#[derive(Debug)]
pub struct Association {
    operator: Checkout,
    integration: Checkout,
}

impl Association {
    /// Inspect the current host checkouts without creating a registry or binding file.
    ///
    /// # Errors
    /// Rejects unavailable, indirect, or worker-owned checkouts.
    pub fn establish(operator: &Path) -> Result<Self, Error> {
        Self::resolve(operator)
    }

    /// Resolve the controlling workspace for an operator or its integration invocation.
    ///
    /// # Errors
    /// Rejects unavailable or worker-owned storage.
    pub fn resolve(checkout: &Path) -> Result<Self, Error> {
        let state = crate::state::path(checkout);
        let operator = state
            .parent()
            .and_then(Path::parent)
            .ok_or(Error::Association {
                reason: "state path has no controlling workspace",
            })?;
        let operator = Checkout::read(operator)?;
        let integration = Checkout::read(&operator.path.join(".loom/integration"))?;
        Ok(Self {
            operator,
            integration,
        })
    }

    /// Require this invocation's checked storage to remain unchanged during an operation.
    ///
    /// # Errors
    /// Rejects replacement or loss of either checkout or Git directory.
    pub fn recheck(&self) -> Result<(), Error> {
        if Checkout::read(self.operator())? != self.operator
            || Checkout::read(self.integration())? != self.integration
        {
            return Err(Error::Association {
                reason: "checkout changed during the operation",
            });
        }
        Ok(())
    }

    pub fn operator(&self) -> &Path {
        &self.operator.path
    }
    pub fn integration(&self) -> &Path {
        &self.integration.path
    }
}

fn disposable(path: &Path) -> bool {
    path.ancestors().any(|ancestor| {
        matches!(
            ancestor.file_name().and_then(|name| name.to_str()),
            Some("beads" | "tune" | "scratch")
        ) && ancestor
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == ".loom")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let operator = root.path().join("operator");
        fs::create_dir_all(operator.join(".git")).unwrap();
        fs::create_dir_all(operator.join(".loom/integration/.git")).unwrap();
        (root, operator)
    }

    #[test]
    fn independent_machine_resolves_without_old_home_or_bindings() {
        let (_a, first) = fixture();
        let (_b, second) = fixture();
        for operator in [&first, &second] {
            let host = Association::establish(operator).unwrap();
            let integration = Association::resolve(host.integration()).unwrap();
            assert_eq!(
                crate::state::path(host.operator()),
                crate::state::path(integration.integration())
            );
            assert!(!operator.join(".git/loom-workspace.json").exists());
            assert!(!operator.join(".loom/state.db").exists());
        }
    }

    #[test]
    fn invocation_rechecks_current_identity_without_pin_to_a_previous_machine() {
        let (_root, operator) = fixture();
        let host = Association::resolve(&operator).unwrap();
        fs::rename(operator.join(".git"), operator.join("old-git")).unwrap();
        fs::create_dir(operator.join(".git")).unwrap();
        assert!(host.recheck().is_err());
        assert!(Association::resolve(&operator).is_ok());
    }

    #[test]
    fn worker_cannot_resolve_host_state_or_mount_database_and_sidecars() {
        let (_root, operator) = fixture();
        let worker = operator.join(".loom/beads/lm-work");
        fs::create_dir_all(worker.join(".git")).unwrap();
        assert!(Association::resolve(&worker).is_err());
        let host = Association::resolve(&operator).unwrap();
        for path in [
            ".loom/state.db",
            ".loom/state.db-wal",
            ".loom/state.db-shm",
            ".loom/state.db.lock",
            ".git",
            ".loom/integration",
        ] {
            assert!(
                host.admit_mount(&crate::agent::MountSpec {
                    host_path: operator.join(path),
                    container_path: "/secrets".into(),
                    read_only: false,
                })
                .is_err(),
                "{path}"
            );
        }
    }
}
