use askama::Template;
use loom_driver::identifier::{BeadId, MoleculeId, SpecLabel};
use loom_templates::run::{LoopContext, PreviousFailure};

/// Inputs for [`build_loop_context`]. Constructed once per bead spawn — for
/// retries the driver rebuilds with `previous_failure` set + `attempt`
/// incremented.
pub struct LoopContextInputs {
    pub label: SpecLabel,
    pub spec_path: String,
    pub pinned_context: String,
    pub companion_paths: Vec<String>,
    pub molecule_id: Option<MoleculeId>,
    pub issue_id: BeadId,
    pub title: String,
    pub description: String,
    /// Typed retry context from the prior attempt (the driver maps the
    /// verdict-gate's `RecoveryCause` onto the right variant). The template
    /// renders this via `Display` so framing prefixes ride along.
    pub previous_failure: Option<PreviousFailure>,
    /// `Review notes:` companion body — only populated when
    /// `previous_failure` is `VerifyFailures` and the reviewer also raised a
    /// concern.
    pub review_notes: Option<String>,
    /// In-session per-bead retry counter. `0` on fresh dispatch; `loop.md`
    /// omits the retry line when zero (see `specs/templates.md` §
    /// Attempt Counter).
    pub attempt: u32,
    /// Absolute path to `.loom/scratch/<bead-id>/scratch.md` for this
    /// session. Embedded in the rendered prompt so the agent can write to
    /// the correct file under compaction recovery.
    pub scratchpad_path: String,
    /// Workspace-relative path to the project's style-rules document. Pinned
    /// in the rendered prompt so the implementer reads applicable rules
    /// before writing code.
    pub style_rules: String,
}

/// Build the typed [`LoopContext`] for a single bead spawn from the driver's
/// per-iteration inputs.
pub fn build_loop_context(inputs: LoopContextInputs) -> LoopContext {
    LoopContext {
        pinned_context: inputs.pinned_context,
        label: inputs.label,
        spec_path: inputs.spec_path,
        companion_paths: inputs.companion_paths,
        molecule_id: inputs.molecule_id,
        issue_id: Some(inputs.issue_id),
        title: Some(inputs.title),
        description: Some(inputs.description),
        previous_failure: inputs.previous_failure,
        review_notes: inputs.review_notes,
        attempt: inputs.attempt,
        scratchpad_path: inputs.scratchpad_path,
        style_rules: inputs.style_rules,
    }
}

/// Render the loop prompt for `inputs` so binaries that lack a direct askama
/// dependency can build the same prompt as the workflow's own controllers.
pub fn render_loop_prompt(inputs: LoopContextInputs) -> Result<String, askama::Error> {
    build_loop_context(inputs).render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use askama::Template;

    fn inputs() -> LoopContextInputs {
        LoopContextInputs {
            label: SpecLabel::new("harness"),
            spec_path: "specs/harness.md".into(),
            pinned_context: "PIN".into(),
            companion_paths: vec![],
            molecule_id: Some(MoleculeId::new("lm-3hhwq")),
            issue_id: BeadId::new("lm-3hhwq.15").expect("valid bead id"),
            title: "Implement loom loop".into(),
            description: "Per-bead loop".into(),
            previous_failure: None,
            review_notes: None,
            attempt: 0,
            scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.15/scratch.md".into(),
            style_rules: "docs/style-rules.md".into(),
        }
    }

    #[test]
    fn retry_input_carries_typed_previous_failure() {
        let mut i = inputs();
        i.previous_failure = Some(PreviousFailure::from_agent_error("cargo test failed"));
        i.attempt = 1;
        let ctx = build_loop_context(i);
        let pf = ctx.previous_failure.expect("set on retry");
        let rendered = pf.to_string();
        assert!(
            rendered.contains("cargo test failed"),
            "rendered body must carry agent error: {rendered}",
        );
        assert_eq!(ctx.attempt, 1);
    }

    #[test]
    fn fresh_dispatch_omits_previous_failure_and_attempt_is_zero() {
        let ctx = build_loop_context(inputs());
        assert!(ctx.previous_failure.is_none());
        assert_eq!(ctx.attempt, 0);
    }

    #[test]
    fn attempt_zero_on_fresh_bead_dispatch() {
        let ctx = build_loop_context(inputs());
        assert_eq!(ctx.attempt, 0);
    }

    #[test]
    fn rendered_prompt_includes_issue_and_title() {
        let ctx = build_loop_context(inputs());
        let body = ctx.render().expect("render");
        assert!(body.contains("lm-3hhwq.15"), "{body}");
        assert!(body.contains("Implement loom loop"), "{body}");
    }

    #[test]
    fn rendered_retry_prompt_includes_previous_failure_body() {
        let mut i = inputs();
        i.previous_failure = Some(PreviousFailure::from_agent_error(
            "STDERR: cargo test failure",
        ));
        i.attempt = 1;
        let ctx = build_loop_context(i);
        let body = ctx.render().expect("render");
        assert!(body.contains("STDERR: cargo test failure"), "{body}");
    }
}
