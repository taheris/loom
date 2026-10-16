//! Checked additive graph extensions, retaining exact resolved goal identities.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use loom_driver::bd::{BdClient, Bead, CommandRunner, IssueType};
use loom_driver::identifier::BeadId;
use loom_protocol::acceptance::GoalReference;
use loom_protocol::gate::{Finding, RawFinding};
use serde::{Deserialize, Serialize};

use super::{Decision, LoopError, invalid};
use crate::r#loop::{DecisionWaitError, acceptance};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    decision: BeadId,
    task: BeadId,
    finding: RawFinding,
    reference: GoalReference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Context {
    workspace: PathBuf,
    decisions: Vec<BeadId>,
    bindings: Vec<Binding>,
}

impl Context {
    pub(super) fn covers(&self, decision: &BeadId) -> bool {
        self.decisions.contains(decision)
    }

    pub(super) fn matches(&self, decisions: &[Decision]) -> bool {
        let unique: HashSet<_> = self
            .bindings
            .iter()
            .map(|binding| (&binding.decision, &binding.task))
            .collect();
        let claimed: HashSet<_> = self.decisions.iter().collect();
        unique.len() == self.bindings.len()
            && claimed.len() == self.decisions.len()
            && self.decisions.iter().all(|id| {
                decisions
                    .iter()
                    .find(|decision| decision.id == *id)
                    .is_some_and(|decision| {
                        decision
                            .affected_work
                            .iter()
                            .all(|task| unique.contains(&(id, task)))
                    })
            })
            && self.bindings.iter().all(|binding| {
                claimed.contains(&binding.decision)
                    && decisions.iter().any(|decision| {
                        decision.id == binding.decision
                            && decision.affected_work.contains(&binding.task)
                    })
            })
    }

    pub(super) async fn recheck<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        root: &BeadId,
    ) -> Result<(), LoopError> {
        for binding in &self.bindings {
            let reference =
                resolve_goal(bd, root, &binding.task, &binding.finding, &self.workspace).await?;
            if reference != binding.reference {
                return Err(invalid(format!(
                    "affected work {} changed integrity batch attribution",
                    binding.task
                ))
                .into());
            }
        }
        Ok(())
    }
}

async fn resolve_goal<R: CommandRunner>(
    bd: &BdClient<R>,
    root: &BeadId,
    id: &BeadId,
    finding: &RawFinding,
    workspace: &Path,
) -> Result<GoalReference, LoopError> {
    let task = bd.show(id).await?;
    if task
        .metadata
        .contains_key(crate::mint::acceptance::FOLLOW_UP_KEY)
    {
        return Err(invalid(format!(
            "affected work {id} needs prior follow-up acceptance admission"
        ))
        .into());
    }
    let resolved = acceptance::resolve(bd, &task, workspace)
        .await
        .map_err(DecisionWaitError::from)?;
    let acceptance::Resolved::Remediation(acceptance) = resolved else {
        return Err(invalid(format!(
            "affected work {id} has no matching finding-goal acceptance"
        ))
        .into());
    };
    let matching: Vec<_> = acceptance
        .goals()
        .iter()
        .filter(|goal| {
            goal.task() == id
                && goal.work_root() == root
                && goal.finding().clone().into_raw() == *finding
        })
        .collect();
    match matching.as_slice() {
        [goal] => Ok(goal.reference().clone()),
        _ => Err(invalid(format!(
            "affected work {id} lacks a unique exact integrity goal"
        ))
        .into()),
    }
}

pub(super) fn additive(old: &Decision, current: &Decision) -> bool {
    let mut proposed = current.clone();
    proposed.affected_work.clone_from(&old.affected_work);
    if let (Some(proposed), Some(original)) = (&mut proposed.candidate, &old.candidate) {
        proposed.affected_work.clone_from(&original.affected_work);
    }
    proposed == *old
        && old
            .affected_work
            .iter()
            .all(|id| current.affected_work.contains(id))
}

/// Only exact driver-resolved findings can authorize additions; other graph
/// changes still require explicit repair and never rewrite native edges.
pub async fn resolve<R: CommandRunner>(
    bd: &BdClient<R>,
    source: &Bead,
    root: &BeadId,
    decisions: &[Bead],
    previous: Option<&super::Context>,
    findings: &[(BeadId, Finding)],
    workspace: &Path,
) -> Result<super::Context, LoopError> {
    let mut current = super::resolve(bd, source, root, decisions, None).await?;
    if source.id != *root
        || source.issue_type != IssueType::Epic
        || previous.is_some_and(|old| {
            old.producer != current.producer || old.producer_type != current.producer_type
        })
    {
        return Err(invalid("integrity producer identity or work root changed").into());
    }
    let mut context = previous
        .and_then(|old| old.integrity.clone())
        .unwrap_or_else(|| Context {
            workspace: workspace.to_path_buf(),
            decisions: Vec::new(),
            bindings: Vec::new(),
        });
    if context.workspace != workspace {
        return Err(invalid("integrity acceptance workspace changed").into());
    }
    context.recheck(bd, root).await?;
    for (id, _) in findings {
        if !context.decisions.contains(id) {
            context.decisions.push(id.clone());
        }
    }
    for decision in &current.decisions {
        let old =
            previous.and_then(|context| context.decisions.iter().find(|old| old.id == decision.id));
        if old == Some(decision) {
            continue;
        }
        if old.is_some_and(|old| !additive(old, decision)) {
            return Err(invalid(format!(
                "integrity decision {} changed identity or removed affected work",
                decision.id
            ))
            .into());
        }
        if decision.affected_work.is_empty() {
            continue;
        }
        let finding = findings
            .iter()
            .find(|(id, _)| *id == decision.id)
            .map(|(_, finding)| finding)
            .ok_or_else(|| {
                invalid(format!(
                    "changed integrity decision {} lacks current resolved finding",
                    decision.id
                ))
            })?;
        for id in &decision.affected_work {
            let finding = finding.clone().into_raw();
            let reference = resolve_goal(bd, root, id, &finding, workspace).await?;
            let binding = Binding {
                decision: decision.id.clone(),
                task: id.clone(),
                finding,
                reference,
            };
            if !context.bindings.contains(&binding) {
                context.bindings.push(binding);
            }
        }
    }
    if !context.matches(&current.decisions) {
        return Err(invalid("integrity goal bindings disagree with current graph").into());
    }
    current.integrity = Some(context);
    Ok(current)
}
