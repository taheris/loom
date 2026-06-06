use displaydoc::Display;
use thiserror::Error;

use loom_driver::agent::ProtocolError;
use loom_driver::bd::BdError;
use loom_driver::config::LoomConfigError;
use loom_driver::git::GitError;
use loom_driver::logging::LogError;
use loom_driver::profile_manifest::ProfileError;
use loom_driver::state::StateError;

/// Errors raised by the `loom loop` driver.
#[derive(Debug, Display, Error)]
pub enum LoopError {
    /// agent backend protocol failure during `loom loop`
    Protocol(#[from] ProtocolError),

    /// bd CLI failure during `loom loop`
    Bd(#[from] BdError),

    /// rendering the loop.md template failed
    Render(#[from] askama::Error),

    /// log sink failure during `loom loop`
    Log(#[from] LogError),

    /// git step failed during `loom loop`
    Git(#[from] GitError),

    /// io operation failed during `loom loop`
    Io(#[from] std::io::Error),

    /// profile-image manifest dispatch failed during `loom loop`
    Profile(#[from] ProfileError),

    /// config load failed during `loom loop`
    Config(#[from] LoomConfigError),

    /// state.db access failure during `loom loop`
    State(#[from] StateError),

    /// spec → molecule resolution failed during `loom loop`
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
