//! Transient decision reconciliation; canonical task inputs are rechecked before use.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use loom_driver::bd::{BdClient, CommandRunner};
use loom_driver::identifier::BeadId;
use serde::{Deserialize, Serialize};

use super::{LoopError, Record, invalid};

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
    #[serde(default)]
    pub native_wait: bool,
}

/// Transient operation state: never reconstructed from worker-writable claims. The
/// driver workspace is not the independent clone mounted into a worker.
#[derive(Clone)]
pub struct DecisionWaitJournal {
    pub(super) workspace: PathBuf,
    pub(super) clock: std::sync::Arc<dyn loom_driver::clock::Clock>,
    pub(super) admissions: super::AdmissionTracker,
    pub(super) source_route: crate::gate_clarify::ClarifySourceRoute,
}

impl std::fmt::Debug for DecisionWaitJournal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DecisionWaitJournal")
            .field("workspace", &self.workspace)
            .field("admissions", &self.admissions)
            .field("source_route", &self.source_route)
            .finish_non_exhaustive()
    }
}

impl DecisionWaitJournal {
    #[must_use]
    pub fn new(workspace: &Path) -> Self {
        Self {
            workspace: loom_driver::state::path(workspace)
                .parent()
                .and_then(Path::parent)
                .unwrap_or(workspace)
                .to_path_buf(),
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
        let locks = loom_driver::lock::LockManager::new(&self.workspace)?;
        Ok(locks
            .acquire_record_async(
                loom_driver::state::RecordKind::DecisionInventory,
                source,
                self.clock.as_ref(),
            )
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
        super::migration::recover(bd, self).await?;
        self.refresh_native_waits(bd, root, Some(label)).await?;
        for entry in self
            .entries()?
            .into_iter()
            .filter(|entry| entry.phase != Phase::Released)
        {
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
            if in_scope {
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
        let native_blocked = self.native_blocks_scope(bd, root).await?;
        let mut blocked = self.has_pending_for(root)? || native_blocked;
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
                if entry.phase.blocks_publication()
                    || (entry.native_wait
                        && !bd
                            .dependency_snapshot(&entry.record.source)
                            .await?
                            .active_blockers()
                            .is_empty())
                {
                    // A live native source wait is already a publication veto,
                    // not permission to reseal its inventory's changed graph.
                    blocked = true;
                } else {
                    super::inventory::audit(bd, self, &entry).await?;
                }
            }
        }
        Ok(blocked)
    }

    pub(super) fn db(&self) -> Result<loom_driver::state::CacheDb, LoopError> {
        Ok(loom_driver::state::CacheDb::open(
            loom_driver::state::path(&self.workspace),
        )?)
    }

    fn entries(&self) -> Result<Vec<Entry>, LoopError> {
        let entries = self
            .db()?
            .records::<Entry>(loom_driver::state::RecordKind::DecisionInventory)?;
        entries
            .into_iter()
            .map(|(key, entry)| {
                if key != entry.record.source.as_str() {
                    return Err(invalid("mismatched decision record key").into());
                }
                self.load(&entry.record.source)?
                    .ok_or_else(|| invalid("decision record changed during reconciliation").into())
            })
            .collect()
    }

    pub(super) fn load(&self, source: &BeadId) -> Result<Option<Entry>, LoopError> {
        let Some(mut entry) = self.db()?.record::<Entry>(
            loom_driver::state::RecordKind::DecisionInventory,
            source.as_str(),
        )?
        else {
            return Ok(None);
        };
        if let Some(context) = &mut entry.context {
            context.apply_migrations(self)?;
        }
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
        self.db()?.put_record(
            loom_driver::state::RecordKind::DecisionInventory,
            entry.record.source.as_str(),
            entry,
        )?;
        Ok(())
    }
}
