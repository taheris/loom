use displaydoc::Display;
use thiserror::Error;

use loom_driver::agent::ProtocolError;
use loom_driver::bd::BdError;
use loom_driver::git::GitError;
use loom_driver::logging::LogError;
use loom_driver::profile_manifest::ProfileError;
use loom_driver::state::StateError;

/// Errors raised by the `loom loop` driver.
#[derive(Debug, Display, Error)]
pub enum LoopError {
    /// agent backend protocol failure: {0}
    Protocol(#[from] ProtocolError),

    /// bd CLI failure: {0}
    Bd(#[from] BdError),

    /// rendering the loop.md template failed: {0}
    Render(#[from] askama::Error),

    /// log sink failure: {0}
    Log(#[from] LogError),

    /// git operation failed: {0}
    Git(#[from] GitError),

    /// io operation failed: {0}
    Io(#[from] std::io::Error),

    /// `beads-push` failed after `git push` succeeded: {0}
    BeadsPushFailed(String),

    /// profile-image manifest dispatch failed: {0}
    Profile(#[from] ProfileError),

    /// state.db access failure: {0}
    State(#[from] StateError),

    /// spec → molecule resolution failed: {0}
    Resolve(#[from] crate::resolve::ResolveError),

    /// no active molecule for spec `{label}`
    NoActiveMolecule { label: String },

    /// active molecule {id} has no `loom.base_commit` metadata and no parent to inherit from — set it with: bd update {id} --set-metadata loom.base_commit=<sha>
    MoleculeMissingBaseCommit { id: String },

    /// active molecule {id} has no `loom.base_commit` metadata and its parent {parent} also lacks it — set it with: bd update {id} --set-metadata loom.base_commit=<sha>
    MoleculeMissingBaseCommitNoParentMetadata { id: String, parent: String },

    /// internal invariant violated: {context}
    Bug { context: String },
}
