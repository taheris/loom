//! Structural observations for semantic planning probes; parser success is not coverage.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use loom_driver::identifier::SpecLabel;
use loom_gate::annotation::{Tier, parse_content, parse_workspace};
use loom_gate::inputs::InputResolver;
use loom_templates::SkillIndexMarkdown;
use loom_workflow::plan::{PlanPromptInputs, render_prompt};

fn repo_root() -> Result<PathBuf> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}

#[test]
fn planning_package_probes_are_structurally_valid_without_claiming_semantic_coverage() -> Result<()>
{
    for (case, expected_count) in [("unmapped", 1), ("equal-count", 2), ("split", 3)] {
        let dir = tempfile::tempdir()?;
        let package = dir.path().join("specs/queue");
        fs::create_dir_all(&package)?;
        fs::create_dir(dir.path().join("docs"))?;
        let fixture = repo_root()?
            .join("tests/fixtures/planning/coverage")
            .join(case);
        for document in ["spec.md", "tests.md"] {
            fs::copy(fixture.join(document), package.join(document))?;
        }
        let index = "## Specs\n\n| Spec | Purpose |\n| --- | --- |\n| [queue](../specs/queue/spec.md) | Queue |\n";
        fs::write(dir.path().join("docs/README.md"), index)?;

        let parsed = parse_workspace(dir.path())?;
        assert_eq!(parsed.criteria.len(), expected_count, "{case}");
        assert_eq!(parsed.annotations.len(), expected_count, "{case}");
        for criterion in &parsed.criteria {
            assert_eq!(criterion.source_spec, package.join("tests.md"));
            assert_eq!(
                parsed
                    .annotations
                    .iter()
                    .filter(|annotation| {
                        (annotation.source_spec.as_path(), annotation.criterion_line)
                            == (criterion.source_spec.as_path(), criterion.line)
                    })
                    .count(),
                1,
                "{case}: exactly one adjacent binding"
            );
        }
        assert!(
            parsed
                .annotations
                .iter()
                .all(|annotation| annotation.pending)
        );
        assert_eq!(
            parse_content(
                &package.join("spec.md"),
                &fs::read_to_string(package.join("spec.md"))?
            )
            .annotations,
            vec![]
        );

        let prompt = render_prompt(PlanPromptInputs {
            anchor_labels: vec![SpecLabel::new("queue")?],
            pinned_context: "Planning probe".into(),
            spec_index: index.into(),
            companion_paths: vec![],
            scratchpad_path: "/workspace/.loom/scratch/queue/scratch.md".into(),
            spec_conventions: "docs/spec-conventions.md".into(),
            skill_index: SkillIndexMarkdown::empty(),
        })?;
        assert!(prompt.contains("read both `specs/queue/spec.md` and `specs/queue/tests.md`"));
        assert!(prompt.contains("Assess actual semantic coverage"));
    }
    Ok(())
}

#[test]
fn planning_judges_collect_delivered_guidance_and_semantic_inputs() -> Result<()> {
    let root = repo_root()?;
    let source = root.join("specs/plan.md");
    let parsed = parse_content(&source, &fs::read_to_string(&source)?);
    let mut resolver = InputResolver::new(root);
    for (file, fixture) in [
        (
            "package-contract-coverage.md",
            "tests/fixtures/planning/coverage-traces.md",
        ),
        (
            "planning-interview-authority.md",
            "tests/fixtures/planning/interview-traces.md",
        ),
    ] {
        let annotation = parsed
            .annotations
            .iter()
            .find(|annotation| annotation.tier == Tier::Judge && annotation.target.contains(file))
            .expect("planning judge binding");
        assert!(!annotation.pending);
        let (inputs, declared) = resolver.resolve_with_provenance(annotation);
        assert!(declared, "{file}: collect-mode inputs must be declared");
        for path in [
            "crates/loom-workflow/src/plan/runner.rs",
            "crates/loom-templates/templates/plan.md",
            "crates/loom-templates/tests/snapshots/snapshots__plan_snapshot.snap",
            fixture,
        ] {
            assert!(
                inputs.paths.contains(&PathBuf::from(path)),
                "{file}: missing {path}"
            );
        }
    }
    Ok(())
}
