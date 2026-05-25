//! `loom run` template: the per-bead implementation prompt with retry context.

use askama::Template;
use loom_events::identifier::{BeadId, MoleculeId, SpecLabel};

pub use crate::previous_failure::{
    DriverNoticeCause, PREVIOUS_FAILURE_MAX_LEN, PreviousFailure, ReviewConcernKind,
    STDERR_TAIL_PER_BLOCK, VerifierFailure,
};

/// Context for `loom run` executing a single bead.
#[derive(Template)]
#[template(path = "run.md", escape = "none")]
pub struct RunContext {
    pub pinned_context: String,
    pub label: SpecLabel,
    pub spec_path: String,
    pub companion_paths: Vec<String>,
    pub molecule_id: Option<MoleculeId>,
    pub issue_id: Option<BeadId>,
    pub title: Option<String>,
    pub description: Option<String>,
    /// Typed retry context — variants render with their documented framing
    /// (see `crate::previous_failure::PreviousFailure`'s `Display` impl).
    pub previous_failure: Option<PreviousFailure>,
    /// Companion ~1000-char review-notes block appended under
    /// `Review notes:` when `previous_failure` is `VerifyFailures` and the
    /// reviewer also raised a concern. Independent of the `previous_failure`
    /// budget so review reasoning never crowds out mechanical failure detail.
    pub review_notes: Option<String>,
    /// In-session per-bead retry counter, populated by the driver. `0` on
    /// fresh dispatch; `run.md` omits the retry line when zero.
    pub attempt: u32,
    pub scratchpad_path: String,
    pub style_rules: String,
}
