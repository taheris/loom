use std::path::PathBuf;

use displaydoc::Display;
use loom_driver::agent::ProtocolError;
use loom_driver::profile_manifest::ProfileError;
use loom_driver::state::StateError;
use thiserror::Error;

/// Errors raised by the `loom todo` driver.
#[derive(Debug, Display, Error)]
pub enum TodoError {
    /// the `--since {commit}` override does not refer to a reachable commit
    InvalidSinceCommit { commit: String },

    /// agent supplied no exit signal — neither LOOM_COMPLETE nor LOOM_BLOCKED observed before session ended
    MissingExitSignal,

    /// agent reported LOOM_BLOCKED: {reason}
    AgentBlocked { reason: String },

    /// could not read spec file at {path}
    ReadSpec {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// rendering the prompt template failed
    Render(#[from] askama::Error),

    /// io operation failed
    Io(#[from] std::io::Error),

    /// agent backend protocol failure
    Protocol(#[from] ProtocolError),

    /// state-db read/write failure
    State(#[from] StateError),

    /// profile-image manifest dispatch failed
    Profile(#[from] ProfileError),
}
