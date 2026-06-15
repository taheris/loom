use std::fmt;
use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseLock {
    Planning,
    Todo,
}

impl PhaseLock {
    pub fn file_stem(self) -> &'static str {
        match self {
            Self::Planning => "plan",
            Self::Todo => "todo",
        }
    }
}

impl fmt::Display for PhaseLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.file_stem())
    }
}

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

    /// another loom command is operating on {phase} phase
    PhaseBusy { phase: PhaseLock },

    /// another loom command is operating on work root {root}
    WorkRootBusy { root: String },

    /// loom init cannot run while lock is held: {root}
    WorkspaceBusy { root: String },

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
