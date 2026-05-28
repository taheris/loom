use askama::Template;
use loom_events::identifier::{MoleculeId, SpecLabel};

use crate::criterion_status::CriterionStatus;

/// Context for `loom todo` adding tasks to an existing molecule (anchor + siblings).
#[derive(Template)]
#[template(path = "todo_update.md", escape = "none")]
pub struct TodoUpdateContext {
    pub pinned_context: String,
    pub label: SpecLabel,
    pub spec_path: String,
    pub companion_paths: Vec<String>,
    pub spec_diff: Option<String>,
    pub existing_tasks: Option<String>,
    pub molecule_id: Option<MoleculeId>,
    pub implementation_notes: Vec<String>,
    /// Per-criterion recency + verdict rows surfacing which Success-Criteria
    /// bullets already pass before the agent fans out beads. Populated by the
    /// driver from the gate's status cache; empty when no cache exists yet.
    /// See `specs/templates.md` § Criterion-Status Surface.
    pub criterion_status: Vec<CriterionStatus>,
    pub scratchpad_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const PINNED: &str = "# Project Overview\n\nLoom orchestrates the spec-to-implementation workflow.";
    const SCRATCH: &str = "/workspace/.wrapix/loom/scratch/harness/scratch.md";

    fn ctx_with_notes(notes: Vec<String>) -> TodoUpdateContext {
        TodoUpdateContext {
            pinned_context: PINNED.to_string(),
            label: SpecLabel::new("harness"),
            spec_path: "specs/harness.md".to_string(),
            companion_paths: vec![],
            spec_diff: None,
            existing_tasks: None,
            molecule_id: Some(MoleculeId::new("lm-mol")),
            implementation_notes: notes,
            criterion_status: vec![],
            scratchpad_path: SCRATCH.to_string(),
        }
    }

    #[test]
    fn implementation_notes_present_names_notes_as_work_and_forbids_complete() {
        let ctx = ctx_with_notes(vec![
            "Wire the integrity-gate branching into the pre-commit hook".to_string(),
        ]);
        let out = ctx.render().expect("render");

        assert!(
            out.contains("describes work that MUST become"),
            "notes section must name notes as work-to-be-spawned: {out}"
        );
        assert!(
            out.contains("malformed exit"),
            "notes section must mark LOOM_COMPLETE/LOOM_NOOP without new beads as malformed: {out}"
        );
        assert!(
            out.contains("`LOOM_COMPLETE`") && out.contains("`LOOM_NOOP`"),
            "notes section must name both LOOM_COMPLETE and LOOM_NOOP as forbidden when notes are intact: {out}"
        );
        assert!(
            out.contains("Wire the integrity-gate branching into the pre-commit hook"),
            "verbatim note text must survive into the rendered prompt: {out}"
        );
    }

    #[test]
    fn empty_implementation_notes_omit_notes_as_work_framing() {
        let ctx = ctx_with_notes(vec![]);
        let out = ctx.render().expect("render");

        assert!(
            !out.contains("## Implementation Notes"),
            "empty notes must suppress the section heading entirely: {out}"
        );
        assert!(
            !out.contains("describes work that MUST become"),
            "audit-only path must not see the notes-as-work framing: {out}"
        );
        assert!(
            !out.contains("malformed exit"),
            "audit-only path must not see the malformed-exit clause: {out}"
        );
    }
}
