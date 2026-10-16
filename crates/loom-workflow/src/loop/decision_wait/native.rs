//! Driver-attributed native dependency waits; projections never confer ownership.

use std::collections::HashSet;
use std::io::Write;

use loom_driver::bd::{BdClient, Bead, CommandRunner, IssueType, Status, UpdateOpts};
use loom_driver::identifier::{BeadId, SpecLabel};
use serde::{Deserialize, Serialize};

use super::{DecisionWaitError, DecisionWaitJournal, LoopError, invalid};

const KEY: &str = "loom.dependency_wait";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    })
}

impl DecisionWaitJournal {
    /// Validate a native Waiting proposal and durably admit its scheduling ownership.
    ///
    /// # Errors
    /// Native transport and journal failures remain errors; invalid proposals are recovery outcomes.
    pub async fn reconcile_native_wait<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        expected: &Bead,
        outcome: super::super::AgentOutcome,
    ) -> Result<super::super::AgentOutcome, LoopError> {
        use super::super::{AgentOutcome, WaitPrerequisites};
        let outcome = super::super::validate_waiting_outcome(bd, &expected.id, outcome).await?;
        if !matches!(
            outcome,
            AgentOutcome::Waiting {
                blockers: WaitPrerequisites::Native(_)
            }
        ) {
            return Ok(outcome);
        }
        match self.admit_native_wait(bd, expected).await {
            Ok(blockers) => Ok(AgentOutcome::Waiting {
                blockers: WaitPrerequisites::Native(blockers),
            }),
            Err(LoopError::DecisionWait(error)) => Ok(AgentOutcome::Failure {
                error: error.to_string(),
            }),
            Err(error) => Err(error),
        }
    }

    fn native(&self) -> Self {
        Self {
            directory: self.directory.with_file_name("dependency-waits"),
            ..self.clone()
        }
    }

    fn read_native(&self, id: &BeadId) -> Result<Option<Entry>, LoopError> {
        let bytes = match std::fs::read(self.path(id)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let entry: Entry = serde_json::from_slice(&bytes).map_err(DecisionWaitError::from)?;
        let unique: HashSet<_> = entry.record.blockers.iter().collect();
        let additional: HashSet<_> = entry.additional_blockers.iter().collect();
        if entry.record.version != 1
            || entry.record.source != *id
            || unique.is_empty()
            || unique.len() != entry.record.blockers.len()
            || additional.len() != entry.additional_blockers.len()
            || !unique.is_disjoint(&additional)
            || !matches!(
                entry.record.initial_status,
                Status::Open | Status::InProgress
            )
            || matches!(entry.record.kind, IssueType::Decision | IssueType::Molecule)
        {
            return Err(invalid("invalid canonical native wait attribution").into());
        }
        Ok(Some(entry))
    }

    fn save_native(&self, entry: &Entry) -> Result<(), LoopError> {
        std::fs::create_dir_all(&self.directory)?;
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        file.write_all(&serde_json::to_vec(entry).map_err(DecisionWaitError::from)?)?;
        file.as_file().sync_all()?;
        file.persist(self.path(&entry.record.source))
            .map_err(|error| error.error)?;
        for directory in self.directory.ancestors().take(3) {
            std::fs::File::open(directory)?.sync_all()?;
        }
        Ok(())
    }

    /// Park only work with current native prerequisites and no independent hold.
    ///
    /// # Errors
    /// Rejects changed identity, unsupported state and unowned projections.
    pub async fn admit_native_wait<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        expected: &Bead,
    ) -> Result<super::super::ActiveBlockers, LoopError> {
        let journal = self.native();
        let _guard = journal.acquire(&expected.id).await?;
        let source = bd.show(&expected.id).await?;
        let previous = journal.read_native(&source.id)?;
        let current_root = super::root(bd, &source).await?;
        if source.id != expected.id
            || source.parent != expected.parent
            || source.issue_type != expected.issue_type
            || independent(&source)
            || !matches!(
                source.status,
                Status::Open | Status::InProgress | Status::Blocked
            )
            || matches!(source.issue_type, IssueType::Decision | IssueType::Molecule)
        {
            return Err(
                invalid("native wait producer identity or independent hold changed").into(),
            );
        }
        let snapshot = bd.dependency_snapshot(&source.id).await?;
        let mut blockers = snapshot.active_blockers().into_iter();
        let first = blockers
            .next()
            .ok_or_else(|| invalid("native wait has no current prerequisite"))?;
        let blockers = super::super::ActiveBlockers::new(first, blockers.collect());
        let ids: Vec<_> = blockers.ids().cloned().collect();
        if ids.iter().collect::<HashSet<_>>().len() != ids.len() {
            return Err(invalid("native wait has duplicate prerequisite identities").into());
        }
        if let Some(mut entry) = previous
            && matches!(entry.phase, Phase::ProjectionPending | Phase::Active)
        {
            journal.reconcile_native(bd, &source, &mut entry).await?;
            if entry.phase != Phase::Active {
                return Err(invalid("native wait attribution was cancelled").into());
            }
            return Ok(blockers);
        }
        if !matches!(source.status, Status::Open | Status::InProgress)
            || source.metadata.contains_key(KEY)
        {
            return Err(invalid("native wait projection lacks current driver ownership").into());
        }
        let mut entry = Entry {
            record: Record {
                version: 1,
                source: source.id.clone(),
                root: current_root,
                parent: source.parent.clone(),
                kind: source.issue_type,
                initial_status: source.status,
                blockers: ids,
            },
            phase: Phase::ProjectionPending,
            additional_blockers: Vec::new(),
        };
        journal.save_native(&entry)?;
        journal.reconcile_native(bd, &source, &mut entry).await?;
        if entry.phase != Phase::Active {
            return Err(invalid("native wait changed before projection").into());
        }
        Ok(blockers)
    }

    async fn current_native_blockers<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        entry: &mut Entry,
    ) -> Result<Vec<BeadId>, LoopError> {
        let current = bd
            .dependency_snapshot(&entry.record.source)
            .await?
            .active_blockers();
        for id in entry
            .record
            .blockers
            .iter()
            .chain(&entry.additional_blockers)
        {
            let prerequisite = bd.show(id).await?;
            if prerequisite.id != *id
                || (prerequisite.status != Status::Closed && !current.contains(id))
            {
                return Err(invalid("native wait lost an unresolved admitted prerequisite").into());
            }
        }
        let added: Vec<_> = current
            .iter()
            .filter(|id| {
                !entry.record.blockers.contains(id) && !entry.additional_blockers.contains(id)
            })
            .cloned()
            .collect();
        if !added.is_empty() {
            entry.additional_blockers.extend(added);
            self.save_native(entry)?;
        }
        Ok(current)
    }

    async fn reconcile_native<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        source: &Bead,
        entry: &mut Entry,
    ) -> Result<bool, LoopError> {
        let projection = source
            .metadata
            .get(KEY)
            .map(|value| {
                let encoded = value
                    .as_str()
                    .ok_or_else(|| invalid("native wait projection must be a JSON string"))?;
                serde_json::from_str::<Record>(encoded).map_err(DecisionWaitError::from)
            })
            .transpose()?;
        if projection
            .as_ref()
            .is_some_and(|record| record != &entry.record)
        {
            return Err(invalid("native wait projection conflicts with driver attribution").into());
        }
        if entry.phase == Phase::Released || entry.phase == Phase::Cancelled {
            if projection.is_some() && entry.phase == Phase::Released {
                return Err(invalid("released native wait has a new unowned projection").into());
            }
            return Ok(source.status != Status::Open || independent(source));
        }
        if source.id != entry.record.source
            || source.parent != entry.record.parent
            || source.issue_type != entry.record.kind
            || super::root(bd, source).await? != entry.record.root
        {
            return Err(invalid("native wait source ownership changed").into());
        }
        if independent(source)
            || !matches!(
                source.status,
                Status::Open | Status::InProgress | Status::Blocked
            )
        {
            entry.phase = Phase::Cancelled;
            self.save_native(entry)?;
            return Ok(true);
        }
        if entry.phase == Phase::ReleasePending
            && source.status == Status::Open
            && projection.is_none()
        {
            entry.phase = Phase::Released;
            self.save_native(entry)?;
            return Ok(false);
        }
        if entry.phase == Phase::ProjectionPending
            && source.status == entry.record.initial_status
            && projection.is_none()
        {
            if self.current_native_blockers(bd, entry).await?.is_empty() {
                entry.phase = Phase::Released;
                self.save_native(entry)?;
                return Ok(false);
            }
            bd.update(
                &source.id,
                UpdateOpts {
                    status: Some(Status::Blocked),
                    if_status: Some(entry.record.initial_status),
                    set_metadata: vec![(
                        KEY.into(),
                        serde_json::to_string(&entry.record).map_err(DecisionWaitError::from)?,
                    )],
                    ..UpdateOpts::default()
                },
            )
            .await?;
            entry.phase = Phase::Active;
            self.save_native(entry)?;
            return Ok(true);
        }
        if source.status != Status::Blocked || projection.is_none() {
            entry.phase = Phase::Cancelled;
            self.save_native(entry)?;
            return Ok(true);
        }
        if !self.current_native_blockers(bd, entry).await?.is_empty() {
            if entry.phase == Phase::ReleasePending {
                return Err(invalid("new native prerequisite appeared during release").into());
            }
            entry.phase = Phase::Active;
            self.save_native(entry)?;
            return Ok(true);
        }
        entry.phase = Phase::ReleasePending;
        self.save_native(entry)?;
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
        entry.phase = Phase::Released;
        self.save_native(entry)?;
        Ok(false)
    }

    pub(super) async fn native_held<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        bead: &Bead,
    ) -> Result<bool, LoopError> {
        let journal = self.native();
        let _guard = journal.acquire(&bead.id).await?;
        let Some(mut entry) = journal.read_native(&bead.id)? else {
            if bead.metadata.contains_key(KEY) {
                return Err(invalid("native wait assertion lacks canonical attribution").into());
            }
            return Ok(false);
        };
        let source = bd.show(&bead.id).await?;
        journal.reconcile_native(bd, &source, &mut entry).await
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
        let affected: HashSet<_> = context.affected_work().collect();
        for id in affected {
            if *id == entry.record.source {
                continue;
            }
            let source = bd.show(id).await?;
            if source.status != Status::Open || independent(&source) {
                continue;
            }
            if self
                .native()
                .read_native(id)?
                .is_some_and(|wait| wait.phase == Phase::Cancelled)
                || self.native_held(bd, &source).await?
            {
                continue;
            }
            let active = bd.dependency_snapshot(id).await?.active_blockers();
            if decisions
                .iter()
                .any(|decision| decision.status != Status::Closed && active.contains(&decision.id))
            {
                self.admit_native_wait(bd, &source).await?;
            }
        }
        Ok(())
    }

    pub(super) fn native_pending_for(&self, root: &BeadId) -> Result<bool, LoopError> {
        Ok(self
            .native_pending_entries()?
            .iter()
            .any(|entry| entry.record.root == *root || entry.record.source == *root))
    }

    pub(super) async fn native_blocks_scope<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        root: &BeadId,
    ) -> Result<bool, LoopError> {
        let mut blocked = false;
        for entry in self.native_pending_entries()? {
            let source = bd.show(&entry.record.source).await?;
            let current_root = super::root(bd, &source).await?;
            if source.id != entry.record.source
                || source.parent != entry.record.parent
                || source.issue_type != entry.record.kind
                || current_root != entry.record.root
            {
                return Err(invalid("native wait ownership changed at publication barrier").into());
            }
            blocked |= entry.record.root == *root || entry.record.source == *root;
        }
        Ok(blocked)
    }

    fn native_pending_entries(&self) -> Result<Vec<Entry>, LoopError> {
        let journal = self.native();
        let files = match std::fs::read_dir(&journal.directory) {
            Ok(files) => files,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut entries = Vec::new();
        for file in files {
            let path = file?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let entry: Entry =
                serde_json::from_slice(&std::fs::read(&path)?).map_err(DecisionWaitError::from)?;
            if path != journal.path(&entry.record.source) {
                return Err(invalid("native wait filename disagrees with attribution").into());
            }
            let entry = journal
                .read_native(&entry.record.source)?
                .ok_or_else(|| invalid("native wait disappeared during publication"))?;
            if matches!(
                entry.phase,
                Phase::ProjectionPending | Phase::Active | Phase::ReleasePending
            ) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Reconcile owned waits absent from native readiness, without interpreting human prose.
    ///
    /// # Errors
    /// Malformed or conflicting durable state is retained and reported.
    pub async fn refresh_native_waits<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        root: Option<&BeadId>,
        label: Option<&SpecLabel>,
    ) -> Result<(), LoopError> {
        let journal = self.native();
        let files = match std::fs::read_dir(&journal.directory) {
            Ok(files) => files,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for file in files {
            let path = file?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let entry: Entry =
                serde_json::from_slice(&std::fs::read(&path)?).map_err(DecisionWaitError::from)?;
            if path != journal.path(&entry.record.source) {
                return Err(invalid("native wait filename disagrees with attribution").into());
            }
            if root.is_some_and(|root| entry.record.root != *root && entry.record.source != *root) {
                continue;
            }
            let source = bd.show(&entry.record.source).await?;
            if root.is_none()
                && let Some(label) = label
            {
                let owner = bd.show(&entry.record.root).await?;
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
            self.native_held(bd, &source).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
