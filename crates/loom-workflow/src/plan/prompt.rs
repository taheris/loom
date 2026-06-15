use askama::Template;

use loom_templates::plan::PlanContext;

use super::error::PlanError;
use loom_driver::identifier::SpecLabel;

/// Inputs threaded into the unified plan context struct.
pub struct PlanPromptInputs {
    pub anchor_labels: Vec<SpecLabel>,
    pub pinned_context: String,
    pub spec_index: String,
    pub companion_paths: Vec<String>,
    /// Absolute path to `.loom/scratch/<key>/scratch.md` for this session.
    /// Embedded in the rendered prompt so the agent can write to the correct
    /// file under compaction recovery.
    pub scratchpad_path: String,
    /// Workspace-relative path to the spec-authoring conventions document.
    /// Pinned via `partial/spec_conventions.md` so the agent reads the
    /// conventions before authoring or editing specs.
    pub spec_conventions: String,
}

/// Render the Askama template for `loom plan [SPEC_LABEL ...]`.
pub fn render_prompt(inputs: PlanPromptInputs) -> Result<String, PlanError> {
    Ok(PlanContext {
        pinned_context: inputs.pinned_context,
        anchor_labels: inputs.anchor_labels,
        spec_index: inputs.spec_index,
        companion_paths: inputs.companion_paths,
        scratchpad_path: inputs.scratchpad_path,
        spec_conventions: inputs.spec_conventions,
    }
    .render()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> PlanPromptInputs {
        PlanPromptInputs {
            anchor_labels: vec![SpecLabel::new("harness"), SpecLabel::new("future-spec")],
            pinned_context: "PIN".into(),
            spec_index: "| Spec | Purpose |\n| [harness](../specs/harness.md) | Harness |".into(),
            companion_paths: vec!["lib/sandbox/".into()],
            scratchpad_path: "/workspace/.loom/scratch/harness+future-spec/scratch.md".into(),
            spec_conventions: "docs/spec-conventions.md".into(),
        }
    }

    #[test]
    fn plan_renders_index_anchors_and_companions() {
        let body = render_prompt(inputs()).expect("render");
        assert!(body.contains("# Specification Interview"));
        assert!(body.contains("| [harness](../specs/harness.md) | Harness |"));
        assert!(body.contains("`harness`"));
        assert!(body.contains("`future-spec`"));
        assert!(body.contains("- lib/sandbox/"));
    }

    #[test]
    fn plan_renders_empty_anchor_roster() {
        let body = render_prompt(PlanPromptInputs {
            anchor_labels: Vec::new(),
            pinned_context: "PIN".into(),
            spec_index: "INDEX".into(),
            companion_paths: Vec::new(),
            scratchpad_path: "/workspace/.loom/scratch/plan/scratch.md".into(),
            spec_conventions: "docs/spec-conventions.md".into(),
        })
        .expect("render");
        assert!(body.contains("No anchor labels were supplied"));
    }

    #[test]
    fn plan_prompt_instructs_agent_to_call_loom_note_set() {
        let body = render_prompt(inputs()).expect("render");
        assert!(body.contains("loom note set"));
        assert!(body.contains("--kind implementation"));
        assert!(body.to_lowercase().contains("keep still-relevant notes"));
        assert!(body.to_lowercase().contains("drop notes"));
        assert!(body.to_lowercase().contains("add fresh notes"));
    }
}
