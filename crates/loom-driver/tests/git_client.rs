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

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
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
    let bead = BeadId::new("wx-3hhwq.6")?;
    let created = client.create_worktree(&label, &bead).await?;

    assert!(
        created.path.exists(),
        "workspace path {:?} should exist on disk",
        created.path
    );
    assert_eq!(created.branch, "loom/harness/wx-3hhwq.6");
    assert!(
        created.path.ends_with("harness/wx-3hhwq.6"),
        "workspace path should end with <label>/<bead-id>: {:?}",
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

    assert_eq!(result, MergeResult::Conflict);
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

/// Spec gate (`specs/harness.md` § Bead worktree dispatch): every
/// per-bead worktree created by `loom run` MUST have an empty `git
/// status --porcelain` immediately after creation, so the verdict gate's
/// tree-clean check is sound by construction — anything dirty the agent
/// leaves behind is unambiguously the agent's own write.
#[tokio::test]
async fn bead_worktree_starts_with_empty_porcelain() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;

    let label = SpecLabel::new("harness");
    let bead = BeadId::new("wx-clean.1")?;
    let created = client.create_worktree(&label, &bead).await?;

    let porcelain = client.status_porcelain_at(&created.path).await?;
    assert!(
        porcelain.is_empty(),
        "fresh bead worktree must have empty `git status --porcelain` so \
         tree-clean checks attribute every dirty path to the agent. got: {porcelain:?}",
    );

    // The bead branch lives inside the clone (not the main repo) until
    // the merge-back push, so cleanup is just removing the directory —
    // no `git branch -D` against the main repo's refs.
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

fn capture_head(repo: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()?;
    anyhow::ensure!(output.status.success(), "git rev-parse failed");
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}
