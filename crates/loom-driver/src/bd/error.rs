use std::io;

use displaydoc::Display;
use thiserror::Error;

use crate::identifier::ParseBeadIdError;

#[derive(Debug, Display, Error)]
pub enum BdError {
    /// failed to spawn `bd`
    Spawn(#[source] io::Error),

    /// `bd` did not finish within the configured timeout: bd {args}
    Timeout { args: String },

    /// `bd` exited with status {status}: {stderr}
    Cli {
        status: i32,
        args: String,
        stderr: String,
    },

    /// failed to decode `bd` JSON output for `bd {args}`
    Decode {
        args: String,
        #[source]
        source: serde_json::Error,
    },

    /// `bd show` returned no rows for the requested id(s)
    ShowEmpty,

    /// `bd create --json` did not include an id
    CreateMissingId,

    /// `bd create --silent` returned a malformed bead id: {0}
    CreateInvalidId(#[from] ParseBeadIdError),

    /// invalid utf-8 in `bd` output
    Utf8(#[from] std::string::FromUtf8Error),

    /// task panicked or was cancelled
    JoinError(#[from] tokio::task::JoinError),
}
