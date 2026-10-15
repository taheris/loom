//! Driver-owned waits on discovery children, without native blocking edges.

use std::collections::HashSet;

use displaydoc::Display;
use loom_driver::bd::{BdClient, Bead, CommandRunner, IssueType, ListOpts, Status, UpdateOpts};
use loom_driver::identifier::BeadId;
use loom_protocol::output::Message;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{AgentOutcome, LoopError, SessionResult, WaitPrerequisites, classify_session};

mod journal;
pub use journal::DecisionWaitJournal;
use journal::{Entry, Phase};

const HOLD_LABEL: &str = "loom:decision-wait";
const HOLD_KEY: &str = "loom.decision_wait";

/// Errors validating or recovering an attributed discovery-child wait.
#[derive(Debug, Display, Error)]
pub enum DecisionWaitError {
    /// incompatible decision wait: {0}
    State(String),
    /// malformed decision-wait attribution
    Record(#[from] serde_json::Error),
}

/// Nonempty unresolved decisions checked against current native ownership.
/// Only contextual reconciliation can construct this proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionHold {
    active: Vec<BeadId>,
}

impl DecisionHold {
    #[must_use]
    pub const fn count(&self) -> usize {
        self.active.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    source: BeadId,
    root: BeadId,
    decisions: Vec<BeadId>,
}

fn invalid(detail: impl Into<String>) -> DecisionWaitError {
    DecisionWaitError::State(detail.into())
}

fn record(source: &Bead) -> Result<Option<Record>, DecisionWaitError> {
    let Some(value) = source.metadata.get(HOLD_KEY) else {
        return Ok(None);
    };
    let encoded = value
        .as_str()
        .ok_or_else(|| invalid("decision-wait projection must be a JSON string"))?;
    let record: Record = serde_json::from_str(encoded)?;
    let unique: HashSet<_> = record.decisions.iter().collect();
    if record.version != 1
        || record.source != source.id
        || unique.is_empty()
        || unique.len() != record.decisions.len()
    {
        return Err(invalid(
            "unsupported, mismatched, duplicate or empty projection",
        ));
    }
    Ok(Some(record))
}

async fn root<R: CommandRunner>(bd: &BdClient<R>, source: &Bead) -> Result<BeadId, LoopError> {
    if let Some(parent) = &source.parent {
        let parent = bd.show(parent).await?;
        if parent.id == source.id {
            return Err(invalid("self-parented waiting source").into());
        }
        Ok(parent.id)
    } else {
        Ok(source.id.clone())
    }
}

/// Read the complete current inventory before any update. Native projections
/// never establish attribution or extend the set of admitted references.
async fn inventory<R: CommandRunner>(
    bd: &BdClient<R>,
    source: &Bead,
    known: &HashSet<BeadId>,
) -> Result<Vec<Bead>, LoopError> {
    let candidates = bd
        .list(ListOpts {
            all: true,
            limit: Some(0),
            parent: Some(source.id.clone()),
            issue_type: Some(IssueType::Decision),
            ..ListOpts::default()
        })
        .await?;
    let mut ids = known.clone();
    ids.extend(candidates.into_iter().map(|bead| bead.id));
    let mut ids: Vec<_> = ids.into_iter().collect();
    ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    let mut decisions = Vec::with_capacity(ids.len());
    for id in ids {
        let decision = bd.show(&id).await?;
        if decision.id != id
            || decision.issue_type != IssueType::Decision
            || decision.parent.as_ref() != Some(&source.id)
        {
            return Err(invalid(format!(
                "{id} is not a native decision child of {}",
                source.id
            ))
            .into());
        }
        if decision.status != Status::Closed && !known.contains(&id) {
            return Err(invalid(format!("unreported current decision {id}")).into());
        }
        if matches!(decision.status, Status::Pinned | Status::Tombstone) {
            return Err(invalid(format!(
                "decision {id} has incompatible status {}",
                decision.status
            ))
            .into());
        }
        decisions.push(decision);
    }
    Ok(decisions)
}

async fn stage<R: CommandRunner>(bd: &BdClient<R>, decisions: &[Bead]) -> Result<(), LoopError> {
    for original in decisions
        .iter()
        .filter(|decision| decision.status != Status::Closed)
    {
        let decision = bd.show(&original.id).await?;
        if decision.id != original.id
            || decision.parent != original.parent
            || decision.issue_type != IssueType::Decision
        {
            return Err(invalid("decision ownership changed during staging").into());
        }
        if matches!(decision.status, Status::Pinned | Status::Tombstone) {
            return Err(invalid("decision status became incompatible during staging").into());
        }
        if decision.status == Status::Closed
            || decision.labels.iter().any(|label| {
                matches!(
                    label.as_str(),
                    "loom:clarify" | "loom:blocked" | "loom:infra"
                )
            })
            || matches!(decision.status, Status::Deferred | Status::InProgress)
        {
            continue;
        }
        crate::gate_clarify::apply_guarded_decision_report(bd, decision).await?;
    }
    Ok(())
}

/// Shared sequential/parallel Loop dispatch boundary for decision-child waits.
/// Native dependency waits remain a separate proof and no child becomes a
/// native blocker of its parent.
///
/// # Errors
/// Native or journal failures stop execution. Invalid contextual proposals
/// become fail-closed recovery without partial queue updates.
pub async fn reconcile_session<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    expected: &Bead,
    session: SessionResult,
    marker: Option<&Message>,
) -> Result<AgentOutcome, LoopError> {
    let reported: HashSet<_> = match &session {
        SessionResult::Decoded { output, .. } => output
            .as_ref()
            .as_ref()
            .map(|admitted| admitted.context().decisions())
            .unwrap_or_default(),
        _ => HashSet::new(),
    };
    let source_id = &expected.id;
    let outcome = classify_session(session, marker);
    if outcome != AgentOutcome::WaitingRequested {
        return super::validate_waiting_outcome(bd, source_id, outcome).await;
    }
    match admit(bd, journal, expected, &reported).await {
        Ok(Some(hold)) => Ok(AgentOutcome::Waiting {
            blockers: WaitPrerequisites::Decisions(hold),
        }),
        Ok(None) => super::validate_waiting_outcome(bd, source_id, outcome).await,
        Err(LoopError::DecisionWait(error)) => Ok(AgentOutcome::Failure {
            error: error.to_string(),
        }),
        Err(error) => Err(error),
    }
}

async fn admit<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    expected: &Bead,
    reported: &HashSet<BeadId>,
) -> Result<Option<DecisionHold>, LoopError> {
    let source_id = &expected.id;
    let _guard = journal.acquire(source_id).await?;
    let source = bd.show(source_id).await?;
    if source.id != *source_id
        || source.status != Status::Open
        || source.parent != expected.parent
        || source.issue_type != expected.issue_type
        || matches!(source.issue_type, IssueType::Decision | IssueType::Epic)
    {
        return Err(invalid(format!("waiting source {source_id} must remain open")).into());
    }
    let previous = journal.load(source_id)?;
    if previous
        .as_ref()
        .is_none_or(|entry| entry.phase == Phase::Released)
        && (source.metadata.contains_key(HOLD_KEY)
            || source
                .labels
                .iter()
                .any(|label| label.as_str() == HOLD_LABEL))
    {
        return Err(invalid("native hold assertions lack driver attribution").into());
    }
    let current_root = root(bd, &source).await?;
    let mut known = reported.clone();
    if let Some(previous) = &previous {
        known.extend(previous.record.decisions.iter().cloned());
        if previous.record.root != current_root || previous.phase == Phase::ReleasePending {
            return Err(invalid("source changed root or has an unfinished release").into());
        }
    }
    let decisions = inventory(bd, &source, &known).await?;
    let active = decisions
        .iter()
        .filter(|decision| decision.status != Status::Closed)
        .map(|decision| decision.id.clone())
        .collect::<Vec<_>>();
    if active.is_empty() {
        return Ok(None);
    }
    let attribution = Record {
        version: 1,
        source: source.id.clone(),
        root: current_root,
        decisions: decisions
            .iter()
            .map(|decision| decision.id.clone())
            .collect(),
    };
    if previous
        .as_ref()
        .is_some_and(|previous| previous.phase != Phase::Released && previous.record != attribution)
    {
        return Err(invalid("conflicting live driver hold inventory").into());
    }
    let mut entry = previous
        .filter(|entry| entry.phase != Phase::Released)
        .unwrap_or(Entry {
            record: attribution,
            phase: Phase::ProjectionPending,
        });
    // Journal intent must survive a crash before projection or queue staging.
    journal.save(&entry)?;
    project(bd, journal, &source, &mut entry).await?;
    stage(bd, &decisions).await?;
    Ok(Some(DecisionHold { active }))
}

/// Refresh before dispatch; recover partial staging and release only this
/// exact owned hold. Human status, history and independent holds stay intact.
///
/// # Errors
/// Missing, malformed or conflicting attribution is retained and rejected.
pub(super) async fn held<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    bead: &Bead,
) -> Result<bool, LoopError> {
    let _guard = journal.acquire(&bead.id).await?;
    let Some(mut entry) = journal.load(&bead.id)? else {
        if bead.labels.iter().any(|label| label.as_str() == HOLD_LABEL) {
            return Err(invalid("native hold label lacks driver attribution").into());
        }
        return Ok(false);
    };
    if entry.phase == Phase::Released {
        if bead.labels.iter().any(|label| label.as_str() == HOLD_LABEL) {
            return Err(
                invalid("native hold assertion conflicts with released attribution").into(),
            );
        }
        return Ok(false);
    }
    let source = bd.show(&bead.id).await?;
    if source.id != bead.id || entry.record.root != root(bd, &source).await? {
        return Err(invalid("source identity or work root changed while held").into());
    }
    if source.status != Status::Open
        || source.labels.iter().any(|label| {
            matches!(
                label.as_str(),
                "loom:clarify" | "loom:blocked" | "loom:infra"
            )
        })
    {
        return Ok(true);
    }
    let known = entry.record.decisions.iter().cloned().collect();
    let decisions = inventory(bd, &source, &known).await?;
    if decisions
        .iter()
        .any(|decision| decision.status != Status::Closed)
    {
        if entry.phase == Phase::ReleasePending {
            return Err(invalid("decision became unresolved during release").into());
        }
        project(bd, journal, &source, &mut entry).await?;
        stage(bd, &decisions).await?;
        return Ok(true);
    }
    let projected = record(&source)?;
    let labelled = source
        .labels
        .iter()
        .any(|label| label.as_str() == HOLD_LABEL);
    if projected.is_none() && !labelled && entry.phase == Phase::ReleasePending {
        entry.phase = Phase::Released;
        journal.save(&entry)?;
        return Ok(false);
    }
    project(bd, journal, &source, &mut entry).await?;
    entry.phase = Phase::ReleasePending;
    journal.save(&entry)?;
    bd.update(
        &source.id,
        UpdateOpts {
            if_status: Some(Status::Open),
            remove_labels: vec![HOLD_LABEL.into()],
            unset_metadata: vec![HOLD_KEY.into()],
            ..UpdateOpts::default()
        },
    )
    .await?;
    entry.phase = Phase::Released;
    journal.save(&entry)?;
    Ok(false)
}

async fn project<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    source: &Bead,
    entry: &mut Entry,
) -> Result<(), LoopError> {
    let projected = record(source)?;
    let labelled = source
        .labels
        .iter()
        .any(|label| label.as_str() == HOLD_LABEL);
    if projected.as_ref() == Some(&entry.record) && labelled {
        if entry.phase == Phase::ProjectionPending {
            entry.phase = Phase::Active;
            journal.save(entry)?;
        }
        return Ok(());
    }
    if projected.is_some() || labelled || entry.phase != Phase::ProjectionPending {
        return Err(invalid("missing or conflicting native hold projection").into());
    }
    bd.update(
        &source.id,
        UpdateOpts {
            if_status: Some(Status::Open),
            add_labels: vec![HOLD_LABEL.into()],
            set_metadata: vec![(
                HOLD_KEY.into(),
                serde_json::to_string(&entry.record).map_err(DecisionWaitError::from)?,
            )],
            ..UpdateOpts::default()
        },
    )
    .await?;
    entry.phase = Phase::Active;
    journal.save(entry)?;
    Ok(())
}

#[cfg(test)]
mod tests;
