//! Integration tests for `ProductionAgentLoopController::run_bead`'s
//! per-bead worktree dispatch and verdict-gate tree-not-clean handling.
//!
//! These tests must run against a real git repo so the controller's
//! `create_worktree` / `merge_branch` calls observe a real refs/index
//! state (spec gate from `harness.md` § Worktree Dispatch). The pure
//! marker-routing logic lives in `src/loop/production.rs::tests`; this
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
use loom_driver::logging::{BeadOutcome, LogSink};
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_workflow::r#loop::{
    AgentLoopController, AgentOutcome, ProductionAgentLoopController, SessionResult,
};
use loom_workflow::todo::ExitSignal;
use std::os::unix::fs::PermissionsExt;
use std::time::SystemTime;
use tempfile::TempDir;

/// Write a `beads-push` stub script to `dir` that exits 0 — used to override
/// the controller's default `beads-push`-on-`PATH` so cargo nextest doesn't
/// shell out to the real beads remote while still exercising the
/// post-merge push path. Returns the script's absolute path.
fn beads_push_stub(dir: &Path) -> std::path::PathBuf {
    let stub = dir.join("beads-push-stub.sh");
    std::fs::write(&stub, "#!/bin/sh\nexit 0\n").expect("write stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
    stub
}

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

/// `loom loop --parallel 1` dispatches every bead through a per-bead
/// workspace under `.wrapix/loom/beads/<bead-id>/` (flat — globally-unique
/// bead ids, no spec partition per `harness.md` § Bead dispatch). The
/// workspace is a `git clone --local` of the loom workspace — its `.git/`
/// is a regular directory inside the bind-mounted path, so workers in
/// the wrapix container can commit. On clean agent success the
/// controller pushes the bead branch back to the loom workspace, merges
/// it into the integration branch, and removes the workspace + branch.
#[tokio::test]
async fn run_bead_dispatches_into_per_bead_worktree_and_merges_back_on_success() -> Result<()> {
    let (_dir, workspace, manifest, git_client) = setup();
    let label = SpecLabel::new("harness");
    let bead = fake_bead("lm-1");
    let expected_worktree = workspace.join(".wrapix/loom/beads/lm-1");

    let observed_workspace: Arc<Mutex<Option<std::path::PathBuf>>> = Arc::new(Mutex::new(None));
    let observed_clone = Arc::clone(&observed_workspace);
    let expected_worktree_clone = expected_worktree.clone();

    let stub = beads_push_stub(_dir.path());
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
    )
    .with_beads_push_program(stub);

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
    let branches = git_capture(&workspace, &["branch", "--list", "loom/lm-1"])?;
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
/// that emits `LOOM_COMPLETE` but leaves the per-bead workspace dirty
/// (uncommitted / untracked tracked-file edits) routes to `tree-not-clean`
/// recovery. The controller must:
///
/// 1. Return `AgentOutcome::Failure` from the first attempt so the runner
///    enters the retry path.
/// 2. Stash `PreviousFailure::TreeNotClean { dirty_paths }` so the next
///    `run_bead` call threads the typed variant into the rendered prompt
///    (rather than the opaque "tree-not-clean" string the runner sees).
/// 3. Clean up the workspace even though the agent claimed success — the
///    half-staged tree would confuse the next attempt's diff.
#[tokio::test]
async fn run_bead_dirty_tree_stashes_tree_not_clean_and_threads_it_on_retry() -> Result<()> {
    let (_dir, workspace, manifest, git_client) = setup();
    let label = SpecLabel::new("harness");
    let bead = fake_bead("lm-dirty");
    let expected_worktree = workspace.join(".wrapix/loom/beads/lm-dirty");

    // Capture every prompt the spawn closure sees so we can assert what
    // the controller threaded on the retry.
    let captured_prompts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured_prompts);
    let expected_clone = expected_worktree.clone();
    let stub = beads_push_stub(_dir.path());

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
    )
    .with_beads_push_program(stub);

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

/// Spec gate (`specs/harness.md` § Verdict Gate · Tree-clean check): the
/// empty-starting-tree invariant the verdict gate builds on comes from the
/// pre-attempt `reset_bead_clone`, not from `create_worktree` freshness. A
/// bead workspace that has *already* accumulated leftover scratch (e.g.
/// uncommitted tracked-file edits and untracked top-level files from an
/// earlier dispatch that didn't run cleanup) MUST still surface an empty
/// `git status --porcelain` to the agent — otherwise post-bead dirt cannot
/// be cleanly attributed to the agent vs. a reset-step bug.
#[tokio::test]
async fn run_bead_resets_dirty_bead_workspace_before_dispatch() -> Result<()> {
    let (_dir, workspace, manifest, git_client) = setup();
    let label = SpecLabel::new("harness");
    let bead = fake_bead("lm-resetdispatch");
    let expected_worktree = workspace.join(".wrapix/loom/beads/lm-resetdispatch");

    // Pre-materialize the bead workspace + branch so we can plant dirt
    // *before* `run_bead` is called — modelling the persistence-across-
    // attempts shape (per `harness.md` § Bead dispatch — Per-bead-close
    // lifecycle) without an actual prior attempt. `create_worktree` is
    // idempotent at the directory level, so the controller's subsequent
    // call inside `run_bead` reuses this tree rather than re-cloning.
    let created = git_client.create_worktree(&label, &bead.id).await?;
    assert_eq!(created.path, expected_worktree);

    // Mirror the production `.gitignore` shape so `.wrapix/` doesn't
    // leak into porcelain — production workspaces have it ignored at
    // the repo root and so must the test workspace, otherwise the
    // controller's scratch staging confounds the post-reset assertion.
    std::fs::write(expected_worktree.join(".gitignore"), ".wrapix/\n")?;
    git(&expected_worktree, &["add", ".gitignore"])?;
    git(
        &expected_worktree,
        &["commit", "-q", "-m", "ignore .wrapix/"],
    )?;

    // Plant the two shapes the verdict gate must catch: a tracked-file
    // edit (would otherwise show as ` M README.md`) and an untracked
    // top-level file (would otherwise show as `?? leftover.txt`). If the
    // pre-attempt reset is NOT wired into the dispatch path, the spawn
    // closure below would see both entries in `git status --porcelain`.
    std::fs::write(
        expected_worktree.join("README.md"),
        "stale mid-session edit\n",
    )?;
    std::fs::write(
        expected_worktree.join("leftover.txt"),
        "from a prior attempt\n",
    )?;

    // Sanity: porcelain is dirty *before* dispatch.
    let pre_porcelain = git_capture(&expected_worktree, &["status", "--porcelain"])?;
    assert!(
        !pre_porcelain.trim().is_empty(),
        "test precondition: workspace must be dirty before run_bead so the reset is observable",
    );

    let observed_porcelain: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let observed_clone = Arc::clone(&observed_porcelain);
    let stub = beads_push_stub(_dir.path());
    let mut controller = ProductionAgentLoopController::new(
        BdClient::new(),
        label.clone(),
        std::path::PathBuf::from("/loom/bin"),
        workspace.clone(),
        git_client,
        manifest,
        None,
        ProfileName::new("base"),
        move |cfg: SpawnConfig, bead_id: BeadId| {
            let observed = Arc::clone(&observed_clone);
            async move {
                let porcelain = git_capture(&cfg.workspace, &["status", "--porcelain"])
                    .expect("git status --porcelain at dispatch");
                *observed.lock().unwrap() = Some(porcelain);
                let file = format!("{}.txt", bead_id.as_str());
                std::fs::write(cfg.workspace.join(&file), "work\n").expect("write");
                git(&cfg.workspace, &["add", &file]).expect("git add");
                git(&cfg.workspace, &["commit", "-q", "-m", "bead work"]).expect("git commit");
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            }
        },
    )
    .with_beads_push_program(stub);

    let outcome = controller.run_bead(&bead, None).await?;
    assert_eq!(
        outcome,
        AgentOutcome::Success,
        "agent saw a clean tree and committed cleanly — must succeed",
    );

    let porcelain = observed_porcelain
        .lock()
        .unwrap()
        .clone()
        .expect("spawn closure ran");
    assert!(
        porcelain.trim().is_empty(),
        "post-reset dispatch porcelain MUST be empty — the pre-attempt reset is the source of \
         the empty-starting-tree guarantee, not create_worktree freshness. got: {porcelain:?}",
    );
    Ok(())
}

/// Spec gate (`specs/harness.md` § "loop dispatch: per-bead push regression"):
/// every clean merge MUST push the driver branch to `origin` so per-bead
/// state reaches GitHub before the molecule-end review-phase push fires.
/// The bare origin set up by `init_test_repo` is the proxy: after three
/// beads each commit + merge cleanly, `origin/main` MUST equal `main` —
/// proving the post-merge `git push` ran for every bead.
#[tokio::test]
async fn production_loop_pushes_main_after_each_successful_merge() -> Result<()> {
    let (_dir, workspace, manifest, git_client) = setup();
    let label = SpecLabel::new("harness");
    let stub = beads_push_stub(_dir.path());
    let beads_push_calls = Arc::new(Mutex::new(0u32));

    // Stub that increments a counter on each invocation so we can also
    // verify beads-push fires per bead. Stdout is irrelevant; exit 0
    // signals success.
    let counter_stub = _dir.path().join("beads-push-counter.sh");
    let counter_file = _dir.path().join("beads-push-count");
    std::fs::write(&counter_file, "0")?;
    std::fs::write(
        &counter_stub,
        format!(
            "#!/bin/sh\nset -eu\nn=$(cat {file})\necho $((n+1)) > {file}\nexit 0\n",
            file = counter_file.to_string_lossy(),
        ),
    )?;
    std::fs::set_permissions(&counter_stub, std::fs::Permissions::from_mode(0o755))?;
    let _ = &stub; // silence unused warning when the counter stub takes over
    let _ = beads_push_calls;

    let bead_ids = ["lm-push.1", "lm-push.2", "lm-push.3"];

    let mut controller = ProductionAgentLoopController::new(
        BdClient::new(),
        label.clone(),
        std::path::PathBuf::from("/loom/bin"),
        workspace.clone(),
        git_client,
        manifest,
        None,
        ProfileName::new("base"),
        move |cfg: SpawnConfig, bead_id: BeadId| async move {
            // Commit a unique file inside the bead workspace so the merge
            // back is a real fast-forward and the post-merge push has
            // something to publish.
            let file = format!("{}.txt", bead_id.as_str());
            std::fs::write(cfg.workspace.join(&file), format!("from-{bead_id}\n"))
                .expect("write bead file");
            git(&cfg.workspace, &["add", &file]).expect("git add");
            git(
                &cfg.workspace,
                &["commit", "-q", "-m", &format!("work for {bead_id}")],
            )
            .expect("git commit");
            (
                SessionResult::Complete(SessionOutcome {
                    exit_code: 0,
                    cost_usd: None,
                }),
                Some(ExitSignal::Complete),
            )
        },
    )
    .with_beads_push_program(counter_stub);

    for id in bead_ids {
        let outcome = controller.run_bead(&fake_bead(id), None).await?;
        assert_eq!(
            outcome,
            AgentOutcome::Success,
            "bead {id} must reach Success so push fires",
        );
    }

    // beads-push was invoked once per successful merge.
    let count: u32 = std::fs::read_to_string(&counter_file)?.trim().parse()?;
    assert_eq!(
        count, 3,
        "beads-push must run once per successful merge (got {count})",
    );

    // `git push` published the merged commits — bare origin's main MUST
    // match the workspace's main and contain every bead's file.
    let origin = loom_driver::git::bare_origin_path(&workspace);
    let origin_head = git_capture(&origin, &["rev-parse", "main"])?;
    let workspace_head = git_capture(&workspace, &["rev-parse", "main"])?;
    assert_eq!(
        origin_head.trim(),
        workspace_head.trim(),
        "post-merge push must keep origin/main pinned to workspace HEAD",
    );
    for id in bead_ids {
        let file = format!("{id}.txt");
        let listed = git_capture(&origin, &["ls-tree", "-r", "--name-only", "main"])?;
        assert!(
            listed.lines().any(|l| l == file),
            "origin must carry {file} after per-bead push (tree: {listed})",
        );
    }
    Ok(())
}

/// Spec gate (`specs/harness.md` § "loop dispatch: per-bead push regression"):
/// when `git push` fails (e.g. origin unreachable / non-fast-forward), the
/// controller MUST preserve the bead worktree so a human can investigate
/// the transient blip, and surface `AgentOutcome::Blocked` carrying
/// "push failed: ...". This mirrors the merge-conflict preservation
/// semantics. Routing through `Blocked` (rather than `Failure`) is
/// load-bearing: a retry would invoke `create_worktree` against the
/// still-existing directory and abort the entire `loom loop` with
/// `git clone --local: destination path already exists`.
#[tokio::test]
async fn production_loop_preserves_worktree_on_push_failure() -> Result<()> {
    let (_dir, workspace, manifest, git_client) = setup();
    let label = SpecLabel::new("harness");
    let stub = beads_push_stub(_dir.path());
    let expected_worktree = workspace.join(".wrapix/loom/beads/lm-pushfail.1");

    // Break the origin by repointing `origin` at a nonexistent path so
    // `git push` fails. The repo started healthy (init_test_repo built a
    // bare origin), so the failure is purely on push-time URL resolution.
    git(
        &workspace,
        &[
            "remote",
            "set-url",
            "origin",
            "/nonexistent/path/that/cannot/exist.git",
        ],
    )?;

    let mut controller = ProductionAgentLoopController::new(
        BdClient::new(),
        label.clone(),
        std::path::PathBuf::from("/loom/bin"),
        workspace.clone(),
        git_client,
        manifest,
        None,
        ProfileName::new("base"),
        move |cfg: SpawnConfig, bead_id: BeadId| async move {
            let file = format!("{}.txt", bead_id.as_str());
            std::fs::write(cfg.workspace.join(&file), "work\n").expect("write");
            git(&cfg.workspace, &["add", &file]).expect("git add");
            git(&cfg.workspace, &["commit", "-q", "-m", "bead work"]).expect("git commit");
            (
                SessionResult::Complete(SessionOutcome {
                    exit_code: 0,
                    cost_usd: None,
                }),
                Some(ExitSignal::Complete),
            )
        },
    )
    .with_beads_push_program(stub);

    let outcome = controller
        .run_bead(&fake_bead("lm-pushfail.1"), None)
        .await?;
    match outcome {
        AgentOutcome::Blocked { reason } => {
            assert!(
                reason.contains("push failed:"),
                "blocked reason must signal push failure: {reason}",
            );
        }
        other => panic!(
            "post-merge push failure must route to Blocked (not Failure — the worktree is \
             preserved and a retry would collide with the existing directory): got {other:?}",
        ),
    }
    assert!(
        expected_worktree.exists(),
        "worktree must be preserved on push failure so a human can resolve the blip; got removed at {expected_worktree:?}",
    );
    Ok(())
}

/// Regression: when `merge_branch` returned `MergeResult::Conflict` the
/// run-phase used to emit `AgentOutcome::Failure`. `process_one_bead`
/// routed that through `policy.decide(...)` → `Retry`, and the retry
/// called `create_worktree` against the still-preserved per-bead
/// directory and aborted the whole `loom loop` with
/// `git clone --local: destination path already exists`. The fix routes
/// the conflict through `AgentOutcome::Blocked` instead so the bead is
/// parked under `loom:blocked` and the loop keeps draining other work.
/// The preserved worktree must remain on disk for human resolution.
#[tokio::test]
async fn production_loop_preserves_worktree_on_merge_conflict() -> Result<()> {
    let (_dir, workspace, manifest, git_client) = setup();
    let label = SpecLabel::new("harness");
    let stub = beads_push_stub(_dir.path());
    let expected_worktree = workspace.join(".wrapix/loom/beads/lm-conflict.1");
    let workspace_for_closure = workspace.clone();

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
            let main_ws = workspace_for_closure.clone();
            async move {
                std::fs::write(cfg.workspace.join("README.md"), "bead version\n")
                    .expect("write bead README");
                git(&cfg.workspace, &["commit", "-q", "-am", "bead change"])
                    .expect("git commit in bead workspace");
                std::fs::write(main_ws.join("README.md"), "main version\n")
                    .expect("write main README");
                git(&main_ws, &["commit", "-q", "-am", "main change"])
                    .expect("git commit in main workspace");
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            }
        },
    )
    .with_beads_push_program(stub);

    let outcome = controller
        .run_bead(&fake_bead("lm-conflict.1"), None)
        .await?;
    match outcome {
        AgentOutcome::Blocked { reason } => {
            assert!(
                reason.contains("merge conflict"),
                "blocked reason must signal merge conflict: {reason}",
            );
        }
        other => panic!(
            "merge conflict must route to Blocked (not Failure — the worktree is preserved and \
             a retry would collide with the existing directory): got {other:?}",
        ),
    }
    assert!(
        expected_worktree.exists(),
        "worktree must be preserved on merge conflict so a human can resolve it; got removed at {expected_worktree:?}",
    );
    Ok(())
}

/// Open a per-bead JSONL sink inside the spawn closure so the
/// controller's `find_latest_bead_log` lookup resolves the file the
/// closure just wrote to. Mirrors the wiring the binary's
/// `open_bead_sink_with_renderer` does in production minus the
/// terminal renderer, which is irrelevant for the driver-event
/// channel.
fn open_bead_sink_for_test(logs_root: &Path, label: &SpecLabel, bead_id: &BeadId) -> LogSink {
    LogSink::open_in_at(logs_root, label, bead_id, None, SystemTime::now()).expect("open bead sink")
}

/// Read every JSONL event from the bead's per-attempt log file and
/// return them as parsed JSON values. The sink writes to
/// `<logs_root>/<label>/<bead_id>-<utc>.jsonl`; the helper picks the
/// most-recent file matching that prefix.
fn read_bead_events(
    logs_root: &Path,
    label: &SpecLabel,
    bead_id: &BeadId,
) -> Vec<serde_json::Value> {
    let dir = logs_root.join(label.as_str());
    let prefix = format!("{}-", bead_id.as_str());
    let entries = std::fs::read_dir(&dir).expect("read logs dir");
    let mut best: Option<(std::path::PathBuf, SystemTime)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.starts_with(&prefix) || !name_str.ends_with(".jsonl") {
            continue;
        }
        let mtime = entry.metadata().unwrap().modified().unwrap();
        match &best {
            Some((_, prev)) if mtime <= *prev => continue,
            _ => best = Some((entry.path(), mtime)),
        }
    }
    let path = best.expect("a matching log file exists").0;
    let body = std::fs::read_to_string(&path).expect("read log");
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("parse jsonl line"))
        .collect()
}

/// Filter `events` to driver events whose `driver_kind` matches one
/// of the run-phase merge/push/cleanup kinds and return the wire
/// kind plus the envelope seq for ordering checks.
fn merge_window_events(events: &[serde_json::Value]) -> Vec<(String, u64)> {
    let run_phase_kinds = [
        "bead_branch_pushed",
        "merge_ok",
        "merge_conflict",
        "post_merge_push_ok",
        "post_merge_push_failed",
        "worktree_cleanup_ok",
        "tree_not_clean",
    ];
    events
        .iter()
        .filter(|e| e["kind"] == "driver_event")
        .filter_map(|e| {
            let dk = e["driver_kind"].as_str()?;
            if !run_phase_kinds.contains(&dk) {
                return None;
            }
            let seq = e["seq"].as_u64()?;
            Some((dk.to_string(), seq))
        })
        .collect()
}

/// Happy path: a clean agent session + tree + merge + push must emit
/// `bead_branch_pushed`, `merge_ok`, `post_merge_push_ok`,
/// `worktree_cleanup_ok` exactly once each, in that order, with
/// strictly increasing `seq`. The events must surface in the same
/// per-bead `.jsonl` the spawn closure already wrote to so operators
/// tailing the loop see the dispatch-to-dispatch gap as named steps.
#[tokio::test]
async fn run_bead_emits_driver_events_for_happy_path_in_seq_order() -> Result<()> {
    let (_dir, workspace, manifest, git_client) = setup();
    let label = SpecLabel::new("harness");
    let stub = beads_push_stub(_dir.path());
    let logs_root = workspace.join(".wrapix/loom/logs");
    let logs_root_for_closure = logs_root.clone();
    let label_for_closure = label.clone();

    let mut controller = ProductionAgentLoopController::new(
        BdClient::new(),
        label.clone(),
        std::path::PathBuf::from("/loom/bin"),
        workspace.clone(),
        git_client,
        manifest,
        None,
        ProfileName::new("base"),
        move |cfg: SpawnConfig, bead_id: BeadId| {
            let logs_root = logs_root_for_closure.clone();
            let label = label_for_closure.clone();
            async move {
                let mut sink = open_bead_sink_for_test(&logs_root, &label, &bead_id);
                sink.finish(BeadOutcome::Done).expect("finish sink");
                let file = format!("{}.txt", bead_id.as_str());
                std::fs::write(cfg.workspace.join(&file), "work\n").expect("write");
                git(&cfg.workspace, &["add", &file]).expect("git add");
                git(&cfg.workspace, &["commit", "-q", "-m", "bead work"]).expect("git commit");
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            }
        },
    )
    .with_beads_push_program(stub)
    .with_phase_log_root(logs_root.clone());

    let bead = fake_bead("lm-emithappy");
    let outcome = controller.run_bead(&bead, None).await?;
    assert_eq!(outcome, AgentOutcome::Success);

    let events = read_bead_events(&logs_root, &label, &bead.id);
    let merge_window = merge_window_events(&events);
    let kinds: Vec<String> = merge_window.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(
        kinds,
        vec![
            "bead_branch_pushed",
            "merge_ok",
            "post_merge_push_ok",
            "worktree_cleanup_ok",
        ],
        "happy path must emit the four merge-window driver events in order: {events:?}",
    );
    let seqs: Vec<u64> = merge_window.iter().map(|(_, s)| *s).collect();
    for window in seqs.windows(2) {
        assert!(
            window[0] < window[1],
            "merge-window seq must be strictly increasing: {seqs:?}",
        );
    }
    Ok(())
}

/// Conflict path: when `merge_branch` aborts on conflict, the
/// controller MUST emit a single `merge_conflict` event and NEITHER
/// `merge_ok` NOR `post_merge_push_ok`. Mirrors the existing
/// `production_loop_preserves_worktree_on_merge_conflict` regression
/// — adds the driver-event channel assertion on top.
#[tokio::test]
async fn run_bead_emits_merge_conflict_and_no_merge_ok() -> Result<()> {
    let (_dir, workspace, manifest, git_client) = setup();
    let label = SpecLabel::new("harness");
    let stub = beads_push_stub(_dir.path());
    let logs_root = workspace.join(".wrapix/loom/logs");
    let logs_root_for_closure = logs_root.clone();
    let label_for_closure = label.clone();
    let workspace_for_closure = workspace.clone();

    let mut controller = ProductionAgentLoopController::new(
        BdClient::new(),
        label.clone(),
        std::path::PathBuf::from("/loom/bin"),
        workspace.clone(),
        git_client,
        manifest,
        None,
        ProfileName::new("base"),
        move |cfg: SpawnConfig, bead_id: BeadId| {
            let logs_root = logs_root_for_closure.clone();
            let label = label_for_closure.clone();
            let main_ws = workspace_for_closure.clone();
            async move {
                let mut sink = open_bead_sink_for_test(&logs_root, &label, &bead_id);
                sink.finish(BeadOutcome::Done).expect("finish sink");
                std::fs::write(cfg.workspace.join("README.md"), "bead version\n")
                    .expect("write bead README");
                git(&cfg.workspace, &["commit", "-q", "-am", "bead change"])
                    .expect("git commit in bead workspace");
                std::fs::write(main_ws.join("README.md"), "main version\n")
                    .expect("write main README");
                git(&main_ws, &["commit", "-q", "-am", "main change"])
                    .expect("git commit in main workspace");
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            }
        },
    )
    .with_beads_push_program(stub)
    .with_phase_log_root(logs_root.clone());

    let bead = fake_bead("lm-emitconflict");
    let outcome = controller.run_bead(&bead, None).await?;
    assert!(matches!(outcome, AgentOutcome::Blocked { .. }));

    let events = read_bead_events(&logs_root, &label, &bead.id);
    let merge_window = merge_window_events(&events);
    let kinds: Vec<&str> = merge_window.iter().map(|(k, _)| k.as_str()).collect();
    assert!(
        kinds.contains(&"merge_conflict"),
        "conflict path MUST emit merge_conflict: {events:?}",
    );
    assert!(
        !kinds.contains(&"merge_ok"),
        "conflict path MUST NOT emit merge_ok: {events:?}",
    );
    assert!(
        !kinds.contains(&"post_merge_push_ok"),
        "conflict path MUST NOT emit post_merge_push_ok: {events:?}",
    );
    Ok(())
}

/// Push-failure path: when `git push` to GitHub fails, the controller
/// MUST emit `merge_ok` (the local merge succeeded) followed by
/// `post_merge_push_failed`, and MUST NOT emit `worktree_cleanup_ok`
/// (the worktree is preserved for retry).
#[tokio::test]
async fn run_bead_emits_post_merge_push_failed_after_merge_ok_on_push_failure() -> Result<()> {
    let (_dir, workspace, manifest, git_client) = setup();
    let label = SpecLabel::new("harness");
    let stub = beads_push_stub(_dir.path());
    let logs_root = workspace.join(".wrapix/loom/logs");
    let logs_root_for_closure = logs_root.clone();
    let label_for_closure = label.clone();

    git(
        &workspace,
        &[
            "remote",
            "set-url",
            "origin",
            "/nonexistent/path/that/cannot/exist.git",
        ],
    )?;

    let mut controller = ProductionAgentLoopController::new(
        BdClient::new(),
        label.clone(),
        std::path::PathBuf::from("/loom/bin"),
        workspace.clone(),
        git_client,
        manifest,
        None,
        ProfileName::new("base"),
        move |cfg: SpawnConfig, bead_id: BeadId| {
            let logs_root = logs_root_for_closure.clone();
            let label = label_for_closure.clone();
            async move {
                let mut sink = open_bead_sink_for_test(&logs_root, &label, &bead_id);
                sink.finish(BeadOutcome::Done).expect("finish sink");
                let file = format!("{}.txt", bead_id.as_str());
                std::fs::write(cfg.workspace.join(&file), "work\n").expect("write");
                git(&cfg.workspace, &["add", &file]).expect("git add");
                git(&cfg.workspace, &["commit", "-q", "-m", "bead work"]).expect("git commit");
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            }
        },
    )
    .with_beads_push_program(stub)
    .with_phase_log_root(logs_root.clone());

    let bead = fake_bead("lm-emitpushfail");
    let outcome = controller.run_bead(&bead, None).await?;
    assert!(matches!(outcome, AgentOutcome::Blocked { .. }));

    let events = read_bead_events(&logs_root, &label, &bead.id);
    let merge_window = merge_window_events(&events);
    let kinds: Vec<&str> = merge_window.iter().map(|(k, _)| k.as_str()).collect();
    let merge_ok_idx = kinds.iter().position(|k| *k == "merge_ok");
    let push_failed_idx = kinds.iter().position(|k| *k == "post_merge_push_failed");
    assert!(
        merge_ok_idx.is_some() && push_failed_idx.is_some(),
        "push-failure path MUST emit both merge_ok and post_merge_push_failed: {events:?}",
    );
    assert!(
        merge_ok_idx.unwrap() < push_failed_idx.unwrap(),
        "merge_ok MUST precede post_merge_push_failed: {kinds:?}",
    );
    assert!(
        !kinds.contains(&"worktree_cleanup_ok"),
        "push-failure path MUST NOT emit worktree_cleanup_ok: {events:?}",
    );
    Ok(())
}

/// Tree-not-clean path: when the agent leaves untracked tracked-file
/// edits in the workspace, the controller MUST emit `tree_not_clean`
/// carrying the dirty path list, and MUST NOT emit any of the merge
/// / push / cleanup events (the merge-back never fires).
#[tokio::test]
async fn run_bead_emits_tree_not_clean_when_porcelain_is_dirty() -> Result<()> {
    let (_dir, workspace, manifest, git_client) = setup();
    let label = SpecLabel::new("harness");
    let stub = beads_push_stub(_dir.path());
    let logs_root = workspace.join(".wrapix/loom/logs");
    let logs_root_for_closure = logs_root.clone();
    let label_for_closure = label.clone();
    let expected_worktree = workspace.join(".wrapix/loom/beads/lm-emitdirty");

    let mut controller = ProductionAgentLoopController::new(
        BdClient::new(),
        label.clone(),
        std::path::PathBuf::from("/loom/bin"),
        workspace.clone(),
        git_client,
        manifest,
        None,
        ProfileName::new("base"),
        move |_cfg: SpawnConfig, bead_id: BeadId| {
            let logs_root = logs_root_for_closure.clone();
            let label = label_for_closure.clone();
            let expected = expected_worktree.clone();
            async move {
                let mut sink = open_bead_sink_for_test(&logs_root, &label, &bead_id);
                sink.finish(BeadOutcome::Done).expect("finish sink");
                std::fs::write(expected.join("scratch.tmp"), "leftover\n")
                    .expect("write dirty file");
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            }
        },
    )
    .with_beads_push_program(stub)
    .with_phase_log_root(logs_root.clone());

    let bead = fake_bead("lm-emitdirty");
    let outcome = controller.run_bead(&bead, None).await?;
    assert!(matches!(outcome, AgentOutcome::Failure { .. }));

    let events = read_bead_events(&logs_root, &label, &bead.id);
    let merge_window = merge_window_events(&events);
    let kinds: Vec<&str> = merge_window.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        kinds,
        vec!["tree_not_clean"],
        "tree-not-clean path emits only tree_not_clean from the merge-window kinds: {events:?}",
    );
    let tnc = events
        .iter()
        .find(|e| e["driver_kind"] == "tree_not_clean")
        .expect("tree_not_clean event present");
    let dirty_paths = tnc["payload"]["dirty_paths"]
        .as_array()
        .expect("dirty_paths array");
    assert!(
        dirty_paths
            .iter()
            .any(|p| p.as_str().is_some_and(|s| s.contains("scratch.tmp"))),
        "tree_not_clean payload must enumerate the dirty path: {tnc}",
    );
    Ok(())
}
