use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

use loom_driver::bd::BdError;
use loom_driver::config::LoomConfigError;
use loom_driver::git::GitError;
use loom_driver::identifier::ParseMoleculeIdError;
use loom_driver::lock::LockError;
use loom_driver::state::CacheError;

/// Failures raised by [`super::run`] and [`super::fetch_epics`].
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

    /// lock acquisition failed while initializing the loom workspace
    Lock(#[from] LockError),

    /// cache-db operation failed while initializing the loom workspace
    State(#[from] CacheError),

    /// `bd` CLI invocation failed while gathering active molecules
    Bd(#[from] BdError),

    /// git operation failed while materializing the loom workspace
    Git(#[from] GitError),

    /// failed to load `<workspace>/loom.toml` while resolving the integration branch
    Config(#[from] LoomConfigError),

    /// active molecule id is malformed
    InvalidMoleculeId {
        #[source]
        source: ParseMoleculeIdError,
    },

    /// epic `{id}` has invalid `{key}` metadata: {detail}; repair the durable epic before rebuilding
    InvalidEpicMetadata {
        id: String,
        key: &'static str,
        detail: String,
    },
}
