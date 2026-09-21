//! `loom init` — workspace bootstrap and optional cache-DB rebuild.
//!
//! Acquires the workspace lock (errors immediately if any phase or work-root
//! lock is held), ensures `<workspace>/loom.toml` and `.loom/cache.db`
//! exist, and — when `--rebuild` is passed — drops/recreates the cache DB
//! and repopulates it from specs and independent durable spec/work epics.

mod epic;
mod error;

pub use epic::fetch_epics;

use std::fs;
use std::path::{Path, PathBuf};

use loom_driver::config::LoomConfig;
use loom_driver::git::{
    clone_loom_workspace, enable_rerere, fast_forward_loom_workspace_to_origin, read_origin_url,
    resolve_prek_hooks_path_for_workspace, write_hooks_config,
};
#[cfg(test)]
use loom_driver::identifier::MoleculeId;
use loom_driver::lock::{LockGuard, LockManager};
use loom_driver::state::{CacheDb, RebuildEpic, RebuildReport};

pub use error::InitError;

/// Default body for `<workspace>/loom.toml`. Mirrors the Configuration
/// section of `specs/harness.md` verbatim so a fresh `loom init` writes
/// a file that round-trips through `LoomConfig::default()`.
pub const DEFAULT_CONFIG_TOML: &str = include_str!("default-loom.toml");

/// Options accepted by [`run`].
#[derive(Debug, Clone, Copy, Default)]
pub struct InitOpts {
    /// Drop and repopulate the cache DB from on-disk specs + active beads.
    pub rebuild: bool,
}

/// Files touched by [`run`] and (optionally) the rebuild report.
#[derive(Debug, Clone)]
pub struct InitReport {
    pub config_path: PathBuf,
    pub cache_db_path: PathBuf,
    pub config_created: bool,
    pub rebuild: Option<RebuildReport>,
    /// The loom-owned integration workspace at
    /// `<workspace>/.loom/integration/`. `None` when the operator
    /// workspace has no `origin` remote (the materialization step is
    /// silently skipped — fresh test fixtures and unconfigured workspaces
    /// can still `loom init` without acquiring an origin first).
    pub integration_workspace: Option<MaterializedIntegration>,
}

/// Outcome of the per-init materialization of
/// `<workspace>/.loom/integration/`.
#[derive(Debug, Clone)]
pub struct MaterializedIntegration {
    pub path: PathBuf,
    /// `true` when this invocation cloned the integration workspace; `false`
    /// when the directory already existed (idempotent re-init).
    pub created: bool,
}

/// Run `loom init` against `workspace`.
///
/// 1. Acquires the workspace lock — errors immediately with `WorkspaceBusy`
///    if any phase or work-root lock is held.
/// 2. Creates `<workspace>/.loom/` and writes `loom.toml` if it
///    does not already exist (existing config files are preserved).
/// 3. Opens `cache.db` (creating the schema on first open). When
///    `opts.rebuild` is true, the file is dropped and recreated, and the
///    schema is repopulated from `specs/*.md` plus `molecules`.
///
/// # Errors
///
/// Returns an error when workspace initialization or validation fails.
pub fn run(
    workspace: &Path,
    opts: InitOpts,
    molecules: &[RebuildEpic],
) -> Result<InitReport, InitError> {
    run_with_hooks_resolver(
        workspace,
        opts,
        molecules,
        resolve_prek_hooks_path_for_workspace,
    )
}

fn run_with_hooks_resolver(
    workspace: &Path,
    opts: InitOpts,
    molecules: &[RebuildEpic],
    resolve_hooks: impl Fn(&Path) -> Result<PathBuf, loom_driver::git::GitError>,
) -> Result<InitReport, InitError> {
    let lock_mgr = LockManager::new(workspace)?;
    let guard = lock_mgr.acquire_workspace()?;
    run_locked(workspace, opts, molecules, guard, resolve_hooks)
}

fn run_locked(
    workspace: &Path,
    opts: InitOpts,
    molecules: &[RebuildEpic],
    _guard: LockGuard,
    resolve_hooks: impl Fn(&Path) -> Result<PathBuf, loom_driver::git::GitError>,
) -> Result<InitReport, InitError> {
    let runtime_dir = workspace.join(".loom");
    fs::create_dir_all(&runtime_dir).map_err(|source| InitError::CreateDir {
        path: runtime_dir.clone(),
        source,
    })?;

    let config_path = LoomConfig::resolve_path(workspace);
    let cache_db_path = runtime_dir.join("cache.db");

    let config_created = !config_path.exists();
    if config_created {
        if let Some(parent) = config_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|source| InitError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&config_path, DEFAULT_CONFIG_TOML).map_err(|source| InitError::WriteConfig {
            path: config_path.clone(),
            source,
        })?;
    }

    let rebuild_report = if opts.rebuild {
        let db = CacheDb::recreate(&cache_db_path)?;
        Some(db.rebuild(workspace, molecules)?)
    } else {
        let _db = CacheDb::open(&cache_db_path)?;
        None
    };

    let integration_workspace =
        materialize_integration_workspace(workspace, &config_path, &resolve_hooks)?;

    Ok(InitReport {
        config_path,
        cache_db_path,
        config_created,
        rebuild: rebuild_report,
        integration_workspace,
    })
}

/// Materialize the loom-owned integration workspace at
/// `<workspace>/.loom/integration/` via a one-shot
/// `git clone <origin> .loom/integration`. Idempotent: when the
/// directory already exists this returns `Ok(Some { created: false })`
/// without touching git. Returns `Ok(None)` when the operator workspace
/// has no `origin` remote (the step is silently skipped — used by test
/// fixtures whose tempdirs aren't bound to a remote).
///
/// The integration branch comes from `[loom] integration_branch` in
/// `<config_path>` (default `main`). The cloned workspace has that branch
/// checked out and never switches — per `specs/harness.md § Bead Dispatch`.
fn materialize_integration_workspace(
    workspace: &Path,
    config_path: &Path,
    resolve_hooks: &impl Fn(&Path) -> Result<PathBuf, loom_driver::git::GitError>,
) -> Result<Option<MaterializedIntegration>, InitError> {
    let dest = workspace.join(".loom/integration");
    if dest.exists() {
        // Reconcile an existing integration line with published HEAD before
        // any later `loom loop` materializes bead clones off it. A diverged
        // line (local commits never pushed) fails loud rather than seeding
        // every bead with a stale base (per `specs/harness.md` § Bead
        // dispatch).
        let config = LoomConfig::load(config_path)?;
        fast_forward_loom_workspace_to_origin(&dest, &config.loom.integration_branch)?;
        enable_rerere(&dest)?;
        let hooks_path = resolve_hooks(workspace)?;
        write_hooks_config(&dest, &hooks_path)?;
        return Ok(Some(MaterializedIntegration {
            path: dest,
            created: false,
        }));
    }
    let Some(origin_url) = read_origin_url(workspace)? else {
        return Ok(None);
    };
    let config = LoomConfig::load(config_path)?;
    clone_loom_workspace(&origin_url, &dest, &config.loom.integration_branch)?;

    // Enable rerere unconditionally so driver-side rebases replay recorded
    // conflict resolutions. `loom loop` installs repository Git policy in a
    // separate, fail-fast startup preflight.
    enable_rerere(&dest)?;
    let hooks_path = resolve_hooks(workspace)?;
    write_hooks_config(&dest, &hooks_path)?;

    Ok(Some(MaterializedIntegration {
        path: dest,
        created: true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Result, anyhow};
    use loom_driver::config::{LoomConfig, Phase};
    use loom_driver::identifier::SpecLabel;
    use loom_driver::lock::LockError;
    use loom_driver::testing::epic_fixture;

    fn temp_workspace() -> Result<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        // Sanity: the workspace must contain a `specs/` for rebuild to work,
        // but `run()` itself does not require it — empty rebuild is valid.
        Ok(dir)
    }

    fn temp_child_workspace(root: &Path, label: &str) -> PathBuf {
        let unique = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("tmp");
        root.join(format!("{label}-{unique}"))
    }

    fn fake_prek_hooks(root: &Path) -> Result<PathBuf> {
        let hooks = root.join("fake-prek-hooks");
        std::fs::create_dir_all(&hooks)?;
        for hook in ["pre-commit", "pre-push"] {
            std::fs::write(hooks.join(hook), "#!/bin/sh\n")?;
        }
        Ok(hooks)
    }

    fn run_with_hooks(workspace: &Path, hooks: &Path) -> Result<InitReport, InitError> {
        run_with_hooks_resolver(workspace, InitOpts::default(), &[], |_workspace| {
            Ok(hooks.to_path_buf())
        })
    }

    /// Spec contract `[test]` annotation
    /// (`specs/harness.md` § Success Criteria · Loom Workspace):
    /// `loom init` materializes `<workspace>/.loom/integration/`
    /// as a one-shot `git clone <origin> .loom/integration`. The
    /// directory exists, contains a real `.git/`, and has the integration
    /// branch checked out (default `main`).
    #[test]
    fn loom_init_materializes_loom_workspace() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let workspace = temp_child_workspace(tmp.path(), "loom-init-materialize-ws");
        loom_driver::git::init_test_repo(&workspace)?;
        let hooks = fake_prek_hooks(tmp.path())?;

        let report = run_with_hooks(&workspace, &hooks)?;
        let integ = report
            .integration_workspace
            .ok_or_else(|| anyhow!("integration workspace must be materialized"))?;
        assert!(integ.created, "first init must clone the workspace");
        assert_eq!(integ.path, workspace.join(".loom/integration"));
        assert!(
            integ.path.join(".git").is_dir(),
            "integration workspace must contain a real `.git/` directory",
        );
        // The clone checks out the integration branch (default `main`,
        // matching what `init_test_repo` pushes to origin).
        let head = std::fs::read_to_string(integ.path.join(".git/HEAD"))?;
        assert!(
            head.contains("refs/heads/main"),
            "integration workspace HEAD must point at the integration branch; got: {head:?}",
        );
        Ok(())
    }

    /// Spec contract `[test]` annotation
    /// (`specs/harness.md` § Success Criteria · Bead dispatch): `loom init`
    /// configures the integration workspace's local `core.hooksPath` from
    /// wrix's canonical prek hooks directory.
    #[test]
    fn loom_init_configures_integration_hooks_path_from_wrix_prekhooks() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let workspace = temp_child_workspace(tmp.path(), "loom-init-hooks-ws");
        loom_driver::git::init_test_repo(&workspace)?;
        let hooks = fake_prek_hooks(tmp.path())?;

        let report = run_with_hooks(&workspace, &hooks)?;
        let integ = report
            .integration_workspace
            .ok_or_else(|| anyhow!("integration workspace must be materialized"))?;

        loom_driver::git::validate_hooks_config(&integ.path, &hooks)?;
        let config = std::fs::read_to_string(integ.path.join(".git/config"))?;
        assert!(config.contains("[rerere]"), "rerere preserved: {config}");
        Ok(())
    }

    #[test]
    fn loom_init_fails_loud_when_prek_hooks_path_cannot_be_resolved() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let workspace = temp_child_workspace(tmp.path(), "loom-init-hooks-missing-ws");
        loom_driver::git::init_test_repo(&workspace)?;

        let err = run_with_hooks_resolver(&workspace, InitOpts::default(), &[], |_workspace| {
            Err(loom_driver::git::GitError::PrekHooksUnresolved)
        })
        .expect_err("unresolved hooks must fail init");
        assert!(matches!(
            err,
            InitError::Git(loom_driver::git::GitError::PrekHooksUnresolved)
        ));
        Ok(())
    }

    #[test]
    fn loom_init_is_idempotent_when_integration_exists() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let workspace = temp_child_workspace(tmp.path(), "loom-init-idempotent-ws");
        loom_driver::git::init_test_repo(&workspace)?;
        let hooks = fake_prek_hooks(tmp.path())?;

        let first = run_with_hooks(&workspace, &hooks)?
            .integration_workspace
            .ok_or_else(|| anyhow!("first init must materialize integration workspace"))?;
        assert!(first.created, "first init must clone");

        // Sentinel inside the clone proves a second init does not blow it
        // away and re-clone.
        let sentinel = first.path.join("loom-sentinel.txt");
        std::fs::write(&sentinel, b"keep me")?;

        let second = run_with_hooks(&workspace, &hooks)?
            .integration_workspace
            .ok_or_else(|| anyhow!("second init must report the existing workspace"))?;
        assert!(
            !second.created,
            "second init must NOT clone — directory exists",
        );
        assert_eq!(second.path, first.path);
        assert!(
            sentinel.exists(),
            "sentinel file must survive — proves no re-clone",
        );
        Ok(())
    }

    /// Workspaces without an `origin` remote (typical for fresh test
    /// fixtures) silently skip the materialization step rather than
    /// failing `loom init` — the spec mandates materialization only when
    /// an origin is bound.
    #[test]
    fn loom_init_skips_integration_when_workspace_has_no_origin() -> Result<()> {
        let dir = temp_workspace()?;
        let report = run(dir.path(), InitOpts::default(), &[])?;
        assert!(
            report.integration_workspace.is_none(),
            "integration workspace must be skipped when no origin remote",
        );
        assert!(
            !dir.path().join(".loom/integration").exists(),
            "integration directory must NOT be created without an origin",
        );
        Ok(())
    }

    /// Spec contract `[test]` annotation (`specs/harness.md` § Success
    /// Criteria · Verdict Gate): `loom init` enables rerere
    /// (`rerere.enabled=true`, `rerere.autoupdate=true`) in the loom
    /// workspace's local `.git/config`, unconditionally.
    #[test]
    fn loom_init_enables_rerere_in_loom_workspace_gitconfig() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let workspace = temp_child_workspace(tmp.path(), "loom-init-rerere-ws");
        loom_driver::git::init_test_repo(&workspace)?;
        let hooks = fake_prek_hooks(tmp.path())?;

        let report = run_with_hooks(&workspace, &hooks)?;
        let integ = report
            .integration_workspace
            .ok_or_else(|| anyhow!("integration workspace must be materialized"))?;

        let config = std::fs::read_to_string(integ.path.join(".git/config"))?;
        assert!(config.contains("[rerere]"), "rerere section: {config}");
        assert!(
            config.contains("enabled = true"),
            "rerere.enabled: {config}"
        );
        assert!(
            config.contains("autoupdate = true"),
            "rerere.autoupdate: {config}",
        );
        Ok(())
    }

    #[test]
    fn run_creates_config_and_cache_db() -> Result<()> {
        let dir = temp_workspace()?;
        let report = run(dir.path(), InitOpts::default(), &[])?;
        assert!(report.config_created, "first init must write config");
        assert!(report.config_path.exists(), "loom.toml must exist on disk");
        assert!(report.cache_db_path.exists(), "cache.db must exist on disk");
        // The default body must parse cleanly and resolve through `agent_for`
        // identically to the empty-default config — the file writes the
        // built-in `[phase.default]` values explicitly for documentation,
        // which means the parsed `phase` map and `BTreeMap::new()` are not
        // structurally equal but resolve to the same agent selection.
        let body = std::fs::read_to_string(&report.config_path)?;
        let parsed = LoomConfig::from_toml_str(&body)?;
        let empty = LoomConfig::default();
        for phase in [
            Phase::Plan,
            Phase::Todo,
            Phase::Loop,
            Phase::Review,
            Phase::Inbox,
        ] {
            assert_eq!(
                parsed.agent_for(phase),
                empty.agent_for(phase),
                "phase={phase:?}",
            );
        }
        assert_eq!(parsed.pinned_context, empty.pinned_context);
        assert_eq!(parsed.beads, empty.beads);
        assert_eq!(parsed.loop_, empty.loop_);
        assert_eq!(parsed.logs, empty.logs);
        assert_eq!(parsed.claude, empty.claude);
        assert_eq!(parsed.security, empty.security);
        // No rebuild on a plain init.
        assert!(report.rebuild.is_none());
        Ok(())
    }

    #[test]
    fn run_preserves_existing_config_file() -> Result<()> {
        let dir = temp_workspace()?;
        let custom = "pinned_context = \"AGENTS.md\"\n";
        std::fs::write(dir.path().join("loom.toml"), custom)?;

        let report = run(dir.path(), InitOpts::default(), &[])?;
        assert!(!report.config_created);
        let body = std::fs::read_to_string(report.config_path)?;
        assert_eq!(body, custom, "existing config must not be overwritten");
        Ok(())
    }

    #[test]
    fn rebuild_drops_and_repopulates_cache_db() -> Result<()> {
        let dir = temp_workspace()?;
        let specs = dir.path().join("specs");
        std::fs::create_dir_all(&specs)?;
        std::fs::write(specs.join("alpha.md"), "# alpha\n")?;
        std::fs::write(specs.join("beta.md"), "# beta\n")?;

        // First init seeds the DB and bumps an iteration so we can prove
        // rebuild wiped it.
        run(dir.path(), InitOpts::default(), &[])?;
        let molecules = epic_fixture(
            MoleculeId::new("lm-mol1").unwrap(),
            SpecLabel::new("alpha").unwrap(),
            None,
        )?;
        let db = CacheDb::open(dir.path().join(".loom/cache.db"))?;
        db.rebuild(dir.path(), &molecules)?;
        let post = db.increment_iteration(&MoleculeId::new("lm-mol1").unwrap())?;
        assert_eq!(post, 1);
        drop(db);

        let report = run(dir.path(), InitOpts { rebuild: true }, &molecules)?;
        let rb = report
            .rebuild
            .ok_or_else(|| anyhow::anyhow!("rebuild must produce a report"))?;
        assert_eq!(rb.specs, 2, "two spec files");
        assert_eq!(rb.work_epics, 1, "one active work epic");

        // Iteration counter reset to 0 after rebuild.
        let db = CacheDb::open(dir.path().join(".loom/cache.db"))?;
        let row = db
            .work_epic(&MoleculeId::new("lm-mol1").unwrap())?
            .ok_or_else(|| anyhow::anyhow!("active work epic must exist"))?;
        assert_eq!(row.iteration_count, 0);
        Ok(())
    }

    /// A fresh `loom init` must lay down each of the four schema tables —
    /// `specs`, `molecules`, `companions`, `meta` — so subsequent commands
    /// see an immediately-queryable surface. Verified by exercising one
    /// read per table through the typed API; each query succeeds only when
    /// its backing table exists.
    #[test]
    fn run_creates_schema_with_specs_molecules_companions_meta() -> Result<()> {
        let dir = temp_workspace()?;
        let report = run(dir.path(), InitOpts::default(), &[])?;
        let db = CacheDb::open(&report.cache_db_path)?;
        let probe = SpecLabel::new("probe").unwrap();

        match db.spec(&probe) {
            Err(loom_driver::state::CacheError::SpecNotFound { .. }) => {}
            other => {
                return Err(anyhow!(
                    "expected SpecNotFound on empty specs table, got {other:?}"
                ));
            }
        }
        assert!(db.spec_epic(&probe)?.is_none());
        assert!(db.companions(&probe)?.is_empty());
        assert!(db.work_epics()?.is_empty());
        Ok(())
    }

    /// Plain `loom init` (no `--rebuild`) must preserve existing work-epic rows.
    #[test]
    fn run_is_idempotent_and_preserves_work_epics() -> Result<()> {
        let dir = temp_workspace()?;
        let specs = dir.path().join("specs");
        std::fs::create_dir_all(&specs)?;
        std::fs::write(specs.join("alpha.md"), "# alpha\n")?;

        run(dir.path(), InitOpts::default(), &[])?;
        let db_path = dir.path().join(".loom/cache.db");
        let db = CacheDb::open(&db_path)?;
        db.rebuild(
            dir.path(),
            &epic_fixture(
                MoleculeId::new("lm-mol1").unwrap(),
                SpecLabel::new("alpha").unwrap(),
                Some("deadbeef".into()),
            )?,
        )?;
        let bumped = db.increment_iteration(&MoleculeId::new("lm-mol1").unwrap())?;
        assert_eq!(bumped, 1);
        drop(db);

        let report = run(dir.path(), InitOpts::default(), &[])?;
        assert!(report.rebuild.is_none(), "plain init must not run rebuild");
        let db = CacheDb::open(&db_path)?;
        let row = db
            .work_epic(&MoleculeId::new("lm-mol1").unwrap())?
            .ok_or_else(|| anyhow!("work epic row was clobbered"))?;
        assert_eq!(row.epic_id.as_str(), "lm-mol1");
        assert_eq!(
            row.iteration_count, 1,
            "iteration counter must survive a plain init"
        );
        assert_eq!(row.base_commit.as_deref(), Some("deadbeef"));
        Ok(())
    }

    #[test]
    fn workspace_lock_errors_when_phase_lock_held() -> Result<()> {
        let dir = temp_workspace()?;
        let mgr = LockManager::new(dir.path())?;
        let _phase_guard = mgr.acquire_planning()?;
        match run(dir.path(), InitOpts::default(), &[]) {
            Err(InitError::Lock(LockError::WorkspaceBusy { root })) => {
                assert_eq!(root, "plan");
                Ok(())
            }
            other => Err(anyhow::anyhow!("expected WorkspaceBusy, got {other:?}")),
        }
    }
}
