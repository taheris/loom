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
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub record: Record,
    pub phase: Phase,
}

/// Not cache state: never reconstructed from worker-writable metadata. The
/// driver workspace is not the independent clone mounted into a worker.
#[derive(Debug, Clone)]
pub struct DecisionWaitJournal {
    directory: PathBuf,
}

impl DecisionWaitJournal {
    #[must_use]
    pub fn new(workspace: &Path) -> Self {
        Self {
            directory: workspace.join(".loom/decision-waits"),
        }
    }

    pub(super) async fn acquire(
        &self,
        source: &BeadId,
    ) -> Result<loom_driver::lock::LockGuard, LoopError> {
        std::fs::create_dir_all(&self.directory)?;
        let locks =
            loom_driver::lock::LockManager::with_state_home(&self.directory, &self.directory)?;
        Ok(locks
            .acquire_work_root_async(source, &loom_driver::clock::SystemClock::new())
            .await?)
    }

    pub(crate) fn has_pending_for(&self, root: &BeadId) -> Result<bool, LoopError> {
        Ok(self.entries()?.iter().any(|entry| {
            entry.phase != Phase::Released
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
            if entry.record.root == *root || entry.record.source == *root {
                return Ok(true);
            }
            let mut current = bd.show(&entry.record.source).await?;
            let mut seen = HashSet::new();
            loop {
                if current.id == *root {
                    return Ok(true);
                }
                if !seen.insert(current.id.clone()) {
                    return Err(invalid("cyclic native ownership at publication barrier").into());
                }
                let Some(parent) = current.parent else {
                    break;
                };
                current = bd.show(&parent).await?;
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
