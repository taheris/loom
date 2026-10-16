use anyhow::{Context as _, Result};
use askama::Template;
use loom_protocol::output::Phase;
use loom_templates::{
    CriterionStatus, EvidenceState, InboxContext, LoopContext, PlanContext, ReviewContext,
    SkillIndexMarkdown, TodoContext,
    criterion_status::{AnnotationTarget, AnnotationTier, CriterionAnnotation},
    review::ReviewLane,
    todo::{EvidenceDelivery, SpecImplementationNotes, TodoChangedSpec},
};

pub const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
pub const FINGERPRINT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

pub fn todo_context() -> Result<TodoContext> {
    Ok(TodoContext {
        package_context: None,
        pinned_context: "Read docs/README.md".into(),
        spec_index: "Example and sibling contracts".into(),
        changed_specs: vec![TodoChangedSpec {
            label: "example".parse()?,
            spec_path: "specs/example.md".into(),
            acceptance_path: "specs/example.md".into(),
            input_scope: "specs/example.md".into(),
            diff: Some("+ explicit contract".into()),
        }],
        work_epic: "lm-work".parse()?,
        todo_head: SHA.parse()?,
        todo_fingerprint: FINGERPRINT.parse()?,
        spec_epics: vec![],
        companion_paths: vec![],
        implementation_notes: vec![SpecImplementationNotes {
            label: "example".parse()?,
            notes: vec!["Implement the example contract".into()],
        }],
        criterion_status: vec![CriterionStatus {
            spec_label: "example".parse()?,
            criterion_id: "criterion-0000000000000001".parse()?,
            criterion_text: "Exercise the live consumer".into(),
            annotation: CriterionAnnotation {
                tier: AnnotationTier::Test,
                pending: false,
                target: AnnotationTarget::new("crate::example::test_live"),
            },
            evidence: EvidenceState::Missing,
        }],
        scratchpad_path: "/scratch/example.md".into(),
        skill_index: SkillIndexMarkdown::empty(),
        evidence_delivery: EvidenceDelivery::Inline,
    })
}

pub fn render(phase: Phase) -> Result<String> {
    Ok(match phase {
        Phase::Plan => PlanContext {
            package_context: None,
            pinned_context: "Read docs/README.md".into(),
            anchor_labels: vec!["example".parse()?],
            spec_index: "Example contract".into(),
            companion_paths: vec![],
            scratchpad_path: "/scratch/example.md".into(),
            spec_conventions: "docs/spec-conventions.md".into(),
            skill_index: SkillIndexMarkdown::empty(),
        }
        .render()?,
        Phase::Todo => todo_context()?.render()?,
        Phase::Loop => LoopContext {
            package_context: None,
            acceptance: None,
            pinned_context: "Read docs/README.md".into(),
            label: "example".parse()?,
            spec_path: "specs/example.md".into(),
            companion_paths: vec![],
            molecule_id: Some("lm-work".parse()?),
            issue_id: Some("lm-work.1".parse()?),
            bead_base: SHA.parse()?,
            title: Some("Implement the example contract".into()),
            description: None,
            previous_failure: None,
            workspace_recovery: None,
            review_notes: None,
            attempt: 0,
            scratchpad_path: "/scratch/example.md".into(),
            style_rules: "docs/style-rules.md".into(),
            skill_index: SkillIndexMarkdown::empty(),
        }
        .render()?,
        Phase::Review => ReviewContext {
            package_context: None,
            pinned_context: "Read docs/README.md".into(),
            label: "example".parse()?,
            spec_path: "specs/example.md".into(),
            companion_paths: vec![],
            beads_summary: None,
            base_commit: None,
            molecule_id: Some("lm-work".parse()?),
            test_sources: vec![],
            judge_rubrics: vec![],
            scratchpad_path: "/scratch/example.md".into(),
            style_rules: "docs/style-rules.md".into(),
            lane: ReviewLane::Both,
            default_profile: "base".parse()?,
            skill_index: SkillIndexMarkdown::empty(),
        }
        .render()?,
        Phase::Inbox => InboxContext {
            pinned_context: "Read docs/README.md".into(),
            companion_paths: vec![],
            inbox_items: vec![],
            scratchpad_path: "/scratch/example.md".into(),
            skill_index: SkillIndexMarkdown::empty(),
        }
        .render()?,
    })
}

pub fn workspace() -> Result<tempfile::TempDir> {
    let directory = tempfile::tempdir()?;
    for (path, body) in [
        (
            "docs/README.md",
            "# Project\n\n| Spec | Purpose |\n| --- | --- |\n| [example](../specs/example.md) | Example |\n| [sibling](../specs/sibling.md) | Sibling |\n",
        ),
        (
            "specs/example.md",
            "# Example\n\n## Invariant\nA single writer owns publication.\n\n## Acceptance\n- Exercise the consumer. [test](crate::example::test_live)\n",
        ),
        (
            "specs/sibling.md",
            "# Sibling\n\n## Acceptance\n- Exercise the sibling. [test](crate::example::test_live)\n",
        ),
        (
            "tests/example.rs",
            "#[test]\nfn test_live() { assert_eq!(1, 1); }\n",
        ),
        (
            "crates/loom-templates/templates/example.md",
            "# Example template\n",
        ),
    ] {
        let destination = directory.path().join(path);
        std::fs::create_dir_all(destination.parent().context("fixture path has parent")?)?;
        std::fs::write(destination, body)?;
    }
    Ok(directory)
}
