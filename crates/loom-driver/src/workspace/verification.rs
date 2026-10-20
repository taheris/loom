//! Transient driver recovery snapshots in the protected operator state database.
//! These records retain unfinished driver work; they never authenticate coverage.

use crate::state::{CacheDb, RecordKind};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{Association, Error, Identity};
use crate::bd::Status;
use crate::git::{GitOid, head_tree_oid_sync, sync_head_commit_sha};
use crate::identifier::{BeadId, SpecLabel};
use crate::scratch::directory::Directory;

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

/// Observed post-integration failure retained for the next source-repair prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationFailure {
    pub target: String,
    pub exit_code: i32,
    pub output: String,
    pub log_path: std::path::PathBuf,
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
    pub repair_failure: Option<VerificationFailure>,
    #[serde(default)]
    pub semantic_failures: u32,
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
    fn verification_lifecycle_lock(&self) -> Result<std::fs::File, Error> {
        self.recheck()?;
        let git = Directory::open(&self.operator().join(".git"))?;
        let lock = git.append_private_file(Path::new("loom-verification-lifecycle.lock"))?;
        lock.lock()?;
        self.recheck()?;
        Ok(lock)
    }

    /// Remove a bead clone only when no unresolved continuation retains it.
    ///
    /// # Errors
    /// Unavailable authority, malformed history and filesystem failures fail closed.
    pub fn remove_verification_checkout(&self, bead: &BeadId) -> Result<bool, Error> {
        let _lock = self.verification_lifecycle_lock()?;
        let path = self.operator().join(".loom/beads").join(bead.as_str());
        if self
            .verification_continuations()?
            .iter()
            .any(|record| record.bead == *bead)
        {
            tracing::info!(%bead, path = %path.display(), "checkout retained by unresolved verification continuation");
            return Ok(false);
        }
        if path.exists()
            && !crate::git::status_porcelain_sync(&path)
                .map_err(std::io::Error::other)?
                .trim()
                .is_empty()
        {
            tracing::info!(%bead, "dirty checkout preserved during cleanup");
            return Ok(false);
        }
        let resolved = self
            .state_db()?
            .record::<VerificationContinuation>(RecordKind::Verification, bead.as_str())?
            .is_some_and(|record| record.phase == Phase::Resolved);
        if path.exists()
            && !crate::git::checkout_redundant_sync(&path, self.integration(), resolved)
                .map_err(std::io::Error::other)?
        {
            tracing::info!(%bead, "checkout with unique Git data preserved without local cleanup disposition");
            return Ok(false);
        }
        match Directory::open(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(source) => {
                return Err(verification_io(
                    "open cleanup checkout",
                    &path,
                    bead,
                    source,
                ));
            }
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(source) => Err(verification_io("remove checkout", &path, bead, source)),
        }
    }

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
        let _lock = self.verification_lifecycle_lock()?;
        let path = self.operator().join(".loom/beads").join(bead.as_str());
        let (directory, git) = verification_directories(&path, &bead)?;
        let semantic_failures = self
            .verification_continuations()?
            .into_iter()
            .find(|record| record.bead == bead)
            .map_or(0, |record| record.semantic_failures);
        let directory = verification_identity(&directory, &path, &bead)?;
        let git = verification_identity(&git, &path.join(".git"), &bead)?;
        let source_head = sync_head_commit_sha(&path).map_err(|error| {
            verification_io(
                "read source HEAD",
                &path,
                &bead,
                std::io::Error::other(error),
            )
        })?;
        let source_tree = head_tree_oid_sync(&path).map_err(|error| {
            verification_io(
                "read source tree",
                &path,
                &bead,
                std::io::Error::other(error),
            )
        })?;
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
            repair_failure: None,
            semantic_failures,
            parked_cause: None,
            source_head,
            source_tree,
            directory,
            git,
            phase: Phase::Verify,
        };
        self.append_verification_locked(&record)?;
        Ok(record)
    }

    /// Read unresolved driver continuations without consulting worker metadata.
    ///
    /// # Errors
    /// Missing or corrupt authority and malformed retained history are not repaired.
    pub fn verification_continuations(&self) -> Result<Vec<VerificationContinuation>, Error> {
        self.recheck()?;
        let mut pending = Vec::new();
        for (key, record) in self
            .state_db()?
            .records::<VerificationContinuation>(RecordKind::Verification)?
        {
            if key != record.bead.as_str() {
                return Err(Error::Association {
                    reason: "verification record key changed",
                });
            }
            if record.phase != Phase::Resolved {
                pending.push(record);
            }
        }
        self.recheck()?;
        Ok(pending)
    }

    fn state_db(&self) -> Result<CacheDb, Error> {
        Ok(CacheDb::open(crate::state::path(self.operator()))?)
    }

    /// Replace an operation snapshot without admitting verification coverage.
    ///
    /// # Errors
    /// Reports current checkout and database failures.
    pub fn append_verification(&self, record: &VerificationContinuation) -> Result<(), Error> {
        let _lock = self.verification_lifecycle_lock()?;
        self.append_verification_locked(record)
    }

    fn append_verification_locked(&self, record: &VerificationContinuation) -> Result<(), Error> {
        self.validate_verification_checkout(record)?;
        self.state_db()?
            .put_record(RecordKind::Verification, record.bead.as_str(), record)?;
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
        failure: Option<VerificationFailure>,
    ) -> Result<(), Error> {
        let mut record = record.clone();
        if !record.needs_source_repair() {
            record.semantic_failures =
                record.semantic_failures.checked_add(1).ok_or_else(|| {
                    std::io::Error::other("semantic verification failure count exhausted")
                })?;
        }
        record.repair_failure = failure;
        record.publication_rebase = None;
        record.phase = Phase::Repair;
        record.repair_detail = Some(detail);
        self.append_verification(&record)
    }

    /// Bind independently checked recovery storage, discarding unavailable verification reuse.
    ///
    /// This captures local storage identity only. It never admits coverage, changes a
    /// native task, or removes existing work.
    ///
    /// # Errors
    /// Reports unavailable independent storage or concurrent operation changes.
    pub fn rebind_verification_checkout(
        &self,
        record: &VerificationContinuation,
        reconstructed: bool,
    ) -> Result<VerificationContinuation, Error> {
        let _lock = self.verification_lifecycle_lock()?;
        if self.validate_verification_checkout(record).is_ok() && !reconstructed {
            return Ok(record.clone());
        }
        let path = self
            .operator()
            .join(".loom/beads")
            .join(record.bead.as_str());
        let (directory, git) = verification_directories(&path, &record.bead)?;
        let head = sync_head_commit_sha(&path).map_err(std::io::Error::other)?;
        let tree = head_tree_oid_sync(&path).map_err(std::io::Error::other)?;
        let dirty = !crate::git::status_porcelain_sync(&path)
            .map_err(std::io::Error::other)?
            .trim()
            .is_empty();
        let mut next = record.clone();
        next.directory = verification_identity(&directory, &path, &record.bead)?;
        next.git = verification_identity(&git, &path.join(".git"), &record.bead)?;
        next.publication_rebase = None;
        if next.needs_source_repair()
            || dirty
            || (!reconstructed && (head != record.source_head || tree != record.source_tree))
        {
            next.phase = Phase::Repair;
            next.repair_detail = Some("Recovery storage contains unfinished or changed source; preserve it for implementation repair and fresh verification".into());
        } else {
            next.phase = Phase::Verify;
        }
        next.source_head = head;
        next.source_tree = tree;
        self.append_verification_locked(&next)?;
        Ok(next)
    }

    /// Require this operation's current repair checkout while allowing its worker to edit source.
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
        let (directory, git) = verification_directories(&path, &record.bead)?;
        if verification_identity(&directory, &path, &record.bead)? != record.directory
            || verification_identity(&git, &path.join(".git"), &record.bead)? != record.git
        {
            return Err(Error::Association {
                reason: "source repair checkout identity changed; retained without dispatch",
            });
        }
        self.recheck()
    }

    /// Reconcile source edits as unfinished repair, never as verified coverage.
    ///
    /// # Errors
    /// Rejects replaced checkouts or unavailable retained history.
    pub fn reconcile_verification_source(
        &self,
        record: &VerificationContinuation,
    ) -> Result<VerificationContinuation, Error> {
        self.validate_verification_checkout(record)?;
        if record.needs_source_repair() || self.verification_source_unchanged(record)? {
            return Ok(record.clone());
        }
        let mut record = record.clone();
        record.phase = Phase::Repair;
        record.publication_rebase = None;
        record.repair_detail = Some(
            "Source checkout changed after candidate capture; retained candidate and source edits require fresh implementation verification".into(),
        );
        self.append_verification(&record)?;
        Ok(record)
    }

    /// Check completed source content while requiring its original checkout identity.
    ///
    /// # Errors
    /// Rejects replaced checkout storage or unavailable Git state.
    pub fn verification_source_unchanged(
        &self,
        record: &VerificationContinuation,
    ) -> Result<bool, Error> {
        self.validate_verification_checkout(record)?;
        let path = self
            .operator()
            .join(".loom/beads")
            .join(record.bead.as_str());
        let unchanged = sync_head_commit_sha(&path).map_err(|error| {
            verification_io(
                "read source HEAD",
                &path,
                &record.bead,
                std::io::Error::other(error),
            )
        })? == record.source_head
            && head_tree_oid_sync(&path).map_err(|error| {
                verification_io(
                    "read source tree",
                    &path,
                    &record.bead,
                    std::io::Error::other(error),
                )
            })? == record.source_tree;
        self.recheck()?;
        Ok(unchanged)
    }

    /// Require the same independent source checkout and immutable completed source.
    ///
    /// # Errors
    /// Rejects path/device/inode replacement or changed source commits/trees.
    pub fn validate_verification(&self, record: &VerificationContinuation) -> Result<(), Error> {
        if !self.verification_source_unchanged(record)? {
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

fn verification_io(
    operation: &'static str,
    path: &Path,
    bead: &BeadId,
    source: std::io::Error,
) -> Error {
    Error::VerificationFilesystem {
        operation,
        path: path.to_path_buf(),
        bead: bead.clone(),
        source,
    }
}

fn verification_identity(
    directory: &Directory,
    path: &Path,
    bead: &BeadId,
) -> Result<Identity, Error> {
    let stat = directory
        .stat()
        .map_err(|source| verification_io("stat verification directory", path, bead, source))?;
    Ok(Identity::from_stat(&stat))
}

fn verification_directories(path: &Path, bead: &BeadId) -> Result<(Directory, Directory), Error> {
    let directory = Directory::open(path)
        .map_err(|source| verification_io("open verification checkout", path, bead, source))?;
    let git = directory.child(Path::new(".git")).map_err(|source| {
        verification_io(
            "open verification Git directory",
            &path.join(".git"),
            bead,
            source,
        )
    })?;
    Ok((directory, git))
}

/// Whether this workspace has local verification recovery snapshots.
///
/// # Errors
/// Reports malformed state rather than treating it as absence.
pub fn has_verification_state(workspace: &Path) -> Result<bool, Error> {
    Ok(!CacheDb::open(crate::state::path(workspace))?
        .records::<VerificationContinuation>(RecordKind::Verification)?
        .is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        crate::git::GitClient,
        VerificationContinuation,
    ) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("operator");
        let git = crate::git::init_test_repo_with_integration(&workspace).unwrap();
        let bead = BeadId::new("lm-cleanup").unwrap();
        let label = SpecLabel::new("gate").unwrap();
        let source = git.create_worktree(&label, &bead).await.unwrap();
        let head = sync_head_commit_sha(&source.path).unwrap();
        git.preserve_verification_candidate(&bead, &head)
            .await
            .unwrap();
        let record = Association::resolve(&workspace)
            .unwrap()
            .begin_verification(bead, label, None, head.clone(), head)
            .unwrap();
        (root, workspace, git, record)
    }

    #[tokio::test]
    async fn cleanup_retains_every_unresolved_phase_across_restart() {
        let (_root, workspace, git, record) = fixture().await;
        let path = workspace.join(".loom/beads").join(record.bead.as_str());
        for phase in [Phase::Verify, Phase::Repair, Phase::Publication] {
            let mut record = record.clone();
            record.phase = phase;
            Association::resolve(&workspace)
                .unwrap()
                .append_verification(&record)
                .unwrap();
            assert!(!git.remove_worktree(&path).await.unwrap());
        }
        let association = Association::resolve(&workspace).unwrap();
        association.finish_verification(&record).unwrap();
        assert!(git.remove_worktree(&path).await.unwrap());
        assert!(git.remove_worktree(&path).await.unwrap());
        assert!(
            !workspace
                .join(".git/loom-verification-continuations")
                .exists()
        );
    }

    #[tokio::test]
    async fn cleanup_serializes_with_continuation_capture() {
        let (_root, workspace, _git, record) = fixture().await;
        let association = Association::resolve(&workspace).unwrap();
        association.finish_verification(&record).unwrap();
        let lock = association.verification_lifecycle_lock().unwrap();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                barrier.wait();
                Association::resolve(&workspace)
                    .unwrap()
                    .remove_verification_checkout(&record.bead)
                    .unwrap()
            });
            barrier.wait();
            association.append_verification_locked(&record).unwrap();
            drop(lock);
            assert!(!worker.join().unwrap());
        });
    }

    #[tokio::test]
    async fn reset_does_not_authorize_discarding_dirty_work() {
        let (_root, workspace, git, record) = fixture().await;
        let path = workspace.join(".loom/beads").join(record.bead.as_str());
        std::fs::write(path.join("unfinished.txt"), "irreplaceable work").unwrap();
        drop(CacheDb::recreate(crate::state::path(&workspace)).unwrap());
        assert!(!git.remove_worktree(&path).await.unwrap());
        assert_eq!(
            std::fs::read_to_string(path.join("unfinished.txt")).unwrap(),
            "irreplaceable work"
        );
    }

    #[tokio::test]
    async fn verification_snapshots_serialize_concurrent_writers() {
        let (_root, workspace, _git, record) = fixture().await;
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..8 {
                        Association::resolve(&workspace)
                            .unwrap()
                            .append_verification(&record)
                            .unwrap();
                    }
                });
            }
        });
        let association = Association::resolve(&workspace).unwrap();
        assert_eq!(association.verification_continuations().unwrap().len(), 1);
        association.validate_verification(&record).unwrap();
    }

    #[tokio::test]
    async fn missing_checkout_is_reconstructed_without_restoring_publication_coverage() {
        let (_root, workspace, git, record) = fixture().await;
        let association = Association::resolve(&workspace).unwrap();
        association.await_verification_publication(&record).unwrap();
        let record = association.verification_continuations().unwrap().remove(0);
        let path = workspace.join(".loom/beads").join(record.bead.as_str());
        std::fs::remove_dir_all(&path).unwrap();
        let recovered = git.recover_verification_checkout(&record).await.unwrap();
        assert_eq!(recovered.phase, Phase::Verify);
        assert!(recovered.publication_rebase.is_none());
        assert_eq!(sync_head_commit_sha(&path).unwrap(), record.candidate);
        association.validate_verification(&recovered).unwrap();
        assert!(!git.remove_worktree(&path).await.unwrap());
    }

    #[tokio::test]
    async fn replacement_checkout_retains_dirty_work_and_requires_source_repair() {
        let (_root, workspace, git, record) = fixture().await;
        let path = workspace.join(".loom/beads").join(record.bead.as_str());
        let original = path.with_extension("saved");
        std::fs::rename(&path, &original).unwrap();
        git.create_worktree(&record.label, &record.bead)
            .await
            .unwrap();
        std::fs::write(path.join("human-draft.txt"), "keep these bytes").unwrap();
        let recovered = git.recover_verification_checkout(&record).await.unwrap();
        assert_eq!(recovered.phase, Phase::Repair);
        assert_eq!(
            std::fs::read_to_string(path.join("human-draft.txt")).unwrap(),
            "keep these bytes"
        );
        assert!(original.is_dir());
        assert!(!git.remove_worktree(&path).await.unwrap());
    }

    #[tokio::test]
    async fn invalid_database_snapshot_never_authorizes_cleanup() {
        let (_root, workspace, git, record) = fixture().await;
        CacheDb::open(crate::state::path(&workspace))
            .unwrap()
            .put_record(
                RecordKind::Verification,
                record.bead.as_str(),
                &"invalid snapshot",
            )
            .unwrap();
        let path = workspace.join(".loom/beads").join(record.bead.as_str());
        assert!(git.remove_worktree(&path).await.is_err());
        assert!(path.exists());
    }
}
