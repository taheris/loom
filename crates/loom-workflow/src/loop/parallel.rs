use std::path::{Path, PathBuf};
use std::sync::Arc;

use loom_driver::bd::Bead;
use loom_driver::git::{CreatedWorktree, GitClient, MergeResult};
use loom_driver::identifier::{BeadId, SpecLabel};
use loom_events::DriverKind;
use tokio::task::JoinSet;
use tracing::{info, warn};

use super::driver_emit::BeadEmit;
use super::error::LoopError;
use super::outcome::AgentOutcome;
use super::post_merge_push::push_merged_main_then_beads;

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

    /// Merge succeeded but the post-merge push (`git push` to the GitHub
    /// origin, then the beads-remote sync) failed. The worktree is
    /// **preserved** so a transient failure stays recoverable on the next
    /// iteration — mirroring [`BatchResult::Conflict`] semantics — rather
    /// than letting the local/remote divergence pile up silently.
    PushFailed {
        bead: BeadId,
        worktree_path: PathBuf,
        branch: String,
        error: String,
    },
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

    pub fn push_failed(&self) -> Vec<(BeadId, String)> {
        self.results
            .iter()
            .filter_map(|r| match r {
                BatchResult::PushFailed { bead, error, .. } => Some((bead.clone(), error.clone())),
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
    beads_push_program: &Path,
    spawn: S,
) -> Result<BatchOutcome, LoopError>
where
    S: Fn(WorktreeBead) -> F + Send + Sync + 'static,
    F: std::future::Future<Output = AgentOutcome> + Send + 'static,
{
    run_parallel_batch_with_logs(git, label, beads, beads_push_program, None, spawn).await
}

/// Same as [`run_parallel_batch`] but threads a `logs_root` through to
/// `merge_back_one` so each slot's merge/push/cleanup steps emit
/// driver events into the per-bead `.jsonl` the spawn closure already
/// wrote to. Production callers pass
/// `Some(<workspace>/.wrapix/loom/logs)`; tests that do not exercise
/// the driver-event channel pass `None`.
pub async fn run_parallel_batch_with_logs<S, F>(
    git: &GitClient,
    label: &SpecLabel,
    beads: Vec<Bead>,
    beads_push_program: &Path,
    logs_root: Option<&Path>,
    spawn: S,
) -> Result<BatchOutcome, LoopError>
where
    S: Fn(WorktreeBead) -> F + Send + Sync + 'static,
    F: std::future::Future<Output = AgentOutcome> + Send + 'static,
{
    let slots = create_worktrees(git, label, beads).await?;
    let batch_slots = run_concurrent_spawns(slots, spawn).await;
    merge_back_with_logs(git, beads_push_program, batch_slots, logs_root, label).await
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
) -> Result<Vec<WorktreeBead>, LoopError> {
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
pub async fn merge_back(
    git: &GitClient,
    beads_push_program: &Path,
    slots: Vec<BatchSlot>,
) -> Result<BatchOutcome, LoopError> {
    let mut results = Vec::with_capacity(slots.len());
    for slot in slots {
        let result = merge_back_one(git, beads_push_program, slot, None, None).await?;
        results.push(result);
    }
    Ok(BatchOutcome { results })
}

/// Same as [`merge_back`] but threads `logs_root` + `label` through to
/// every slot's merge/push/cleanup so driver events surface in the
/// per-bead `.jsonl`. Production callers pass
/// `Some(<workspace>/.wrapix/loom/logs)`; tests that do not exercise
/// the driver-event channel pass `None`.
pub async fn merge_back_with_logs(
    git: &GitClient,
    beads_push_program: &Path,
    slots: Vec<BatchSlot>,
    logs_root: Option<&Path>,
    label: &SpecLabel,
) -> Result<BatchOutcome, LoopError> {
    let mut results = Vec::with_capacity(slots.len());
    for slot in slots {
        let result = merge_back_one(git, beads_push_program, slot, logs_root, Some(label)).await?;
        results.push(result);
    }
    Ok(BatchOutcome { results })
}

async fn merge_back_one(
    git: &GitClient,
    beads_push_program: &Path,
    slot: BatchSlot,
    logs_root: Option<&Path>,
    label: Option<&SpecLabel>,
) -> Result<BatchResult, LoopError> {
    let BatchSlot {
        bead,
        worktree,
        outcome,
    } = slot;
    let mut emit = logs_root
        .zip(label)
        .and_then(|(root, lbl)| BeadEmit::for_bead(root, lbl, &bead.id));
    match outcome {
        AgentOutcome::Success => {
            // Push the bead branch from the clone back to its origin so
            // `merge_branch` can fold it into the driver branch. Path A
            // from `specs/harness.md § Worktree Dispatch`: the bead workspace
            // is a standalone clone, so the bead's branch lives only inside
            // the clone until this push exposes it on the main repo.
            git.push_branch_to_origin(&worktree.path, &worktree.branch)
                .await?;
            if let Some(e) = emit.as_mut() {
                e.emit(
                    DriverKind::BeadBranchPushed,
                    &format!("bead branch pushed to driver origin: {}", worktree.branch),
                    serde_json::json!({
                        "bead_id": bead.id.to_string(),
                        "branch": worktree.branch,
                        "worktree_path": worktree.path.to_string_lossy(),
                    }),
                );
            }
            match git.merge_branch(&worktree.branch).await? {
                MergeResult::Ok => {
                    let main_sha = git
                        .head_commit_sha()
                        .await
                        .ok()
                        .unwrap_or_else(|| "unknown".to_string());
                    if let Some(e) = emit.as_mut() {
                        e.emit(
                            DriverKind::MergeOk,
                            &format!("merge ok: {} → main", worktree.branch),
                            serde_json::json!({
                                "bead_id": bead.id.to_string(),
                                "branch": worktree.branch,
                                "main_sha": main_sha,
                            }),
                        );
                    }
                    // Per-bead push of the freshly-merged driver branch to
                    // GitHub plus a beads-remote sync. Without this, beads
                    // closed by the parallel batch only reach `origin` at
                    // the molecule-end review-phase push. Preserve the
                    // workspace on push failure so a transient blip stays
                    // recoverable (mirrors merge-conflict semantics).
                    if let Err(error) =
                        push_merged_main_then_beads(git, git.workdir(), beads_push_program).await
                    {
                        warn!(
                            bead = %bead.id,
                            branch = %worktree.branch,
                            path = %worktree.path.display(),
                            %error,
                            "post-merge push failed — worktree preserved for retry",
                        );
                        if let Some(e) = emit.as_mut() {
                            e.emit(
                                DriverKind::PostMergePushFailed,
                                &format!("post-merge push failed: {error}"),
                                serde_json::json!({
                                    "bead_id": bead.id.to_string(),
                                    "branch": worktree.branch,
                                    "worktree_path": worktree.path.to_string_lossy(),
                                    "error": error.to_string(),
                                }),
                            );
                        }
                        return Ok(BatchResult::PushFailed {
                            bead: bead.id,
                            worktree_path: worktree.path,
                            branch: worktree.branch,
                            error: format!("push failed: {error}"),
                        });
                    }
                    if let Some(e) = emit.as_mut() {
                        e.emit(
                            DriverKind::PostMergePushOk,
                            &format!("post-merge push ok for bead {}", bead.id),
                            serde_json::json!({
                                "bead_id": bead.id.to_string(),
                            }),
                        );
                    }
                    git.remove_worktree(&worktree.path).await?;
                    git.delete_branch(&worktree.branch).await?;
                    if let Some(e) = emit.as_mut() {
                        e.emit(
                            DriverKind::WorktreeCleanupOk,
                            &format!("worktree + branch cleanup ok for bead {}", bead.id),
                            serde_json::json!({
                                "bead_id": bead.id.to_string(),
                                "branch": worktree.branch,
                                "worktree_path": worktree.path.to_string_lossy(),
                            }),
                        );
                    }
                    Ok(BatchResult::Merged { bead: bead.id })
                }
                MergeResult::Conflict { detail } => {
                    warn!(
                        bead = %bead.id,
                        branch = %worktree.branch,
                        path = %worktree.path.display(),
                        detail = %detail,
                        "merge conflict — worktree preserved for inspection",
                    );
                    if let Some(e) = emit.as_mut() {
                        e.emit(
                            DriverKind::MergeConflict,
                            &format!("merge conflict: {}", worktree.branch),
                            serde_json::json!({
                                "bead_id": bead.id.to_string(),
                                "branch": worktree.branch,
                                "worktree_path": worktree.path.to_string_lossy(),
                                "detail": detail,
                            }),
                        );
                    }
                    Ok(BatchResult::Conflict {
                        bead: bead.id,
                        worktree_path: worktree.path,
                        branch: worktree.branch,
                    })
                }
            }
        }
        AgentOutcome::Failure { error }
        | AgentOutcome::InfraPreflight { error }
        | AgentOutcome::InfraMidSession { error }
        | AgentOutcome::UnknownProfile { error } => {
            warn!(bead = %bead.id, %error, "agent failed — cleaning up worktree");
            git.remove_worktree(&worktree.path).await?;
            Ok(BatchResult::AgentFailed {
                bead: bead.id,
                error,
            })
        }
        AgentOutcome::Blocked { reason } => {
            warn!(bead = %bead.id, %reason, "agent emitted LOOM_BLOCKED — cleaning up worktree");
            git.remove_worktree(&worktree.path).await?;
            Ok(BatchResult::AgentBlocked {
                bead: bead.id,
                reason,
            })
        }
        AgentOutcome::Clarify { question } => {
            warn!(bead = %bead.id, %question, "agent emitted LOOM_CLARIFY — cleaning up worktree");
            git.remove_worktree(&worktree.path).await?;
            Ok(BatchResult::AgentClarify {
                bead: bead.id,
                question,
            })
        }
    }
}

#[cfg(test)]
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

        let slots = vec![fake_slot("lm-a"), fake_slot("lm-b"), fake_slot("lm-c")];
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
                    if slot.bead.id.as_str() == "lm-fail" {
                        AgentOutcome::Failure {
                            error: "boom".into(),
                        }
                    } else {
                        AgentOutcome::Success
                    }
                }
            }
        };

        let slots = vec![fake_slot("lm-a"), fake_slot("lm-fail"), fake_slot("lm-c")];
        let mut out = run_concurrent_spawns(slots, spawn).await;
        out.sort_by(|a, b| a.bead.id.as_str().cmp(b.bead.id.as_str()));
        // sorted: lm-a, lm-c, lm-fail
        assert_eq!(out.len(), 3);
        assert_eq!(counter.load(Ordering::SeqCst), 3);
        assert_eq!(out[0].bead.id.as_str(), "lm-a");
        assert!(matches!(out[0].outcome, AgentOutcome::Success));
        assert_eq!(out[1].bead.id.as_str(), "lm-c");
        assert!(matches!(out[1].outcome, AgentOutcome::Success));
        assert_eq!(out[2].bead.id.as_str(), "lm-fail");
        assert!(matches!(out[2].outcome, AgentOutcome::Failure { .. }));
    }
}
