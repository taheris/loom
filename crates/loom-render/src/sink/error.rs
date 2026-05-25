use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

#[derive(Debug, Display, Error)]
pub enum LogError {
    /// failed to create log directory at {path}
    CreateDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// failed to open log file at {path}
    OpenFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// failed to write log file at {path}
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// failed to serialize event to JSON
    Serialize(#[from] serde_json::Error),

    /// io failure
    Io(#[from] io::Error),
}
