use displaydoc::Display;
use thiserror::Error;

use loom_driver::agent::ProtocolError;
use loom_driver::bd::BdError;
use loom_driver::git::GitError;
use loom_driver::logging::LogError;
use loom_driver::profile_manifest::ProfileError;

/// Errors raised by the `loom run` driver.
#[derive(Debug, Display, Error)]
pub enum RunError {
    /// agent backend protocol failure
    Protocol(#[from] ProtocolError),

    /// bd CLI failure
    Bd(#[from] BdError),

    /// rendering the run.md template failed
    Render(#[from] askama::Error),

    /// log sink failure
    Log(#[from] LogError),

    /// git operation failed (worktree, merge, branch)
    Git(#[from] GitError),

    /// io operation failed
    Io(#[from] std::io::Error),

    /// profile-image manifest dispatch failed
    Profile(#[from] ProfileError),

    /// no active molecule for spec `{label}`
    NoActiveMolecule { label: String },

    /// active molecule {id} has no `loom.base_commit` metadata and no parent to inherit from — set it with: bd update {id} --set-metadata loom.base_commit=<sha>
    MoleculeMissingBaseCommit { id: String },

    /// active molecule {id} has no `loom.base_commit` metadata and its parent {parent} also lacks it — set it with: bd update {id} --set-metadata loom.base_commit=<sha>
    MoleculeMissingBaseCommitNoParentMetadata { id: String, parent: String },

    /// internal invariant violated: {context}
    Bug { context: String },
}
