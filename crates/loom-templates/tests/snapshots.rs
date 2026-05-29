//! `insta` snapshot tests for every Askama template × representative input set.
//!
//! The rendered template body is the contract we ship to the agent — layout
//! drift slips silently past substring assertions. Snapshots surface the diff
//! in PR review. Updates require an explicit "snapshot updated because: ..."
//! line in the PR description (see `docs/style-rules.md`).
//!
//! One snapshot per typed context struct, named after the test function via
//! `insta::assert_snapshot!`'s default file naming.

use askama::Template;
use loom_events::identifier::{BeadId, MoleculeId, ProfileName, SpecLabel};
use loom_templates::criterion_status::{CriterionResult, CriterionStatus};
use loom_templates::finding::{ConcernToken, Finding, FindingTarget};
use loom_templates::msg::{BeadKind, ClarifyBead, ClarifyOption, MsgContext};
use loom_templates::plan::{PlanNewContext, PlanUpdateContext};
use loom_templates::review::{ReviewContext, ReviewLane, ReviewSource};
use loom_templates::run::{DriverNoticeCause, LoopContext, PreviousFailure, VerifierFailure};
use loom_templates::todo::{TodoNewContext, TodoUpdateContext};

const PINNED_CONTEXT_BODY: &str =
    "# Project Overview\n\nLoom orchestrates the spec-to-implementation workflow.";
const SCRATCHPAD_PATH_BODY: &str = "/workspace/.wrapix/loom/scratch/harness/scratch.md";

#[test]
fn plan_new_snapshot() {
    let ctx = PlanNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

#[test]
fn plan_update_snapshot() {
    let ctx = PlanUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![
            "lib/sandbox/".into(),
            "crates/loom-templates/templates/".into(),
        ],
        implementation_notes: vec![
            "Read `specs/harness.md` end-to-end before touching the parser".into(),
            "Retry policy is described in `## Recovery & Retry`".into(),
        ],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

#[test]
fn todo_new_snapshot() {
    let ctx = TodoNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        implementation_notes: vec![],
        criterion_status: vec![
            CriterionStatus {
                criterion_anchor: "engine-001".into(),
                annotation: "[check](cargo build -p loom-templates)".into(),
                last_result: CriterionResult::Pass,
                last_timestamp_ms: Some(1_716_300_000_000),
                last_commit: Some("abc1234".into()),
                commits_since: Some(0),
            },
            CriterionStatus {
                criterion_anchor: "engine-002".into(),
                annotation: "[test](template_renders_are_byte_stable_across_runs)".into(),
                last_result: CriterionResult::Fail,
                last_timestamp_ms: Some(1_716_200_000_000),
                last_commit: Some("def5678".into()),
                commits_since: Some(7),
            },
            CriterionStatus {
                criterion_anchor: "criterion-status-001".into(),
                annotation: "[test](todo_templates_render_criterion_status_rows)".into(),
                last_result: CriterionResult::NoResult,
                last_timestamp_ms: None,
                last_commit: None,
                commits_since: None,
            },
            CriterionStatus {
                criterion_anchor: "engine-003".into(),
                annotation: "[check](cargo run -p loom-walk -- template_pinning_matrix)".into(),
                last_result: CriterionResult::Skipped,
                last_timestamp_ms: Some(1_716_250_000_000),
                last_commit: Some("9abcdef".into()),
                commits_since: Some(3),
            },
        ],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

#[test]
fn todo_update_snapshot() {
    let ctx = TodoUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec![],
        spec_diff: Some("=== specs/harness.md ===\n+ new requirement".into()),
        existing_tasks: Some("- lm-3hhwq.1: scaffold workspace".into()),
        molecule_id: Some(MoleculeId::new("lm-3hhwq")),
        implementation_notes: vec![],
        criterion_status: vec![
            CriterionStatus {
                criterion_anchor: "engine-001".into(),
                annotation: "[check](cargo build -p loom-templates)".into(),
                last_result: CriterionResult::Pass,
                last_timestamp_ms: Some(1_716_300_000_000),
                last_commit: Some("abc1234".into()),
                commits_since: Some(0),
            },
            CriterionStatus {
                criterion_anchor: "engine-002".into(),
                annotation: "[test](template_renders_are_byte_stable_across_runs)".into(),
                last_result: CriterionResult::Fail,
                last_timestamp_ms: Some(1_716_200_000_000),
                last_commit: Some("def5678".into()),
                commits_since: Some(7),
            },
            CriterionStatus {
                criterion_anchor: "criterion-status-001".into(),
                annotation: "[test](todo_templates_render_criterion_status_rows)".into(),
                last_result: CriterionResult::NoResult,
                last_timestamp_ms: None,
                last_commit: None,
                commits_since: None,
            },
            CriterionStatus {
                criterion_anchor: "engine-003".into(),
                annotation: "[check](cargo run -p loom-walk -- template_pinning_matrix)".into(),
                last_result: CriterionResult::Skipped,
                last_timestamp_ms: Some(1_716_250_000_000),
                last_commit: Some("9abcdef".into()),
                commits_since: Some(3),
            },
        ],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

#[test]
fn run_snapshot() {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        molecule_id: Some(MoleculeId::new("lm-3hhwq")),
        issue_id: Some(BeadId::new("lm-3hhwq.10").unwrap()),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::from_agent_error(
            "error: cargo test failed",
        )),
        review_notes: None,
        attempt: 1,
        scratchpad_path: "/workspace/.wrapix/loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

/// Fresh dispatch: `attempt = 0` with `previous_failure = None` must render
/// the false branch of the first-instruction reframe — no blockquote, no
/// retry line — so a clean run prompt is byte-stable against the false
/// branch.
#[test]
fn run_snapshot_no_failure() {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        molecule_id: Some(MoleculeId::new("lm-3hhwq")),
        issue_id: Some(BeadId::new("lm-3hhwq.10").unwrap()),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.wrapix/loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

/// Retry with the `DriverNotice` variant: pins the reframe + framing prefix
/// for procedural failures like `incomplete-signaling`.
#[test]
fn run_snapshot_driver_notice() {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        molecule_id: Some(MoleculeId::new("lm-3hhwq")),
        issue_id: Some(BeadId::new("lm-3hhwq.10").unwrap()),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::IncompleteSignaling,
            detail: "Marker `LOOM_COMPLETE` emitted but bead `lm-3hhwq.10` was not bd-closed."
                .into(),
        }),
        review_notes: None,
        attempt: 1,
        scratchpad_path: "/workspace/.wrapix/loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

/// Retry with the `VerifyFailures` variant: pins the reframe alongside the
/// collective verifier-failures framing.
#[test]
fn run_snapshot_verify_failures() {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        molecule_id: Some(MoleculeId::new("lm-3hhwq")),
        issue_id: Some(BeadId::new("lm-3hhwq.10").unwrap()),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::VerifyFailures(vec![VerifierFailure::new(
            "tests/run-tests.sh",
            1,
            "assertion failed: expected reframe in prompt\n",
        )])),
        review_notes: None,
        attempt: 1,
        scratchpad_path: "/workspace/.wrapix/loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

/// Retry with the `ReviewConcern` variant: pins the reframe alongside the
/// review concern framing and its token prefix.
#[test]
fn run_snapshot_review_concern() {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        molecule_id: Some(MoleculeId::new("lm-3hhwq")),
        issue_id: Some(BeadId::new("lm-3hhwq.10").unwrap()),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::ReviewConcern {
            summary: "test mocks the agent backend instead of running the live driver".into(),
            findings: vec![Finding {
                token: ConcernToken::VerifierBypass,
                bonds: vec![SpecLabel::new("harness")],
                target: FindingTarget::Annotation {
                    target_string: "cargo test --lib parse_walks_all_md_files".into(),
                },
                evidence: "test mocks the agent backend instead of running the live driver".into(),
            }],
        }),
        review_notes: None,
        attempt: 1,
        scratchpad_path: "/workspace/.wrapix/loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

/// Retry with the `BuildFailure` variant: pins the reframe alongside the
/// compiler/build framing. Companion to the legacy `run_snapshot` which uses
/// `PreviousFailure::from_agent_error` (`stage = "agent"`); this variant pins
/// a real stage name.
#[test]
fn run_snapshot_build_failure() {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        molecule_id: Some(MoleculeId::new("lm-3hhwq")),
        issue_id: Some(BeadId::new("lm-3hhwq.10").unwrap()),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::BuildFailure {
            stage: "cargo check".into(),
            output: "error[E0382]: borrow of moved value: `ctx`".into(),
        }),
        review_notes: None,
        attempt: 1,
        scratchpad_path: "/workspace/.wrapix/loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

#[test]
fn review_snapshot() {
    let ctx = ReviewContext {
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
        tree_scope_epics: Vec::new(),
        default_profile: ProfileName::new("rust"),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

#[test]
fn msg_snapshot() {
    let ctx = MsgContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        clarify_beads: vec![ClarifyBead {
            id: BeadId::new("lm-clar.1").unwrap(),
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
    insta::assert_snapshot!(ctx.render().unwrap());
}
