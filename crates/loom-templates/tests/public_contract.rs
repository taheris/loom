//! Pin the public-contract surface external consumers depend on:
//! `PinnedContext`, the `PARTIAL_*` constants, and the re-exported typed
//! context structs. These tests imitate a downstream Rust crate that
//! composes its own template prompt from Loom's exposed building blocks
//! without touching any workflow-template internals.

use loom_templates::{
    PARTIAL_CHAT_MARKER_FINAL_TURN_ONLY, PARTIAL_COMPANIONS_CONTEXT, PARTIAL_CONTEXT_PINNING,
    PARTIAL_EXIT_SIGNALS, PARTIAL_INTERVIEW_MODES, PARTIAL_INVARIANT_CLASH,
    PARTIAL_PLAN_STAGE_RUBRIC, PARTIAL_REVIEW_RUBRIC, PARTIAL_SCRATCHPAD,
    PARTIAL_SIBLING_SPEC_EDITING, PARTIAL_SPEC_CONVENTIONS, PARTIAL_SPEC_HEADER,
    PARTIAL_STYLE_RULES, PinnedContext, PreviousFailure, ReviewConcernKind, RunContext,
    VerifierFailure,
};

#[test]
fn pinned_context_holds_project_overview_and_style_rules() {
    let ctx = PinnedContext {
        pinned_context: "# Project Overview".into(),
        style_rules: "docs/style-rules.md body".into(),
    };
    assert_eq!(ctx.pinned_context, "# Project Overview");
    assert_eq!(ctx.style_rules, "docs/style-rules.md body");
}

#[test]
fn partial_constants_carry_their_source_files() {
    for (name, body) in [
        (
            "chat_marker_final_turn_only",
            PARTIAL_CHAT_MARKER_FINAL_TURN_ONLY,
        ),
        ("companions_context", PARTIAL_COMPANIONS_CONTEXT),
        ("context_pinning", PARTIAL_CONTEXT_PINNING),
        ("exit_signals", PARTIAL_EXIT_SIGNALS),
        ("interview_modes", PARTIAL_INTERVIEW_MODES),
        ("invariant_clash", PARTIAL_INVARIANT_CLASH),
        ("plan_stage_rubric", PARTIAL_PLAN_STAGE_RUBRIC),
        ("review_rubric", PARTIAL_REVIEW_RUBRIC),
        ("scratchpad", PARTIAL_SCRATCHPAD),
        ("sibling_spec_editing", PARTIAL_SIBLING_SPEC_EDITING),
        ("spec_conventions", PARTIAL_SPEC_CONVENTIONS),
        ("spec_header", PARTIAL_SPEC_HEADER),
        ("style_rules", PARTIAL_STYLE_RULES),
    ] {
        assert!(
            !body.is_empty(),
            "partial `{name}` constant is empty — include_str! resolved an empty file?",
        );
    }
}

#[test]
fn partial_context_pinning_renders_pinned_context_variable() {
    assert!(
        PARTIAL_CONTEXT_PINNING.contains("{{ pinned_context"),
        "context_pinning partial must render the `pinned_context` variable",
    );
}

#[test]
fn partial_style_rules_renders_style_rules_variable() {
    assert!(
        PARTIAL_STYLE_RULES.contains("{{ style_rules"),
        "style_rules partial must render the `style_rules` variable",
    );
}

#[test]
fn typed_retry_context_round_trips_through_public_re_exports() {
    let pf =
        PreviousFailure::VerifyFailures(vec![VerifierFailure::new("tests/sample.sh", 1, "boom\n")]);
    let rendered = pf.to_string();
    assert!(rendered.contains("tests/sample.sh"));

    let review = PreviousFailure::ReviewConcern {
        concern: ReviewConcernKind::SpecCoherence,
        reason: "scope creep".into(),
    };
    assert!(review.to_string().contains("(spec-coherence)"));
}

#[test]
fn run_context_is_publicly_constructible_from_crate_root() {
    use loom_events::identifier::{BeadId, MoleculeId, SpecLabel};

    let _ctx = RunContext {
        pinned_context: String::new(),
        label: SpecLabel::new("demo"),
        spec_path: String::new(),
        companion_paths: vec![],
        molecule_id: Some(MoleculeId::new("wx-demo")),
        issue_id: BeadId::new("wx-demo.1").ok(),
        title: None,
        description: None,
        previous_failure: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: String::new(),
        style_rules: String::new(),
    };
}
