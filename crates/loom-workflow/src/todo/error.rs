use std::path::PathBuf;

use displaydoc::Display;
use loom_driver::agent::ProtocolError;
use loom_driver::bd::BdError;
use loom_driver::profile_manifest::ProfileError;
use loom_driver::state::StateError;
use thiserror::Error;

/// Errors raised by the `loom todo` driver.
#[derive(Debug, Display, Error)]
pub enum TodoError {
    /// the `--since {commit}` override does not refer to a reachable commit
    InvalidSinceCommit { commit: String },

    /// multiple open epics found for spec `{label}`: {ids}; close all but one before re-running
    InvariantViolation { label: String, ids: String },

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

    /// rendering the prompt template failed: {0}
    Render(#[from] askama::Error),

    /// io operation failed: {0}
    Io(#[from] std::io::Error),

    /// multi-spec fan-out collision; `loom:clarify` bead {clarify_id} created — resolve via `loom msg`
    MultiSpecCollision { clarify_id: String },

    /// agent reported productive completion for spec `{label}` but minted no implementation beads despite {notes_remaining} note(s) remaining — either re-run after `loom note clear {label}` if the notes are obsolete, or investigate why the agent skipped fan-out (see logs/{label}/todo-*.jsonl)
    ProductiveCompletionWithoutFanout {
        label: String,
        notes_remaining: usize,
    },

    /// agent backend protocol failure: {0}
    Protocol(#[from] ProtocolError),

    /// state-db read/write failure: {0}
    State(#[from] StateError),

    /// profile-image manifest dispatch failed: {0}
    Profile(#[from] ProfileError),

    /// bd client failure: {0}
    Bd(#[from] BdError),

    /// spec → molecule resolution failed: {0}
    Resolve(#[from] crate::resolve::ResolveError),
}
