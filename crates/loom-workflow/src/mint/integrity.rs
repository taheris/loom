//! Integrity cap decisions are independent of remediation dedup and acceptance.

use std::collections::BTreeMap;

use displaydoc::Display;
use loom_driver::bd::{
    BdClient, BdError, Bead, CommandRunner, CreateOpts, IssueType, ListOpts, Status, UpdateOpts,
};
use loom_driver::identifier::BeadId;
use loom_events::DriverEventPayload;
use loom_gate::IntegrityFinding;
use loom_protocol::gate::{
    DispatchScope, Finding, FindingParseError, FindingValidator, RawFinding,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::gate_clarify::{
    ClarifyApplyOutcome, ClarifyRouteContext, ClarifySourceRoute, apply_clarify_or_blocked_report,
};
use crate::review::default_profile_for_spec;

const RECORD_KEY: &str = "loom.integrity_decision";
const LABEL_PREFIX: &str = "decision:integrity:";

#[derive(Debug, Display, Error)]
pub enum Error {
    /// Beads operation failed while materializing integrity decisions
    Bd(#[from] BdError),
    /// integrity finding does not resolve in the current workspace
    Finding(#[from] FindingParseError),
    /// integrity decision provenance could not be encoded or decoded
    Record(#[from] serde_json::Error),
    /// integrity decision graph is invalid: {0}
    Graph(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Disposition {
    Candidate,
    Clarify,
    BriefRepair,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    work_root: BeadId,
    finding: RawFinding,
    disposition: Disposition,
}

fn decision_label(finding: &Finding) -> String {
    format!("{LABEL_PREFIX}{}", finding.hash())
}

fn check_record(
    bead: &Bead,
    parent: &BeadId,
    finding: &Finding,
    validator: &(impl FindingValidator + Sync + ?Sized),
) -> Result<Record, Error> {
    let value = bead
        .metadata
        .get(RECORD_KEY)
        .ok_or_else(|| Error::Graph(format!("decision {} has no driver provenance", bead.id)))?;
    let encoded = value.as_str().ok_or_else(|| {
        Error::Graph(format!(
            "decision {} has malformed driver provenance",
            bead.id
        ))
    })?;
    let record: Record = serde_json::from_str(encoded)?;
    let recorded = record
        .finding
        .clone()
        .resolve(DispatchScope::PushGate, validator)?;
    if bead.parent.as_ref() != Some(parent)
        || record.work_root != *parent
        || recorded.id() != finding.id()
        || (record.disposition == Disposition::Candidate
            && (bead.status != Status::Blocked
                || bead
                    .labels
                    .iter()
                    .any(|label| matches!(label.as_str(), "loom:blocked" | "loom:infra"))))
    {
        return Err(Error::Graph(format!(
            "decision {} has incompatible finding/root/disposition",
            bead.id
        )));
    }
    Ok(record)
}

/// Admit per-finding cap decisions without implementation or publication effects.
/// Replays preserve closed human history.
///
/// # Errors
/// Invalid findings, duplicate/colliding decisions or incompatible provenance
/// reject admission; successfully staged candidates survive partial I/O failure.
pub async fn escalate<R: CommandRunner>(
    bd: &BdClient<R>,
    parent: &BeadId,
    findings: &[IntegrityFinding],
    validator: &(impl FindingValidator + Sync + ?Sized),
) -> Result<Vec<DriverEventPayload>, Error> {
    let mut resolved = BTreeMap::new();
    for integrity in findings
        .iter()
        .filter(|finding| finding.is_push_gate_terminal())
    {
        let raw = integrity.to_raw_finding().ok_or_else(|| {
            Error::Graph(format!(
                "terminal finding has no document owner: {integrity}"
            ))
        })?;
        let finding = raw.resolve(DispatchScope::PushGate, validator)?;
        let label = decision_label(&finding);
        if let Some(previous) = resolved.insert(label.clone(), finding.clone())
            && previous.id() != finding.id()
        {
            return Err(Error::Graph(format!("finding hash collision for {label}")));
        }
    }
    if resolved.is_empty() {
        return Ok(Vec::new());
    }
    let epic = bd.show(parent).await?;
    if epic.issue_type != IssueType::Epic || epic.status == Status::Closed {
        return Err(Error::Graph(format!(
            "work root {parent} is not a live epic"
        )));
    }
    let children = bd
        .list(ListOpts {
            all: true,
            limit: Some(0),
            parent: Some(parent.clone()),
            ..ListOpts::default()
        })
        .await?;
    let mut plan = Vec::new();
    for (label, finding) in resolved {
        let matches: Vec<_> = children
            .iter()
            .filter(|bead| {
                bead.labels
                    .iter()
                    .any(|candidate| candidate.as_str() == label)
            })
            .collect();
        let existing = match matches.as_slice() {
            [] => None,
            [bead] => {
                let snapshot = bd.show(&bead.id).await?;
                check_record(&snapshot, parent, &finding, validator)?;
                Some(snapshot.id)
            }
            _ => {
                return Err(Error::Graph(format!(
                    "multiple decisions for {label}: {:?}",
                    matches.iter().map(|bead| &bead.id).collect::<Vec<_>>()
                )));
            }
        };
        plan.push((finding, existing));
    }
    let mut staged = Vec::new();
    for (finding, existing) in plan {
        let id = if let Some(id) = existing {
            id
        } else {
            let record = Record {
                work_root: parent.clone(),
                finding: finding.clone().into_raw(),
                disposition: Disposition::Candidate,
            };
            let mut labels = vec![decision_label(&finding)];
            labels.extend(finding.bonds().iter().map(|spec| format!("spec:{spec}")));
            labels.push(format!(
                "profile:{}",
                default_profile_for_spec(&finding.bonds()[0])
            ));
            bd.create(CreateOpts {
                status: Some(Status::Blocked),
                title: format!("Decide integrity repair: {}", finding.id()),
                description: format!(
                    "Integrity iteration cap exhausted. Finding: {}\n\n{}",
                    finding.id(),
                    finding.evidence()
                ),
                parent: Some(parent.clone()),
                metadata: Some(
                    serde_json::json!({ RECORD_KEY: serde_json::to_string(&record)? }).to_string(),
                ),
                labels,
                ..CreateOpts::default()
            })
            .await?
        };
        staged.push((id, finding));
    }
    let mut admitted = Vec::new();
    for (id, finding) in staged {
        let snapshot = bd.show(&id).await?;
        let record = check_record(&snapshot, parent, &finding, validator)?;
        admitted.push((snapshot, record));
    }
    let mut events = Vec::new();
    for (snapshot, mut record) in admitted {
        if snapshot.status == Status::Closed || record.disposition != Disposition::Candidate {
            continue;
        }
        let report = apply_clarify_or_blocked_report(bd, &snapshot.id).await?;
        record.disposition = match report.outcome {
            ClarifyApplyOutcome::Clarify => Disposition::Clarify,
            ClarifyApplyOutcome::BlockedClarifyWithoutOptions => Disposition::BriefRepair,
        };
        bd.update(
            &snapshot.id,
            UpdateOpts {
                set_metadata: vec![(RECORD_KEY.into(), serde_json::to_string(&record)?)],
                ..UpdateOpts::default()
            },
        )
        .await?;
        events.extend(report.routing_events(
            &snapshot.id,
            &ClarifyRouteContext {
                source_route: ClarifySourceRoute::ReviewVerdict,
                identity: "integrity-cap".into(),
                gate_log_path: None,
            },
        ));
    }
    Ok(events)
}
