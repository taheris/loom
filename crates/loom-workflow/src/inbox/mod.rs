//! `loom inbox` — human decision queue.
//!
//! Lists, views, and launches chat for outstanding `loom:clarify`,
//! `loom:blocked`, `loom:infra`, and tune proposal items. Host-side modes are
//! read-only; resolution happens in `loom inbox chat`.

mod apply;
pub mod chat;
mod context;
mod error;
mod list;
mod options;
mod terminal;

pub use apply::{ApplyError, ApplyReport, apply_proposals, ensure_integration_clean_after_chat};
pub use context::build_inbox_context;
pub use error::InboxError;
pub use list::{
    InboxItem, InboxKind, InboxRow, InfraInfo, TuneInfo, build_queue, build_rows, find_by_bead_id,
    find_by_index, find_by_proposal_id, frame_unavailable_tune_items, kind_of, spec_label_of,
};
pub use options::{
    OptionEntry, OptionsParse, find_options_block_range, parse_options, parse_options_in,
    strip_options_block,
};
