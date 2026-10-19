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
        let git = Directory::open(&self.operator().join(".git"))?;
        match git.child(Path::new(LOG)) {
            Ok(directory) => {
                if directory
                    .names()?
                    .iter()
                    .any(|name| name.to_string_lossy().starts_with(".pending-"))
                {
                    return Err(Error::Association {
                        reason: "interrupted verification append requires provenance-aware recovery; all checkout cleanup is blocked and staging records must be preserved",
                    });
                }
                directory.sync()?;
                git.sync()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if self
            .verification_continuations()?
            .iter()
            .any(|record| record.bead == *bead)
        {
            tracing::info!(%bead, path = %path.display(), "checkout retained by unresolved verification continuation");
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
        let _lock = self.verification_lifecycle_lock()?;
        self.append_verification_locked(record)
    }

    fn append_verification_locked(&self, record: &VerificationContinuation) -> Result<(), Error> {
        self.validate_verification_checkout(record)?;
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

    async fn cleanup_fixture() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        crate::git::GitClient,
        VerificationContinuation,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("operator");
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
        (dir, workspace, git, record)
    }

    #[tokio::test]
    async fn startup_sweep_rechecks_continuations_created_after_its_inventory() {
        struct CaptureDuringLookup {
            workspace: std::path::PathBuf,
            record: VerificationContinuation,
        }
        impl crate::bd::CommandRunner for CaptureDuringLookup {
            async fn run(
                &self,
                args: Vec<std::ffi::OsString>,
                _timeout: std::time::Duration,
            ) -> Result<crate::bd::RunOutput, crate::bd::BdError> {
                assert_eq!(args[0], "show");
                assert_eq!(args[1], self.record.bead.as_str());
                Association::resolve(&self.workspace)
                    .unwrap()
                    .append_verification(&self.record)
                    .unwrap();
                Ok(crate::bd::RunOutput {
                    status: 0,
                    stdout: serde_json::to_vec(&serde_json::json!([{
                        "id": self.record.bead, "title": "completed source", "status": "closed", "parent": "lm-root"
                    }])).unwrap(),
                    stderr: Vec::new(),
                })
            }
        }
        let (_dir, workspace, git, record) = cleanup_fixture().await;
        let association = Association::resolve(&workspace).unwrap();
        association.finish_verification(&record).unwrap();
        let bd = crate::bd::BdClient::with_runner(CaptureDuringLookup {
            workspace: workspace.clone(),
            record: record.clone(),
        });
        let removed = git
            .sweep_orphan_bead_clones(&bd, &crate::identifier::MoleculeId::new("lm-root").unwrap())
            .await
            .unwrap();
        assert_eq!(removed, Vec::<std::path::PathBuf>::new());
        association.validate_verification(&record).unwrap();
        assert_eq!(association.verification_continuations().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cleanup_retains_every_unresolved_phase_across_restart() {
        let (_dir, workspace, git, record) = cleanup_fixture().await;
        let path = workspace.join(".loom/beads").join(record.bead.as_str());
        for phase in [Phase::Verify, Phase::Repair, Phase::Publication] {
            let mut retained = record.clone();
            retained.phase = phase;
            Association::resolve(&workspace)
                .unwrap()
                .append_verification(&retained)
                .unwrap();
            assert!(!git.remove_worktree(&path).await.unwrap());
            Association::resolve(&workspace)
                .unwrap()
                .validate_verification(&retained)
                .unwrap();
        }
    }

    #[tokio::test]
    async fn durable_resolution_allows_idempotent_cleanup_without_removing_history_or_pins() {
        let (_dir, workspace, git, record) = cleanup_fixture().await;
        let association = Association::resolve(&workspace).unwrap();
        association.await_verification_publication(&record).unwrap();
        let path = workspace.join(".loom/beads").join(record.bead.as_str());
        assert!(!git.remove_worktree(&path).await.unwrap());
        association.finish_verification(&record).unwrap();
        assert!(git.remove_worktree(&path).await.unwrap());
        assert!(git.remove_worktree(&path).await.unwrap());
        assert!(!path.exists());
        assert!(
            Association::resolve(&workspace)
                .unwrap()
                .verification_continuations()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            record_names(&Directory::open(&workspace.join(".git").join(LOG)).unwrap())
                .unwrap()
                .len(),
            3
        );
        let pin = format!(
            "refs/loom/verification/{}/{}",
            record.bead, record.candidate
        );
        assert_eq!(
            crate::git::sync_rev_parse(&git.loom_workspace(), &pin).unwrap(),
            record.candidate
        );
        assert!(association.await_verification_publication(&record).is_err());
    }

    #[tokio::test]
    async fn missing_checkout_refuses_recovery_without_mutating_journal_or_candidate_pin() {
        let (_dir, workspace, git, record) = cleanup_fixture().await;
        let path = workspace.join(".loom/beads").join(record.bead.as_str());
        std::fs::remove_dir_all(&path).unwrap();
        let association = Association::resolve(&workspace).unwrap();
        let error = association
            .validate_verification(&record)
            .unwrap_err()
            .to_string();
        assert!(error.contains("open verification checkout"));
        assert!(error.contains(path.to_str().unwrap()));
        assert!(error.contains(record.bead.as_str()));
        assert!(error.contains("provenance-aware recovery"));
        assert!(association.finish_verification(&record).is_err());
        assert!(!git.remove_worktree(&path).await.unwrap());
        assert_eq!(association.verification_continuations().unwrap().len(), 1);
        assert_eq!(
            record_names(&Directory::open(&workspace.join(".git").join(LOG)).unwrap())
                .unwrap()
                .len(),
            1
        );
        let pin = format!(
            "refs/loom/verification/{}/{}",
            record.bead, record.candidate
        );
        assert_eq!(
            crate::git::sync_rev_parse(&git.loom_workspace(), &pin).unwrap(),
            record.candidate
        );
    }

    #[tokio::test]
    async fn failed_resolution_append_preserves_checkout_and_unresolved_record() {
        let (_dir, workspace, git, record) = cleanup_fixture().await;
        let events = workspace.join(".git").join(LOG);
        let lock = events.join(".lock");
        let saved_lock = events.join(".saved-lock");
        std::fs::rename(&lock, &saved_lock).unwrap();
        let association = Association::resolve(&workspace).unwrap();
        assert!(association.finish_verification(&record).is_err());
        let path = workspace.join(".loom/beads").join(record.bead.as_str());
        assert!(git.remove_worktree(&path).await.is_err());
        assert!(path.exists());
        assert!(!lock.exists());
        std::fs::rename(&saved_lock, &lock).unwrap();
        assert!(!git.remove_worktree(&path).await.unwrap());
        assert_eq!(association.verification_continuations().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn interrupted_resolution_staging_never_authorizes_cleanup() {
        let (_dir, workspace, git, record) = cleanup_fixture().await;
        let events = workspace.join(".git").join(LOG);
        let mut resolved = record.clone();
        resolved.phase = Phase::Resolved;
        let staging = events.join(".pending-resolution");
        let bytes = serde_json::to_vec(&resolved).unwrap();
        std::fs::write(&staging, &bytes).unwrap();
        let path = workspace.join(".loom/beads").join(record.bead.as_str());
        assert!(git.remove_worktree(&path).await.is_err());
        assert!(path.exists());
        assert_eq!(std::fs::read(staging).unwrap(), bytes);
        assert_eq!(
            Association::resolve(&workspace)
                .unwrap()
                .verification_continuations()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn process_exit_releases_lifecycle_lock_but_retains_interrupted_intent() {
        const CHILD: &str = "LOOM_TEST_CLEANUP_CRASH_WORKSPACE";
        if let Some(workspace) = std::env::var_os(CHILD) {
            let workspace = std::path::PathBuf::from(workspace);
            let association = Association::resolve(&workspace).unwrap();
            let _lock = association.verification_lifecycle_lock().unwrap();
            let events = workspace.join(".git").join(LOG);
            let mut file = std::fs::File::create(events.join(".pending-crash")).unwrap();
            file.write_all(b"interrupted append").unwrap();
            file.sync_all().unwrap();
            Directory::open(&events).unwrap().sync().unwrap();
            std::process::exit(73);
        }
        let (_dir, workspace, git, record) = cleanup_fixture().await;
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "workspace::verification::tests::process_exit_releases_lifecycle_lock_but_retains_interrupted_intent"])
            .env(CHILD, &workspace)
            .status().unwrap();
        assert_eq!(status.code(), Some(73));
        let association = Association::resolve(&workspace).unwrap();
        let lock = association.verification_lifecycle_lock().unwrap();
        drop(lock);
        let path = workspace.join(".loom/beads").join(record.bead.as_str());
        assert!(git.remove_worktree(&path).await.is_err());
        assert!(path.exists());
        assert_eq!(association.verification_continuations().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cleanup_serializes_with_continuation_capture() {
        let (_dir, workspace, _git, record) = cleanup_fixture().await;
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
        association.validate_verification(&record).unwrap();
    }

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
