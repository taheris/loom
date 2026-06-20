use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use tokio::task::spawn_blocking;
use tracing::{info, warn};

use crate::bd::{BdClient, CommandRunner};
use crate::clock::{Clock, SystemClock};
use crate::identifier::{BeadId, MoleculeId, SpecLabel};

use loom_protocol::oid::GitOid;

use super::error::GitError;

const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Default timeout for git operations whose hooks can legitimately run
/// for minutes — pre-push fires the workspace's pre-push CI stage
/// (nextest + nix build), which on a warm sccache takes a few minutes.
/// The timeout surfaces true hangs (deadlocked subprocess, runaway
/// network); it must not abort legitimate CI. Overridable per client via
/// [`GitClient::with_hook_timeout`], which production threads from
/// `[loom] git_hook_timeout_secs`.
const GIT_HOOK_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const WORKTREE_BASE: &str = ".loom/beads";
const BRANCH_PREFIX: &str = "loom";
/// Path of the loom-owned integration workspace relative to the workspace
/// root the `GitClient` is opened against. Materialized by `loom init` and
/// used by [`GitClient::merge_branch`], [`GitClient::push`],
/// [`GitClient::delete_branch`], and [`GitClient::create_worktree`] as the
/// cwd / clone source so bead integration never touches the operator's
/// working tree.
const LOOM_WORKSPACE_REL: &str = ".loom/integration";
/// Number of times an index-mutating git invocation in the loom workspace
/// retries against git lock-file contention before surfacing
/// [`GitError::IndexLocked`]. Cross-spec `loom loop` invocations share one
/// loom workspace; the rebase + ff critical section is serialized by git's
/// own `index.lock` (specs/harness.md § Concurrency). The loop's command
/// that loses the race observes git's "Unable to create '…/index.lock':
/// File exists" failure — having mutated nothing — and retries from its
/// current view of the integration branch. Bounded so a crashed peer that
/// left a stale lock cannot loop forever.
const INDEX_LOCK_RETRIES: u32 = 100;
/// Backoff between [`INDEX_LOCK_RETRIES`] attempts. The lock is held only
/// for the duration of a peer's single index write (rebase / checkout /
/// ff-merge run no hooks, so milliseconds), so a short fixed pause drains
/// realistic contention well inside the retry budget; the product
/// (`RETRIES * BACKOFF` ≈ 2s) is the worst-case wait before a stale lock is
/// reported.
const INDEX_LOCK_BACKOFF: Duration = Duration::from_millis(20);
/// Fallback integration branch when the caller opens a `GitClient` via
/// [`GitClient::open`] / [`GitClient::open_with_clock`] without naming
/// one. Production paths thread `[loom] integration_branch` from
/// `LoomConfig`; only test fixtures and one-shot CLI utilities take the
/// default.
const DEFAULT_INTEGRATION_BRANCH: &str = "main";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualPushRange {
    pub range: String,
    pub tree_oid: GitOid,
    pub remote_ref: String,
}

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
    integration_branch: String,
    /// Timeout for hook-running git operations (currently [`Self::push`]).
    /// Defaults to [`GIT_HOOK_TIMEOUT`]; production overrides it from
    /// `[loom] git_hook_timeout_secs` via [`Self::with_hook_timeout`].
    hook_timeout: Duration,
    /// Test-only seam: when `Some`, loom-workspace signing refresh uses
    /// this override instead of resolving from the env + deploy-key fallback.
    /// `Some(key)` forces a key; `None` forces the no-key path. Production
    /// resolves per [`super::signing::resolve_signing_key`]; the seam exists
    /// because `std::env::set_var` is unsafe under edition 2024 and the
    /// workspace forbids `unsafe_code`, so tests cannot drive
    /// `$WRIX_SIGNING_KEY`. Gated behind `cfg(test)` / the `test-support`
    /// feature so it is absent from production builds (RS-14).
    #[cfg(any(test, feature = "test-support"))]
    signing_key_override: Option<Option<PathBuf>>,
    #[cfg(any(test, feature = "test-support"))]
    prek_hooks_path_override: Option<PathBuf>,
}

impl GitClient {
    /// Open an existing repository at `path` using a [`SystemClock`] for
    /// subprocess timeouts and the default integration branch (`main`).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GitError> {
        Self::open_with_clock(path, Arc::new(SystemClock::new()))
    }

    /// Open an existing repository at `path` with an explicit clock for
    /// subprocess timeouts. Integration branch defaults to `main`.
    pub fn open_with_clock(
        path: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, GitError> {
        Self::open_with(path, clock, DEFAULT_INTEGRATION_BRANCH.to_string())
    }

    /// Open an existing repository at `path` and pin the integration
    /// branch the loom workspace has checked out. Production callers
    /// pass the value of `[loom] integration_branch` from `LoomConfig`;
    /// the field is consulted by [`Self::merge_branch`] (rebase target)
    /// and [`Self::push`] (origin push target) instead of querying
    /// `HEAD` or relying on `git push`'s upstream defaulting.
    pub fn open_with_integration_branch(
        path: impl AsRef<Path>,
        integration_branch: String,
    ) -> Result<Self, GitError> {
        Self::open_with(path, Arc::new(SystemClock::new()), integration_branch)
    }

    fn open_with(
        path: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
        integration_branch: String,
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
            integration_branch,
            hook_timeout: GIT_HOOK_TIMEOUT,
            #[cfg(any(test, feature = "test-support"))]
            signing_key_override: None,
            #[cfg(any(test, feature = "test-support"))]
            prek_hooks_path_override: None,
        })
    }

    /// Override the timeout used for hook-running git operations
    /// (currently [`Self::push`]). Production threads
    /// `[loom] git_hook_timeout_secs` here; absent an override the client
    /// uses [`GIT_HOOK_TIMEOUT`].
    pub fn with_hook_timeout(mut self, hook_timeout: Duration) -> Self {
        self.hook_timeout = hook_timeout;
        self
    }

    /// Test-only: force loom-workspace signing refresh and launcher key
    /// export to use `key` instead of resolving it from the environment.
    /// Gated behind `cfg(test)` / the `test-support` feature (RS-14).
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn set_signing_key_override(&mut self, key: PathBuf) {
        self.signing_key_override = Some(Some(key));
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn disable_signing_key_resolution(&mut self) {
        self.signing_key_override = Some(None);
    }

    /// Test-only: force [`Self::create_worktree`] / push-gate hook
    /// validation to use `path` instead of resolving `$WRIX_PREK_HOOKS`.
    /// Gated behind `cfg(test)` / the `test-support` feature (RS-14).
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn set_prek_hooks_path_override(&mut self, path: PathBuf) {
        self.prek_hooks_path_override = Some(path);
    }

    /// The signing-key override set via [`Self::set_signing_key_override`]
    /// or [`Self::disable_signing_key_resolution`]. Production builds (where
    /// the seam is compiled out) always return `None`, so the signing key
    /// resolves from the environment + deploy-key fallback.
    #[cfg(any(test, feature = "test-support"))]
    fn signing_override(&self) -> Option<Option<PathBuf>> {
        self.signing_key_override.clone()
    }

    /// See the `cfg(test)` / `test-support` variant — production carries no
    /// override seam, so this always resolves the key from the environment.
    #[cfg(not(any(test, feature = "test-support")))]
    fn signing_override(&self) -> Option<Option<PathBuf>> {
        None
    }

    #[cfg(any(test, feature = "test-support"))]
    fn prek_hooks_override(&self) -> Option<PathBuf> {
        self.prek_hooks_path_override.clone()
    }

    #[cfg(not(any(test, feature = "test-support")))]
    fn prek_hooks_override(&self) -> Option<PathBuf> {
        None
    }

    fn resolve_prek_hooks_path(&self) -> Result<PathBuf, GitError> {
        match self.prek_hooks_override() {
            Some(path) => {
                super::hooks::ensure_prek_hooks_dir(&path)?;
                Ok(path)
            }
            None => self.resolve_default_prek_hooks_path(),
        }
    }

    fn resolve_default_prek_hooks_path(&self) -> Result<PathBuf, GitError> {
        let test_hooks = self.workdir.join(".loom/test-prek-hooks");
        if test_hooks.exists() {
            super::hooks::ensure_prek_hooks_dir(&test_hooks)?;
            return Ok(test_hooks);
        }
        super::hooks::resolve_prek_hooks_path_for_workspace(&self.workdir)
    }

    fn resolve_signing_key(&self) -> Result<Option<PathBuf>, GitError> {
        match self.signing_override() {
            Some(key) => Ok(key),
            None => super::signing::resolve_signing_key(&self.loom_workspace()),
        }
    }

    fn refresh_loom_signing_config(&self) -> Result<(), GitError> {
        let signing_key = self.resolve_signing_key()?;
        super::signing::reconcile_signing_config(&self.loom_workspace(), signing_key.as_deref())
    }

    /// Name of the integration branch this client targets (the branch
    /// loaded from `[loom] integration_branch`).
    pub fn integration_branch(&self) -> &str {
        &self.integration_branch
    }

    /// The repository's working-tree root. Callers outside `loom-driver` use
    /// this to derive a workspace-relative subprocess `current_dir` without
    /// re-opening or duplicating the path they already passed to
    /// [`Self::open`].
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Path of the loom-owned integration workspace —
    /// `<workdir>/.loom/integration`. This is where bead branches
    /// are pushed, rebased, and fast-forward-merged into the integration
    /// branch; the operator's `<workdir>` itself is never the target of
    /// loom-driven merges or origin pushes.
    pub fn loom_workspace(&self) -> PathBuf {
        self.workdir.join(LOOM_WORKSPACE_REL)
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

    /// Create a per-bead workspace at `.loom/beads/<bead_id>/`
    /// containing a `git clone --local` of the loom workspace at
    /// `.loom/integration/`, with a fresh branch `loom/<bead_id>`
    /// checked out. Bead ids are globally unique, so the path is flat
    /// — no spec-label partition. The `label` argument is accepted for
    /// source-compat with existing callers and ignored by the path /
    /// branch construction.
    ///
    /// Cloning from the loom workspace (not the operator's workdir)
    /// leaves the bead clone's `origin` pointing at the loom workspace
    /// path. Under the A3 push-reliability model the worker never pushes
    /// to it; the remote is preserved only so host-side ahead/behind
    /// tracking works when the operator `cd`s into the bead clone (e.g.
    /// the starship prompt). The driver pulls the bead branch via
    /// [`Self::fetch_bead_branch`] against the same filesystem path.
    ///
    /// Path A from `specs/harness.md § Bead dispatch`: the per-bead
    /// workspace is a self-contained clone — its `.git/` is a regular
    /// directory inside the bind-mounted path, so workers running in the
    /// wrix container can resolve the gitdir and commit. Linked
    /// worktrees were rejected here because a worktree's `.git` file
    /// points at a host-absolute path outside the container's
    /// `/workspace` bind-mount.
    ///
    /// Idempotent at the directory level: if the destination already
    /// exists, the call returns a [`CreatedWorktree`] pointing at it
    /// without re-cloning. This shape is load-bearing for the
    /// per-bead-close lifecycle — a bead workspace persists across
    /// attempts and `loom loop` invocations until the bead is closed, so
    /// a second dispatch attempt must observe the existing tree rather
    /// than tripping `git clone --local: destination path already exists`.
    pub async fn create_worktree(
        &self,
        _label: &SpecLabel,
        bead_id: &BeadId,
    ) -> Result<CreatedWorktree, GitError> {
        let branch = format!("{BRANCH_PREFIX}/{bead_id}");
        let rel = PathBuf::from(WORKTREE_BASE).join(bead_id.as_str());
        let path = self.workdir.join(&rel);
        if path.exists() {
            self.repair_bead_hooks_path(&path).await?;
            return Ok(CreatedWorktree { path, branch });
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let loom_workspace = self.loom_workspace();
        let src_arg: OsString = loom_workspace.clone().into();
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
            if let Some(value) =
                read_config_value(&loom_workspace, self.clock.as_ref(), key).await?
            {
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

        // Track `origin/<integration_branch>` so the pre-push hook's
        // `--diff @{u}..HEAD` (`.pre-commit-config.yaml`) resolves to the
        // bead's commits over its base instead of failing with "no upstream
        // configured" — which silently degrades the gate to an unscoped,
        // whole-tree walk. The clone's `origin` is the loom workspace, whose
        // only branch is the integration branch, so the tracking ref exists.
        let upstream = format!("origin/{}", self.integration_branch);
        run_git(
            &path,
            self.clock.as_ref(),
            ["branch", "--set-upstream-to", &upstream, &branch],
            None,
        )
        .await?;

        ensure_wrix_mount_dir(&path)?;
        self.repair_bead_hooks_path(&path).await?;

        // No signing block is written into the bead clone. The clone is the
        // workspace wrix bind-mounts into the bead container, where the
        // worker's commits are the load-bearing signed path. wrix's
        // `git-ssh-setup.sh` entrypoint configures container signing in the
        // GLOBAL `~/.gitconfig`, pointing `user.signingkey` at the
        // in-container key copy (`/etc/wrix/keys/<id>-nix-signing`). A local
        // `.git/config` block here would carry the HOST key path — which does
        // not exist in-container — and local config beats global, so the block
        // would shadow the entrypoint's correct container path and break the
        // worker's `git commit` with "Couldn't load public key ...". Host-side
        // operator debug/test commits in the clone instead fall through to the
        // operator's global gitconfig. Only the host-only loom workspace
        // (never container-mounted) carries loom's local signing block, so it
        // is free of this host/container path conflict (`specs/harness.md`
        // § Commit signing).

        Ok(CreatedWorktree { path, branch })
    }

    /// Resolve the host key paths loom must hand to `wrix spawn` through
    /// the **launcher** environment (the child-process env, not the
    /// in-container [`SpawnConfig::env`] allowlist) so the wrapper can
    /// bind-mount the deploy + signing keys into the bead container.
    ///
    /// Returns `(env var, host path)` pairs — `WRIX_DEPLOY_KEY` and
    /// `WRIX_SIGNING_KEY` — for each key that resolves. Resolution runs
    /// against the loom workspace (whose `origin` is GitHub), matching
    /// loom-workspace signing refresh, and honors the signing-key override
    /// test seam (when built with `test-support`). A key
    /// that does not resolve is omitted rather than erroring: `wrix spawn`
    /// fails loudly on its own when a key it needs is absent, and the
    /// "wrix isn't set up on this host" path must stay non-fatal here.
    ///
    /// The host paths never cross the sandbox boundary; wrix copies the
    /// keys to `/etc/wrix/keys/<basename>` in-container and resets
    /// `$WRIX_DEPLOY_KEY` / `$WRIX_SIGNING_KEY` to those paths (wrix
    /// `specs/security.md` § Credential Surfaces). See `specs/harness.md`
    /// § Commit signing.
    pub fn launcher_key_env(&self) -> Result<Vec<(String, String)>, GitError> {
        let loom_workspace = self.loom_workspace();
        let mut env = Vec::new();
        let signing = self.resolve_signing_key()?;
        if let Some(key) = signing {
            env.push((
                super::signing::WRIX_SIGNING_KEY_ENV.to_string(),
                key.to_string_lossy().into_owned(),
            ));
        }
        if let Some(key) = super::signing::resolve_deploy_key(&loom_workspace)? {
            env.push((
                super::signing::WRIX_DEPLOY_KEY_ENV.to_string(),
                key.to_string_lossy().into_owned(),
            ));
        }
        Ok(env)
    }

    /// Reset a per-bead workspace's working tree to its current `HEAD` and
    /// drop everything outside the tracked content + the preserved
    /// scratch dirs. Runs `git reset --hard HEAD` followed by
    /// `git clean -fdx --exclude=target --exclude=.git --exclude=.wrix`.
    ///
    /// Called by the dispatch path immediately before every agent session
    /// attempt — first attempt (where it is a no-op against a freshly-cloned
    /// tree) and every recovery iteration (where it discards mid-session
    /// leftovers while preserving the agent's prior commits on the bead
    /// branch). Idempotent. `target/` survives so cargo + sccache stay warm;
    /// `.git/` survives so refs and the bead branch stay intact; `.wrix/`
    /// survives so extra-mount staging (e.g. dolt socket landing point)
    /// persists across attempts.
    pub async fn reset_bead_clone(&self, path: &Path) -> Result<(), GitError> {
        run_git(
            path,
            self.clock.as_ref(),
            ["reset", "--hard", "--quiet", "HEAD"],
            None,
        )
        .await?;
        run_git(
            path,
            self.clock.as_ref(),
            [
                "clean",
                "-fdx",
                "--quiet",
                "--exclude=target",
                "--exclude=.git",
                "--exclude=.wrix",
            ],
            None,
        )
        .await
    }

    /// Configure a bead clone's local `core.hooksPath` from wrix's
    /// canonical prek hooks directory.
    pub async fn repair_bead_hooks_path(&self, path: &Path) -> Result<(), GitError> {
        let hooks_path = self.resolve_prek_hooks_path()?;
        let value = hooks_path.to_string_lossy().into_owned();
        run_git(
            path,
            self.clock.as_ref(),
            ["config", "core.hooksPath", &value],
            None,
        )
        .await
    }

    /// Validate the loom integration workspace's local `core.hooksPath`.
    pub async fn validate_loom_hooks_path_configured(&self) -> Result<(), GitError> {
        let expected = self.resolve_prek_hooks_path()?;
        let workdir = self.loom_workspace();
        let actual = read_config_value(&workdir, self.clock.as_ref(), "core.hooksPath").await?;
        let expected_value = expected.to_string_lossy().into_owned();
        if actual.as_deref() == Some(expected_value.as_str()) {
            return Ok(());
        }
        Err(GitError::HooksPathInvalid {
            workdir,
            expected: expected_value,
            actual: actual.unwrap_or_else(|| "<unset>".to_string()),
        })
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

    /// Enumerate `.loom/beads/` and remove each bead workspace whose
    /// bead is closed and parented by `molecule`. Called at `loom loop`
    /// startup, under the spec advisory lock, to reap orphans from crashed
    /// prior runs without touching another molecule's active workers.
    ///
    /// Failure modes are log-and-skip rather than abort, so a single
    /// corrupt orphan (unparsable directory name, `bd` lookup error,
    /// stuck removal) does not block `loom loop` from running:
    ///
    /// - directory whose name does not parse as a [`BeadId`] → skip
    /// - `bd show` failure (record gone, network blip) → skip
    /// - `remove_dir_all` failure → skip
    ///
    /// A missing `.loom/beads/` directory is a no-op (returns the
    /// empty vec). The returned paths are the workspaces actually
    /// removed during this sweep.
    pub async fn sweep_orphan_bead_clones<R: CommandRunner>(
        &self,
        bd: &BdClient<R>,
        molecule: &MoleculeId,
    ) -> Result<Vec<PathBuf>, GitError> {
        let base = self.workdir.join(WORKTREE_BASE);
        let entries = match std::fs::read_dir(&base) {
            Ok(it) => it,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(GitError::Io(e)),
        };
        let mut removed = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(error) => {
                    warn!(
                        base = %base.display(),
                        %error,
                        "sweep_orphan_bead_clones: failed to read entry — skipping",
                    );
                    continue;
                }
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(error) => {
                    warn!(
                        path = %path.display(),
                        %error,
                        "sweep_orphan_bead_clones: failed to stat entry — skipping",
                    );
                    continue;
                }
            };
            if !file_type.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                warn!(
                    path = %path.display(),
                    "sweep_orphan_bead_clones: unreadable directory name — skipping",
                );
                continue;
            };
            let bead_id = match BeadId::new(name) {
                Ok(id) => id,
                Err(error) => {
                    warn!(
                        path = %path.display(),
                        %error,
                        "sweep_orphan_bead_clones: directory name is not a bead id — skipping",
                    );
                    continue;
                }
            };
            let bead = match bd.show(&bead_id).await {
                Ok(b) => b,
                Err(error) => {
                    warn!(
                        bead = %bead_id,
                        path = %path.display(),
                        %error,
                        "sweep_orphan_bead_clones: bd show failed — skipping",
                    );
                    continue;
                }
            };
            let in_current_molecule = bead
                .parent
                .as_ref()
                .is_some_and(|parent| parent.as_str() == molecule.as_str());
            if bead.status != "closed" || !in_current_molecule {
                continue;
            }
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {
                    info!(
                        bead = %bead_id,
                        path = %path.display(),
                        "sweep_orphan_bead_clones: removed closed bead workspace",
                    );
                    removed.push(path);
                }
                Err(error) => {
                    warn!(
                        bead = %bead_id,
                        path = %path.display(),
                        %error,
                        "sweep_orphan_bead_clones: removal failed — skipping",
                    );
                }
            }
        }
        Ok(removed)
    }

    /// Fetch the bead branch `loom/<bead_id>` from the bead workspace at
    /// `bead_workspace_path` into the loom workspace, where
    /// [`Self::merge_branch`] rebases and fast-forwards it onto the
    /// integration branch.
    ///
    /// Runs `git fetch <bead-workspace-path> loom/<id>:loom/<id>` in the
    /// loom workspace ([`Self::loom_workspace`]), treating the bead
    /// workspace path as an ad-hoc filesystem URL — no network, no daemon.
    /// Under the A3 push-reliability model the worker never pushes; the
    /// driver pulls the branch over the filesystem path, which is always
    /// reachable from the host through the bead's lifetime (the bead
    /// container, by contrast, has no mount back to the loom workspace).
    pub async fn fetch_bead_branch(
        &self,
        bead_workspace_path: &Path,
        bead_id: &BeadId,
    ) -> Result<(), GitError> {
        let refspec = format!("{BRANCH_PREFIX}/{bead_id}:{BRANCH_PREFIX}/{bead_id}");
        run_git(
            &self.loom_workspace(),
            self.clock.as_ref(),
            [
                OsString::from("fetch"),
                OsString::from("--quiet"),
                bead_workspace_path.into(),
                OsString::from(refspec),
            ],
            None,
        )
        .await
    }

    /// Create an isolated tune proposal checkout at `destination`.
    pub async fn create_tune_checkout(
        &self,
        destination: &Path,
        base: &str,
        branch: &str,
    ) -> Result<(), GitError> {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        run_git(
            &self.workdir,
            self.clock.as_ref(),
            [
                OsString::from("clone"),
                OsString::from("--no-local"),
                OsString::from("--no-checkout"),
                self.workdir.clone().into(),
                destination.into(),
            ],
            None,
        )
        .await?;
        run_git(
            destination,
            self.clock.as_ref(),
            ["checkout", "-B", branch, base],
            None,
        )
        .await?;
        run_git(
            destination,
            self.clock.as_ref(),
            ["config", "user.name", "Loom Tune"],
            None,
        )
        .await?;
        run_git(
            destination,
            self.clock.as_ref(),
            ["config", "user.email", "loom-tune@example.invalid"],
            None,
        )
        .await?;
        Ok(())
    }

    /// Commit every current worktree change, permitting an empty candidate.
    pub async fn commit_all_allow_empty(&self, message: &str) -> Result<(), GitError> {
        run_git(&self.workdir, self.clock.as_ref(), ["add", "--all"], None).await?;
        run_git(
            &self.workdir,
            self.clock.as_ref(),
            ["commit", "--allow-empty", "-m", message],
            None,
        )
        .await
    }

    /// Force-delete the named branch in the loom workspace. Used by the
    /// parallel batch driver to reclaim the per-bead branch after agent
    /// failure (the worktree has already been removed by
    /// [`Self::remove_worktree`]). A non-existent branch surfaces as
    /// [`GitError::GitCli`] — call only when the branch is known to
    /// exist.
    pub async fn delete_branch(&self, branch: &str) -> Result<(), GitError> {
        run_git(
            &self.loom_workspace(),
            self.clock.as_ref(),
            ["branch", "-D", branch],
            None,
        )
        .await?;
        Ok(())
    }

    /// Push the configured integration branch from the loom workspace
    /// to `origin`.
    ///
    /// Used by the push gate (`loom gate verify`). Routed through this client so
    /// `Command::new("git")` stays inside `loom-driver/src/git/`, satisfying
    /// the encapsulation rule asserted by `crates/loom/tests/style.rs`.
    /// Pushes `origin <integration_branch>` explicitly rather than
    /// relying on the current branch's upstream defaulting so the
    /// pushed ref name is unambiguous regardless of how the workspace
    /// was set up.
    ///
    /// On a non-fast-forward rejection (operator pushed first, or another
    /// loom landed cross-spec work) the push fetches
    /// `origin/<integration_branch>`, rebases the local integration
    /// branch onto it, and re-pushes — up to [`PUSH_NON_FF_RETRIES`]
    /// times. Other push failures (auth, pre-push hook, network) are
    /// surfaced as [`GitError::GitCli`] without retry.
    ///
    /// Uses [`Self::hook_timeout`] (configurable via
    /// `[loom] git_hook_timeout_secs`, default [`GIT_HOOK_TIMEOUT`])
    /// because the remote's pre-push hook (or loom's own pre-push hook on
    /// the GitHub publish) runs the workspace's pre-push CI stage.
    pub async fn push(&self) -> Result<(), GitError> {
        self.push_once().await
    }

    pub async fn prepare_actual_push_range(&self) -> Result<ActualPushRange, GitError> {
        self.validate_loom_hooks_path_configured().await?;
        self.refresh_loom_signing_config()?;
        let workdir = self.loom_workspace();
        let integration_branch = self.integration_branch.as_str();
        let remote_ref = format!("origin/{integration_branch}");
        run_git(
            &workdir,
            self.clock.as_ref(),
            ["fetch", "--quiet", "origin", integration_branch],
            None,
        )
        .await?;
        let rebase_output =
            run_git_index_mut(&workdir, self.clock.as_ref(), ["rebase", &remote_ref], None).await?;
        if !rebase_output.status.success() {
            let _ = run_git_raw(&workdir, self.clock.as_ref(), ["rebase", "--abort"], None).await?;
            return Err(cli_error(&rebase_output));
        }
        let tree_oid = self.rev_parse_in_loom("HEAD^{tree}").await?;
        Ok(ActualPushRange {
            range: format!("{remote_ref}..HEAD"),
            tree_oid,
            remote_ref,
        })
    }

    pub async fn run_pre_push_chain(&self) -> Result<(), GitError> {
        self.validate_loom_hooks_path_configured().await?;
        let workdir = self.loom_workspace();
        let output = run_git_raw_with_timeout(
            &workdir,
            self.clock.as_ref(),
            self.hook_timeout,
            [
                "push",
                "--dry-run",
                "origin",
                self.integration_branch.as_str(),
            ],
            None,
        )
        .await?;
        if output.status.success() {
            return Ok(());
        }
        Err(cli_error(&output))
    }

    pub async fn push_once(&self) -> Result<(), GitError> {
        self.validate_loom_hooks_path_configured().await?;
        let workdir = self.loom_workspace();
        let output = run_git_raw_with_timeout(
            &workdir,
            self.clock.as_ref(),
            self.hook_timeout,
            ["push", "origin", self.integration_branch.as_str()],
            None,
        )
        .await?;
        if output.status.success() {
            return Ok(());
        }
        Err(cli_error(&output))
    }

    async fn rev_parse_in_loom(&self, rev: &str) -> Result<GitOid, GitError> {
        let output = run_git_raw(
            &self.loom_workspace(),
            self.clock.as_ref(),
            ["rev-parse", "--verify", rev],
            None,
        )
        .await?;
        if !output.status.success() {
            return Err(cli_error(&output));
        }
        let raw = String::from_utf8(output.stdout)?;
        Ok(GitOid::new(raw.trim())?)
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
            return Err(cli_error(&output));
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
            return Err(cli_error(&output));
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
            return Err(cli_error(&output));
        }
        let stdout = String::from_utf8(output.stdout)?;
        Ok(stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect())
    }

    /// `git ls-files` — repo-relative paths tracked at the current worktree.
    pub async fn tracked_files(&self) -> Result<Vec<PathBuf>, GitError> {
        let output = run_git_raw(&self.workdir, self.clock.as_ref(), ["ls-files"], None).await?;
        tracked_files_from_output(output)
    }

    /// Synchronous `git ls-files` for sync workflow phases such as `loom plan`.
    pub fn tracked_files_sync(&self) -> Result<Vec<PathBuf>, GitError> {
        let output = sync_git_raw(&self.workdir, &["ls-files"])?;
        tracked_files_from_output(output)
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
            return Err(cli_error(&output));
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
            return Err(cli_error(&output));
        }
        Ok(String::from_utf8(output.stdout)?)
    }

    /// `git diff --quiet <base> HEAD -- <path>` — true when `path` changed.
    pub async fn path_changed_since(&self, base: &str, path: &Path) -> Result<bool, GitError> {
        let path_str = path.to_string_lossy().into_owned();
        let output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            ["diff", "--quiet", base, "HEAD", "--", &path_str],
            None,
        )
        .await?;
        match output.status.code() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err(cli_error(&output)),
        }
    }

    /// `git show <rev>:<path>` — file contents at `rev`, or `None` if absent.
    pub async fn file_at_revision(
        &self,
        rev: &str,
        path: &Path,
    ) -> Result<Option<String>, GitError> {
        let path_str = path.to_string_lossy();
        let spec = format!("{rev}:{path_str}");
        let output = run_git_raw(&self.workdir, self.clock.as_ref(), ["show", &spec], None).await?;
        if output.status.success() {
            return Ok(Some(String::from_utf8(output.stdout)?));
        }
        Ok(None)
    }

    /// `git rev-parse HEAD:<path>` — blob SHA for a path at HEAD.
    pub async fn head_blob_sha(&self, path: &Path) -> Result<GitOid, GitError> {
        let path_str = path.to_string_lossy();
        let spec = format!("HEAD:{path_str}");
        let output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            ["rev-parse", &spec],
            None,
        )
        .await?;
        if !output.status.success() {
            return Err(cli_error(&output));
        }
        let raw = String::from_utf8(output.stdout)?;
        Ok(GitOid::new(raw.trim())?)
    }

    /// `git rev-parse <rev>:<path>` — blob SHA for a path at `rev`, if present.
    pub async fn blob_sha_at_revision(
        &self,
        rev: &str,
        path: &Path,
    ) -> Result<Option<GitOid>, GitError> {
        let path_str = path.to_string_lossy();
        let spec = format!("{rev}:{path_str}");
        let output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            ["rev-parse", &spec],
            None,
        )
        .await?;
        if !output.status.success() {
            return Ok(None);
        }
        let raw = String::from_utf8(output.stdout)?;
        Ok(Some(GitOid::new(raw.trim())?))
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
            return Err(cli_error(&output));
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
            return Err(cli_error(&output));
        }
        Ok(String::from_utf8(output.stdout)?)
    }

    /// `git rev-parse HEAD` — full SHA of the current `HEAD`.
    pub async fn head_commit_sha(&self) -> Result<GitOid, GitError> {
        let output = run_git_raw(
            &self.workdir,
            self.clock.as_ref(),
            ["rev-parse", "HEAD"],
            None,
        )
        .await?;
        if !output.status.success() {
            return Err(cli_error(&output));
        }
        let raw = String::from_utf8(output.stdout)?;
        Ok(GitOid::new(raw.trim())?)
    }

    /// Merge `branch` into the configured integration branch inside the
    /// loom workspace ([`Self::loom_workspace`]). Rebases `branch` onto
    /// the current integration-branch `HEAD` first so the merge is
    /// always a fast-forward — sequential dispatch (bead branch is a
    /// strict descendant of `HEAD`) is a rebase no-op; parallel
    /// dispatch's second-and-later beads pick up the moved `HEAD` from
    /// an earlier merge. True overlap surfaces as
    /// [`MergeResult::Conflict`]; other failures surface as [`GitError`].
    ///
    /// The integration-branch name comes from the constructor (via
    /// `[loom] integration_branch`) — never from a `symbolic-ref HEAD`
    /// query, so the value is unambiguously the operator's configured
    /// target rather than whatever happens to be checked out.
    pub async fn merge_branch(&self, branch: &str) -> Result<MergeResult, GitError> {
        match self.rebase_onto_integration(branch).await? {
            RebaseOutcome::Conflict {
                detail,
                files,
                new_base_sha,
            } => Ok(MergeResult::Conflict {
                detail,
                files,
                new_base_sha,
            }),
            RebaseOutcome::Rebased => {
                self.ff_merge_integration(branch).await?;
                Ok(MergeResult::Ok)
            }
        }
    }

    /// Rebase `branch` onto the configured integration branch inside the
    /// loom workspace, replaying any rerere-recorded conflict resolution.
    /// On [`RebaseOutcome::Rebased`] the workspace is left checked out on
    /// the rewritten `branch`, whose `<integration-branch>..<branch>`
    /// range is the rebased commits ready for pass-2 signature
    /// verification; the integration branch has *not* moved yet — the
    /// fast-forward happens in [`Self::ff_merge_integration`]. Splitting
    /// the rebase from the ff-merge lets the verdict gate verify the
    /// rewritten commits between the two steps (per `specs/harness.md`
    /// § Bead dispatch — Bead branch flow), so a pass-2 failure leaves
    /// the integration line untouched.
    ///
    /// rerere (enabled in the loom workspace at `loom init`) replays a
    /// previously-recorded resolution before falling through to
    /// `integration-conflict` recovery. Because `rerere.autoupdate`
    /// stages the recorded resolution into the index but still pauses the
    /// rebase awaiting `--continue`, a paused rebase with no remaining
    /// unmerged paths means rerere resolved the conflict: the
    /// `--continue` drive loop below carries it to completion. Paths that
    /// stay unmerged are a conflict rerere has no record for — abort,
    /// restore the integration branch, and surface
    /// [`RebaseOutcome::Conflict`].
    pub async fn rebase_onto_integration(&self, branch: &str) -> Result<RebaseOutcome, GitError> {
        self.refresh_loom_signing_config()?;
        let workdir = self.loom_workspace();
        let integration_branch = self.integration_branch.as_str();

        // Upper bound on `--continue` drives: the rebase replays one
        // commit per step, so it can pause for a rerere-resolved conflict
        // at most once per replayed commit. The `+ 1` absorbs an
        // off-by-one when the final commit also conflicts; the bound
        // keeps a non-advancing `--continue` (e.g. a resolution that
        // emptied the commit) from spinning forever.
        let count_output = run_git_raw(
            &workdir,
            self.clock.as_ref(),
            [
                "rev-list",
                "--count",
                &format!("{integration_branch}..{branch}"),
            ],
            None,
        )
        .await?;
        if !count_output.status.success() {
            // A failed `rev-list --count` (bad range, spawn failure,
            // timeout) must not collapse to the sentinel 0 — that would
            // silently degrade `max_steps` to 1 and starve the `--continue`
            // drive loop below, indistinguishable from a legitimately empty
            // range (RS-11). Surface the CLI error instead.
            return Err(cli_error(&count_output));
        }
        let count_stdout = String::from_utf8(count_output.stdout)?;
        let max_steps = count_stdout
            .trim()
            .parse::<u32>()
            .map_err(|e: std::num::ParseIntError| GitError::GitCli {
                status: 0,
                stderr: format!("rev-list --count returned non-integer `{count_stdout}`: {e}"),
            })?
            .saturating_add(1);

        // The rebase / --continue / --abort / checkout below all take the
        // loom workspace's `index.lock`; a cross-spec peer holding it loses
        // the race to a clear, retryable error rather than a spurious
        // conflict (specs/harness.md § Concurrency).
        let mut output = run_git_index_mut(
            &workdir,
            self.clock.as_ref(),
            ["rebase", integration_branch, branch],
            None,
        )
        .await?;
        let mut steps = 0u32;
        while !output.status.success() {
            // Capture the unmerged-path set while the rebase is still
            // paused mid-conflict — `git rebase --abort` discards it. A
            // non-conflict refusal (unstaged changes) leaves no
            // diff-filter=U entries, so `files` comes back empty.
            let files = self.unmerged_paths(&workdir).await?;
            if files.is_empty() && steps < max_steps {
                // rerere staged a full resolution but the rebase paused
                // awaiting `--continue`; carry it forward.
                // `-c core.editor=true` keeps `--continue` from opening an
                // editor for the reused commit message.
                steps += 1;
                output = run_git_index_mut(
                    &workdir,
                    self.clock.as_ref(),
                    ["-c", "core.editor=true", "rebase", "--continue"],
                    None,
                )
                .await?;
                continue;
            }
            // Genuine conflict (or a stuck `--continue`). Capture stderr
            // BEFORE the abort+checkout so the caller sees the actual
            // rebase refusal (content conflict vs. "cannot rebase: You
            // have unstaged changes" vs. anything else) rather than the
            // cleanup commands' output.
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let _ = run_git_index_mut(&workdir, self.clock.as_ref(), ["rebase", "--abort"], None)
                .await?;
            let checkout = run_git_index_mut(
                &workdir,
                self.clock.as_ref(),
                ["checkout", "-q", integration_branch],
                None,
            )
            .await?;
            if !checkout.status.success() {
                return Err(cli_error(&checkout));
            }
            // The integration-branch tip is the base the rebase
            // targeted; the agent's retry rebases its branch onto this.
            let new_base_raw = run_git_raw(
                &workdir,
                self.clock.as_ref(),
                ["rev-parse", integration_branch],
                None,
            )
            .await?;
            let new_base_sha = GitOid::new(String::from_utf8_lossy(&new_base_raw.stdout).trim())?;
            return Ok(RebaseOutcome::Conflict {
                detail,
                files,
                new_base_sha,
            });
        }
        Ok(RebaseOutcome::Rebased)
    }

    /// Fast-forward the configured integration branch to the tip of the
    /// already-rebased `branch` (see [`Self::rebase_onto_integration`]).
    /// Checks out the integration branch and runs `git merge --ff-only`
    /// so history stays linear (no merge commits); a non-fast-forward
    /// refusal surfaces as [`GitError`].
    pub async fn ff_merge_integration(&self, branch: &str) -> Result<(), GitError> {
        let workdir = self.loom_workspace();
        let integration_branch = self.integration_branch.as_str();

        // Both the checkout and the ff-merge take the loom workspace's
        // `index.lock`; a cross-spec peer holding it loses the race to a
        // retryable [`GitError::IndexLocked`] rather than a spurious
        // failure (specs/harness.md § Concurrency).
        let checkout = run_git_index_mut(
            &workdir,
            self.clock.as_ref(),
            ["checkout", "-q", integration_branch],
            None,
        )
        .await?;
        if !checkout.status.success() {
            return Err(cli_error(&checkout));
        }

        let output = run_git_index_mut(
            &workdir,
            self.clock.as_ref(),
            ["merge", "--ff-only", branch],
            None,
        )
        .await?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(GitError::GitCli {
            status: output.status.code().unwrap_or(-1),
            stderr,
        })
    }

    /// Commit SHA the integration branch currently points at, read in the
    /// loom workspace (`git rev-parse <integration-branch>`). Used to tell
    /// whether a bead's ff-merge actually advanced the integration line so
    /// the per-bead audit rollback fires only when there is a bead commit
    /// to unwind (`specs/harness.md` § Verdict Gate).
    pub async fn integration_commit_sha(&self) -> Result<GitOid, GitError> {
        let output = run_git_raw(
            &self.loom_workspace(),
            self.clock.as_ref(),
            ["rev-parse", self.integration_branch.as_str()],
            None,
        )
        .await?;
        if !output.status.success() {
            return Err(cli_error(&output));
        }
        let raw = String::from_utf8(output.stdout)?;
        Ok(GitOid::new(raw.trim())?)
    }

    /// Roll the integration branch back one commit (`git reset --hard
    /// HEAD~1`) in the loom workspace.
    ///
    /// The per-bead audit (`loom gate verify --diff`) runs after the
    /// ff-merge but before any origin push; an audit failure undoes the
    /// just-merged commit so the integration line returns to its
    /// pre-merge tip and the next iteration sees the cross-bead breakage
    /// (`specs/harness.md` § Verdict Gate — `post-integrate-fail`). Takes
    /// the loom workspace's `index.lock`; a cross-spec peer holding it
    /// loses the race to a retryable [`GitError::IndexLocked`].
    pub async fn rollback_integration(&self) -> Result<(), GitError> {
        let output = run_git_index_mut(
            &self.loom_workspace(),
            self.clock.as_ref(),
            ["reset", "--hard", "HEAD~1"],
            None,
        )
        .await?;
        if output.status.success() {
            return Ok(());
        }
        Err(cli_error(&output))
    }

    /// Check out the configured integration branch in the loom workspace.
    ///
    /// A successful [`Self::rebase_onto_integration`] leaves the workspace
    /// on the rewritten bead branch; the pass-2 signature-failure cleanup
    /// uses this to return to the integration branch before deleting that
    /// transient branch (git refuses to delete the checked-out branch) so a
    /// driver-side rejection leaves the integration line as the checked-out
    /// state with the transient ref gone. When the workspace is already on
    /// the integration branch (the pass-1 path) it is a no-op.
    pub async fn checkout_integration(&self) -> Result<(), GitError> {
        let workdir = self.loom_workspace();
        let integration_branch = self.integration_branch.as_str();
        let checkout = run_git_index_mut(
            &workdir,
            self.clock.as_ref(),
            ["checkout", "-q", integration_branch],
            None,
        )
        .await?;
        if checkout.status.success() {
            return Ok(());
        }
        Err(cli_error(&checkout))
    }

    /// Unmerged paths in `workdir` (`git diff --name-only
    /// --diff-filter=U`). Used while a rebase is paused mid-conflict to
    /// tell a genuine conflict from a rerere-staged resolution. A clean
    /// index yields an empty list; a failed `git diff` surfaces as
    /// [`GitError`] rather than collapsing to an empty list, which the
    /// caller would otherwise mistake for a no-conflict result (RS-11).
    async fn unmerged_paths(&self, workdir: &Path) -> Result<Vec<PathBuf>, GitError> {
        let output = run_git_raw(
            workdir,
            self.clock.as_ref(),
            ["diff", "--name-only", "--diff-filter=U"],
            None,
        )
        .await?;
        if !output.status.success() {
            return Err(cli_error(&output));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|l| PathBuf::from(l.trim()))
            .filter(|p| !p.as_os_str().is_empty())
            .collect())
    }

    /// Path to the loom-workspace allowed_signers file the per-bead
    /// integration step verifies commits against. Written by `loom init`
    /// and refreshed before signing-sensitive git operations when a wrix
    /// signing key resolves (see `specs/harness.md` § Commit signing).
    /// Absent when no key is configured.
    fn allowed_signers_path(&self) -> PathBuf {
        self.loom_workspace()
            .join(".git")
            .join("loom-allowed-signers")
    }

    /// Whether driver-side signature verification is active in the loom
    /// workspace. True only when the allowed_signers file exists — i.e. a
    /// wrix signing key resolved at `loom init` or the latest signing
    /// refresh. When false the per-bead integration step skips both
    /// verify-signature passes (the spec-sanctioned "no key" path —
    /// `specs/harness.md` § Verdict Gate, phase 2).
    pub async fn signing_verification_enabled(&self) -> Result<bool, GitError> {
        // `try_exists` already reports Ok(false) for an absent file (the
        // spec-sanctioned "no key" path), so a genuine Err here is an
        // anomalous IO failure (permission denied, symlink loop) on a path
        // under our own `.git/`. Propagate it rather than swallowing to
        // false — collapsing it to "no key" would fail open, silently
        // skipping both verify-signature passes on a security control.
        Ok(tokio::fs::try_exists(self.allowed_signers_path()).await?)
    }

    /// Verify every commit in `range` (e.g. `main..loom/<id>`) against the
    /// loom workspace's allowed_signers file with `git verify-commit`.
    ///
    /// Returns [`SignatureCheck::Skipped`] when signing verification is
    /// disabled (no allowed_signers file — no key configured), so the
    /// per-bead integration step proceeds unchanged on hosts where wrix
    /// signing is not set up. When enabled, walks the commits oldest-first
    /// and returns [`SignatureCheck::Failed`] on the first commit
    /// `git verify-commit` rejects, carrying the offending sha + stderr so
    /// the caller can route to `signature-verification-failed`.
    pub async fn verify_commit_range(&self, range: &str) -> Result<SignatureCheck, GitError> {
        self.refresh_loom_signing_config()?;
        if !self.signing_verification_enabled().await? {
            return Ok(SignatureCheck::Skipped);
        }
        let workdir = self.loom_workspace();
        let listed = run_git_raw(
            &workdir,
            self.clock.as_ref(),
            ["rev-list", "--reverse", range],
            None,
        )
        .await?;
        let shas: Vec<String> = String::from_utf8_lossy(&listed.stdout)
            .lines()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        for sha in &shas {
            let out =
                run_git_raw(&workdir, self.clock.as_ref(), ["verify-commit", sha], None).await?;
            if !out.status.success() {
                return Ok(SignatureCheck::Failed {
                    commit: sha.clone(),
                    detail: String::from_utf8_lossy(&out.stderr).trim().to_string(),
                });
            }
        }
        Ok(SignatureCheck::Verified)
    }

    /// Fast-forward the loom workspace's integration branch to
    /// `origin/<integration-branch>` before any bead clone is
    /// materialized, so every `loom/<id>` branch forks from published
    /// `HEAD` rather than a stale local base.
    ///
    /// Delegates to [`fast_forward_loom_workspace_to_origin`] inside
    /// `spawn_blocking`. A diverged integration line (local commits not on
    /// origin) surfaces as [`GitError::IntegrationDiverged`] so the caller
    /// fails loud instead of branching off a split-brained base (per
    /// `specs/harness.md` § Bead dispatch).
    pub async fn fast_forward_integration_to_origin(&self) -> Result<FastForwardOutcome, GitError> {
        let loom_workspace = self.loom_workspace();
        let branch = self.integration_branch.clone();
        spawn_blocking(move || fast_forward_loom_workspace_to_origin(&loom_workspace, &branch))
            .await?
    }
}

/// Outcome of [`GitClient::fast_forward_integration_to_origin`] /
/// [`fast_forward_loom_workspace_to_origin`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FastForwardOutcome {
    /// The loom workspace's integration branch already matched
    /// `origin/<branch>` — no ref moved.
    AlreadyUpToDate,
    /// The integration branch was fast-forwarded to `origin/<branch>`,
    /// advancing by `advanced` published commits.
    FastForwarded { advanced: u32 },
    /// No fast-forward was attempted: the loom workspace is absent or
    /// `origin/<branch>` does not resolve (fresh fixture, unpublished
    /// branch). The caller proceeds without reconciliation.
    Skipped,
}

/// Fast-forward the loom workspace's local `branch` to `origin/<branch>`
/// so bead clones always fork from published `HEAD`.
///
/// Fetches `origin <branch>`, then compares the local branch to
/// `origin/<branch>`: already equal → [`FastForwardOutcome::AlreadyUpToDate`];
/// origin strictly ahead → `git merge --ff-only` →
/// [`FastForwardOutcome::FastForwarded`]; local carries commits not on origin
/// (diverged) → [`GitError::IntegrationDiverged`] naming the divergent
/// commits. Returns [`FastForwardOutcome::Skipped`] when `loom_workspace`
/// does not exist or `origin/<branch>` does not resolve.
///
/// Synchronous: invoked directly from `loom init` and, via `spawn_blocking`,
/// from `loom loop` startup ([`GitClient::fast_forward_integration_to_origin`]).
pub fn fast_forward_loom_workspace_to_origin(
    loom_workspace: &Path,
    branch: &str,
) -> Result<FastForwardOutcome, GitError> {
    if !loom_workspace.exists() {
        return Ok(FastForwardOutcome::Skipped);
    }
    let fetch = sync_git_raw(loom_workspace, &["fetch", "--quiet", "origin", branch])?;
    if !fetch.status.success() {
        return Err(cli_error(&fetch));
    }
    let remote_ref = format!("origin/{branch}");
    let resolve = sync_git_raw(
        loom_workspace,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{remote_ref}^{{commit}}"),
        ],
    )?;
    if !resolve.status.success() {
        return Ok(FastForwardOutcome::Skipped);
    }
    let ahead = sync_rev_count(loom_workspace, &format!("{remote_ref}..{branch}"))?;
    if ahead > 0 {
        let commits = sync_git_capture(
            loom_workspace,
            &["rev-list", "--oneline", &format!("{remote_ref}..{branch}")],
        )?;
        return Err(GitError::IntegrationDiverged {
            branch: branch.to_string(),
            commits: commits.trim().to_string(),
        });
    }
    let behind = sync_rev_count(loom_workspace, &format!("{branch}..{remote_ref}"))?;
    if behind == 0 {
        return Ok(FastForwardOutcome::AlreadyUpToDate);
    }
    let merged = sync_git_raw(loom_workspace, &["merge", "--ff-only", &remote_ref])?;
    if !merged.status.success() {
        return Err(cli_error(&merged));
    }
    Ok(FastForwardOutcome::FastForwarded { advanced: behind })
}

/// Synchronous `git -C <workspace> <args>` returning the raw `Output`
/// (status preserved for skip / divergence classification). Spawn failures
/// surface as [`GitError::Spawn`]; non-zero exits are the caller's to
/// interpret.
fn sync_git_raw(workspace: &Path, args: &[&str]) -> Result<std::process::Output, GitError> {
    use std::process::Command as StdCommand;
    StdCommand::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .map_err(GitError::Spawn)
}

fn tracked_files_from_output(output: std::process::Output) -> Result<Vec<PathBuf>, GitError> {
    if !output.status.success() {
        return Err(cli_error(&output));
    }
    let stdout = String::from_utf8(output.stdout)?;
    Ok(stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect())
}

/// Synchronous `git -C <workspace> rev-list --count <range>` parsed to a
/// commit count. Used by [`fast_forward_loom_workspace_to_origin`] to
/// classify the local-vs-origin relationship.
fn sync_rev_count(workspace: &Path, range: &str) -> Result<u32, GitError> {
    let raw = sync_git_capture(workspace, &["rev-list", "--count", range])?;
    raw.trim()
        .parse()
        .map_err(|e: std::num::ParseIntError| GitError::GitCli {
            status: 0,
            stderr: format!("rev-list --count {range} returned non-integer `{raw}`: {e}"),
        })
}

/// Outcome of [`GitClient::verify_commit_range`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureCheck {
    /// No allowed_signers file resolved — verification skipped. The
    /// per-bead integration step proceeds (spec-sanctioned "no key" path).
    Skipped,
    /// Every commit in range carried a signature trusted by the loom
    /// workspace's allowed_signers file.
    Verified,
    /// `git verify-commit` rejected `commit`; `detail` is its stderr.
    Failed { commit: String, detail: String },
}

/// Initialize a real git repository at `path` with one `initial` commit
/// and a bare `origin` remote alongside it. The bare `origin` lives at a
/// sibling `<path>.git`; production `loom loop` pushes the integration
/// branch to `origin` after every successful per-bead merge, so tests
/// that exercise the merge-back path need a real push destination or the
/// post-merge push gate would fail.
///
/// Does **not** materialize the loom-owned
/// `.loom/integration/` workspace — `loom init` tests depend on
/// observing a clean state, and use [`run_loom_init`-equivalent] paths
/// to create it. Tests that need a ready-to-use loom workspace (most of
/// the driver / workflow integration suite) call
/// [`init_test_repo_with_integration`].
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
    init_bare_test_repo(path, DEFAULT_INTEGRATION_BRANCH)
}

/// Same as [`init_test_repo`] but additionally clones the loom-owned
/// integration workspace at `<path>/.loom/integration/` from the
/// bare origin so [`GitClient::create_worktree`],
/// [`GitClient::merge_branch`], [`GitClient::push`], and
/// [`GitClient::delete_branch`] all have their loom-workspace cwd ready.
#[doc(hidden)]
pub fn init_test_repo_with_integration(path: &Path) -> Result<GitClient, GitError> {
    init_test_repo_with_integration_branch(path, DEFAULT_INTEGRATION_BRANCH)
}

/// Like [`init_test_repo_with_integration`] but takes an explicit
/// integration-branch name — used by tests that exercise
/// `[loom] integration_branch = "<name>"` end-to-end without a
/// hard-coded `main`.
#[doc(hidden)]
pub fn init_test_repo_with_integration_branch(
    path: &Path,
    branch: &str,
) -> Result<GitClient, GitError> {
    init_bare_test_repo(path, branch)?;
    let origin_path = bare_origin_path(path);
    let origin_url = origin_path.to_string_lossy().into_owned();
    let loom_workspace = path.join(LOOM_WORKSPACE_REL);
    if let Some(parent) = loom_workspace.parent() {
        std::fs::create_dir_all(parent)?;
    }
    clone_loom_workspace(&origin_url, &loom_workspace, branch)?;
    run_test_git(
        &loom_workspace,
        &["config", "user.email", "test@example.com"],
    )?;
    run_test_git(&loom_workspace, &["config", "user.name", "Test"])?;
    run_test_git(&loom_workspace, &["config", "commit.gpgsign", "false"])?;
    let hooks = ensure_test_prek_hooks(path)?;
    super::hooks::write_hooks_config(&loom_workspace, &hooks)?;
    GitClient::open_with_integration_branch(path, branch.to_string())
}

fn ensure_test_prek_hooks(path: &Path) -> Result<PathBuf, GitError> {
    use std::os::unix::fs::PermissionsExt;
    let hooks = path.join(".loom/test-prek-hooks");
    std::fs::create_dir_all(&hooks)?;
    for hook in ["pre-commit", "pre-push"] {
        let script = hooks.join(hook);
        std::fs::write(&script, "#!/bin/sh\nexit 0\n")?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(hooks)
}

fn init_bare_test_repo(path: &Path, branch: &str) -> Result<GitClient, GitError> {
    std::fs::create_dir_all(path)?;
    run_test_git(path, &["init", "-q", "-b", branch])?;
    run_test_git(path, &["config", "user.email", "test@example.com"])?;
    run_test_git(path, &["config", "user.name", "Test"])?;
    run_test_git(path, &["config", "commit.gpgsign", "false"])?;
    std::fs::write(path.join("README.md"), "initial\n")?;
    // Mirror the production `.gitignore` so the loom workspace at
    // `.loom/integration/` does not show as untracked in the
    // operator workspace's `git status`.
    std::fs::write(path.join(".gitignore"), ".loom/\n.wrix/\ntarget/\n")?;
    run_test_git(path, &["add", "README.md", ".gitignore"])?;
    run_test_git(path, &["commit", "-q", "-m", "initial"])?;
    let origin_path = bare_origin_path(path);
    if let Some(parent) = origin_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(&origin_path)?;
    run_test_git(&origin_path, &["init", "-q", "--bare", "-b", branch])?;
    let origin_url = origin_path.to_string_lossy().into_owned();
    run_test_git(path, &["remote", "add", "origin", &origin_url])?;
    run_test_git(path, &["push", "-q", "-u", "origin", branch])?;
    GitClient::open(path)
}

/// Stage all changes in `workspace` and commit them with `msg`. Test
/// support for exercising the loom-workspace integration line (e.g. the
/// `post-integrate-fail` rollback) without callers outside this module
/// shelling `git`, which the git-client-encapsulation gate forbids.
#[doc(hidden)]
pub fn commit_all_in(workspace: &Path, msg: &str) -> Result<(), GitError> {
    run_test_git(workspace, &["add", "-A"])?;
    run_test_git(workspace, &["commit", "-q", "-m", msg])
}

fn run_test_git(dir: &Path, args: &[&str]) -> Result<(), GitError> {
    use std::process::Command as StdCommand;
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
}

/// Read the URL of the `origin` remote at `workdir` using
/// `git config --get remote.origin.url`. Returns `Ok(None)` when `workdir`
/// is not a git repository (exit 128) or has no `origin` remote (exit 1).
///
/// Synchronous — used by `loom init`, which is a one-shot workspace
/// bootstrap and not driven by tokio.
pub fn read_origin_url(workdir: &Path) -> Result<Option<String>, GitError> {
    use std::process::Command as StdCommand;
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .map_err(GitError::Spawn)?;
    if output.status.success() {
        let url = String::from_utf8(output.stdout)?.trim().to_string();
        return Ok((!url.is_empty()).then_some(url));
    }
    if matches!(output.status.code(), Some(1) | Some(128)) {
        return Ok(None);
    }
    Err(GitError::GitCli {
        status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// One-shot `git clone --branch <branch> <origin_url> <dest>` used by
/// `loom init` to materialize the loom-owned integration workspace at
/// `<workspace>/.loom/integration/`. Caller guarantees `dest` does
/// not exist; the parent directory is created if missing.
///
/// Synchronous: `loom init` is not async, and the spec marks the operation
/// as one-shot + infrequent (see § Git operations table).
pub fn clone_loom_workspace(origin_url: &str, dest: &Path, branch: &str) -> Result<(), GitError> {
    use std::process::Command as StdCommand;
    let parent = dest.parent().ok_or_else(|| GitError::GitCli {
        status: -1,
        stderr: format!(
            "clone destination {} has no parent directory",
            dest.display()
        ),
    })?;
    std::fs::create_dir_all(parent)?;
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(parent)
        .args(["clone", "--quiet", "--branch", branch])
        .arg(origin_url)
        .arg(dest)
        .output()
        .map_err(GitError::Spawn)?;
    if output.status.success() {
        return Ok(());
    }
    Err(GitError::GitCli {
        status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Synchronous `git -C <workspace> rev-parse HEAD^{tree}`. Used by
/// `loom gate verify-marker`, which is a one-shot CLI helper that
/// does not justify the cost of standing up a tokio runtime for two
/// git invocations.
pub fn head_tree_oid_sync(workspace: &Path) -> Result<GitOid, GitError> {
    let raw = sync_git_capture(workspace, &["rev-parse", "HEAD^{tree}"])?;
    Ok(GitOid::new(raw.trim())?)
}

/// Synchronous `git -C <workspace> rev-parse HEAD`. The informational
/// commit SHA stamped onto a freshly minted [`crate::marker`] proof
/// (the load-bearing fingerprint is the tree OID, not the commit SHA).
pub fn sync_head_commit_sha(workspace: &Path) -> Result<GitOid, GitError> {
    sync_rev_parse(workspace, "HEAD")
}

/// Synchronous `git -C <workspace> rev-parse --verify <rev>`.
pub fn sync_rev_parse(workspace: &Path, rev: &str) -> Result<GitOid, GitError> {
    let raw = sync_git_capture(workspace, &["rev-parse", "--verify", rev])?;
    Ok(GitOid::new(raw.trim())?)
}

/// Synchronous `git -C <workspace> status --porcelain`. Paired with
/// [`head_tree_oid_sync`] for the marker fingerprint check.
pub fn status_porcelain_sync(workspace: &Path) -> Result<String, GitError> {
    sync_git_capture(workspace, &["status", "--porcelain"])
}

fn sync_git_capture(workspace: &Path, args: &[&str]) -> Result<String, GitError> {
    use std::process::Command as StdCommand;
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .map_err(GitError::Spawn)?;
    if !output.status.success() {
        return Err(GitError::GitCli {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8(output.stdout)?)
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

/// Outcome of [`GitClient::rebase_onto_integration`] — the rebase half of
/// the per-bead integration step, split from the fast-forward
/// ([`GitClient::ff_merge_integration`]) so the verdict gate can verify
/// the rewritten commits in between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebaseOutcome {
    /// `branch` was rebased onto the integration tip (replaying any
    /// rerere-recorded resolution). The workspace is checked out on the
    /// rewritten `branch`; the integration branch has not moved.
    Rebased,
    /// `git rebase` left unmerged paths rerere had no recorded resolution
    /// for. The rebase was aborted and the integration branch restored.
    /// `detail`/`files`/`new_base_sha` carry the same recovery payload as
    /// [`MergeResult::Conflict`].
    Conflict {
        detail: String,
        files: Vec<PathBuf>,
        new_base_sha: GitOid,
    },
}

/// Outcome of [`GitClient::merge_branch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeResult {
    Ok,
    /// `git rebase` exited non-zero. `detail` carries the rebase's
    /// stderr (newline-preserved, trailing whitespace trimmed) so the
    /// caller can distinguish a real content conflict from a
    /// "cannot rebase: You have unstaged changes" refusal — both used
    /// to map to the same opaque `Conflict` and the actual cause was
    /// lost at the warn! line. Empty string is allowed but unusual.
    ///
    /// `files` is the unmerged-path set captured (`git diff
    /// --name-only --diff-filter=U`) before the `git rebase --abort`,
    /// and `new_base_sha` is the integration-branch tip the rebase was
    /// targeting. Both ride out to the verdict gate's
    /// `integration-conflict` recovery so the agent's single retry can
    /// rebase its bead-workspace branch onto the new tip and resolve
    /// the named files (per `specs/harness.md` § Verdict Gate). For a
    /// non-conflict rebase refusal (e.g. unstaged changes) `files` is
    /// empty.
    Conflict {
        detail: String,
        files: Vec<PathBuf>,
        new_base_sha: GitOid,
    },
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
    Err(cli_error(&output))
}

/// Construct `GitError::GitCli` from a `git` invocation's `Output`,
/// preserving natural newlines in stderr (and folding in stdout, under a
/// `[stdout]` heading, when it carries content too). `git push` failures
/// in particular emit multiple lines (`error:` + `hint:` + `! [rejected]`)
/// AND the rejecting pre-push hook (e.g. `nix flake check`) typically
/// writes its failure diagnostic to stdout while git itself only emits a
/// terse "failed to push some refs" on stderr — without the stdout fold,
/// hook-driven push failures bottom out at the one-line git wrapper and
/// the actual cause is lost. Trailing whitespace is stripped per-line;
/// fully-empty leading/trailing lines are dropped.
/// Classify a failed git invocation as loom-workspace lock contention.
/// When a concurrent process holds `index.lock`, git refuses to start the
/// index-mutating command and writes `fatal: Unable to create
/// '<path>/index.lock': File exists.` plus an "Another git process seems to
/// be running" hint to `stderr`, having mutated nothing. The
/// [`run_git_index_mut`] retry loop keys off this signature to distinguish
/// recoverable contention (retry) from a content failure or rebase conflict
/// (surface), matching on the `index.lock` lock-file name the spec names as
/// the serialization point rather than the variable cleanup output.
fn is_index_locked(output: &std::process::Output) -> bool {
    if output.status.success() {
        return false;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.contains("index.lock")
        && (stderr.contains("File exists")
            || stderr.contains("Another git process seems to be running"))
}

/// Run an index-mutating git invocation in the loom workspace, retrying on
/// git lock-file contention ([`is_index_locked`]). Two `loom loop`
/// invocations on different specs share the loom workspace and collide on
/// its `index.lock` during the rebase + ff critical section; the command
/// that loses the race mutated nothing, so re-running it is safe and picks
/// up the winner's now-current integration tip (specs/harness.md §
/// Concurrency). Bounded by [`INDEX_LOCK_RETRIES`] with an
/// [`INDEX_LOCK_BACKOFF`] pause between attempts (clock-driven so paused-time
/// tests stay deterministic); exhaustion surfaces [`GitError::IndexLocked`]
/// rather than the raw CLI error so the caller can tell stale-lock
/// contention from a content failure. The returned `Output` is the first
/// attempt that was *not* lock-contended — its status may still be a
/// non-zero conflict the caller inspects.
async fn run_git_index_mut<I, S>(
    workdir: &Path,
    clock: &dyn Clock,
    args: I,
    trailing: Option<&OsString>,
) -> Result<std::process::Output, GitError>
where
    I: IntoIterator<Item = S> + Clone,
    S: AsRef<std::ffi::OsStr>,
{
    for _ in 0..INDEX_LOCK_RETRIES {
        let output = run_git_raw(workdir, clock, args.clone(), trailing).await?;
        if !is_index_locked(&output) {
            return Ok(output);
        }
        clock.sleep(INDEX_LOCK_BACKOFF).await;
    }
    Err(GitError::IndexLocked {
        workdir: workdir.to_path_buf(),
    })
}

fn cli_error(output: &std::process::Output) -> GitError {
    fn trim_lines(buf: &[u8]) -> String {
        let s: String = String::from_utf8_lossy(buf)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        s.trim().to_string()
    }
    let stderr = trim_lines(&output.stderr);
    let stdout = trim_lines(&output.stdout);
    let combined = match (stderr.is_empty(), stdout.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stderr,
        (true, false) => format!("[stdout]\n{stdout}"),
        (false, false) => format!("{stderr}\n[stdout]\n{stdout}"),
    };
    GitError::GitCli {
        status: output.status.code().unwrap_or(-1),
        stderr: combined,
    }
}

/// Create `<clone>/.wrix/` mode 0o777 so the container's dolt-socket
/// bind-mount target pre-exists and both the host user (post-session
/// pre-push hook) and container-namespace processes can write to it.
/// Without this, the container runtime mkdirs `.wrix/` as namespace-root
/// (host uid 100000) when materializing the mount, locking the host user
/// out of the dir and breaking the post-session push.
fn ensure_wrix_mount_dir(clone_path: &Path) -> Result<(), GitError> {
    use std::os::unix::fs::PermissionsExt;
    let dir = clone_path.join(".wrix");
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))?;
    Ok(())
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
            timeout_secs: timeout.as_secs(),
            workdir: workdir.to_path_buf(),
        }),
    }
}
