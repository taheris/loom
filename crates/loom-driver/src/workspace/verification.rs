//! Driver recovery state beneath the existing protected operator Git directory.
//! These records retain unfinished driver work; they never authenticate coverage.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{Association, Error, Identity};
use crate::bd::Status;
use crate::git::{GitOid, head_tree_oid_sync, sync_head_commit_sha};
use crate::identifier::{BeadId, SpecLabel};
use crate::scratch::directory::Directory;

const LOG: &str = "loom-verification-continuations";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Phase {
    Verify,
    Publication,
    Repair,
    Resolved,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationRebase {
    pub before: GitOid,
    pub onto: GitOid,
    pub after: Option<GitOid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationContinuation {
    pub bead: BeadId,
    pub label: SpecLabel,
    pub root: Option<BeadId>,
    pub base: GitOid,
    pub candidate: GitOid,
    pub parked_status: Option<Status>,
    #[serde(default)]
    pub repair_detail: Option<String>,
    #[serde(default)]
    pub parked_cause: Option<String>,
    #[serde(default)]
    pub rollback: Option<GitOid>,
    #[serde(default)]
    pub publication_rebase: Option<PublicationRebase>,
    source_head: GitOid,
    source_tree: GitOid,
    directory: Identity,
    git: Identity,
    phase: Phase,
}

impl Association {
    /// Capture a completed source candidate before integration advances.
    ///
    /// # Errors
    /// Rejects unavailable authority or changed/indirect source storage.
    pub fn begin_verification(
        &self,
        bead: BeadId,
        label: SpecLabel,
        root: Option<BeadId>,
        base: GitOid,
        candidate: GitOid,
    ) -> Result<VerificationContinuation, Error> {
        self.recheck()?;
        let path = self.operator().join(".loom/beads").join(bead.as_str());
        let directory = Directory::open(&path)?;
        let git = directory.child(Path::new(".git"))?;
        let record = VerificationContinuation {
            bead,
            label,
            root,
            rollback: Some(base.clone()),
            publication_rebase: None,
            base,
            candidate,
            parked_status: None,
            repair_detail: None,
            parked_cause: None,
            source_head: sync_head_commit_sha(&path).map_err(std::io::Error::other)?,
            source_tree: head_tree_oid_sync(&path).map_err(std::io::Error::other)?,
            directory: Identity::from_stat(&directory.stat()?),
            git: Identity::from_stat(&git.stat()?),
            phase: Phase::Verify,
        };
        self.append_verification(&record)?;
        Ok(record)
    }

    /// Read unresolved driver continuations without consulting worker metadata.
    ///
    /// # Errors
    /// Missing or corrupt authority and malformed retained history are not repaired.
    pub fn verification_continuations(&self) -> Result<Vec<VerificationContinuation>, Error> {
        self.recheck()?;
        let git = Directory::open(&self.operator().join(".git"))?;
        let directory = match git.child(Path::new(LOG)) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        directory.require_private()?;
        let lock = directory.read_private_file(Path::new(".lock"))?;
        lock.lock_shared()?;
        let mut latest = BTreeMap::new();
        for (_, name) in record_names(&directory)? {
            let record: VerificationContinuation =
                serde_json::from_reader(directory.read_private_file(&name)?)?;
            latest.insert(record.bead.as_str().to_owned(), record);
        }
        self.recheck()?;
        Ok(latest
            .into_values()
            .filter(|record| record.phase != Phase::Resolved)
            .collect())
    }

    /// Append one immutable, atomically published recovery event, without minting coverage.
    ///
    /// # Errors
    /// Rejects changed authority or unsafe storage; interrupted staging files remain retained.
    pub fn append_verification(&self, record: &VerificationContinuation) -> Result<(), Error> {
        self.recheck()?;
        let git = Directory::open(&self.operator().join(".git"))?;
        let directory = git.ensure_child(Path::new(LOG))?;
        directory.require_private()?;
        let names = directory.names()?;
        if !names.is_empty() && !names.iter().any(|name| name == ".lock") {
            return Err(std::io::Error::other(
                "retained verification history lacks its synchronization lock",
            )
            .into());
        }
        let lock = directory.append_private_file(Path::new(".lock"))?;
        lock.lock()?;
        let sequence = record_names(&directory)?
            .last()
            .map_or(Ok(1), |(sequence, _)| {
                sequence
                    .checked_add(1)
                    .ok_or_else(|| std::io::Error::other("verification history sequence exhausted"))
            })?;
        // Allocate under the lock, retaining interrupted staging files without clock dependence.
        let existing = directory.names()?;
        let staging = (0..1024)
            .map(|nonce| format!(".pending-{sequence}-{}-{nonce}", std::process::id()))
            .find(|name| !existing.iter().any(|entry| entry == name.as_str()))
            .ok_or_else(|| std::io::Error::other("verification staging names exhausted"))?;
        let published = format!("{sequence:020}.json");
        let mut file = directory.create_file(
            Path::new(&staging),
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )?;
        let mut bytes = serde_json::to_vec(record)?;
        bytes.push(b'\n');
        file.write_all(&bytes)?;
        file.sync_all()?;
        directory.rename_file(Path::new(&staging), Path::new(&published))?;
        directory.sync()?;
        git.sync()?;
        self.recheck()
    }

    /// Preserve publication work across driver loss; this phase carries no coverage proof.
    ///
    /// # Errors
    /// Returns retained-history or authority failures without deleting state.
    pub fn await_verification_publication(
        &self,
        record: &VerificationContinuation,
    ) -> Result<(), Error> {
        let mut record = record.clone();
        record.phase = Phase::Publication;
        self.append_verification(&record)
    }

    /// Retain rejected implementation work until a fresh source candidate supersedes it.
    ///
    /// # Errors
    /// Returns authority/history failures without discarding the source or its recovery intent.
    pub fn await_verification_repair(
        &self,
        record: &VerificationContinuation,
        detail: String,
    ) -> Result<(), Error> {
        let mut record = record.clone();
        record.phase = Phase::Repair;
        record.repair_detail = Some(detail);
        self.append_verification(&record)
    }

    /// Require the original repair checkout while allowing its worker to edit source.
    ///
    /// # Errors
    /// Rejects replacement of the retained checkout or its Git directory.
    pub fn validate_verification_checkout(
        &self,
        record: &VerificationContinuation,
    ) -> Result<(), Error> {
        self.recheck()?;
        let path = self
            .operator()
            .join(".loom/beads")
            .join(record.bead.as_str());
        let directory = Directory::open(&path)?;
        let git = directory.child(Path::new(".git"))?;
        if Identity::from_stat(&directory.stat()?) != record.directory
            || Identity::from_stat(&git.stat()?) != record.git
        {
            return Err(Error::Association {
                reason: "source repair checkout identity changed; retained without dispatch",
            });
        }
        self.recheck()
    }

    /// Require the same independent source checkout and immutable completed source.
    ///
    /// # Errors
    /// Rejects path/device/inode replacement or changed source commits/trees.
    pub fn validate_verification(&self, record: &VerificationContinuation) -> Result<(), Error> {
        self.recheck()?;
        let path = self
            .operator()
            .join(".loom/beads")
            .join(record.bead.as_str());
        let directory = Directory::open(&path)?;
        let git = directory.child(Path::new(".git"))?;
        if Identity::from_stat(&directory.stat()?) != record.directory
            || Identity::from_stat(&git.stat()?) != record.git
            || sync_head_commit_sha(&path).map_err(std::io::Error::other)? != record.source_head
            || head_tree_oid_sync(&path).map_err(std::io::Error::other)? != record.source_tree
        {
            return Err(Error::Association {
                reason: "verification source checkout changed; preserved candidate requires reconciliation",
            });
        }
        self.recheck()
    }

    /// Retain a terminal recovery record; ordinary admission still owns all coverage.
    ///
    /// # Errors
    /// Returns retained-history or association failures without deleting state.
    pub fn finish_verification(&self, record: &VerificationContinuation) -> Result<(), Error> {
        let mut record = record.clone();
        record.phase = Phase::Resolved;
        self.append_verification(&record)
    }
}

impl VerificationContinuation {
    #[must_use]
    pub const fn needs_source_repair(&self) -> bool {
        matches!(self.phase, Phase::Repair)
    }

    #[must_use]
    pub const fn awaiting_publication(&self) -> bool {
        matches!(self.phase, Phase::Publication)
    }
}

fn record_names(directory: &Directory) -> Result<Vec<(u64, std::path::PathBuf)>, Error> {
    let mut names = Vec::new();
    for name in directory.names()? {
        let path = std::path::PathBuf::from(name);
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let sequence = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.parse::<u64>().ok())
                .ok_or_else(|| std::io::Error::other("invalid verification history filename"))?;
            names.push((sequence, path));
        }
    }
    names.sort_by_key(|(sequence, _)| *sequence);
    Ok(names)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "isolated authority fixture failures abort the test"
    )]
    use super::*;

    #[tokio::test]
    async fn verification_history_serializes_contending_writers_and_readers() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("operator");
        let git = crate::git::init_test_repo_with_integration(&workspace).unwrap();
        let association = Association::resolve(&workspace).unwrap();
        let label = SpecLabel::new("gate").unwrap();
        let mut records = Vec::new();
        for index in 0..4 {
            let bead = BeadId::new(&format!("lm-contention{index}")).unwrap();
            let source = git.create_worktree(&label, &bead).await.unwrap();
            let head = sync_head_commit_sha(&source.path).unwrap();
            records.push(
                association
                    .begin_verification(bead, label.clone(), None, head.clone(), head)
                    .unwrap(),
            );
        }
        let events = workspace.join(".git").join(LOG);
        let interrupted = events.join(".pending-interrupted");
        std::fs::write(&interrupted, b"interrupted bytes").unwrap();
        let barrier = std::sync::Barrier::new(5);
        std::thread::scope(|scope| {
            for record in &records {
                let workspace = &workspace;
                let barrier = &barrier;
                scope.spawn(move || {
                    let association = Association::resolve(workspace).unwrap();
                    barrier.wait();
                    for _ in 0..8 {
                        association.append_verification(record).unwrap();
                    }
                    association.await_verification_publication(record).unwrap();
                });
            }
            barrier.wait();
            for _ in 0..16 {
                assert_eq!(association.verification_continuations().unwrap().len(), 4);
            }
        });
        let retained = association.verification_continuations().unwrap();
        assert_eq!(retained.len(), 4);
        assert!(
            retained
                .iter()
                .all(VerificationContinuation::awaiting_publication)
        );
        for record in &retained {
            association.validate_verification(record).unwrap();
        }
        let names = record_names(&Directory::open(&events).unwrap()).unwrap();
        assert_eq!(names.len(), 40);
        assert_eq!(
            names
                .iter()
                .map(|(sequence, _)| *sequence)
                .collect::<Vec<_>>(),
            (1..=40).collect::<Vec<_>>()
        );
        assert_eq!(std::fs::read(&interrupted).unwrap(), b"interrupted bytes");
    }

    #[tokio::test]
    async fn verification_continuation_rejects_replaced_source_missing_authority_and_corrupt_history()
     {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("operator");
        let git = crate::git::init_test_repo_with_integration(&workspace).unwrap();
        let association = Association::resolve(&workspace).unwrap();
        let bead = BeadId::new("lm-vstate").unwrap();
        let label = SpecLabel::new("gate").unwrap();
        let source = git.create_worktree(&label, &bead).await.unwrap();
        let head = sync_head_commit_sha(&source.path).unwrap();
        let record = association
            .begin_verification(bead, label, None, head.clone(), head)
            .unwrap();
        association.validate_verification(&record).unwrap();
        std::fs::write(source.path.join(".git").join(LOG), "worker-forged-state\n").unwrap();
        assert_eq!(association.verification_continuations().unwrap().len(), 1);
        let source_git = source.path.join(".git");
        let saved_git = source.path.join("saved-git");
        std::fs::rename(&source_git, &saved_git).unwrap();
        std::fs::create_dir(&source_git).unwrap();
        assert!(association.validate_verification(&record).is_err());
        std::fs::remove_dir(&source_git).unwrap();
        std::fs::rename(&saved_git, &source_git).unwrap();
        association.validate_verification(&record).unwrap();
        let binding = workspace.join(".git/loom-workspace.json");
        let saved_binding = workspace.join(".git/saved-workspace.json");
        std::fs::rename(&binding, &saved_binding).unwrap();
        assert!(association.verification_continuations().is_err());
        assert!(!binding.exists());
        std::fs::rename(&saved_binding, &binding).unwrap();
        association.validate_verification(&record).unwrap();
        let events = workspace.join(".git").join(LOG);
        let lock = events.join(".lock");
        let saved_lock = events.join(".saved-lock");
        std::fs::rename(&lock, &saved_lock).unwrap();
        assert!(association.verification_continuations().is_err());
        assert!(association.append_verification(&record).is_err());
        assert!(!lock.exists());
        std::fs::rename(&saved_lock, &lock).unwrap();
        assert_eq!(association.verification_continuations().unwrap().len(), 1);
        std::fs::write(
            events.join(".pending-interrupted"),
            b"partial staging record",
        )
        .unwrap();
        assert_eq!(association.verification_continuations().unwrap().len(), 1);
        let journal = events.join("00000000000000000001.json");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&journal)
            .unwrap();
        file.write_all(b"malformed retained history\n").unwrap();
        let bytes = std::fs::read(&journal).unwrap();
        assert!(association.verification_continuations().is_err());
        assert_eq!(std::fs::read(&journal).unwrap(), bytes);
    }
}
