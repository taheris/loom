use std::io;

use displaydoc::Display;
use thiserror::Error;

use loom_events::identifier::ParseBeadIdError;

#[derive(Debug, Display, Error)]
pub enum BdError {
    /// unsupported Beads capability: {reason}
    Unsupported { reason: String },

    /// native `bd` transport I/O failed; execution may already have started
    Spawn(#[source] io::Error),

    /// `bd` did not finish within the configured timeout: bd {args}
    Timeout { args: String },

    /// failed to terminate or reap `bd`
    Cleanup(#[source] io::Error),

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

    /// `bd create --silent` did not include an id
    CreateMissingId,

    /// `bd create --silent` returned a malformed bead id
    CreateInvalidId(#[from] ParseBeadIdError),

    /// invalid utf-8 in `bd` output
    Utf8(#[from] std::string::FromUtf8Error),
}
