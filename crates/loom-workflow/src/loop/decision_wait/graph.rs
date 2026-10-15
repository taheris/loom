//! Checked native affected-work edges and untrusted candidate provenance.

use std::collections::HashSet;

use loom_driver::bd::{BdClient, Bead, CommandRunner, IssueType, Status};
use loom_driver::identifier::BeadId;
use serde::{Deserialize, Serialize};

use super::{LoopError, invalid};

pub(super) const CANDIDATE_KEY: &str = "loom.decision_candidate";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    version: u8,
    producer: BeadId,
    root: BeadId,
    affected_work: Vec<BeadId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    id: BeadId,
    candidate: Option<Candidate>,
    affected_work: Vec<BeadId>,
}

/// Stored only in the driver journal after reading the whole native graph.
/// Missing contexts in legacy .93 records do not invent an affected-work proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Context {
    decisions: Vec<Decision>,
}

impl Context {
    pub(super) fn matches_inventory(&self, ids: &[BeadId]) -> bool {
        self.decisions.len() == ids.len()
            && self
                .decisions
                .iter()
                .zip(ids)
                .all(|(decision, id)| decision.id == *id)
    }
}

fn candidate(
    source: &Bead,
    root: &BeadId,
    decision: &Bead,
) -> Result<Option<Candidate>, LoopError> {
    let Some(value) = decision.metadata.get(CANDIDATE_KEY) else {
        // Keep the previously authorized native discovery-child path. New
        // explicitly staged candidates must carry their untrusted proposal.
        if decision.status == Status::Blocked
            && !decision.labels.iter().any(|label| {
                matches!(
                    label.as_str(),
                    "loom:clarify" | "loom:blocked" | "loom:infra"
                )
            })
        {
            return Err(invalid(format!(
                "staged decision {} lacks candidate provenance",
                decision.id
            ))
            .into());
        }
        return Ok(None);
    };
    let encoded = value.as_str().ok_or_else(|| {
        invalid(format!(
            "decision {} candidate provenance must be a JSON string",
            decision.id
        ))
    })?;
    let mut proposal: Candidate = serde_json::from_str(encoded).map_err(|error| {
        invalid(format!(
            "decision {} has malformed candidate provenance: {error}",
            decision.id
        ))
    })?;
    let unique: HashSet<_> = proposal.affected_work.iter().collect();
    if proposal.version != 1
        || proposal.producer != source.id
        || proposal.root != *root
        || unique.len() != proposal.affected_work.len()
    {
        return Err(invalid(format!(
            "decision {} has incompatible candidate provenance",
            decision.id
        ))
        .into());
    }
    proposal
        .affected_work
        .sort_by(|left, right| left.as_str().cmp(right.as_str()));
    Ok(Some(proposal))
}

async fn check_affected_work<R: CommandRunner>(
    bd: &BdClient<R>,
    root: &BeadId,
    id: &BeadId,
) -> Result<(), LoopError> {
    let work = bd.show(id).await?;
    if work.id != *id
        || matches!(
            work.issue_type,
            IssueType::Epic | IssueType::Molecule | IssueType::Decision
        )
        || matches!(work.status, Status::Pinned | Status::Tombstone)
    {
        return Err(invalid(format!("decision affects incompatible work {id}")).into());
    }
    let mut current = work;
    let mut seen = HashSet::new();
    loop {
        if current.id == *root {
            return Ok(());
        }
        if !seen.insert(current.id.clone()) {
            return Err(invalid(format!("cyclic ownership of affected work {id}")).into());
        }
        let Some(parent) = current.parent else {
            return Err(invalid(format!(
                "affected work {id} is outside decision work root {root}"
            ))
            .into());
        };
        current = bd.show(&parent).await?;
        if current.id != parent {
            return Err(invalid(format!(
                "affected work {id} has ambiguous native parent {parent}"
            ))
            .into());
        }
    }
}

/// Validate every current affected-work edge before the first queue mutation.
/// Previously admitted decisions may be reused only with the recorded graph.
pub(super) async fn resolve<R: CommandRunner>(
    bd: &BdClient<R>,
    source: &Bead,
    root: &BeadId,
    decisions: &[Bead],
    previous: Option<&Context>,
) -> Result<Context, LoopError> {
    let mut checked = Vec::with_capacity(decisions.len());
    for decision in decisions {
        let proposal = candidate(source, root, decision)?;
        let mut affected_work = bd.blocking_dependents(&decision.id).await?;
        let unique: HashSet<_> = affected_work.iter().collect();
        if unique.len() != affected_work.len() {
            return Err(invalid(format!(
                "duplicate native affected-work edges for {}",
                decision.id
            ))
            .into());
        }
        affected_work.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        for id in &affected_work {
            check_affected_work(bd, root, id).await?;
        }
        if let Some(proposal) = &proposal {
            let mut declared = proposal.affected_work.clone();
            declared.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            if declared != affected_work {
                return Err(invalid(format!(
                    "decision {} candidate disagrees with native affected-work edges",
                    decision.id
                ))
                .into());
            }
        }
        let current = Decision {
            id: decision.id.clone(),
            candidate: proposal,
            affected_work,
        };
        if let Some(previous) =
            previous.and_then(|context| context.decisions.iter().find(|old| old.id == current.id))
            && previous != &current
        {
            return Err(invalid(format!(
                "admitted decision {} changed candidate or affected-work graph",
                decision.id
            ))
            .into());
        }
        checked.push(current);
    }
    Ok(Context { decisions: checked })
}
