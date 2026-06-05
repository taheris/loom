use askama::Template;

use loom_templates::plan::{PlanNewContext, PlanUpdateContext};

use super::args::PlanMode;
use super::error::PlanError;

/// Inputs threaded into the plan-new/plan-update context structs.
///
/// Constructed once by the driver per `loom plan` call. `companion_paths`
/// is consumed only by `Update`; `New` ignores it — companions are an
/// update-only concept.
pub struct PlanPromptInputs {
    pub mode: PlanMode,
    pub spec_path: String,
    pub pinned_context: String,
    pub companion_paths: Vec<String>,
    /// Existing `kind = implementation` notes for this spec. Consumed only by
    /// `Update`; `New` ignores it (the row does not exist yet, so there are
    /// no prior notes to merge). The runner reads these via
    /// `StateDb::notes_list` and passes them through so the rendered
    /// interview prompt can show the agent what's already on file for the
    /// keep/drop/add merge.
    pub implementation_notes: Vec<String>,
    /// Absolute path to `.loom/scratch/<key>/scratch.md` for this
    /// session. Embedded in the rendered prompt so the agent can write to
    /// the correct file under compaction recovery.
    pub scratchpad_path: String,
    /// Workspace-relative path to the spec-authoring conventions document.
    /// Pinned in both planning templates via `partial/spec_conventions.md`
    /// so the agent reads the conventions before authoring or editing the
    /// spec.
    pub spec_conventions: String,
}

/// Render the appropriate Askama template for `inputs.mode`. Returns the
/// rendered prompt body the driver will pass to `wrix run`.
pub fn render_prompt(inputs: PlanPromptInputs) -> Result<String, PlanError> {
    let body = match inputs.mode {
        PlanMode::New(label) => PlanNewContext {
            pinned_context: inputs.pinned_context,
            label,
            spec_path: inputs.spec_path,
            scratchpad_path: inputs.scratchpad_path,
            spec_conventions: inputs.spec_conventions,
        }
        .render()?,
        PlanMode::Update(label) => PlanUpdateContext {
            pinned_context: inputs.pinned_context,
            label,
            spec_path: inputs.spec_path,
            companion_paths: inputs.companion_paths,
            implementation_notes: inputs.implementation_notes,
            scratchpad_path: inputs.scratchpad_path,
            spec_conventions: inputs.spec_conventions,
        }
        .render()?,
    };
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::identifier::SpecLabel;

    fn inputs_new() -> PlanPromptInputs {
        PlanPromptInputs {
            mode: PlanMode::New(SpecLabel::new("harness")),
            spec_path: "specs/harness.md".into(),
            pinned_context: "PIN".into(),
            companion_paths: vec![],
            implementation_notes: vec![],
            scratchpad_path: "/workspace/.loom/scratch/harness/scratch.md".into(),
            spec_conventions: "docs/spec-conventions.md".into(),
        }
    }

    fn inputs_update() -> PlanPromptInputs {
        PlanPromptInputs {
            mode: PlanMode::Update(SpecLabel::new("harness")),
            spec_path: "specs/harness.md".into(),
            pinned_context: "PIN".into(),
            companion_paths: vec!["lib/sandbox/".into()],
            implementation_notes: vec![],
            scratchpad_path: "/workspace/.loom/scratch/harness/scratch.md".into(),
            spec_conventions: "docs/spec-conventions.md".into(),
        }
    }

    #[test]
    fn new_renders_specification_interview_header() {
        let body = render_prompt(inputs_new()).expect("render");
        assert!(body.contains("# Specification Interview"));
        assert!(body.contains("specs/harness.md"));
        assert!(body.contains("LOOM_COMPLETE"));
    }

    #[test]
    fn update_renders_specification_update_header_with_companion() {
        let body = render_prompt(inputs_update()).expect("render");
        assert!(body.contains("# Specification Update Interview"));
        assert!(body.contains("- lib/sandbox/"));
    }

    /// `plan -n` must instruct the agent to call `loom note set <label>`
    /// at the end of the interview to seed the implementation-notes table
    /// for downstream `loom todo` consumption.
    #[test]
    fn new_prompt_instructs_agent_to_call_loom_note_set() {
        let body = render_prompt(inputs_new()).expect("render");
        assert!(
            body.contains("loom note set"),
            "plan_new must mention `loom note set`:\n{body}",
        );
        assert!(
            body.contains("harness"),
            "plan_new must reference the spec label so the CLI invocation is concrete:\n{body}",
        );
        assert!(
            body.contains("--kind implementation"),
            "plan_new must name the implementation note kind:\n{body}",
        );
    }

    /// `plan -u` must render the agent's prior implementation notes verbatim
    /// and frame the rewrite as a keep/drop/add merge (not a blind append,
    /// not a blind replace), then instruct the agent to write the merged
    /// array back via `loom note set`.
    #[test]
    fn update_prompt_renders_existing_notes_and_names_merge_operations() {
        let inputs = PlanPromptInputs {
            mode: PlanMode::Update(SpecLabel::new("harness")),
            spec_path: "specs/harness.md".into(),
            pinned_context: "PIN".into(),
            companion_paths: vec![],
            implementation_notes: vec![
                "note-alpha covers parser invariants".into(),
                "note-beta covers retry/backoff".into(),
            ],
            scratchpad_path: "/workspace/.loom/scratch/harness/scratch.md".into(),
            spec_conventions: "docs/spec-conventions.md".into(),
        };
        let body = render_prompt(inputs).expect("render");
        assert!(
            body.contains("note-alpha covers parser invariants")
                && body.contains("note-beta covers retry/backoff"),
            "plan_update must surface the existing implementation notes verbatim:\n{body}",
        );
        let lower = body.to_lowercase();
        assert!(
            lower.contains("keep") && lower.contains("drop") && lower.contains("add"),
            "plan_update must name all three merge operations (keep/drop/add):\n{body}",
        );
        assert!(
            body.contains("loom note set"),
            "plan_update must instruct the agent to write back via `loom note set`:\n{body}",
        );
    }

    /// When `plan -u` has no prior notes the prompt must still cleanly render
    /// (empty array, not a panic) and still surface the `loom note set`
    /// instruction so the agent seeds the table from scratch.
    #[test]
    fn update_prompt_handles_empty_existing_notes() {
        let inputs = PlanPromptInputs {
            mode: PlanMode::Update(SpecLabel::new("harness")),
            spec_path: "specs/harness.md".into(),
            pinned_context: "PIN".into(),
            companion_paths: vec![],
            implementation_notes: vec![],
            scratchpad_path: "/workspace/.loom/scratch/harness/scratch.md".into(),
            spec_conventions: "docs/spec-conventions.md".into(),
        };
        let body = render_prompt(inputs).expect("render");
        assert!(body.contains("loom note set"));
    }
}
