use std::io;
use std::path::PathBuf;
use std::time::SystemTimeError;

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

    /// wall-clock timestamp is before the Unix epoch
    ClockBeforeUnixEpoch(#[source] SystemTimeError),

    /// $WRIX_SIGNING_KEY points at a non-existent file: {path}
    SigningKeyMissing { path: PathBuf },

    /// $WRIX_DEPLOY_KEY points at a non-existent file: {path}
    DeployKeyMissing { path: PathBuf },

    /// repository deploy key is unavailable; set $WRIX_DEPLOY_KEY or provision the repository fallback before running loom loop (use --host-key only to opt into ambient host Git credentials)
    RepositoryDeployKeyRequired,

    /// repository signing key is unavailable; set $WRIX_SIGNING_KEY or provision the repository fallback before running loom loop (use --host-key only to opt into ambient host Git signing)
    RepositorySigningKeyRequired,

    /// ambient ${variable} would override the repository deploy-key transport; unset it before running loom loop (or use --host-key to opt into host Git policy)
    AmbientGitTransportOverride { variable: String },

    /// repository key path must be absolute so Wrix helpers remain valid across Git working directories: {path}
    RepositoryKeyPathNotAbsolute { path: PathBuf },

    /// repository deploy-key path has no valid UTF-8 file name: {path}
    RepositoryKeyName { path: PathBuf },

    /// repository Git policy is missing resolved key state
    RepositoryPolicyIncomplete,

    /// failed to spawn `{executable}` for repository Git policy: {source}
    WrixSpawn {
        executable: PathBuf,
        #[source]
        source: io::Error,
    },

    /// `wrix init --offline` failed in {workdir} with status {status}: {detail}
    WrixInit {
        workdir: PathBuf,
        status: i32,
        detail: String,
    },

    /// `wrix init --offline` left invalid repository Git policy in {workdir}: {detail}
    WrixPolicyInvalid { workdir: PathBuf, detail: String },

    /// cannot resolve canonical wrix.prekHooks hooks directory — set $WRIX_PREK_HOOKS or enter a wrix devshell that configures core.hooksPath
    PrekHooksUnresolved,

    /// $WRIX_PREK_HOOKS does not point at a directory containing canonical prek hooks: {path}
    PrekHooksMissing { path: PathBuf },

    /// core.hooksPath in {workdir} is not the canonical wrix.prekHooks path: expected {expected}, found {actual}
    HooksPathInvalid {
        workdir: PathBuf,
        expected: String,
        actual: String,
    },

    /// `ssh-keygen -y` failed deriving the public half of {key}: {stderr}
    SshKeygen { key: PathBuf, stderr: String },

    /// integration branch {branch} has diverged from origin/{branch} — it carries local commits not on origin, so it cannot fast-forward; reconcile before looping. divergent commits: {commits}
    IntegrationDiverged { branch: String, commits: String },

    /// git lock contention in {workdir} persisted across the retry budget — a concurrent loom process is holding the loom-workspace `index.lock`, or a crashed process left a stale `.git/index.lock` behind
    IndexLocked { workdir: PathBuf },
}
