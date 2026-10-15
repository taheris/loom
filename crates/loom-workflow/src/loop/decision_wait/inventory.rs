//! Decision accounting independent of the producer's terminal proposal.

use std::collections::HashSet;

use loom_driver::bd::{BdClient, Bead, CommandRunner, IssueType, Status};
use loom_driver::identifier::BeadId;

use super::{
    DecisionWaitJournal, Entry, HOLD_KEY, HOLD_LABEL, LoopError, Phase, Record, invalid, root,
    stage,
};

/// Admit the entire contextual set before staging any sibling. Unlike a
/// discovery-child hold, this disposition never projects a wait onto the source.
pub(super) async fn admit<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    expected: &Bead,
    reported: &HashSet<BeadId>,
) -> Result<(), LoopError> {
    let _guard = journal.acquire(&expected.id).await?;
    let source = bd.show(&expected.id).await?;
    if source.id != expected.id
        || source.parent != expected.parent
        || source.issue_type != expected.issue_type
        || !matches!(source.status, Status::Open | Status::Closed)
        || matches!(source.issue_type, IssueType::Decision | IssueType::Epic)
    {
        return Err(invalid("decision producer identity or status changed").into());
    }
    let previous = journal.load(&source.id)?;
    if previous
        .as_ref()
        .is_some_and(|entry| entry.phase.holds_source())
    {
        return Err(invalid("source still has an unreconciled discovery-child hold").into());
    }
    check_projection(&source)?;
    let current_root = root(bd, &source).await?;
    let mut known = reported.clone();
    if let Some(previous) = &previous {
        if previous.record.root != current_root {
            return Err(invalid("decision producer changed work root").into());
        }
        known.extend(previous.record.decisions.iter().cloned());
    }
    let decisions = super::inventory(bd, &source, &known).await?;
    let context = super::graph::resolve(
        bd,
        &source,
        &current_root,
        &decisions,
        previous.as_ref().and_then(|entry| entry.context.as_ref()),
    )
    .await?;
    if decisions.is_empty() {
        return Ok(());
    }
    let mut entry = Entry {
        record: Record {
            version: 1,
            source: source.id,
            root: current_root,
            decisions: decisions
                .iter()
                .map(|decision| decision.id.clone())
                .collect(),
        },
        phase: Phase::InventoryPending,
        context: Some(context),
    };
    // Durable intent precedes every queue mutation. A restarted selector must
    // finish staging rather than redispatch the producer past a partial batch.
    journal.save(&entry)?;
    stage(bd, &decisions).await?;
    entry.phase = Phase::InventoryAdmitted;
    journal.save(&entry)
}

/// Publication reads the current inventory and compares the recorded graph;
/// this read-only path cannot repair missing or stale admission authority.
pub(super) async fn audit<R: CommandRunner>(
    bd: &BdClient<R>,
    entry: &Entry,
) -> Result<(), LoopError> {
    let source = bd.show(&entry.record.source).await?;
    if source.id != entry.record.source
        || matches!(source.issue_type, IssueType::Decision | IssueType::Epic)
        || root(bd, &source).await? != entry.record.root
    {
        return Err(
            invalid("decision producer identity or root changed before publication").into(),
        );
    }
    check_projection(&source)?;
    let context = entry
        .context
        .as_ref()
        .ok_or_else(|| invalid("inventory admission needs graph recovery before publication"))?;
    let known = entry.record.decisions.iter().cloned().collect();
    let decisions = super::inventory(bd, &source, &known).await?;
    super::graph::resolve(bd, &source, &entry.record.root, &decisions, Some(context)).await?;
    Ok(())
}

fn check_projection(source: &Bead) -> Result<(), LoopError> {
    if source.metadata.contains_key(HOLD_KEY)
        || source
            .labels
            .iter()
            .any(|label| label.as_str() == HOLD_LABEL)
    {
        return Err(
            invalid("inventory disposition cannot authorize a native hold projection").into(),
        );
    }
    Ok(())
}

pub(super) async fn recover<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    bead: &Bead,
    entry: &mut Entry,
) -> Result<bool, LoopError> {
    let source = bd.show(&bead.id).await?;
    if source.id != bead.id
        || matches!(source.issue_type, IssueType::Decision | IssueType::Epic)
        || root(bd, &source).await? != entry.record.root
    {
        return Err(invalid("decision producer identity or work root changed on recovery").into());
    }
    check_projection(&source)?;
    let known = entry.record.decisions.iter().cloned().collect();
    let decisions = super::inventory(bd, &source, &known).await?;
    let context = super::graph::resolve(
        bd,
        &source,
        &entry.record.root,
        &decisions,
        entry.context.as_ref(),
    )
    .await?;
    if entry.context.is_none() {
        entry.context = Some(context);
        journal.save(entry)?;
    }
    if entry.phase == Phase::InventoryPending {
        stage(bd, &decisions).await?;
        entry.phase = Phase::InventoryAdmitted;
        journal.save(entry)?;
    }
    // Native prerequisites and independent holds remain the scheduler's job.
    // Reporting decisions alone never establishes a wait for this producer.
    Ok(false)
}
