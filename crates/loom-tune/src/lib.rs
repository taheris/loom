//! Internal tuning registry, case, score, and proposal types.

pub mod case;
pub mod checker;
pub mod config;
pub mod evidence;
pub mod executor;
pub mod gate;
pub mod plan;
pub mod proposal;
pub mod score;
pub mod target;

pub use case::Id as TuningCaseId;
pub use checker::CheckerId;
pub use target::Target as TuneTarget;

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use crate::case::{Document, LoadContext, load_documents};
    use crate::checker::{CheckerId, Level, Registry};
    use crate::config::{ChecksConfig, EvidenceConfig, SelectionFraction, TuneConfig};
    use crate::evidence::{Item, ItemId, RootReport, Snapshot, SplitMetadata};
    use crate::executor::Replay;
    use crate::gate::{self, Outcome, State};
    use crate::plan::{PlannedCaseId, Pool, Request, build};
    use crate::target::{Catalog as TargetCatalog, Target};

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, body).expect("write file");
    }

    fn tracked(paths: &[&str]) -> BTreeSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn tuning_markdown(case_id: &str) -> String {
        format!(
            r#"```loom-case
id = "{case_id}"
checker = "behavior.review.finding-recall"
targets = ["skill:loom-review-finding-recall", "phase:review"]

[input]
patch = "cases/review.diff"

[expected]
max_extra_findings = 1

[[expected.findings]]
contains = ["missing test"]
```
"#
        )
    }

    fn target_catalog() -> TargetCatalog {
        TargetCatalog::new([
            "skill:loom-review-finding-recall"
                .parse::<Target>()
                .expect("skill target"),
            "phase:review".parse::<Target>().expect("phase target"),
        ])
    }

    fn loaded_cases(repo: &Path, markdown: String) -> crate::case::LoadedCases {
        let registry = Registry::builtin().expect("registry");
        let targets = target_catalog();
        let disabled = BTreeSet::new();
        load_documents(
            &[Document::repo(repo.join("docs/tuning.md"), markdown)],
            &LoadContext {
                repo_root: repo,
                tracked_files: &tracked(&["docs/tuning.md", "docs/cases/review.diff"]),
                targets: &targets,
                registry: &registry,
                disabled_checkers: &disabled,
            },
        )
        .expect("cases load")
    }

    fn split_metadata() -> SplitMetadata {
        SplitMetadata {
            algorithm: "sha256-salt-v1".to_owned(),
            salt_id: "repo".to_owned(),
            selection_fraction: SelectionFraction::new(0.34).expect("fraction"),
        }
    }

    #[test]
    fn checker_plan_rebuild_accepts_unchanged_inputs() {
        let repo = TempDir::new().expect("tempdir");
        write(
            &repo.path().join("docs/tuning.md"),
            &tuning_markdown("case-a"),
        );
        write(&repo.path().join("docs/cases/review.diff"), "diff --git\n");
        let registry = Registry::builtin().expect("registry");
        let cases = loaded_cases(repo.path(), tuning_markdown("case-a"));
        let target = "skill:loom-review-finding-recall"
            .parse::<Target>()
            .expect("target");
        let evidence = Snapshot {
            train: Vec::new(),
            selection: vec![Item::for_test(
                ItemId::new("selection-review-1").expect("item id"),
                CheckerId::new("behavior.review.finding-recall").expect("checker"),
                vec![target.clone()],
            )],
            metadata: split_metadata(),
        };
        let config = TuneConfig {
            checks: ChecksConfig {
                max_behavior_cases: 1,
                ..ChecksConfig::default()
            },
            evidence: EvidenceConfig::default(),
        };
        let plan = build(Request {
            targets: vec![target],
            level: Level::Run,
            cases: &cases,
            evidence: &evidence,
            config: &config,
            registry: &registry,
            seed: 99,
        })
        .expect("plan builds");
        assert!(!plan.checker_plan.is_empty());
        assert_eq!(plan.selected_cases.len(), 1);
        assert_eq!(plan.outcome_skeletons.len(), plan.selected_cases.len());
        let rebuilt = build(Request {
            targets: plan.targets.clone(),
            level: Level::Run,
            cases: &cases,
            evidence: &evidence,
            config: &config,
            registry: &registry,
            seed: 99,
        })
        .expect("plan rebuilds");
        plan.reject_if_changed(&rebuilt).expect("same hash");
    }

    #[test]
    fn tune_gate_requires_executor_results_for_selected_cases() {
        let repo = TempDir::new().expect("tempdir");
        write(
            &repo.path().join("docs/tuning.md"),
            &tuning_markdown("case-a"),
        );
        write(&repo.path().join("docs/cases/review.diff"), "diff --git\n");
        let evidence_config = EvidenceConfig {
            external_roots: vec![PathBuf::from("/tmp/explicit-transcripts")],
            ..EvidenceConfig::default()
        };
        let root_report = RootReport::from_config(repo.path(), &evidence_config);
        assert_eq!(root_report.roots().len(), 2);
        assert!(root_report.lines()[0].starts_with("workspace:"));
        assert!(root_report.lines()[1].contains("/tmp/explicit-transcripts"));
        assert!(
            !root_report
                .lines()
                .iter()
                .any(|line| line.contains(".claude"))
        );

        let registry = Registry::builtin().expect("registry");
        let cases = loaded_cases(repo.path(), tuning_markdown("case-a"));
        let target = "skill:loom-review-finding-recall"
            .parse::<Target>()
            .expect("target");
        let evidence = Snapshot {
            train: Vec::new(),
            selection: Vec::new(),
            metadata: split_metadata(),
        };
        let config = TuneConfig {
            evidence: evidence_config,
            checks: ChecksConfig::default(),
        };
        let plan = build(Request {
            targets: vec![target.clone()],
            level: Level::Run,
            cases: &cases,
            evidence: &evidence,
            config: &config,
            registry: &registry,
            seed: 7,
        })
        .expect("plan builds");
        let selected_by_id = plan
            .selected_cases
            .iter()
            .map(|case| (case.case_id.to_string(), case.checker.as_str().to_owned()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(selected_by_id.len(), 1);
        assert!(
            selected_by_id
                .values()
                .any(|checker| checker == "behavior.review.finding-recall")
        );

        let selected = plan
            .selected_cases
            .iter()
            .find(|case| matches!(case.case_id, PlannedCaseId::Declared(_)))
            .expect("declared case selected");
        assert_eq!(selected.pool, Pool::DeclaredRegression);
        let selected_case = plan.selected_cases[0].case_id.clone();
        let result = crate::executor::run(
            &plan,
            &cases,
            &[Replay::new(
                selected_case,
                "LOOM_COMPLETE",
                r#"LOOM_FINDING: {"evidence":"missing test"}"#,
            )],
        )
        .expect("checker implementations score replayed output");
        let report =
            gate::evaluate(&plan, result, &registry).expect("gate evaluates selected behavior");
        assert_eq!(report.state, State::Passed);
        assert_eq!(report.cases[0].outcome, Outcome::Improved);

        let missing = gate::evaluate(&plan, Vec::<gate::CaseResult>::new(), &registry)
            .expect_err("selected behavior must run before staging");
        assert!(matches!(missing, gate::Error::MissingResult { .. }));
    }
}
