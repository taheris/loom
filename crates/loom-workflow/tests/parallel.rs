//! Integration tests for `loom_workflow::run::parallel` and the public
//! `Parallelism` flag — anything that touches a real git repo lives here.
//! Pure logic tests stay in `src/run/parallel.rs::tests` and
//! `src/run/parallelism.rs::tests`.
//!
//! These tests spawn `git` to seed and inspect real repos (spec NFR #8):
//! parallel dispatch's contract is the on-disk shape of worktrees, the
//! per-bead branches `git worktree list` reports, and the index/merge
//! state after `merge_back`. An in-process `LineParse + tokio::io::duplex`
//! substitute can't observe those — git's index, refs database, and
//! conflict resolution are what the tests assert on.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;
use std::str::FromStr;

use anyhow::{Context, Result};
use loom_driver::bd::Bead;
use loom_driver::git::GitClient;
use loom_driver::identifier::{BeadId, SpecLabel};
use loom_workflow::run::{
    AgentOutcome, BatchResult, BatchSlot, Parallelism, ParallelismError, create_worktrees,
    merge_back,
};
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
        out.status
    );
    Ok(String::from_utf8(out.stdout)?)
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

fn fake_bead(id: &str) -> Bead {
    Bead {
        id: BeadId::new(id).expect("valid bead id"),
        title: format!("title-{id}"),
        description: "desc".into(),
        status: "open".into(),
        priority: 2,
        issue_type: "task".into(),
        labels: vec![],
        parent: None,
        metadata: Default::default(),
        notes: None,
    }
}

/// Acceptance: `--parallel 1` (default) does not create a worktree and works
/// on the driver branch. The worktree branch is gated by `Parallelism::is_one`,
/// so the contract is enforceable as a pure function on the parsed flag — the
/// driver routes on the same predicate before ever calling `create_worktrees`.
#[test]
fn parallel_one_no_worktree() {
    let p: Parallelism = "1".parse().expect("parses");
    assert!(p.is_one(), "Parallelism::ONE must report is_one() == true");
    assert_eq!(p.get(), 1);

    let bigger: Parallelism = "2".parse().expect("parses");
    assert!(!bigger.is_one(), "N>1 must NOT take the no-worktree path");
    let four: Parallelism = "4".parse().expect("parses");
    assert!(!four.is_one());
}

/// Acceptance: `--parallel N` (`N > 1`) creates one worktree per dispatched
/// bead under `.wrapix/worktree/<label>/<bead-id>/`.
#[tokio::test]
async fn parallel_creates_worktrees() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let beads = vec![fake_bead("wx-1"), fake_bead("wx-2"), fake_bead("wx-3")];

    let slots = create_worktrees(&client, &label, beads.clone()).await?;

    assert_eq!(slots.len(), 3, "one worktree per bead");
    for (bead, slot) in beads.iter().zip(slots.iter()) {
        let expected_rel = format!(".wrapix/worktree/harness/{}", bead.id);
        let expected_path = repo.path().join(&expected_rel);
        assert!(
            slot.worktree.path.exists(),
            "worktree path {:?} for {} must exist",
            slot.worktree.path,
            bead.id,
        );
        assert_eq!(slot.worktree.path, expected_path);
        assert_eq!(slot.worktree.branch, format!("loom/harness/{}", bead.id));
        assert_eq!(slot.bead.id, bead.id);
    }

    let listed = client.worktrees().await?;
    for slot in &slots {
        assert!(
            listed.iter().any(|w| w.path == slot.worktree.path),
            "gix worktrees() must list {:?}",
            slot.worktree.path,
        );
    }
    Ok(())
}

/// Acceptance: successful bead branches are merged back to the driver
/// branch after the batch completes.
#[tokio::test]
async fn parallel_merge_back() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let beads = vec![fake_bead("wx-mergea"), fake_bead("wx-mergeb")];

    let slots = create_worktrees(&client, &label, beads.clone()).await?;

    // Simulate a "successful agent run" inside each worktree: write a unique
    // file, commit on the per-bead branch.
    for slot in &slots {
        let unique = format!("from-{}\n", slot.bead.id);
        let file = format!("{}.txt", slot.bead.id);
        std::fs::write(slot.worktree.path.join(&file), &unique)?;
        git(&slot.worktree.path, &["add", &file])?;
        git(
            &slot.worktree.path,
            &["commit", "-q", "-m", &format!("work for {}", slot.bead.id)],
        )?;
    }

    let batch_slots: Vec<BatchSlot> = slots
        .iter()
        .map(|w| BatchSlot {
            bead: w.bead.clone(),
            worktree: w.worktree.clone(),
            outcome: AgentOutcome::Success,
        })
        .collect();

    let outcome = merge_back(&client, batch_slots).await?;

    assert_eq!(outcome.results.len(), 2);
    let merged = outcome.merged_ids();
    assert_eq!(merged.len(), 2, "both should merge: {:?}", outcome.results);
    for slot in &slots {
        assert!(merged.contains(&slot.bead.id));
        // Per-bead file landed on the driver branch.
        let file = format!("{}.txt", slot.bead.id);
        assert!(
            repo.path().join(&file).exists(),
            "{} should be merged into driver",
            file,
        );
        // Worktree dir is removed after a clean merge.
        assert!(
            !slot.worktree.path.exists(),
            "worktree {:?} should be removed after merge",
            slot.worktree.path,
        );
        // Branch is gone.
        let branches = git_capture(repo.path(), &["branch", "--list", &slot.worktree.branch])?;
        assert!(
            branches.trim().is_empty(),
            "branch {} should be deleted after merge, listed: {:?}",
            slot.worktree.branch,
            branches,
        );
    }

    Ok(())
}

/// Acceptance: on agent failure the per-bead worktree branch is cleaned up
/// (deleted) and the bead is queued for retry per the retry policy. The
/// `BatchResult::AgentFailed` variant carries the error body the caller
/// threads back into the next attempt as `previous_failure`.
#[tokio::test]
async fn parallel_failure_cleanup() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let beads = vec![fake_bead("wx-faila"), fake_bead("wx-failb")];
    let slots = create_worktrees(&client, &label, beads.clone()).await?;

    // Make at least one commit on the bead branch so `git branch -D` has
    // something to delete (an empty branch with no diff from main is still
    // deletable, but exercising the realistic "agent did some work then
    // crashed" path is more useful).
    for slot in &slots {
        let file = format!("{}.partial", slot.bead.id);
        std::fs::write(slot.worktree.path.join(&file), "partial work\n")?;
        git(&slot.worktree.path, &["add", &file])?;
        git(
            &slot.worktree.path,
            &[
                "commit",
                "-q",
                "-m",
                &format!("partial for {}", slot.bead.id),
            ],
        )?;
    }

    let batch_slots: Vec<BatchSlot> = slots
        .iter()
        .map(|w| BatchSlot {
            bead: w.bead.clone(),
            worktree: w.worktree.clone(),
            outcome: AgentOutcome::Failure {
                error: format!("crashed inside {}", w.bead.id),
            },
        })
        .collect();

    let outcome = merge_back(&client, batch_slots).await?;
    assert_eq!(outcome.results.len(), 2);

    let failures = outcome.failure_ids();
    assert_eq!(
        failures.len(),
        2,
        "both beads should be in AgentFailed: {:?}",
        outcome.results
    );

    for slot in &slots {
        // Worktree dir gone.
        assert!(
            !slot.worktree.path.exists(),
            "worktree {:?} should be cleaned up on agent failure",
            slot.worktree.path,
        );
        // Branch deleted.
        let branches = git_capture(repo.path(), &["branch", "--list", &slot.worktree.branch])?;
        assert!(
            branches.trim().is_empty(),
            "branch {} should be deleted after agent failure (got: {:?})",
            slot.worktree.branch,
            branches,
        );
        // Error body threaded into AgentFailed for retry-with-context.
        let r = outcome
            .results
            .iter()
            .find(|r| matches!(r, BatchResult::AgentFailed { bead, .. } if *bead == slot.bead.id))
            .expect("AgentFailed for slot");
        if let BatchResult::AgentFailed { error, .. } = r {
            assert!(error.contains(slot.bead.id.as_str()));
        }
    }
    // Driver branch must NOT contain the partial work.
    for slot in &slots {
        let file = format!("{}.partial", slot.bead.id);
        assert!(
            !repo.path().join(&file).exists(),
            "{} must not appear on driver branch after agent failure",
            file,
        );
    }
    Ok(())
}

/// Acceptance: on merge conflict the worktree is preserved and the bead is
/// marked failed (not silently overwritten). The driver branch is left
/// in a merge-in-progress state, which the caller resolves out-of-band.
#[tokio::test]
async fn parallel_conflict_preserves_worktree() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let bead = fake_bead("wx-conflict");
    let slots = create_worktrees(&client, &label, vec![bead.clone()]).await?;
    let slot = slots.into_iter().next().expect("one slot");

    // Worktree edits README on bead branch.
    std::fs::write(slot.worktree.path.join("README.md"), "from-bead\n")?;
    git(&slot.worktree.path, &["commit", "-q", "-am", "bead edit"])?;
    // Driver branch edits the same line.
    std::fs::write(repo.path().join("README.md"), "from-driver\n")?;
    git(repo.path(), &["commit", "-q", "-am", "driver edit"])?;

    let batch_slot = BatchSlot {
        bead: slot.bead.clone(),
        worktree: slot.worktree.clone(),
        outcome: AgentOutcome::Success,
    };
    let outcome = merge_back(&client, vec![batch_slot]).await?;

    assert_eq!(outcome.results.len(), 1);
    let r = &outcome.results[0];
    let BatchResult::Conflict {
        bead: bid,
        worktree_path,
        branch,
    } = r
    else {
        panic!("expected Conflict, got {r:?}");
    };
    assert_eq!(*bid, bead.id);
    // Worktree preserved.
    assert!(
        worktree_path.exists(),
        "worktree {:?} should be preserved on conflict",
        worktree_path,
    );
    assert_eq!(*branch, slot.worktree.branch);
    // Branch still exists.
    let branches = git_capture(repo.path(), &["branch", "--list", &slot.worktree.branch])?;
    assert!(
        !branches.trim().is_empty(),
        "branch {} should be preserved on conflict (got: {:?})",
        slot.worktree.branch,
        branches,
    );

    Ok(())
}

/// Spec contract: `merge_back` runs sequentially — the output
/// `BatchResult` vector preserves the input `BatchSlot` order. The spec
/// pins this as the mitigation for index-lock contention ("single-threaded
/// merge avoids index lock races"); a future refactor to `JoinSet` /
/// `join_all` would scramble the order, so a stable-order assertion is a
/// direct behavioural fence against accidental parallelism.
#[tokio::test]
async fn merge_back_preserves_input_slot_order() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    // Use bead ids that sort differently lexically and numerically so a
    // scrambling re-order would be observable on either axis.
    let beads = vec![
        fake_bead("wx-zeta"),
        fake_bead("wx-alpha"),
        fake_bead("wx-mu"),
        fake_bead("wx-beta"),
    ];

    let slots = create_worktrees(&client, &label, beads.clone()).await?;

    // Mark each worktree with a unique commit so merges are real, not
    // empty-tree no-ops.
    for slot in &slots {
        let file = format!("{}.txt", slot.bead.id);
        std::fs::write(slot.worktree.path.join(&file), b"work\n")?;
        git(&slot.worktree.path, &["add", &file])?;
        git(
            &slot.worktree.path,
            &["commit", "-q", "-m", &format!("work {}", slot.bead.id)],
        )?;
    }

    let batch_slots: Vec<BatchSlot> = slots
        .iter()
        .map(|w| BatchSlot {
            bead: w.bead.clone(),
            worktree: w.worktree.clone(),
            outcome: AgentOutcome::Success,
        })
        .collect();

    let outcome = merge_back(&client, batch_slots).await?;
    let observed: Vec<&str> = outcome
        .results
        .iter()
        .map(|r| match r {
            BatchResult::Merged { bead } => bead.as_str(),
            BatchResult::Conflict { bead, .. } => bead.as_str(),
            BatchResult::AgentFailed { bead, .. } => bead.as_str(),
            BatchResult::AgentBlocked { bead, .. } => bead.as_str(),
            BatchResult::AgentClarify { bead, .. } => bead.as_str(),
        })
        .collect();
    let expected: Vec<&str> = beads.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(
        observed, expected,
        "merge_back must produce results in input order — parallel dispatch \
         would scramble them and race the git index",
    );
    Ok(())
}

/// Acceptance: `--parallel N` flag validation — positive integers parse;
/// non-positive or non-integer values fail with a clear error before any
/// work begins.
#[test]
fn run_parallel_flag_validation() {
    // Positive integers parse.
    for ok_input in ["1", "2", "8", "16", "100"] {
        let p = Parallelism::from_str(ok_input).expect("positive int parses");
        assert_eq!(p.get(), ok_input.parse::<u32>().unwrap());
    }
    // Defaults to 1.
    assert!(Parallelism::default().is_one());

    // Rejected: zero, negatives, non-integers, empty.
    for bad in [
        "0", "-1", "-100", "abc", "1.5", "", "  ", "0x10", "1e3", "+1abc",
    ] {
        let err = Parallelism::from_str(bad)
            .err()
            .unwrap_or_else(|| panic!("`{bad}` must be rejected"));
        assert!(matches!(err, ParallelismError::NotPositiveInteger { .. }));
        // The error message echoes the offending input so users see
        // exactly what they typed.
        let msg = format!("{err}");
        assert!(
            msg.contains("--parallel must be a positive integer"),
            "error message must say what's required: {msg}",
        );
    }
}
