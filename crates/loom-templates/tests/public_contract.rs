//! Pin the public-contract surface external consumers depend on:
//! `PinnedContext`, the `PARTIAL_*` constants, and the re-exported typed
//! context structs. These tests imitate a downstream Rust crate that
//! composes its own template prompt from Loom's exposed building blocks
//! without touching any workflow-template internals.

use loom_templates::{
    AnnotationTarget, AnnotationTier, BadWalk, ConcernToken, CriterionAnnotation, CriterionId,
    CriterionResult, CriterionStatus, DriverNoticeCause, EvidenceState, Finding, FindingTarget,
    LoopContext, PARTIAL_CHAT_MARKER_FINAL_TURN_ONLY, PARTIAL_COMPANIONS_CONTEXT,
    PARTIAL_CONTEXT_PINNING, PARTIAL_FINDINGS_WALK, PARTIAL_INTERVIEW_MODES,
    PARTIAL_INVARIANT_CLASH, PARTIAL_PLAN_STAGE_RUBRIC, PARTIAL_PROGRESS_MARKERS,
    PARTIAL_REVIEW_RUBRIC, PARTIAL_REVIEW_SELF_REPORT_MARKERS, PARTIAL_SCRATCHPAD,
    PARTIAL_SELF_REPORT_MARKERS, PARTIAL_SIBLING_SPEC_EDITING, PARTIAL_SKILL_INDEX,
    PARTIAL_SPEC_CONVENTIONS, PARTIAL_SPEC_HEADER, PARTIAL_STYLE_RULES, PARTIAL_TODO_SUCCESS,
    PARTIAL_WORKSPACE_RECOVERY, PinnedContext, PlanContext, PreviousFailure, RecoveryStash,
    SkillIndexMarkdown, SpecImplementationNotes, TodoChangedSpec, TodoContext, VerifierFailure,
    WorkspaceAlignment, WorkspaceRecovery,
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
        ("findings_walk", PARTIAL_FINDINGS_WALK),
        ("interview_modes", PARTIAL_INTERVIEW_MODES),
        ("invariant_clash", PARTIAL_INVARIANT_CLASH),
        ("plan_stage_rubric", PARTIAL_PLAN_STAGE_RUBRIC),
        ("progress_markers", PARTIAL_PROGRESS_MARKERS),
        ("review_rubric", PARTIAL_REVIEW_RUBRIC),
        (
            "review_self_report_markers",
            PARTIAL_REVIEW_SELF_REPORT_MARKERS,
        ),
        ("scratchpad", PARTIAL_SCRATCHPAD),
        ("skill_index", PARTIAL_SKILL_INDEX),
        ("self_report_markers", PARTIAL_SELF_REPORT_MARKERS),
        ("sibling_spec_editing", PARTIAL_SIBLING_SPEC_EDITING),
        ("spec_conventions", PARTIAL_SPEC_CONVENTIONS),
        ("spec_header", PARTIAL_SPEC_HEADER),
        ("style_rules", PARTIAL_STYLE_RULES),
        ("todo_success", PARTIAL_TODO_SUCCESS),
        ("workspace_recovery", PARTIAL_WORKSPACE_RECOVERY),
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
fn workspace_recovery_context_is_publicly_constructible_from_crate_root() {
    let commit = loom_protocol::todo::GitSha::new("0123456789abcdef0123456789abcdef01234567")
        .expect("valid git sha");
    let integration_tip =
        loom_protocol::todo::GitSha::new("1111111111111111111111111111111111111111")
            .expect("valid git sha");
    let ctx = WorkspaceRecovery {
        pre_stash_status: "## loom/demo\n M src/lib.rs".into(),
        stash: RecoveryStash {
            selector: "stash@{0}".into(),
            commit: commit.clone(),
            message: "loom workspace-recovery lm-demo.1 1716250000".into(),
        },
        integration_tip: integration_tip.clone(),
        alignment: WorkspaceAlignment::Conflict {
            files: vec!["src/lib.rs".into()],
        },
    };

    assert_eq!(ctx.pre_stash_status, "## loom/demo\n M src/lib.rs");
    assert_eq!(ctx.stash.selector, "stash@{0}");
    assert_eq!(ctx.stash.commit, commit);
    assert_eq!(ctx.integration_tip, integration_tip);
    assert!(ctx.alignment.is_conflict());
    assert_eq!(ctx.alignment.conflict_files(), &["src/lib.rs".to_string()]);

    let clean = WorkspaceAlignment::Clean;
    assert!(!clean.is_conflict());
    assert_eq!(clean.to_string(), "Clean");

    let rebased = WorkspaceAlignment::Rebased {
        previous_head: loom_protocol::todo::GitSha::new("2222222222222222222222222222222222222222")
            .expect("valid git sha"),
        current_head: loom_protocol::todo::GitSha::new("3333333333333333333333333333333333333333")
            .expect("valid git sha"),
    };
    assert!(!rebased.is_conflict());
    assert!(rebased.to_string().contains("Rebased"));
}

#[test]
fn criterion_status_public_shape_carries_annotation_and_evidence_states() {
    let spec_label = loom_events::identifier::SpecLabel::new("templates");
    let criterion_id = CriterionId::new("criterion-status-surface");
    let annotation = CriterionAnnotation {
        tier: AnnotationTier::Check,
        target: AnnotationTarget::new("cargo run -p loom-walk -- template_pinning_matrix"),
        pending: false,
    };
    let current = CriterionStatus {
        spec_label: spec_label.clone(),
        criterion_id: criterion_id.clone(),
        criterion_text: "CriterionStatus rows carry evidence.".into(),
        annotation: annotation.clone(),
        evidence: EvidenceState::Current {
            result: CriterionResult::Pass,
            last_timestamp_ms: 1_716_300_000_000,
            last_commit: loom_protocol::todo::GitSha::new(
                "0123456789abcdef0123456789abcdef01234567",
            )
            .expect("valid git sha"),
            commits_since: 0,
        },
    };
    assert_eq!(current.spec_label, spec_label);
    assert_eq!(current.criterion_id, criterion_id);
    assert_eq!(
        current.criterion_text,
        "CriterionStatus rows carry evidence."
    );
    assert_eq!(current.annotation.tier, AnnotationTier::Check);
    assert_eq!(current.evidence.as_str(), "Current");
    assert_eq!(current.evidence.result_label(), "Pass");

    let missing = EvidenceState::Missing;
    assert_eq!(missing.as_str(), "Missing");
    assert_eq!(missing.result_label(), "—");

    let stale = EvidenceState::StaleAnnotation {
        cached_annotation: annotation,
        last_timestamp_ms: 1_716_000_000_000,
        last_commit: loom_protocol::todo::GitSha::new("1111111111111111111111111111111111111111")
            .expect("valid git sha"),
        commits_since: 2,
    };
    assert_eq!(stale.as_str(), "StaleAnnotation");
    assert_eq!(stale.commits_since_label(), "2");
    assert!(stale.cached_annotation_label().contains("[check]"));
}

#[test]
fn previous_failure_public_variant_contract_is_constructible() {
    let spec = loom_events::identifier::SpecLabel::new("templates");
    let finding = Finding {
        token: ConcernToken::SpecCoherenceFail,
        route: loom_protocol::gate::FindingRoute::Deferred,
        bonds: vec![spec.clone()],
        target: FindingTarget::Criterion {
            spec,
            anchor: "typed-previousfailure".into(),
        },
        evidence: "typed retry context carries the finding".into(),
    };
    let verifier = VerifierFailure::new("cargo test -p loom-templates", 101, "compile error");
    let variants = vec![
        PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::ZeroProgress,
            detail: "empty diff".into(),
        },
        PreviousFailure::VerifyFailures(vec![verifier.clone()]),
        PreviousFailure::ReviewConcern {
            summary: "review summary".into(),
            findings: vec![finding],
        },
        PreviousFailure::BadWalk(BadWalk::ConcernWithoutFindings {
            summary: "missing streamed finding".into(),
        }),
        PreviousFailure::BuildFailure {
            stage: "cargo".into(),
            output: "build failed".into(),
        },
        PreviousFailure::TreeNotClean {
            dirty_paths: vec!["src/lib.rs".into()],
        },
        PreviousFailure::PostIntegrateFail {
            failures: vec![verifier],
            gate_log_path: std::path::PathBuf::from(".loom/logs/gate.json"),
        },
        PreviousFailure::IntegrationConflict {
            files: vec![std::path::PathBuf::from("src/lib.rs")],
            new_base_sha: loom_protocol::oid::GitOid::new(
                "deadbeefcafe1234567890abcdef0123456789ab",
            )
            .expect("valid git oid"),
        },
        PreviousFailure::AgentRetry {
            reason: "sandbox cwd disappeared".into(),
        },
    ];

    for variant in variants {
        assert!(!variant.to_string().is_empty());
    }
}

#[test]
fn typed_retry_context_round_trips_through_public_re_exports() {
    let pf =
        PreviousFailure::VerifyFailures(vec![VerifierFailure::new("tests/sample.sh", 1, "boom\n")]);
    let rendered = pf.to_string();
    assert!(rendered.contains("tests/sample.sh"));

    let review = PreviousFailure::ReviewConcern {
        summary: "spec coherence wobble".into(),
        findings: vec![Finding {
            token: ConcernToken::SpecCoherenceFail,
            route: loom_protocol::gate::FindingRoute::Deferred,
            bonds: vec![loom_events::identifier::SpecLabel::new("gate")],
            target: FindingTarget::Criterion {
                spec: loom_events::identifier::SpecLabel::new("gate"),
                anchor: "verifier-honesty".into(),
            },
            evidence: "annotation does not exercise the contract".into(),
        }],
    };
    let rendered = review.to_string();
    assert!(
        rendered.starts_with("Review raised a concern ("),
        "label-prefixed framing missing: {rendered}",
    );
    assert!(
        rendered.contains("spec coherence wobble"),
        "summary in framing: {rendered}",
    );
    assert!(
        rendered.contains("spec-coherence-fail @ criterion:gate:verifier-honesty"),
        "{rendered}",
    );
    assert!(
        rendered.contains("annotation does not exercise the contract"),
        "{rendered}",
    );

    let bad = PreviousFailure::BadWalk(BadWalk::FindingsWithoutConcern {
        finding_count: 2,
        findings: vec![],
    });
    assert!(bad.to_string().contains("LOOM_FINDING"));
}

#[test]
fn plan_context_is_publicly_constructible_from_crate_root() {
    let _ctx = PlanContext {
        pinned_context: String::new(),
        anchor_labels: vec![loom_events::identifier::SpecLabel::new("demo")],
        spec_index: String::new(),
        companion_paths: vec![],
        scratchpad_path: String::new(),
        spec_conventions: String::new(),
        skill_index: SkillIndexMarkdown::empty(),
    };
}

#[test]
fn todo_context_is_publicly_constructible_from_crate_root() {
    use loom_events::identifier::BeadId;
    use loom_protocol::todo::{GitSha, TodoFingerprint};

    let _ctx = TodoContext {
        pinned_context: String::new(),
        spec_index: String::new(),
        changed_specs: vec![TodoChangedSpec {
            label: loom_events::identifier::SpecLabel::new("demo"),
            spec_path: "specs/demo.md".to_string(),
            diff: None,
        }],
        work_epic: BeadId::new("lm-work").expect("valid bead id"),
        todo_head: GitSha::new("0123456789abcdef0123456789abcdef01234567").expect("valid git sha"),
        todo_fingerprint: TodoFingerprint::new(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("valid fingerprint"),
        spec_epics: vec![],
        companion_paths: vec![],
        implementation_notes: vec![SpecImplementationNotes {
            label: loom_events::identifier::SpecLabel::new("demo"),
            notes: vec![],
        }],
        criterion_status: vec![],
        scratchpad_path: String::new(),
        skill_index: SkillIndexMarkdown::empty(),
    };
}

#[test]
fn run_context_is_publicly_constructible_from_crate_root() {
    use loom_events::identifier::{BeadId, MoleculeId, SpecLabel};

    let _ctx = LoopContext {
        pinned_context: String::new(),
        label: SpecLabel::new("demo"),
        spec_path: String::new(),
        companion_paths: vec![],
        molecule_id: Some(MoleculeId::new("lm-demo")),
        issue_id: BeadId::new("lm-demo.1").ok(),
        title: None,
        description: None,
        previous_failure: None,
        workspace_recovery: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: String::new(),
        style_rules: String::new(),
        skill_index: SkillIndexMarkdown::empty(),
    };
}
