use std::path::PathBuf;
use std::sync::Arc;

use loom_driver::bd::Bead;
use loom_driver::git::{CreatedWorktree, GitClient, MergeResult};
use loom_driver::identifier::{BeadId, SpecLabel};
use tokio::task::JoinSet;
use tracing::{info, warn};

use super::error::RunError;
use super::outcome::AgentOutcome;

/// Pairing of a bead with the worktree that was created for it. Built by
/// [`create_worktrees`] and consumed by [`run_concurrent_spawns`].
#[derive(Debug, Clone)]
pub struct WorktreeBead {
    pub bead: Bead,
    pub worktree: CreatedWorktree,
}

/// One slot's state after the concurrent spawn phase finishes — before the
/// sequential merge-back.
#[derive(Debug, Clone)]
pub struct BatchSlot {
    pub bead: Bead,
    pub worktree: CreatedWorktree,
    pub outcome: AgentOutcome,
}

/// Per-bead result after merge-back. Drives the bd-side cleanup the caller
/// will perform: `Merged` → driver observes the agent's `bd close` (no
/// driver-side close), `Conflict` → mark failed (worktree preserved),
/// `AgentFailed` → re-queue per the retry policy, `AgentBlocked` /
/// `AgentClarify` → apply the matching `loom:*` label with the agent's
/// reason / question as notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchResult {
    /// Agent finished cleanly and the bead branch merged into the driver
    /// branch without conflict. The worktree has been removed.
    Merged { bead: BeadId },

    /// Agent finished cleanly but the merge produced conflicts. The
    /// worktree is **preserved** at `worktree_path` for human inspection
    /// (per the spec — "on merge conflict the worktree is preserved").
    Conflict {
        bead: BeadId,
        worktree_path: PathBuf,
        branch: String,
    },

    /// Agent failed. The worktree branch was deleted; the bead is queued
    /// for retry per the configured policy (the caller owns retry budget
    /// accounting).
    AgentFailed { bead: BeadId, error: String },

    /// Agent emitted `LOOM_BLOCKED`. Worktree + branch were deleted; the
    /// caller applies `loom:blocked` and writes `reason` to notes.
    AgentBlocked { bead: BeadId, reason: String },

    /// Agent emitted `LOOM_CLARIFY`. Worktree + branch were deleted; the
    /// caller applies `loom:clarify` and writes `question` to notes.
    AgentClarify { bead: BeadId, question: String },
}

/// Aggregate outcome of one parallel batch.
#[derive(Debug, Default, Clone)]
pub struct BatchOutcome {
    pub results: Vec<BatchResult>,
}

impl BatchOutcome {
    pub fn merged_ids(&self) -> Vec<BeadId> {
        self.results
            .iter()
            .filter_map(|r| match r {
                BatchResult::Merged { bead } => Some(bead.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn conflict_ids(&self) -> Vec<BeadId> {
        self.results
            .iter()
            .filter_map(|r| match r {
                BatchResult::Conflict { bead, .. } => Some(bead.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn failure_ids(&self) -> Vec<BeadId> {
        self.results
            .iter()
            .filter_map(|r| match r {
                BatchResult::AgentFailed { bead, .. } => Some(bead.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn blocked(&self) -> Vec<(BeadId, String)> {
        self.results
            .iter()
            .filter_map(|r| match r {
                BatchResult::AgentBlocked { bead, reason } => Some((bead.clone(), reason.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn clarified(&self) -> Vec<(BeadId, String)> {
        self.results
            .iter()
            .filter_map(|r| match r {
                BatchResult::AgentClarify { bead, question } => {
                    Some((bead.clone(), question.clone()))
                }
                _ => None,
            })
            .collect()
    }
}

/// Drive one parallel batch end-to-end: create worktrees, spawn agents
/// concurrently via `spawn`, then merge the finished branches back to the
/// driver branch sequentially.
///
/// `spawn` is the per-slot dispatcher — typically a closure that resolves
/// the per-phase backend through the binary's `dispatch` function and runs
/// `wrapix spawn --spawn-config <file> --stdio` inside it. The closure
/// returns an [`AgentOutcome`] so this driver does not need to know which
/// backend ran or whether `LOOM_COMPLETE` / `LOOM_BLOCKED` was the verdict;
/// that translation lives one layer up.
///
/// On any [`GitError`](loom_driver::git::GitError) during worktree creation or
/// merge-back, the function returns immediately and the partial batch is
/// surfaced through the error — slots already merged stay merged, slots not
/// yet merged stay in the worktree and require manual intervention.
pub async fn run_parallel_batch<S, F>(
    git: &GitClient,
    label: &SpecLabel,
    beads: Vec<Bead>,
    spawn: S,
) -> Result<BatchOutcome, RunError>
where
    S: Fn(WorktreeBead) -> F + Send + Sync + 'static,
    F: std::future::Future<Output = AgentOutcome> + Send + 'static,
{
    let slots = create_worktrees(git, label, beads).await?;
    let batch_slots = run_concurrent_spawns(slots, spawn).await;
    merge_back(git, batch_slots).await
}

/// Step 1 of a parallel batch: create one worktree per bead.
///
/// Worktree creation goes through `git worktree add -b loom/<label>/<id>`
/// (handled by [`GitClient::create_worktree`]), so this step is *not*
/// parallelised — git's worktree command serializes against the repo
/// `.git/worktrees/` directory. Running them concurrently buys nothing.
pub async fn create_worktrees(
    git: &GitClient,
    label: &SpecLabel,
    beads: Vec<Bead>,
) -> Result<Vec<WorktreeBead>, RunError> {
    let mut out = Vec::with_capacity(beads.len());
    for bead in beads {
        let wt = git.create_worktree(label, &bead.id).await?;
        info!(bead = %bead.id, path = %wt.path.display(), branch = %wt.branch, "worktree created");
        out.push(WorktreeBead { bead, worktree: wt });
    }
    Ok(out)
}

/// Step 2 of a parallel batch: spawn one agent invocation per worktree
/// **concurrently** via [`tokio::task::JoinSet`], wait for all of them, and
/// collect their per-bead outcomes.
///
/// `spawn` is the per-slot dispatcher. The driver passes a closure that
/// builds a `SpawnConfig` with the worktree path as the workspace mount
/// and runs `wrapix spawn --spawn-config <file> --stdio` against an
/// `AgentBackend` — see [`super::spawn::build_spawn_config`]. Tests pass
/// closures that resolve immediately so the join logic can be exercised
/// without a real container.
pub async fn run_concurrent_spawns<S, F>(slots: Vec<WorktreeBead>, spawn: S) -> Vec<BatchSlot>
where
    S: Fn(WorktreeBead) -> F + Send + Sync + 'static,
    F: std::future::Future<Output = AgentOutcome> + Send + 'static,
{
    let spawn = Arc::new(spawn);
    let mut set: JoinSet<BatchSlot> = JoinSet::new();
    for slot in slots {
        let spawn = Arc::clone(&spawn);
        let bead = slot.bead.clone();
        let worktree = slot.worktree.clone();
        set.spawn(async move {
            let outcome = spawn(WorktreeBead {
                bead: bead.clone(),
                worktree: worktree.clone(),
            })
            .await;
            BatchSlot {
                bead,
                worktree,
                outcome,
            }
        });
    }
    let mut results = Vec::with_capacity(set.len());
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(slot) => results.push(slot),
            Err(e) => warn!(error = %e, "parallel worker join failure"),
        }
    }
    results
}

/// Step 3 of a parallel batch: merge each finished bead back to the driver
/// branch **sequentially** (the spec calls this out — "single-threaded merge
/// avoids index lock contention").
///
/// Per-slot policy:
///
/// - [`AgentOutcome::Success`] + [`MergeResult::Ok`] → remove the worktree
///   and return [`BatchResult::Merged`].
/// - [`AgentOutcome::Success`] + [`MergeResult::Conflict`] → **preserve** the
///   worktree, return [`BatchResult::Conflict`].
/// - [`AgentOutcome::Failure`] → remove the worktree, delete the branch,
///   return [`BatchResult::AgentFailed`] (the caller owns retry accounting).
pub async fn merge_back(git: &GitClient, slots: Vec<BatchSlot>) -> Result<BatchOutcome, RunError> {
    let mut results = Vec::with_capacity(slots.len());
    for slot in slots {
        let result = merge_back_one(git, slot).await?;
        results.push(result);
    }
    Ok(BatchOutcome { results })
}

async fn merge_back_one(git: &GitClient, slot: BatchSlot) -> Result<BatchResult, RunError> {
    let BatchSlot {
        bead,
        worktree,
        outcome,
    } = slot;
    match outcome {
        AgentOutcome::Success => match git.merge_branch(&worktree.branch).await? {
            MergeResult::Ok => {
                git.remove_worktree(&worktree.path).await?;
                git.delete_branch(&worktree.branch).await?;
                Ok(BatchResult::Merged { bead: bead.id })
            }
            MergeResult::Conflict => {
                warn!(
                    bead = %bead.id,
                    branch = %worktree.branch,
                    path = %worktree.path.display(),
                    "merge conflict — worktree preserved for inspection",
                );
                Ok(BatchResult::Conflict {
                    bead: bead.id,
                    worktree_path: worktree.path,
                    branch: worktree.branch,
                })
            }
        },
        AgentOutcome::Failure { error }
        | AgentOutcome::InfraPreflight { error }
        | AgentOutcome::InfraMidSession { error } => {
            warn!(bead = %bead.id, %error, "agent failed — cleaning up worktree");
            git.remove_worktree(&worktree.path).await?;
            git.delete_branch(&worktree.branch).await?;
            Ok(BatchResult::AgentFailed {
                bead: bead.id,
                error,
            })
        }
        AgentOutcome::Blocked { reason } => {
            warn!(bead = %bead.id, %reason, "agent emitted LOOM_BLOCKED — cleaning up worktree");
            git.remove_worktree(&worktree.path).await?;
            git.delete_branch(&worktree.branch).await?;
            Ok(BatchResult::AgentBlocked {
                bead: bead.id,
                reason,
            })
        }
        AgentOutcome::Clarify { question } => {
            warn!(bead = %bead.id, %question, "agent emitted LOOM_CLARIFY — cleaning up worktree");
            git.remove_worktree(&worktree.path).await?;
            git.delete_branch(&worktree.branch).await?;
            Ok(BatchResult::AgentClarify {
                bead: bead.id,
                question,
            })
        }
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use loom_driver::bd::Bead;
    use loom_driver::clock::{Clock, MockClock};
    use loom_driver::git::CreatedWorktree;
    use loom_driver::identifier::BeadId;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;
    use tokio::sync::Barrier;

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

    fn fake_slot(id: &str) -> WorktreeBead {
        WorktreeBead {
            bead: fake_bead(id),
            worktree: CreatedWorktree {
                path: PathBuf::from(format!(".wrapix/worktree/test/{id}")),
                branch: format!("loom/test/{id}"),
            },
        }
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_spawns_overlap_in_wall_clock() {
        // Three slots, each spawn rendezvouses on a barrier then sleeps via
        // the injected `MockClock`. Under `start_paused = true`, tokio
        // auto-advances paused time when every task is blocked on a timer —
        // so concurrent sleeps coalesce into one paused-time window.
        // Sequential dispatch would advance time by `3 * sleep`; concurrent
        // dispatch advances it by `~sleep`. We verify by reading
        // `clock.now()` before and after.
        let sleep = Duration::from_millis(80);
        let barrier = Arc::new(Barrier::new(3));
        let clock: Arc<dyn Clock> = Arc::new(MockClock::new());
        let spawn = {
            let barrier = Arc::clone(&barrier);
            let clock = Arc::clone(&clock);
            move |slot: WorktreeBead| {
                let barrier = Arc::clone(&barrier);
                let clock = Arc::clone(&clock);
                async move {
                    barrier.wait().await;
                    clock.sleep(sleep).await;
                    let _ = slot.bead.id;
                    AgentOutcome::Success
                }
            }
        };

        let slots = vec![fake_slot("wx-a"), fake_slot("wx-b"), fake_slot("wx-c")];
        let start = clock.now();
        let results = run_concurrent_spawns(slots, spawn).await;
        let elapsed = clock.now().saturating_duration_since(start);

        assert_eq!(results.len(), 3);
        assert!(
            elapsed < sleep * 2,
            "expected overlap (< {:?}), got {:?}",
            sleep * 2,
            elapsed,
        );
    }

    #[tokio::test]
    async fn concurrent_spawns_collect_outcomes_for_every_slot() {
        let counter = Arc::new(AtomicU32::new(0));
        let spawn = {
            let counter = Arc::clone(&counter);
            move |slot: WorktreeBead| {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    if slot.bead.id.as_str() == "wx-fail" {
                        AgentOutcome::Failure {
                            error: "boom".into(),
                        }
                    } else {
                        AgentOutcome::Success
                    }
                }
            }
        };

        let slots = vec![fake_slot("wx-a"), fake_slot("wx-fail"), fake_slot("wx-c")];
        let mut out = run_concurrent_spawns(slots, spawn).await;
        out.sort_by(|a, b| a.bead.id.as_str().cmp(b.bead.id.as_str()));
        // sorted: wx-a, wx-c, wx-fail
        assert_eq!(out.len(), 3);
        assert_eq!(counter.load(Ordering::SeqCst), 3);
        assert_eq!(out[0].bead.id.as_str(), "wx-a");
        assert!(matches!(out[0].outcome, AgentOutcome::Success));
        assert_eq!(out[1].bead.id.as_str(), "wx-c");
        assert!(matches!(out[1].outcome, AgentOutcome::Success));
        assert_eq!(out[2].bead.id.as_str(), "wx-fail");
        assert!(matches!(out[2].outcome, AgentOutcome::Failure { .. }));
    }
}
