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
use loom_protocol::todo::{GitSha, TodoFingerprint};
use loom_templates::SkillIndexMarkdown;
use loom_templates::criterion_status::{
    AnnotationTarget, AnnotationTier, CriterionAnnotation, CriterionId, CriterionResult,
    CriterionStatus, EvidenceState,
};
use loom_templates::finding::{ConcernToken, Finding, FindingTarget};
use loom_templates::inbox::{ClarifyOption, InboxContext, InboxItem, ItemKind, TuneItem};
use loom_templates::plan::PlanContext;
use loom_templates::review::{ReviewContext, ReviewLane, ReviewSource};
use loom_templates::run::{
    DriverNoticeCause, LoopContext, PREVIOUS_FAILURE_MAX_LEN, PreviousFailure, RecoveryStash,
    VerifierFailure, WorkspaceAlignment, WorkspaceRecovery,
};
use loom_templates::todo::{
    SpecEpicContext, SpecImplementationNotes, TodoChangedSpec, TodoContext,
};

const PINNED_CONTEXT_BODY: &str =
    "# Project Overview\n\nLoom orchestrates the spec-to-implementation workflow.";
const SCRATCHPAD_PATH_BODY: &str = "/workspace/.loom/scratch/harness/scratch.md";
const TEST_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const TEST_SHA_2: &str = "1111111111111111111111111111111111111111";
const TEST_SHA_3: &str = "2222222222222222222222222222222222222222";
const TEST_SHA_4: &str = "3333333333333333333333333333333333333333";
const TEST_SHA_5: &str = "4444444444444444444444444444444444444444";
const TEST_FINGERPRINT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn inbox_item(id: &str, spec: &str, title: &str, kind: ItemKind) -> InboxItem {
    InboxItem {
        index: 1,
        id: id.to_string(),
        bead_id: id.to_string(),
        spec_label: spec.to_string(),
        title: title.to_string(),
        body: String::new(),
        notes: None,
        options_summary: None,
        options: Vec::new(),
        kind,
        tune: None,
    }
}

fn inbox_ctx(items: Vec<InboxItem>) -> InboxContext {
    InboxContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        companion_paths: vec![],
        inbox_items: items,
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        skill_index: SkillIndexMarkdown::empty(),
    }
}

#[expect(
    clippy::expect_used,
    reason = "fixture literals are part of the render contract; parse failure means the test fixture is invalid"
)]
fn git_sha(raw: &str) -> GitSha {
    GitSha::new(raw).expect("valid git sha")
}

fn workspace_recovery(alignment: WorkspaceAlignment) -> WorkspaceRecovery {
    WorkspaceRecovery {
        pre_stash_status: "## loom/lm-demo.1\n M crates/demo/src/lib.rs\n?? notes.txt".into(),
        stash: RecoveryStash {
            selector: "stash@{0}".into(),
            commit: git_sha(TEST_SHA_2),
            message: "loom workspace-recovery lm-demo.1 1716250000".into(),
        },
        integration_tip: git_sha(TEST_SHA_3),
        alignment,
    }
}

#[expect(
    clippy::expect_used,
    reason = "fixture identifiers are parsed once so rendered contexts use production newtypes"
)]
fn todo_context(notes: Vec<String>, criterion_status: Vec<CriterionStatus>) -> TodoContext {
    TodoContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        spec_index: "# Loom Docs\n| Spec | Purpose |".to_string(),
        changed_specs: vec![TodoChangedSpec {
            label: SpecLabel::new("harness"),
            spec_path: "specs/harness.md".to_string(),
            diff: Some("=== specs/harness.md ===\n+ new requirement".to_string()),
        }],
        work_epic: BeadId::new("lm-work").expect("valid bead id"),
        todo_head: git_sha(TEST_SHA),
        todo_fingerprint: TodoFingerprint::new(TEST_FINGERPRINT).expect("valid fingerprint"),
        spec_epics: vec![SpecEpicContext {
            label: SpecLabel::new("harness"),
            epic_id: Some(MoleculeId::new("lm-spec")),
            todo_cursor: Some(TEST_SHA.to_string()),
        }],
        companion_paths: vec!["lib/sandbox/".into()],
        implementation_notes: vec![SpecImplementationNotes {
            label: SpecLabel::new("harness"),
            notes,
        }],
        criterion_status,
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        skill_index: SkillIndexMarkdown::empty(),
    }
}

fn plan_ctx() -> PlanContext {
    PlanContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        anchor_labels: vec![SpecLabel::new("harness")],
        spec_index: "# Loom Docs\n| Spec | Purpose |\n| [harness](../specs/harness.md) | Harness |"
            .to_string(),
        companion_paths: vec![
            "lib/sandbox/".into(),
            "crates/loom-templates/templates/".into(),
        ],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
    }
}

#[test]
fn plan_renders_partials_index_anchors_and_companions() -> Result<()> {
    let out = plan_ctx().render()?;

    assert!(out.contains("# Specification Interview"));
    assert!(out.contains(PINNED_CONTEXT_BODY));
    assert!(out.contains("# Loom Docs"));
    assert!(out.contains("`harness`"));
    assert!(out.contains("- lib/sandbox/"));
    assert!(out.contains("Anchor Context & Sibling-Spec Editing"));
    assert!(out.contains("LOOM_COMPLETE"));
    assert!(out.contains("Interview Modes"));
    Ok(())
}

#[test]
fn skill_index_partial_renders_precomputed_markdown() -> Result<()> {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("agent"),
        spec_path: "specs/agent.md".to_string(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: Some(BeadId::new("lm-agent.1")?),
        title: Some("wire skills".into()),
        description: Some("Render skills.".into()),
        previous_failure: None,
        workspace_recovery: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::new(
            "- `rust-review` — Use for Rust reviews. (path: `/workspace/skills/rust-review.md`)",
        ),
    };
    let out = ctx.render()?;

    assert!(out.contains("## Skills"));
    assert!(out.contains("`rust-review`"));
    assert!(out.contains("/workspace/skills/rust-review.md"));
    Ok(())
}

#[test]
fn plan_template_prohibits_bd_writes() -> Result<()> {
    let out = plan_ctx().render()?;

    assert!(out.contains("Do NOT create beads, epics, bd state"));
    assert!(out.contains("In `loom plan`, do not write bd"));
    assert!(out.contains("loom note set"));
    Ok(())
}

#[test]
fn plan_template_requires_index_rows_for_new_specs() -> Result<()> {
    let out = plan_ctx().render()?;

    assert!(out.contains("verify every new `specs/<label>.md` file has exactly one"));
    assert!(out.contains("unindexed specs are invisible to `loom todo`"));
    Ok(())
}

#[test]
fn plan_template_renders_three_plan_stage_checks() -> Result<()> {
    let out = plan_ctx().render()?;
    assert!(out.contains("Plan-Stage Rubric"));
    assert!(out.contains("Completeness check"));
    assert!(out.contains("Internal coherence check"));
    assert!(out.contains("Invariant-clash scan") || out.contains("Invariant-Clash Awareness"));
    assert!(out.contains("three-paths"));
    Ok(())
}

#[test]
fn plan_defers_spec_format_to_conventions_doc() -> Result<()> {
    let out = plan_ctx().render()?;

    assert!(
        out.contains("docs/spec-conventions.md"),
        "plan must defer to docs/spec-conventions.md"
    );
    assert!(
        !out.contains("[ ] CLI") && !out.contains("[ ] Error"),
        "plan must not teach `[ ]` checkbox examples"
    );
    assert!(
        !out.contains("Affected files/modules") && !out.contains("Affected Files"),
        "plan must not instruct an Affected Files section"
    );
    Ok(())
}

#[test]
fn todo_renders_driver_created_work_epic_and_changed_roster() -> Result<()> {
    let out = todo_context(vec![], vec![]).render()?;

    assert!(out.contains("# Todo Decomposition"));
    assert!(out.contains("driver-injected changed-spec roster"));
    assert!(out.contains("Work epic**: lm-work"));
    assert!(out.contains("### harness"));
    assert!(out.contains("specs/harness.md"));
    assert!(out.contains("--parent=\"lm-work\""));
    assert!(!out.contains("## Implementation Notes"));
    Ok(())
}

#[test]
fn todo_renders_implementation_notes_when_present() -> Result<()> {
    let ctx = todo_context(
        vec![
            "Hidden constraint: touch lib/sandbox/linux/default.nix".into(),
            "Design trade-off: prefer single FK over join table".into(),
        ],
        vec![],
    );
    let out = ctx.render()?;
    assert!(out.contains("## Implementation Notes"));
    assert!(out.contains("Hidden constraint: touch lib/sandbox/linux/default.nix"));
    assert!(out.contains("Design trade-off: prefer single FK over join table"));
    assert_eq!(out.matches("<implementation-note>").count(), 2);
    assert_eq!(out.matches("</implementation-note>").count(), 2);
    Ok(())
}

#[test]
fn todo_template_rejects_generic_success_markers() -> Result<()> {
    let out = todo_context(vec![], vec![]).render()?;

    assert!(out.contains("LOOM_TODO: <json>"));
    assert!(out.contains("wrong-phase success markers"));
    assert!(out.contains("`LOOM_COMPLETE`") && out.contains("`LOOM_NOOP`"));
    assert!(out.contains("final assistant message"));
    assert!(out.contains("driver parses assistant text only"));
    assert!(out.contains("`bash`") && out.contains("`python`"));
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
        workspace_recovery: None,
        review_notes: None,
        attempt: 1,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
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
        workspace_recovery: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
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
        workspace_recovery: None,
        review_notes: None,
        attempt: 2,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
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
        workspace_recovery: None,
        review_notes: None,
        attempt: 1,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
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
        workspace_recovery: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
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
        workspace_recovery: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
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
        workspace_recovery: None,
        review_notes: Some("[verifier-bypass] test mocks the agent backend".into()),
        attempt: 1,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
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
fn loop_context_renders_workspace_recovery_without_retry_attempt() -> Result<()> {
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
        workspace_recovery: Some(workspace_recovery(WorkspaceAlignment::Clean)),
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
    };
    let out = ctx.render()?;

    assert!(
        out.contains("## Workspace Recovery"),
        "recovery block missing: {out}"
    );
    assert!(
        out.contains(&format!("git stash show --stat {TEST_SHA_2}")),
        "stable stat command missing: {out}",
    );
    assert!(
        out.contains(&format!("git stash show -p {TEST_SHA_2}")),
        "stable patch command missing: {out}",
    );
    assert!(
        out.contains("apply the stash, cherry-pick relevant hunks, leave it unapplied"),
        "intentional recovery choices missing: {out}",
    );
    assert!(
        !out.contains("Retry attempt"),
        "workspace recovery must not imply a retry attempt: {out}",
    );
    assert!(
        !out.contains("Re-read the previous failure block above"),
        "workspace recovery must not render the retry reframe: {out}",
    );
    Ok(())
}

#[test]
fn loop_template_renders_previous_failure_before_workspace_recovery() -> Result<()> {
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
        workspace_recovery: Some(workspace_recovery(WorkspaceAlignment::Rebased {
            previous_head: git_sha(TEST_SHA_4),
            current_head: git_sha(TEST_SHA_5),
        })),
        review_notes: None,
        attempt: 1,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
    };
    let out = ctx.render()?;

    let previous_failure_pos = out
        .find("Previous attempt:")
        .expect("previous failure block present");
    let workspace_recovery_pos = out
        .find("## Workspace Recovery")
        .expect("workspace recovery block present");
    assert!(
        previous_failure_pos < workspace_recovery_pos,
        "previous failure must render before workspace recovery: {out}",
    );
    assert!(
        out.contains("Rebased (previous head"),
        "rebased alignment detail missing: {out}",
    );
    Ok(())
}

#[test]
fn workspace_recovery_summary_prompt_is_non_authoritative() -> Result<()> {
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
        workspace_recovery: Some(workspace_recovery(WorkspaceAlignment::Conflict {
            files: vec!["crates/demo/src/lib.rs".into(), "Cargo.toml".into()],
        })),
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
    };
    let out = ctx.render()?;

    assert!(
        out.contains("agent-owned merge-conflict recovery"),
        "conflict recovery framing missing: {out}",
    );
    assert!(
        out.contains("crates/demo/src/lib.rs"),
        "conflict file missing: {out}"
    );
    assert!(
        out.contains("LOOM_CLARIFY"),
        "clarify fallback missing: {out}"
    );
    assert!(
        out.contains("final prose before the terminal marker"),
        "final-summary instruction missing: {out}",
    );
    assert!(
        out.contains("the driver does not parse it"),
        "driver prose non-authority clause missing: {out}",
    );
    assert!(
        out.contains("does not reject `LOOM_COMPLETE` solely because the stash still exists"),
        "stash-still-exists non-rejection clause missing: {out}",
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
        skill_index: SkillIndexMarkdown::empty(),
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
        skill_index: SkillIndexMarkdown::empty(),
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
        skill_index: SkillIndexMarkdown::empty(),
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
        skill_index: SkillIndexMarkdown::empty(),
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
        skill_index: SkillIndexMarkdown::empty(),
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
        skill_index: SkillIndexMarkdown::empty(),
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
        out.contains("`route=\"clarify\"` finding line"),
        "persistence text must name any clarify-route finding, not a token-specific path: {out}",
    );
    assert!(
        !out.contains("only** through the `evidence` field of an\n`invariant-clash`"),
        "persistence text must not say only invariant-clash findings carry Options blocks: {out}",
    );
    assert!(
        out.contains("Do not try to persist\nreview options yourself with `bd` commands"),
        "review prompt must forbid reviewer-side bd persistence for options: {out}",
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
        skill_index: SkillIndexMarkdown::empty(),
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
fn review_self_report_markers_do_not_authorize_bd_writes() -> Result<()> {
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
        skill_index: SkillIndexMarkdown::empty(),
    };
    let out = ctx.render()?;

    assert!(
        out.contains("Review Self-Report Markers"),
        "review prompt must render the review-specific self-report partial: {out}",
    );
    assert!(
        out.contains("Review is inspection-only") && out.contains("Do not mutate `bd` state"),
        "review self-report guidance must forbid bd mutation: {out}",
    );
    for forbidden in [
        "Persist the question and the canonical options block to the target",
        "bead notes (`bd update <id> --notes",
        "Cross-session follow-up work → a new bead (`bd create",
        "After persisting, the gate applies `loom:clarify` to the target",
        "`bd update --description` on the bead under dispatch",
    ] {
        assert!(
            !out.contains(forbidden),
            "review prompt must not render direct bd-backed self-report guidance `{forbidden}`: {out}",
        );
    }
    assert!(
        out.contains("do **not** use direct `LOOM_CLARIFY`")
            && out.contains("`route=\"clarify\"`")
            && out.contains("`evidence`"),
        "review clarifications must route through finding evidence, not direct LOOM_CLARIFY: {out}",
    );
    assert!(
        out.contains("`LOOM_BLOCKED` instead")
            && out.contains("explaining why options cannot be safely")
            && out.contains("surfaced"),
        "review guidance must name LOOM_BLOCKED for no-options dead ends: {out}",
    );
    Ok(())
}

#[test]
fn inbox_renders_clarify_items_with_options() -> Result<()> {
    let mut item = inbox_item(
        "lm-clar.1",
        "harness",
        "State storage choice",
        ItemKind::Clarify,
    );
    item.options_summary = Some("State JSON vs. dedicated table".into());
    item.options = vec![
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
    ];
    item.body = "Canonical body".into();
    let mut ctx = inbox_ctx(vec![item]);
    ctx.companion_paths = vec!["lib/sandbox/".into()];
    let out = ctx.render()?;

    assert!(out.contains("# Inbox Resolution — Interactive Session"));
    assert!(out.contains("### 1. lm-clar.1 — [clarify] [spec:harness] State storage choice"));
    assert!(out.contains("## Options — State JSON vs. dedicated table"));
    assert!(out.contains("#### Option 1 — Keep state in JSON"));
    assert!(out.contains("Add a companions array."));
    assert!(out.contains("#### Option 2 — Migrate to a table"));
    assert!(out.contains("Use a SQLite table."));
    assert!(out.contains("## Companions"));
    assert!(out.contains("- lib/sandbox/"));
    Ok(())
}

#[test]
fn inbox_renders_blocked_item_with_enumerate_first_framing() -> Result<()> {
    let ctx = inbox_ctx(vec![inbox_item(
        "lm-block.1",
        "harness",
        "Push hook fails inside sandbox",
        ItemKind::Blocked,
    )]);
    let out = ctx.render()?;

    assert!(out.contains("### 1. lm-block.1 — [blocked] [spec:harness]"));
    assert!(out.contains("`loom:blocked`"), "{out}");
    assert!(out.contains("enumerat"), "{out}");
    assert!(!out.contains("#### Option "), "{out}");
    Ok(())
}

#[test]
fn inbox_renders_clarify_item_without_blocked_framing() -> Result<()> {
    let mut item = inbox_item(
        "lm-clar.2",
        "harness",
        "Adopt new API surface",
        ItemKind::Clarify,
    );
    item.options_summary = Some("Pick API shape".into());
    item.options = vec![ClarifyOption {
        n: 1,
        title: Some("Keep existing".into()),
        body: Some("Defer the change.".into()),
    }];
    let out = inbox_ctx(vec![item]).render()?;
    assert!(out.contains("`loom:clarify`"));
    assert!(out.contains("## Options — Pick API shape"));
    assert!(out.contains("#### Option 1 — Keep existing"));
    Ok(())
}

#[test]
fn inbox_template_teaches_agent_bd_write_authority() -> Result<()> {
    let out = inbox_ctx(vec![inbox_item(
        "lm-clar.9",
        "harness",
        "Pick",
        ItemKind::Clarify,
    )])
    .render()?;
    for required in ["bd update", "bd close", "--remove-label"] {
        assert!(
            out.contains(required),
            "inbox.md must name `{required}` for bd-write authority: {out}",
        );
    }
    assert!(out.contains("driver does not reconcile") || out.contains("driver does NOT reconcile"));
    Ok(())
}

#[test]
fn inbox_template_renders_chat_interview_discipline() -> Result<()> {
    let out = inbox_ctx(vec![]).render()?;
    assert!(
        out.contains("AskUserQuestion") || out.contains("option-picker"),
        "inbox.md must surface picker prohibition: {out}",
    );
    assert!(out.contains("MEMORY.md"), "{out}");
    assert!(out.contains("bd update <id> --notes"), "{out}");
    Ok(())
}

#[test]
fn inbox_renders_tune_proposal_artifact_paths() -> Result<()> {
    let mut item = inbox_item("lm-tune.1", "skills", "Tune built-in", ItemKind::Tune);
    item.tune = Some(TuneItem {
        state: "pending".into(),
        proposal_branch: Some("loom/tune/lm-tune.1".into()),
        proposal_head: Some("abc123".into()),
        base_commit: Some("base123".into()),
        envelope_path: "/workspace/.loom/tune/lm-tune.1".into(),
        repo_path: "/workspace/.loom/tune/lm-tune.1/repo".into(),
        manifest_path: "/workspace/.loom/tune/lm-tune.1/manifest.json".into(),
        evidence_path: "/workspace/.loom/tune/lm-tune.1/evidence.md".into(),
    });
    let out = inbox_ctx(vec![item]).render()?;
    assert!(out.contains("Tune state"), "{out}");
    assert!(out.contains("loom/tune/lm-tune.1"), "{out}");
    assert!(out.contains("manifest.json"), "{out}");
    assert!(out.contains("evidence.md"), "{out}");
    Ok(())
}

#[test]
fn inbox_renders_with_no_items() -> Result<()> {
    let out = inbox_ctx(vec![]).render()?;
    assert!(out.contains("# Inbox Resolution — Interactive Session"));
    assert!(!out.contains("### lm-"));
    Ok(())
}

#[test]
fn every_multi_turn_template_includes_chat_marker_partial() -> Result<()> {
    let inbox_out = inbox_ctx(vec![]).render()?;
    let plan_out = plan_ctx().render()?;

    for (name, out) in [("inbox", &inbox_out), ("plan", &plan_out)] {
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

/// One-shot worker templates (`run`, `todo`, `review`) deliberately
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
        workspace_recovery: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".into(),
        skill_index: SkillIndexMarkdown::empty(),
    }
    .render()?;

    let todo_out = todo_context(vec![], vec![]).render()?;

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
        skill_index: SkillIndexMarkdown::empty(),
    }
    .render()?;

    for (name, out) in [
        ("run", &run_out),
        ("todo", &todo_out),
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
        workspace_recovery: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.loom/scratch/lm-mol.1/scratch.md".into(),
        style_rules: "docs/style-rules.md".into(),
        skill_index: SkillIndexMarkdown::empty(),
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
    assert!(
        out.contains("Semantic dead end with no safe options to enumerate?")
            && out.contains("why options cannot be safely enumerated"),
        "loop prompt must reserve LOOM_BLOCKED for no-options semantic dead ends: {out}",
    );
    assert!(
        !out.contains("Need user input? → write the reason on a prior line, then `LOOM_BLOCKED`"),
        "loop prompt must not route generic user input to LOOM_BLOCKED: {out}",
    );
    Ok(())
}

#[test]
fn run_template_uses_injected_self_check_range_not_head_shorthand() -> Result<()> {
    let ctx = LoopContext {
        pinned_context: "PIN".into(),
        label: SpecLabel::new("demo"),
        spec_path: "specs/demo.md".into(),
        companion_paths: vec![],
        molecule_id: Some(MoleculeId::new("lm-mol")),
        issue_id: Some(BeadId::new("lm-mol.1")?),
        title: Some("the title".into()),
        description: Some("the description".into()),
        previous_failure: None,
        workspace_recovery: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.loom/scratch/lm-mol.1/scratch.md".into(),
        style_rules: "docs/style-rules.md".into(),
        skill_index: SkillIndexMarkdown::empty(),
    };
    let out = ctx.render()?;

    assert!(
        out.contains("loom gate verify --diff <bead-base>..HEAD"),
        "loop prompt must name the injected bead-base diff command: {out}",
    );
    assert!(
        out.contains("loom gate verify --diff @{u}..HEAD"),
        "loop prompt must allow upstream shorthand only for the injected base: {out}",
    );
    assert!(
        !out.contains("loom gate verify --diff HEAD"),
        "loop prompt must not use the HEAD shorthand as completion contract: {out}",
    );
    let preflight = out
        .find("Preflight self-check")
        .expect("preflight instruction present");
    let progress_markers = out
        .find("## Progress Markers")
        .expect("progress markers present");
    assert!(
        preflight < progress_markers,
        "self-check instruction must precede final-marker instructions: {out}",
    );
    Ok(())
}

#[test]
fn run_template_requires_self_check_rerun_after_post_check_changes() -> Result<()> {
    let ctx = LoopContext {
        pinned_context: "PIN".into(),
        label: SpecLabel::new("demo"),
        spec_path: "specs/demo.md".into(),
        companion_paths: vec![],
        molecule_id: Some(MoleculeId::new("lm-mol")),
        issue_id: Some(BeadId::new("lm-mol.1")?),
        title: Some("the title".into()),
        description: Some("the description".into()),
        previous_failure: None,
        workspace_recovery: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.loom/scratch/lm-mol.1/scratch.md".into(),
        style_rules: "docs/style-rules.md".into(),
        skill_index: SkillIndexMarkdown::empty(),
    };
    let out = ctx.render()?;

    assert!(
        out.contains("Rerun the self-check after any later commit"),
        "loop prompt must require rerun after later commits: {out}",
    );
    assert!(
        out.contains("formatter or hook tree change"),
        "loop prompt must require rerun after formatter/hook tree changes: {out}",
    );
    assert!(
        out.contains("invalidate the prior run"),
        "loop prompt must cover other invalidating changes: {out}",
    );
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
/// fields (`title`, `description`, `previous_failure`, implementation notes) are
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
        workspace_recovery: None,
        review_notes: None,
        attempt: 1,
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
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

    let todo = todo_context(vec!["AGENTOUT_NOTE_TOKEN".into()], vec![]).render()?;
    assert!(
        contained_within_agent_output(&todo, "AGENTOUT_NOTE_TOKEN"),
        "todo.md: implementation note not enclosed in <agent-output>: {todo}",
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

    assert_stable("plan", plan_ctx())?;
    assert_stable("todo", todo_context(vec![], vec![]))?;
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
            workspace_recovery: None,
            review_notes: None,
            attempt: 1,
            scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
            style_rules: "docs/style-rules.md".to_string(),
            skill_index: SkillIndexMarkdown::empty(),
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
            skill_index: SkillIndexMarkdown::empty(),
        },
    )?;
    let mut stable_inbox_item = inbox_item(
        "lm-clar.1",
        "harness",
        "State storage choice",
        ItemKind::Clarify,
    );
    stable_inbox_item.options_summary = Some("State JSON vs. dedicated table".into());
    stable_inbox_item.options = vec![
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
    ];
    let mut stable_inbox = inbox_ctx(vec![stable_inbox_item]);
    stable_inbox.companion_paths = vec!["lib/sandbox/".into()];
    assert_stable("inbox", stable_inbox)?;
    Ok(())
}

/// Representative `criterion_status` rows used by both todo fixtures.
fn representative_criterion_status() -> Vec<CriterionStatus> {
    vec![
        CriterionStatus {
            spec_label: SpecLabel::new("templates"),
            criterion_id: CriterionId::new("engine-fresh-pass"),
            criterion_text: "All workflow templates compile under Askama.".into(),
            annotation: ann(AnnotationTier::Check, "cargo build -p loom-templates"),
            evidence: EvidenceState::Current {
                result: CriterionResult::Pass,
                last_timestamp_ms: 1_716_300_000_000,
                last_commit: git_sha(TEST_SHA),
                commits_since: 0,
            },
        },
        CriterionStatus {
            spec_label: SpecLabel::new("templates"),
            criterion_id: CriterionId::new("engine-stale-pass"),
            criterion_text: "Every non-pending pinning cell matches the include graph.".into(),
            annotation: ann(
                AnnotationTier::Check,
                "cargo run -p loom-walk -- template_pinning_matrix",
            ),
            evidence: EvidenceState::Current {
                result: CriterionResult::Pass,
                last_timestamp_ms: 1_716_000_000_000,
                last_commit: git_sha(TEST_SHA_2),
                commits_since: 42,
            },
        },
        CriterionStatus {
            spec_label: SpecLabel::new("templates"),
            criterion_id: CriterionId::new("engine-fail"),
            criterion_text: "Rendered output is stable across runs.".into(),
            annotation: ann(
                AnnotationTier::Test,
                "template_renders_are_byte_stable_across_runs",
            ),
            evidence: EvidenceState::Current {
                result: CriterionResult::Fail,
                last_timestamp_ms: 1_716_200_000_000,
                last_commit: git_sha(TEST_SHA_3),
                commits_since: 7,
            },
        },
        CriterionStatus {
            spec_label: SpecLabel::new("templates"),
            criterion_id: CriterionId::new("engine-skipped"),
            criterion_text: "Snapshot tests run under clippy test exemptions.".into(),
            annotation: ann(
                AnnotationTier::Check,
                "cargo nextest run -p loom-templates --test render",
            ),
            evidence: EvidenceState::Current {
                result: CriterionResult::Skipped,
                last_timestamp_ms: 1_716_250_000_000,
                last_commit: git_sha(TEST_SHA_4),
                commits_since: 3,
            },
        },
        CriterionStatus {
            spec_label: SpecLabel::new("templates"),
            criterion_id: CriterionId::new("criterion-status-never-run"),
            criterion_text: "Todo prompts render typed criterion status rows.".into(),
            annotation: ann(
                AnnotationTier::Test,
                "todo_template_renders_typed_criterion_status_rows",
            ),
            evidence: EvidenceState::Missing,
        },
        CriterionStatus {
            spec_label: SpecLabel::new("templates"),
            criterion_id: CriterionId::new("criterion-status-stale-annotation"),
            criterion_text: "Stale annotations are explicit evidence states.".into(),
            annotation: ann(AnnotationTier::Test, "new_target"),
            evidence: EvidenceState::StaleAnnotation {
                cached_annotation: ann(AnnotationTier::Check, "old_target"),
                last_timestamp_ms: 1_716_260_000_000,
                last_commit: git_sha(TEST_SHA_5),
                commits_since: 5,
            },
        },
    ]
}

fn ann(tier: AnnotationTier, target: &str) -> CriterionAnnotation {
    CriterionAnnotation {
        tier,
        target: AnnotationTarget::new(target),
        pending: false,
    }
}

/// `todo` surfaces typed evidence states for every row.
#[test]
fn todo_template_renders_typed_criterion_status_rows() -> Result<()> {
    let rows = representative_criterion_status();
    let out = todo_context(vec![], rows.clone()).render()?;

    for row in &rows {
        assert!(
            out.contains(&row.annotation.to_string()),
            "annotation `{}` missing from render: {out}",
            row.annotation,
        );
        assert!(
            out.contains(row.evidence.as_str()),
            "evidence state `{}` missing from render: {out}",
            row.evidence.as_str(),
        );
    }

    let fresh_line = format!(
        "**templates / engine-fresh-pass** · All workflow templates compile under Askama. · annotation `[check](cargo build -p loom-templates)` · evidence `Current` · result Pass · last commit `{TEST_SHA}` · commits since 0 · last timestamp 1716300000000 · cached annotation `—`"
    );
    let missing_line = "**templates / criterion-status-never-run** · Todo prompts render typed criterion status rows. · annotation `[test](todo_template_renders_typed_criterion_status_rows)` · evidence `Missing` · result — · last commit — · commits since — · last timestamp — · cached annotation `—`";
    let stale_line = format!(
        "**templates / criterion-status-stale-annotation** · Stale annotations are explicit evidence states. · annotation `[test](new_target)` · evidence `StaleAnnotation` · result — · last commit `{TEST_SHA_5}` · commits since 5 · last timestamp 1716260000000 · cached annotation `[check](old_target)`"
    );
    assert!(
        out.contains(&fresh_line),
        "current row layout drifted: {out}"
    );
    assert!(
        out.contains(missing_line),
        "missing row layout drifted: {out}"
    );
    assert!(
        out.contains(&stale_line),
        "stale annotation row layout drifted: {out}"
    );
    Ok(())
}

/// `todo` must render the decomposition audit clause so the decomposition
/// agent is committed to confirming missing work by inspection before
/// authoring any non-audit bead.
#[test]
fn todo_template_renders_pre_decomposition_audit_clause() -> Result<()> {
    let out = todo_context(vec![], vec![]).render()?;

    assert!(out.contains("Decomposition Discipline"));
    assert!(out.contains("evidence-confirmed"));
    assert!(out.contains("Audit before fan-out"));
    assert!(out.contains("LOOM_CLARIFY"));
    assert!(out.contains("driver-created work epic"));
    Ok(())
}

#[test]
fn todo_template_forbids_blanket_full_spec_reads() -> Result<()> {
    let out = todo_context(vec![], vec![]).render()?;

    assert!(
        out.contains("Do **not** perform a blanket full-file read of every changed spec"),
        "todo prompt must explicitly steer away from context-burning full-spec reads: {out}"
    );
    assert!(
        out.contains("read only targeted spec sections"),
        "todo prompt must allow targeted source/spec inspection: {out}"
    );
    assert!(
        !out.contains("Read every changed spec in the injected roster"),
        "todo prompt reintroduced the old blanket-read instruction: {out}"
    );
    Ok(())
}
