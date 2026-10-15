//! Canonical host attribution. Native metadata is only a checked projection.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use loom_driver::bd::{BdClient, CommandRunner};
use loom_driver::identifier::BeadId;
use serde::{Deserialize, Serialize};

use super::{DecisionWaitError, LoopError, Record, invalid};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Phase {
    ProjectionPending,
    Active,
    ReleasePending,
    Released,
    InventoryPending,
    InventoryAdmitted,
}

impl Phase {
    pub(super) const fn holds_source(self) -> bool {
        matches!(
            self,
            Self::ProjectionPending | Self::Active | Self::ReleasePending
        )
    }

    const fn blocks_publication(self) -> bool {
        self.holds_source() || matches!(self, Self::InventoryPending)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub record: Record,
    pub phase: Phase,
    #[serde(default)]
    pub context: Option<super::graph::Context>,
}

/// Not cache state: never reconstructed from worker-writable metadata. The
/// driver workspace is not the independent clone mounted into a worker.
#[derive(Clone)]
pub struct DecisionWaitJournal {
    pub(super) directory: PathBuf,
    pub(super) clock: std::sync::Arc<dyn loom_driver::clock::Clock>,
    pub(super) admissions: super::AdmissionTracker,
    pub(super) source_route: crate::gate_clarify::ClarifySourceRoute,
}

impl std::fmt::Debug for DecisionWaitJournal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DecisionWaitJournal")
            .field("directory", &self.directory)
            .field("admissions", &self.admissions)
            .field("source_route", &self.source_route)
            .finish_non_exhaustive()
    }
}

impl DecisionWaitJournal {
    #[must_use]
    pub fn new(workspace: &Path) -> Self {
        Self {
            directory: workspace.join(".loom/decision-waits"),
            clock: std::sync::Arc::new(loom_driver::clock::SystemClock::new()),
            admissions: super::AdmissionTracker::default(),
            source_route: crate::gate_clarify::ClarifySourceRoute::LoopMarker,
        }
    }

    /// Inject the trusted clock for journal locks and durable event timestamps.
    #[must_use]
    pub fn with_clock(mut self, clock: std::sync::Arc<dyn loom_driver::clock::Clock>) -> Self {
        self.clock = clock;
        self
    }

    #[must_use]
    pub fn with_admissions(mut self, admissions: super::AdmissionTracker) -> Self {
        self.admissions = admissions;
        self
    }

    #[must_use]
    pub const fn with_source_route(
        mut self,
        route: crate::gate_clarify::ClarifySourceRoute,
    ) -> Self {
        self.source_route = route;
        self
    }

    /// Whether this producer has a completed canonical disposition for the decision.
    ///
    /// # Errors
    /// Malformed or mismatched canonical records are retained and rejected.
    pub fn has_disposition(&self, producer: &BeadId, decision: &BeadId) -> Result<bool, LoopError> {
        let entry = self
            .load(producer)?
            .ok_or_else(|| invalid("decision disposition lacks canonical inventory"))?;
        let parent = entry
            .context
            .as_ref()
            .and_then(|context| context.discovery_parent(decision))
            .ok_or_else(|| invalid("decision disposition lacks checked discovery parent"))?;
        super::disposition::reusable(self, producer, decision, parent)
    }

    pub(super) fn complete_dispositions(&self, entry: &Entry) -> Result<bool, LoopError> {
        for id in &entry.record.decisions {
            if !self.has_disposition(&entry.record.source, id)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) async fn acquire(
        &self,
        source: &BeadId,
    ) -> Result<loom_driver::lock::LockGuard, LoopError> {
        std::fs::create_dir_all(&self.directory)?;
        let locks =
            loom_driver::lock::LockManager::with_state_home(&self.directory, &self.directory)?;
        Ok(locks
            .acquire_work_root_async(source, self.clock.as_ref())
            .await?)
    }

    /// Complete interrupted queue staging even when the producer is no longer
    /// returned by native readiness (for example, after agent-side closure).
    pub(crate) async fn recover_inventory<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        root: Option<&BeadId>,
        label: &loom_driver::identifier::SpecLabel,
    ) -> Result<(), LoopError> {
        for entry in self.entries()?.into_iter().filter(|entry| {
            matches!(
                entry.phase,
                Phase::InventoryPending | Phase::InventoryAdmitted
            )
        }) {
            if root.is_some_and(|root| entry.record.root != *root && entry.record.source != *root) {
                continue;
            }
            let source = bd.show(&entry.record.source).await?;
            let in_scope = if let Some(root) = root {
                entry.record.root == *root || entry.record.source == *root
            } else {
                let owner = bd.show(&entry.record.root).await?;
                let expected_label = format!("spec:{label}");
                source
                    .labels
                    .iter()
                    .chain(&owner.labels)
                    .any(|actual| actual.as_str() == expected_label)
            };
            if in_scope
                && (entry.phase == Phase::InventoryPending
                    || entry.context.is_none()
                    || !self.complete_dispositions(&entry)?)
            {
                super::held(bd, self, &source).await?;
            }
        }
        Ok(())
    }

    pub(crate) fn has_pending_for(&self, root: &BeadId) -> Result<bool, LoopError> {
        Ok(self.entries()?.iter().any(|entry| {
            entry.phase.blocks_publication()
                && (entry.record.root == *root || entry.record.source == *root)
        }))
    }

    /// Protect both original attribution and current native membership: moving
    /// or closing held work cannot evade a molecule's publication barrier.
    pub(crate) async fn blocks_scope<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        root: &BeadId,
    ) -> Result<bool, LoopError> {
        if self.has_pending_for(root)? {
            return Ok(true);
        }
        for entry in self
            .entries()?
            .into_iter()
            .filter(|entry| entry.phase != Phase::Released)
        {
            let mut in_scope = entry.record.root == *root || entry.record.source == *root;
            if !in_scope {
                let mut current = bd.show(&entry.record.source).await?;
                let mut seen = HashSet::new();
                loop {
                    if current.id == *root {
                        in_scope = true;
                        break;
                    }
                    if !seen.insert(current.id.clone()) {
                        return Err(
                            invalid("cyclic native ownership at publication barrier").into()
                        );
                    }
                    let Some(parent) = current.parent else {
                        break;
                    };
                    current = bd.show(&parent).await?;
                    if current.id != parent {
                        return Err(
                            invalid("ambiguous native ownership at publication barrier").into()
                        );
                    }
                }
            }
            if in_scope {
                if entry.phase.blocks_publication() {
                    return Ok(true);
                }
                super::inventory::audit(bd, self, &entry).await?;
            }
        }
        Ok(false)
    }

    fn entries(&self) -> Result<Vec<Entry>, LoopError> {
        let files = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut entries = Vec::new();
        for path in files {
            let path = path?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let bytes = std::fs::read(&path)?;
            let entry: Entry = serde_json::from_slice(&bytes).map_err(DecisionWaitError::from)?;
            if path != self.path(&entry.record.source) {
                return Err(invalid("mismatched journal filename").into());
            }
            entries.push(
                self.load(&entry.record.source)?
                    .ok_or_else(|| invalid("journal changed during publication barrier"))?,
            );
        }
        Ok(entries)
    }

    fn path(&self, source: &BeadId) -> PathBuf {
        let hash = blake3::hash(source.as_str().as_bytes()).to_hex();
        self.directory.join(format!("{hash}.json"))
    }

    pub(super) fn load(&self, source: &BeadId) -> Result<Option<Entry>, LoopError> {
        let bytes = match std::fs::read(self.path(source)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let entry: Entry = serde_json::from_slice(&bytes).map_err(DecisionWaitError::from)?;
        let ids: HashSet<_> = entry.record.decisions.iter().collect();
        if entry.record.version != 1
            || entry.record.source != *source
            || ids.is_empty()
            || ids.len() != entry.record.decisions.len()
            || entry.context.as_ref().is_some_and(|context| {
                !context.matches_inventory(&entry.record.source, &entry.record.decisions)
            })
        {
            return Err(
                invalid("unsupported, empty, duplicate or mismatched host attribution").into(),
            );
        }
        Ok(Some(entry))
    }

    pub(super) fn save(&self, entry: &Entry) -> Result<(), LoopError> {
        std::fs::create_dir_all(&self.directory)?;
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        file.write_all(&serde_json::to_vec(entry).map_err(DecisionWaitError::from)?)?;
        file.as_file().sync_all()?;
        file.persist(self.path(&entry.record.source))
            .map_err(|error| error.error)?;
        // Also persist directory creation, not only the renamed receipt.
        for directory in self.directory.ancestors().take(3) {
            std::fs::File::open(directory)?.sync_all()?;
        }
        Ok(())
    }
}
