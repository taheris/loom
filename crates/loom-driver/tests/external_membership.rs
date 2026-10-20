//! Loom production preparation against a status-only external adapter fixture, not Weld acceptance.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use loom_driver::git::{GitClient, init_test_repo_with_integration};
use loom_driver::identifier::{BeadId, SpecLabel};
use loom_driver::lock::LockManager;
use loom_driver::workspace::{Association, external::Membership};
use loom_test_support::workspace_associator::Fixture;

const SELECTION: &str = ".git/loom-workspace-associator.json";

struct Workspace {
    root: tempfile::TempDir,
    operator: PathBuf,
    adapter: Fixture,
    git: GitClient,
    bead: BeadId,
    label: SpecLabel,
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
                "failed membership fixture retained at {}",
                self.root.path().display()
            );
        }
    }
}

impl Workspace {
    fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let operator = root.path().join(format!(
            "operator-{}",
            root.path()
                .file_name()
                .context("fixture name")?
                .to_string_lossy()
        ));
        let git = init_test_repo_with_integration(&operator)?;
        let adapter = Fixture::new(&root.path().join("adapter"))?;
        Fixture::register(&operator, "workspace-a")?;
        let workspace = Self {
            root,
            operator,
            adapter,
            git,
            bead: BeadId::new("lm-member")?,
            label: SpecLabel::new("workspaces")?,
        };
        workspace.configure(Some(&workspace.adapter.program))?;
        Ok(workspace)
    }

    fn configure(&self, program: Option<&Path>) -> Result<()> {
        let manager = LockManager::with_state_home(&self.operator, self.root.path().join("state"))?;
        let guard = manager.acquire_workspace()?;
        Membership::configure(&self.operator, program, &guard)?;
        Ok(())
    }

    fn worker(&self) -> PathBuf {
        self.operator.join(".loom/beads/lm-member")
    }
}

#[tokio::test]
async fn external_membership_admits_fresh_and_reused_clones_without_losing_dirty_work() -> Result<()>
{
    let ws = Workspace::new()?;
    let created = ws.git.create_worktree(&ws.label, &ws.bead).await?;
    assert_eq!(
        fs::read_to_string(created.path.join(".git/fixture-membership"))?,
        "workspace-a"
    );
    assert_eq!(
        fs::read_to_string(ws.git.loom_workspace().join(".git/fixture-membership"))?,
        "workspace-a"
    );
    fs::write(created.path.join("dirty.txt"), "retain this work")?;
    ws.git.create_worktree(&ws.label, &ws.bead).await?;
    assert_eq!(
        fs::read_to_string(created.path.join("dirty.txt"))?,
        "retain this work"
    );
    let preparation = ws.git.prepare_bead_clone(&created.path, &ws.bead).await?;
    assert!(
        preparation.recovery.is_some(),
        "production preparation must preserve dirty work in its stash"
    );
    let mut git = std::process::Command::new("git");
    loom_test_support::scrub_git_local_env(&mut git);
    let recovered = git
        .arg("-C")
        .arg(&created.path)
        .args(["show", "stash@{0}^3:dirty.txt"])
        .output()?;
    assert!(recovered.status.success());
    assert_eq!(recovered.stdout, b"retain this work");
    Ok(())
}

#[tokio::test]
async fn external_membership_refuses_independent_clone_before_configuration_or_stashing()
-> Result<()> {
    let ws = Workspace::new()?;
    let created = ws.git.create_worktree(&ws.label, &ws.bead).await?;
    Fixture::register(&created.path, "independently-registered")?;
    fs::write(created.path.join("dirty.txt"), "keep dirty")?;
    let config = fs::read(created.path.join(".git/config"))?;
    assert!(ws.git.create_worktree(&ws.label, &ws.bead).await.is_err());
    assert!(
        ws.git
            .prepare_bead_clone(&created.path, &ws.bead)
            .await
            .is_err()
    );
    assert_eq!(fs::read(created.path.join(".git/config"))?, config);
    assert_eq!(
        fs::read_to_string(created.path.join("dirty.txt"))?,
        "keep dirty"
    );
    assert!(!created.path.join(".git/refs/stash").exists());
    assert_eq!(
        fs::read_to_string(created.path.join(".git/fixture-membership"))?,
        "independently-registered"
    );
    Ok(())
}

#[tokio::test]
async fn external_membership_rejects_fresh_clone_before_identity_hooks_or_mount_setup() -> Result<()>
{
    let ws = Workspace::new()?;
    ws.adapter.reject(&ws.worker())?;
    assert!(ws.git.create_worktree(&ws.label, &ws.bead).await.is_err());
    let config = fs::read_to_string(ws.worker().join(".git/config"))?;
    assert!(!config.contains("[user]"));
    assert!(!config.contains("hooksPath"));
    assert!(!ws.worker().join(".wrix").exists());
    assert!(
        ws.worker().join(".git/HEAD").exists(),
        "failed checkout remains inspectable"
    );
    fs::remove_file(ws.adapter.root.join("reject"))?;
    ws.git.create_worktree(&ws.label, &ws.bead).await?;
    Ok(())
}

#[tokio::test]
async fn external_membership_malformed_unavailable_and_interrupted_selection_fail_closed()
-> Result<()> {
    let ws = Workspace::new()?;
    for malformed in [
        "{",
        "{\"version\":1}",
        "{\"version\":2,\"program\":null}",
        "{\"version\":1,\"program\":\"relative\"}",
    ] {
        fs::write(ws.operator.join(SELECTION), malformed)?;
        assert!(ws.git.create_worktree(&ws.label, &ws.bead).await.is_err());
        assert!(!ws.worker().exists());
    }
    ws.configure(Some(&ws.adapter.root.join("missing")))?;
    assert!(ws.git.create_worktree(&ws.label, &ws.bead).await.is_err());
    ws.configure(None)?;
    fs::write(
        ws.operator.join(".git/loom-workspace-associator.staging"),
        "interrupted",
    )?;
    assert!(ws.git.create_worktree(&ws.label, &ws.bead).await.is_err());
    assert!(ws.configure(None).is_err());
    assert_eq!(
        fs::read_to_string(ws.operator.join(".git/loom-workspace-associator.staging"))?,
        "interrupted"
    );
    Ok(())
}

#[test]
fn external_membership_selection_refuses_unsafe_objects_and_foreign_lock() -> Result<()> {
    let ws = Workspace::new()?;
    let selection = ws.operator.join(SELECTION);
    fs::set_permissions(&selection, fs::Permissions::from_mode(0o644))?;
    assert!(Membership::load(&ws.operator).is_err());
    fs::set_permissions(&selection, fs::Permissions::from_mode(0o600))?;
    fs::hard_link(&selection, ws.adapter.root.join("copied-selection"))?;
    assert!(Membership::load(&ws.operator).is_err());
    fs::remove_file(ws.adapter.root.join("copied-selection"))?;
    fs::rename(&selection, ws.adapter.root.join("saved-selection"))?;
    symlink(ws.adapter.root.join("saved-selection"), &selection)?;
    assert!(Membership::load(&ws.operator).is_err());
    fs::remove_file(&selection)?;
    let manager = LockManager::with_state_home(&ws.adapter.root, ws.root.path().join("state"))?;
    let guard = manager.acquire_workspace()?;
    assert!(Membership::configure(&ws.operator, Some(&ws.adapter.program), &guard).is_err());
    Ok(())
}

#[tokio::test]
async fn external_membership_checks_controlling_selection_and_rejects_git_replacement() -> Result<()>
{
    let ws = Workspace::new()?;
    assert!(Membership::load(&ws.git.loom_workspace())?.is_some());
    let target = ws.git.create_worktree(&ws.label, &ws.bead).await?.path;
    assert!(Membership::load(&target)?.is_none());
    fs::write(target.join(SELECTION), "worker-controlled selection")?;
    assert!(Membership::load(&target).is_err());
    fs::write(
        ws.adapter.root.join("action"),
        "if [[ \"$checkout\" != \"$operator\" && \"$checkout\" == */beads/* ]]; then mv \"$checkout/.git\" \"$checkout/retained-git\"; mkdir \"$checkout/.git\"; fi\n",
    )?;
    fs::write(target.join("dirty.txt"), "preserve replacement evidence")?;
    assert!(ws.git.create_worktree(&ws.label, &ws.bead).await.is_err());
    assert!(target.join("retained-git/config").exists());
    assert_eq!(
        fs::read_to_string(target.join("dirty.txt"))?,
        "preserve replacement evidence"
    );
    assert!(!target.join(".git/config").exists());
    Ok(())
}

#[tokio::test]
async fn external_membership_moved_pair_prepares_reused_and_fresh_clones_and_invalidates_old_capability()
-> Result<()> {
    let ws = Workspace::new()?;
    ws.git.create_worktree(&ws.label, &ws.bead).await?;
    let old = Association::resolve(&ws.operator)?;
    let membership = Membership::load(&ws.operator)?.context("selected membership")?;
    let database = loom_driver::state::CacheDb::open(loom_driver::state::path(&ws.operator))?;
    database.notes_add(&ws.label, "fixture", "history survives move", 1)?;
    drop(database);
    let moved = ws.root.path().join("moved operator");
    fs::rename(&ws.operator, &moved)?;
    assert!(old.recheck().is_err());
    assert!(membership.admit(&moved).await.is_err());
    let git = GitClient::open(&moved)?;
    let reused = git.create_worktree(&ws.label, &ws.bead).await?;
    git.prepare_bead_clone(&reused.path, &ws.bead).await?;
    let fresh = git
        .create_worktree(&ws.label, &BeadId::new("lm-fresh")?)
        .await?;
    assert_eq!(
        fs::read_to_string(fresh.path.join(".git/fixture-membership"))?,
        "workspace-a"
    );
    let native = Association::resolve(&git.loom_workspace())?;
    assert_eq!(native.operator(), moved);
    let database =
        loom_driver::state::CacheDb::open(loom_driver::state::path(native.integration()))?;
    assert_eq!(database.notes_list(Some(&ws.label), None)?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn external_membership_does_not_participate_in_native_checkout_cleanup() -> Result<()> {
    let ws = Workspace::new()?;
    let target = ws.git.create_worktree(&ws.label, &ws.bead).await?.path;
    let calls = fs::read(ws.adapter.root.join("calls"))?;
    fs::remove_file(&ws.adapter.program)?;
    assert!(ws.git.remove_worktree(&target).await?);
    assert!(!target.exists());
    assert_eq!(fs::read(ws.adapter.root.join("calls"))?, calls);
    Ok(())
}

#[tokio::test]
async fn external_membership_preserves_linked_worktree_refusal_before_target_admission()
-> Result<()> {
    let ws = Workspace::new()?;
    fs::create_dir_all(ws.worker().parent().context("worker parent")?)?;
    let mut git = std::process::Command::new("git");
    loom_test_support::scrub_git_local_env(&mut git);
    let result = git
        .arg("-C")
        .arg(&ws.operator)
        .args(["worktree", "add", "--detach"])
        .arg(ws.worker())
        .arg("HEAD")
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let pointer = fs::read(ws.worker().join(".git"))?;
    fs::write(ws.worker().join("dirty.txt"), "linked dirty work")?;
    assert!(ws.git.create_worktree(&ws.label, &ws.bead).await.is_err());
    assert_eq!(fs::read(ws.worker().join(".git"))?, pointer);
    assert_eq!(
        fs::read_to_string(ws.worker().join("dirty.txt"))?,
        "linked dirty work"
    );
    let calls = fs::read_to_string(ws.adapter.root.join("calls"))?;
    assert!(!calls.contains("/beads/"));
    let mut git = std::process::Command::new("git");
    loom_test_support::scrub_git_local_env(&mut git);
    assert!(
        git.arg("-C")
            .arg(&ws.operator)
            .args(["worktree", "remove", "--force"])
            .arg(ws.worker())
            .status()?
            .success()
    );
    Ok(())
}

#[tokio::test]
async fn external_membership_tune_clone_admission_precedes_checkout_and_identity_configuration()
-> Result<()> {
    let ws = Workspace::new()?;
    let target = ws.operator.join(".loom/tune/lm-tune/repo");
    ws.adapter.reject(&target)?;
    assert!(
        ws.git
            .create_tune_checkout(&target, "HEAD", "loom/tune/lm-tune")
            .await
            .is_err()
    );
    assert!(!fs::read_to_string(target.join(".git/config"))?.contains("Loom Tune"));
    assert!(
        fs::read_dir(&target)?.all(|entry| entry.is_ok_and(|entry| entry.file_name() == ".git"))
    );
    Ok(())
}

#[tokio::test]
async fn external_membership_explicit_disable_restores_standalone_preparation() -> Result<()> {
    let ws = Workspace::new()?;
    ws.configure(None)?;
    fs::remove_file(&ws.adapter.program)?;
    let created = ws.git.create_worktree(&ws.label, &ws.bead).await?;
    ws.git.prepare_bead_clone(&created.path, &ws.bead).await?;
    assert!(!created.path.join(".git/fixture-membership").exists());
    assert!(!ws.adapter.root.join("calls").exists());
    Ok(())
}
