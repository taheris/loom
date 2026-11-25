//! Actual Git endpoints for publication impact analysis; never execution evidence.

use std::collections::BTreeMap;

use displaydoc::Display;
use loom_driver::git::{
    CommitSource, GitClient, GitOid, status_porcelain_sync, sync_head_commit_sha,
};
use thiserror::Error;

use super::snapshot::{ResourcePath, Snapshot};

/// Literal path changes, including mode-only changes. A move is a removal and an addition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Removed,
    Modified,
}

/// Two checked Git object trees and a live snapshot bound to the pushed head.
///
/// The caller still owns the trust decision for the remote endpoints and the
/// proof of compile/runtime impact. Equality here is not a test result.
#[derive(Debug)]
pub struct Sources {
    snapshot: Snapshot,
    before: CommitSource,
    after: CommitSource,
    changes: BTreeMap<ResourcePath, Change>,
}

impl Sources {
    /// Read both exact commits and bind the head to the captured live checkout.
    /// Never resolves a branch name, merge base, caller's diff, or partial tree.
    ///
    /// # Errors
    /// Fails on unknown inputs, malformed trees, or a draft checkout that is not
    /// the pushed commit's exact bytes, tracked membership and executable modes.
    pub fn capture(snapshot: &Snapshot, prior: &GitOid, head: &GitOid) -> Result<Self, Error> {
        require_head(snapshot, head)?;
        let git = GitClient::open(snapshot.root())?;
        let before = git.source_at(prior)?;
        let after = git.source_at(head)?;
        let mut changes = BTreeMap::new();
        for (path, new) in after.files() {
            let resource = ResourcePath::try_from(path.clone())?;
            if snapshot
                .files()
                .get(&resource)
                .is_none_or(|bytes| bytes != new.bytes())
                || snapshot.is_executable(&resource) != new.is_executable()
            {
                return Err(Error::Source { path: resource });
            }
            match before.files().get(path) {
                None => {
                    changes.insert(resource, Change::Added);
                }
                Some(old)
                    if old.bytes() != new.bytes() || old.is_executable() != new.is_executable() =>
                {
                    changes.insert(resource, Change::Modified);
                }
                Some(_) => {}
            }
        }
        if let Some(path) = snapshot
            .files()
            .keys()
            .find(|p| !after.files().contains_key(p.as_path()))
        {
            return Err(Error::Source { path: path.clone() });
        }
        for path in before.files().keys() {
            let resource = ResourcePath::try_from(path.clone())?;
            if !after.files().contains_key(path) {
                changes.insert(resource, Change::Removed);
            }
        }
        let sources = Self {
            snapshot: snapshot.clone(),
            before,
            after,
            changes,
        };
        sources.require_current()?;
        Ok(sources)
    }

    /// Recheck the checkout and its exact commit before consuming the comparison.
    ///
    /// # Errors
    /// Rejects changed files, modes, membership or HEAD, and a dirty Git index.
    pub fn require_current(&self) -> Result<(), Error> {
        self.snapshot.require_current()?;
        require_head(&self.snapshot, self.after.commit())?;
        if !status_porcelain_sync(self.snapshot.root())?.is_empty() {
            return Err(Error::Dirty);
        }
        Ok(())
    }

    pub const fn before(&self) -> &CommitSource {
        &self.before
    }

    pub const fn after(&self) -> &CommitSource {
        &self.after
    }

    pub const fn changes(&self) -> &BTreeMap<ResourcePath, Change> {
        &self.changes
    }
}

fn require_head(snapshot: &Snapshot, expected: &GitOid) -> Result<(), Error> {
    let actual = sync_head_commit_sha(snapshot.root())?;
    if &actual != expected {
        return Err(Error::Head {
            actual,
            expected: expected.clone(),
        });
    }
    Ok(())
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// cannot read exact committed source
    Git(#[from] loom_driver::git::GitError),
    /// cannot bind the publication source snapshot
    Snapshot(#[from] super::snapshot::Error),
    /// current checkout commit {actual} is not the pushed commit {expected}
    Head { actual: GitOid, expected: GitOid },
    /// captured checkout disagrees with the pushed tree at {path:?}
    Source { path: ResourcePath },
    /// publication source comparison requires a clean index and worktree
    Dirty,
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::git::{commit_all_in, init_test_repo};
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    fn resource(path: &str) -> ResourcePath {
        ResourcePath::try_from(std::path::PathBuf::from(path)).unwrap()
    }

    #[test]
    fn committed_source_changes_include_both_move_paths_mode_and_policy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_test_repo(root).unwrap();
        fs::create_dir(root.join("sender")).unwrap();
        fs::create_dir(root.join("receiver")).unwrap();
        fs::write(root.join("sender/case.rs"), "pub fn probe() {}\n").unwrap();
        fs::write(root.join("helper.sh"), "exit 0\n").unwrap();
        fs::set_permissions(root.join("helper.sh"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(root.join("loom.toml"), "# Initial policy\n").unwrap();
        commit_all_in(root, "Capture prior sources").unwrap();
        let before = sync_head_commit_sha(root).unwrap();
        fs::rename(root.join("sender/case.rs"), root.join("receiver/case.rs")).unwrap();
        fs::set_permissions(root.join("helper.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(root.join("loom.toml"), "# Changed policy\n").unwrap();
        commit_all_in(root, "Move source and change policy").unwrap();
        let after = sync_head_commit_sha(root).unwrap();
        let snapshot = Snapshot::discover(root).unwrap();
        let comparison = Sources::capture(&snapshot, &before, &after).unwrap();
        assert_eq!(comparison.before().commit(), &before);
        assert_eq!(comparison.after().commit(), &after);
        assert_eq!(
            comparison.changes(),
            &BTreeMap::from([
                (resource("sender/case.rs"), Change::Removed),
                (resource("receiver/case.rs"), Change::Added),
                (resource("helper.sh"), Change::Modified),
                (resource("loom.toml"), Change::Modified),
            ])
        );
        comparison.require_current().unwrap();
        fs::write(root.join("untracked.txt"), "not in pushed tree\n").unwrap();
        assert!(comparison.require_current().is_err());
        let draft = Snapshot::discover(root).unwrap();
        assert!(
            matches!(Sources::capture(&draft, &before, &after), Err(Error::Source { path }) if path == resource("untracked.txt"))
        );
    }

    #[test]
    fn staged_only_divergence_is_not_a_clean_publication_source() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_test_repo(root).unwrap();
        fs::write(root.join("source.rs"), "pub fn probe() {}\n").unwrap();
        commit_all_in(root, "Source").unwrap();
        let head = sync_head_commit_sha(root).unwrap();
        let snapshot = Snapshot::discover(root).unwrap();
        fs::write(root.join("source.rs"), "pub fn draft() {}\n").unwrap();
        assert!(
            GitClient::fixture_command(root)
                .args(["add", "--", "source.rs"])
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.join("source.rs"), "pub fn probe() {}\n").unwrap();
        snapshot.require_current().unwrap();
        assert!(matches!(
            Sources::capture(&snapshot, &head, &head),
            Err(Error::Dirty)
        ));
    }

    #[test]
    fn same_tree_under_another_commit_is_not_the_pushed_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_test_repo(root).unwrap();
        fs::write(root.join("source.rs"), "pub fn probe() {}\n").unwrap();
        commit_all_in(root, "Source").unwrap();
        let prior = sync_head_commit_sha(root).unwrap();
        let git = GitClient::open(root).unwrap();
        let output = GitClient::fixture_command(root)
            .args([
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-qm",
                "Equal tree",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        let actual = sync_head_commit_sha(root).unwrap();
        assert_eq!(
            git.source_at(&prior).unwrap().tree(),
            git.source_at(&actual).unwrap().tree()
        );
        assert_ne!(prior, actual);
        let snapshot = Snapshot::discover(root).unwrap();
        assert!(
            matches!(Sources::capture(&snapshot, &prior, &prior), Err(Error::Head { actual: got, expected }) if got == actual && expected == prior)
        );
    }
}
