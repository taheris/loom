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
    producer: BeadId,
    candidate: Option<Candidate>,
    affected_work: Vec<BeadId>,
}

/// Stored only in the driver journal after reading the whole native graph.
/// Missing contexts in legacy .93 records do not invent an affected-work proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Context {
    producer: BeadId,
    producer_type: IssueType,
    decisions: Vec<Decision>,
}

impl Context {
    pub(super) fn discovery_parent(&self, id: &BeadId) -> Option<&BeadId> {
        self.decisions
            .iter()
            .find(|decision| decision.id == *id)
            .map(|decision| &decision.producer)
    }

    pub(super) fn matches_inventory(&self, producer: &BeadId, ids: &[BeadId]) -> bool {
        self.producer == *producer
            && self.decisions.len() == ids.len()
            && self
                .decisions
                .iter()
                .zip(ids)
                .all(|(decision, id)| decision.id == *id)
    }
}

pub(super) fn claims(decision: &Bead, producer: &BeadId) -> bool {
    let Some(value) = decision.metadata.get(CANDIDATE_KEY) else {
        return false;
    };
    let decoded = match value {
        serde_json::Value::String(raw) => match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(%error, decision = %decision.id, "foreign malformed candidate has no attributable producer");
                return false;
            }
        },
        value => value.clone(),
    };
    decoded.get("producer").and_then(serde_json::Value::as_str) == Some(producer.as_str())
}

pub(super) async fn check_reuse<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &super::DecisionWaitJournal,
    source: &Bead,
    decision: &Bead,
) -> Result<(), LoopError> {
    let incompatible = |detail: &str| invalid(format!("decision {}: {detail}", decision.id));
    let producer = decision
        .parent
        .as_ref()
        .ok_or_else(|| incompatible("reused decision has no discovery parent"))?;
    let entry = journal
        .load(producer)?
        .ok_or_else(|| incompatible("foreign decision lacks prior canonical admission"))?;
    let owner = bd.show(producer).await?;
    let root = super::root(bd, source).await?;
    if entry.record.root != root
        || super::root(bd, &owner).await? != root
        || !entry.record.decisions.contains(&decision.id)
        || !journal.has_disposition(producer, &decision.id)?
    {
        return Err(incompatible("foreign decision lacks compatible checked disposition").into());
    }
    let context = entry
        .context
        .as_ref()
        .ok_or_else(|| incompatible("reused decision lacks checked graph context"))?;
    resolve(
        bd,
        &owner,
        &root,
        std::slice::from_ref(decision),
        Some(context),
    )
    .await?;
    Ok(())
}

fn candidate(
    producer: &BeadId,
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
        || proposal.producer != *producer
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
    source: &Bead,
) -> Result<(), LoopError> {
    let work = bd.show(id).await?;
    if work.id != *id
        || matches!(work.issue_type, IssueType::Molecule | IssueType::Decision)
        || (work.issue_type == IssueType::Epic
            && (source.id != *root || source.issue_type != IssueType::Epic || work.id != *root))
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
    if previous
        .is_some_and(|old| old.producer != source.id || old.producer_type != source.issue_type)
    {
        return Err(invalid("decision producer identity or kind changed").into());
    }
    let mut checked = Vec::with_capacity(decisions.len());
    for decision in decisions {
        let producer = decision
            .parent
            .as_ref()
            .ok_or_else(|| invalid("decision lacks native discovery parent"))?;
        let proposal = candidate(producer, root, decision)?;
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
            check_affected_work(bd, root, id, source).await?;
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
            producer: producer.clone(),
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
    Ok(Context {
        producer: source.id.clone(),
        producer_type: source.issue_type,
        decisions: checked,
    })
}
