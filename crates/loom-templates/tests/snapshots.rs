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
use loom_protocol::todo::{GitSha, TodoFingerprint};
use loom_templates::SkillIndexMarkdown;
use loom_templates::criterion_status::{
    AnnotationTarget, AnnotationTier, CriterionAnnotation, CriterionId, CriterionResult,
    CriterionStatus, EvidenceState,
};
use loom_templates::finding::{ConcernToken, Finding, FindingTarget};
use loom_templates::msg::{BeadKind, ClarifyBead, ClarifyOption, MsgContext};
use loom_templates::plan::PlanContext;
use loom_templates::review::{ReviewContext, ReviewLane, ReviewSource};
use loom_templates::run::{DriverNoticeCause, LoopContext, PreviousFailure, VerifierFailure};
use loom_templates::todo::{
    SpecEpicContext, SpecImplementationNotes, TodoChangedSpec, TodoContext,
};

const PINNED_CONTEXT_BODY: &str =
    "# Project Overview\n\nLoom orchestrates the spec-to-implementation workflow.";
const SCRATCHPAD_PATH_BODY: &str = "/workspace/.loom/scratch/harness/scratch.md";
const TEST_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const TEST_SHA_2: &str = "1111111111111111111111111111111111111111";
const TEST_SHA_3: &str = "2222222222222222222222222222222222222222";
const TEST_FINGERPRINT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[expect(
    clippy::unwrap_used,
    reason = "fixture literals are part of the snapshot contract; parse failure means the test fixture is invalid"
)]
fn git_sha(raw: &str) -> GitSha {
    GitSha::new(raw).unwrap()
}

#[expect(
    clippy::unwrap_used,
    reason = "fixture identifiers are parsed once so snapshots exercise production newtypes"
)]
fn todo_ctx() -> TodoContext {
    TodoContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        spec_index: "# Loom Docs\n| Spec | Purpose |\n| [harness](../specs/harness.md) | Harness |"
            .to_string(),
        changed_specs: vec![TodoChangedSpec {
            label: SpecLabel::new("harness"),
            spec_path: "specs/harness.md".to_string(),
            diff: Some("=== specs/harness.md ===\n+ new requirement".to_string()),
        }],
        work_epic: BeadId::new("lm-work").unwrap(),
        todo_head: git_sha(TEST_SHA),
        todo_fingerprint: TodoFingerprint::new(TEST_FINGERPRINT).unwrap(),
        spec_epics: vec![SpecEpicContext {
            label: SpecLabel::new("harness"),
            epic_id: Some(MoleculeId::new("lm-spec")),
            todo_cursor: Some(TEST_SHA.to_string()),
        }],
        companion_paths: vec!["lib/sandbox/".into()],
        implementation_notes: vec![SpecImplementationNotes {
            label: SpecLabel::new("harness"),
            notes: vec!["Carry the unified todo protocol into snapshots.".into()],
        }],
        criterion_status: snapshot_criterion_status(),
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        skill_index: SkillIndexMarkdown::empty(),
    }
}

fn snapshot_criterion_status() -> Vec<CriterionStatus> {
    vec![
        CriterionStatus {
            spec_label: SpecLabel::new("templates"),
            criterion_id: CriterionId::new("engine-001"),
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
            criterion_id: CriterionId::new("engine-002"),
            criterion_text: "Rendered output is stable across runs.".into(),
            annotation: ann(
                AnnotationTier::Test,
                "template_renders_are_byte_stable_across_runs",
            ),
            evidence: EvidenceState::Current {
                result: CriterionResult::Fail,
                last_timestamp_ms: 1_716_200_000_000,
                last_commit: git_sha(TEST_SHA_2),
                commits_since: 7,
            },
        },
        CriterionStatus {
            spec_label: SpecLabel::new("templates"),
            criterion_id: CriterionId::new("criterion-status-001"),
            criterion_text: "Todo prompts render typed criterion status rows.".into(),
            annotation: ann(
                AnnotationTier::Test,
                "todo_template_renders_typed_criterion_status_rows",
            ),
            evidence: EvidenceState::Missing,
        },
        CriterionStatus {
            spec_label: SpecLabel::new("templates"),
            criterion_id: CriterionId::new("engine-003"),
            criterion_text: "Every non-pending pinning cell matches the include graph.".into(),
            annotation: ann(
                AnnotationTier::Check,
                "cargo run -p loom-walk -- template_pinning_matrix",
            ),
            evidence: EvidenceState::Current {
                result: CriterionResult::Skipped,
                last_timestamp_ms: 1_716_250_000_000,
                last_commit: git_sha(TEST_SHA_3),
                commits_since: 3,
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

#[test]
fn plan_snapshot() {
    let ctx = PlanContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        anchor_labels: vec![SpecLabel::new("harness"), SpecLabel::new("future-spec")],
        spec_index: "# Loom Docs\n| Spec | Purpose |\n| [harness](../specs/harness.md) | Harness |"
            .to_string(),
        companion_paths: vec![
            "lib/sandbox/".into(),
            "crates/loom-templates/templates/".into(),
        ],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

#[test]
fn todo_snapshot() {
    insta::assert_snapshot!(todo_ctx().render().unwrap());
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
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
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
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
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
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
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
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
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
                route: loom_protocol::gate::FindingRoute::Deferred,
                bonds: vec![SpecLabel::new("harness")],
                target: FindingTarget::Annotation {
                    target_string: "cargo test --lib parse_walks_all_md_files".into(),
                },
                evidence: "test mocks the agent backend instead of running the live driver".into(),
            }],
        }),
        review_notes: None,
        attempt: 1,
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
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
        scratchpad_path: "/workspace/.loom/scratch/lm-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
        skill_index: SkillIndexMarkdown::empty(),
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
        default_profile: ProfileName::new("rust"),
        skill_index: SkillIndexMarkdown::empty(),
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
        skill_index: SkillIndexMarkdown::empty(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}
