use askama::Template;
use loom_events::identifier::SpecLabel;

use crate::criterion_status::CriterionStatus;

/// Context for `loom todo` decomposing a fresh spec into a new molecule.
#[derive(Template)]
#[template(path = "todo_new.md", escape = "none")]
pub struct TodoNewContext {
    pub pinned_context: String,
    pub label: SpecLabel,
    pub spec_path: String,
    pub companion_paths: Vec<String>,
    pub implementation_notes: Vec<String>,
    /// Per-criterion recency + verdict rows surfacing which Success-Criteria
    /// bullets already pass before the agent fans out beads. Populated by the
    /// driver from the gate's status cache; empty when no cache exists yet.
    /// See `specs/templates.md` § Criterion-Status Surface.
    pub criterion_status: Vec<CriterionStatus>,
    pub scratchpad_path: String,
}
