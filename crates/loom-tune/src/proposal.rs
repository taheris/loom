use loom_events::identifier::BeadId;
use loom_skills::identity::SkillName;
use serde::{Deserialize, Serialize};

use crate::checker::CheckerId;

/// Tune proposal manifest persisted beside an isolated proposal worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalManifest {
    pub proposal_id: BeadId,
    pub targets: Vec<TuneTarget>,
    pub checker_plan: Vec<CheckerId>,
}

impl ProposalManifest {
    pub fn new(
        proposal_id: BeadId,
        targets: Vec<TuneTarget>,
        checker_plan: Vec<CheckerId>,
    ) -> Self {
        Self {
            proposal_id,
            targets,
            checker_plan,
        }
    }
}

/// Artifact surface a tune proposal edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TuneTarget {
    Skill { name: SkillName },
}
