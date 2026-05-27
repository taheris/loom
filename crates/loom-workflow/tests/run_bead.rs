//! Integration tests for `ProductionAgentLoopController::run_bead`'s
//! per-bead worktree dispatch and verdict-gate tree-not-clean handling.
//!
//! These tests must run against a real git repo so the controller's
//! `create_worktree` / `merge_branch` calls observe a real refs/index
//! state (spec gate from `harness.md` § Worktree Dispatch). The pure
//! marker-routing logic lives in `src/run/production.rs::tests`; this
//! file exercises the worktree-mutation side that needs the `git`
//! binary.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use loom_driver::agent::{SessionOutcome, SpawnConfig};
use loom_driver::bd::{BdClient, Bead, Label};
use loom_driver::git::{GitClient, init_test_repo};
use loom_driver::identifier::{BeadId, ProfileName, SpecLabel};
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_workflow::run::{
    AgentLoopController, AgentOutcome, ProductionAgentLoopController, SessionResult,
};
use loom_workflow::todo::ExitSignal;
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .with_context(|| format!("spawn git {args:?}"))?;
    anyhow::ensure!(status.success(), "git {args:?} exited with {status}");
    Ok(())
}

fn git_capture(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .with_context(|| format!("spawn git {args:?}"))?;
    anyhow::ensure!(
        out.status.success(),
        "git {args:?} exited with {}",
        out.status,
    );
    Ok(String::from_utf8(out.stdout)?)
}

fn write_manifest(dir: &Path) -> Arc<ProfileImageManifest> {
    let body = r#"{
      "base": { "ref": "localhost/wrapix-base:abc", "source": "/nix/store/aaa-image-base" }
    }"#;
    let path = dir.join("profile-images.json");
    std::fs::write(&path, body).expect("write manifest");
    Arc::new(ProfileImageManifest::from_path(&path).expect("parse manifest"))
}

fn fake_bead(id: &str) -> Bead {
    Bead {
        id: BeadId::new(id).expect("valid bead id"),
        title: format!("title-{id}"),
        description: "desc".into(),
        status: "open".into(),
        priority: 2,
        issue_type: "task".into(),
        labels: vec![Label::new("profile:base")],
        parent: None,
        metadata: Default::default(),
        notes: None,
    }
}

/// Set up a tempdir, init a workspace git repo under `ws/`, and write a
/// minimal profile manifest. Returns the tempdir guard, the workspace path,
/// the manifest, and a [`GitClient`] rooted at the workspace.
fn setup() -> (
    TempDir,
    std::path::PathBuf,
    Arc<ProfileImageManifest>,
    GitClient,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = write_manifest(dir.path());
    let workspace = dir.path().join("ws");
    let git = init_test_repo(&workspace).expect("init repo");
    (dir, workspace, manifest, git)
}

/// `loom run --parallel 1` dispatches every bead through a per-bead
/// worktree under `.wrapix/worktree/<label>/<bead-id>/` (universal
/// worktree isolation per `harness.md` § Worktree Dispatch). On clean
/// agent success the controller merges the bead's branch back to the
/// driver branch and removes the worktree + branch.
///
/// Currently ignored: the sequential dispatch path runs against the
/// driver's workdir directly because the wrapix container's
/// `/workspace` bind-mount cannot reach a worktree's host gitdir.
/// Re-enable once SpawnConfig exposes extra mounts (or workers run
/// against a self-contained clone) and worktree dispatch is restored.
#[tokio::test]
#[ignore = "worktree dispatch disabled for sequential path; pending container .git mount"]
async fn run_bead_dispatches_into_per_bead_worktree_and_merges_back_on_success() -> Result<()> {
    let (_dir, workspace, manifest, git_client) = setup();
    let label = SpecLabel::new("harness");
    let bead = fake_bead("wx-1");
    let expected_worktree = workspace.join(".wrapix/worktree/harness/wx-1");

    let observed_workspace: Arc<Mutex<Option<std::path::PathBuf>>> = Arc::new(Mutex::new(None));
    let observed_clone = Arc::clone(&observed_workspace);
    let expected_worktree_clone = expected_worktree.clone();

    let mut controller = ProductionAgentLoopController::new(
        BdClient::new(),
        label.clone(),
        std::path::PathBuf::from("/loom/bin"),
        workspace.clone(),
        git_client,
        manifest,
        None,
        ProfileName::new("base"),
        move |cfg: SpawnConfig, _bead_id: BeadId| {
            let observed = Arc::clone(&observed_clone);
            let expected = expected_worktree_clone.clone();
            async move {
                *observed.lock().unwrap() = Some(cfg.workspace.clone());
                // Worktree must exist when the agent dispatches; the
                // bead "commits" its work so merge-back has a real diff
                // to fold rather than an "Already up to date" no-op.
                assert!(
                    expected.exists(),
                    "worktree {expected:?} must exist at dispatch",
                );
                let work_file = expected.join("from-bead.txt");
                std::fs::write(&work_file, "from-bead\n").expect("write file");
                git(&expected, &["add", "from-bead.txt"]).expect("git add");
                git(&expected, &["commit", "-q", "-m", "bead work"]).expect("git commit");
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            }
        },
    );

    let outcome = controller.run_bead(&bead, None).await.expect("run_bead ok");
    assert_eq!(outcome, AgentOutcome::Success);
    assert_eq!(
        observed_workspace.lock().unwrap().as_deref(),
        Some(expected_worktree.as_path()),
        "agent's SpawnConfig.workspace MUST be the per-bead worktree, not the driver checkout",
    );
    assert!(
        !expected_worktree.exists(),
        "worktree must be removed after clean merge-back",
    );
    let branches = git_capture(&workspace, &["branch", "--list", "loom/harness/wx-1"])?;
    assert!(
        branches.trim().is_empty(),
        "bead's branch must be deleted after merge-back (got: {branches:?})",
    );
    assert!(
        workspace.join("from-bead.txt").exists(),
        "bead's work must land on the driver branch after merge-back",
    );
    Ok(())
}

/// Spec gate (`harness.md` § Verdict Gate · Tree-clean check): a worker
/// that emits `LOOM_COMPLETE` but leaves the worktree dirty
/// (uncommitted / untracked tracked-file edits) routes to `tree-not-clean`
/// recovery. The controller must:
///
/// 1. Return `AgentOutcome::Failure` from the first attempt so the runner
///    enters the retry path.
/// 2. Stash `PreviousFailure::TreeNotClean { dirty_paths }` so the next
///    `run_bead` call threads the typed variant into the rendered prompt
///    (rather than the opaque "tree-not-clean" string the runner sees).
/// 3. Clean up the worktree + branch even though the agent claimed
///    success — the half-staged tree would confuse the next attempt's
///    diff.
///
/// Currently ignored: tree-clean recovery is meaningful only in
/// worktree-dispatch mode (a fresh worktree starts empty so any dirty
/// entry is agent leftover). The sequential path runs against the
/// driver's workdir which has its own pre-existing state; gating
/// re-enables when worktree dispatch is restored.
#[tokio::test]
#[ignore = "tree-clean recovery is worktree-only; sequential dispatch runs against driver workdir"]
async fn run_bead_dirty_tree_stashes_tree_not_clean_and_threads_it_on_retry() -> Result<()> {
    let (_dir, workspace, manifest, git_client) = setup();
    let label = SpecLabel::new("harness");
    let bead = fake_bead("wx-dirty");
    let expected_worktree = workspace.join(".wrapix/worktree/harness/wx-dirty");

    // Capture every prompt the spawn closure sees so we can assert what
    // the controller threaded on the retry.
    let captured_prompts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured_prompts);
    let expected_clone = expected_worktree.clone();

    let mut controller = ProductionAgentLoopController::new(
        BdClient::new(),
        label.clone(),
        std::path::PathBuf::from("/loom/bin"),
        workspace.clone(),
        git_client,
        manifest,
        None,
        ProfileName::new("base"),
        move |cfg: SpawnConfig, _bead_id: BeadId| {
            let captured = Arc::clone(&captured_clone);
            let expected = expected_clone.clone();
            async move {
                let attempt = {
                    let mut g = captured.lock().unwrap();
                    g.push(cfg.initial_prompt.clone());
                    g.len()
                };
                if attempt == 1 {
                    // First attempt: agent claims success but leaves an
                    // untracked file in the worktree — the tree-clean
                    // dispatcher must observe this and reroute.
                    std::fs::write(expected.join("scratch.tmp"), "leftover\n")
                        .expect("write dirty file");
                }
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            }
        },
    );

    // First attempt: dirty tree → Failure with the typed stash queued.
    let first = controller.run_bead(&bead, None).await.expect("run_bead ok");
    match &first {
        AgentOutcome::Failure { error } => assert!(
            error.contains("tree-not-clean"),
            "first-attempt failure body must signal tree-not-clean: {error}",
        ),
        other => panic!("expected Failure, got {other:?}"),
    }
    assert!(
        !expected_worktree.exists(),
        "worktree must be cleaned up after tree-not-clean recovery",
    );

    // Second attempt: the runner threads the opaque error back as the
    // `previous_failure` argument, but the controller must override that
    // with the stashed typed `TreeNotClean` so the rendered prompt
    // surfaces the dirty-path list.
    let second = controller
        .run_bead(&bead, Some("tree-not-clean".to_string()))
        .await
        .expect("run_bead ok");
    assert_eq!(second, AgentOutcome::Success);

    let prompts = captured_prompts.lock().unwrap();
    assert_eq!(prompts.len(), 2, "controller must have dispatched twice");
    assert!(
        !prompts[0].contains("Working tree was not clean"),
        "first attempt's prompt must NOT carry the tree-not-clean framing: {}",
        prompts[0],
    );
    assert!(
        prompts[1].contains("Working tree was not clean after the bead committed"),
        "retry prompt MUST render the typed TreeNotClean framing: {}",
        prompts[1],
    );
    assert!(
        prompts[1].contains("scratch.tmp"),
        "retry prompt MUST enumerate the dirty path observed on the previous attempt: {}",
        prompts[1],
    );
    // The opaque "tree-not-clean" string from the runner must NOT leak
    // into the rendered body — the stashed typed variant takes
    // precedence, so the BuildFailure-from-agent-error framing must be
    // absent.
    assert!(
        !prompts[1].contains("Build failed at agent"),
        "retry prompt must not fall back to the opaque agent-error framing when a typed stash is present: {}",
        prompts[1],
    );
    Ok(())
}
