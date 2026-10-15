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
    if let Some(previous) = previous {
        if previous.record.root != current_root {
            return Err(invalid("decision producer changed work root").into());
        }
        known.extend(previous.record.decisions);
    }
    let decisions = super::inventory(bd, &source, &known).await?;
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
    };
    // Durable intent precedes every queue mutation. A restarted selector must
    // finish staging rather than redispatch the producer past a partial batch.
    journal.save(&entry)?;
    stage(bd, &decisions).await?;
    entry.phase = Phase::InventoryAdmitted;
    journal.save(&entry)
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
    if source.id != bead.id || root(bd, &source).await? != entry.record.root {
        return Err(invalid("decision producer identity or work root changed on recovery").into());
    }
    check_projection(&source)?;
    let known = entry.record.decisions.iter().cloned().collect();
    let decisions = super::inventory(bd, &source, &known).await?;
    if entry.phase == Phase::InventoryPending {
        stage(bd, &decisions).await?;
        entry.phase = Phase::InventoryAdmitted;
        journal.save(entry)?;
    }
    // Native prerequisites and independent holds remain the scheduler's job.
    // Reporting decisions alone never establishes a wait for this producer.
    Ok(false)
}
