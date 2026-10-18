//! Contextual decision admission and driver-attributed native prerequisite waits.

use std::collections::HashSet;

use displaydoc::Display;
use loom_driver::bd::LabelExt;
use loom_driver::bd::{BdClient, Bead, CommandRunner, IssueType, ListOpts, Status};
use loom_driver::identifier::BeadId;
use loom_protocol::output::Message;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{AgentOutcome, LoopError, SessionResult, classify_session};

mod disposition;
mod graph;
pub use disposition::AdmissionTracker;
mod inventory;
mod journal;
mod migration;
mod native;
pub use inventory::{admit_integrity, admit_todo};
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
    /// affected-work acceptance could not be independently resolved
    Acceptance(#[from] super::acceptance::Error),
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
    let mut current = source.clone();
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(current.id.clone()) {
            return Err(invalid("cyclic decision producer ownership").into());
        }
        if current.issue_type == IssueType::Epic {
            return Ok(current.id);
        }
        let Some(parent_id) = current.parent else {
            return Ok(current.id);
        };
        current = bd.show(&parent_id).await?;
        if current.id != parent_id {
            return Err(invalid("ambiguous decision producer ownership").into());
        }
    }
}

/// Read the complete current inventory before any update. Native projections
/// never establish attribution or extend the set of admitted references.
async fn inventory<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    source: &Bead,
    known: &HashSet<BeadId>,
) -> Result<Vec<Bead>, LoopError> {
    let candidates = bd
        .list(ListOpts {
            all: true,
            limit: Some(0),
            parent: None,
            issue_type: Some(IssueType::Decision),
            ..ListOpts::default()
        })
        .await?;
    let mut ids = known.clone();
    ids.extend(
        candidates
            .into_iter()
            .filter(|bead| {
                bead.parent.as_ref() == Some(&source.id) || graph::claims(bead, &source.id)
            })
            .map(|bead| bead.id),
    );
    let mut ids: Vec<_> = ids.into_iter().collect();
    ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    let mut decisions = Vec::with_capacity(ids.len());
    let mut unreported = Vec::new();
    for id in ids {
        let decision = bd.show(&id).await?;
        if decision.id != id || decision.issue_type != IssueType::Decision {
            return Err(invalid(format!(
                "{id} is not a native decision child of {}",
                source.id
            ))
            .into());
        }
        let current_root = root(bd, source).await?;
        let producer = graph::provenance(&decision, &current_root)?;
        if producer != source.id {
            let owner = bd.show(&producer).await?;
            let legacy_closed = decision.status == Status::Closed
                && decision.parent.as_ref() == Some(&producer)
                && root(bd, &owner).await? == current_root;
            if !legacy_closed {
                graph::check_reuse(bd, journal, source, &decision).await?;
            }
        }
        if decision.status != Status::Closed && !known.contains(&id) {
            unreported.push(id.to_string());
        }
        if matches!(decision.status, Status::Pinned | Status::Tombstone) {
            return Err(invalid(format!(
                "decision {id} has incompatible status {}",
                decision.status
            ))
            .into());
        }
        // Closed legacy records are migrated and preserved, never reopened.
        decisions.push(decision);
    }
    if !unreported.is_empty() {
        return Err(invalid(format!(
            "unreported current decisions: {}",
            unreported.join(", ")
        ))
        .into());
    }
    Ok(decisions)
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
    let admitted = matches!(&session, SessionResult::Decoded { output, outcome }
        if outcome.exit_code == 0 && output.is_ok());
    let source_id = &expected.id;
    let outcome = classify_session(session, marker);
    if outcome != AgentOutcome::WaitingRequested {
        if admitted {
            match inventory::admit(bd, journal, expected, &reported).await {
                Ok(()) => {}
                Err(LoopError::DecisionWait(error)) => {
                    return Ok(AgentOutcome::Failure {
                        error: error.to_string(),
                    });
                }
                Err(error) => return Err(error),
            }
        }
        return super::validate_waiting_outcome(bd, source_id, outcome).await;
    }
    match inventory::admit_waiting(bd, journal, expected, &reported).await {
        Ok(()) => journal.reconcile_native_wait(bd, expected, outcome).await,
        Err(LoopError::DecisionWait(error)) => Ok(AgentOutcome::Failure {
            error: error.to_string(),
        }),
        Err(error) => Err(error),
    }
}

/// Refresh canonical admissions and migrate legacy holds before selection.
pub(super) async fn held<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    bead: &Bead,
) -> Result<bool, LoopError> {
    migration::recover(bd, journal).await?;
    if let Some(mut entry) = journal.load(&bead.id)? {
        if entry.phase.holds_source() || entry.phase == Phase::Released {
            let source = bd.show(&bead.id).await?;
            if !matches!(source.status, Status::Open | Status::InProgress)
                || source
                    .labels
                    .iter()
                    .any(|label| label.is_blocked() || label.is_clarify() || label.is_infra())
            {
                return Err(invalid(
                    "legacy wait has independent human state; migration requires review",
                )
                .into());
            }
            inventory::admit_waiting(bd, journal, &source, &HashSet::new()).await?;
        } else {
            let _guard = journal.acquire(&bead.id).await?;
            inventory::recover(bd, journal, bead, &mut entry).await?;
        }
    } else if bead.metadata.contains_key(HOLD_KEY)
        || bead.labels.iter().any(|label| label.as_str() == HOLD_LABEL)
    {
        return Err(invalid("native hold assertion lacks driver attribution").into());
    } else {
        return journal.native_held(bd, bead).await;
    }
    let source = bd.show(&bead.id).await?;
    journal.native_held(bd, &source).await
}

#[cfg(test)]
mod tests;
