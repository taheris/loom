//! Integration tests for `loom_driver::git::GitClient`.
//!
//! Each test builds a throwaway repo in a `tempdir` via the system `git`
//! binary, opens it through the typed client, and asserts the documented
//! behaviour for create/remove worktree and merge-back.
//!
//! These tests spawn the system `git` binary instead of an in-process
//! `LineParse + tokio::io::duplex` substitute (spec NFR #8): `GitClient`'s
//! contract is precisely the typed wrapper around git's on-disk and
//! ref-database state — branches, worktrees, merge results, and conflict
//! detection are observable only through real refs, real index files, and
//! real merge-resolution machinery. A duplex-pipe stand-in would skip the
//! state mutations the tests exist to pin.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use loom_driver::bd::{BdClient, BdError, CommandRunner, RunOutput};
use loom_driver::git::{
    FastForwardOutcome, GitClient, GitError, MergeResult, RebaseOutcome, SignatureCheck,
    StatusKind, write_signing_config,
};
use loom_driver::identifier::{BeadId, MoleculeId, SpecLabel};
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

fn init_repo() -> Result<TempDir> {
    let dir = tempfile::tempdir()?;
    loom_driver::git::init_test_repo_with_integration(dir.path())?;
    Ok(dir)
}

/// Loom-integration-workspace path under a repo created by [`init_repo`].
fn loom_path(repo: &Path) -> std::path::PathBuf {
    repo.join(".loom/integration")
}

#[tokio::test]
async fn create_and_remove_worktree_round_trip() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;

    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-3hhwq.6")?;
    let created = client.create_worktree(&label, &bead).await?;

    assert!(
        created.path.exists(),
        "workspace path {:?} should exist on disk",
        created.path
    );
    assert_eq!(created.branch, "loom/lm-3hhwq.6");
    assert!(
        created.path.ends_with(".loom/beads/lm-3hhwq.6"),
        "workspace path should end with .loom/beads/<bead-id>: {:?}",
        created.path
    );
    // Path A (specs/harness.md § Bead dispatch): the bead workspace is
    // a self-contained `git clone --local`, so `.git/` is a regular
    // directory inside the bind-mounted path (not a `.git` *file* pointing
    // at a host-absolute gitdir, which is what `git worktree add` produces).
    assert!(
        created.path.join(".git").is_dir(),
        ".git inside the bead workspace must be a regular directory \
         (clone), not a worktree pointer file: got {:?}",
        created.path.join(".git"),
    );

    client.remove_worktree(&created.path).await?;
    assert!(
        !created.path.exists(),
        "workspace path {:?} should be cleaned up",
        created.path
    );

    // remove_worktree is idempotent — a second call on a missing path is
    // not an error (the spec calls this out for the cleanup path).
    client.remove_worktree(&created.path).await?;

    Ok(())
}

#[tokio::test]
async fn merge_branch_clean_returns_ok() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());

    git(&loom, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(loom.join("feature.txt"), "added on feature\n")?;
    git(&loom, &["add", "feature.txt"])?;
    git(&loom, &["commit", "-q", "-m", "feature commit"])?;
    git(&loom, &["checkout", "-q", "main"])?;

    let client = GitClient::open(repo.path())?;
    let result = client.merge_branch("feature").await?;

    assert_eq!(result, MergeResult::Ok);
    assert!(
        loom.join("feature.txt").exists(),
        "merge_branch must land changes in the loom workspace",
    );
    Ok(())
}

#[tokio::test]
async fn merge_branch_non_conflicting_returns_ok() -> Result<()> {
    // Both branches diverge after a shared base — feature adds feature.txt,
    // main adds main.txt. Neither touches the other's file, so merge_branch
    // rebases feature onto main's HEAD then fast-forwards. Both files land
    // on the integration branch with linear history.
    let repo = init_repo()?;
    let loom = loom_path(repo.path());

    git(&loom, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(loom.join("feature.txt"), "feature side\n")?;
    git(&loom, &["add", "feature.txt"])?;
    git(&loom, &["commit", "-q", "-m", "feature commit"])?;

    git(&loom, &["checkout", "-q", "main"])?;
    std::fs::write(loom.join("main.txt"), "main side\n")?;
    git(&loom, &["add", "main.txt"])?;
    git(&loom, &["commit", "-q", "-m", "main commit"])?;

    let client = GitClient::open(repo.path())?;
    let result = client.merge_branch("feature").await?;

    assert_eq!(result, MergeResult::Ok);
    assert!(loom.join("feature.txt").exists());
    assert!(loom.join("main.txt").exists());

    let on_main = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&loom)
            .args(["symbolic-ref", "--short", "HEAD"])
            .output()?
            .stdout,
    )?
    .trim()
    .to_string();
    assert_eq!(
        on_main, "main",
        "merge_branch must leave the loom workspace on the integration branch",
    );
    Ok(())
}

/// merge_branch rejects creating non-fast-forward history: when the bead
/// branch and the integration branch have both moved beyond their shared
/// base with non-overlapping changes, the resulting `HEAD` in the loom
/// workspace contains no merge commits — the rebase + `--ff-only` path
/// replaces what `--no-ff` would have produced as a merge commit.
#[tokio::test]
async fn merge_branch_uses_ff_only_and_rejects_non_ff_history() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());
    let base = capture_head(&loom)?;

    git(&loom, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(loom.join("feature.txt"), "feature side\n")?;
    git(&loom, &["add", "feature.txt"])?;
    git(&loom, &["commit", "-q", "-m", "feature commit"])?;

    git(&loom, &["checkout", "-q", "main"])?;
    std::fs::write(loom.join("main.txt"), "main side\n")?;
    git(&loom, &["add", "main.txt"])?;
    git(&loom, &["commit", "-q", "-m", "main commit"])?;

    let client = GitClient::open(repo.path())?;
    let result = client.merge_branch("feature").await?;

    assert_eq!(result, MergeResult::Ok);

    let merges = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&loom)
            .args(["log", "--merges", "--format=%H", &format!("{base}..HEAD")])
            .output()?
            .stdout,
    )?;
    assert!(
        merges.trim().is_empty(),
        "merge_branch must produce linear history (no merge commits) — got: {merges:?}",
    );
    Ok(())
}

/// merge_branch rebases the bead branch onto `HEAD` before fast-forwarding —
/// the parallel-dispatch case where an earlier bead has already landed on
/// the integration branch and a second bead, forked from the original
/// base, must land on the moved `HEAD` of the loom workspace.
#[tokio::test]
async fn merge_branch_rebases_bead_branch_onto_head_before_ff() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());
    let base = capture_head(&loom)?;

    git(&loom, &["checkout", "-q", "-b", "bead-a", &base])?;
    std::fs::write(loom.join("bead-a.txt"), "bead a\n")?;
    git(&loom, &["add", "bead-a.txt"])?;
    git(&loom, &["commit", "-q", "-m", "bead a commit"])?;

    git(&loom, &["checkout", "-q", "-b", "bead-b", &base])?;
    std::fs::write(loom.join("bead-b.txt"), "bead b\n")?;
    git(&loom, &["add", "bead-b.txt"])?;
    git(&loom, &["commit", "-q", "-m", "bead b commit"])?;

    git(&loom, &["checkout", "-q", "main"])?;
    let client = GitClient::open(repo.path())?;
    assert_eq!(client.merge_branch("bead-a").await?, MergeResult::Ok);

    let bead_b_pre = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&loom)
            .args(["rev-parse", "bead-b"])
            .output()?
            .stdout,
    )?
    .trim()
    .to_string();

    assert_eq!(client.merge_branch("bead-b").await?, MergeResult::Ok);

    let merges = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&loom)
            .args(["log", "--merges", "--format=%H", &format!("{base}..HEAD")])
            .output()?
            .stdout,
    )?;
    assert!(
        merges.trim().is_empty(),
        "linear history required after both merges — got: {merges:?}",
    );

    let bead_b_post = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&loom)
            .args(["rev-parse", "bead-b"])
            .output()?
            .stdout,
    )?
    .trim()
    .to_string();
    assert_ne!(
        bead_b_pre, bead_b_post,
        "rebase must rewrite bead-b's commits onto the new HEAD",
    );

    assert!(loom.join("bead-a.txt").exists());
    assert!(loom.join("bead-b.txt").exists());
    Ok(())
}

#[tokio::test]
async fn merge_branch_conflict_is_reported() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());

    git(&loom, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(loom.join("README.md"), "feature line\n")?;
    git(&loom, &["commit", "-q", "-am", "feature edit"])?;

    git(&loom, &["checkout", "-q", "main"])?;
    std::fs::write(loom.join("README.md"), "main line\n")?;
    git(&loom, &["commit", "-q", "-am", "main edit"])?;

    let client = GitClient::open(repo.path())?;
    let result = client.merge_branch("feature").await?;

    assert!(
        matches!(result, MergeResult::Conflict { .. }),
        "expected Conflict, got {result:?}",
    );
    Ok(())
}

/// rerere replay carries the driver-side rebase to completion. With
/// `rerere.enabled` + `rerere.autoupdate` (written by `loom init`), a
/// previously-recorded conflict resolution is auto-staged when the same
/// conflict recurs — but the rebase still pauses awaiting `--continue`.
/// `merge_branch` must drive that `--continue` to completion and return
/// `Ok` rather than aborting on the paused status, so the recorded
/// resolution is not dead weight (specs/harness.md § Verdict Gate
/// phase 3, § Commit signing — rerere configuration).
#[tokio::test]
async fn merge_branch_replays_recorded_rerere_resolution() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());

    // rerere config mirrors what `loom init` writes into the loom
    // workspace's local gitconfig.
    git(&loom, &["config", "rerere.enabled", "true"])?;
    git(&loom, &["config", "rerere.autoupdate", "true"])?;

    // A conflicting pair: feature and main both edit README.md.
    git(&loom, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(loom.join("README.md"), "feature line\n")?;
    git(&loom, &["commit", "-q", "-am", "feature edit"])?;
    let feature_base = capture_head(&loom)?;

    git(&loom, &["checkout", "-q", "main"])?;
    std::fs::write(loom.join("README.md"), "main line\n")?;
    git(&loom, &["commit", "-q", "-am", "main edit"])?;

    // Round 1 — record a resolution. `git rebase` conflicts; resolve to a
    // distinctive merged value, stage it, and `--continue` (with a no-op
    // editor so the reused commit message needs no interaction). rerere
    // records the resolution keyed by the conflict's preimage.
    let conflicting_rebase = Command::new("git")
        .arg("-C")
        .arg(&loom)
        .args(["rebase", "main", "feature"])
        .status()?;
    assert!(
        !conflicting_rebase.success(),
        "round-1 rebase should conflict so rerere can record a resolution",
    );
    std::fs::write(loom.join("README.md"), "resolved line\n")?;
    git(&loom, &["add", "README.md"])?;
    git(&loom, &["-c", "core.editor=true", "rebase", "--continue"])?;

    // Round 2 — reproduce the identical conflict. Restore feature to its
    // pre-rebase commit (from a clean checkout so the ref move is allowed)
    // and let `merge_branch` rebase it onto main again.
    git(&loom, &["checkout", "-q", "main"])?;
    git(&loom, &["branch", "-f", "feature", &feature_base])?;

    let client = GitClient::open(repo.path())?;
    let result = client.merge_branch("feature").await?;

    assert_eq!(
        result,
        MergeResult::Ok,
        "rerere should replay the recorded resolution and the rebase complete",
    );
    assert_eq!(
        std::fs::read_to_string(loom.join("README.md"))?,
        "resolved line\n",
        "the replayed resolution must land on the integration branch",
    );
    Ok(())
}

/// `rebase_onto_integration` rewrites the bead branch onto the
/// integration tip but does NOT advance the integration branch — the
/// fast-forward is the separate `ff_merge_integration` step. This is the
/// ordering the verdict gate relies on: pass-2 signature verification
/// runs on the rewritten `<integration>..<branch>` commits between the
/// two, so a pass-2 failure leaves the integration line untouched
/// (specs/harness.md § Bead dispatch — Bead branch flow).
#[tokio::test]
async fn rebase_onto_integration_leaves_integration_branch_unmoved() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());

    git(&loom, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(loom.join("feature.txt"), "feature side\n")?;
    git(&loom, &["add", "feature.txt"])?;
    git(&loom, &["commit", "-q", "-m", "feature commit"])?;

    git(&loom, &["checkout", "-q", "main"])?;
    std::fs::write(loom.join("main.txt"), "main side\n")?;
    git(&loom, &["add", "main.txt"])?;
    git(&loom, &["commit", "-q", "-m", "main commit"])?;
    let main_tip = capture_rev(&loom, "main")?;

    let client = GitClient::open(repo.path())?;
    let outcome = client.rebase_onto_integration("feature").await?;
    assert!(
        matches!(outcome, RebaseOutcome::Rebased),
        "expected Rebased, got {outcome:?}",
    );

    // The integration branch must not have moved — only the ff-merge
    // advances it.
    assert_eq!(
        capture_rev(&loom, "main")?,
        main_tip,
        "rebase_onto_integration must not advance the integration branch",
    );
    // The rewritten commit is observable as `main..feature`, the pass-2
    // verification range.
    let rewritten = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&loom)
            .args(["rev-list", "--count", "main..feature"])
            .output()?
            .stdout,
    )?
    .trim()
    .to_string();
    assert_eq!(rewritten, "1", "one rewritten commit in the pass-2 range");

    // The follow-up ff-merge advances the integration branch to the
    // rebased tip and lands the work.
    client.ff_merge_integration("feature").await?;
    assert_ne!(
        capture_rev(&loom, "main")?,
        main_tip,
        "ff_merge_integration must advance the integration branch",
    );
    assert!(loom.join("feature.txt").exists());
    assert!(loom.join("main.txt").exists());
    Ok(())
}

/// `integration_commit_sha` reads the integration branch tip in the loom
/// workspace and tracks it as commits land — the signal the per-bead
/// audit uses to tell whether a bead's ff actually advanced the line.
#[tokio::test]
async fn integration_commit_sha_tracks_the_integration_branch_tip() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());
    let client = GitClient::open(repo.path())?;

    let before = client.integration_commit_sha().await?;
    assert_eq!(
        before.to_string(),
        capture_rev(&loom, "main")?,
        "integration_commit_sha must read the loom-workspace integration tip",
    );

    std::fs::write(loom.join("advance.txt"), "more\n")?;
    git(&loom, &["add", "advance.txt"])?;
    git(&loom, &["commit", "-q", "-m", "advance"])?;
    let after = client.integration_commit_sha().await?;
    assert_ne!(
        after.to_string(),
        before.to_string(),
        "integration_commit_sha must follow the branch as commits land",
    );
    Ok(())
}

/// A per-bead audit failure rolls the just-ff'd commit back one
/// (`git reset --hard HEAD~1`) so the integration line returns to its
/// pre-merge tip and the merged content is gone, before the bead routes
/// to `post-integrate-fail` recovery (specs/harness.md § Verdict Gate).
#[tokio::test]
async fn rollback_integration_resets_integration_branch_by_one_commit() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());
    let base_tip = capture_rev(&loom, "main")?;

    std::fs::write(loom.join("integrated.txt"), "merged work\n")?;
    git(&loom, &["add", "integrated.txt"])?;
    git(&loom, &["commit", "-q", "-m", "integrated bead"])?;
    assert_ne!(
        capture_rev(&loom, "main")?,
        base_tip,
        "the ff-merge stand-in must advance the integration branch",
    );

    let client = GitClient::open(repo.path())?;
    client.rollback_integration().await?;

    assert_eq!(
        capture_rev(&loom, "main")?,
        base_tip,
        "rollback must return the integration branch to its pre-merge tip",
    );
    assert!(
        !loom.join("integrated.txt").exists(),
        "reset --hard must drop the rolled-back commit's tree content",
    );
    Ok(())
}

/// Cross-spec `loom loop` invocations share one loom workspace; the
/// rebase + ff critical section is serialized by git's `index.lock`
/// (specs/harness.md § Concurrency). When a peer holds the lock the losing
/// `rebase_onto_integration` must treat git's "Unable to create
/// '…/index.lock': File exists" refusal as *recoverable contention* — retry
/// from its current view of the integration tip — not as a spurious
/// conflict. Modelled deterministically: a pre-created `index.lock` stands
/// in for a peer's held lock, and a concurrent task releases it mid-flight
/// so the retry loop observes the lock clear and the rebase lands.
#[tokio::test]
async fn rebase_onto_integration_retries_through_index_lock_contention() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());

    git(&loom, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(loom.join("feature.txt"), "feature side\n")?;
    git(&loom, &["add", "feature.txt"])?;
    git(&loom, &["commit", "-q", "-m", "feature commit"])?;
    git(&loom, &["checkout", "-q", "main"])?;
    std::fs::write(loom.join("main.txt"), "main side\n")?;
    git(&loom, &["add", "main.txt"])?;
    git(&loom, &["commit", "-q", "-m", "main commit"])?;

    // Stand in for a peer mid-rebase holding the workspace index lock. The
    // loom workspace is a clone, so `.git` is a regular directory.
    let lock = loom.join(".git/index.lock");
    std::fs::write(&lock, b"")?;

    // Release the lock shortly after the rebase starts contending so the
    // bounded retry loop observes it clear and proceeds.
    let lock_clone = lock.clone();
    let releaser = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(120)).await;
        std::fs::remove_file(&lock_clone)
    });

    let client = GitClient::open(repo.path())?;
    let outcome = client.rebase_onto_integration("feature").await?;
    releaser.await?.context("release stand-in index.lock")?;

    assert!(
        matches!(outcome, RebaseOutcome::Rebased),
        "rebase must retry through index.lock contention and land, got {outcome:?}",
    );
    assert!(
        !lock.exists(),
        "the stand-in index.lock should have been released by the concurrent task",
    );
    Ok(())
}

/// The ff-merge half of the same critical section: `ff_merge_integration`
/// also takes the loom-workspace `index.lock` (checkout + `merge --ff-only`)
/// and must retry through a peer's held lock rather than surfacing a
/// spurious failure (specs/harness.md § Concurrency). Same deterministic
/// stand-in lock + concurrent release as the rebase case.
#[tokio::test]
async fn ff_merge_integration_retries_through_index_lock_contention() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());

    git(&loom, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(loom.join("feature.txt"), "feature side\n")?;
    git(&loom, &["add", "feature.txt"])?;
    git(&loom, &["commit", "-q", "-m", "feature commit"])?;
    git(&loom, &["checkout", "-q", "main"])?;

    let client = GitClient::open(repo.path())?;
    // Rebase first (no contention) so the ff-merge is the operation under
    // lock contention.
    assert!(matches!(
        client.rebase_onto_integration("feature").await?,
        RebaseOutcome::Rebased,
    ));

    let lock = loom.join(".git/index.lock");
    std::fs::write(&lock, b"")?;
    let lock_clone = lock.clone();
    let releaser = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(120)).await;
        std::fs::remove_file(&lock_clone)
    });

    client.ff_merge_integration("feature").await?;
    releaser.await?.context("release stand-in index.lock")?;

    assert!(
        loom.join("feature.txt").exists(),
        "ff-merge must land after retrying through index.lock contention",
    );
    Ok(())
}

/// A lock that never clears (a crashed peer's stale `index.lock`) must not
/// loop forever: the bounded retry budget exhausts and surfaces a typed
/// [`GitError::IndexLocked`] naming the loom workspace, distinct from a
/// content failure, so the caller can report stale-lock contention.
#[tokio::test]
async fn rebase_onto_integration_surfaces_index_locked_on_stale_lock() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());

    git(&loom, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(loom.join("feature.txt"), "feature side\n")?;
    git(&loom, &["add", "feature.txt"])?;
    git(&loom, &["commit", "-q", "-m", "feature commit"])?;
    git(&loom, &["checkout", "-q", "main"])?;

    // Held for the duration of the call — never released.
    let lock = loom.join(".git/index.lock");
    std::fs::write(&lock, b"")?;

    let client = GitClient::open(repo.path())?;
    let err = client
        .rebase_onto_integration("feature")
        .await
        .expect_err("a stale index.lock must exhaust the retry budget");
    assert!(
        matches!(err, GitError::IndexLocked { ref workdir } if workdir.ends_with(".loom/integration")),
        "expected GitError::IndexLocked naming the loom workspace, got {err:?}",
    );
    Ok(())
}

#[tokio::test]
async fn status_reports_working_tree_and_index_changes() -> Result<()> {
    // Status on a clean checkout reports zero entries; modifying a tracked
    // file surfaces as a WorktreeChange, and `git add`ing a new file
    // surfaces as an IndexChange. The typed API distinguishes the two
    // without leaking gix internals.
    let repo = init_repo()?;
    let path = repo.path();
    let client = GitClient::open(path)?;

    let clean = client.status().await?;
    assert!(
        clean.is_empty(),
        "clean checkout should report zero status entries, got {clean:?}",
    );

    std::fs::write(path.join("README.md"), "modified line\n")?;
    let worktree_only = client.status().await?;
    assert!(
        worktree_only
            .iter()
            .any(|e| e.path == "README.md" && e.kind == StatusKind::WorktreeChange),
        "modifying tracked file should produce a WorktreeChange entry, got {worktree_only:?}",
    );

    std::fs::write(path.join("new.txt"), "staged content\n")?;
    git(path, &["add", "new.txt"])?;
    let both = client.status().await?;
    assert!(
        both.iter()
            .any(|e| e.path == "new.txt" && e.kind == StatusKind::IndexChange),
        "git add of a new path should produce an IndexChange entry, got {both:?}",
    );
    Ok(())
}

#[tokio::test]
async fn rev_exists_and_ancestor_walk_real_repo() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    let client = GitClient::open(path)?;

    let initial = capture_head(path)?;
    assert!(client.rev_exists(&initial).await?);
    assert!(
        !client
            .rev_exists("0000000000000000000000000000000000000000")
            .await?
    );
    assert!(client.is_ancestor_of_head(&initial).await?);

    // Detach a commit on a side branch — exists but not an ancestor of main HEAD.
    git(path, &["checkout", "-q", "-b", "side"])?;
    std::fs::write(path.join("side.txt"), "side\n")?;
    git(path, &["add", "side.txt"])?;
    git(path, &["commit", "-q", "-m", "side"])?;
    let side_sha = capture_head(path)?;
    git(path, &["checkout", "-q", "main"])?;

    assert!(client.rev_exists(&side_sha).await?);
    assert!(!client.is_ancestor_of_head(&side_sha).await?);
    Ok(())
}

#[tokio::test]
async fn changed_spec_files_and_diff_spec_pick_up_changes() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    std::fs::create_dir_all(path.join("specs"))?;
    std::fs::write(path.join("specs/alpha.md"), "# alpha\n")?;
    std::fs::write(path.join("specs/beta.md"), "# beta\n")?;
    git(path, &["add", "specs"])?;
    git(path, &["commit", "-q", "-m", "seed specs"])?;
    let base = capture_head(path)?;

    std::fs::write(path.join("specs/alpha.md"), "# alpha\n\nupdated\n")?;
    std::fs::write(path.join("README.md"), "ignore me — non-spec change\n")?;
    git(path, &["commit", "-q", "-am", "update alpha + readme"])?;

    let client = GitClient::open(path)?;
    let changed = client.changed_spec_files(&base).await?;
    assert_eq!(
        changed,
        vec![std::path::PathBuf::from("specs/alpha.md")],
        "only specs/ paths must surface — README ignored: got {changed:?}",
    );

    let alpha_diff = client
        .diff_spec(&base, std::path::Path::new("specs/alpha.md"))
        .await?;
    assert!(
        alpha_diff.contains("updated"),
        "diff should contain the new line: {alpha_diff}",
    );
    let beta_diff = client
        .diff_spec(&base, std::path::Path::new("specs/beta.md"))
        .await?;
    assert!(
        beta_diff.is_empty(),
        "untouched spec must produce empty diff: {beta_diff}",
    );
    Ok(())
}

#[tokio::test]
async fn head_commit_sha_round_trips_through_git() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    let client = GitClient::open(path)?;
    let sha = client.head_commit_sha().await?;
    let expected = capture_head(path)?;
    assert_eq!(sha.as_str(), expected);
    assert_eq!(
        sha.as_str().len(),
        40,
        "git rev-parse HEAD returns a 40-char SHA: {sha}"
    );
    Ok(())
}

#[tokio::test]
async fn commits_since_counts_revisions_added_after_a_commit() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    let client = GitClient::open(path)?;

    let base = capture_head(path)?;
    assert_eq!(client.commits_since(&base).await?, 0);

    std::fs::write(path.join("a.txt"), "a\n")?;
    git(path, &["add", "a.txt"])?;
    git(path, &["commit", "-q", "-m", "add a"])?;
    std::fs::write(path.join("b.txt"), "b\n")?;
    git(path, &["add", "b.txt"])?;
    git(path, &["commit", "-q", "-m", "add b"])?;

    assert_eq!(client.commits_since(&base).await?, 2);
    Ok(())
}

/// Spec gate (`specs/harness.md` § Bead dispatch — `[test?]`
/// `bead_dispatch_creates_clone_under_loom_beads`): bead workspaces live
/// under `.loom/beads/<id>/` (flat — globally-unique bead ids, no
/// spec partition) and the bead branch is `loom/<id>`. The destination
/// is created as a `git clone --local` of the loom workspace, so its
/// `.git/` is a regular directory inside the bind-mounted path.
/// Idempotent at the directory level: a second call against the same
/// bead returns the existing path rather than tripping
/// `git clone --local: destination path already exists`.
#[tokio::test]
async fn bead_dispatch_creates_clone_under_loom_beads() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;

    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-bead.1")?;
    let created = client.create_worktree(&label, &bead).await?;

    let expected_path = repo.path().join(".loom/beads/lm-bead.1");
    assert_eq!(
        created.path, expected_path,
        "bead workspace must live under .loom/beads/<id>/ (flat — no \
         spec partition); got {:?}",
        created.path,
    );
    assert_eq!(
        created.branch, "loom/lm-bead.1",
        "bead branch must be loom/<id> (spec component dropped); got {:?}",
        created.branch,
    );
    assert!(
        created.path.join(".git").is_dir(),
        ".git inside the bead workspace must be a regular directory \
         (clone), not a worktree pointer file: got {:?}",
        created.path.join(".git"),
    );

    // Idempotent: a second call against the same bead returns the
    // existing path without re-cloning. Without this, the per-bead-close
    // lifecycle would trip `git clone --local: destination path already
    // exists` on the second dispatch attempt.
    let again = client.create_worktree(&label, &bead).await?;
    assert_eq!(again.path, created.path);
    assert_eq!(again.branch, created.branch);

    client.remove_worktree(&created.path).await?;
    Ok(())
}

/// Spec gate (`specs/harness.md` § Bead dispatch — Per-bead-close
/// lifecycle): the dispatch path runs `reset_bead_clone` before every
/// agent attempt, so the worker observes an empty `git status
/// --porcelain` going in. This pins the *post-reset* state — the
/// load-bearing invariant the verdict gate's tree-clean check builds on,
/// so anything dirty the agent leaves behind is unambiguously the
/// agent's own write.
#[tokio::test]
async fn bead_worktree_post_reset_porcelain_is_empty() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;

    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-clean.1")?;
    let created = client.create_worktree(&label, &bead).await?;
    client.reset_bead_clone(&created.path).await?;

    let porcelain = client.status_porcelain_at(&created.path).await?;
    assert!(
        porcelain.is_empty(),
        "post-reset bead worktree must have empty `git status --porcelain` \
         so tree-clean checks attribute every dirty path to the agent. got: {porcelain:?}",
    );

    client.remove_worktree(&created.path).await?;
    Ok(())
}

/// Spec gate (`specs/harness.md` § Bead dispatch — Per-bead-close
/// lifecycle): `reset_bead_clone` MUST drop uncommitted mid-session
/// leftovers while preserving the bead branch's `HEAD` *and* the warm
/// caches under `target/`, `.git/`, and `.wrix/`. Without those
/// excludes, every recovery iteration would burn cargo + sccache state
/// and tear out bind-mount staging.
#[tokio::test]
async fn bead_workspace_reset_preserves_target_and_dotwrix() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;

    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-reset.1")?;
    let created = client.create_worktree(&label, &bead).await?;

    // Simulate the state the dispatch path observes between attempts:
    //
    // * an agent-committed file on the bead branch (must survive the reset
    //   — `git reset --hard HEAD` keeps committed work),
    // * an unstaged tracked-file edit (must be dropped — that's the
    //   recovery-leftover case),
    // * an untracked top-level file (must be dropped — `git clean -fdx`),
    // * warm scratch dirs at `target/`, `.git/objects/loom-test`, and
    //   `.wrix/dolt.sock-marker` (each must survive the clean — those
    //   are cargo/sccache state, refs, and bind-mount staging).
    agent_commit(&created.path, "committed.txt", "agent commit\n", "agent")?;
    std::fs::write(created.path.join("README.md"), "mid-session edit\n")?;
    std::fs::write(created.path.join("untracked.txt"), "scratch\n")?;
    std::fs::create_dir_all(created.path.join("target/debug"))?;
    std::fs::write(
        created.path.join("target/debug/sentinel"),
        b"cargo artifact\n",
    )?;
    std::fs::create_dir_all(created.path.join(".wrix"))?;
    std::fs::write(
        created.path.join(".wrix/dolt.sock-marker"),
        b"bind-mount staging\n",
    )?;
    let head_before = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&created.path)
            .args(["rev-parse", "HEAD"])
            .output()?
            .stdout,
    )?
    .trim()
    .to_string();

    client.reset_bead_clone(&created.path).await?;

    // HEAD is unchanged — `git reset --hard HEAD` does not drop commits.
    let head_after = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&created.path)
            .args(["rev-parse", "HEAD"])
            .output()?
            .stdout,
    )?
    .trim()
    .to_string();
    assert_eq!(
        head_before, head_after,
        "reset_bead_clone must preserve the bead branch's HEAD (agent's prior commits)",
    );

    // Committed work survives.
    assert!(
        created.path.join("committed.txt").exists(),
        "agent's committed file must survive reset",
    );

    // Uncommitted leftover (tracked edit + untracked file) is gone.
    let readme = std::fs::read_to_string(created.path.join("README.md"))?;
    assert_eq!(
        readme, "initial\n",
        "tracked-file edit must be reset to HEAD content",
    );
    assert!(
        !created.path.join("untracked.txt").exists(),
        "untracked top-level file must be removed by clean -fdx",
    );

    // Preserved scratch dirs survive the clean.
    assert!(
        created.path.join("target/debug/sentinel").exists(),
        "target/ must survive so cargo + sccache stay warm",
    );
    assert!(
        created.path.join(".git").is_dir(),
        ".git/ must survive (refs + bead branch)",
    );
    assert!(
        created.path.join(".wrix/dolt.sock-marker").exists(),
        ".wrix/ must survive so extra-mount staging persists across attempts",
    );

    // Idempotence: a second reset against the now-clean tree is a no-op —
    // committed content + every preserved scratch dir still in place.
    client.reset_bead_clone(&created.path).await?;
    assert!(created.path.join("committed.txt").exists());
    assert!(created.path.join("target/debug/sentinel").exists());
    assert!(created.path.join(".wrix/dolt.sock-marker").exists());

    client.remove_worktree(&created.path).await?;
    Ok(())
}

/// `create_worktree` MUST NOT write `commit.gpgsign=false` into the bead
/// clone's local config. The wrix profile provisions a signing key and
/// sets `commit.gpgsign=true` globally; a local `false` override silently
/// produces unsigned agent commits that then land on driver `main`.
#[tokio::test]
async fn create_worktree_does_not_disable_signing() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;

    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-sign.1")?;
    let created = client.create_worktree(&label, &bead).await?;

    let output = Command::new("git")
        .arg("-C")
        .arg(&created.path)
        .args(["config", "--local", "--get-all", "commit.gpgsign"])
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        !stdout
            .lines()
            .any(|l| l.trim().eq_ignore_ascii_case("false")),
        "bead clone must not carry a local commit.gpgsign=false override — \
         that silently bypasses the signing key provisioned by the profile. \
         got local commit.gpgsign entries: {stdout:?}",
    );

    client.remove_worktree(&created.path).await?;
    Ok(())
}

#[tokio::test]
async fn commits_since_surfaces_git_cli_error_on_unknown_commit() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    let client = GitClient::open(path)?;

    let bogus = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let err = client.commits_since(bogus).await;
    assert!(
        err.is_err(),
        "commits_since against unknown commit must surface an error: {err:?}",
    );
    Ok(())
}

fn agent_commit(worktree: &Path, filename: &str, body: &str, msg: &str) -> Result<()> {
    std::fs::write(worktree.join(filename), body)?;
    git(worktree, &["add", filename])?;
    git(
        worktree,
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", msg],
    )?;
    Ok(())
}

/// Spec contract (`specs/harness.md` § Success Criteria · Bead dispatch):
/// the driver fetches the bead branch from the bead workspace path into
/// the loom workspace — `git fetch <bead-workspace-path> loom/<id>:loom/<id>`
/// treating the path as an ad-hoc filesystem URL — so `merge_branch` can
/// rebase + ff it onto the integration branch. The worker never pushes;
/// the fetch is the only thing that makes the bead branch visible in the
/// loom workspace.
#[tokio::test]
async fn driver_fetches_bead_branch_from_workspace_path() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-fetch.1")?;
    let created = client.create_worktree(&label, &bead).await?;
    agent_commit(&created.path, "agent-change.txt", "agent work\n", "agent")?;
    let bead_head = capture_head(&created.path)?;

    // Pre-condition: the bead branch is absent from the loom workspace
    // until the driver fetches it — the worker never pushed.
    assert!(
        rev_parse(&loom, &created.branch).is_none(),
        "bead branch must not exist in the loom workspace before the fetch",
    );

    client.fetch_bead_branch(&created.path, &bead).await?;

    let fetched = rev_parse(&loom, &created.branch)
        .context("bead branch must exist in the loom workspace after fetch")?;
    assert_eq!(
        fetched, bead_head,
        "fetched ref must point at the bead clone's HEAD",
    );

    // The fetched branch is what `merge_branch` rebases + ff's onto the
    // integration branch — no push was involved at any step.
    let result = client.merge_branch(&created.branch).await?;
    assert_eq!(result, MergeResult::Ok);
    assert!(
        loom.join("agent-change.txt").exists(),
        "fetched bead work must land in the loom workspace after merge",
    );

    client.remove_worktree(&created.path).await?;
    Ok(())
}

/// Spec criterion (`specs/harness.md` § Verdict Gate, phase 2): when no
/// signing key resolves — i.e. the loom workspace has no
/// `.git/loom-allowed-signers` file — signature verification is skipped
/// at both passes and the integration step proceeds. The fetched bead
/// branch verifies as `SignatureCheck::Skipped` rather than failing.
/// Criterion: `signature_verification_skipped_when_no_key`.
#[tokio::test]
async fn signature_verification_skipped_when_no_key() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-nosig.1")?;
    let created = client.create_worktree(&label, &bead).await?;
    agent_commit(&created.path, "agent-change.txt", "agent work\n", "agent")?;
    client.fetch_bead_branch(&created.path, &bead).await?;

    // No allowed_signers file was written, so verification is inactive.
    assert!(
        !loom.join(".git/loom-allowed-signers").exists(),
        "precondition: no allowed_signers file (no key configured)",
    );
    assert!(
        !client.signing_verification_enabled().await?,
        "verification must be disabled when no allowed_signers file resolves",
    );
    let range = format!("HEAD..{}", created.branch);
    assert_eq!(
        client.verify_commit_range(&range).await?,
        SignatureCheck::Skipped,
        "with no key the range must verify as Skipped, not fail",
    );

    // And the integration step proceeds: the conditional verify is a
    // no-op, so the merge still folds the bead work in cleanly.
    assert_eq!(client.merge_branch(&created.branch).await?, MergeResult::Ok);
    client.remove_worktree(&created.path).await?;
    Ok(())
}

/// Key-present counterpart to [`signature_verification_skipped_when_no_key`]:
/// with a loom-workspace `.git/loom-allowed-signers` file present, the early
/// "no key" skip guard in `verify_commit_range` is bypassed — a signed bead
/// commit verifies as [`SignatureCheck::Verified`], NOT `Skipped`. Without
/// this contrast the no-key test would pass coincidentally (the loom
/// workspace never carries an allowed_signers file in that fixture, so the
/// `Skipped` result holds regardless of the key state). Spec criterion
/// (`specs/harness.md` § Verdict Gate, phase 2):
/// `signature_verification_runs_when_key_present`.
#[tokio::test]
async fn signature_verification_runs_when_key_present() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());
    let key = gen_signing_key(repo.path())?;

    // Materialize the loom-workspace allowed_signers file (and the ssh
    // verify config `verify-commit` reads) for the resolved key — the state
    // `loom init` / `create_worktree` produce when a wrix key resolves.
    write_signing_config(&loom, &key)?;
    let signers_file = loom.join(".git").join("loom-allowed-signers");
    assert!(
        signers_file.is_file(),
        "precondition: allowed_signers file present (key configured)",
    );
    // verify-commit matches the committer email against the allowed_signers
    // principal — derive it so the signed bead commit lines up.
    let signers = std::fs::read_to_string(&signers_file)?;
    let identity = signers
        .split_whitespace()
        .next()
        .context("allowed_signers principal")?
        .to_string();

    // Sign the bead commit with the same key the loom-workspace
    // allowed_signers file trusts. The bead clone carries no signing block
    // (it would shadow wrix's container global config), so the signing key
    // and format are passed explicitly here — standing in for the wrix
    // `git-ssh-setup.sh` global config that signs the worker's commits
    // in-container.
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-sig.1")?;
    let created = client.create_worktree(&label, &bead).await?;
    std::fs::write(created.path.join("agent-change.txt"), "agent work\n")?;
    git(&created.path, &["add", "agent-change.txt"])?;
    let email_arg = format!("user.email={identity}");
    let signingkey_arg = format!("user.signingkey={}", key.to_string_lossy());
    git(
        &created.path,
        &[
            "-c",
            email_arg.as_str(),
            "-c",
            "user.name=loom",
            "-c",
            "gpg.format=ssh",
            "-c",
            signingkey_arg.as_str(),
            "commit",
            "-S",
            "-q",
            "-m",
            "signed agent work",
        ],
    )?;
    client.fetch_bead_branch(&created.path, &bead).await?;

    assert!(
        client.signing_verification_enabled().await?,
        "verification must be enabled when the allowed_signers file resolves",
    );
    let range = format!("HEAD..{}", created.branch);
    assert_eq!(
        client.verify_commit_range(&range).await?,
        SignatureCheck::Verified,
        "with a key present the signed bead commit must verify, not skip",
    );

    client.remove_worktree(&created.path).await?;
    Ok(())
}

/// Spec contract `[test?]` annotation (`specs/harness.md` § Success Criteria
/// · Commit signing): the driver-side rebase in the loom workspace produces
/// signed commits — the rewritten commit carries a `gpgsig` header even
/// though the worker's original commit was unsigned. The wrix signing key
/// is passphrase-less, so the rebase completes non-interactively (a prompt
/// would hang the test rather than fail it).
/// Criterion: `driver_rebase_signs_with_wrix_key`.
#[tokio::test]
async fn driver_rebase_signs_with_wrix_key() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());
    let key = gen_signing_key(repo.path())?;

    // Worker commit on the bead branch BEFORE any signing block is written,
    // so the rewrite is the only place a signature can appear.
    git(&loom, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(loom.join("feature.txt"), "feature side\n")?;
    git(&loom, &["add", "feature.txt"])?;
    git(&loom, &["commit", "-q", "-m", "feature commit"])?;
    anyhow::ensure!(
        !commit_has_gpgsig(&loom, "feature")?,
        "precondition: the worker commit must start unsigned",
    );

    git(&loom, &["checkout", "-q", "main"])?;
    std::fs::write(loom.join("main.txt"), "main side\n")?;
    git(&loom, &["add", "main.txt"])?;
    git(&loom, &["commit", "-q", "-m", "main commit"])?;

    // The signing block `loom init` writes when a wrix key resolves
    // (commit.gpgsign=true + user.signingkey) is what makes the rebase sign.
    write_signing_config(&loom, &key)?;

    let client = GitClient::open(repo.path())?;
    let outcome = client.rebase_onto_integration("feature").await?;
    assert!(
        matches!(outcome, RebaseOutcome::Rebased),
        "expected Rebased, got {outcome:?}",
    );

    assert!(
        commit_has_gpgsig(&loom, "feature")?,
        "the driver-rebased commit must carry a gpgsig header",
    );
    Ok(())
}

/// Spec contract `[test?]` annotation (`specs/harness.md` § Success Criteria
/// · Commit signing): `git log --show-signature` against a driver-rebased
/// commit prints `Good "git" signature` using the derived allowed_signers
/// file, and the typed pass-2 path (`verify_commit_range`) agrees that the
/// rewritten range verifies.
/// Criterion: `rebased_commits_verify_via_derived_allowed_signers`.
#[tokio::test]
async fn rebased_commits_verify_via_derived_allowed_signers() -> Result<()> {
    let repo = init_repo()?;
    let loom = loom_path(repo.path());
    let key = gen_signing_key(repo.path())?;

    git(&loom, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(loom.join("feature.txt"), "feature side\n")?;
    git(&loom, &["add", "feature.txt"])?;
    git(&loom, &["commit", "-q", "-m", "feature commit"])?;

    git(&loom, &["checkout", "-q", "main"])?;
    std::fs::write(loom.join("main.txt"), "main side\n")?;
    git(&loom, &["add", "main.txt"])?;
    git(&loom, &["commit", "-q", "-m", "main commit"])?;

    write_signing_config(&loom, &key)?;

    let client = GitClient::open(repo.path())?;
    assert!(
        matches!(
            client.rebase_onto_integration("feature").await?,
            RebaseOutcome::Rebased,
        ),
        "rebase must rewrite the worker commit onto the integration tip",
    );

    // `git log --show-signature` reads `gpg.ssh.allowedSignersFile` from the
    // block `write_signing_config` wrote and reports a good signature on its
    // stdout for the rewritten commit.
    let shown = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&loom)
            .args(["log", "--show-signature", "-1", "feature"])
            .output()?
            .stdout,
    )?;
    assert!(
        shown.contains("Good \"git\" signature"),
        "show-signature must report a good signature via the derived \
         allowed_signers file, got: {shown}",
    );

    // The typed pass-2 verification path agrees on the rewritten range.
    assert_eq!(
        client.verify_commit_range("main..feature").await?,
        SignatureCheck::Verified,
        "verify_commit_range must accept the driver-rebased commit",
    );
    Ok(())
}

/// Spec contract (`specs/harness.md` § Success Criteria · Bead dispatch):
/// under A3 the bead clone's `origin` remote remains pointing at the loom
/// workspace path after `create_worktree`. It is unused by the worker (no
/// push) but preserved so host-side ahead/behind tracking works when the
/// operator `cd`s into the bead clone; `create_worktree` must not stub or
/// rewrite it.
#[tokio::test]
async fn bead_clone_origin_unchanged_under_a3() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-origin.1")?;
    let created = client.create_worktree(&label, &bead).await?;

    let origin = loom_driver::git::read_origin_url(&created.path)?
        .context("bead clone must retain an origin remote under A3")?;
    assert_eq!(
        std::path::Path::new(&origin),
        client.loom_workspace().as_path(),
        "bead clone origin must point at the loom workspace path, not be stubbed",
    );

    client.remove_worktree(&created.path).await?;
    Ok(())
}

/// `create_worktree` must set the bead branch's upstream to
/// `origin/<integration_branch>` so the pre-push hook's
/// `loom gate verify --diff @{u}..HEAD` (`.pre-commit-config.yaml`)
/// resolves to the bead's own commits. Without an upstream, `@{u}` fails
/// ("no upstream configured") and the gate silently degrades to an
/// unscoped, whole-tree walk.
#[tokio::test]
async fn bead_branch_tracks_integration_branch_for_push_scope() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-upstream.1")?;
    let created = client.create_worktree(&label, &bead).await?;

    let upstream = rev_parse(&created.path, &format!("{}@{{upstream}}", created.branch))
        .context("bead branch must have an upstream so @{u} resolves")?;
    let origin_main = rev_parse(&created.path, "origin/main")
        .context("bead clone must have an origin/main tracking ref")?;
    assert_eq!(
        upstream, origin_main,
        "bead branch upstream must point at origin/<integration_branch>",
    );

    // The agent's commit is the only thing in the `@{u}..HEAD` push range —
    // exactly the scope the pre-push gate verifies.
    agent_commit(&created.path, "agent-change.txt", "agent work\n", "agent")?;
    let count = Command::new("git")
        .arg("-C")
        .arg(&created.path)
        .args(["rev-list", "--count", "@{u}..HEAD"])
        .output()?;
    assert!(count.status.success(), "rev-list @{{u}}..HEAD must succeed");
    assert_eq!(
        String::from_utf8(count.stdout)?.trim(),
        "1",
        "@{{u}}..HEAD must scope to the bead's lone commit",
    );

    client.remove_worktree(&created.path).await?;
    Ok(())
}

/// `git -C <repo> rev-parse <rev>` returning the resolved SHA, or `None`
/// when `<rev>` does not resolve (e.g. a branch absent from this repo).
fn rev_parse(repo: &Path, rev: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet", rev])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// Spec contract `[test?]` annotation
/// (`specs/harness.md` § Success Criteria · Bead dispatch):
/// `[loom] integration_branch` is honored end-to-end by the
/// loop's git plumbing — `merge_branch` rebases onto the configured
/// branch (NOT a hard-coded `main`), and `push` targets
/// `origin <integration_branch>`. The fixture stands up a repo whose
/// integration branch is `trunk` (not `main`), opens a `GitClient` with
/// `open_with_integration_branch("trunk")`, exercises both methods, and
/// asserts the configured branch is the one that advances.
#[tokio::test]
async fn integration_branch_setting_honored_by_loop() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("ws");
    loom_driver::git::init_test_repo_with_integration_branch(&path, "trunk")?;
    let loom = loom_path(&path);

    git(&loom, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(loom.join("feature.txt"), "feature\n")?;
    git(&loom, &["add", "feature.txt"])?;
    git(&loom, &["commit", "-q", "-m", "feature commit"])?;
    git(&loom, &["checkout", "-q", "trunk"])?;

    let client = GitClient::open_with_integration_branch(&path, "trunk".to_string())?;
    assert_eq!(client.integration_branch(), "trunk");

    let result = client.merge_branch("feature").await?;
    assert_eq!(result, MergeResult::Ok, "merge into configured branch");

    let on_branch = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&loom)
            .args(["symbolic-ref", "--short", "HEAD"])
            .output()?
            .stdout,
    )?
    .trim()
    .to_string();
    assert_eq!(
        on_branch, "trunk",
        "merge_branch must leave HEAD on the configured integration branch",
    );

    client.push().await?;

    let origin = loom_driver::git::bare_origin_path(&path);
    let origin_trunk = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&origin)
            .args(["rev-parse", "trunk"])
            .output()?
            .stdout,
    )?
    .trim()
    .to_string();
    let local_trunk = capture_head(&loom)?;
    assert_eq!(
        origin_trunk, local_trunk,
        "push must advance origin/<integration_branch>, not origin/main",
    );

    Ok(())
}

/// Spec contract (`specs/harness.md` § Success Criteria · Bead dispatch):
/// Origin push of the integration branch retries on non-fast-forward by
/// fetching `origin/<integration-branch>`, rebasing the loom workspace's
/// local branch onto it, and re-pushing. After the retry succeeds,
/// `origin` carries both the cross-spec commit and the loom workspace's
/// own commit on linear history.
#[tokio::test]
async fn origin_push_retries_non_fast_forward() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    let loom = loom_path(path);
    let origin = loom_driver::git::bare_origin_path(path);

    let other_root = tempfile::tempdir()?;
    let other = other_root.path().join("other");
    git(
        other_root.path(),
        &[
            "clone",
            "--quiet",
            origin.to_str().expect("origin utf-8"),
            other.to_str().expect("other utf-8"),
        ],
    )?;
    git(&other, &["config", "user.email", "other@example.com"])?;
    git(&other, &["config", "user.name", "Other"])?;
    git(&other, &["config", "commit.gpgsign", "false"])?;
    std::fs::write(other.join("other.txt"), "other\n")?;
    git(&other, &["add", "other.txt"])?;
    git(&other, &["commit", "-q", "-m", "other commit"])?;
    git(&other, &["push", "-q", "origin", "main"])?;

    std::fs::write(loom.join("loom-side.txt"), "loom\n")?;
    git(&loom, &["add", "loom-side.txt"])?;
    git(&loom, &["commit", "-q", "-m", "loom commit"])?;

    let client = GitClient::open(path)?;
    client.push().await?;

    let origin_tree = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&origin)
            .args(["ls-tree", "--name-only", "main"])
            .output()?
            .stdout,
    )?;
    assert!(
        origin_tree.contains("other.txt"),
        "origin must retain the cross-spec commit's file after retry: {origin_tree}",
    );
    assert!(
        origin_tree.contains("loom-side.txt"),
        "origin must carry the loom workspace's commit after retry: {origin_tree}",
    );

    let merges = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&loom)
            .args(["log", "--merges", "--format=%H"])
            .output()?
            .stdout,
    )?;
    assert!(
        merges.trim().is_empty(),
        "non-ff retry must rebase (linear history), not merge — got: {merges:?}",
    );

    Ok(())
}

fn capture_head(repo: &Path) -> Result<String> {
    capture_rev(repo, "HEAD")
}

fn capture_rev(repo: &Path, rev: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", rev])
        .output()?;
    anyhow::ensure!(output.status.success(), "git rev-parse {rev} failed");
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// Fake [`CommandRunner`] that answers `bd show <id> --json` from a fixed
/// map of bead-id → status. The parent is inferred from dotted ids. Unknown
/// ids yield a `not found`-shaped failure so the sweep exercises the
/// `bd show failed → skip` branch.
struct ScriptedBd {
    by_id: Mutex<HashMap<String, String>>,
}

impl ScriptedBd {
    fn new<const N: usize>(entries: [(&str, &str); N]) -> Self {
        let map = entries
            .into_iter()
            .map(|(id, status)| (id.to_string(), status.to_string()))
            .collect();
        Self {
            by_id: Mutex::new(map),
        }
    }
}

impl CommandRunner for ScriptedBd {
    async fn run(&self, args: Vec<OsString>, _t: Duration) -> Result<RunOutput, BdError> {
        let argv: Vec<String> = args
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let id = argv.get(1).cloned().unwrap_or_default();
        let map = self
            .by_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match map.get(&id) {
            Some(status) => {
                let parent = id
                    .split_once('.')
                    .map(|(molecule, _)| format!(",\"parent\":\"{molecule}\""))
                    .unwrap_or_default();
                let body = format!(
                    "[{{\"id\":\"{id}\",\"title\":\"\",\"status\":\"{status}\"{parent}}}]",
                );
                Ok(RunOutput {
                    status: 0,
                    stdout: body.into_bytes(),
                    stderr: Vec::new(),
                })
            }
            None => Ok(RunOutput {
                status: 1,
                stdout: Vec::new(),
                stderr: format!("no issue found matching \"{id}\"").into_bytes(),
            }),
        }
    }
}

fn make_bead_clone_dir(workspace: &Path, name: &str) -> Result<std::path::PathBuf> {
    let path = workspace.join(".loom/beads").join(name);
    std::fs::create_dir_all(&path)?;
    std::fs::write(path.join("marker"), b"present\n")?;
    Ok(path)
}

/// Spec gate (`specs/harness.md` § Garbage collection): `loom loop` startup
/// MUST drop directories under `.loom/beads/` whose bead is closed and
/// parented by the current molecule.
#[tokio::test]
async fn loop_startup_gc_drops_closed_bead_workspaces_for_current_molecule() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    let client = GitClient::open(path)?;
    let molecule = MoleculeId::new("lm-current");
    let bd = BdClient::with_runner(ScriptedBd::new([("lm-current.1", "closed")]));

    let dir = make_bead_clone_dir(path, "lm-current.1")?;
    assert!(dir.exists(), "precondition: workspace exists");

    let removed = client.sweep_orphan_bead_clones(&bd, &molecule).await?;

    assert_eq!(removed, vec![dir.clone()]);
    assert!(
        !dir.exists(),
        "closed-bead workspace must be removed: {dir:?}",
    );
    Ok(())
}

/// Closed workspaces from a different molecule may still be active under a
/// concurrently running loop, so startup GC must leave them alone.
#[tokio::test]
async fn loop_startup_gc_skips_closed_bead_workspaces_from_other_molecules() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    let client = GitClient::open(path)?;
    let molecule = MoleculeId::new("lm-current");
    let bd = BdClient::with_runner(ScriptedBd::new([
        ("lm-current.1", "closed"),
        ("lm-other.1", "closed"),
    ]));

    let current = make_bead_clone_dir(path, "lm-current.1")?;
    let other = make_bead_clone_dir(path, "lm-other.1")?;

    let removed = client.sweep_orphan_bead_clones(&bd, &molecule).await?;

    assert_eq!(removed, vec![current.clone()]);
    assert!(
        !current.exists(),
        "current molecule closed workspace is reaped"
    );
    assert!(
        other.exists(),
        "other molecule workspace must survive: {other:?}"
    );
    Ok(())
}

/// Spec gate (`specs/harness.md` § Garbage collection): bead workspaces
/// whose bead is in any non-closed state (open, in_progress, blocked) MUST
/// survive the sweep. The startup GC reaps closed orphans only.
#[tokio::test]
async fn loop_startup_gc_skips_open_bead_workspaces() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    let client = GitClient::open(path)?;
    let molecule = MoleculeId::new("lm-current");
    let bd = BdClient::with_runner(ScriptedBd::new([
        ("lm-current.1", "open"),
        ("lm-current.2", "in_progress"),
        ("lm-current.3", "blocked"),
    ]));

    let open = make_bead_clone_dir(path, "lm-current.1")?;
    let progress = make_bead_clone_dir(path, "lm-current.2")?;
    let blocked = make_bead_clone_dir(path, "lm-current.3")?;

    let removed = client.sweep_orphan_bead_clones(&bd, &molecule).await?;

    assert!(
        removed.is_empty(),
        "non-closed workspaces must be preserved: removed={removed:?}",
    );
    assert!(open.exists(), "open bead workspace must survive: {open:?}");
    assert!(
        progress.exists(),
        "in_progress bead workspace must survive: {progress:?}",
    );
    assert!(
        blocked.exists(),
        "blocked bead workspace must survive: {blocked:?}",
    );
    Ok(())
}

/// Spec gate (`specs/harness.md` § Garbage collection): a corrupt orphan
/// (directory name that does not parse as a bead id, or whose `bd show`
/// lookup fails) MUST NOT abort the sweep — log + skip so `loom loop` can
/// still start. The sweep continues past the corrupt entry and reaps any
/// closed orphans that follow it.
#[tokio::test]
async fn loop_startup_gc_logs_and_skips_corrupt_orphans() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    let client = GitClient::open(path)?;
    // `lm-current.1` is closed; `lm-unknown.1` has no bd record (lookup
    // fails); `not a bead id` does not parse as a `BeadId` at all.
    let molecule = MoleculeId::new("lm-current");
    let bd = BdClient::with_runner(ScriptedBd::new([("lm-current.1", "closed")]));

    let invalid = make_bead_clone_dir(path, "not a bead id")?;
    let unknown = make_bead_clone_dir(path, "lm-unknown.1")?;
    let closed = make_bead_clone_dir(path, "lm-current.1")?;

    let removed = client.sweep_orphan_bead_clones(&bd, &molecule).await?;

    assert_eq!(
        removed,
        vec![closed.clone()],
        "only the closed orphan is reaped; corrupt entries are skipped",
    );
    assert!(
        invalid.exists(),
        "invalid-id directory must be preserved: {invalid:?}",
    );
    assert!(
        unknown.exists(),
        "bd-lookup-failed directory must be preserved: {unknown:?}",
    );
    assert!(!closed.exists(), "closed bead workspace must be removed");
    Ok(())
}

/// A missing `.loom/beads/` directory is a no-op for the sweep — the
/// happy first-run shape where no bead has been dispatched yet.
#[tokio::test]
async fn loop_startup_gc_no_op_when_base_dir_missing() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let molecule = MoleculeId::new("lm-current");
    let bd = BdClient::with_runner(ScriptedBd::new([]));

    let removed = client.sweep_orphan_bead_clones(&bd, &molecule).await?;

    assert!(removed.is_empty());
    Ok(())
}

fn gen_signing_key(dir: &Path) -> Result<std::path::PathBuf> {
    let key = dir.join("signing-key");
    let status = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-q", "-C", "", "-f"])
        .arg(&key)
        .status()
        .context("spawn ssh-keygen")?;
    anyhow::ensure!(status.success(), "ssh-keygen exited with {status}");
    Ok(key)
}

/// Whether `rev`'s commit object carries a `gpgsig` header — the structural
/// marker of a signed commit, present regardless of which key signed it.
fn commit_has_gpgsig(repo: &Path, rev: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "commit", rev])
        .output()?;
    anyhow::ensure!(output.status.success(), "git cat-file commit {rev} failed");
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .take_while(|line| !line.is_empty())
        .any(|line| line.starts_with("gpgsig")))
}

fn local_config(repo: &Path, key: &str) -> Result<Option<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["config", "--local", "--get", key])
        .output()
        .context("spawn git config")?;
    Ok(out
        .status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()))
}

/// Spec contract `[test]` annotation (`specs/harness.md` § Success
/// Criteria · Commit signing): `GitClient::create_worktree` writes **no**
/// signing block into the bead clone's local `.git/config`, even when a
/// signing key resolves. A local block would carry the HOST key path, which
/// does not exist inside the wrix bead container; because local config
/// beats global, it would shadow wrix's `git-ssh-setup.sh` global config
/// (which points `user.signingkey` at the in-container key copy) and break
/// the worker's in-container `git commit`. Driven through the test-only
/// override seam — which forces the key to resolve — because
/// `$WRIX_SIGNING_KEY` cannot be set under edition 2024's unsafe
/// `env::set_var` with `unsafe_code` forbidden.
#[tokio::test]
async fn create_worktree_omits_signing_block_in_bead_clone() -> Result<()> {
    let repo = init_repo()?;
    let key = gen_signing_key(repo.path())?;
    let mut client = GitClient::open(repo.path())?;
    client.set_signing_key_override(key.clone());

    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-sign.1")?;
    let created = client.create_worktree(&label, &bead).await?;

    // No signing keys are written into the clone's local config, so the
    // wrix container global config remains the sole in-container authority.
    for k in [
        "gpg.format",
        "user.signingkey",
        "commit.gpgsign",
        "gpg.ssh.allowedSignersFile",
    ] {
        assert_eq!(
            local_config(&created.path, k)?,
            None,
            "bead clone must carry no local `{k}` — it would shadow wrix's \
             container global signing config and break the worker's commit",
        );
    }
    assert!(
        !created
            .path
            .join(".git")
            .join("loom-allowed-signers")
            .exists(),
        "no allowed_signers file should be derived into the bead clone",
    );
    Ok(())
}

/// Without a resolved signing key, `create_worktree` writes no signing
/// block — the operator's global gitconfig governs (the bead clone's own
/// `origin` is a local path, so the GitHub deploy-key fallback is skipped).
#[tokio::test]
async fn create_worktree_omits_signing_block_when_no_key() -> Result<()> {
    if std::env::var_os("WRIX_SIGNING_KEY").is_some() {
        // The ambient env carries a real signing key; the no-key path
        // cannot be exercised here.
        return Ok(());
    }
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;

    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-sign.2")?;
    let created = client.create_worktree(&label, &bead).await?;

    assert_eq!(local_config(&created.path, "user.signingkey")?, None);
    assert_eq!(local_config(&created.path, "gpg.format")?, None);
    Ok(())
}

/// `GitClient::launcher_key_env` surfaces the resolved signing key as a
/// `WRIX_SIGNING_KEY` → HOST-path pair so loom can hand it to the `wrix
/// spawn` launcher (loop agents otherwise boot with no git keys).
/// Unlike the bead-clone gitconfig (which maps to in-container paths), the
/// launcher env carries the host path verbatim — wrix performs the
/// host→container mapping itself. Driven through the signing-key override
/// seam; the deploy key is absent because the test repo's origin is a local
/// path (not GitHub), so the deploy-key fallback is skipped.
#[tokio::test]
async fn launcher_key_env_exposes_signing_key_host_path() -> Result<()> {
    if std::env::var_os("WRIX_DEPLOY_KEY").is_some() {
        // Ambient env would inject a deploy-key entry; skip to keep the
        // assertion on the absent-deploy-key path deterministic.
        return Ok(());
    }
    let repo = init_repo()?;
    let key = gen_signing_key(repo.path())?;
    let mut client = GitClient::open(repo.path())?;
    client.set_signing_key_override(key.clone());

    let env = client.launcher_key_env()?;

    let signing = env
        .iter()
        .find(|(k, _)| k == "WRIX_SIGNING_KEY")
        .expect("WRIX_SIGNING_KEY must be present");
    assert_eq!(
        signing.1,
        key.to_string_lossy(),
        "launcher env must carry the HOST signing-key path verbatim",
    );
    assert!(
        !env.iter().any(|(k, _)| k == "WRIX_DEPLOY_KEY"),
        "no deploy key resolves for a non-GitHub origin: {env:?}",
    );
    Ok(())
}

/// Land a fresh commit carrying `file` on `origin/main` from a throwaway
/// clone — simulating cross-path work reaching published main while the
/// loom workspace's local main stays behind (its `origin/main` tracking
/// ref goes stale).
fn advance_origin_main(repo: &Path, file: &str) -> Result<()> {
    let origin = loom_driver::git::bare_origin_path(repo);
    let other_root = tempfile::tempdir()?;
    let other = other_root.path().join("other");
    let origin_str = origin
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("origin path not utf-8"))?;
    let other_str = other
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("other path not utf-8"))?;
    git(
        other_root.path(),
        &["clone", "--quiet", origin_str, other_str],
    )?;
    git(&other, &["config", "user.email", "other@example.com"])?;
    git(&other, &["config", "user.name", "Other"])?;
    git(&other, &["config", "commit.gpgsign", "false"])?;
    std::fs::write(other.join(file), b"published\n")?;
    git(&other, &["add", file])?;
    git(&other, &["commit", "-q", "-m", "published commit"])?;
    git(&other, &["push", "-q", "origin", "main"])?;
    Ok(())
}

/// Spec contract (`specs/harness.md` § Success Criteria · Bead dispatch):
/// `loom loop` / `loom init` startup fast-forwards the loom workspace's
/// integration branch to `origin/<branch>` before any bead clone is
/// materialized, so the local base matches published HEAD.
#[tokio::test]
async fn loop_start_fast_forwards_integration_to_origin_main() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    let loom = loom_path(path);

    advance_origin_main(path, "published.txt")?;
    assert!(
        !loom.join("published.txt").exists(),
        "precondition: loom workspace main is behind published HEAD",
    );

    let client = GitClient::open(path)?;
    let outcome = client.fast_forward_integration_to_origin().await?;
    assert_eq!(outcome, FastForwardOutcome::FastForwarded { advanced: 1 });

    assert!(
        loom.join("published.txt").exists(),
        "loom workspace main must carry the published commit after FF",
    );
    let origin = loom_driver::git::bare_origin_path(path);
    assert_eq!(
        capture_head(&loom)?,
        capture_head(&origin)?,
        "local integration HEAD must equal origin/main after FF",
    );
    Ok(())
}

/// Spec contract (`specs/harness.md` § Success Criteria · Bead dispatch):
/// when the integration branch has diverged from `origin/<branch>` (local
/// commits not on origin), startup fails loud naming the divergent commits
/// rather than silently branching every bead off the stale base.
#[tokio::test]
async fn loop_start_fails_loud_when_integration_diverged_from_origin() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    let loom = loom_path(path);

    // Unpublished driver work on local main (never pushed to origin).
    std::fs::write(loom.join("loom-only.txt"), b"local\n")?;
    git(&loom, &["add", "loom-only.txt"])?;
    git(&loom, &["commit", "-q", "-m", "unpublished driver work"])?;
    let divergent = capture_head(&loom)?;

    // Origin advances along a different line — the split-brain.
    advance_origin_main(path, "published.txt")?;

    let client = GitClient::open(path)?;
    let err = client
        .fast_forward_integration_to_origin()
        .await
        .err()
        .ok_or_else(|| anyhow::anyhow!("diverged integration must fail loud"))?;
    match err {
        GitError::IntegrationDiverged { branch, commits } => {
            assert_eq!(branch, "main");
            assert!(
                commits.contains(&divergent[..7]),
                "error must name the divergent commit {divergent}: {commits}",
            );
        }
        other => anyhow::bail!("expected IntegrationDiverged, got {other:?}"),
    }
    assert_eq!(
        capture_head(&loom)?,
        divergent,
        "diverged local main must be left untouched — no silent FF or reset",
    );
    Ok(())
}

/// Spec contract (`specs/harness.md` § Success Criteria · Bead dispatch):
/// after the startup fast-forward, a bead clone forks from published HEAD,
/// carrying commits that landed on `origin/main` rather than the stale
/// local base that existed before reconciliation.
#[tokio::test]
async fn bead_clone_branches_off_published_head_not_stale_base() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();

    advance_origin_main(path, "published.txt")?;

    let client = GitClient::open(path)?;
    client.fast_forward_integration_to_origin().await?;

    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-ff.1")?;
    let created = client.create_worktree(&label, &bead).await?;

    assert!(
        created.path.join("published.txt").exists(),
        "bead clone must branch off published HEAD carrying the landed commit",
    );
    Ok(())
}
