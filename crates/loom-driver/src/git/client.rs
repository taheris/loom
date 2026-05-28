use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use tokio::task::spawn_blocking;

use crate::clock::{Clock, SystemClock};
use crate::identifier::{BeadId, SpecLabel};

use super::error::GitError;

const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Timeout for git operations whose hooks can legitimately run for
/// minutes — pre-push fires the workspace's pre-push CI stage (nextest +
/// nix build), which on a warm sccache takes a few minutes. The timeout
/// surfaces true hangs (deadlocked subprocess, runaway network); it must
/// not abort legitimate CI.
const GIT_HOOK_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const WORKTREE_BASE: &str = ".wrapix/worktree";
const BRANCH_PREFIX: &str = "loom";

/// Single typed surface for git operations.
///
/// Backend split is internal: `gix` handles read-only inspection (status,
/// diff, refs, commit graph, worktree iteration); the `git` CLI handles
/// worktree mutation and merge-back. Callers see only the methods on this
/// struct — neither `gix` nor `Command::new("git")` is exposed.
///
/// The injected [`Clock`] drives the per-subprocess timeout so tests can
/// substitute [`crate::clock::MockClock`].
pub struct GitClient {
    repo: gix::ThreadSafeRepository,
    workdir: PathBuf,
    clock: Arc<dyn Clock>,
}

impl GitClient {
    /// Open an existing repository at `path` using a [`SystemClock`] for
    /// subprocess timeouts.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GitError> {
        Self::open_with_clock(path, Arc::new(SystemClock::new()))
    }

    /// Open an existing repository at `path` with an explicit clock for
    /// subprocess timeouts.
    pub fn open_with_clock(
        path: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, GitError> {
        let path = path.as_ref();
        let repo = gix::ThreadSafeRepository::open(path).map_err(|source| GitError::OpenRepo {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        let workdir = repo
            .work_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf());
        Ok(Self {
            repo,
            workdir,
            clock,
        })
    }

    /// The repository's working-tree root. Callers outside `loom-driver` use
    /// this to derive a workspace-relative subprocess `current_dir` without
    /// re-opening or duplicating the path they already passed to
    /// [`Self::open`].
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Working tree status against HEAD.
    pub async fn status(&self) -> Result<Vec<StatusEntry>, GitError> {
        let repo = self.repo.clone();
        spawn_blocking(move || -> Result<Vec<StatusEntry>, GitError> {
            let repo = repo.to_thread_local();
            let platform = repo
                .status(gix::progress::Discard)
                .map_err(|e| GitError::Gix(e.to_string()))?;
            let iter = platform
                .into_iter(None)
                .map_err(|e| GitError::Gix(e.to_string()))?;
            let mut out = Vec::new();
            for item in iter {
                let item = item.map_err(|e| GitError::Gix(e.to_string()))?;
                out.push(StatusEntry::from_item(&item));
            }
            Ok(out)
        })
        .await?
    }

    /// Unified diff of `HEAD` against its first parent (`HEAD~`).
    ///
    /// Returns an empty string when `HEAD` has no parent (initial commit).
    pub async fn diff_head_parent(&self) -> Result<String, GitError> {
        let repo = self.repo.clone();
        spawn_blocking(move || -> Result<String, GitError> {
            let repo = repo.to_thread_local();
            let head = repo
                .head_commit()
                .map_err(|e| GitError::Gix(e.to_string()))?;
            let parents: Vec<_> = head.parent_ids().collect();
            let Some(parent_id) = parents.first() else {
                return Ok(String::new());
            };
            let parent = repo
                .find_object(*parent_id)
                .map_err(|e| GitError::Gix(e.to_string()))?
                .try_into_commit()
                .map_err(|e| GitError::Gix(e.to_string()))?;
            let head_tree = head.tree().map_err(|e| GitError::Gix(e.to_string()))?;
            let parent_tree = parent.tree().map_err(|e| GitError::Gix(e.to_string()))?;
            let mut changes = parent_tree
                .changes()
                .map_err(|e| GitError::Gix(e.to_string()))?;
            let mut buf = String::new();
            changes
                .for_each_to_obtain_tree(
                    &head_tree,
                    |change| -> Result<_, std::convert::Infallible> {
                        use std::fmt::Write as _;
                        let _ = writeln!(buf, "{}", change.location());
                        Ok(std::ops::ControlFlow::Continue(()))
                    },
                )
                .map_err(|e| GitError::Gix(e.to_string()))?;
            Ok(buf)
        })
        .await?
    }

    /// Linked worktrees registered with the repository.
    pub async fn worktrees(&self) -> Result<Vec<WorktreeInfo>, GitError> {
        let repo = self.repo.clone();
        spawn_blocking(move || -> Result<Vec<WorktreeInfo>, GitError> {
            let repo = repo.to_thread_local();
            let proxies = repo.worktrees().map_err(|e| GitError::Gix(e.to_string()))?;
            let mut out = Vec::with_capacity(proxies.len());
            for proxy in proxies {
                let path = proxy.base().map_err(|e| GitError::Gix(e.to_string()))?;
                let branch = proxy
                    .into_repo_with_possibly_inaccessible_worktree()
                    .ok()
                    .and_then(|wt| wt.head_name().ok().flatten())
                    .map(|name| name.shorten().to_string());
                out.push(WorktreeInfo { path, branch });
            }
            Ok(out)
        })
        .await?
    }

    /// Create a per-bead workspace at `.wrapix/worktree/<label>/<bead_id>/`
    /// containing a `git clone --local` of the main repo, with a fresh
    /// branch `loom/<label>/<bead_id>` checked out.
    ///
    /// Path A from `specs/harness.md § Worktree Dispatch`: the per-bead
    /// workspace is a self-contained clone — its `.git/` is a regular
    /// directory inside the bind-mounted path, so workers running in the
    /// wrapix container can resolve the gitdir, commit, and (driver-side)
    /// the bead branch is pushed back to the main repo for merge-back.
    /// Linked worktrees were rejected here because a worktree's `.git`
    /// file points at a host-absolute path outside the container's
    /// `/workspace` bind-mount.
    pub async fn create_worktree(
        &self,
        label: &SpecLabel,
        bead_id: &BeadId,
    ) -> Result<CreatedWorktree, GitError> {
        let branch = format!("{BRANCH_PREFIX}/{label}/{bead_id}");
        let rel = PathBuf::from(WORKTREE_BASE)
            .join(label.as_str())
            .join(bead_id.as_str());
        let path = self.workdir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let src_arg: OsString = self.workdir.clone().into();
        let dst_arg: OsString = path.clone().into();
        run_git(
            &self.workdir,
            self.clock.as_ref(),
            [
                OsString::from("clone"),
                OsString::from("--local"),
                OsString::from("--quiet"),
                src_arg,
                dst_arg,
            ],
            None,
        )
        .await?;

        // Inherit user.email / user.name from the source's effective git
        // config (local + global) into the clone's local config so commits
        // inside the bead workspace work even when only the source repo
        // has the identity set (the nix build sandbox, CI images, and
        // bead containers all lack a global identity). `git clone --local`
        // does not copy `.git/config`, so without this inheritance the
        // first commit fails with "Author identity unknown".
        for key in ["user.email", "user.name"] {
            if let Some(value) = read_config_value(&self.workdir, self.clock.as_ref(), key).await? {
                run_git(&path, self.clock.as_ref(), ["config", key, &value], None).await?;
            }
        }

        run_git(
            &path,
            self.clock.as_ref(),
            ["checkout", "-q", "-b", &branch],
            None,
        )
        .await?;

        // Hard-link the host's dolt socket into the clone so bd inside the
        // wrapix container (which only sees the clone bind-mounted at
        // /workspace) finds a working endpoint at the path
        // `BEADS_DOLT_SERVER_SOCKET` points at. Same-fs hard links to a
        // bound unix socket preserve the inode, so connecting via the
        // linked path reaches the same dolt server. Shim — proper fix is
        // an `extra_mounts` field on `SpawnConfig` (tracked separately).
        let src_sock = self.workdir.join(".wrapix").join("dolt.sock");
        if src_sock.exists() {
            let dest_dir = path.join(".wrapix");
            std::fs::create_dir_all(&dest_dir)?;
            let dest_sock = dest_dir.join("dolt.sock");
            let _ = std::fs::remove_file(&dest_sock);
            std::fs::hard_link(&src_sock, &dest_sock)?;
        }

        Ok(CreatedWorktree { path, branch })
    }

    /// Remove a per-bead workspace directory.
    ///
    /// The workspace is a standalone clone (not a git-registered linked
    /// worktree), so cleanup is a recursive directory removal. Idempotent:
    /// a missing directory is not an error.
    pub async fn remove_worktree(&self, path: &Path) -> Result<(), GitError> {
        if !path.exists() {
            return Ok(());
        }
        let path = path.to_path_buf();
        spawn_blocking(move || -> Result<(), GitError> {
            std::fs::remove_dir_all(&path)?;
            Ok(())
        })
        .await?
    }

    /// Push `branch` from the bead's clone at `workdir` back to its origin
    /// (the main repo this `GitClient` was opened against). Run after a
    /// successful agent session so [`Self::merge_branch`] can fold the bead's
    /// work into the driver branch.
    ///
    /// Uses [`GIT_HOOK_TIMEOUT`] because the destination repo's pre-push
    /// hook runs the workspace's pre-push CI stage (nextest + nix smoke)
    /// — legitimate backpressure that takes minutes, not seconds.
    pub async fn push_branch_to_origin(
        &self,
        workdir: &Path,
        branch: &str,
    ) -> Result<(), GitError> {
        run_git_with_timeout(
            workdir,
            self.clock.as_ref(),
            GIT_HOOK_TIMEOUT,
            ["push", "--quiet", "origin", branch],
            None,
        )
        .await
    }

    /// Force-delete the named branch. Used by the parallel batch driver to
    /// reclaim the per-bead branch after agent failure (the worktree has
    /// already been removed by [`Self::remove_worktree`]). A non-existent
    /// branch surfaces as [`GitError::GitCli`] — call only when the branch
    /// is known to exist.
    pub async fn delete_branch(&self, branch: &str) -> Result<(), GitError> {
        run_git(
            &self.workdir,
            self.clock.as_ref(),
            ["branch", "-D", branch],
            None,
        )
        .await?;
        Ok(())
    }

    /// Push the current branch to its configured remote (`git push`).
    ///
    /// Used by the push gate (`loom gate verify`). Routed through this client so
    /// `Command::new("git")` stays inside `loom-driver/src/git/`, satisfying
    /// the encapsulation rule asserted by `crates/loom/tests/style.rs`.
    ///
    /// Uses [`GIT_HOOK_TIMEOUT`] because the remote's pre-push hook (or
    /// loom's own pre-push hook on the GitHub publish) runs the workspace's
    /// pre-push CI stage.
    pub async fn push(&self) -> Result<(), GitError> {
        run_git_with_timeout(
            &self.workdir,
            self.clock.as_ref(),
            GIT_HOOK_TIMEOUT,
            ["push"],
            None,
        )
        .await
    }

    /// `git rev-parse --verify <rev>^{commit}` — true iff `rev` resolves to
    /// a commit object in this repository.
    pub async fn rev_exists(&self, rev: &str) -> Result<bool, GitError> {
        let output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            [
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{rev}^{{commit}}"),
            ],
            None,
        )
        .await?;
        Ok(output.status.success())
    }

    /// `git merge-base --is-ancestor <rev> HEAD` — true iff `rev` is an
    /// ancestor of the current `HEAD`.
    pub async fn is_ancestor_of_head(&self, rev: &str) -> Result<bool, GitError> {
        let output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            ["merge-base", "--is-ancestor", rev, "HEAD"],
            None,
        )
        .await?;
        Ok(output.status.success())
    }

    /// `git diff <base> HEAD --name-only -- specs/` — repo-relative spec
    /// file paths changed since `base`.
    pub async fn changed_spec_files(&self, base: &str) -> Result<Vec<PathBuf>, GitError> {
        let output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            ["diff", "--name-only", base, "HEAD", "--", "specs/"],
            None,
        )
        .await?;
        if !output.status.success() {
            return Err(GitError::GitCli {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let stdout = String::from_utf8(output.stdout)?;
        Ok(stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect())
    }

    /// `git diff --name-only <range> -- <pathspec>` — repo-relative paths
    /// changed across the supplied diff range, optionally filtered by a
    /// pathspec (e.g. `"specs/"`). `range` is forwarded verbatim, so any
    /// shape `git diff` accepts works (`A..B`, `A...B`, `A B`, etc.).
    pub async fn changed_files_in_range(
        &self,
        range: &str,
        pathspec: Option<&str>,
    ) -> Result<Vec<PathBuf>, GitError> {
        let mut args: Vec<&str> = vec!["diff", "--name-only", range];
        if let Some(p) = pathspec {
            args.push("--");
            args.push(p);
        }
        let output = run_git_raw(&self.workdir, self.clock.as_ref(), args, None).await?;
        if !output.status.success() {
            return Err(GitError::GitCli {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let stdout = String::from_utf8(output.stdout)?;
        Ok(stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect())
    }

    /// `git diff HEAD --name-only -- specs/` — repo-relative paths of spec
    /// files whose working-tree contents differ from `HEAD`. Powers
    /// `loom todo`'s touched-set discovery: any spec edit visible in the
    /// working tree (committed or not) qualifies.
    pub async fn workdir_changed_specs(&self) -> Result<Vec<PathBuf>, GitError> {
        let output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            ["diff", "HEAD", "--name-only", "--", "specs/"],
            None,
        )
        .await?;
        if !output.status.success() {
            return Err(GitError::GitCli {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let stdout = String::from_utf8(output.stdout)?;
        Ok(stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect())
    }

    /// `git diff HEAD -- <spec_path>` — working-tree diff for one spec file.
    /// Empty string when the file matches `HEAD`.
    pub async fn workdir_diff_spec(&self, spec_path: &Path) -> Result<String, GitError> {
        let path_str = spec_path.to_string_lossy().into_owned();
        let output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            ["diff", "HEAD", "--", &path_str],
            None,
        )
        .await?;
        if !output.status.success() {
            return Err(GitError::GitCli {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8(output.stdout)?)
    }

    /// `git diff <base> HEAD -- <spec_path>` — unified diff of one spec
    /// file. Empty string when there is no diff.
    pub async fn diff_spec(&self, base: &str, spec_path: &Path) -> Result<String, GitError> {
        let path_str = spec_path.to_string_lossy().into_owned();
        let output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            ["diff", base, "HEAD", "--", &path_str],
            None,
        )
        .await?;
        if !output.status.success() {
            return Err(GitError::GitCli {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8(output.stdout)?)
    }

    /// `git rev-list --count <commit>..HEAD` — number of commits between
    /// `commit` and the current `HEAD`. Returns `0` when `commit` is `HEAD`,
    /// and surfaces [`GitError::GitCli`] when `commit` does not resolve.
    pub async fn commits_since(&self, commit: &str) -> Result<u32, GitError> {
        let range = format!("{commit}..HEAD");
        let output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            ["rev-list", "--count", &range],
            None,
        )
        .await?;
        if !output.status.success() {
            return Err(GitError::GitCli {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let stdout = String::from_utf8(output.stdout)?;
        let parsed: u32 = stdout
            .trim()
            .parse()
            .map_err(|e: std::num::ParseIntError| GitError::GitCli {
                status: 0,
                stderr: format!("rev-list --count returned non-integer `{stdout}`: {e}"),
            })?;
        Ok(parsed)
    }

    /// `git -C <workdir> status --porcelain` against an arbitrary linked
    /// worktree under this repo. Returns the raw porcelain output verbatim
    /// so callers can route it through
    /// [`crate::run::dirty_paths_from_porcelain`] (or equivalent) without
    /// reopening a [`GitClient`] per worktree. Used by the run-phase
    /// verdict-gate tree-not-clean dispatcher.
    pub async fn status_porcelain_at(&self, workdir: &Path) -> Result<String, GitError> {
        let output = run_git_raw(
            workdir,
            self.clock.as_ref(),
            ["status", "--porcelain"],
            None,
        )
        .await?;
        if !output.status.success() {
            return Err(GitError::GitCli {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8(output.stdout)?)
    }

    /// `git rev-parse HEAD` — full SHA of the current `HEAD`.
    pub async fn head_commit_sha(&self) -> Result<String, GitError> {
        let output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            ["rev-parse", "HEAD"],
            None,
        )
        .await?;
        if !output.status.success() {
            return Err(GitError::GitCli {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    /// Merge `branch` into the current driver branch. Rebases `branch` onto
    /// the current `HEAD` first so the merge is always a fast-forward —
    /// sequential dispatch (bead branch is a strict descendant of `HEAD`)
    /// is a rebase no-op; parallel dispatch's second-and-later beads pick
    /// up the moved `HEAD` from an earlier merge. True overlap surfaces as
    /// [`MergeResult::Conflict`]; other failures surface as [`GitError`].
    pub async fn merge_branch(&self, branch: &str) -> Result<MergeResult, GitError> {
        let head_branch_output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            ["symbolic-ref", "--short", "HEAD"],
            None,
        )
        .await?;
        if !head_branch_output.status.success() {
            return Err(GitError::GitCli {
                status: head_branch_output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&head_branch_output.stderr).into_owned(),
            });
        }
        let head_branch = String::from_utf8(head_branch_output.stdout)?
            .trim()
            .to_string();

        let rebase_output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            ["rebase", &head_branch, branch],
            None,
        )
        .await?;
        if !rebase_output.status.success() {
            let _ = run_git_raw(
                &self.workdir,
                self.clock.as_ref(),
                ["rebase", "--abort"],
                None,
            )
            .await?;
            run_git(
                &self.workdir,
                self.clock.as_ref(),
                ["checkout", "-q", &head_branch],
                None,
            )
            .await?;
            return Ok(MergeResult::Conflict);
        }

        run_git(
            &self.workdir,
            self.clock.as_ref(),
            ["checkout", "-q", &head_branch],
            None,
        )
        .await?;

        let output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            ["merge", "--ff-only", branch],
            None,
        )
        .await?;
        if output.status.success() {
            return Ok(MergeResult::Ok);
        }
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(GitError::GitCli {
            status: output.status.code().unwrap_or(-1),
            stderr,
        })
    }
}

/// Initialize a real git repository at `path` with one `initial` commit and
/// a bare `origin` remote alongside it so [`GitClient::create_worktree`],
/// [`GitClient::merge_branch`], and [`GitClient::push`] all succeed against
/// it. The bare `origin` lives at a sibling `<path>.git`; production
/// `loom loop` pushes `main` to `origin` after every successful per-bead
/// merge, so tests that exercise the merge-back path need a real push
/// destination or the post-merge push gate would fail.
///
/// Exposed for cross-crate test consumption — production callers operate
/// on the caller-supplied workspace and never need to bootstrap one. The
/// function is the only sanctioned way for tests outside
/// `loom-driver/src/git/` to stand up a git repo: the workspace-level
/// `git_client_encapsulation` style lint rejects bare
/// `Command::new("git")` calls in tests under `crates/*/src/`.
///
/// Returns an opened [`GitClient`] rooted at `path`.
#[doc(hidden)]
pub fn init_test_repo(path: &Path) -> Result<GitClient, GitError> {
    use std::process::Command as StdCommand;
    std::fs::create_dir_all(path)?;
    let run_in = |dir: &Path, args: &[&str]| -> Result<(), GitError> {
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .map_err(GitError::Spawn)?;
        if status.success() {
            return Ok(());
        }
        Err(GitError::GitCli {
            status: status.code().unwrap_or(-1),
            stderr: format!("git {args:?} exited {status}"),
        })
    };
    run_in(path, &["init", "-q", "-b", "main"])?;
    run_in(path, &["config", "user.email", "test@example.com"])?;
    run_in(path, &["config", "user.name", "Test"])?;
    run_in(path, &["config", "commit.gpgsign", "false"])?;
    std::fs::write(path.join("README.md"), "initial\n")?;
    run_in(path, &["add", "README.md"])?;
    run_in(path, &["commit", "-q", "-m", "initial"])?;
    let origin_path = bare_origin_path(path);
    if let Some(parent) = origin_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(&origin_path)?;
    run_in(&origin_path, &["init", "-q", "--bare", "-b", "main"])?;
    let origin_url = origin_path.to_string_lossy().into_owned();
    run_in(path, &["remote", "add", "origin", &origin_url])?;
    run_in(path, &["push", "-q", "-u", "origin", "main"])?;
    GitClient::open(path)
}

/// Bare origin path used by [`init_test_repo`]. Exposed so tests that need
/// to inspect the published refs (e.g. assert that `main` advanced after a
/// per-bead push) can locate the bare repo without re-deriving the
/// suffix.
#[doc(hidden)]
pub fn bare_origin_path(workspace: &Path) -> PathBuf {
    let parent = workspace.parent().unwrap_or(workspace);
    let name = workspace
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".to_string());
    parent.join(format!("{name}.git"))
}

/// Result of [`GitClient::create_worktree`].
#[derive(Debug, Clone)]
pub struct CreatedWorktree {
    pub path: PathBuf,
    pub branch: String,
}

/// Linked worktree as reported by `gix`.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: Option<String>,
}

/// Working tree status entry.
#[derive(Debug, Clone)]
pub struct StatusEntry {
    pub path: String,
    pub kind: StatusKind,
}

impl StatusEntry {
    fn from_item(item: &gix::status::Item) -> Self {
        let path = item.location().to_string();
        let kind = match item {
            gix::status::Item::IndexWorktree(_) => StatusKind::WorktreeChange,
            gix::status::Item::TreeIndex(_) => StatusKind::IndexChange,
        };
        Self { path, kind }
    }
}

/// Kind of change reported by [`StatusEntry`]. Coarse on purpose — callers
/// that need richer detail (rename detection, etc.) should grow this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    IndexChange,
    WorktreeChange,
}

/// Outcome of [`GitClient::merge_branch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeResult {
    Ok,
    Conflict,
}

/// Run `git` with an explicit `-C <workdir>`, no shell, 60s ceiling. Returns
/// `Ok(())` only on a clean exit.
async fn run_git<I, S>(
    workdir: &Path,
    clock: &dyn Clock,
    args: I,
    trailing: Option<&OsString>,
) -> Result<(), GitError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    run_git_with_timeout(workdir, clock, GIT_TIMEOUT, args, trailing).await
}

async fn run_git_with_timeout<I, S>(
    workdir: &Path,
    clock: &dyn Clock,
    timeout: Duration,
    args: I,
    trailing: Option<&OsString>,
) -> Result<(), GitError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = run_git_raw_with_timeout(workdir, clock, timeout, args, trailing).await?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Err(GitError::GitCli {
        status: output.status.code().unwrap_or(-1),
        stderr,
    })
}

/// `git -C <workdir> config --get <key>` — returns the resolved value or
/// `Ok(None)` when the key is unset (git exits with code 1). Used by
/// [`GitClient::create_worktree`] to inherit the source's user identity
/// into the freshly-cloned bead workspace.
async fn read_config_value(
    workdir: &Path,
    clock: &dyn Clock,
    key: &str,
) -> Result<Option<String>, GitError> {
    let output = run_git_raw(workdir, clock, ["config", "--get", key], None).await?;
    if output.status.success() {
        let value = String::from_utf8(output.stdout)?.trim().to_string();
        return Ok(Some(value));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    Err(GitError::GitCli {
        status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

async fn run_git_raw<I, S>(
    workdir: &Path,
    clock: &dyn Clock,
    args: I,
    trailing: Option<&OsString>,
) -> Result<std::process::Output, GitError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    run_git_raw_with_timeout(workdir, clock, GIT_TIMEOUT, args, trailing).await
}

async fn run_git_raw_with_timeout<I, S>(
    workdir: &Path,
    clock: &dyn Clock,
    timeout: Duration,
    args: I,
    trailing: Option<&OsString>,
) -> Result<std::process::Output, GitError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(workdir);
    let mut argv_for_log: Vec<String> = Vec::new();
    for arg in args {
        argv_for_log.push(arg.as_ref().to_string_lossy().into_owned());
        cmd.arg(arg);
    }
    if let Some(t) = trailing {
        argv_for_log.push(t.to_string_lossy().into_owned());
        cmd.arg(t);
    }

    let fut = cmd.output();
    let sleep = clock.sleep(timeout);
    tokio::select! {
        result = fut => match result {
            Ok(output) => Ok(output),
            Err(e) => Err(GitError::Spawn(e)),
        },
        () = sleep => Err(GitError::GitTimeout {
            args: argv_for_log.join(" "),
        }),
    }
}
