//! `loom review` template: the LLM-judged rubric prompt.

use askama::Template;
use loom_events::identifier::{MoleculeId, SpecLabel};

/// One file body included in the review prompt — either a `[verify]` test
/// script the gate just ran, or a `[judge]` rubric the LLM must score
/// against. `path` is the workspace-relative source location used as the
/// rendered section title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSource {
    pub path: String,
    pub body: String,
}

/// Which lane(s) of the review the prompt asks the agent to run. `Both`
/// drives the full `loom gate review` path (criterion-attached `[judge]`
/// rubrics *and* the rubric walk over the diff). `Judge` and `Rubric` are
/// the focused per-lane re-runs surfaced by `loom gate judge` /
/// `loom gate rubric` respectively, used when iterating on one lane
/// without paying the cost of the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewLane {
    /// Both lanes — the default for `loom gate review`.
    #[default]
    Both,
    /// `[judge]`-tier rubrics only; rubric walk suppressed.
    Judge,
    /// Rubric walk only; `[judge]` rubric bodies suppressed.
    Rubric,
}

impl ReviewLane {
    /// True when the lane includes the `[judge]` rubric evaluation.
    pub fn includes_judge(self) -> bool {
        matches!(self, Self::Both | Self::Judge)
    }

    /// True when the lane includes the rubric walk over the diff.
    pub fn includes_rubric(self) -> bool {
        matches!(self, Self::Both | Self::Rubric)
    }
}

/// Context for `loom review` reviewing a completed molecule.
#[derive(Template)]
#[template(path = "review.md", escape = "none")]
pub struct ReviewContext {
    pub pinned_context: String,
    pub label: SpecLabel,
    pub spec_path: String,
    pub companion_paths: Vec<String>,
    pub beads_summary: Option<String>,
    pub base_commit: Option<String>,
    pub molecule_id: Option<MoleculeId>,
    pub verify_sources: Vec<ReviewSource>,
    pub judge_rubrics: Vec<ReviewSource>,
    pub scratchpad_path: String,
    /// Workspace-relative path to the project's style-rules document
    /// (`docs/style-rules.md` by default). Pinned in the review prompt so
    /// the LLM walks the rules rule-by-rule and cites each violation by
    /// rule id + file/line.
    pub style_rules: String,
    /// Which lane(s) the agent is being asked to run. Drives the template's
    /// per-lane section gates.
    pub lane: ReviewLane,
}
