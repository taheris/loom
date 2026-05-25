//! `loom msg` — clarify resolution.
//!
//! Drives the clarify flow on top of `loom-driver`'s typed `BdClient` and
//! `templates`' `msg.md` template.
//! The command:
//!
//! 1. lists outstanding beads carrying `loom:clarify`, optionally filtered
//!    to `spec:<label>` so the SPEC column collapses;
//! 2. resolves an `-n <N>` / `-i <id>` selector to a [`BeadId`];
//! 3. for `-a <choice>` either looks up `### Option <N>` per the Options
//!    Format Contract and composes a `Chose option N — title: body` note
//!    ([`reply::FastReply::Option`]), or stores the choice verbatim
//!    ([`reply::FastReply::Verbatim`]);
//! 4. for `-d` writes the canonical [`reply::DISMISS_NOTE`] and removes the
//!    `loom:clarify` label so the bead drops off the list.
//!
//! The render path uses [`build_msg_context`] to compose the typed
//! [`MsgContext`](loom_templates::msg::MsgContext) the Askama template
//! consumes.

pub mod chat;
mod context;
mod error;
mod list;
mod options;
mod reply;

pub use context::{build_msg_context, resolve_target};
pub use error::MsgError;
pub use list::{MsgKind, MsgRow, build_rows, filter_msg_beads, kind_of, spec_label_of};
pub use options::{OptionEntry, OptionsParse, parse_options};
pub use reply::{DISMISS_NOTE, FastReply, build_fast_reply, compose_option_note};
