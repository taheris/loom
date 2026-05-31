use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

use super::oid::ParseGitOidError;

#[derive(Debug, Display, Error)]
pub enum GitError {
    /// failed to open repository at {path}
    OpenRepo {
        path: PathBuf,
        #[source]
        source: Box<gix::open::Error>,
    },

    /// gix operation failed: {0}
    Gix(String),

    /// git CLI exited with status {status}: {stderr}
    GitCli { status: i32, stderr: String },

    /// git CLI did not finish within {timeout_secs}s: git {args}
    GitTimeout { args: String, timeout_secs: u64 },

    /// failed to spawn git CLI
    Spawn(#[source] io::Error),

    /// merge of {branch} produced conflicts
    MergeConflict { branch: String },

    /// worktree task panicked or was cancelled
    JoinError(#[from] tokio::task::JoinError),

    /// io failure
    Io(#[from] io::Error),

    /// invalid utf-8 in git CLI output
    Utf8(#[from] std::string::FromUtf8Error),

    /// git CLI returned a malformed OID: {0}
    ParseOid(#[from] ParseGitOidError),
}
