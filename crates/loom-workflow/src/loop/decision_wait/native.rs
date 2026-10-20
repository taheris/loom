//! Native prerequisites determine waiting without a local release credential.

use std::collections::HashSet;

use loom_driver::bd::LabelExt;
use loom_driver::bd::{BdClient, Bead, CommandRunner, IssueType, ListOpts, Status, UpdateOpts};
use loom_driver::identifier::{BeadId, SpecLabel};
use loom_driver::state::RecordKind;
use serde::{Deserialize, Serialize};

use super::{DecisionWaitError, DecisionWaitJournal, LoopError, invalid};

const KEY: &str = "loom.dependency_wait";

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    source: BeadId,
    root: BeadId,
    parent: Option<BeadId>,
    kind: IssueType,
    initial_status: Status,
    blockers: Vec<BeadId>,
}

#[derive(Debug, Serialize, Deserialize)]
enum Phase {
    ProjectionPending,
    Active,
    ReleasePending,
    Released,
    Cancelled,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    record: Record,
    phase: Phase,
    #[serde(default)]
    additional_blockers: Vec<BeadId>,
}

fn independent(source: &Bead) -> bool {
    source.labels.iter().any(|label| {
        label.is_blocked()
            || label.is_clarify()
            || label.is_infra()
            || label.is_deferred()
            || label.as_str() == super::HOLD_LABEL
    }) || matches!(
        source.status,
        Status::Deferred | Status::Pinned | Status::Tombstone
    )
}

impl DecisionWaitJournal {
    /// Admit an ended session's waiting proposal against current native prerequisites.
    ///
    /// # Errors
    /// Reports tracker failures; invalid proposals remain worker recovery outcomes.
    pub async fn reconcile_native_wait<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        expected: &Bead,
        outcome: super::super::AgentOutcome,
    ) -> Result<super::super::AgentOutcome, LoopError> {
        use super::super::{AgentOutcome, WaitPrerequisites};
        if outcome != AgentOutcome::WaitingRequested {
            return Ok(outcome);
        }
        match self.admit_native_wait(bd, expected).await {
            Ok(blockers) => Ok(AgentOutcome::Waiting {
                blockers: WaitPrerequisites::Native(blockers),
            }),
            Err(LoopError::DecisionWait(error)) => Ok(AgentOutcome::Failure {
                error: format!("{}: {error}", super::super::waiting::INVALID_WAITING_CAUSE),
            }),
            Err(error) => Err(error),
        }
    }

    /// Release the ended session's claim; real dependency edges keep it off native readiness.
    ///
    /// # Errors
    /// Rejects changed task identity, independent holds and empty prerequisites.
    pub async fn admit_native_wait<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        expected: &Bead,
    ) -> Result<super::super::ActiveBlockers, LoopError> {
        let _guard = self.acquire(&expected.id).await?;
        let source = bd.show(&expected.id).await?;
        if source.id != expected.id
            || source.parent != expected.parent
            || source.issue_type != expected.issue_type
            || independent(&source)
            || !matches!(source.status, Status::Open | Status::InProgress)
            || matches!(source.issue_type, IssueType::Decision | IssueType::Molecule)
        {
            return Err(invalid("native wait source identity or independent hold changed").into());
        }
        let snapshot = bd.dependency_snapshot(&source.id).await?;
        let mut active = snapshot.active_blockers().into_iter();
        let first = active
            .next()
            .ok_or_else(|| invalid("native wait has no current prerequisite"))?;
        let blockers = super::super::ActiveBlockers::new(first, active.collect());
        if source.status == Status::InProgress {
            bd.update(
                &source.id,
                UpdateOpts {
                    status: Some(Status::Open),
                    if_status: Some(Status::InProgress),
                    ..UpdateOpts::default()
                },
            )
            .await?;
        }
        Ok(blockers)
    }

    pub(super) async fn native_held<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        bead: &Bead,
    ) -> Result<bool, LoopError> {
        if bead.metadata.contains_key(KEY) {
            return Err(invalid("legacy native wait lacks a checked release disposition; preserve the hold for review").into());
        }
        if bead.status == Status::Closed {
            return Ok(false);
        }
        if matches!(
            bead.status,
            Status::Deferred | Status::Pinned | Status::Tombstone
        ) || bead
            .labels
            .iter()
            .any(|label| label.is_blocked() || label.is_clarify() || label.is_deferred())
        {
            return Ok(true);
        }
        Ok(!bd
            .dependency_snapshot(&bead.id)
            .await?
            .active_blockers()
            .is_empty())
    }

    pub(super) async fn park_native_affected<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        entry: &super::Entry,
        decisions: &[Bead],
    ) -> Result<(), LoopError> {
        let context = entry
            .context
            .as_ref()
            .ok_or_else(|| invalid("affected work lacks admitted graph context"))?;
        let producer = bd.show(&entry.record.source).await?;
        super::graph::resolve(bd, &producer, &entry.record.root, decisions, Some(context)).await?;
        Ok(())
    }

    pub(super) async fn native_blocks_scope<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        root: &BeadId,
    ) -> Result<bool, LoopError> {
        let mut pending = vec![root.clone()];
        let mut visited = HashSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id.clone()) {
                return Err(invalid("cyclic native ownership at publication barrier").into());
            }
            if !bd
                .dependency_snapshot(&id)
                .await?
                .active_blockers()
                .is_empty()
            {
                return Ok(true);
            }
            for child in bd
                .list(ListOpts {
                    parent: Some(id.clone()),
                    all: true,
                    limit: Some(0),
                    ..ListOpts::default()
                })
                .await?
            {
                if child.parent.as_ref() != Some(&id) {
                    return Err(invalid("ambiguous native ownership at publication barrier").into());
                }
                // Native closure does not remove an independent legacy hold assertion.
                // A missing local inventory cannot authorize ignoring that current input.
                if child.metadata.contains_key(super::HOLD_KEY)
                    || child
                        .labels
                        .iter()
                        .any(|label| label.as_str() == super::HOLD_LABEL)
                {
                    return Ok(true);
                }
                if !matches!(child.status, Status::Closed | Status::Tombstone) {
                    pending.push(child.id);
                }
            }
        }
        Ok(false)
    }

    /// Convert positively attributed legacy parking to native graph-only waiting.
    ///
    /// # Errors
    /// Ambiguous legacy holds remain unchanged; malformed attribution is diagnosed.
    pub async fn refresh_native_waits<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        root: Option<&BeadId>,
        label: Option<&SpecLabel>,
    ) -> Result<(), LoopError> {
        for (key, entry) in self.db()?.records::<Entry>(RecordKind::DependencyWait)? {
            let record = &entry.record;
            if root.is_some_and(|root| record.root != *root && record.source != *root) {
                continue;
            }
            if key != record.source.as_str()
                || record.version != 1
                || record.blockers.is_empty()
                || record.blockers.iter().collect::<HashSet<_>>().len() != record.blockers.len()
            {
                return Err(invalid("invalid legacy native wait attribution").into());
            }
            let _guard = self.acquire(&record.source).await?;
            let source = bd.show(&record.source).await?;
            if let Some(label) = label {
                let owner = bd.show(&record.root).await?;
                let expected = format!("spec:{label}");
                if !source
                    .labels
                    .iter()
                    .chain(&owner.labels)
                    .any(|label| label.as_str() == expected)
                {
                    continue;
                }
            }
            let kind_changed = source.issue_type != record.kind;
            if kind_changed
                || source.parent != record.parent
                || super::root(bd, &source).await? != record.root
            {
                return Err(invalid("legacy native wait source ownership changed").into());
            }
            if matches!(entry.phase, Phase::Cancelled) || independent(&source) {
                continue;
            }
            let projected = source
                .metadata
                .get(KEY)
                .and_then(serde_json::Value::as_str)
                .map(|value| serde_json::from_str::<Record>(value).map_err(DecisionWaitError::from))
                .transpose()?;
            let matching = projected.as_ref() == Some(record);
            if source.status == Status::Blocked && matching {
                bd.update(
                    &source.id,
                    UpdateOpts {
                        status: Some(Status::Open),
                        if_status: Some(Status::Blocked),
                        unset_metadata: vec![KEY.into()],
                        ..UpdateOpts::default()
                    },
                )
                .await?;
            } else if source.status == Status::Blocked || projected.is_some() {
                return Err(
                    invalid("legacy native wait cannot override an unattributed hold").into(),
                );
            }
            self.db()?.remove_record(RecordKind::DependencyWait, &key)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
