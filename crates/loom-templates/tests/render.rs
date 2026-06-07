//! Integration tests that exercise every context struct's render path.
//!
//! Acceptance for `test_askama_templates_compile`, `test_template_partials`,
//! `test_template_output_parity`, and `test_template_compile_time_check`:
//! every template lives behind a typed context, partials resolve via
//! `{% include %}`, agent-supplied content is wrapped in `<agent-output>` and
//! `previous_failure` truncates at [`PREVIOUS_FAILURE_MAX_LEN`].

use anyhow::Result;
use askama::Template;
use loom_events::identifier::{BeadId, MoleculeId, ProfileName, SpecLabel};
use loom_templates::criterion_status::{CriterionResult, CriterionStatus};
use loom_templates::finding::{ConcernToken, Finding, FindingTarget};
use loom_templates::msg::{BeadKind, ClarifyBead, ClarifyOption, MsgContext};
use loom_templates::plan::{PlanNewContext, PlanUpdateContext};
use loom_templates::review::{ReviewContext, ReviewLane, ReviewSource};
use loom_templates::run::{
    DriverNoticeCause, LoopContext, PREVIOUS_FAILURE_MAX_LEN, PreviousFailure, VerifierFailure,
};
use loom_templates::todo::{TodoNewContext, TodoUpdateContext};

const PINNED_CONTEXT_BODY: &str =
    "# Project Overview\n\nLoom orchestrates the spec-to-implementation workflow.";
const SCRATCHPAD_PATH_BODY: &str = "/workspace/.loom/scratch/harness/scratch.md";

#[test]
fn plan_new_renders_partials_and_inputs() -> Result<()> {
    let ctx = PlanNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Specification Interview"));
    assert!(out.contains(PINNED_CONTEXT_BODY));
    assert!(out.contains("Label: harness"));
    assert!(out.contains("Spec file: specs/harness.md"));
    assert!(out.contains("LOOM_COMPLETE"));
    assert!(out.contains("Interview Modes"));
    Ok(())
}

#[test]
fn plan_update_renders_partials_and_companions() -> Result<()> {
    let ctx = PlanUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![
            "lib/sandbox/".into(),
            "crates/loom-templates/templates/".into(),
        ],
        implementation_notes: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Specification Update Interview"));
    assert!(out.contains("- lib/sandbox/"));
    assert!(out.contains("- crates/loom-templates/templates/"));
    assert!(out.contains("Anchor Session & Sibling-Spec Editing"));
    assert!(out.contains("Invariant-Clash Awareness"));
    Ok(())
}

/// Pins the three Plan-stage rubric checks (completeness, internal
/// coherence, invariant-clash) into both planning templates per
/// `specs/gate.md` § Plan-stage checks. A future refactor must not
/// silently drop any of the three from the prompt.
#[test]
fn plan_templates_render_three_plan_stage_checks() -> Result<()> {
    let new_out = PlanNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
    }
    .render()?;
    let update_out = PlanUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        implementation_notes: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
    }
    .render()?;

    for (name, out) in [("plan_new", &new_out), ("plan_update", &update_out)] {
        assert!(
            out.contains("Plan-Stage Rubric"),
            "{name}: rubric heading missing"
        );
        assert!(
            out.contains("Completeness check"),
            "{name}: completeness check missing"
        );
        assert!(
            out.contains("Internal coherence check"),
            "{name}: internal coherence check missing"
        );
        assert!(
            out.contains("Invariant-clash scan") || out.contains("Invariant-Clash Awareness"),
            "{name}: invariant-clash check missing"
        );
        assert!(
            out.contains("three-paths"),
            "{name}: invariant-clash three-paths protocol not described"
        );
    }
    Ok(())
}

/// FR14 compliance: plan_new.md must not teach `[ ]` / `[x]` checkbox
/// syntax, must not instruct an `Affected Files` section, and must defer
/// the spec format to `docs/spec-conventions.md` rather than re-describing
/// it. Pins the audit findings (TA-1, TA-2, TA-7) so a refactor cannot
/// regress them.
#[test]
fn plan_new_defers_spec_format_to_conventions_doc() -> Result<()> {
    let out = PlanNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
    }
    .render()?;

    assert!(
        out.contains("docs/spec-conventions.md"),
        "plan_new must defer to docs/spec-conventions.md"
    );
    assert!(
        !out.contains("[ ] CLI") && !out.contains("[ ] Error"),
        "plan_new must not teach `[ ]` checkbox examples"
    );
    assert!(
        !out.contains("Affected files/modules") && !out.contains("Affected Files"),
        "plan_new must not instruct an Affected Files section"
    );
    Ok(())
}

#[test]
fn todo_new_renders_spec_label_marker() -> Result<()> {
    let ctx = TodoNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        implementation_notes: vec![],
        criterion_status: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Task Decomposition"));
    assert!(out.contains("spec:harness"));
    // Empty notes → the section header is suppressed entirely (no empty
    // "## Implementation Notes" header in the prompt).
    assert!(!out.contains("## Implementation Notes"));
    Ok(())
}

#[test]
fn todo_new_renders_implementation_notes_when_present() -> Result<()> {
    let ctx = TodoNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        implementation_notes: vec![
            "Hidden constraint: touch lib/sandbox/linux/default.nix".into(),
            "Design trade-off: prefer single FK over join table".into(),
        ],
        criterion_status: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;
    assert!(out.contains("## Implementation Notes"));
    assert!(out.contains("Hidden constraint: touch lib/sandbox/linux/default.nix"));
    assert!(out.contains("Design trade-off: prefer single FK over join table"));
    assert_eq!(out.matches("<implementation-note>").count(), 2);
    assert_eq!(out.matches("</implementation-note>").count(), 2);
    Ok(())
}

#[test]
fn todo_update_renders_implementation_notes_when_present() -> Result<()> {
    let ctx = TodoUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        spec_diff: Some("=== specs/harness.md ===\n+ change".into()),
        existing_tasks: None,
        molecule_id: Some(MoleculeId::new("lm-mol")),
        implementation_notes: vec!["beware FK cascade ordering".into()],
        criterion_status: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;
    assert!(out.contains("## Implementation Notes"));
    assert!(out.contains("beware FK cascade ordering"));
    assert!(out.contains("<implementation-note>"));
    Ok(())
}

#[test]
fn todo_update_wraps_existing_tasks_in_agent_output() -> Result<()> {
    let ctx = TodoUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        spec_diff: Some("=== specs/harness.md ===\n+ new requirement".into()),
        existing_tasks: Some("- lm-3hhwq.1: scaffold workspace".into()),
        molecule_id: Some(MoleculeId::new("lm-3hhwq")),
        implementation_notes: vec![],
        criterion_status: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Add Tasks to Existing Molecule"));
    assert!(out.contains("=== specs/harness.md ==="));
    assert!(out.contains("Molecule ID: lm-3hhwq"));
    let agent_open = out.find("<agent-output>");
    let agent_close = out.find("</agent-output>");
    assert!(agent_open.is_some() && agent_close.is_some());
    let (open, close) = (agent_open.unwrap_or(0), agent_close.unwrap_or(0));
    assert!(open < close);
    let inside = &out[open..close];
    assert!(inside.contains("lm-3hhwq.1: scaffold workspace"));
    Ok(())
}

#[test]
fn run_wraps_agent_supplied_fields_in_agent_output() -> Result<()> {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        molecule_id: Some(MoleculeId::new("lm-3hhwq")),
        issue_id: Some(BeadId::new("lm-3hhwq.10")?),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::from_agent_error(
            "error: cargo test failed",
        )),
        review_notes: None,
        attempt: 1,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Implementation Step"));
    assert!(out.contains("Issue: lm-3hhwq.10"));
    assert!(out.contains("Title: <agent-output>port templates</agent-output>"));
    assert!(out.contains("Port templates to Askama."));
    assert!(out.contains("error: cargo test failed"));
    let count_open = out.matches("<agent-output>").count();
    let count_close = out.matches("</agent-output>").count();
    assert_eq!(count_open, count_close);
    assert!(
        count_open >= 3,
        "expected at least 3 agent-output blocks, got {count_open}"
    );
    Ok(())
}

#[test]
fn run_template_omits_attempt_line_when_zero() -> Result<()> {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: Some(BeadId::new("lm-3hhwq.10")?),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;
    assert!(
        !out.contains("Retry attempt"),
        "fresh dispatch must omit retry line: {out}",
    );
    Ok(())
}

#[test]
fn run_template_renders_attempt_line_on_retry() -> Result<()> {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: Some(BeadId::new("lm-3hhwq.10")?),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::ZeroProgress,
            detail: "Marker `LOOM_COMPLETE` emitted with empty diff.".into(),
        }),
        review_notes: None,
        attempt: 2,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;
    assert!(
        out.contains("Retry attempt 2 — previous attempt failed with:"),
        "retry line missing: {out}",
    );
    assert!(out.contains("Previous attempt: "), "framing missing: {out}");
    Ok(())
}

/// Per `specs/harness.md` § Recovery context, the run prompt must
/// prepend a first-instruction reframe when `previous_failure.is_some() &&
/// attempt > 0`. Pins the canonical wording and ordering so a refactor
/// cannot silently drop or move the reframe.
#[test]
fn run_template_prepends_first_instruction_reframe_on_retry() -> Result<()> {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: Some(BeadId::new("lm-3hhwq.10")?),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::ZeroProgress,
            detail: "Marker `LOOM_COMPLETE` emitted with empty diff.".into(),
        }),
        review_notes: None,
        attempt: 1,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;
    let reframe = "> Re-read the previous failure block above and address its specific\n> concern before re-implementing.";
    assert!(out.contains(reframe), "reframe missing: {out}");
    let instructions_heading = out
        .find("## Instructions")
        .expect("## Instructions heading present");
    let reframe_pos = out.find(reframe).expect("reframe present");
    let first_step = out
        .find("1. **Understand**")
        .expect("first numbered step present");
    assert!(
        instructions_heading < reframe_pos && reframe_pos < first_step,
        "reframe must sit between the heading and step 1: heading={instructions_heading} reframe={reframe_pos} step1={first_step}",
    );
    Ok(())
}

/// Pins the false branch: fresh dispatch (`attempt = 0`, no
/// `previous_failure`) must not include the reframe blockquote — instruction
/// 1 follows the heading directly.
#[test]
fn run_template_omits_first_instruction_reframe_on_fresh_dispatch() -> Result<()> {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: Some(BeadId::new("lm-3hhwq.10")?),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;
    assert!(
        !out.contains("Re-read the previous failure block above"),
        "reframe must be absent on fresh dispatch: {out}",
    );
    Ok(())
}

/// Defensive boundary: `previous_failure.is_some()` alone is not enough —
/// the driver must also have bumped `attempt` past zero. If a caller wires
/// `attempt = 0` while supplying a `previous_failure`, the template stays in
/// the false branch (mirroring the existing `Retry attempt` line, which is
/// already gated on `attempt > 0`).
#[test]
fn run_template_omits_first_instruction_reframe_when_attempt_zero() -> Result<()> {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: Some(BeadId::new("lm-3hhwq.10")?),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::ZeroProgress,
            detail: "stray previous_failure with attempt=0".into(),
        }),
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;
    assert!(
        !out.contains("Re-read the previous failure block above"),
        "reframe must be absent when attempt is 0: {out}",
    );
    Ok(())
}

#[test]
fn run_template_renders_review_notes_block_when_set() -> Result<()> {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: Some(BeadId::new("lm-3hhwq.10")?),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::VerifyFailures(vec![VerifierFailure::new(
            "tests/a.sh",
            1,
            "boom\n",
        )])),
        review_notes: Some("[verifier-bypass] test mocks the agent backend".into()),
        attempt: 1,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;
    assert!(out.contains("Review notes:"), "heading missing: {out}");
    assert!(
        out.contains("[verifier-bypass] test mocks the agent backend"),
        "review-notes body missing: {out}",
    );
    Ok(())
}

#[test]
fn previous_failure_truncates_at_max_len() {
    let huge = "x".repeat(PREVIOUS_FAILURE_MAX_LEN * 2);
    let pf = PreviousFailure::BuildFailure {
        stage: "cargo".into(),
        output: huge,
    };
    assert!(pf.to_string().len() <= PREVIOUS_FAILURE_MAX_LEN);
}

#[test]
fn previous_failure_renders_review_concern_with_summary_and_findings() {
    let pf = PreviousFailure::ReviewConcern {
        summary: "mock under test".into(),
        findings: vec![Finding {
            token: ConcernToken::MockDiscipline,
            route: loom_protocol::gate::FindingRoute::Deferred,
            bonds: vec![SpecLabel::new("harness")],
            target: FindingTarget::TestPath {
                path: "tests/example.rs".into(),
            },
            evidence: "mock is the thing under test".into(),
        }],
    };
    let rendered = pf.to_string();
    assert!(
        rendered.starts_with("Review raised a concern (mock-discipline): mock under test"),
        "label-prefixed framing missing: {rendered}",
    );
    assert!(
        rendered.contains("mock-discipline @ test:tests/example.rs"),
        "token+target missing: {rendered}",
    );
    assert!(
        rendered.contains("mock is the thing under test"),
        "{rendered}"
    );
}

#[test]
fn review_renders_review_context_fields() -> Result<()> {
    let test_path = "tests/run-tests.sh";
    let test_body = "test_review_inputs_include_judge_rubrics_signature() { :; }\n";
    let judge_path = "tests/judges/loom.sh";
    let judge_body = "judge_live_path_coverage_signature() { :; }\n";

    let ctx = ReviewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        beads_summary: Some("- lm-3hhwq.10: closed".into()),
        base_commit: Some("abc1234".into()),
        molecule_id: Some(MoleculeId::new("lm-3hhwq")),
        test_sources: vec![ReviewSource {
            path: test_path.into(),
            body: test_body.into(),
        }],
        judge_rubrics: vec![ReviewSource {
            path: judge_path.into(),
            body: judge_body.into(),
        }],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        lane: ReviewLane::Both,
        default_profile: ProfileName::new("base"),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Post-Epic Review"));
    assert!(out.contains("Base commit**: abc1234"));
    assert!(out.contains("Molecule**: lm-3hhwq"));
    assert!(out.contains("git diff abc1234..HEAD"));
    assert!(out.contains("- lm-3hhwq.10: closed"));

    assert!(out.contains("## Deterministic-Verifier Sources"));
    assert!(out.contains(test_path), "test path missing: {out}");
    assert!(out.contains(test_body.trim()), "test body missing: {out}");

    assert!(out.contains("## `[judge]` Rubrics"));
    assert!(out.contains(judge_path), "judge path missing: {out}");
    assert!(out.contains(judge_body.trim()), "judge body missing: {out}");
    Ok(())
}

/// `ReviewLane::Judge` narrows the prompt to the `[judge]` rubric evaluation
/// lane — the `[judge]` rubric bodies still render so the agent has its
/// inputs, but the rubric walk over the diff (Review Dimensions,
/// review_rubric.md content, Invariant-Clash Detection) is suppressed.
/// Pins the per-lane render contract for `loom gate judge`.
#[test]
fn review_lane_judge_omits_rubric_walk_sections_and_keeps_judge_rubrics() -> Result<()> {
    let ctx = ReviewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        beads_summary: None,
        base_commit: None,
        molecule_id: None,
        test_sources: vec![],
        judge_rubrics: vec![ReviewSource {
            path: "tests/judges/loom.sh".into(),
            body: "JUDGE_BODY_MARKER".into(),
        }],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        lane: ReviewLane::Judge,
        default_profile: ProfileName::new("base"),
    };
    let out = ctx.render()?;

    assert!(
        out.contains("## `[judge]` Rubrics"),
        "judge lane must keep [judge] rubrics section: {out}",
    );
    assert!(
        out.contains("JUDGE_BODY_MARKER"),
        "judge lane must inline judge rubric bodies: {out}",
    );
    assert!(
        !out.contains("## Review Dimensions"),
        "judge lane must suppress Review Dimensions: {out}",
    );
    assert!(
        !out.contains("## Verifier Honesty"),
        "judge lane must suppress verifier-honesty rubric: {out}",
    );
    assert!(
        !out.contains("## Style-Rule Conformance"),
        "judge lane must suppress style-rule walk: {out}",
    );
    assert!(
        !out.contains("## Invariant-Clash Detection"),
        "judge lane must suppress invariant-clash detection: {out}",
    );
    Ok(())
}

/// `ReviewLane::Rubric` narrows the prompt to the rubric walk over the diff —
/// the rubric content (Review Dimensions, verifier honesty, style-rule
/// conformance, invariant-clash detection) all render, but the `[judge]`
/// rubric bodies are suppressed. Pins the per-lane render contract for
/// `loom gate rubric`.
#[test]
fn review_lane_rubric_omits_judge_rubrics_and_keeps_rubric_walk_sections() -> Result<()> {
    let ctx = ReviewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        beads_summary: None,
        base_commit: None,
        molecule_id: None,
        test_sources: vec![],
        judge_rubrics: vec![ReviewSource {
            path: "tests/judges/loom.sh".into(),
            body: "JUDGE_BODY_MARKER".into(),
        }],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        lane: ReviewLane::Rubric,
        default_profile: ProfileName::new("base"),
    };
    let out = ctx.render()?;

    assert!(
        !out.contains("## `[judge]` Rubrics"),
        "rubric lane must suppress [judge] rubrics section: {out}",
    );
    assert!(
        !out.contains("JUDGE_BODY_MARKER"),
        "rubric lane must not inline judge rubric bodies: {out}",
    );
    assert!(
        out.contains("## Review Dimensions"),
        "rubric lane must keep Review Dimensions: {out}",
    );
    assert!(
        out.contains("## Verifier Honesty"),
        "rubric lane must keep verifier-honesty rubric: {out}",
    );
    assert!(
        out.contains("## Style-Rule Conformance"),
        "rubric lane must keep style-rule walk: {out}",
    );
    assert!(
        out.contains("## Invariant-Clash Detection"),
        "rubric lane must keep invariant-clash detection: {out}",
    );
    Ok(())
}

/// The review rubric must walk `{{ style_rules }}` rule by rule and require
/// rule-id + file/line citations. Per `specs/templates.md` *Style-Rules
/// Partial*, the rubric is **rule-family-agnostic**: it tells the judge to
/// discover families from the pinned document rather than enumerate them in
/// the prompt. This test pins those directives so a future refactor cannot
/// silently drop them.
#[test]
fn review_renders_style_rule_conformance_walkthrough() -> Result<()> {
    let ctx = ReviewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        beads_summary: None,
        base_commit: None,
        molecule_id: None,
        test_sources: vec![],
        judge_rubrics: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        lane: ReviewLane::Both,
        default_profile: ProfileName::new("base"),
    };
    let out = ctx.render()?;

    assert!(
        out.contains("## Style-Rule Conformance"),
        "rubric section heading missing: {out}",
    );
    assert!(
        out.contains("docs/style-rules.md"),
        "style_rules path not pinned: {out}",
    );
    assert!(
        out.contains("Discover the families") && out.contains("do not assume a fixed prefix list"),
        "family-discovery instruction missing: {out}",
    );
    assert!(
        out.contains("rule id"),
        "citation contract (rule id) not described: {out}",
    );
    assert!(
        out.contains("file and line range") || out.contains("file/line range"),
        "citation contract (file/line range) not described: {out}",
    );
    assert!(
        out.contains("`style-rule-violation`") || out.contains("`style-rule`"),
        "style-rule-violation concern token missing from flag schema: {out}",
    );
    assert!(
        out.contains("LOOM_CONCERN"),
        "terminator marker not documented for style-rule walk: {out}",
    );
    for forbidden in ["**SH-**", "**NX-**", "**RS-**", "**COM-**", "**CLI-**"] {
        assert!(
            !out.contains(forbidden),
            "rule-family marker {forbidden} leaked into prompt: {out}",
        );
    }
    Ok(())
}

/// A.7 — the rendered review template instructs the agent to emit
/// exactly one terminal marker per session and forbids co-emission of
/// `LOOM_CONCERN` + `LOOM_COMPLETE`. The May-19 incident was a
/// reviewer agent emitting `LOOM_REVIEW_FLAG:` (the legacy name) and
/// `LOOM_COMPLETE` together; A.7 renamed the marker and rewrote the
/// instructions to be mutually exclusive. This test pins the rendered
/// surface so a future template edit cannot silently undo either
/// contract.
#[test]
fn review_renders_single_marker_instruction_with_concern_xor_complete() -> Result<()> {
    let ctx = ReviewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        beads_summary: None,
        base_commit: None,
        molecule_id: None,
        test_sources: vec![],
        judge_rubrics: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        lane: ReviewLane::Both,
        default_profile: ProfileName::new("base"),
    };
    let out = ctx.render()?;

    assert!(
        out.contains("LOOM_CONCERN")
            && (out.contains("never emit both") || out.contains("never both")),
        "review template must forbid LOOM_CONCERN + LOOM_COMPLETE co-emission. \
         body:\n{out}",
    );
    assert!(
        out.contains("xor")
            || out.contains("mutually exclusive")
            || out.contains("one and only one"),
        "rendered template must instruct mutual exclusivity for the final-line \
         marker. body:\n{out}",
    );
    assert!(
        !out.contains("LOOM_REVIEW_FLAG"),
        "rendered template must not reference the legacy LOOM_REVIEW_FLAG keyword. \
         body:\n{out}",
    );
    Ok(())
}

/// Pins the Options Format Contract: the agent embeds the canonical
/// `## Options — …` block inside the `evidence` field of any
/// `route="clarify"` `LOOM_FINDING:` line; the driver lifts it into
/// the minted clarify bead's description. The contract is universal
/// (applies to every clarify-worthy decision, not just invariant
/// clashes) and never directs the reviewer to bd-write.
#[test]
fn review_renders_options_format_contract_embedded_in_evidence() -> Result<()> {
    let ctx = ReviewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        beads_summary: None,
        base_commit: None,
        molecule_id: None,
        test_sources: vec![],
        judge_rubrics: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        lane: ReviewLane::Both,
        default_profile: ProfileName::new("base"),
    };
    let out = ctx.render()?;

    assert!(
        out.contains("Options Format Contract"),
        "Options Format Contract section missing: {out}",
    );
    assert!(
        out.contains("`evidence`") && out.contains("route=\"clarify\""),
        "contract must direct the agent to embed Options block in each clarify-route finding's evidence field: {out}",
    );
    assert!(
        out.contains("clarify situation"),
        "contract scope must remain universal across clarify-worthy decisions: {out}",
    );
    assert!(
        out.contains("gate does NOT parse your prose")
            || out.contains("does not scrape")
            || out.contains("does NOT parse your prose"),
        "persistence-boundary statement missing: {out}",
    );
    assert!(
        out.contains("## Options —") && out.contains("### Option 1 —"),
        "canonical Options block shape missing: {out}",
    );
    Ok(())
}

/// The review phase is inspection-only: the rendered prompt must
/// instruct the agent to stream `LOOM_FINDING:` JSON lines and **not**
/// to invoke `bd find` / `bd create` / `bd update` / `bd mol bond`.
/// Driver-side `loom gate mint` consumes the streamed findings and
/// performs the bd writes itself (per `specs/gate.md` §
/// *Findings and Minting*).
#[test]
fn review_prompt_is_inspection_only_and_documents_loom_finding_wire_format() -> Result<()> {
    let ctx = ReviewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("alpha"),
        spec_path: "specs/alpha.md".to_string(),
        companion_paths: vec![],
        beads_summary: None,
        base_commit: None,
        molecule_id: None,
        test_sources: vec![],
        judge_rubrics: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        lane: ReviewLane::Both,
        default_profile: ProfileName::new("base"),
    };
    let out = ctx.render()?;

    for forbidden_heading in [
        "Current Molecule Mapping",
        "Authorization — Bead Mutations",
        "Recovery Epic Resolution",
        "Handling Each Clash",
        "## Creating Fix-Up Beads",
        "## Flag Emission Schema",
    ] {
        assert!(
            !out.contains(forbidden_heading),
            "review prompt must not contain `{forbidden_heading}` heading — phase is inspection-only: {out}",
        );
    }

    // No bash code blocks should instruct the agent to invoke bd
    // mutations — every `bd create` / `bd update --notes` / `bd mol bond`
    // example block from the legacy template body must be gone. Negative
    // references in prose ("do NOT invoke `bd create`") are fine.
    assert!(
        !out.contains("```bash"),
        "review prompt must contain no bash code blocks — every legacy bd-write example must be gone: {out}",
    );

    assert!(
        out.contains("LOOM_FINDING:"),
        "review prompt must document the LOOM_FINDING emit shape: {out}",
    );
    assert!(
        out.contains("inspection-only"),
        "review prompt must declare itself inspection-only: {out}",
    );
    assert!(
        out.contains(r#""token""#)
            && out.contains(r#""route""#)
            && out.contains(r#""bonds""#)
            && out.contains(r#""target""#)
            && out.contains(r#""evidence""#),
        "LOOM_FINDING JSON shape (token/route/bonds/target/evidence) must be documented: {out}",
    );

    for variant_shape in [
        r#"{"kind":"Criterion","spec":"#,
        r#"{"kind":"Contract","id":"#,
        r#"{"kind":"StyleRule","rule_id":"#,
        r#""subject":"#,
        r#"{"kind":"Annotation","target_string":"#,
        r#"{"kind":"TestPath","path":"#,
        r#"{"kind":"LockSite","file":"#,
        r#"{"kind":"Invariant","spec":"#,
        r#"{"kind":"Template","path":"#,
    ] {
        assert!(
            out.contains(variant_shape),
            "canonical target shape `{variant_shape}` missing from prompt: {out}",
        );
    }

    assert!(
        out.contains("target.spec") && out.contains("MUST appear in `bonds`"),
        "validation rule (Criterion/Invariant target.spec ∈ bonds) must be documented: {out}",
    );
    Ok(())
}

#[test]
fn msg_renders_clarify_beads_with_options() -> Result<()> {
    let ctx = MsgContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        clarify_beads: vec![ClarifyBead {
            id: BeadId::new("lm-clar.1")?,
            spec_label: SpecLabel::new("harness"),
            title: "State storage choice".into(),
            options_summary: Some("State JSON vs. dedicated table".into()),
            options: vec![
                ClarifyOption {
                    n: 1,
                    title: Some("Keep state in JSON".into()),
                    body: Some("Add a companions array.".into()),
                },
                ClarifyOption {
                    n: 2,
                    title: Some("Migrate to a table".into()),
                    body: Some("Use a SQLite table.".into()),
                },
            ],
            kind: BeadKind::Clarify,
        }],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Clarify Resolution — Drafter Session"));
    assert!(out.contains("### lm-clar.1 — [spec:harness] State storage choice"));
    assert!(out.contains("## Options — State JSON vs. dedicated table"));
    assert!(out.contains("#### Option 1 — Keep state in JSON"));
    assert!(out.contains("Add a companions array."));
    assert!(out.contains("#### Option 2 — Migrate to a table"));
    assert!(out.contains("Use a SQLite table."));
    assert!(out.contains("## Companions"));
    assert!(out.contains("- lib/sandbox/"));
    Ok(())
}

/// `loom:blocked` beads carry no `## Options` block. The template must
/// render a distinct flow for them — naming the kind explicitly and
/// telling the drafter to walk the user through enumerating candidates
/// — rather than the clarify framing that asserts "the reviewer has
/// already presented options."
#[test]
fn msg_renders_blocked_bead_with_enumerate_first_framing() -> Result<()> {
    let ctx = MsgContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        companion_paths: vec![],
        clarify_beads: vec![ClarifyBead {
            id: BeadId::new("lm-block.1")?,
            spec_label: SpecLabel::new("harness"),
            title: "Push hook fails inside sandbox".into(),
            options_summary: None,
            options: vec![],
            kind: BeadKind::Blocked,
        }],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("### lm-block.1 — [spec:harness] Push hook fails inside sandbox"));
    assert!(
        out.contains("`loom:blocked`"),
        "kind line must name loom:blocked: {out}",
    );
    assert!(
        out.contains("enumerat"),
        "blocked bead must trigger enumerate-first framing: {out}",
    );
    assert!(
        !out.contains("#### Option "),
        "no enumerated options should be rendered for blocked beads without notes: {out}",
    );
    Ok(())
}

/// A `loom:clarify` bead carries its options; the template should
/// render the existing clarify framing without mentioning the
/// enumerate-first language meant for `loom:blocked`.
#[test]
fn msg_renders_clarify_bead_without_enumerate_first_framing() -> Result<()> {
    let ctx = MsgContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        companion_paths: vec![],
        clarify_beads: vec![ClarifyBead {
            id: BeadId::new("lm-clar.2")?,
            spec_label: SpecLabel::new("harness"),
            title: "Adopt new API surface".into(),
            options_summary: Some("Pick API shape".into()),
            options: vec![ClarifyOption {
                n: 1,
                title: Some("Keep existing".into()),
                body: Some("Defer the change.".into()),
            }],
            kind: BeadKind::Clarify,
        }],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;
    assert!(out.contains("`loom:clarify`"));
    assert!(out.contains("## Options — Pick API shape"));
    assert!(out.contains("#### Option 1 — Keep existing"));
    Ok(())
}

/// Per `specs/gate.md` § Resolution lifecycle, the rendered msg.md chat
/// instructions must direct the Drafter agent to strip the originating
/// `## Options — …` block from the bead's notes in the same `bd update`
/// that records the resolution note. Without this clause the chat path
/// drifts from the driver's `-o`/`-r`/`-d` paths (which call
/// `compose_resolved_notes` to do the same strip) and accumulating
/// clarifies on the molecule epic stop being lifecycle-clean.
#[test]
fn msg_chat_template_instructs_options_block_removal_on_resolution() -> Result<()> {
    let ctx = MsgContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        companion_paths: vec![],
        clarify_beads: vec![ClarifyBead {
            id: BeadId::new("lm-clar.3")?,
            spec_label: SpecLabel::new("harness"),
            title: "Pick a thing".into(),
            options_summary: Some("Pick".into()),
            options: vec![ClarifyOption {
                n: 1,
                title: Some("A".into()),
                body: Some("body".into()),
            }],
            kind: BeadKind::Clarify,
        }],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;
    assert!(
        out.contains("remove the originating `## Options"),
        "chat template must direct the agent to remove the originating Options block: {out}",
    );
    assert!(
        out.contains("Resolution lifecycle") || out.contains("specs/gate.md"),
        "chat template must anchor the contract to the spec citation: {out}",
    );
    Ok(())
}

/// Per `specs/templates.md` Implementation Note 5, the chat agent owns
/// the full bd-state transition during a msg session — including
/// `bd close` on resolved beads and `--remove-label` on unblock-without-
/// close. The driver does NOT reconcile bd state after the session.
/// This test pins that the template names the writes the agent must
/// make so the agent has explicit authority.
#[test]
fn msg_template_teaches_agent_bd_write_authority() -> Result<()> {
    let ctx = MsgContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        companion_paths: vec![],
        clarify_beads: vec![ClarifyBead {
            id: BeadId::new("lm-clar.9")?,
            spec_label: SpecLabel::new("harness"),
            title: "Pick a thing".into(),
            options_summary: Some("Pick".into()),
            options: vec![ClarifyOption {
                n: 1,
                title: Some("A".into()),
                body: Some("body".into()),
            }],
            kind: BeadKind::Clarify,
        }],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;
    for required in ["bd update", "bd close", "--remove-label"] {
        assert!(
            out.contains(required),
            "msg.md must name the literal `{required}` so the chat agent \
             has explicit bd-write authority per Implementation Note 5",
        );
    }
    Ok(())
}

/// Per `specs/templates.md` criterion 979, the rendered `msg.md` prompt
/// must surface both chat-discipline clauses sourced from
/// `partial/chat_interview.md`: the picker prohibition (criterion 971
/// rule body) and the bd-persistence clause (criterion 974 rule body).
/// The msg session is interactive, so the chat-discipline partial is
/// pinned there alongside the planning templates.
#[test]
fn msg_template_renders_chat_interview_discipline() -> Result<()> {
    let ctx = MsgContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        companion_paths: vec![],
        clarify_beads: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(
        out.contains("AskUserQuestion") || out.contains("option-picker"),
        "msg.md must surface the picker prohibition from chat_interview.md: {out}",
    );
    assert!(
        out.contains("MEMORY.md"),
        "msg.md must surface the MEMORY.md container-local clause from chat_interview.md: {out}",
    );
    assert!(
        out.contains("bd update <id> --notes"),
        "msg.md must surface the bd-persistence destination from chat_interview.md: {out}",
    );
    Ok(())
}

#[test]
fn msg_renders_with_no_clarify_beads() -> Result<()> {
    let ctx = MsgContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        companion_paths: vec![],
        clarify_beads: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Clarify Resolution — Drafter Session"));
    assert!(!out.contains("### lm-"));
    Ok(())
}

/// `msg`, `plan_new`, and `plan_update` are all multi-turn templates
/// that include `exit_signals.md`, a partial written for single-shot
/// worker phases ("end your response with the marker"). Each must
/// disambiguate via the `chat_marker_final_turn_only.md` partial,
/// stating that `LOOM_COMPLETE` is emitted on the final assistant turn
/// only — never on intermediate turns. The May-21 bug reports were the
/// chat agent (lm-lq12o) and planning agent (lm-qzdhj) appending
/// `LOOM_COMPLETE` to every reply mid-conversation; this test pins the
/// clarifying clause across the entire multi-turn template set so a
/// future edit cannot silently regress any one of them.
#[test]
fn every_multi_turn_template_includes_chat_marker_partial() -> Result<()> {
    let msg_out = MsgContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        companion_paths: vec![],
        clarify_beads: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    }
    .render()?;
    let plan_new_out = PlanNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
    }
    .render()?;
    let plan_update_out = PlanUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        implementation_notes: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
    }
    .render()?;

    for (name, out) in [
        ("msg", &msg_out),
        ("plan_new", &plan_new_out),
        ("plan_update", &plan_update_out),
    ] {
        assert!(
            out.contains("final turn only") || out.contains("final assistant turn"),
            "{name}: chat-restrictions must name the final-turn-only rule: {out}",
        );
        assert!(
            out.contains("Do **NOT** append `LOOM_COMPLETE` to intermediate turns")
                || out.contains("not on intermediate turns"),
            "{name}: chat-restrictions must explicitly forbid intermediate-turn markers: {out}",
        );
    }
    Ok(())
}

/// One-shot worker templates (`run`, `todo_*`, `review`) deliberately
/// omit the chat-mode final-turn restriction: every response in those
/// phases IS the final output, so the wrap-up clause is meaningless and
/// could confuse the agent into delaying the marker. This test pins the
/// asymmetry — worker templates must not pick up the chat-only partial
/// by accident (e.g. via a copy-pasted include).
#[test]
fn worker_templates_omit_chat_final_turn_clause() -> Result<()> {
    let run_out = LoopContext {
        pinned_context: "PIN".into(),
        label: SpecLabel::new("demo"),
        spec_path: "specs/demo.md".into(),
        companion_paths: vec![],
        molecule_id: Some(MoleculeId::new("lm-mol")),
        issue_id: Some(BeadId::new("lm-mol.1")?),
        title: Some("the title".into()),
        description: Some("the description".into()),
        previous_failure: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".into(),
    }
    .render()?;

    let todo_new_out = TodoNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        implementation_notes: vec![],
        criterion_status: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    }
    .render()?;

    let todo_update_out = TodoUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        spec_diff: None,
        existing_tasks: None,
        molecule_id: Some(MoleculeId::new("lm-mol")),
        implementation_notes: vec![],
        criterion_status: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    }
    .render()?;

    let review_out = ReviewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        beads_summary: None,
        base_commit: None,
        molecule_id: None,
        test_sources: vec![],
        judge_rubrics: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        lane: ReviewLane::Both,
        default_profile: ProfileName::new("base"),
    }
    .render()?;

    for (name, out) in [
        ("run", &run_out),
        ("todo_new", &todo_new_out),
        ("todo_update", &todo_update_out),
        ("review", &review_out),
    ] {
        assert!(
            !out.contains("intermediate turns"),
            "{name}: worker template must not include the chat-only final-turn clause; output: {out}",
        );
        assert!(
            !out.contains("final turn only"),
            "{name}: worker template must not include the chat-only final-turn clause; output: {out}",
        );
    }
    Ok(())
}

/// Smoke check: the rendered run prompt contains every instruction section,
/// header, and substituted value the loop.md template promises for shared
/// inputs.
#[test]
fn run_renders_expected_sections_for_shared_inputs() -> Result<()> {
    let ctx = LoopContext {
        pinned_context: "PIN".into(),
        label: SpecLabel::new("demo"),
        spec_path: "specs/demo.md".into(),
        companion_paths: vec!["lib/demo/".into()],
        molecule_id: Some(MoleculeId::new("lm-mol")),
        issue_id: Some(BeadId::new("lm-mol.1")?),
        title: Some("the title".into()),
        description: Some("the description".into()),
        previous_failure: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.loom/scratch/lm-mol.1/scratch.md".into(),
        style_rules: "docs/style-rules.md".into(),
    };
    let out = ctx.render()?;

    for shared in [
        "## Context Pinning",
        "## Current Feature",
        "## Companions",
        "## Issue Details",
        "Issue: lm-mol.1",
        "the title",
        "the description",
        "`bd ready`",
        "## Spec Verifications",
        "## Quality Gates",
        "## Land the Plane",
        "## Progress Markers",
        "## Self-Report Markers",
    ] {
        assert!(
            out.contains(shared),
            "loom loop missing shared section: {shared}"
        );
    }
    Ok(())
}

/// Returns true when `needle` falls inside any `<agent-output>...</agent-output>`
/// span in `haystack`. Used to assert that each agent-supplied field is
/// delimited by the markers, not merely that the markers appear somewhere in
/// the rendered prompt.
fn contained_within_agent_output(haystack: &str, needle: &str) -> bool {
    const OPEN: &str = "<agent-output>";
    const CLOSE: &str = "</agent-output>";
    let mut cursor = 0;
    while let Some(open_rel) = haystack[cursor..].find(OPEN) {
        let span_start = cursor + open_rel + OPEN.len();
        let Some(close_rel) = haystack[span_start..].find(CLOSE) else {
            return false;
        };
        let span_end = span_start + close_rel;
        if haystack[span_start..span_end].contains(needle) {
            return true;
        }
        cursor = span_end + CLOSE.len();
    }
    false
}

/// Pins the per-field agent-output wrapping: each of the four agent-supplied
/// fields (`title`, `description`, `previous_failure`, `existing_tasks`) is
/// rendered inside an `<agent-output>` span, not merely in the same prompt.
#[test]
fn agent_output_markers_wrap_each_agent_supplied_field() -> Result<()> {
    let run = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        molecule_id: Some(MoleculeId::new("lm-3hhwq")),
        issue_id: Some(BeadId::new("lm-3hhwq.10")?),
        title: Some("AGENTOUT_TITLE_TOKEN".into()),
        description: Some("AGENTOUT_DESC_TOKEN".into()),
        previous_failure: Some(PreviousFailure::from_agent_error("AGENTOUT_FAILURE_TOKEN")),
        review_notes: None,
        attempt: 1,
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    }
    .render()?;
    for token in [
        "AGENTOUT_TITLE_TOKEN",
        "AGENTOUT_DESC_TOKEN",
        "AGENTOUT_FAILURE_TOKEN",
    ] {
        assert!(
            contained_within_agent_output(&run, token),
            "loop.md: {token} not enclosed in <agent-output>: {run}",
        );
    }

    let todo_update = TodoUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        spec_diff: Some("=== specs/harness.md ===\n+ change".into()),
        existing_tasks: Some("AGENTOUT_TASKS_TOKEN".into()),
        molecule_id: Some(MoleculeId::new("lm-3hhwq")),
        implementation_notes: vec![],
        criterion_status: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    }
    .render()?;
    assert!(
        contained_within_agent_output(&todo_update, "AGENTOUT_TASKS_TOKEN"),
        "todo_update.md: existing_tasks not enclosed in <agent-output>: {todo_update}",
    );
    Ok(())
}

/// Pins template-render determinism: every context renders byte-identically
/// twice in a row from identical inputs. Catches non-determinism (HashMap
/// ordering, time, env reads) that snapshots would only flag on the next
/// snapshot review.
#[test]
fn template_renders_are_byte_stable_across_runs() -> Result<()> {
    fn assert_stable<T: Template>(name: &str, ctx: T) -> Result<()> {
        let first = ctx.render()?;
        let second = ctx.render()?;
        assert_eq!(
            first, second,
            "{name}: render output differs between two consecutive renders with identical inputs",
        );
        Ok(())
    }

    assert_stable(
        "plan_new",
        PlanNewContext {
            pinned_context: PINNED_CONTEXT_BODY.to_string(),
            label: SpecLabel::new("harness"),
            spec_path: "specs/harness.md".to_string(),
            scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
            spec_conventions: "docs/spec-conventions.md".to_string(),
        },
    )?;
    assert_stable(
        "plan_update",
        PlanUpdateContext {
            pinned_context: PINNED_CONTEXT_BODY.to_string(),
            label: SpecLabel::new("harness"),
            spec_path: "specs/harness.md".to_string(),
            companion_paths: vec!["lib/sandbox/".into()],
            implementation_notes: vec!["pin: stability check".into()],
            scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
            spec_conventions: "docs/spec-conventions.md".to_string(),
        },
    )?;
    assert_stable(
        "todo_new",
        TodoNewContext {
            pinned_context: PINNED_CONTEXT_BODY.to_string(),
            label: SpecLabel::new("harness"),
            spec_path: "specs/harness.md".to_string(),
            companion_paths: vec!["lib/sandbox/".into()],
            implementation_notes: vec![],
            criterion_status: vec![],
            scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        },
    )?;
    assert_stable(
        "todo_update",
        TodoUpdateContext {
            pinned_context: PINNED_CONTEXT_BODY.to_string(),
            label: SpecLabel::new("harness"),
            spec_path: "specs/harness.md".to_string(),
            companion_paths: vec![],
            spec_diff: Some("=== specs/harness.md ===\n+ stability".into()),
            existing_tasks: Some("- lm-3hhwq.1: scaffold".into()),
            molecule_id: Some(MoleculeId::new("lm-3hhwq")),
            implementation_notes: vec![],
            criterion_status: vec![],
            scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        },
    )?;
    assert_stable(
        "run",
        LoopContext {
            pinned_context: PINNED_CONTEXT_BODY.to_string(),
            label: SpecLabel::new("harness"),
            spec_path: "specs/harness.md".to_string(),
            companion_paths: vec!["lib/sandbox/".into()],
            molecule_id: Some(MoleculeId::new("lm-3hhwq")),
            issue_id: Some(BeadId::new("lm-3hhwq.10")?),
            title: Some("port templates".into()),
            description: Some("Port templates to Askama.".into()),
            previous_failure: Some(PreviousFailure::from_agent_error(
                "error: cargo test failed",
            )),
            review_notes: None,
            attempt: 1,
            scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
            style_rules: "docs/style-rules.md".to_string(),
        },
    )?;
    assert_stable(
        "review",
        ReviewContext {
            pinned_context: PINNED_CONTEXT_BODY.to_string(),
            label: SpecLabel::new("harness"),
            spec_path: "specs/harness.md".to_string(),
            companion_paths: vec!["lib/sandbox/".into()],
            beads_summary: Some("- lm-3hhwq.10: closed".into()),
            base_commit: Some("abc1234".into()),
            molecule_id: Some(MoleculeId::new("lm-3hhwq")),
            test_sources: vec![ReviewSource {
                path: "tests/run-tests.sh".into(),
                body: "test_review_inputs() { :; }\n".into(),
            }],
            judge_rubrics: vec![ReviewSource {
                path: "tests/judges/loom.sh".into(),
                body: "judge_live_path_coverage() { :; }\n".into(),
            }],
            scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
            style_rules: "docs/style-rules.md".to_string(),
            lane: ReviewLane::Both,
            default_profile: ProfileName::new("rust"),
        },
    )?;
    assert_stable(
        "msg",
        MsgContext {
            pinned_context: PINNED_CONTEXT_BODY.to_string(),
            companion_paths: vec!["lib/sandbox/".into()],
            clarify_beads: vec![ClarifyBead {
                id: BeadId::new("lm-clar.1")?,
                spec_label: SpecLabel::new("harness"),
                title: "State storage choice".into(),
                options_summary: Some("State JSON vs. dedicated table".into()),
                options: vec![
                    ClarifyOption {
                        n: 1,
                        title: Some("Keep state in JSON".into()),
                        body: Some("Add a companions array.".into()),
                    },
                    ClarifyOption {
                        n: 2,
                        title: Some("Migrate to a table".into()),
                        body: Some("Use a SQLite table.".into()),
                    },
                ],
                kind: BeadKind::Clarify,
            }],
            scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        },
    )?;
    Ok(())
}

/// Representative `criterion_status` rows used by both
/// `todo_new` and `todo_update` fixtures below. Covers every
/// [`CriterionResult`] variant so the assertions can pin the rendered
/// shape (annotation + last result + recency signal) for each.
fn representative_criterion_status() -> Vec<CriterionStatus> {
    vec![
        CriterionStatus {
            criterion_anchor: "engine-fresh-pass".into(),
            annotation: "[check](cargo build -p loom-templates)".into(),
            last_result: CriterionResult::Pass,
            last_timestamp_ms: Some(1_716_300_000_000),
            last_commit: Some("abc1234".into()),
            commits_since: Some(0),
        },
        CriterionStatus {
            criterion_anchor: "engine-stale-pass".into(),
            annotation: "[check](cargo run -p loom-walk -- template_pinning_matrix)".into(),
            last_result: CriterionResult::Pass,
            last_timestamp_ms: Some(1_716_000_000_000),
            last_commit: Some("9abcdef".into()),
            commits_since: Some(42),
        },
        CriterionStatus {
            criterion_anchor: "engine-fail".into(),
            annotation: "[test](template_renders_are_byte_stable_across_runs)".into(),
            last_result: CriterionResult::Fail,
            last_timestamp_ms: Some(1_716_200_000_000),
            last_commit: Some("def5678".into()),
            commits_since: Some(7),
        },
        CriterionStatus {
            criterion_anchor: "engine-skipped".into(),
            annotation: "[check](cargo nextest run -p loom-templates --test render)".into(),
            last_result: CriterionResult::Skipped,
            last_timestamp_ms: Some(1_716_250_000_000),
            last_commit: Some("11aa22bb".into()),
            commits_since: Some(3),
        },
        CriterionStatus {
            criterion_anchor: "criterion-status-never-run".into(),
            annotation: "[test](todo_templates_render_criterion_status_rows)".into(),
            last_result: CriterionResult::NoResult,
            last_timestamp_ms: None,
            last_commit: None,
            commits_since: None,
        },
    ]
}

/// `todo_new` and `todo_update` must surface every `criterion_status` row's
/// annotation, last result, and a recency signal so the decomposition agent
/// can distinguish fresh-pass criteria from stale or never-run ones. Pins
/// the spec's *Criterion-Status Surface* contract per
/// `specs/templates.md` § Success Criteria.
#[test]
fn todo_templates_render_criterion_status_rows() -> Result<()> {
    let rows = representative_criterion_status();

    let todo_new_out = TodoNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        implementation_notes: vec![],
        criterion_status: rows.clone(),
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    }
    .render()?;
    let todo_update_out = TodoUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        spec_diff: Some("=== specs/harness.md ===\n+ added requirement".into()),
        existing_tasks: None,
        molecule_id: Some(MoleculeId::new("lm-mol")),
        implementation_notes: vec![],
        criterion_status: rows.clone(),
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    }
    .render()?;

    for (name, out) in [
        ("todo_new", &todo_new_out),
        ("todo_update", &todo_update_out),
    ] {
        for row in &rows {
            assert!(
                out.contains(&row.annotation),
                "{name}: annotation `{}` missing from render: {out}",
                row.annotation,
            );
            assert!(
                out.contains(row.last_result.as_str()),
                "{name}: last_result `{}` missing from render: {out}",
                row.last_result.as_str(),
            );
            match (&row.last_commit, &row.commits_since, &row.last_timestamp_ms) {
                (Some(commit), Some(n), Some(ts)) => {
                    let recency = format!(
                        "last commit `{commit}` · commits since {n} · last timestamp {ts}",
                    );
                    assert!(
                        out.contains(&recency),
                        "{name}: recency signal `{recency}` missing for `{}`: {out}",
                        row.criterion_anchor,
                    );
                }
                (None, None, None) => {
                    let recency = "last commit — · commits since — · last timestamp —";
                    assert!(
                        out.contains(recency),
                        "{name}: never-run recency placeholder missing for `{}`: {out}",
                        row.criterion_anchor,
                    );
                }
                other => panic!(
                    "{name}: fixture row `{}` mixes Some/None recency fields: {other:?}",
                    row.criterion_anchor,
                ),
            }
        }

        let fresh_line = "**engine-fresh-pass** · annotation `[check](cargo build -p loom-templates)` · result `Pass` · last commit `abc1234` · commits since 0 · last timestamp 1716300000000";
        let stale_line = "**engine-stale-pass** · annotation `[check](cargo run -p loom-walk -- template_pinning_matrix)` · result `Pass` · last commit `9abcdef` · commits since 42 · last timestamp 1716000000000";
        let never_line = "**criterion-status-never-run** · annotation `[test](todo_templates_render_criterion_status_rows)` · result `NoResult` · last commit — · commits since — · last timestamp —";
        assert!(
            out.contains(fresh_line),
            "{name}: fresh-pass row layout drifted: {out}",
        );
        assert!(
            out.contains(stale_line),
            "{name}: stale-pass row (commits_since=42) layout drifted: {out}",
        );
        assert!(
            out.contains(never_line),
            "{name}: never-run row layout drifted: {out}",
        );
        assert_ne!(
            fresh_line, stale_line,
            "fresh-pass and stale-pass rows must be visually distinct",
        );
        assert_ne!(
            fresh_line, never_line,
            "fresh-pass and never-run rows must be visually distinct",
        );
    }
    Ok(())
}

/// `todo_new` and `todo_update` must each render the
/// `partial/decomposition_discipline.md` audit clause so the decomposition
/// agent is committed to confirming missing work by inspection before
/// authoring any non-audit bead. Pins the spec's grep tripwires
/// (`evidence-confirmed`, `audit before`, `LOOM_CLARIFY`) per
/// `specs/templates.md` § Decomposition discipline.
#[test]
fn todo_templates_render_pre_decomposition_audit_clause() -> Result<()> {
    let todo_new_out = TodoNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        implementation_notes: vec![],
        criterion_status: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    }
    .render()?;
    let todo_update_out = TodoUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        spec_diff: None,
        existing_tasks: None,
        molecule_id: Some(MoleculeId::new("lm-mol")),
        implementation_notes: vec![],
        criterion_status: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    }
    .render()?;

    for (name, out) in [
        ("todo_new", &todo_new_out),
        ("todo_update", &todo_update_out),
    ] {
        assert!(
            out.contains("Decomposition Discipline"),
            "{name}: decomposition discipline section missing: {out}",
        );
        assert!(
            out.contains("evidence-confirmed"),
            "{name}: missing `evidence-confirmed` grep tripwire: {out}",
        );
        assert!(
            out.contains("Audit before fan-out"),
            "{name}: missing `Audit before fan-out` clause heading: {out}",
        );
        assert!(
            out.contains("LOOM_CLARIFY"),
            "{name}: missing `LOOM_CLARIFY` fallback citation: {out}",
        );
        assert!(
            out.contains("molecule epic"),
            "{name}: missing `molecule epic` fallback target: {out}",
        );
    }
    Ok(())
}
