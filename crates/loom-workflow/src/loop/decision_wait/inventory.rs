//! Decision accounting independent of the producer's terminal proposal.

use std::collections::HashSet;

use loom_driver::bd::{BdClient, Bead, CommandRunner, IssueType, Status};
use loom_driver::identifier::BeadId;

use super::{
    DecisionWaitJournal, Entry, HOLD_KEY, HOLD_LABEL, LoopError, Phase, Record, disposition::stage,
    invalid, root,
};

#[derive(Clone, Copy)]
enum Producer<'a> {
    Task,
    Waiting,
    Todo,
    Integrity {
        findings: &'a [(BeadId, loom_protocol::gate::Finding)],
        workspace: &'a std::path::Path,
    },
}

/// Admit a driver-resolved integrity batch through the canonical inventory lifecycle.
/// Native affected-work extensions require freshly resolved matching finding goals.
///
/// # Errors
/// Rejects omissions, changed provenance and incompatible acceptance before queue effects.
pub async fn admit_integrity<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    expected: &Bead,
    findings: &[(BeadId, loom_protocol::gate::Finding)],
    workspace: &std::path::Path,
) -> Result<(), LoopError> {
    let reported = findings.iter().map(|(id, _)| id.clone()).collect();
    let journal = journal
        .clone()
        .with_source_route(crate::gate_clarify::ClarifySourceRoute::ReviewVerdict);
    admit_for(
        bd,
        &journal,
        expected,
        &reported,
        Producer::Integrity {
            findings,
            workspace,
        },
    )
    .await
}

/// Admit Todo's reported union against its pending native work epic.
///
/// # Errors
/// Rejects incomplete inventories and incompatible ownership before queue effects.
pub async fn admit_todo<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    expected: &Bead,
    reported: &HashSet<BeadId>,
) -> Result<HashSet<BeadId>, LoopError> {
    let journal = journal
        .clone()
        .with_source_route(crate::gate_clarify::ClarifySourceRoute::TodoMarker);
    admit_for(bd, &journal, expected, reported, Producer::Todo).await?;
    Ok(journal
        .load(&expected.id)?
        .map_or_else(HashSet::new, |entry| {
            entry.record.decisions.into_iter().collect()
        }))
}

/// Admit the entire contextual set before staging any sibling. Unlike a
/// discovery-child hold, this disposition never projects a wait onto the source.
pub(super) async fn admit<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    expected: &Bead,
    reported: &HashSet<BeadId>,
) -> Result<(), LoopError> {
    admit_for(bd, journal, expected, reported, Producer::Task).await
}

pub(super) async fn admit_waiting<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    expected: &Bead,
    reported: &HashSet<BeadId>,
) -> Result<(), LoopError> {
    admit_for(bd, journal, expected, reported, Producer::Waiting).await
}

async fn admit_for<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    expected: &Bead,
    reported: &HashSet<BeadId>,
    producer: Producer<'_>,
) -> Result<(), LoopError> {
    super::migration::recover(bd, journal).await?;
    let _guard = journal.acquire(&expected.id).await?;
    let source = bd.show(&expected.id).await?;
    if source.id != expected.id
        || source.parent != expected.parent
        || source.issue_type != expected.issue_type
        || !(matches!(
            source.status,
            Status::Open | Status::InProgress | Status::Closed
        ) || (matches!(producer, Producer::Waiting) && source.status == Status::Blocked))
        || match producer {
            Producer::Task | Producer::Waiting => {
                matches!(source.issue_type, IssueType::Decision | IssueType::Epic)
            }
            Producer::Integrity { .. } => {
                source.issue_type != IssueType::Epic || source.status != Status::Open
            }
            Producer::Todo => {
                source.issue_type != IssueType::Epic
                    || !source
                        .labels
                        .iter()
                        .any(|label| label.as_str() == "loom:todo")
            }
        }
    {
        return Err(invalid("decision producer identity or status changed").into());
    }
    let previous = journal.load(&source.id)?;
    let legacy_wait = previous
        .as_ref()
        .is_some_and(|entry| entry.phase.holds_source());
    if legacy_wait {
        let entry = previous
            .as_ref()
            .ok_or_else(|| invalid("legacy hold disappeared"))?;
        let projected = super::record(&source)?;
        let labelled = source
            .labels
            .iter()
            .any(|label| label.as_str() == HOLD_LABEL);
        let absent_pending = projected.is_none()
            && !labelled
            && matches!(
                entry.phase,
                Phase::ProjectionPending | Phase::ReleasePending
            );
        if !absent_pending && (projected.as_ref() != Some(&entry.record) || !labelled) {
            return Err(
                invalid("legacy hold projection conflicts with canonical attribution").into(),
            );
        }
    } else {
        check_projection(&source)?;
    }
    if let Some(previous) = &previous
        && previous.phase == Phase::InventoryAdmitted
        && previous.context.is_some()
        && !journal.complete_dispositions(previous)?
    {
        return Err(invalid("completed inventory lost canonical decision dispositions").into());
    }
    let current_root = root(bd, &source).await?;
    let mut known = reported.clone();
    if let Some(previous) = &previous {
        if previous.record.root != current_root {
            return Err(invalid("decision producer changed work root").into());
        }
        known.extend(previous.record.decisions.iter().cloned());
    }
    let decisions = super::inventory(bd, journal, &source, &known).await?;
    let previous_context = previous.as_ref().and_then(|entry| entry.context.as_ref());
    let context = match producer {
        Producer::Integrity {
            findings,
            workspace,
        } => {
            super::graph::resolve_integrity(
                bd,
                &source,
                &current_root,
                &decisions,
                previous_context,
                findings,
                workspace,
            )
            .await?
        }
        Producer::Task | Producer::Waiting
            if legacy_wait
                || (source.status == Status::Blocked
                    && previous.as_ref().is_some_and(|entry| entry.native_wait)) =>
        {
            super::graph::resolve(bd, &source, &current_root, &decisions, previous_context).await?
        }
        Producer::Task | Producer::Waiting => {
            super::graph::readmit_reuse(
                bd,
                journal,
                &source,
                &current_root,
                &decisions,
                previous_context,
            )
            .await?
        }
        Producer::Todo => {
            super::graph::resolve(bd, &source, &current_root, &decisions, previous_context).await?
        }
    };
    if decisions.is_empty() {
        return Ok(());
    }
    let mut entry = Entry {
        record: Record {
            version: 1,
            source: source.id.clone(),
            root: current_root.clone(),
            decisions: decisions
                .iter()
                .map(|decision| decision.id.clone())
                .collect(),
        },
        phase: Phase::InventoryPending,
        native_wait: legacy_wait || matches!(producer, Producer::Waiting),
        context: Some(context),
    };
    // Durable intent precedes every queue mutation. A restarted selector must
    // finish staging rather than redispatch the producer past a partial batch.
    // Keep legacy hold authority until its exact projection is removed. The
    // migration plans themselves durably block selection during this interval.
    if !legacy_wait {
        journal.save(&entry)?;
    }
    let decisions = super::migration::apply(
        bd,
        journal,
        &source,
        &current_root,
        &decisions,
        legacy_wait || matches!(producer, Producer::Waiting),
    )
    .await?;
    let mut context = entry
        .context
        .take()
        .ok_or_else(|| invalid("missing admission graph"))?;
    context.apply_migrations(journal)?;
    entry.context =
        Some(super::graph::resolve(bd, &source, &current_root, &decisions, Some(&context)).await?);
    if legacy_wait {
        entry.phase = Phase::ReleasePending;
        journal.save(&entry)?;
        bd.update(
            &source.id,
            loom_driver::bd::UpdateOpts {
                if_status: Some(source.status),
                remove_labels: vec![HOLD_LABEL.into()],
                unset_metadata: vec![HOLD_KEY.into()],
                ..loom_driver::bd::UpdateOpts::default()
            },
        )
        .await?;
    }
    entry.phase = Phase::InventoryPending;
    journal.save(&entry)?;
    stage(bd, journal, &entry.record.source, &decisions).await?;
    entry.phase = Phase::InventoryAdmitted;
    journal.save(&entry)?;
    journal.park_native_affected(bd, &entry, &decisions).await
}

/// Publication reads the current inventory and compares the recorded graph;
/// this read-only path cannot repair missing or stale admission authority.
pub(super) async fn audit<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    entry: &Entry,
) -> Result<(), LoopError> {
    let source = bd.show(&entry.record.source).await?;
    if source.id != entry.record.source
        || source.issue_type == IssueType::Decision
        || root(bd, &source).await? != entry.record.root
    {
        return Err(
            invalid("decision producer identity or root changed before publication").into(),
        );
    }
    check_projection(&source)?;
    if !journal.complete_dispositions(entry)? {
        return Err(
            invalid("inventory admission lacks completed canonical decision dispositions").into(),
        );
    }
    let context = entry
        .context
        .as_ref()
        .ok_or_else(|| invalid("inventory admission needs graph recovery before publication"))?;
    let known = entry.record.decisions.iter().cloned().collect();
    let decisions = super::inventory(bd, journal, &source, &known).await?;
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
        || source.issue_type == IssueType::Decision
        || root(bd, &source).await? != entry.record.root
    {
        return Err(invalid("decision producer identity or work root changed on recovery").into());
    }
    check_projection(&source)?;
    let known = entry.record.decisions.iter().cloned().collect();
    let decisions = super::inventory(bd, journal, &source, &known).await?;
    super::graph::resolve(
        bd,
        &source,
        &entry.record.root,
        &decisions,
        entry.context.as_ref(),
    )
    .await?;
    let decisions = super::migration::apply(
        bd,
        journal,
        &source,
        &entry.record.root,
        &decisions,
        entry.native_wait,
    )
    .await?;
    if let Some(context) = &mut entry.context {
        context.apply_migrations(journal)?;
    }
    let context = super::graph::resolve(
        bd,
        &source,
        &entry.record.root,
        &decisions,
        entry.context.as_ref(),
    )
    .await?;
    let legacy_context = entry.context.is_none();
    if legacy_context {
        entry.record.decisions = decisions
            .iter()
            .map(|decision| decision.id.clone())
            .collect();
        entry.context = Some(context);
        journal.save(entry)?;
    }
    if entry.phase == Phase::InventoryAdmitted
        && !legacy_context
        && !journal.complete_dispositions(entry)?
    {
        return Err(invalid("completed inventory lost canonical decision dispositions").into());
    }
    if entry.phase == Phase::InventoryPending || legacy_context {
        entry.phase = Phase::InventoryPending;
        journal.save(entry)?;
        stage(bd, journal, &entry.record.source, &decisions).await?;
        entry.phase = Phase::InventoryAdmitted;
        journal.save(entry)?;
    }
    journal.park_native_affected(bd, entry, &decisions).await?;
    Ok(false)
}
