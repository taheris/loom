use askama::Template;
use loom_events::identifier::{BeadId, MoleculeId, SpecLabel};
use loom_protocol::todo::{GitSha, TodoFingerprint};

use crate::criterion_status::CriterionStatus;

/// Context for the unified `loom todo` decomposition prompt.
#[derive(Template)]
#[template(path = "todo.md", escape = "none")]
pub struct TodoContext {
    pub pinned_context: String,
    pub spec_index: String,
    pub changed_specs: Vec<TodoChangedSpec>,
    pub work_epic: BeadId,
    pub todo_head: GitSha,
    pub todo_fingerprint: TodoFingerprint,
    pub spec_epics: Vec<SpecEpicContext>,
    pub companion_paths: Vec<String>,
    pub implementation_notes: Vec<SpecImplementationNotes>,
    pub criterion_status: Vec<CriterionStatus>,
    pub scratchpad_path: String,
}

impl TodoContext {
    pub fn has_implementation_notes(&self) -> bool {
        self.implementation_notes
            .iter()
            .any(|group| !group.notes.is_empty())
    }
}

/// One driver-injected changed spec in a todo decomposition batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoChangedSpec {
    pub label: SpecLabel,
    pub spec_path: String,
    pub diff: Option<String>,
}

/// Cached per-spec epic metadata rendered for todo decomposition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecEpicContext {
    pub label: SpecLabel,
    pub epic_id: Option<MoleculeId>,
    pub todo_cursor: Option<String>,
}

/// Planning implementation notes grouped by spec label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecImplementationNotes {
    pub label: SpecLabel,
    pub notes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const PINNED: &str =
        "# Project Overview\n\nLoom orchestrates the spec-to-implementation workflow.";
    const SCRATCH: &str = "/workspace/.loom/scratch/todo/scratch.md";
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const FINGERPRINT: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn ctx_with_notes(notes: Vec<String>) -> TodoContext {
        TodoContext {
            pinned_context: PINNED.to_string(),
            spec_index: "| Spec | Beads |\n|---|---|".to_string(),
            changed_specs: vec![TodoChangedSpec {
                label: SpecLabel::new("harness"),
                spec_path: "specs/harness.md".to_string(),
                diff: None,
            }],
            work_epic: BeadId::new("lm-work").expect("valid bead id"),
            todo_head: GitSha::new(SHA).expect("valid git sha"),
            todo_fingerprint: TodoFingerprint::new(FINGERPRINT).expect("valid fingerprint"),
            spec_epics: vec![],
            companion_paths: vec![],
            implementation_notes: vec![SpecImplementationNotes {
                label: SpecLabel::new("harness"),
                notes,
            }],
            criterion_status: vec![],
            scratchpad_path: SCRATCH.to_string(),
        }
    }

    #[test]
    fn implementation_notes_present_names_notes_as_work_and_forbids_complete() {
        let ctx = ctx_with_notes(vec![
            "Refactor the parser to accept pending modifiers".to_string(),
        ]);
        let out = ctx.render().expect("render");

        assert!(
            out.contains("describes work that MUST become"),
            "notes section must name notes as work-to-be-spawned: {out}"
        );
        assert!(
            out.contains("wrong-phase success markers"),
            "todo success section must reject generic success markers: {out}"
        );
        assert!(
            out.contains("`LOOM_COMPLETE`") && out.contains("`LOOM_NOOP`"),
            "success section must name both LOOM_COMPLETE and LOOM_NOOP as forbidden: {out}"
        );
        assert!(
            out.contains("Refactor the parser to accept pending modifiers"),
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
    }
}
