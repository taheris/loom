//! `loom init` — workspace bootstrap and optional state-DB rebuild.
//!
//! Acquires the workspace lock (errors immediately if any per-spec lock is
//! held), ensures `<workspace>/config.toml` and `.wrapix/loom/state.db`
//! exist, and — when `--rebuild` is passed — drops/recreates the state DB
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
use loom_driver::identifier::MoleculeId;
use loom_driver::lock::LockManager;
use loom_driver::state::{ActiveMolecule, RebuildReport, StateDb};

pub use error::InitError;

/// Default body for `<workspace>/config.toml`. Mirrors the Configuration
/// section of `specs/harness.md` verbatim so a fresh `loom init` writes
/// a file that round-trips through `LoomConfig::default()`.
pub const DEFAULT_CONFIG_TOML: &str = include_str!("default-config.toml");

/// Options accepted by [`run`].
#[derive(Debug, Clone, Copy, Default)]
pub struct InitOpts {
    /// Drop and repopulate the state DB from on-disk specs + active beads.
    pub rebuild: bool,
}

/// Files touched by [`run`] and (optionally) the rebuild report.
#[derive(Debug, Clone)]
pub struct InitReport {
    pub config_path: PathBuf,
    pub state_db_path: PathBuf,
    pub config_created: bool,
    pub rebuild: Option<RebuildReport>,
}

/// Run `loom init` against `workspace`.
///
/// 1. Acquires the workspace lock — errors immediately with `WorkspaceBusy`
///    if any per-spec `<label>.lock` is held.
/// 2. Creates `<workspace>/.wrapix/loom/` and writes `config.toml` if it
///    does not already exist (existing config files are preserved).
/// 3. Opens `state.db` (creating the schema on first open). When
///    `opts.rebuild` is true, the file is dropped and recreated, and the
///    schema is repopulated from `specs/*.md` plus `molecules`.
pub fn run(
    workspace: &Path,
    opts: InitOpts,
    molecules: &[ActiveMolecule],
) -> Result<InitReport, InitError> {
    let lock_mgr = LockManager::new(workspace)?;
    let _guard = lock_mgr.acquire_workspace()?;

    let runtime_dir = workspace.join(".wrapix/loom");
    fs::create_dir_all(&runtime_dir).map_err(|source| InitError::CreateDir {
        path: runtime_dir.clone(),
        source,
    })?;

    let config_path = LoomConfig::resolve_path(workspace);
    let state_db_path = runtime_dir.join("state.db");

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
        let db = StateDb::recreate(&state_db_path)?;
        Some(db.rebuild(workspace, molecules)?)
    } else {
        let _db = StateDb::open(&state_db_path)?;
        None
    };

    Ok(InitReport {
        config_path,
        state_db_path,
        config_created,
        rebuild: rebuild_report,
    })
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

    #[test]
    fn run_creates_config_and_state_db() -> Result<()> {
        let dir = temp_workspace()?;
        let report = run(dir.path(), InitOpts::default(), &[])?;
        assert!(report.config_created, "first init must write config");
        assert!(
            report.config_path.exists(),
            "config.toml must exist on disk"
        );
        assert!(report.state_db_path.exists(), "state.db must exist on disk");
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
            Phase::Run,
            Phase::Check,
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
        std::fs::write(dir.path().join("config.toml"), custom)?;

        let report = run(dir.path(), InitOpts::default(), &[])?;
        assert!(!report.config_created);
        let body = std::fs::read_to_string(report.config_path)?;
        assert_eq!(body, custom, "existing config must not be overwritten");
        Ok(())
    }

    #[test]
    fn rebuild_drops_and_repopulates_state_db() -> Result<()> {
        let dir = temp_workspace()?;
        let specs = dir.path().join("specs");
        std::fs::create_dir_all(&specs)?;
        std::fs::write(specs.join("alpha.md"), "# alpha\n")?;
        std::fs::write(specs.join("beta.md"), "# beta\n")?;

        // First init seeds the DB and bumps an iteration so we can prove
        // rebuild wiped it.
        run(dir.path(), InitOpts::default(), &[])?;
        let molecules = vec![ActiveMolecule {
            id: MoleculeId::new("wx-mol.1"),
            spec_label: SpecLabel::new("alpha"),
            base_commit: None,
        }];
        let db = StateDb::open(dir.path().join(".wrapix/loom/state.db"))?;
        db.rebuild(dir.path(), &molecules)?;
        let post = db.increment_iteration(&MoleculeId::new("wx-mol.1"))?;
        assert_eq!(post, 1);
        drop(db);

        let report = run(
            dir.path(),
            InitOpts { rebuild: true },
            &[ActiveMolecule {
                id: MoleculeId::new("wx-mol.1"),
                spec_label: SpecLabel::new("alpha"),
                base_commit: None,
            }],
        )?;
        let rb = report
            .rebuild
            .ok_or_else(|| anyhow::anyhow!("rebuild must produce a report"))?;
        assert_eq!(rb.specs, 2, "two spec files");
        assert_eq!(rb.molecules, 1, "one active molecule");

        // Iteration counter reset to 0 after rebuild.
        let db = StateDb::open(dir.path().join(".wrapix/loom/state.db"))?;
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
        let db = StateDb::open(&report.state_db_path)?;
        let probe = SpecLabel::new("probe");

        match db.spec(&probe) {
            Err(loom_driver::state::StateError::SpecNotFound { .. }) => {}
            other => {
                return Err(anyhow!(
                    "expected SpecNotFound on empty specs table, got {other:?}"
                ));
            }
        }
        assert!(db.molecule_for_spec(&probe)?.is_none());
        assert!(db.companions(&probe)?.is_empty());
        assert!(db.current_spec()?.is_none());
        Ok(())
    }

    /// Plain `loom init` (no `--rebuild`) must preserve existing
    /// `current_spec` and molecule rows. The state DB is the source of
    /// truth for the active spec across sessions; clobbering it on every
    /// init would erase that pointer the moment a user (or hook) re-runs
    /// `loom init` to refresh the config.
    #[test]
    fn run_is_idempotent_and_preserves_current_spec_and_molecules() -> Result<()> {
        let dir = temp_workspace()?;
        let specs = dir.path().join("specs");
        std::fs::create_dir_all(&specs)?;
        std::fs::write(specs.join("alpha.md"), "# alpha\n")?;

        run(dir.path(), InitOpts::default(), &[])?;
        let db_path = dir.path().join(".wrapix/loom/state.db");
        let db = StateDb::open(&db_path)?;
        db.rebuild(
            dir.path(),
            &[ActiveMolecule {
                id: MoleculeId::new("wx-mol.1"),
                spec_label: SpecLabel::new("alpha"),
                base_commit: Some("deadbeef".into()),
            }],
        )?;
        db.set_current_spec(&SpecLabel::new("alpha"))?;
        let bumped = db.increment_iteration(&MoleculeId::new("wx-mol.1"))?;
        assert_eq!(bumped, 1);
        drop(db);

        let report = run(dir.path(), InitOpts::default(), &[])?;
        assert!(report.rebuild.is_none(), "plain init must not run rebuild");
        let db = StateDb::open(&db_path)?;
        let current = db
            .current_spec()?
            .ok_or_else(|| anyhow!("current_spec was clobbered"))?;
        assert_eq!(current.as_str(), "alpha");
        let row = db
            .molecule_for_spec(&SpecLabel::new("alpha"))?
            .ok_or_else(|| anyhow!("molecule row was clobbered"))?;
        assert_eq!(row.id.as_str(), "wx-mol.1");
        assert_eq!(
            row.iteration_count, 1,
            "iteration counter must survive a plain init"
        );
        assert_eq!(row.base_commit.as_deref(), Some("deadbeef"));
        Ok(())
    }

    #[test]
    fn workspace_lock_errors_when_spec_lock_held() -> Result<()> {
        let dir = temp_workspace()?;
        // Hold a per-spec lock by acquiring through a separate manager.
        let mgr = LockManager::new(dir.path())?;
        let _spec_guard = mgr.acquire_spec(&SpecLabel::new("alpha"))?;
        match run(dir.path(), InitOpts::default(), &[]) {
            Err(InitError::Lock(LockError::WorkspaceBusy { label })) => {
                assert_eq!(label, "alpha");
                Ok(())
            }
            other => Err(anyhow::anyhow!("expected WorkspaceBusy, got {other:?}")),
        }
    }

    /// Spec contract `[test]` annotation
    /// (`specs/harness.md` § Success Criteria · State DB):
    /// `loom init --rebuild` populates `molecules.base_commit` from
    /// `bd show <id> --json` reading `loom.base_commit` metadata; an
    /// active molecule without the key surfaces as
    /// `InitError::MoleculeMissingBaseCommit`.
    #[tokio::test]
    async fn rebuild_reads_base_commit_from_bead_metadata() -> Result<()> {
        let list_json = br#"[
            {
                "id": "wx-mol.1",
                "title": "loom-harness: pending decomposition",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["spec:harness"]
            }
        ]"#;
        let show_json = br#"[
            {
                "id": "wx-mol.1",
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
        assert_eq!(molecules[0].id.as_str(), "wx-mol.1");
        assert_eq!(molecules[0].spec_label.as_str(), "harness");
        assert_eq!(molecules[0].base_commit.as_deref(), Some("7c226fef"));

        let calls = handle.calls();
        assert_eq!(calls.len(), 2, "expected list+show calls: {calls:?}");
        assert_eq!(calls[0][0], "list");
        assert!(calls[0].contains(&"--type=epic".to_string()));
        assert!(calls[0].contains(&"--status=open".to_string()));
        assert_eq!(calls[1][0], "show");
        assert_eq!(calls[1][1], "wx-mol.1");
        assert!(calls[1].contains(&"--json".to_string()));
        Ok(())
    }

    #[tokio::test]
    async fn rebuild_errors_when_active_molecule_lacks_base_commit_metadata() -> Result<()> {
        let list_json = br#"[
            {
                "id": "wx-mol.2",
                "title": "loom-harness: pending decomposition",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["spec:harness"]
            }
        ]"#;
        let show_json = br#"[
            {
                "id": "wx-mol.2",
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
            msg.contains("bd update wx-mol.2 --set-metadata loom.base_commit="),
            "error must surface the fix command: {msg}",
        );
        match err {
            InitError::MoleculeMissingBaseCommit { id } => assert_eq!(id, "wx-mol.2"),
            other => return Err(anyhow!("expected MoleculeMissingBaseCommit, got {other:?}")),
        }
        Ok(())
    }

    /// Spec contract `[test]` annotation
    /// (`specs/harness.md` § Success Criteria · State DB):
    /// An epic created via `bd create --parent=<epic>` without its own
    /// `loom.base_commit` metadata inherits the value from the parent
    /// epic. `fetch_active_molecules` writes the inherited value back via
    /// `bd update --set-metadata` so subsequent reads are self-sufficient.
    #[tokio::test]
    async fn rebuild_inherits_base_commit_from_parent_when_missing() -> Result<()> {
        let list_json = br#"[
            {
                "id": "wx-child.1",
                "title": "follow-up",
                "status": "open",
                "priority": 2,
                "issue_type": "bug",
                "labels": ["spec:harness"]
            }
        ]"#;
        let child_show = br#"[
            {
                "id": "wx-child.1",
                "title": "follow-up",
                "status": "open",
                "priority": 2,
                "issue_type": "bug",
                "labels": ["spec:harness"],
                "parent": "wx-epic",
                "metadata": {}
            }
        ]"#;
        let parent_show = br#"[
            {
                "id": "wx-epic",
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
        assert_eq!(molecules[0].id.as_str(), "wx-child.1");
        assert_eq!(molecules[0].base_commit.as_deref(), Some("40d21b79"));

        let calls = handle.calls();
        assert_eq!(
            calls.len(),
            4,
            "expected list + show(child) + show(parent) + update(child) calls: {calls:?}",
        );
        assert_eq!(calls[1][0], "show");
        assert_eq!(calls[1][1], "wx-child.1");
        assert_eq!(calls[2][0], "show");
        assert_eq!(calls[2][1], "wx-epic");
        assert_eq!(calls[3][0], "update");
        assert_eq!(calls[3][1], "wx-child.1");
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
                "id": "wx-child.2",
                "title": "follow-up",
                "status": "open",
                "priority": 2,
                "issue_type": "bug",
                "labels": ["spec:harness"]
            }
        ]"#;
        let child_show = br#"[
            {
                "id": "wx-child.2",
                "title": "follow-up",
                "status": "open",
                "priority": 2,
                "issue_type": "bug",
                "labels": ["spec:harness"],
                "parent": "wx-epic2",
                "metadata": {}
            }
        ]"#;
        let parent_show = br#"[
            {
                "id": "wx-epic2",
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
            msg.contains("bd update wx-child.2 --set-metadata loom.base_commit="),
            "error must surface the fix command: {msg}",
        );
        match err {
            InitError::MoleculeMissingBaseCommitNoParentMetadata { id, parent } => {
                assert_eq!(id, "wx-child.2");
                assert_eq!(parent, "wx-epic2");
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
