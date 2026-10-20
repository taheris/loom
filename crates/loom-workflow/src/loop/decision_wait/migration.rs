//! Driver-owned, write-ahead migration of legacy discovery children.
//! No worker assertion or dotted ID spelling grants migration authority.

use super::{DecisionWaitError, DecisionWaitJournal, LoopError, graph, invalid};
use loom_driver::bd::{BdClient, Bead, CommandRunner, IssueType, Status, UpdateOpts};
use loom_driver::identifier::BeadId;
use loom_driver::state::RecordKind;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    version: u8,
    pub producer: BeadId,
    pub root: BeadId,
    before: Bead,
    pub original_candidate: Option<graph::Candidate>,
    pub original_affected: Vec<BeadId>,
    pub candidate: graph::Candidate,
    complete: bool,
}

fn read(journal: &DecisionWaitJournal, id: &BeadId) -> Result<Option<Plan>, LoopError> {
    let Some(plan) = journal
        .db()?
        .record::<Plan>(RecordKind::DecisionMigration, id.as_str())?
    else {
        return Ok(None);
    };
    if plan.version != 1
        || plan.before.id != *id
        || plan.before.issue_type != IssueType::Decision
        || plan.before.parent.as_ref() != Some(&plan.producer)
        || plan.producer == plan.root
        || plan.candidate.producer != plan.producer
        || plan.candidate.root != plan.root
        || plan.candidate.version != 1
    {
        return Err(invalid("invalid canonical decision migration").into());
    }
    Ok(Some(plan))
}

fn save(journal: &DecisionWaitJournal, plan: &Plan) -> Result<(), LoopError> {
    journal
        .db()?
        .put_record(RecordKind::DecisionMigration, plan.before.id.as_str(), plan)?;
    Ok(())
}

pub(super) fn completed(
    journal: &DecisionWaitJournal,
    id: &BeadId,
) -> Result<Option<Plan>, LoopError> {
    Ok(read(journal, id)?.filter(|plan| plan.complete))
}

pub(super) fn historical(
    journal: &DecisionWaitJournal,
    decision: &Bead,
    root: &BeadId,
) -> Result<bool, LoopError> {
    let Some(plan) = completed(journal, &decision.id)? else {
        return Ok(false);
    };
    if plan.root != *root
        || plan.before.status != Status::Closed
        || decision.status != Status::Closed
    {
        return Ok(false);
    }
    if decision.parent.as_ref() != Some(root)
        || graph::candidate(&plan.producer, root, decision)?.as_ref() != Some(&plan.candidate)
    {
        return Err(invalid("migrated historical decision changed provenance or graph").into());
    }
    Ok(true)
}

pub(super) fn permits_parent(
    journal: &DecisionWaitJournal,
    id: &BeadId,
    before: Option<&BeadId>,
    after: Option<&BeadId>,
) -> Result<bool, LoopError> {
    Ok(completed(journal, id)?
        .is_some_and(|plan| before == Some(&plan.producer) && after == Some(&plan.root)))
}

/// Called only after the complete contextual set and its original native graph
/// have been validated. Migration adds a producer prerequisite only for Waiting
/// (or a previously attributed legacy hold), never for an independent terminal.
pub(super) async fn apply<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    source: &Bead,
    root: &BeadId,
    decisions: &[Bead],
    waiting: bool,
) -> Result<Vec<Bead>, LoopError> {
    let locks = loom_driver::lock::LockManager::new(&journal.workspace)?;
    let _guard = locks
        .acquire_record_async(RecordKind::DecisionMigration, root, journal.clock.as_ref())
        .await?;
    let mut plans = Vec::new();
    for decision in decisions {
        if decision.parent.as_ref() == Some(root) {
            continue;
        }
        if completed(journal, &decision.id)?.is_some() {
            return Err(invalid("worker changed the parent of a migrated decision").into());
        }
        let producer = decision
            .parent
            .clone()
            .ok_or_else(|| invalid("legacy decision lacks producer"))?;
        let owner = bd.show(&producer).await?;
        if matches!(
            owner.issue_type,
            IssueType::Decision | IssueType::Molecule | IssueType::Epic
        ) || super::root(bd, &owner).await? != *root
        {
            return Err(invalid("legacy decision has incompatible producer or work root").into());
        }
        let mut affected = bd.blocking_dependents(&decision.id).await?;
        affected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        let original_candidate = graph::candidate(&producer, root, decision)?;
        let mut target = affected.clone();
        if waiting && producer == source.id && !target.contains(&source.id) {
            target.push(source.id.clone());
            target.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        }
        plans.push(Plan {
            version: 1,
            producer: producer.clone(),
            root: root.clone(),
            before: decision.clone(),
            original_candidate,
            original_affected: affected,
            candidate: graph::Candidate {
                version: 1,
                producer,
                root: root.clone(),
                affected_work: target,
            },
            complete: false,
        });
    }
    // Persist the whole batch before changing the first native record.
    for plan in &plans {
        if read(journal, &plan.before.id)?.is_none() {
            save(journal, plan)?;
        }
    }
    for plan in plans {
        resume(bd, journal, &plan.before.id).await?;
    }
    let mut current = Vec::with_capacity(decisions.len());
    for decision in decisions {
        current.push(bd.show(&decision.id).await?);
    }
    Ok(current)
}

async fn resume<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    id: &BeadId,
) -> Result<(), LoopError> {
    let mut plan = read(journal, id)?.ok_or_else(|| invalid("missing migration intent"))?;
    if plan.complete {
        return Ok(());
    }
    let owner = bd.show(&plan.producer).await?;
    if super::root(bd, &owner).await? != plan.root {
        return Err(invalid("migration producer changed work root").into());
    }
    let current = bd.show(id).await?;
    let mut expected = plan.before.clone();
    if current.parent.as_ref() == Some(&plan.root) {
        expected.parent = current.parent.clone();
    }
    let encoded = serde_json::to_string(&plan.candidate).map_err(DecisionWaitError::from)?;
    if current.metadata.get(graph::CANDIDATE_KEY)
        == Some(&serde_json::Value::String(encoded.clone()))
    {
        expected
            .metadata
            .insert(graph::CANDIDATE_KEY.into(), encoded.clone().into());
    }
    if current != expected {
        return Err(
            invalid("human or worker changed an interrupted migration; intent retained").into(),
        );
    }
    let mut edges = bd.blocking_dependents(id).await?;
    edges.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    if edges != plan.original_affected && edges != plan.candidate.affected_work {
        return Err(invalid("native graph changed during interrupted migration").into());
    }
    bd.update(
        id,
        UpdateOpts {
            set_metadata: vec![(graph::CANDIDATE_KEY.into(), encoded)],
            ..UpdateOpts::default()
        },
    )
    .await?;
    if current.parent.as_ref() != Some(&plan.root) {
        bd.reparent(id, &plan.root).await?;
    }
    for affected in &plan.candidate.affected_work {
        if !edges.contains(affected) {
            bd.dep_add(affected, id).await?;
        }
    }
    plan.complete = true;
    save(journal, &plan)
}

/// Recover before admission/scheduling, including a crash after reparenting but
/// before saving the transformed inventory. Conflicting edits fail closed.
pub(super) async fn recover<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
) -> Result<(), LoopError> {
    for (key, plan) in journal
        .db()?
        .records::<Plan>(RecordKind::DecisionMigration)?
    {
        if key != plan.before.id.as_str() {
            return Err(invalid("migration record key mismatch").into());
        }
        let locks = loom_driver::lock::LockManager::new(&journal.workspace)?;
        let _guard = locks
            .acquire_record_async(
                RecordKind::DecisionMigration,
                &plan.root,
                journal.clock.as_ref(),
            )
            .await?;
        resume(bd, journal, &plan.before.id).await?;
    }
    Ok(())
}
