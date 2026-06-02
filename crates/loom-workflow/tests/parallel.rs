//! Integration tests for `loom_workflow::r#loop::parallel` and the public
//! `Parallelism` flag — anything that touches a real git repo lives here.
//! Pure logic tests stay in `src/loop/parallel.rs::tests` and
//! `src/loop/parallelism.rs::tests`.
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
use loom_workflow::r#loop::{
    AgentOutcome, BatchResult, BatchSlot, Parallelism, ParallelismError, create_worktrees,
    merge_back,
};
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "commit.gpgsign=false"])
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
        .args(["-c", "commit.gpgsign=false"])
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
    loom_driver::git::init_test_repo_with_integration(dir.path())?;
    Ok(dir)
}

fn loom_path(repo: &Path) -> std::path::PathBuf {
    repo.join(".loom/integration")
}

/// Write a `beads-push` stub at `dir/beads-push-stub.sh` that exits 0,
/// returning its path. Threaded into `merge_back` so cargo nextest does
/// not shell out to the real beads remote while exercising the post-merge
/// push path.
fn beads_push_stub(dir: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let stub = dir.join("beads-push-stub.sh");
    std::fs::write(&stub, "#!/bin/sh\nexit 0\n").expect("write stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
    stub
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

/// Generate a passphrase-less ed25519 signing key under `dir`. Returns
/// `Ok(None)` when `ssh-keygen` is not on `PATH` so the signing tests
/// degrade to a skip on hosts without OpenSSH (the criterion is annotated
/// `[test?]`).
fn gen_signing_key(dir: &Path) -> Result<Option<std::path::PathBuf>> {
    let key = dir.join("signing-key");
    let spawned = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-q", "-C", "", "-f"])
        .arg(&key)
        .status();
    match spawned {
        Ok(status) if status.success() => Ok(Some(key)),
        Ok(status) => anyhow::bail!("ssh-keygen exited with {status}"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("spawn ssh-keygen"),
    }
}

/// Acceptance (`specs/harness.md` § Bead dispatch — flat-keyed
/// `.loom/beads/<id>/` layout): dispatching a single bead — even
/// at `--parallel 1` — materialises `.loom/beads/<bead-id>/` and
/// runs the merge-back path after the bead completes. Universal worktree
/// isolation: the main checkout is never the bead's workdir. The
/// merge-back step (previously a no-op when N=1 ran on the driver
/// branch) now always runs.
#[tokio::test]
async fn bead_dispatch_creates_worktree() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let bead = fake_bead("lm-solo");

    // Step 1: dispatching a single bead through the `create_worktrees`
    // path materialises the per-bead worktree at the spec-pinned path,
    // even though only one bead is in the batch.
    let slots = create_worktrees(&client, &label, vec![bead.clone()]).await?;
    assert_eq!(slots.len(), 1, "one worktree for one bead");
    let slot = &slots[0];
    let expected_path = repo.path().join(".loom/beads/lm-solo");
    assert!(
        slot.worktree.path.exists(),
        "worktree path {:?} must exist after dispatch",
        slot.worktree.path,
    );
    assert_eq!(slot.worktree.path, expected_path);
    assert_eq!(slot.worktree.branch, "loom/lm-solo");

    // The main checkout is never the bead's workdir — the worktree path
    // is strictly under `.loom/beads/...`, not the repo root.
    assert_ne!(
        slot.worktree.path,
        repo.path(),
        "main checkout must NOT be the bead's workdir",
    );

    // Step 2: simulate a successful agent run inside the bead's worktree
    // and assert the merge-back path runs (worktree removed, branch
    // deleted, bead file present on driver). Pre-universal-dispatch, this
    // path was a no-op for N=1; the bead now exercises it unconditionally.
    let unique_file = format!("{}.txt", slot.bead.id);
    std::fs::write(slot.worktree.path.join(&unique_file), "from-bead\n")?;
    git(&slot.worktree.path, &["add", &unique_file])?;
    git(
        &slot.worktree.path,
        &["commit", "-q", "-m", &format!("work for {}", slot.bead.id)],
    )?;
    let batch_slots = vec![BatchSlot {
        bead: slot.bead.clone(),
        worktree: slot.worktree.clone(),
        outcome: AgentOutcome::Success,
    }];
    let stub = beads_push_stub(repo.path());
    let outcome = merge_back(&client, &stub, batch_slots).await?;

    assert_eq!(outcome.merged_ids(), vec![slot.bead.id.clone()]);
    let loom = loom_path(repo.path());
    assert!(
        loom.join(&unique_file).exists(),
        "bead's file must land on the integration branch after merge-back",
    );
    assert!(
        !slot.worktree.path.exists(),
        "worktree must be removed after clean merge-back",
    );
    let branches = git_capture(&loom, &["branch", "--list", &slot.worktree.branch])?;
    assert!(
        branches.trim().is_empty(),
        "bead's branch must be deleted after merge-back (got: {branches:?})",
    );
    Ok(())
}

/// Acceptance (`specs/tests.md` line 597 — `parallel_run_two_beads_e2e`):
/// `loom loop --parallel 2` with two ready beads creates one workspace
/// per bead under `.loom/beads/<bead-id>/` (concurrent dispatch).
#[tokio::test]
async fn parallel_run_two_beads_e2e() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let beads = vec![fake_bead("lm-1"), fake_bead("lm-2"), fake_bead("lm-3")];

    let slots = create_worktrees(&client, &label, beads.clone()).await?;

    assert_eq!(slots.len(), 3, "one worktree per bead");
    for (bead, slot) in beads.iter().zip(slots.iter()) {
        let expected_rel = format!(".loom/beads/{}", bead.id);
        let expected_path = repo.path().join(&expected_rel);
        assert!(
            slot.worktree.path.exists(),
            "worktree path {:?} for {} must exist",
            slot.worktree.path,
            bead.id,
        );
        assert_eq!(slot.worktree.path, expected_path);
        assert_eq!(slot.worktree.branch, format!("loom/{}", bead.id));
        assert_eq!(slot.bead.id, bead.id);
        // Path A (specs/harness.md § Bead dispatch): each workspace is a
        // self-contained clone — its `.git/` is a regular directory inside
        // the bind-mounted path so the wrapix container can resolve gitdir.
        assert!(
            slot.worktree.path.join(".git").is_dir(),
            ".git inside the bead workspace must be a regular directory, \
             not a worktree pointer file: got {:?}",
            slot.worktree.path.join(".git"),
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
    let beads = vec![fake_bead("lm-mergea"), fake_bead("lm-mergeb")];

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

    let stub = beads_push_stub(repo.path());
    let outcome = merge_back(&client, &stub, batch_slots).await?;

    assert_eq!(outcome.results.len(), 2);
    let merged = outcome.merged_ids();
    assert_eq!(merged.len(), 2, "both should merge: {:?}", outcome.results);
    let loom = loom_path(repo.path());
    for slot in &slots {
        assert!(merged.contains(&slot.bead.id));
        let file = format!("{}.txt", slot.bead.id);
        assert!(
            loom.join(&file).exists(),
            "{} should be merged into the integration branch",
            file,
        );
        assert!(
            !slot.worktree.path.exists(),
            "worktree {:?} should be removed after merge",
            slot.worktree.path,
        );
        let branches = git_capture(&loom, &["branch", "--list", &slot.worktree.branch])?;
        assert!(
            branches.trim().is_empty(),
            "branch {} should be deleted after merge, listed: {:?}",
            slot.worktree.branch,
            branches,
        );
    }

    Ok(())
}

/// Acceptance (`specs/harness.md` § Verdict Gate —
/// `workspace_persists_on_all_failure_paths`): on agent failure the per-bead
/// worktree and its branch are PRESERVED on disk (not removed) so a partial
/// diff survives for recovery and the next `bd ready` re-dispatch can reuse
/// the clone. The `BatchResult::AgentFailed` variant carries the error body
/// the caller threads back into the next attempt as `previous_failure`.
#[tokio::test]
async fn parallel_failure_preserves_worktree() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let beads = vec![fake_bead("lm-faila"), fake_bead("lm-failb")];
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

    let stub = beads_push_stub(repo.path());
    let outcome = merge_back(&client, &stub, batch_slots).await?;
    assert_eq!(outcome.results.len(), 2);

    let failures = outcome.failure_ids();
    assert_eq!(
        failures.len(),
        2,
        "both beads should be in AgentFailed: {:?}",
        outcome.results
    );

    let loom = loom_path(repo.path());
    for slot in &slots {
        assert!(
            slot.worktree.path.exists(),
            "worktree {:?} must be preserved on agent failure for recovery",
            slot.worktree.path,
        );
        // The agent's commits live on the bead branch inside the preserved
        // clone (the failure path never fetches the branch into the loom
        // repo), so the bead branch must still resolve there.
        let clone_branches = git_capture(
            &slot.worktree.path,
            &["branch", "--list", &slot.worktree.branch],
        )?;
        assert!(
            !clone_branches.trim().is_empty(),
            "bead branch {} must survive in the preserved clone (got: {:?})",
            slot.worktree.branch,
            clone_branches,
        );
        // Nothing leaks into the loom integration repo on a failed bead.
        let branches = git_capture(&loom, &["branch", "--list", &slot.worktree.branch])?;
        assert!(
            branches.trim().is_empty(),
            "branch {} must not appear in the loom repo after agent failure (got: {:?})",
            slot.worktree.branch,
            branches,
        );
        let r = outcome
            .results
            .iter()
            .find(|r| matches!(r, BatchResult::AgentFailed { bead, .. } if *bead == slot.bead.id))
            .expect("AgentFailed for slot");
        if let BatchResult::AgentFailed { error, .. } = r {
            assert!(error.contains(slot.bead.id.as_str()));
        }
    }
    for slot in &slots {
        let file = format!("{}.partial", slot.bead.id);
        assert!(
            !loom.join(&file).exists(),
            "{} must not appear on the integration branch after agent failure",
            file,
        );
    }
    Ok(())
}

/// Spec contract `[test]` annotation (`specs/harness.md` § Success Criteria —
/// `workspace_persists_on_all_failure_paths`): every non-merged exit
/// (`Failure`, `Retry`, `Blocked`, `Clarify`) leaves the per-bead workspace
/// and its branch on disk so a partial diff survives for recovery and the
/// next `bd ready` re-dispatch reuses the clone. Removing it would force a
/// full re-implementation of an agent that stopped mid-edit.
#[tokio::test]
async fn workspace_persists_on_all_failure_paths() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let beads = vec![
        fake_bead("lm-fail"),
        fake_bead("lm-retry"),
        fake_bead("lm-block"),
        fake_bead("lm-clarify"),
    ];
    let slots = create_worktrees(&client, &label, beads.clone()).await?;

    // Each agent committed partial work on its bead branch before stopping.
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

    let outcomes = [
        AgentOutcome::Failure {
            error: "boom".to_string(),
        },
        AgentOutcome::Retry {
            reason: "stuck".to_string(),
        },
        AgentOutcome::Blocked {
            reason: "needs a human".to_string(),
        },
        AgentOutcome::Clarify {
            question: "which path?".to_string(),
        },
    ];
    let batch_slots: Vec<BatchSlot> = slots
        .iter()
        .zip(outcomes)
        .map(|(w, outcome)| BatchSlot {
            bead: w.bead.clone(),
            worktree: w.worktree.clone(),
            outcome,
        })
        .collect();

    let stub = beads_push_stub(repo.path());
    let outcome = merge_back(&client, &stub, batch_slots).await?;
    assert_eq!(outcome.results.len(), 4);

    // Every result routes to a non-merged variant AND preserves its clone.
    for slot in &slots {
        let result = outcome
            .results
            .iter()
            .find(|r| match r {
                BatchResult::AgentFailed { bead, .. }
                | BatchResult::AgentBlocked { bead, .. }
                | BatchResult::AgentClarify { bead, .. } => *bead == slot.bead.id,
                _ => false,
            })
            .unwrap_or_else(|| panic!("non-merged result for {}", slot.bead.id));
        assert!(
            !matches!(result, BatchResult::Merged { .. }),
            "{} must not merge: {result:?}",
            slot.bead.id,
        );
        assert!(
            slot.worktree.path.exists(),
            "worktree {:?} must persist on a non-merged exit",
            slot.worktree.path,
        );
        let branch = git_capture(
            &slot.worktree.path,
            &["branch", "--list", &slot.worktree.branch],
        )?;
        assert!(
            !branch.trim().is_empty(),
            "bead branch {} must survive in the preserved clone",
            slot.worktree.branch,
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
    let bead = fake_bead("lm-conflict");
    let slots = create_worktrees(&client, &label, vec![bead.clone()]).await?;
    let slot = slots.into_iter().next().expect("one slot");

    let loom = loom_path(repo.path());
    std::fs::write(slot.worktree.path.join("README.md"), "from-bead\n")?;
    git(&slot.worktree.path, &["commit", "-q", "-am", "bead edit"])?;
    std::fs::write(loom.join("README.md"), "from-driver\n")?;
    git(&loom, &["commit", "-q", "-am", "driver edit"])?;

    let batch_slot = BatchSlot {
        bead: slot.bead.clone(),
        worktree: slot.worktree.clone(),
        outcome: AgentOutcome::Success,
    };
    let stub = beads_push_stub(repo.path());
    let outcome = merge_back(&client, &stub, vec![batch_slot]).await?;

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
    let branches = git_capture(&loom, &["branch", "--list", &slot.worktree.branch])?;
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
        fake_bead("lm-zeta"),
        fake_bead("lm-alpha"),
        fake_bead("lm-mu"),
        fake_bead("lm-beta"),
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

    let stub = beads_push_stub(repo.path());
    let outcome = merge_back(&client, &stub, batch_slots).await?;
    let observed: Vec<&str> = outcome
        .results
        .iter()
        .map(|r| match r {
            BatchResult::Merged { bead } => bead.as_str(),
            BatchResult::Conflict { bead, .. } => bead.as_str(),
            BatchResult::AgentFailed { bead, .. } => bead.as_str(),
            BatchResult::AgentBlocked { bead, .. } => bead.as_str(),
            BatchResult::AgentClarify { bead, .. } => bead.as_str(),
            BatchResult::PushFailed { bead, .. } => bead.as_str(),
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

/// Spec gate (`specs/harness.md` § "loop dispatch: per-bead push regression"):
/// every clean merge inside `merge_back_one` MUST push the driver branch
/// to `origin` so per-bead state reaches GitHub before the molecule-end
/// review-phase push. After three beads each merge cleanly, the bare
/// origin's `main` MUST equal the workspace's `main` and the beads-push
/// stub MUST have run exactly three times.
#[tokio::test]
async fn parallel_merge_back_pushes_after_each_merge() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let beads = vec![
        fake_bead("lm-mp.1"),
        fake_bead("lm-mp.2"),
        fake_bead("lm-mp.3"),
    ];
    let slots = create_worktrees(&client, &label, beads.clone()).await?;

    for slot in &slots {
        let file = format!("{}.txt", slot.bead.id);
        std::fs::write(slot.worktree.path.join(&file), b"work\n")?;
        git(&slot.worktree.path, &["add", &file])?;
        git(
            &slot.worktree.path,
            &["commit", "-q", "-m", &format!("work {}", slot.bead.id)],
        )?;
    }

    // Counting `beads-push` stub: increments a file once per invocation.
    let counter_file = repo.path().join("beads-push-count");
    std::fs::write(&counter_file, "0")?;
    let counter_stub = repo.path().join("beads-push-counter.sh");
    std::fs::write(
        &counter_stub,
        format!(
            "#!/bin/sh\nset -eu\nn=$(cat {file})\necho $((n+1)) > {file}\nexit 0\n",
            file = counter_file.to_string_lossy(),
        ),
    )?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&counter_stub, std::fs::Permissions::from_mode(0o755))?;

    let batch_slots: Vec<BatchSlot> = slots
        .iter()
        .map(|w| BatchSlot {
            bead: w.bead.clone(),
            worktree: w.worktree.clone(),
            outcome: AgentOutcome::Success,
        })
        .collect();

    let outcome = merge_back(&client, &counter_stub, batch_slots).await?;
    assert_eq!(outcome.merged_ids().len(), 3);

    // beads-push fired once per successful merge.
    let count: u32 = std::fs::read_to_string(&counter_file)?.trim().parse()?;
    assert_eq!(
        count, 3,
        "beads-push must run once per successful merge in the parallel path (got {count})",
    );

    let origin = loom_driver::git::bare_origin_path(repo.path());
    let loom = loom_path(repo.path());
    let origin_head = git_capture(&origin, &["rev-parse", "main"])?;
    let loom_head = git_capture(&loom, &["rev-parse", "main"])?;
    assert_eq!(
        origin_head.trim(),
        loom_head.trim(),
        "post-merge push must keep origin/main pinned to the integration-branch HEAD",
    );
    for bead in &beads {
        let file = format!("{}.txt", bead.id);
        let listed = git_capture(&origin, &["ls-tree", "-r", "--name-only", "main"])?;
        assert!(
            listed.lines().any(|l| l == file),
            "origin must carry {file} after per-bead push (tree: {listed})",
        );
    }
    Ok(())
}

/// Spec gate (`specs/harness.md` § "loop dispatch: per-bead push regression"):
/// when `git push` fails after a clean merge, `merge_back_one` MUST
/// surface `BatchResult::PushFailed` and preserve the worktree so a
/// transient blip stays recoverable on the next iteration.
#[tokio::test]
async fn parallel_merge_back_preserves_worktree_on_push_failure() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;
    let label = SpecLabel::new("harness");
    let bead = fake_bead("lm-mpfail.1");
    let slots = create_worktrees(&client, &label, vec![bead.clone()]).await?;
    let slot = slots.into_iter().next().expect("one slot");

    // Commit something so merge has work to fold.
    std::fs::write(slot.worktree.path.join("payload.txt"), b"hi\n")?;
    git(&slot.worktree.path, &["add", "payload.txt"])?;
    git(&slot.worktree.path, &["commit", "-q", "-m", "work"])?;

    let loom = loom_path(repo.path());
    git(
        &loom,
        &[
            "remote",
            "set-url",
            "origin",
            "/nonexistent/path/that/cannot/exist.git",
        ],
    )?;

    let stub = beads_push_stub(repo.path());
    let batch_slot = BatchSlot {
        bead: slot.bead.clone(),
        worktree: slot.worktree.clone(),
        outcome: AgentOutcome::Success,
    };
    let outcome = merge_back(&client, &stub, vec![batch_slot]).await?;
    assert_eq!(outcome.results.len(), 1);
    let r = &outcome.results[0];
    let BatchResult::PushFailed {
        bead: bid,
        worktree_path,
        branch,
        error,
    } = r
    else {
        panic!("expected PushFailed, got {r:?}");
    };
    assert_eq!(*bid, bead.id);
    assert_eq!(*branch, slot.worktree.branch);
    assert!(
        error.contains("push failed:"),
        "PushFailed error must signal push failure: {error}",
    );
    assert!(
        worktree_path.exists(),
        "worktree {worktree_path:?} MUST be preserved on push failure",
    );
    Ok(())
}

/// Spec criterion (`specs/harness.md` § Verdict Gate, phases 2 & 4 —
/// `integration_step_verifies_signatures_in_two_passes`): the **parallel**
/// merge-back path runs `git verify-commit` over the fetched bead commits
/// (pass 1) BEFORE folding them onto the integration branch, exactly as the
/// sequential `run_bead` path does. With a signing key resolved in the loom
/// workspace (an `allowed_signers` file present) an UNSIGNED bead commit
/// must route the bead to `loom:blocked` carrying a
/// `signature-verification-failed (worker-side)` reason — NOT merge
/// silently. Before the fix the parallel path called `merge_branch` with
/// zero verification, so an unsigned/tampered commit landed on every host
/// where wrapix signing is configured, bypassing the spec's tamper guard.
#[tokio::test]
async fn integration_step_verifies_signatures_in_two_passes() -> Result<()> {
    let repo = init_repo()?;
    let Some(key) = gen_signing_key(repo.path())? else {
        // No `ssh-keygen` on PATH — the signing path cannot be exercised.
        return Ok(());
    };
    let client = GitClient::open(repo.path())?;
    let loom = loom_path(repo.path());

    // Enable verification in the loom workspace: write the allowed_signers
    // file + ssh-verify config the way `loom init` does when a key resolves.
    loom_driver::git::write_signing_config(&loom, &key)?;
    assert!(
        client.signing_verification_enabled().await?,
        "precondition: verification must be enabled once allowed_signers resolves",
    );

    let label = SpecLabel::new("harness");
    let bead = fake_bead("lm-unsigned.1");
    let slots = create_worktrees(&client, &label, vec![bead.clone()]).await?;
    let slot = slots.into_iter().next().expect("one slot");

    // The shared `git` helper forces `commit.gpgsign=false`, so the bead
    // commit carries no signature — pass 1 must reject it.
    std::fs::write(slot.worktree.path.join("payload.txt"), b"unsigned\n")?;
    git(&slot.worktree.path, &["add", "payload.txt"])?;
    git(
        &slot.worktree.path,
        &["commit", "-q", "-m", "unsigned work"],
    )?;

    let batch_slot = BatchSlot {
        bead: slot.bead.clone(),
        worktree: slot.worktree.clone(),
        outcome: AgentOutcome::Success,
    };
    let stub = beads_push_stub(repo.path());
    let outcome = merge_back(&client, &stub, vec![batch_slot]).await?;

    assert_eq!(outcome.results.len(), 1);
    let r = &outcome.results[0];
    let BatchResult::AgentBlocked { bead: bid, reason } = r else {
        panic!("unsigned commit must route to AgentBlocked, got {r:?}");
    };
    assert_eq!(*bid, bead.id);
    assert!(
        reason.contains("signature-verification-failed (worker-side)"),
        "blocked reason must name the worker-side pass-1 failure: {reason}",
    );

    // The unverified commit was blocked BEFORE the ff-merge, so nothing
    // landed on the integration branch.
    assert!(
        !loom.join("payload.txt").exists(),
        "an unsigned bead commit must NOT reach the integration branch",
    );
    // The transient `loom/<id>` ref was deleted on the block path so a
    // later dispatch's fetch starts clean.
    let leaked = git_capture(&loom, &["branch", "--list", &slot.worktree.branch])?;
    assert!(
        leaked.trim().is_empty(),
        "transient ref {} must be deleted on signature-block (got: {leaked:?})",
        slot.worktree.branch,
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
