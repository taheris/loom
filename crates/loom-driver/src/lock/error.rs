use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

#[derive(Debug, Display, Error)]
pub enum LockError {
    /// failed to create lock directory at {path}
    CreateDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// failed to open lock file at {path}
    OpenFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// another loom command is operating on {label}
    SpecBusy { label: String },

    /// loom init cannot run while spec lock is held: {label}
    WorkspaceBusy { label: String },

    /// `workspace` is reserved and cannot be used as a spec label
    ReservedLabel,

    /// io failure while inspecting locks directory
    Io(#[from] io::Error),

    /// failed to build a tokio runtime for the sync lock-acquire path
    RuntimeBuild(#[source] io::Error),

    /// cannot resolve XDG_STATE_HOME: HOME is unset and no override given
    HomeUnset,

    /// failed to canonicalize workspace path {path}
    CanonicalizeWorkspace {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// workspace path {path} has no basename
    WorkspaceNoBasename { path: PathBuf },
}
