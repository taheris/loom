use std::path::PathBuf;

use loom_events::identifier::BeadId;
use serde::{Deserialize, Serialize};

use crate::checker::{CheckerId, Level};
use crate::config::ChecksConfig;
use crate::plan::{
    Diagnostic, FrozenPlan, Hash as PlanHash, OutcomeSkeleton, SelectedCase, SkippedCase,
};
use crate::target::Target;

/// Tune proposal lifecycle state mirrored onto bead status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Pending,
    Blocked,
    Accepted,
    Applied,
    Rejected,
    ApplyFailed,
}

impl State {
    pub fn bead_status(self) -> &'static str {
        match self {
            Self::Pending | Self::Accepted => "open",
            Self::Blocked | Self::ApplyFailed => "blocked",
            Self::Applied | Self::Rejected => "closed",
        }
    }
}

/// Aggregate case counts recorded in tune bead metadata and manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseCounts {
    pub declared: usize,
    pub mined_train: usize,
    pub mined_selection: usize,
    pub selected: usize,
    pub skipped: usize,
}

/// Aggregate outcome counts recorded before human review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeCounts {
    pub pending: usize,
    pub passed: usize,
    pub failed: usize,
    pub blocked: usize,
}

impl OutcomeCounts {
    pub fn pending(count: usize) -> Self {
        Self {
            pending: count,
            passed: 0,
            failed: 0,
            blocked: 0,
        }
    }
}

/// Candidate-validation row shown in the durable tune report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationRow {
    pub check: String,
    pub status: ValidationStatus,
    pub detail: String,
}

/// Candidate-validation outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    Passed,
    Failed,
    Skipped,
}

/// Local paths that make up the disposable tune envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalPaths {
    pub repo: PathBuf,
    pub manifest: PathBuf,
    pub evidence: PathBuf,
    pub logs: PathBuf,
    pub evidence_dir: PathBuf,
}

/// Resource caps frozen with the checker plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    pub max_behavior_cases: usize,
    pub max_wall_time_secs: u64,
    pub max_llm_judge_calls: usize,
}

impl From<&ChecksConfig> for Caps {
    fn from(config: &ChecksConfig) -> Self {
        Self {
            max_behavior_cases: config.max_behavior_cases,
            max_wall_time_secs: config.max_wall_time_secs,
            max_llm_judge_calls: config.max_llm_judge_calls,
        }
    }
}

/// Tune proposal manifest persisted beside an isolated proposal worktree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalManifest {
    pub schema_version: u32,
    pub proposal_id: BeadId,
    pub workspace_path: PathBuf,
    pub state: State,
    pub targets: Vec<Target>,
    pub target_files: Vec<PathBuf>,
    pub level: Level,
    pub seed: u64,
    pub base_commit: String,
    pub proposal_branch: String,
    pub proposal_head: String,
    pub checker_plan: Vec<CheckerId>,
    pub plan_hash: PlanHash,
    pub selected_cases: Vec<SelectedCase>,
    pub skipped_cases: Vec<SkippedCase>,
    pub outcome_skeletons: Vec<OutcomeSkeleton>,
    pub diagnostics: Vec<Diagnostic>,
    pub case_counts: CaseCounts,
    pub outcome_counts: OutcomeCounts,
    pub validation: Vec<ValidationRow>,
    pub caps: Caps,
    pub local_paths: LocalPaths,
}

impl ProposalManifest {
    pub fn from_plan(input: ManifestInput<'_>) -> Self {
        Self {
            schema_version: 1,
            proposal_id: input.proposal_id,
            workspace_path: input.workspace_path,
            state: input.state,
            targets: input.plan.targets.clone(),
            target_files: input.target_files,
            level: input.plan.level,
            seed: input.plan.seed,
            base_commit: input.base_commit,
            proposal_branch: input.proposal_branch,
            proposal_head: input.proposal_head,
            checker_plan: input.plan.checker_plan.clone(),
            plan_hash: input.plan.plan_hash.clone(),
            selected_cases: input.plan.selected_cases.clone(),
            skipped_cases: input.plan.skipped_cases.clone(),
            outcome_skeletons: input.plan.outcome_skeletons.clone(),
            diagnostics: input.plan.diagnostics.clone(),
            outcome_counts: OutcomeCounts::pending(input.plan.outcome_skeletons.len()),
            case_counts: input.case_counts,
            validation: input.validation,
            caps: input.caps,
            local_paths: input.local_paths,
        }
    }
}

/// Inputs needed to materialize a proposal manifest from a frozen plan.
pub struct ManifestInput<'a> {
    pub proposal_id: BeadId,
    pub workspace_path: PathBuf,
    pub plan: &'a FrozenPlan,
    pub state: State,
    pub target_files: Vec<PathBuf>,
    pub base_commit: String,
    pub proposal_branch: String,
    pub proposal_head: String,
    pub case_counts: CaseCounts,
    pub validation: Vec<ValidationRow>,
    pub caps: Caps,
    pub local_paths: LocalPaths,
}

/// Artifact surface a tune proposal edits.
pub type TuneTarget = Target;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::LoadedCases;
    use crate::checker::Registry;
    use crate::config::{SelectionFraction, TuneConfig};
    use crate::evidence::{Snapshot, SplitMetadata};
    use crate::plan::{Request, build};

    fn target() -> Target {
        "skill:loom-review-finding-recall".parse().expect("target")
    }

    fn frozen_plan() -> FrozenPlan {
        let cases = LoadedCases::new(Vec::new(), Vec::new());
        let evidence = Snapshot::empty(SplitMetadata {
            algorithm: "sha256-salt-v1".to_owned(),
            salt_id: "repo".to_owned(),
            selection_fraction: SelectionFraction::new(0.25).expect("fraction"),
        });
        let config = TuneConfig::default();
        let registry = Registry::builtin().expect("registry");
        build(Request {
            targets: vec![target()],
            level: Level::Fast,
            cases: &cases,
            evidence: &evidence,
            config: &config,
            registry: &registry,
            seed: 17,
        })
        .expect("plan")
    }

    #[test]
    fn proposal_state_maps_to_bead_status() {
        assert_eq!(State::Pending.bead_status(), "open");
        assert_eq!(State::Accepted.bead_status(), "open");
        assert_eq!(State::Blocked.bead_status(), "blocked");
        assert_eq!(State::ApplyFailed.bead_status(), "blocked");
        assert_eq!(State::Applied.bead_status(), "closed");
        assert_eq!(State::Rejected.bead_status(), "closed");
    }

    #[test]
    fn proposal_manifest_records_lifecycle_metadata_from_plan() {
        let plan = frozen_plan();
        let caps = Caps::from(&TuneConfig::default().checks);
        let local_paths = LocalPaths {
            repo: ".loom/tune/lm-tune.1/repo".into(),
            manifest: ".loom/tune/lm-tune.1/manifest.json".into(),
            evidence: ".loom/tune/lm-tune.1/evidence.md".into(),
            logs: ".loom/tune/lm-tune.1/logs".into(),
            evidence_dir: ".loom/tune/lm-tune.1/evidence".into(),
        };
        let validation = vec![ValidationRow {
            check: "askama-render".into(),
            status: ValidationStatus::Passed,
            detail: "rendered sample prompt".into(),
        }];
        let case_counts = CaseCounts {
            declared: 0,
            mined_train: 0,
            mined_selection: 0,
            selected: plan.selected_cases.len(),
            skipped: plan.skipped_cases.len(),
        };
        let manifest = ProposalManifest::from_plan(ManifestInput {
            proposal_id: BeadId::new("lm-tune.1").expect("bead id"),
            workspace_path: "/workspace".into(),
            plan: &plan,
            state: State::Pending,
            target_files: vec!["crates/loom-skills/builtin/base/example/skill.md".into()],
            base_commit: "base".into(),
            proposal_branch: "loom/tune/lm-tune.1".into(),
            proposal_head: "head".into(),
            case_counts: case_counts.clone(),
            validation: validation.clone(),
            caps: caps.clone(),
            local_paths: local_paths.clone(),
        });

        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.state, State::Pending);
        assert_eq!(manifest.level, plan.level);
        assert_eq!(manifest.seed, plan.seed);
        assert_eq!(manifest.targets, plan.targets);
        assert_eq!(manifest.checker_plan, plan.checker_plan);
        assert_eq!(manifest.plan_hash, plan.plan_hash);
        assert_eq!(manifest.case_counts, case_counts);
        assert_eq!(
            manifest.outcome_counts,
            OutcomeCounts::pending(plan.outcome_skeletons.len())
        );
        assert_eq!(manifest.validation, validation);
        assert_eq!(manifest.caps, caps);
        assert_eq!(manifest.local_paths, local_paths);
    }
}
