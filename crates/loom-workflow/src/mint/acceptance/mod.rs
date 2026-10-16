//! Driver-owned finding attribution, distinct from editable task-reference candidates.

#[cfg(test)]
mod dispatch_tests;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use displaydoc::Display;
use loom_driver::bd::{BdClient, BdError, Bead, CommandRunner, IssueType, Status, UpdateOpts};
use loom_driver::identifier::BeadId;
use loom_protocol::acceptance::{
    Candidates, FindingHash, GoalReference, GoalReferences, METADATA_KEY, ParseError,
};
use loom_protocol::gate::{DispatchScope, Finding, FindingParseError, RawFinding, ScopeKind};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::review::WorkspaceFindingValidator;

pub const BATCH_KEY: &str = "loom.finding_batch";
pub const FOLLOW_UP_KEY: &str = "loom.remediation_follow_up";

/// A worker proposal, not task acceptance or permission to borrow another task's findings.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FollowUp {
    pub source: BeadId,
    pub goals: GoalReferences,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    work_root: BeadId,
    findings: Vec<Member>,
    attribution: HashMap<BeadId, GoalReferences>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    hash: FindingHash,
    finding: RawFinding,
}

pub use loom_protocol::acceptance::dispatch::{Goal, Remediation as Resolved};

#[derive(Debug, Display, Error)]
pub enum Error {
    /// Beads failed while resolving finding acceptance
    Bd(#[from] BdError),
    /// malformed finding acceptance or batch provenance
    Json(#[from] serde_json::Error),
    /// invalid finding-goal reference set
    References(#[from] ParseError),
    /// finding no longer resolves in the dispatch workspace
    Finding(#[from] FindingParseError),
    /// finding acceptance needs explicit repair: {0}
    Invalid(String),
}

fn read<T: serde::de::DeserializeOwned>(bead: &Bead, key: &str) -> Result<T, Error> {
    let encoded = bead
        .metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Error::Invalid(format!("task {} lacks string metadata `{key}`", bead.id)))?;
    Ok(serde_json::from_str(encoded)?)
}

fn references(batch: &BeadId, findings: &[Finding]) -> Result<GoalReferences, Error> {
    Ok(GoalReferences::new(
        findings
            .iter()
            .map(|finding| {
                Ok(GoalReference {
                    batch: batch.clone(),
                    finding_hash: FindingHash::new(finding.hash())?,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?,
    )?)
}

/// Add current driver-resolved findings to a batch, preserving unrelated metadata and attribution.
///
/// # Errors
/// Rejects malformed previous records, empty batches and compact-identity collisions.
pub(super) fn batch_update(
    task: &BeadId,
    work_root: &BeadId,
    findings: &[Finding],
    previous: Option<&Bead>,
) -> Result<UpdateOpts, Error> {
    let mut batch = if let Some(previous) = previous {
        let (batch, _) = assigned_batch(previous)?;
        if batch.work_root != *work_root || previous.id != *task {
            return Err(Error::Invalid(
                "batch changed work root or task identity".into(),
            ));
        }
        batch
    } else {
        Batch {
            work_root: work_root.clone(),
            findings: Vec::new(),
            attribution: HashMap::new(),
        }
    };
    references(task, findings)?;
    for finding in findings {
        if finding.evidence().trim().is_empty() {
            return Err(Error::Invalid("finding evidence is empty".into()));
        }
        let hash = FindingHash::new(finding.hash())?;
        let raw = finding.clone().into_raw();
        if let Some(previous) = batch.findings.iter().find(|member| member.hash == hash) {
            if previous.finding != raw {
                return Err(Error::Invalid(
                    "finding hash has conflicting batch evidence or identity".into(),
                ));
            }
        } else {
            batch.findings.push(Member { hash, finding: raw });
        }
    }
    let assigned = GoalReferences::new(
        batch
            .findings
            .iter()
            .map(|member| GoalReference {
                batch: task.clone(),
                finding_hash: member.hash.clone(),
            })
            .collect(),
    )?;
    batch.attribution.insert(task.clone(), assigned.clone());
    Ok(UpdateOpts {
        set_metadata: vec![
            (BATCH_KEY.into(), serde_json::to_string(&batch)?),
            (
                METADATA_KEY.into(),
                serde_json::to_string(&Candidates::Remediation { goals: assigned })?,
            ),
        ],
        ..UpdateOpts::default()
    })
}

/// Promote only an explicitly assigned batch; historical labels cannot repair missing provenance.
///
/// # Errors
/// Rejects missing or changed task purpose, attribution and batch membership.
pub(super) fn promotion_update(task: &Bead) -> Result<UpdateOpts, Error> {
    let (_, goals) = assigned_batch(task)?;
    Ok(UpdateOpts {
        status: Some(Status::Open),
        remove_labels: vec![super::DEFERRED_LABEL.into()],
        set_metadata: vec![(
            METADATA_KEY.into(),
            serde_json::to_string(&Candidates::Remediation { goals })?,
        )],
        ..UpdateOpts::default()
    })
}

fn assigned_batch(task: &Bead) -> Result<(Batch, GoalReferences), Error> {
    let batch: Batch = read(task, BATCH_KEY)?;
    let Candidates::Remediation { goals } = read(task, METADATA_KEY)? else {
        return Err(Error::Invalid(
            "ordinary references cannot be converted to remediation".into(),
        ));
    };
    let members = GoalReferences::new(
        batch
            .findings
            .iter()
            .map(|member| GoalReference {
                batch: task.id.clone(),
                finding_hash: member.hash.clone(),
            })
            .collect(),
    )?;
    if task.parent.as_ref() != Some(&batch.work_root)
        || !batch
            .attribution
            .get(&task.id)
            .is_some_and(|assigned| same_goals(assigned, &goals))
        || !same_goals(&members, &goals)
    {
        return Err(Error::Invalid(
            "batch has inconsistent finding attribution".into(),
        ));
    }
    Ok((batch, goals))
}

async fn load_batch<R: CommandRunner>(
    bd: &BdClient<R>,
    id: &BeadId,
    task: &Bead,
) -> Result<Batch, Error> {
    let bead = bd.show(id).await?;
    let batch: Batch = read(&bead, BATCH_KEY)?;
    let root = bd.show(&batch.work_root).await?;
    if bead.issue_type != IssueType::Task
        || bead.parent.as_ref() != Some(&batch.work_root)
        || task.issue_type != IssueType::Task
        || task.parent.as_ref() != Some(&batch.work_root)
        || root.issue_type != IssueType::Epic
        || root.status == Status::Closed
    {
        return Err(Error::Invalid(format!(
            "batch {id} and task {} do not share a live work root",
            task.id
        )));
    }
    Ok(batch)
}

/// Re-resolve all candidate goals; labels, prose and serialized findings are not resolved authority.
///
/// # Errors
/// Missing membership/attribution, empty evidence, collisions and stale targets reject the whole set.
pub async fn resolve<R: CommandRunner>(
    bd: &BdClient<R>,
    task: &Bead,
    references: &GoalReferences,
    workspace: &Path,
) -> Result<Resolved, Error> {
    resolve_references(bd, task, references, workspace, Coverage::Complete).await
}

#[derive(Clone, Copy)]
enum Coverage {
    Complete,
    Subset,
}

async fn resolve_references<R: CommandRunner>(
    bd: &BdClient<R>,
    task: &Bead,
    references: &GoalReferences,
    workspace: &Path,
    coverage: Coverage,
) -> Result<Resolved, Error> {
    let validator = WorkspaceFindingValidator::new(workspace);
    let mut batches = HashMap::new();
    let mut goals = Vec::new();
    for reference in references.as_slice() {
        if !batches.contains_key(&reference.batch) {
            let batch = load_batch(bd, &reference.batch, task).await?;
            let assigned = batch.attribution.get(&task.id).ok_or_else(|| {
                Error::Invalid(format!("task {} has no driver attribution", task.id))
            })?;
            if matches!(coverage, Coverage::Complete) && !same_goals(assigned, references) {
                return Err(Error::Invalid(format!(
                    "task {} omits or changes attributed goals",
                    task.id
                )));
            }
            let mut findings = HashMap::new();
            for member in batch.findings {
                if findings.insert(member.hash, member.finding).is_some() {
                    return Err(Error::Invalid(format!(
                        "batch {} contains duplicate or colliding identities",
                        reference.batch
                    )));
                }
            }
            batches.insert(
                reference.batch.clone(),
                (batch.work_root, batch.attribution, findings),
            );
        }
        let (root, attribution, findings) = batches.get(&reference.batch).ok_or_else(|| {
            Error::Invalid("resolved batch disappeared during acceptance construction".into())
        })?;
        if !attribution
            .get(&task.id)
            .is_some_and(|refs| refs.as_slice().contains(reference))
        {
            return Err(Error::Invalid(format!(
                "task {} has no driver attribution for {reference:?}",
                task.id
            )));
        }
        let raw = findings.get(&reference.finding_hash).ok_or_else(|| {
            Error::Invalid(format!("finding {reference:?} is absent from its batch"))
        })?;
        let scope = match raw.token.scope_kind() {
            ScopeKind::PerBead => DispatchScope::PerBead,
            ScopeKind::TreeOnly | ScopeKind::TreeAndPushGate | ScopeKind::AnyScope => {
                DispatchScope::Tree
            }
        };
        let finding = raw.clone().resolve(scope, &validator)?;
        if reference.finding_hash.as_str() != finding.hash() {
            return Err(Error::Invalid("recorded finding identity changed".into()));
        }
        if finding.evidence().trim().is_empty() {
            return Err(Error::Invalid(format!(
                "finding {reference:?} contains empty evidence"
            )));
        }
        goals.push(Goal::new(
            reference.clone(),
            task.id.clone(),
            root.clone(),
            finding,
        )?);
    }
    Ok(Resolved::new(goals)?)
}

fn same_goals(left: &GoalReferences, right: &GoalReferences) -> bool {
    let left: HashSet<_> = left.as_slice().iter().collect();
    let right: HashSet<_> = right.as_slice().iter().collect();
    left == right
}

/// Admit a worker-proposed subset as same-root follow-up work, retaining original attribution.
///
/// # Errors
/// Rejects ordinary-purpose conversion, unrelated sources, stale targets and incompatible replays.
pub async fn admit_follow_up<R: CommandRunner>(
    bd: &BdClient<R>,
    task: &Bead,
    proposal: &FollowUp,
    workspace: &Path,
) -> Result<(), Error> {
    if task.id == proposal.source {
        return Err(Error::Invalid(
            "follow-up source must be a different task".into(),
        ));
    }
    if let Some(value) = task.metadata.get(METADATA_KEY) {
        let candidate = Candidates::from_metadata(value)?;
        if candidate
            != (Candidates::Remediation {
                goals: proposal.goals.clone(),
            })
        {
            return Err(Error::Invalid(
                "follow-up cannot replace existing task purpose or goals".into(),
            ));
        }
    }
    let source = bd.show(&proposal.source).await?;
    let Candidates::Remediation { goals: source_refs } = read(&source, METADATA_KEY)? else {
        return Err(Error::Invalid(
            "follow-up source is ordinary implementation work".into(),
        ));
    };
    if task.parent != source.parent || task.issue_type != IssueType::Task {
        return Err(Error::Invalid(
            "follow-up must be a sibling task in the same work root".into(),
        ));
    }
    if proposal
        .goals
        .as_slice()
        .iter()
        .any(|reference| !source_refs.as_slice().contains(reference))
    {
        return Err(Error::Invalid(
            "follow-up borrows goals not attributed to its source".into(),
        ));
    }
    resolve_references(bd, &source, &proposal.goals, workspace, Coverage::Subset).await?;
    let mut batches = HashMap::new();
    for reference in proposal.goals.as_slice() {
        if !batches.contains_key(&reference.batch) {
            batches.insert(
                reference.batch.clone(),
                load_batch(bd, &reference.batch, task).await?,
            );
        }
    }
    for batch in batches.values_mut() {
        if let Some(previous) = batch.attribution.get(&task.id) {
            if !same_goals(previous, &proposal.goals) {
                return Err(Error::Invalid(
                    "follow-up attribution changed; explicitly repair it".into(),
                ));
            }
        } else {
            batch
                .attribution
                .insert(task.id.clone(), proposal.goals.clone());
        }
    }
    for (id, batch) in batches {
        bd.update(
            &id,
            UpdateOpts {
                set_metadata: vec![(BATCH_KEY.into(), serde_json::to_string(&batch)?)],
                ..UpdateOpts::default()
            },
        )
        .await?;
    }
    bd.update(
        &task.id,
        UpdateOpts {
            set_metadata: vec![(
                METADATA_KEY.into(),
                serde_json::to_string(&Candidates::Remediation {
                    goals: proposal.goals.clone(),
                })?,
            )],
            unset_metadata: vec![FOLLOW_UP_KEY.into()],
            ..UpdateOpts::default()
        },
    )
    .await?;
    Ok(())
}

/// Resolve a proposal before persisting task references; no label-derived assignments.
///
/// # Errors
/// Malformed proposals and invalid finding attribution block dispatch.
pub async fn admit_proposal<R: CommandRunner>(
    bd: &BdClient<R>,
    task: &Bead,
    workspace: &Path,
) -> Result<(), Error> {
    let proposal = read(task, FOLLOW_UP_KEY)?;
    admit_follow_up(bd, task, &proposal, workspace).await
}
