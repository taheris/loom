use displaydoc::Display;
use thiserror::Error;

use loom_driver::bd::BdError;

/// Errors raised by the `loom msg` command.
#[derive(Debug, Display, Error)]
pub enum MsgError {
    /// bd CLI failure
    Bd(#[from] BdError),

    /// rendering the msg.md template failed
    Render(#[from] askama::Error),

    /// invalid index '{value}' for -n (expected a positive integer)
    InvalidIndex { value: String },

    /// no clarify at index {index} ({total} outstanding)
    IndexOutOfRange { index: u32, total: u32 },

    /// no clarify with bead id {id}
    BeadNotFound { id: String },

    /// option {option} not found in {bead}: available options: {available}
    OptionMissing {
        bead: String,
        option: u32,
        available: String,
    },

    /// -a / -d require -n <N> or -i <id>
    TargetRequired,

    /// use either -n <N> or -i <id>, not both
    AmbiguousTarget,

    /// use either -a <choice> or -d, not both
    AnswerOrDismiss,
}
