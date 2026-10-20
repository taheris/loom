//! Durable decision effects and replayable routing breadcrumbs.

use loom_driver::state::RecordKind;
use std::collections::HashSet;
use std::io::Write;
use std::sync::{Arc, Mutex};

use loom_driver::bd::{BdClient, Bead, CommandRunner, IssueType, Status};
use loom_driver::identifier::BeadId;
use loom_events::identifier::SessionId;
use loom_events::{AgentEvent, EnvelopeBuilder, SessionScope, Source};
use serde::{Deserialize, Serialize};

use super::{DecisionWaitError, DecisionWaitJournal, LoopError, invalid};
use crate::gate_clarify::{
    ClarifyApplyOutcome, ClarifyApplyReport, ClarifyRouteContext, ClarifySourceRoute,
    apply_guarded_decision_report, inspect_decision,
};

/// Invocation-local accounting, shared by trusted dispatchers, never worker output.
#[derive(Debug, Clone, Default)]
pub struct AdmissionTracker(Arc<Mutex<HashSet<BeadId>>>);

impl AdmissionTracker {
    /// Distinct unresolved identities admitted during this invocation.
    ///
    /// # Errors
    /// A poisoned accounting lock is retained as a visible workflow failure.
    pub fn ids(&self) -> Result<HashSet<BeadId>, LoopError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| invalid("decision accounting lock poisoned"))?
            .clone())
    }

    fn record(&self, decision: &Bead) -> Result<(), LoopError> {
        if decision.status != Status::Closed {
            self.0
                .lock()
                .map_err(|_| invalid("decision accounting lock poisoned"))?
                .insert(decision.id.clone());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum Effect {
    Planned,
    Applied,
    Preserved,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u8,
    producer: BeadId,
    snapshot: Bead,
    effect: Effect,
    event_created_at_ms: i64,
    source_route: ClarifySourceRoute,
    report: Option<ClarifyApplyReport>,
}

pub(super) fn reusable(
    journal: &DecisionWaitJournal,
    producer: &BeadId,
    decision: &BeadId,
    parent: &BeadId,
) -> Result<bool, LoopError> {
    let key = blake3::hash(format!("{producer}\n{decision}").as_bytes()).to_hex();
    let Some(receipt) = journal
        .db()?
        .record::<Receipt>(RecordKind::DecisionDisposition, key.as_ref())?
    else {
        return Ok(false);
    };
    receipt.validate(producer, decision)?;
    if receipt.snapshot.parent.as_ref() != Some(parent)
        && !super::migration::permits_parent(
            journal,
            decision,
            receipt.snapshot.parent.as_ref(),
            Some(parent),
        )?
    {
        return Err(invalid("decision disposition disagrees with checked discovery parent").into());
    }
    Ok(!matches!(receipt.effect, Effect::Planned))
}

impl Receipt {
    fn validate(&self, producer: &BeadId, decision: &BeadId) -> Result<(), LoopError> {
        if self.version != 1
            || self.producer != *producer
            || self.snapshot.id != *decision
            || self.snapshot.issue_type != IssueType::Decision
            || self.snapshot.parent.is_none()
            || (matches!(self.effect, Effect::Planned | Effect::Applied) && self.report.is_none())
            || self
                .report
                .as_ref()
                .is_some_and(|report| *report != inspect_decision(&self.snapshot))
        {
            return Err(invalid("mismatched canonical decision disposition").into());
        }
        Ok(())
    }
}

fn independent(decision: &Bead) -> bool {
    matches!(
        decision.status,
        Status::Closed | Status::Deferred | Status::InProgress
    ) || decision.labels.iter().any(|label| {
        matches!(
            label.as_str(),
            "loom:clarify" | "loom:blocked" | "loom:infra"
        )
    })
}

fn save(journal: &DecisionWaitJournal, key: &str, receipt: &Receipt) -> Result<(), LoopError> {
    journal
        .db()?
        .put_record(RecordKind::DecisionDisposition, key, receipt)?;
    Ok(())
}

pub(super) async fn stage<R: CommandRunner>(
    bd: &BdClient<R>,
    journal: &DecisionWaitJournal,
    producer: &BeadId,
    decisions: &[Bead],
) -> Result<(), LoopError> {
    let entry = journal
        .load(producer)?
        .ok_or_else(|| invalid("decision effects lack canonical inventory"))?;
    if entry.context.is_none() {
        return Err(invalid("decision effects lack checked graph context").into());
    }
    for original in decisions {
        if !entry.record.decisions.contains(&original.id) {
            return Err(invalid("decision effect outside checked inventory").into());
        }
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
        let key = blake3::hash(format!("{producer}\n{}", decision.id).as_bytes())
            .to_hex()
            .to_string();
        let mut receipt: Receipt = if let Some(receipt) = journal
            .db()?
            .record(RecordKind::DecisionDisposition, &key)?
        {
            receipt
        } else {
            let receipt = Receipt {
                version: 1,
                producer: producer.clone(),
                snapshot: decision.clone(),
                event_created_at_ms: i64::try_from(
                    journal
                        .clock
                        .wall_now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|_| invalid("decision event clock precedes Unix epoch"))?
                        .as_millis(),
                )
                .map_err(|_| invalid("decision event clock exceeds timestamp range"))?,
                source_route: journal.source_route,
                effect: if independent(&decision) {
                    Effect::Preserved
                } else {
                    Effect::Planned
                },
                report: if independent(&decision) {
                    None
                } else {
                    Some(inspect_decision(&decision))
                },
            };
            save(journal, &key, &receipt)?;
            receipt
        };
        receipt.validate(producer, &decision.id)?;
        if receipt.snapshot.parent != decision.parent
            && !super::migration::permits_parent(
                journal,
                &decision.id,
                receipt.snapshot.parent.as_ref(),
                decision.parent.as_ref(),
            )?
        {
            return Err(invalid("mismatched decision disposition").into());
        }
        if matches!(receipt.effect, Effect::Planned) {
            let report = receipt
                .report
                .as_ref()
                .ok_or_else(|| invalid("planned decision lacks brief disposition"))?;
            let label = match report.outcome {
                ClarifyApplyOutcome::Clarify => "loom:clarify",
                ClarifyApplyOutcome::BlockedClarifyWithoutOptions => "loom:blocked",
            };
            if decision.status == Status::Blocked
                && decision
                    .labels
                    .iter()
                    .any(|actual| actual.as_str() == label)
            {
                receipt.effect = Effect::Applied;
            } else if independent(&decision) {
                receipt.effect = Effect::Preserved;
            } else {
                if inspect_decision(&decision) != *report {
                    return Err(invalid("decision brief changed during interrupted staging").into());
                }
                let applied = apply_guarded_decision_report(bd, decision.clone()).await?;
                if applied != *report {
                    return Err(
                        invalid("decision brief application disagreed with checked plan").into(),
                    );
                }
                receipt.effect = Effect::Applied;
            }
            save(journal, &key, &receipt)?;
        }
        if matches!(receipt.effect, Effect::Applied) {
            deliver(journal, &key, &receipt)?;
        }
        journal.admissions.record(&bd.show(&decision.id).await?)?;
    }
    Ok(())
}

fn deliver(journal: &DecisionWaitJournal, key: &str, receipt: &Receipt) -> Result<(), LoopError> {
    let report = receipt
        .report
        .as_ref()
        .ok_or_else(|| invalid("applied decision lacks brief disposition"))?;
    let directory = journal.workspace.join(".loom/logs/decisions");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}-{key}.jsonl", receipt.snapshot.id));
    let existing = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let context = ClarifyRouteContext {
        source_route: receipt.source_route,
        identity: "LOOM_CLARIFY".into(),
        gate_log_path: None,
    };
    let session = SessionId::generated(format!("decision-{key}"));
    let timestamp = receipt.event_created_at_ms;
    let mut builder = EnvelopeBuilder::new(
        SessionScope::bead(session, receipt.snapshot.id.clone(), None, 0),
        Source::Driver,
        move || timestamp,
    );
    let mut expected = Vec::new();
    for mut payload in report.routing_events(&receipt.snapshot.id, &context) {
        payload.payload["admission_key"] = key.into();
        payload.payload["bead_id"] =
            serde_json::to_value(&receipt.snapshot.id).map_err(DecisionWaitError::from)?;
        payload.payload["producer"] =
            serde_json::to_value(&receipt.producer).map_err(DecisionWaitError::from)?;
        let event = AgentEvent::from_driver_event(payload, builder.build());
        let mut canonical = serde_json::to_value(&event).map_err(DecisionWaitError::from)?;
        canonical.sort_all_objects();
        serde_json::to_writer(&mut expected, &canonical).map_err(DecisionWaitError::from)?;
        expected.push(b'\n');
    }
    if !expected.starts_with(&existing) {
        return Err(invalid("decision event log disagrees with canonical disposition").into());
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(&expected[existing.len()..])?;
    file.sync_all()?;
    for directory in directory.ancestors().take(3) {
        std::fs::File::open(directory)?.sync_all()?;
    }
    Ok(())
}
