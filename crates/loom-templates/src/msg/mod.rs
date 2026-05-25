//! `loom msg` template: drafter session for resolving outstanding clarify beads.

use askama::Template;
use loom_events::identifier::{BeadId, SpecLabel};

/// Context for `loom msg` rendering the cross-spec clarify queue.
#[derive(Template)]
#[template(path = "msg.md", escape = "none")]
pub struct MsgContext {
    pub pinned_context: String,
    pub companion_paths: Vec<String>,
    pub clarify_beads: Vec<ClarifyBead>,
    pub scratchpad_path: String,
}

/// A single outstanding msg-queue bead surfaced to the drafter session.
///
/// Carries id, owning spec, title, an optional `## Options — <summary>`
/// framing, the parsed option list, and the bead's flow (`Clarify` or
/// `Blocked`). The template branches its instructions per `kind`:
/// `Blocked` beads enter an enumerate-options-first flow because the
/// agent that emitted `LOOM_BLOCKED` did not carry option structure with
/// it.
#[derive(Debug, Clone)]
pub struct ClarifyBead {
    pub id: BeadId,
    pub spec_label: SpecLabel,
    pub title: String,
    pub options_summary: Option<String>,
    pub options: Vec<ClarifyOption>,
    pub kind: BeadKind,
}

impl ClarifyBead {
    /// `true` when the bead carries `loom:blocked` rather than
    /// `loom:clarify`. The template branches on this — blocked beads do
    /// not arrive with options, so the drafter walks the user through
    /// enumerating them first.
    pub fn is_blocked(&self) -> bool {
        matches!(self.kind, BeadKind::Blocked)
    }
}

/// Which `loom:*` flow a msg-queue bead belongs to. Drives the template's
/// per-bead instruction block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeadKind {
    Clarify,
    Blocked,
}

/// A single option under a clarify bead's `## Options` block.
#[derive(Debug, Clone)]
pub struct ClarifyOption {
    pub n: u32,
    pub title: Option<String>,
    pub body: Option<String>,
}
