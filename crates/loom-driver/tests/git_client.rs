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
use loom_driver::git::{GitClient, MergeResult, StatusKind};
use loom_driver::identifier::{BeadId, SpecLabel};
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
    let path = dir.path();
    git(path, &["init", "-q", "-b", "main"])?;
    git(path, &["config", "user.email", "test@example.com"])?;
    git(path, &["config", "user.name", "Test"])?;
    git(path, &["config", "commit.gpgsign", "false"])?;
    std::fs::write(path.join("README.md"), "initial\n")?;
    git(path, &["add", "README.md"])?;
    git(path, &["commit", "-q", "-m", "initial"])?;
    Ok(dir)
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
        created.path.ends_with(".wrapix/loom/beads/lm-3hhwq.6"),
        "workspace path should end with .wrapix/loom/beads/<bead-id>: {:?}",
        created.path
    );
    // Path A (specs/harness.md § Worktree Dispatch): the bead workspace is
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
    let path = repo.path();

    git(path, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(path.join("feature.txt"), "added on feature\n")?;
    git(path, &["add", "feature.txt"])?;
    git(path, &["commit", "-q", "-m", "feature commit"])?;
    git(path, &["checkout", "-q", "main"])?;

    let client = GitClient::open(path)?;
    let result = client.merge_branch("feature").await?;

    assert_eq!(result, MergeResult::Ok);
    assert!(path.join("feature.txt").exists());
    Ok(())
}

#[tokio::test]
async fn merge_branch_non_conflicting_returns_ok() -> Result<()> {
    // Both branches diverge after a shared base — feature adds feature.txt,
    // main adds main.txt. Neither touches the other's file, so merge_branch
    // rebases feature onto main's HEAD then fast-forwards. Both files land
    // on the driver branch with linear history.
    let repo = init_repo()?;
    let path = repo.path();

    git(path, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(path.join("feature.txt"), "feature side\n")?;
    git(path, &["add", "feature.txt"])?;
    git(path, &["commit", "-q", "-m", "feature commit"])?;

    git(path, &["checkout", "-q", "main"])?;
    std::fs::write(path.join("main.txt"), "main side\n")?;
    git(path, &["add", "main.txt"])?;
    git(path, &["commit", "-q", "-m", "main commit"])?;

    let client = GitClient::open(path)?;
    let result = client.merge_branch("feature").await?;

    assert_eq!(result, MergeResult::Ok);
    assert!(path.join("feature.txt").exists());
    assert!(path.join("main.txt").exists());

    let on_main = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["symbolic-ref", "--short", "HEAD"])
            .output()?
            .stdout,
    )?
    .trim()
    .to_string();
    assert_eq!(
        on_main, "main",
        "merge_branch must leave the working tree on the driver branch",
    );
    Ok(())
}

/// merge_branch rejects creating non-fast-forward history: when the bead
/// branch and the driver branch have both moved beyond their shared base
/// with non-overlapping changes, the resulting `HEAD` contains no merge
/// commits — the rebase + `--ff-only` path replaces what `--no-ff` would
/// have produced as a merge commit.
#[tokio::test]
async fn merge_branch_uses_ff_only_and_rejects_non_ff_history() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    let base = capture_head(path)?;

    git(path, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(path.join("feature.txt"), "feature side\n")?;
    git(path, &["add", "feature.txt"])?;
    git(path, &["commit", "-q", "-m", "feature commit"])?;

    git(path, &["checkout", "-q", "main"])?;
    std::fs::write(path.join("main.txt"), "main side\n")?;
    git(path, &["add", "main.txt"])?;
    git(path, &["commit", "-q", "-m", "main commit"])?;

    let client = GitClient::open(path)?;
    let result = client.merge_branch("feature").await?;

    assert_eq!(result, MergeResult::Ok);

    let merges = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(path)
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
/// the driver branch and a second bead, forked from the original base,
/// must land on the moved `HEAD`.
#[tokio::test]
async fn merge_branch_rebases_bead_branch_onto_head_before_ff() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    let base = capture_head(path)?;

    git(path, &["checkout", "-q", "-b", "bead-a", &base])?;
    std::fs::write(path.join("bead-a.txt"), "bead a\n")?;
    git(path, &["add", "bead-a.txt"])?;
    git(path, &["commit", "-q", "-m", "bead a commit"])?;

    git(path, &["checkout", "-q", "-b", "bead-b", &base])?;
    std::fs::write(path.join("bead-b.txt"), "bead b\n")?;
    git(path, &["add", "bead-b.txt"])?;
    git(path, &["commit", "-q", "-m", "bead b commit"])?;

    git(path, &["checkout", "-q", "main"])?;
    let client = GitClient::open(path)?;
    assert_eq!(client.merge_branch("bead-a").await?, MergeResult::Ok);

    let bead_b_pre = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(path)
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
            .arg(path)
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
            .arg(path)
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

    assert!(path.join("bead-a.txt").exists());
    assert!(path.join("bead-b.txt").exists());
    Ok(())
}

#[tokio::test]
async fn merge_branch_conflict_is_reported() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();

    // Branch A: rewrite README on `feature`.
    git(path, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(path.join("README.md"), "feature line\n")?;
    git(path, &["commit", "-q", "-am", "feature edit"])?;

    // Branch B: rewrite same line on `main`.
    git(path, &["checkout", "-q", "main"])?;
    std::fs::write(path.join("README.md"), "main line\n")?;
    git(path, &["commit", "-q", "-am", "main edit"])?;

    let client = GitClient::open(path)?;
    let result = client.merge_branch("feature").await?;

    assert!(
        matches!(result, MergeResult::Conflict { .. }),
        "expected Conflict, got {result:?}",
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
    assert_eq!(sha, expected);
    assert_eq!(
        sha.len(),
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
/// under `.wrapix/loom/beads/<id>/` (flat — globally-unique bead ids, no
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

    let expected_path = repo.path().join(".wrapix/loom/beads/lm-bead.1");
    assert_eq!(
        created.path, expected_path,
        "bead workspace must live under .wrapix/loom/beads/<id>/ (flat — no \
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
/// caches under `target/`, `.git/`, and `.wrapix/`. Without those
/// excludes, every recovery iteration would burn cargo + sccache state
/// and tear out bind-mount staging.
#[tokio::test]
async fn bead_workspace_reset_preserves_target_and_dotwrapix() -> Result<()> {
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
    //   `.wrapix/dolt.sock-marker` (each must survive the clean — those
    //   are cargo/sccache state, refs, and bind-mount staging).
    agent_commit(&created.path, "committed.txt", "agent commit\n", "agent")?;
    std::fs::write(created.path.join("README.md"), "mid-session edit\n")?;
    std::fs::write(created.path.join("untracked.txt"), "scratch\n")?;
    std::fs::create_dir_all(created.path.join("target/debug"))?;
    std::fs::write(
        created.path.join("target/debug/sentinel"),
        b"cargo artifact\n",
    )?;
    std::fs::create_dir_all(created.path.join(".wrapix"))?;
    std::fs::write(
        created.path.join(".wrapix/dolt.sock-marker"),
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
        created.path.join(".wrapix/dolt.sock-marker").exists(),
        ".wrapix/ must survive so extra-mount staging persists across attempts",
    );

    // Idempotence: a second reset against the now-clean tree is a no-op —
    // committed content + every preserved scratch dir still in place.
    client.reset_bead_clone(&created.path).await?;
    assert!(created.path.join("committed.txt").exists());
    assert!(created.path.join("target/debug/sentinel").exists());
    assert!(created.path.join(".wrapix/dolt.sock-marker").exists());

    client.remove_worktree(&created.path).await?;
    Ok(())
}

/// `create_worktree` MUST NOT write `commit.gpgsign=false` into the bead
/// clone's local config. The wrapix profile provisions a signing key and
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

/// Absolute path to a `bash` interpreter resolved at test time.
///
/// Hook scripts need a shebang the kernel can exec. `/usr/bin/env` isn't
/// in the nix build sandbox, so `#!/usr/bin/env bash` fails with
/// `cannot exec ...: No such file or directory` (the error is about the
/// interpreter, not the script). Discover bash via `PATH` and embed the
/// absolute path in the shebang instead.
fn shebang_bash() -> Result<std::path::PathBuf> {
    let path = std::env::var_os("PATH").context("PATH not set")?;
    std::env::split_paths(&path)
        .map(|d| d.join("bash"))
        .find(|p| p.is_file())
        .context("bash not on PATH")
}

fn install_pre_push_hook(hooks_dir: &Path, body: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let hook = hooks_dir.join("pre-push");
    let bash = shebang_bash()?;
    let script = format!("#!{}\nset -euo pipefail\n{body}", bash.display());
    std::fs::write(&hook, script)?;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
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

/// `push_branch_to_origin` fires the destination's pre-push hook — that's
/// the per-bead test backpressure (the workspace's pre-push CI stage runs
/// here). If this stopped firing, broken commits would slip into driver
/// `main` without the test gate.
#[tokio::test]
async fn push_branch_to_origin_invokes_pre_push_hook() -> Result<()> {
    let repo = init_repo()?;
    let hooks_dir = repo.path().join(".loom-test-hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    let marker = repo.path().join("hook-fired");
    let hook_log = repo.path().join("hook-log");
    // The hook writes both a marker file (assertion target) and a log
    // capturing its env, cwd, and any error. When the marker is missing
    // post-push, the log tells us whether the hook ran at all and what
    // it saw.
    install_pre_push_hook(
        &hooks_dir,
        &format!(
            "{{ date; pwd; echo HOOK_PATH=$0; echo HOOKS_PATH=$(git config --get core.hooksPath); }} > {:?} 2>&1\ntouch {:?}\nexit 0\n",
            hook_log.display(),
            marker.display()
        ),
    )?;

    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-pp.1")?;
    let created = client.create_worktree(&label, &bead).await?;
    // pre-push fires on the sending side; set core.hooksPath on the clone.
    git(
        &created.path,
        &["config", "core.hooksPath", hooks_dir.to_str().unwrap()],
    )?;
    agent_commit(&created.path, "agent-change.txt", "agent work\n", "agent")?;

    client
        .push_branch_to_origin(&created.path, &created.branch)
        .await?;
    if !marker.exists() {
        // Diagnostics for the flake: what did the hook see (if it ran)?
        use std::os::unix::fs::PermissionsExt;
        let log =
            std::fs::read_to_string(&hook_log).unwrap_or_else(|e| format!("(log missing: {e})"));
        let hook_meta = std::fs::metadata(hooks_dir.join("pre-push"))
            .map(|m| format!("mode={:o} size={}", m.permissions().mode() & 0o777, m.len()))
            .unwrap_or_else(|e| format!("(hook stat failed: {e})"));
        panic!(
            "pre-push hook must fire on bead-merge push\n\
             marker={}\n\
             hook script: {}\n\
             hook log:\n{}",
            marker.display(),
            hook_meta,
            log,
        );
    }

    client.remove_worktree(&created.path).await?;
    Ok(())
}

/// A failing pre-push hook surfaces as `GitError::GitCli`, blocking the
/// merge. If the workspace's pre-push CI fails (test regression, lint
/// failure), the bead's commit must not land on driver `main`.
#[tokio::test]
async fn push_branch_to_origin_propagates_pre_push_hook_failure() -> Result<()> {
    let repo = init_repo()?;
    let hooks_dir = repo.path().join(".loom-test-hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    install_pre_push_hook(&hooks_dir, "exit 1\n")?;

    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let bead = BeadId::new("lm-pp.2")?;
    let created = client.create_worktree(&label, &bead).await?;
    git(
        &created.path,
        &["config", "core.hooksPath", hooks_dir.to_str().unwrap()],
    )?;
    agent_commit(&created.path, "agent-change.txt", "agent work\n", "agent")?;

    let err = client
        .push_branch_to_origin(&created.path, &created.branch)
        .await
        .expect_err("failing pre-push must surface as an error");
    assert!(
        matches!(err, loom_driver::git::GitError::GitCli { .. }),
        "expected GitCli error from failing hook, got {err:?}",
    );

    client.remove_worktree(&created.path).await?;
    Ok(())
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
    let path = dir.path();
    git(path, &["init", "-q", "-b", "trunk"])?;
    git(path, &["config", "user.email", "test@example.com"])?;
    git(path, &["config", "user.name", "Test"])?;
    git(path, &["config", "commit.gpgsign", "false"])?;
    std::fs::write(path.join("README.md"), "initial\n")?;
    git(path, &["add", "README.md"])?;
    git(path, &["commit", "-q", "-m", "initial"])?;

    let parent = path.parent().expect("tempdir parent");
    let name = path
        .file_name()
        .expect("tempdir name")
        .to_string_lossy()
        .into_owned();
    let origin = parent.join(format!("{name}.git"));
    std::fs::create_dir_all(&origin)?;
    git(&origin, &["init", "-q", "--bare", "-b", "trunk"])?;
    git(
        path,
        &[
            "remote",
            "add",
            "origin",
            origin.to_str().expect("origin path utf-8"),
        ],
    )?;
    git(path, &["push", "-q", "-u", "origin", "trunk"])?;

    git(path, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(path.join("feature.txt"), "feature\n")?;
    git(path, &["add", "feature.txt"])?;
    git(path, &["commit", "-q", "-m", "feature commit"])?;
    git(path, &["checkout", "-q", "trunk"])?;

    let client = GitClient::open_with_integration_branch(path, "trunk".to_string())?;
    assert_eq!(client.integration_branch(), "trunk");

    let result = client.merge_branch("feature").await?;
    assert_eq!(result, MergeResult::Ok, "merge into configured branch");

    let on_branch = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(path)
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
    let local_trunk = capture_head(path)?;
    assert_eq!(
        origin_trunk, local_trunk,
        "push must advance origin/<integration_branch>, not origin/main",
    );

    Ok(())
}

fn capture_head(repo: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()?;
    anyhow::ensure!(output.status.success(), "git rev-parse failed");
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// Fake [`CommandRunner`] that answers `bd show <id> --json` from a fixed
/// map of bead-id → status. Unknown ids yield a `not found`-shaped failure
/// so the sweep exercises the `bd show failed → skip` branch.
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
                let body = format!("[{{\"id\":\"{id}\",\"title\":\"\",\"status\":\"{status}\"}}]",);
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
    let path = workspace.join(".wrapix/loom/beads").join(name);
    std::fs::create_dir_all(&path)?;
    std::fs::write(path.join("marker"), b"present\n")?;
    Ok(path)
}

/// Spec gate (`specs/harness.md` § Garbage collection): `loom loop` startup
/// MUST drop every directory under `.wrapix/loom/beads/` whose bead is
/// `closed`. The sweep runs workspace-global (not spec-scoped) under the
/// spec advisory lock — closed beads cannot be in flight, so the sweep is
/// safe regardless of which spec is being loop'd.
#[tokio::test]
async fn loop_startup_gc_drops_closed_bead_workspaces() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();
    let client = GitClient::open(path)?;
    let bd = BdClient::with_runner(ScriptedBd::new([("lm-closed", "closed")]));

    let dir = make_bead_clone_dir(path, "lm-closed")?;
    assert!(dir.exists(), "precondition: workspace exists");

    let removed = client.sweep_orphan_bead_clones(&bd).await?;

    assert_eq!(removed, vec![dir.clone()]);
    assert!(
        !dir.exists(),
        "closed-bead workspace must be removed: {dir:?}",
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
    let bd = BdClient::with_runner(ScriptedBd::new([
        ("lm-open", "open"),
        ("lm-progress", "in_progress"),
        ("lm-blocked", "blocked"),
    ]));

    let open = make_bead_clone_dir(path, "lm-open")?;
    let progress = make_bead_clone_dir(path, "lm-progress")?;
    let blocked = make_bead_clone_dir(path, "lm-blocked")?;

    let removed = client.sweep_orphan_bead_clones(&bd).await?;

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
    // `lm-closed` is closed; `lm-unknown` has no bd record (lookup fails);
    // `not a bead id` does not parse as a `BeadId` at all.
    let bd = BdClient::with_runner(ScriptedBd::new([("lm-closed", "closed")]));

    let invalid = make_bead_clone_dir(path, "not a bead id")?;
    let unknown = make_bead_clone_dir(path, "lm-unknown")?;
    let closed = make_bead_clone_dir(path, "lm-closed")?;

    let removed = client.sweep_orphan_bead_clones(&bd).await?;

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

/// A missing `.wrapix/loom/beads/` directory is a no-op for the sweep — the
/// happy first-run shape where no bead has been dispatched yet.
#[tokio::test]
async fn loop_startup_gc_no_op_when_base_dir_missing() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let bd = BdClient::with_runner(ScriptedBd::new([]));

    let removed = client.sweep_orphan_bead_clones(&bd).await?;

    assert!(removed.is_empty());
    Ok(())
}
