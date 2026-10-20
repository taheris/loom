//! Public CLI initialization acceptance with a fixture adapter, not Weld interoperability.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::Result;
use loom_driver::git::init_test_repo;
use loom_driver::state::CacheDb;
use loom_test_support::workspace_associator::Fixture;

struct Workspace {
    root: tempfile::TempDir,
    operator: PathBuf,
    adapter: Fixture,
    hooks: PathBuf,
}

impl Drop for Workspace {
    #[expect(
        clippy::print_stderr,
        reason = "failed fixtures must report the retained recovery-evidence path"
    )]
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.root.disable_cleanup(true);
            eprintln!(
                "failed CLI membership fixture retained at {}",
                self.root.path().display()
            );
        }
    }
}

impl Workspace {
    fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let operator = root.path().join("operator");
        init_test_repo(&operator)?;
        let adapter = Fixture::new(&root.path().join("adapter"))?;
        let hooks = root.path().join("hooks");
        fs::create_dir(&hooks)?;
        for name in ["pre-commit", "pre-push"] {
            let path = hooks.join(name);
            fs::write(&path, "#!/bin/sh\nexit 0\n")?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            root,
            operator,
            adapter,
            hooks,
        })
    }

    fn init_at(&self, operator: &Path, args: &[&str]) -> Result<Output> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loom"));
        loom_test_support::scrub_git_local_env(&mut command);
        loom_test_support::configure_hermetic_git(&mut command);
        let search_path =
            std::env::join_paths(std::iter::once(self.root.path().to_path_buf()).chain(
                std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
            ))?;
        Ok(command
            .env("PATH", search_path)
            .env_remove("LOOM_INSIDE")
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("WRIX_PREK_HOOKS", &self.hooks)
            .arg("--workspace")
            .arg(operator)
            .arg("init")
            .args(args)
            .output()?)
    }

    fn select(&self) -> Result<Output> {
        self.init_at(
            &self.operator,
            &[
                "--workspace-associator",
                self.adapter
                    .program
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("fixture program is not UTF-8"))?,
            ],
        )
    }
}

fn succeeded(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn external_membership_cli_selected_before_initialization_confirms_operator_and_integration()
-> Result<()> {
    let ws = Workspace::new()?;
    Fixture::register(&ws.operator, "workspace-a")?;
    succeeded(&ws.select()?);
    let integration = ws.operator.join(".loom/integration");
    assert_eq!(
        fs::read_to_string(integration.join(".git/fixture-membership"))?,
        "workspace-a"
    );
    let database = CacheDb::open(loom_driver::state::path(&ws.operator))?;
    assert_eq!(
        database.notes_list(None, None)?,
        [] as [loom_driver::state::NoteRow; 0]
    );
    let calls = fs::read_to_string(ws.adapter.root.join("calls"))?;
    assert_eq!(
        calls.lines().next(),
        Some(format!("{}\t{}", ws.operator.display(), ws.operator.display()).as_str())
    );
    assert_eq!(
        calls.lines().last(),
        Some(format!("{}\t{}", ws.operator.display(), integration.display()).as_str())
    );
    Ok(())
}

#[test]
fn external_membership_cli_unknown_operator_retains_selection_without_initializing_native_state()
-> Result<()> {
    let ws = Workspace::new()?;
    let output = ws.select()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown operator"));
    assert!(
        ws.operator
            .join(".git/loom-workspace-associator.json")
            .exists()
    );
    assert!(!ws.operator.join(".git/fixture-membership").exists());
    assert!(!ws.operator.join(".loom/state.db").exists());
    assert!(!ws.operator.join("loom.toml").exists());
    assert!(
        !ws.init_at(&ws.operator, &[])?.status.success(),
        "failed init cannot silently become standalone"
    );
    Fixture::register(&ws.operator, "workspace-a")?;
    succeeded(&ws.init_at(&ws.operator, &[])?);
    Ok(())
}

#[test]
fn external_membership_cli_selecting_after_initialization_preserves_native_records() -> Result<()> {
    let ws = Workspace::new()?;
    succeeded(&ws.init_at(&ws.operator, &[])?);
    assert!(!ws.adapter.root.join("calls").exists());
    let database = CacheDb::open(loom_driver::state::path(&ws.operator))?;
    let label = loom_driver::identifier::SpecLabel::new("workspaces")?;
    database.notes_add(&label, "fixture", "retained native state", 1)?;
    drop(database);
    Fixture::register(&ws.operator, "workspace-a")?;
    succeeded(&ws.select()?);
    let database = CacheDb::open(loom_driver::state::path(&ws.operator))?;
    assert_eq!(
        database.notes_list(Some(&label), None)?[0].text,
        "retained native state"
    );
    assert_eq!(
        fs::read_to_string(
            ws.operator
                .join(".loom/integration/.git/fixture-membership")
        )?,
        "workspace-a"
    );
    Ok(())
}

#[test]
fn external_membership_cli_rejected_integration_precedes_configuration_and_native_state()
-> Result<()> {
    let ws = Workspace::new()?;
    Fixture::register(&ws.operator, "workspace-a")?;
    let integration = ws.operator.join(".loom/integration");
    ws.adapter.reject(&integration)?;
    assert!(!ws.select()?.status.success());
    assert!(!ws.operator.join(".loom/state.db").exists());
    let config = fs::read_to_string(integration.join(".git/config"))?;
    assert!(!config.contains("rerere"));
    assert!(!config.contains("hooksPath"));
    fs::write(integration.join("dirty.txt"), "preserve rejected work")?;
    Fixture::register(&integration, "independent")?;
    fs::remove_file(ws.adapter.root.join("reject"))?;
    assert!(!ws.init_at(&ws.operator, &[])?.status.success());
    assert_eq!(fs::read_to_string(integration.join(".git/config"))?, config);
    assert_eq!(
        fs::read_to_string(integration.join("dirty.txt"))?,
        "preserve rejected work"
    );
    Ok(())
}

#[test]
fn external_membership_cli_malformed_selection_cannot_be_hidden_by_reinit() -> Result<()> {
    let ws = Workspace::new()?;
    Fixture::register(&ws.operator, "workspace-a")?;
    succeeded(&ws.select()?);
    let config = fs::read(ws.operator.join("loom.toml"))?;
    fs::write(
        ws.operator.join(".git/loom-workspace-associator.json"),
        "malformed",
    )?;
    assert!(!ws.init_at(&ws.operator, &["--rebuild"])?.status.success());
    assert_eq!(fs::read(ws.operator.join("loom.toml"))?, config);
    succeeded(&ws.init_at(&ws.operator, &["--no-workspace-associator"])?);
    fs::remove_file(&ws.adapter.program)?;
    succeeded(&ws.init_at(&ws.operator, &[])?);
    Ok(())
}

#[test]
fn external_membership_cli_moved_operator_tree_retains_initialization_and_membership() -> Result<()>
{
    let ws = Workspace::new()?;
    Fixture::register(&ws.operator, "workspace-a")?;
    succeeded(&ws.select()?);
    let database = CacheDb::open(loom_driver::state::path(&ws.operator))?;
    let label = loom_driver::identifier::SpecLabel::new("workspaces")?;
    database.notes_add(&label, "fixture", "move retained state", 1)?;
    drop(database);
    let moved = ws.root.path().join("moved operator");
    fs::rename(&ws.operator, &moved)?;
    succeeded(&ws.init_at(&moved, &[])?);
    assert_eq!(
        fs::read_to_string(moved.join(".loom/integration/.git/fixture-membership"))?,
        "workspace-a"
    );
    let database = CacheDb::open(loom_driver::state::path(&moved.join(".loom/integration")))?;
    assert_eq!(
        database.notes_list(Some(&label), None)?[0].text,
        "move retained state"
    );
    Ok(())
}

#[test]
fn external_membership_cli_rebuild_tracker_failure_cannot_erase_explicit_selection() -> Result<()> {
    let ws = Workspace::new()?;
    Fixture::register(&ws.operator, "workspace-a")?;
    let bd = ws.root.path().join("bd");
    fs::write(
        &bd,
        "#!/bin/sh\nprintf 'fixture tracker unavailable\\n' >&2\nexit 29\n",
    )?;
    fs::set_permissions(&bd, fs::Permissions::from_mode(0o700))?;
    let output = ws.init_at(
        &ws.operator,
        &[
            "--workspace-associator",
            ws.adapter
                .program
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("fixture path"))?,
            "--rebuild",
        ],
    )?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("fixture tracker unavailable"));
    assert!(
        ws.operator
            .join(".git/loom-workspace-associator.json")
            .exists()
    );
    assert!(!ws.operator.join(".loom/state.db").exists());
    fs::remove_file(&ws.adapter.program)?;
    assert!(!ws.init_at(&ws.operator, &[])?.status.success());
    Ok(())
}

#[test]
fn external_membership_cli_worker_guard_prevents_selection_execution() -> Result<()> {
    let ws = Workspace::new()?;
    Fixture::register(&ws.operator, "workspace-a")?;
    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .env("LOOM_INSIDE", "1")
        .arg("--workspace")
        .arg(&ws.operator)
        .arg("init")
        .arg("--workspace-associator")
        .arg(&ws.adapter.program)
        .output()?;
    assert!(!output.status.success());
    assert!(
        !ws.operator
            .join(".git/loom-workspace-associator.json")
            .exists()
    );
    assert!(!ws.adapter.root.join("calls").exists());
    Ok(())
}
