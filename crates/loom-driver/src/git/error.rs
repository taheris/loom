use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

use loom_protocol::oid::ParseGitOidError;

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

    /// `git {args}` timed out after {timeout_secs}s in {workdir} (likely a hung hook or stalled remote)
    GitTimeout {
        args: String,
        timeout_secs: u64,
        workdir: PathBuf,
    },

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

    /// git CLI returned a malformed OID
    ParseOid(#[from] ParseGitOidError),

    /// $WRAPIX_SIGNING_KEY points at a non-existent file: {path}
    SigningKeyMissing { path: PathBuf },

    /// $WRAPIX_DEPLOY_KEY points at a non-existent file: {path}
    DeployKeyMissing { path: PathBuf },

    /// `ssh-keygen -y` failed deriving the public half of {key}: {stderr}
    SshKeygen { key: PathBuf, stderr: String },

    /// integration branch {branch} has diverged from origin/{branch} — it carries local commits not on origin, so it cannot fast-forward; reconcile before looping. divergent commits: {commits}
    IntegrationDiverged { branch: String, commits: String },
}
