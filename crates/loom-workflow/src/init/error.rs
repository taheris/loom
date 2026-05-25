use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

use loom_driver::bd::BdError;
use loom_driver::lock::LockError;
use loom_driver::state::StateError;

/// Failures raised by [`super::run`] and [`super::fetch_active_molecules`].
#[derive(Debug, Display, Error)]
pub enum InitError {
    /// failed to create directory at {path}
    CreateDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// failed to write config file at {path}
    WriteConfig {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// lock acquisition failed
    Lock(#[from] LockError),

    /// state-db operation failed
    State(#[from] StateError),

    /// `bd` CLI invocation failed while gathering active molecules
    Bd(#[from] BdError),

    /// active molecule {id} carries no `spec:<label>` label
    MissingSpecLabel { id: String },

    /// active molecule {id} has no `loom.base_commit` metadata and no parent to inherit from — set it with: bd update {id} --set-metadata loom.base_commit=<sha>
    MoleculeMissingBaseCommit { id: String },

    /// active molecule {id} has no `loom.base_commit` metadata and its parent {parent} also lacks it — set it with: bd update {id} --set-metadata loom.base_commit=<sha>
    MoleculeMissingBaseCommitNoParentMetadata { id: String, parent: String },
}
