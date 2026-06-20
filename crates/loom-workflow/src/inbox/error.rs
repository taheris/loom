use displaydoc::Display;
use thiserror::Error;

use loom_driver::bd::BdError;

use super::list::InboxKindParseError;

/// Errors raised by the `loom inbox` command.
#[derive(Debug, Display, Error)]
pub enum InboxError {
    /// bd CLI failure while running `loom inbox`
    Bd(#[from] BdError),

    /// rendering the inbox.md template failed
    Render(#[from] askama::Error),

    /// unknown inbox kind `{0}`
    Kind(#[from] InboxKindParseError),

    /// no inbox item at index {index} ({total} outstanding)
    IndexOutOfRange { index: u32, total: u32 },

    /// no inbox item with bead id {id}
    BeadNotFound { id: String },

    /// no tune proposal with id {id}
    ProposalNotFound { id: String },

    /// use only one address selector: number, --bead, or --proposal
    AmbiguousTarget,

    /// an inbox target is required for this view mode
    TargetRequired,
}
