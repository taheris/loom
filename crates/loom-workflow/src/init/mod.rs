//! `loom init` — workspace bootstrap and optional cache-DB rebuild.
//!
//! Acquires the workspace lock (errors immediately if any phase or work-root
//! lock is held), ensures `<workspace>/loom.toml` and `.loom/cache.db`
//! exist, and — when `--rebuild` is passed — drops/recreates the cache DB
//! and repopulates it from `specs/*.md` plus a caller-supplied slice of
//! active molecules.
//!
//! Subprocess work (calling `bd list --status=open --type=epic` to
//! enumerate epic beads, then filtering by `spec:<label>`) is split out
//! into [`fetch_active_molecules`] so the core init function stays sync
//! and unit-testable without a real `bd` binary.

mod error;

use std::fs;
use std::path::{Path, PathBuf};

use tracing::info;

use loom_driver::bd::{BdClient, CommandRunner, ListOpts, UpdateOpts};
use loom_driver::config::LoomConfig;
use loom_driver::git::{
    clone_loom_workspace, enable_rerere, fast_forward_loom_workspace_to_origin, read_origin_url,
    reconcile_signing_config, resolve_prek_hooks_path_for_workspace, resolve_signing_key,
    write_hooks_config,
};
use loom_driver::identifier::MoleculeId;
use loom_driver::lock::LockManager;
use loom_driver::state::{ActiveMolecule, CacheDb, RebuildReport};

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
pub fn run(
    workspace: &Path,
    opts: InitOpts,
    molecules: &[ActiveMolecule],
) -> Result<InitReport, InitError> {
    run_with_resolvers(
        workspace,
        opts,
        molecules,
        resolve_signing_key,
        resolve_prek_hooks_path_for_workspace,
    )
}

fn run_with_resolvers(
    workspace: &Path,
    opts: InitOpts,
    molecules: &[ActiveMolecule],
    resolve: impl Fn(&Path) -> Result<Option<PathBuf>, loom_driver::git::GitError>,
    resolve_hooks: impl Fn(&Path) -> Result<PathBuf, loom_driver::git::GitError>,
) -> Result<InitReport, InitError> {
    let lock_mgr = LockManager::new(workspace)?;
    let _guard = lock_mgr.acquire_workspace()?;

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
        materialize_integration_workspace(workspace, &config_path, &resolve, &resolve_hooks)?;

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
    resolve: &impl Fn(&Path) -> Result<Option<PathBuf>, loom_driver::git::GitError>,
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
        // Re-init must refresh signing + rerere idempotently: stale host
        // paths must not shadow the current wrix/global gitconfig, and a
        // key provisioned after first init must upgrade the workspace.
        enable_rerere(&dest)?;
        let signing_key = resolve(&dest)?;
        reconcile_signing_config(&dest, signing_key.as_deref())?;
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

    // Enable rerere unconditionally so the driver-side rebase replays
    // recorded conflict resolutions; sync the signing block to the current
    // wrix key, or remove it so the operator's global gitconfig governs.
    enable_rerere(&dest)?;
    let signing_key = resolve(&dest)?;
    reconcile_signing_config(&dest, signing_key.as_deref())?;
    let hooks_path = resolve_hooks(workspace)?;
    write_hooks_config(&dest, &hooks_path)?;

    Ok(Some(MaterializedIntegration {
        path: dest,
        created: true,
    }))
}

/// Enumerate active molecules via `bd list --status=open --type=epic`.
/// Each returned bead's `spec:<label>` label resolves the [`SpecLabel`] for
/// the rebuilt row; beads without a `spec:` label produce
/// [`InitError::MissingSpecLabel`]. For each active bead, `bd show <id>
/// --json` is read for the `loom.base_commit` metadata key.
///
/// `loom plan` sets the key unconditionally on every molecule it creates.
/// Beads created via `bd create` (out-of-band) may inherit `loom.base_commit`
/// from their parent: if the bead lacks the metadata, the parent (via
/// `bd show <parent> --json`) is consulted; a present value is written back
/// to the child via `bd update --set-metadata` and surfaced as the child's
/// base_commit. Beads with neither own metadata nor an inheritable parent
/// produce [`InitError::MoleculeMissingBaseCommit`], whose `Display` includes
/// the `bd update` fix command.
pub async fn fetch_active_molecules<R: CommandRunner>(
    bd: &BdClient<R>,
) -> Result<Vec<ActiveMolecule>, InitError> {
    let beads = bd
        .list(ListOpts {
            status: Some("open".into()),
            issue_type: Some("epic".into()),
            ..ListOpts::default()
        })
        .await?;
    let mut out = Vec::with_capacity(beads.len());
    for bead in beads {
        let spec_label = bead
            .labels
            .iter()
            .find_map(|l| l.spec_label())
            .ok_or_else(|| InitError::MissingSpecLabel {
                id: bead.id.to_string(),
            })?;
        let detail = bd.show(&bead.id).await?;
        let base_commit = resolve_base_commit(bd, &detail).await?;
        out.push(ActiveMolecule {
            id: MoleculeId::new(bead.id.as_str()),
            spec_label,
            base_commit: Some(base_commit),
        });
    }
    Ok(out)
}

/// Read `loom.base_commit` from `detail.metadata`, or inherit from parent.
///
/// When the child lacks the metadata but its parent carries it, write the
/// value back to the child via `bd update --set-metadata` so subsequent
/// reads are self-sufficient, then log the inheritance.
///
/// Shared between the init/rebuild path ([`fetch_active_molecules`]) and the
/// run-phase epic lookup ([`crate::r#loop::production::fetch_molecule_base_commit`])
/// so both surface the same inheritance behaviour the spec mandates.
pub(crate) async fn resolve_base_commit<R: CommandRunner>(
    bd: &BdClient<R>,
    detail: &loom_driver::bd::Bead,
) -> Result<String, InitError> {
    if let Some(v) = detail
        .metadata
        .get("loom.base_commit")
        .and_then(serde_json::Value::as_str)
    {
        return Ok(v.to_owned());
    }
    let parent_id = detail
        .parent
        .as_ref()
        .ok_or_else(|| InitError::MoleculeMissingBaseCommit {
            id: detail.id.to_string(),
        })?;
    let parent = bd.show(parent_id).await?;
    let inherited = parent
        .metadata
        .get("loom.base_commit")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| InitError::MoleculeMissingBaseCommitNoParentMetadata {
            id: detail.id.to_string(),
            parent: parent_id.to_string(),
        })?
        .to_owned();
    bd.update(
        &detail.id,
        UpdateOpts {
            set_metadata: vec![("loom.base_commit".to_string(), inherited.clone())],
            ..UpdateOpts::default()
        },
    )
    .await?;
    info!(
        bead_id = %detail.id,
        parent_id = %parent_id,
        base_commit = %inherited,
        "loom init: inherited `loom.base_commit` from parent molecule",
    );
    Ok(inherited)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Result, anyhow};
    use loom_driver::bd::{BdError, CommandRunner, RunOutput};
    use loom_driver::config::{LoomConfig, Phase};
    use loom_driver::identifier::SpecLabel;
    use loom_driver::lock::LockError;
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Clone, Default)]
    struct CapturingRunner {
        responses: Arc<Mutex<VecDeque<RunOutput>>>,
        calls: Arc<Mutex<Vec<Vec<OsString>>>>,
    }

    impl CapturingRunner {
        fn new(responses: impl IntoIterator<Item = RunOutput>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into_iter().collect())),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|argv| {
                    argv.iter()
                        .map(|a| a.to_string_lossy().into_owned())
                        .collect()
                })
                .collect()
        }
    }

    impl CommandRunner for CapturingRunner {
        async fn run(
            &self,
            args: Vec<OsString>,
            _timeout: Duration,
        ) -> std::result::Result<RunOutput, BdError> {
            self.calls.lock().unwrap().push(args);
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(RunOutput {
                    status: 0,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                }))
        }
    }

    fn ok(stdout: &[u8]) -> RunOutput {
        RunOutput {
            status: 0,
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

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

    fn run_with_hooks(workspace: &Path, hooks: PathBuf) -> Result<InitReport, InitError> {
        run_with_resolvers(
            workspace,
            InitOpts::default(),
            &[],
            |_dir| Ok(None),
            |_workspace| Ok(hooks.clone()),
        )
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

        let report = run_with_hooks(&workspace, hooks)?;
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

        let report = run_with_hooks(&workspace, hooks.clone())?;
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

        let err = run_with_resolvers(
            &workspace,
            InitOpts::default(),
            &[],
            |_dir| Ok(None),
            |_workspace| Err(loom_driver::git::GitError::PrekHooksUnresolved),
        )
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

        let first = run_with_hooks(&workspace, hooks.clone())?
            .integration_workspace
            .ok_or_else(|| anyhow!("first init must materialize integration workspace"))?;
        assert!(first.created, "first init must clone");

        // Sentinel inside the clone proves a second init does not blow it
        // away and re-clone.
        let sentinel = first.path.join("loom-sentinel.txt");
        std::fs::write(&sentinel, b"keep me")?;

        let second = run_with_hooks(&workspace, hooks)?
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

    fn gen_ssh_key(dir: &Path) -> PathBuf {
        let key = dir.join("signing-key");
        let status = std::process::Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-q", "-C", "", "-f"])
            .arg(&key)
            .status()
            .expect("spawn ssh-keygen");
        assert!(status.success(), "ssh-keygen must succeed");
        key
    }

    /// Spec contract `[test]` annotation (`specs/harness.md` § Success
    /// Criteria · Commit signing): `loom init` writes the signing block
    /// (`gpg.format=ssh`, `user.signingkey`, `commit.gpgsign=true`,
    /// `gpg.ssh.allowedSignersFile`) into the loom workspace's local
    /// `.git/config` when a signing key resolves. Driven through the
    /// injectable resolver seam because `$WRIX_SIGNING_KEY` cannot be set
    /// under edition 2024's unsafe `env::set_var`.
    #[test]
    fn loom_init_writes_signing_gitconfig() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let workspace = temp_child_workspace(tmp.path(), "loom-init-signing-ws");
        loom_driver::git::init_test_repo(&workspace)?;
        let key = gen_ssh_key(tmp.path());
        let hooks = fake_prek_hooks(tmp.path())?;

        let report = run_with_resolvers(
            &workspace,
            InitOpts::default(),
            &[],
            |_dir| Ok(Some(key.clone())),
            |_workspace| Ok(hooks.clone()),
        )?;
        let integ = report
            .integration_workspace
            .ok_or_else(|| anyhow!("integration workspace must be materialized"))?;

        let config = std::fs::read_to_string(integ.path.join(".git/config"))?;
        assert!(config.contains("format = ssh"), "gpg.format: {config}");
        assert!(
            config.contains(&format!("signingkey = {}", key.display())),
            "user.signingkey: {config}",
        );
        assert!(
            config.contains("gpgsign = true"),
            "commit.gpgsign: {config}"
        );
        let signers = integ.path.join(".git/loom-allowed-signers");
        assert!(
            config.contains(&format!("allowedSignersFile = {}", signers.display())),
            "gpg.ssh.allowedSignersFile: {config}",
        );
        assert!(
            signers.is_file(),
            "allowed_signers must be derived into the loom workspace: {signers:?}",
        );
        Ok(())
    }

    /// Regression (`criterion:harness:loom_init_writes_signing_gitconfig`):
    /// a re-init over an existing loom workspace must refresh the signing +
    /// rerere block idempotently, so a workspace materialized before the
    /// signing key was provisioned gets upgraded on the next `loom init`
    /// rather than staying stuck on the global gitconfig. The first init
    /// resolves no key (no signing block written); the second resolves the
    /// key and the block must then appear in the existing workspace.
    #[test]
    fn loom_init_reinit_upgrades_signing_gitconfig() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let workspace = temp_child_workspace(tmp.path(), "loom-init-reinit-signing-ws");
        loom_driver::git::init_test_repo(&workspace)?;
        let key = gen_ssh_key(tmp.path());
        let hooks = fake_prek_hooks(tmp.path())?;

        // First init: no key resolves → no signing block.
        let first = run_with_resolvers(
            &workspace,
            InitOpts::default(),
            &[],
            |_dir| Ok(None),
            |_workspace| Ok(hooks.clone()),
        )?
        .integration_workspace
        .ok_or_else(|| anyhow!("first init must materialize integration workspace"))?;
        assert!(first.created, "first init must clone");
        let config = std::fs::read_to_string(first.path.join(".git/config"))?;
        assert!(
            !config.contains("signingkey"),
            "no signing block expected on first init without a key: {config}",
        );

        // Second init over the existing workspace: key now resolves → the
        // reconcile path must write the signing block into the workspace it
        // did NOT re-clone.
        let second = run_with_resolvers(
            &workspace,
            InitOpts::default(),
            &[],
            |_dir| Ok(Some(key.clone())),
            |_workspace| Ok(hooks.clone()),
        )?
        .integration_workspace
        .ok_or_else(|| anyhow!("second init must report the existing workspace"))?;
        assert!(!second.created, "second init must NOT re-clone");
        assert_eq!(second.path, first.path);

        let config = std::fs::read_to_string(second.path.join(".git/config"))?;
        assert!(config.contains("format = ssh"), "gpg.format: {config}");
        assert!(
            config.contains(&format!("signingkey = {}", key.display())),
            "user.signingkey must be written on re-init: {config}",
        );
        assert!(
            config.contains("gpgsign = true"),
            "commit.gpgsign: {config}"
        );
        Ok(())
    }

    #[test]
    fn loom_init_reinit_removes_stale_signing_gitconfig_without_key() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let workspace = temp_child_workspace(tmp.path(), "loom-init-clear-signing-ws");
        loom_driver::git::init_test_repo(&workspace)?;
        let key = gen_ssh_key(tmp.path());
        let hooks = fake_prek_hooks(tmp.path())?;

        let first = run_with_resolvers(
            &workspace,
            InitOpts::default(),
            &[],
            |_dir| Ok(Some(key.clone())),
            |_workspace| Ok(hooks.clone()),
        )?
        .integration_workspace
        .ok_or_else(|| anyhow!("first init must materialize integration workspace"))?;
        let signers = first.path.join(".git/loom-allowed-signers");
        assert!(signers.exists(), "precondition: signing block written");

        let second = run_with_resolvers(
            &workspace,
            InitOpts::default(),
            &[],
            |_dir| Ok(None),
            |_workspace| Ok(hooks.clone()),
        )?
        .integration_workspace
        .ok_or_else(|| anyhow!("second init must report the existing workspace"))?;
        assert!(!second.created, "second init must NOT re-clone");

        let config = std::fs::read_to_string(second.path.join(".git/config"))?;
        assert!(
            !config.contains("signingkey"),
            "stale signing key must be removed when no key resolves: {config}",
        );
        assert!(
            !config.contains("gpgsign"),
            "stale gpgsign must be removed when no key resolves: {config}",
        );
        assert!(!signers.exists(), "stale allowed_signers must be removed");
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

        let report = run_with_hooks(&workspace, hooks)?;
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

    /// Spec contract `[test]` annotation (`specs/harness.md` § Success
    /// Criteria · Commit signing): when no signing key resolves, `loom init`
    /// writes no signing block — the operator's global gitconfig governs.
    /// The test repo's origin is a local bare path (not GitHub), so the
    /// deploy-key fallback is skipped.
    #[test]
    fn no_wrix_keys_leaves_global_gitconfig_governing() -> Result<()> {
        if std::env::var_os("WRIX_SIGNING_KEY").is_some() {
            // Ambient env carries a real signing key; the no-key path
            // cannot be exercised in this environment.
            return Ok(());
        }
        let tmp = tempfile::tempdir()?;
        let workspace = temp_child_workspace(tmp.path(), "loom-init-nokey-ws");
        loom_driver::git::init_test_repo(&workspace)?;
        let hooks = fake_prek_hooks(tmp.path())?;

        let report = run_with_hooks(&workspace, hooks)?;
        let integ = report
            .integration_workspace
            .ok_or_else(|| anyhow!("integration workspace must be materialized"))?;

        let config = std::fs::read_to_string(integ.path.join(".git/config"))?;
        assert!(
            !config.contains("signingkey"),
            "no signing block expected when key unresolved: {config}",
        );
        assert!(
            !config.contains("gpgsign"),
            "no commit.gpgsign expected when key unresolved: {config}",
        );
        assert!(
            !integ.path.join(".git/loom-allowed-signers").exists(),
            "no allowed_signers file expected when key unresolved",
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
            Phase::Msg,
        ] {
            assert_eq!(
                parsed.agent_for(phase).map_err(anyhow::Error::from)?,
                empty.agent_for(phase).map_err(anyhow::Error::from)?,
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
        let molecules = vec![ActiveMolecule {
            id: MoleculeId::new("lm-mol.1"),
            spec_label: SpecLabel::new("alpha"),
            base_commit: None,
        }];
        let db = CacheDb::open(dir.path().join(".loom/cache.db"))?;
        db.rebuild(dir.path(), &molecules)?;
        let post = db.increment_iteration(&MoleculeId::new("lm-mol.1"))?;
        assert_eq!(post, 1);
        drop(db);

        let report = run(
            dir.path(),
            InitOpts { rebuild: true },
            &[ActiveMolecule {
                id: MoleculeId::new("lm-mol.1"),
                spec_label: SpecLabel::new("alpha"),
                base_commit: None,
            }],
        )?;
        let rb = report
            .rebuild
            .ok_or_else(|| anyhow::anyhow!("rebuild must produce a report"))?;
        assert_eq!(rb.specs, 2, "two spec files");
        assert_eq!(rb.work_epics, 1, "one active work epic");

        // Iteration counter reset to 0 after rebuild.
        let db = CacheDb::open(dir.path().join(".loom/cache.db"))?;
        let row = db
            .molecule_for_spec(&SpecLabel::new("alpha"))?
            .ok_or_else(|| anyhow::anyhow!("active molecule must exist"))?;
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
        let probe = SpecLabel::new("probe");

        match db.spec(&probe) {
            Err(loom_driver::state::CacheError::SpecNotFound { .. }) => {}
            other => {
                return Err(anyhow!(
                    "expected SpecNotFound on empty specs table, got {other:?}"
                ));
            }
        }
        assert!(db.molecule_for_spec(&probe)?.is_none());
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
            &[ActiveMolecule {
                id: MoleculeId::new("lm-mol.1"),
                spec_label: SpecLabel::new("alpha"),
                base_commit: Some("deadbeef".into()),
            }],
        )?;
        let bumped = db.increment_iteration(&MoleculeId::new("lm-mol.1"))?;
        assert_eq!(bumped, 1);
        drop(db);

        let report = run(dir.path(), InitOpts::default(), &[])?;
        assert!(report.rebuild.is_none(), "plain init must not run rebuild");
        let db = CacheDb::open(&db_path)?;
        let row = db
            .molecule_for_spec(&SpecLabel::new("alpha"))?
            .ok_or_else(|| anyhow!("molecule row was clobbered"))?;
        assert_eq!(row.id.as_str(), "lm-mol.1");
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

    /// Spec contract `[test]` annotation
    /// (`specs/harness.md` § Success Criteria · Cache DB):
    /// `loom init --rebuild` populates `molecules.base_commit` from
    /// `bd show <id> --json` reading `loom.base_commit` metadata; an
    /// active molecule without the key surfaces as
    /// `InitError::MoleculeMissingBaseCommit`.
    #[tokio::test]
    async fn rebuild_reads_base_commit_from_bead_metadata() -> Result<()> {
        let list_json = br#"[
            {
                "id": "lm-mol.1",
                "title": "loom-harness: pending decomposition",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["spec:harness"]
            }
        ]"#;
        let show_json = br#"[
            {
                "id": "lm-mol.1",
                "title": "loom-harness: pending decomposition",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["spec:harness"],
                "metadata": {"loom.base_commit": "7c226fef"}
            }
        ]"#;
        let runner = CapturingRunner::new([ok(list_json), ok(show_json)]);
        let handle = runner.clone();
        let client = BdClient::with_runner(runner);
        let molecules = fetch_active_molecules(&client).await?;
        assert_eq!(molecules.len(), 1);
        assert_eq!(molecules[0].id.as_str(), "lm-mol.1");
        assert_eq!(molecules[0].spec_label.as_str(), "harness");
        assert_eq!(molecules[0].base_commit.as_deref(), Some("7c226fef"));

        let calls = handle.calls();
        assert_eq!(calls.len(), 2, "expected list+show calls: {calls:?}");
        assert_eq!(calls[0][0], "list");
        assert!(calls[0].contains(&"--type=epic".to_string()));
        assert!(calls[0].contains(&"--status=open".to_string()));
        assert_eq!(calls[1][0], "show");
        assert_eq!(calls[1][1], "lm-mol.1");
        assert!(calls[1].contains(&"--json".to_string()));
        Ok(())
    }

    #[tokio::test]
    async fn rebuild_errors_when_active_molecule_lacks_base_commit_metadata() -> Result<()> {
        let list_json = br#"[
            {
                "id": "lm-mol.2",
                "title": "loom-harness: pending decomposition",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["spec:harness"]
            }
        ]"#;
        let show_json = br#"[
            {
                "id": "lm-mol.2",
                "title": "loom-harness: pending decomposition",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["spec:harness"]
            }
        ]"#;
        let runner = CapturingRunner::new([ok(list_json), ok(show_json)]);
        let client = BdClient::with_runner(runner);
        let err = fetch_active_molecules(&client)
            .await
            .err()
            .ok_or_else(|| anyhow!("expected MoleculeMissingBaseCommit"))?;
        let msg = err.to_string();
        assert!(
            msg.contains("bd update lm-mol.2 --set-metadata loom.base_commit="),
            "error must surface the fix command: {msg}",
        );
        match err {
            InitError::MoleculeMissingBaseCommit { id } => assert_eq!(id, "lm-mol.2"),
            other => return Err(anyhow!("expected MoleculeMissingBaseCommit, got {other:?}")),
        }
        Ok(())
    }

    /// Spec contract `[test]` annotation
    /// (`specs/harness.md` § Success Criteria · Cache DB):
    /// An epic created via `bd create --parent=<epic>` without its own
    /// `loom.base_commit` metadata inherits the value from the parent
    /// epic. `fetch_active_molecules` writes the inherited value back via
    /// `bd update --set-metadata` so subsequent reads are self-sufficient.
    #[tokio::test]
    async fn rebuild_inherits_base_commit_from_parent_when_missing() -> Result<()> {
        let list_json = br#"[
            {
                "id": "lm-child.1",
                "title": "follow-up",
                "status": "open",
                "priority": 2,
                "issue_type": "bug",
                "labels": ["spec:harness"]
            }
        ]"#;
        let child_show = br#"[
            {
                "id": "lm-child.1",
                "title": "follow-up",
                "status": "open",
                "priority": 2,
                "issue_type": "bug",
                "labels": ["spec:harness"],
                "parent": "lm-epic",
                "metadata": {}
            }
        ]"#;
        let parent_show = br#"[
            {
                "id": "lm-epic",
                "title": "loom-harness: pending decomposition",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["spec:harness"],
                "metadata": {"loom.base_commit": "40d21b79"}
            }
        ]"#;
        let runner = CapturingRunner::new([
            ok(list_json),
            ok(child_show),
            ok(parent_show),
            ok(b""), // bd update
        ]);
        let handle = runner.clone();
        let client = BdClient::with_runner(runner);
        let molecules = fetch_active_molecules(&client).await?;

        assert_eq!(molecules.len(), 1);
        assert_eq!(molecules[0].id.as_str(), "lm-child.1");
        assert_eq!(molecules[0].base_commit.as_deref(), Some("40d21b79"));

        let calls = handle.calls();
        assert_eq!(
            calls.len(),
            4,
            "expected list + show(child) + show(parent) + update(child) calls: {calls:?}",
        );
        assert_eq!(calls[1][0], "show");
        assert_eq!(calls[1][1], "lm-child.1");
        assert_eq!(calls[2][0], "show");
        assert_eq!(calls[2][1], "lm-epic");
        assert_eq!(calls[3][0], "update");
        assert_eq!(calls[3][1], "lm-child.1");
        assert!(
            calls[3].contains(&"--set-metadata".to_string()),
            "inherited value must be persisted back to the child: {:?}",
            calls[3],
        );
        assert!(
            calls[3].contains(&"loom.base_commit=40d21b79".to_string()),
            "inherited value must round-trip as the set-metadata pair: {:?}",
            calls[3],
        );
        Ok(())
    }

    #[tokio::test]
    async fn rebuild_errors_when_parent_also_lacks_base_commit_metadata() -> Result<()> {
        let list_json = br#"[
            {
                "id": "lm-child.2",
                "title": "follow-up",
                "status": "open",
                "priority": 2,
                "issue_type": "bug",
                "labels": ["spec:harness"]
            }
        ]"#;
        let child_show = br#"[
            {
                "id": "lm-child.2",
                "title": "follow-up",
                "status": "open",
                "priority": 2,
                "issue_type": "bug",
                "labels": ["spec:harness"],
                "parent": "lm-epic2",
                "metadata": {}
            }
        ]"#;
        let parent_show = br#"[
            {
                "id": "lm-epic2",
                "title": "loom-harness: pending decomposition",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["spec:harness"]
            }
        ]"#;
        let runner = CapturingRunner::new([ok(list_json), ok(child_show), ok(parent_show)]);
        let client = BdClient::with_runner(runner);
        let err = fetch_active_molecules(&client)
            .await
            .err()
            .ok_or_else(|| anyhow!("expected MoleculeMissingBaseCommitNoParentMetadata"))?;
        let msg = err.to_string();
        assert!(
            msg.contains("bd update lm-child.2 --set-metadata loom.base_commit="),
            "error must surface the fix command: {msg}",
        );
        match err {
            InitError::MoleculeMissingBaseCommitNoParentMetadata { id, parent } => {
                assert_eq!(id, "lm-child.2");
                assert_eq!(parent, "lm-epic2");
            }
            other => {
                return Err(anyhow!(
                    "expected MoleculeMissingBaseCommitNoParentMetadata, got {other:?}"
                ));
            }
        }
        Ok(())
    }
}
