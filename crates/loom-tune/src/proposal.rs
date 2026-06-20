use loom_events::identifier::BeadId;
use serde::{Deserialize, Serialize};

use crate::checker::CheckerId;
use crate::plan::{
    Diagnostic, FrozenPlan, Hash as PlanHash, OutcomeSkeleton, SelectedCase, SkippedCase,
};
use crate::target::Target;

/// Tune proposal manifest persisted beside an isolated proposal worktree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalManifest {
    pub proposal_id: BeadId,
    pub targets: Vec<Target>,
    pub checker_plan: Vec<CheckerId>,
    pub plan_hash: PlanHash,
    pub selected_cases: Vec<SelectedCase>,
    pub skipped_cases: Vec<SkippedCase>,
    pub outcome_skeletons: Vec<OutcomeSkeleton>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ProposalManifest {
    pub fn from_plan(proposal_id: BeadId, plan: &FrozenPlan) -> Self {
        Self {
            proposal_id,
            targets: plan.targets.clone(),
            checker_plan: plan.checker_plan.clone(),
            plan_hash: plan.plan_hash.clone(),
            selected_cases: plan.selected_cases.clone(),
            skipped_cases: plan.skipped_cases.clone(),
            outcome_skeletons: plan.outcome_skeletons.clone(),
            diagnostics: plan.diagnostics.clone(),
        }
    }
}

/// Artifact surface a tune proposal edits.
pub type TuneTarget = Target;
