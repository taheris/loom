use std::path::{Path, PathBuf};
use std::sync::Arc;

use loom_driver::bd::Bead;
use loom_driver::git::{CreatedWorktree, GitClient, RebaseOutcome};
use loom_driver::identifier::{BeadId, SpecLabel};
use loom_events::DriverKind;
use tokio::task::JoinSet;
use tracing::{info, warn};

use super::driver_emit::BeadEmit;
use super::error::LoopError;
use super::outcome::{AgentOutcome, InfraDiagnostic};
use super::runner::{
    CONFLICT_RETRY_LABEL, INFRA_INTERRUPTED_CAUSE, INFRA_PREFLIGHT_CAUSE, UNKNOWN_PROFILE_CAUSE,
    UNKNOWN_RUNTIME_FOR_PROFILE_CAUSE, synthesize_integration_conflict_options,
};
use super::verify::{VerifyPass, verify_pass};
use super::waiting::ActiveBlockers;

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

/// Infrastructure class surfaced by the parallel merge-back path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchInfraFailure {
    /// Pre-stream infra failure before the first canonical agent event.
    Preflight { error: String },
    /// Interrupted infra after at least one canonical agent event.
    Interrupted { error: String },
    /// Static diagnostic that cannot be repaired by transport retry.
    Static { cause: String, error: String },
}

impl BatchInfraFailure {
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Preflight { .. } | Self::Interrupted { .. })
    }

    pub fn diagnostic(&self, attempt: u32, max_attempts: u32) -> InfraDiagnostic {
        match self {
            Self::Preflight { error } => InfraDiagnostic::retryable(
                INFRA_PREFLIGHT_CAUSE,
                "infra-preflight",
                error.clone(),
                attempt,
                max_attempts,
                false,
            ),
            Self::Interrupted { error } => InfraDiagnostic::retryable(
                INFRA_INTERRUPTED_CAUSE,
                "infra-interrupted",
                error.clone(),
                attempt,
                max_attempts,
                true,
            ),
            Self::Static { cause, error } => {
                InfraDiagnostic::static_diagnostic(cause, error.clone())
            }
        }
    }
}

/// Per-bead result after merge-back. Drives the bd-side cleanup the caller
/// will perform: `Merged` → driver observes the agent's `bd close` (no
/// driver-side close), `Conflict` → mark failed (worktree preserved),
/// `AgentFailed` → re-queue per the retry policy, `AgentInfra` → retry or
/// park under `loom:infra`, `AgentBlocked` / `AgentClarify` → apply the
/// matching `loom:*` label with the agent's reason / question as notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchResult {
    /// Agent finished cleanly and the bead branch merged into the driver
    /// branch without conflict. The worktree has been removed.
    Merged { bead: BeadId },

    /// A validated dependency wait. The worktree and branch remain in place;
    /// merge-back and per-bead verification are skipped.
    Waiting {
        bead: BeadId,
        blockers: ActiveBlockers,
    },

    /// The driver-side rebase onto the integration tip conflicted on the
    /// bead's **first** integration attempt (it did not yet carry the
    /// [`CONFLICT_RETRY_LABEL`] marker). The worktree is
    /// **preserved** at `worktree_path` and the caller applies the marker so
    /// the next `loom loop` re-dispatches the bead against the moved tip; a
    /// second conflict escalates to [`BatchResult::AgentClarify`]. This is
    /// the parallel-shaped single-retry budget (`specs/harness.md`
    /// § Verdict Gate phase 3).
    Conflict {
        bead: BeadId,
        worktree_path: PathBuf,
        branch: String,
    },

    /// Agent failed. The bead workspace is **preserved** on disk (any
    /// staged-but-uncommitted diff survives) and no branch is deleted; the
    /// bead is queued for retry per the configured policy (the caller owns
    /// retry budget accounting). The per-bead-close lifecycle reaps the
    /// workspace at `bd close`.
    AgentFailed { bead: BeadId, error: String },

    /// Driver-classified infrastructure failure. The bead workspace is
    /// **preserved**; the caller owns the `[loop.infra]` budget, retry queue,
    /// and `loom:infra` parking.
    AgentInfra {
        bead: BeadId,
        failure: BatchInfraFailure,
    },

    /// Agent emitted `LOOM_BLOCKED`, or a driver-side signature pass
    /// rejected the bead. The bead workspace is **preserved** for recovery;
    /// the caller applies `loom:blocked` and writes `reason` to notes.
    AgentBlocked { bead: BeadId, reason: String },

    /// Agent emitted `LOOM_CLARIFY`, or the driver-side integration
    /// conflict exhausted its single retry (the bead already carried the
    /// [`CONFLICT_RETRY_LABEL`] marker). The bead workspace is
    /// **preserved** for recovery; the caller applies `loom:clarify` and
    /// writes `question` to notes — for the integration-conflict case
    /// `question` is the synthesized `## Options — …` block.
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

    pub fn waiting_ids(&self) -> Vec<BeadId> {
        self.results
            .iter()
            .filter_map(|result| match result {
                BatchResult::Waiting { bead, .. } => Some(bead.clone()),
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

    pub fn infra(&self) -> Vec<(BeadId, BatchInfraFailure)> {
        self.results
            .iter()
            .filter_map(|r| match r {
                BatchResult::AgentInfra { bead, failure } => Some((bead.clone(), failure.clone())),
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
/// `wrix spawn --spawn-config <file> --stdio` inside it. The closure
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
) -> Result<BatchOutcome, LoopError>
where
    S: Fn(WorktreeBead) -> F + Send + Sync + 'static,
    F: std::future::Future<Output = AgentOutcome> + Send + 'static,
{
    run_parallel_batch_with_logs(git, label, beads, None, spawn).await
}

/// Same as [`run_parallel_batch`] but threads a `logs_root` through to
/// `merge_back_one` so each slot's merge/cleanup steps emit
/// driver events into the per-bead `.jsonl` the spawn closure already
/// wrote to. Production callers pass
/// `Some(<workspace>/.loom/logs)`; tests that do not exercise
/// the driver-event channel pass `None`.
pub async fn run_parallel_batch_with_logs<S, F>(
    git: &GitClient,
    label: &SpecLabel,
    beads: Vec<Bead>,
    logs_root: Option<&Path>,
    spawn: S,
) -> Result<BatchOutcome, LoopError>
where
    S: Fn(WorktreeBead) -> F + Send + Sync + 'static,
    F: std::future::Future<Output = AgentOutcome> + Send + 'static,
{
    let slots = create_worktrees(git, label, beads).await?;
    let batch_slots = run_concurrent_spawns(slots, spawn).await;
    merge_back_with_logs(git, batch_slots, logs_root, label).await
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
        // Drop any uncommitted mid-session leftovers from a prior attempt
        // while preserving the bead branch's HEAD (i.e. the agent's prior
        // commits) and the warm caches under `target/` + `.wrix/`. No-op
        // on the first attempt against a freshly-cloned tree.
        git.reset_bead_clone(&wt.path).await?;
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
/// and runs `wrix spawn --spawn-config <file> --stdio` against an
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
/// Per-slot policy (driver-side rebase + ff, A3 — never pushes; origin is
/// reached once per molecule by the push gate):
///
/// - [`AgentOutcome::Success`] + [`RebaseOutcome::Rebased`] (and both
///   signature passes clean) → ff-merge, remove the bead workspace,
///   delete the transient `loom/<id>` ref, return [`BatchResult::Merged`].
/// - [`AgentOutcome::Success`] + [`RebaseOutcome::Conflict`] → **preserve**
///   the bead workspace, delete only the transient `loom/<id>` ref (the
///   rebase already aborted); on the bead's first conflict return
///   [`BatchResult::Conflict`] (caller applies the retry marker), on a
///   second conflict (marker present) escalate to
///   [`BatchResult::AgentClarify`] with the synthesized Options block.
/// - [`AgentOutcome::Failure`] → **preserve** the bead workspace for
///   recovery, delete nothing, return [`BatchResult::AgentFailed`] (the
///   caller owns retry accounting; the per-bead-close lifecycle reaps the
///   workspace at `bd close`).
/// - Infra/profile variants → **preserve** the bead workspace and return
///   [`BatchResult::AgentInfra`] so the caller can apply the `[loop.infra]`
///   retry budget instead of collapsing infrastructure into semantic failure.
pub async fn merge_back(git: &GitClient, slots: Vec<BatchSlot>) -> Result<BatchOutcome, LoopError> {
    let mut results = Vec::with_capacity(slots.len());
    for slot in slots {
        let result = merge_back_one(git, slot, None, None).await?;
        results.push(result);
    }
    Ok(BatchOutcome { results })
}

/// Same as [`merge_back`] but threads `logs_root` + `label` through to
/// every slot's merge/cleanup so driver events surface in the
/// per-bead `.jsonl`. Production callers pass
/// `Some(<workspace>/.loom/logs)`; tests that do not exercise
/// the driver-event channel pass `None`.
pub async fn merge_back_with_logs(
    git: &GitClient,
    slots: Vec<BatchSlot>,
    logs_root: Option<&Path>,
    label: &SpecLabel,
) -> Result<BatchOutcome, LoopError> {
    let mut results = Vec::with_capacity(slots.len());
    for slot in slots {
        let result = merge_back_one(git, slot, logs_root, Some(label)).await?;
        results.push(result);
    }
    Ok(BatchOutcome { results })
}

async fn merge_back_one(
    git: &GitClient,
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
    if let Some(identity) = inferred_terminal_marker(&outcome)
        && let Some(event_sink) = emit.as_mut()
    {
        event_sink.emit(
            DriverKind::MarkerRouted,
            &format!(
                "terminal marker {identity} routed to {} for bead {}",
                super::production::agent_outcome_route(&outcome),
                bead.id,
            ),
            serde_json::json!({
                "source_route": "loop-marker",
                "identity": identity,
                "route": super::production::agent_outcome_route(&outcome),
                "bead_id": bead.id.to_string(),
                "parallel": true,
            }),
        );
    }
    match outcome {
        AgentOutcome::Waiting { blockers } => {
            info!(
                bead = %bead.id,
                blocker_count = blockers.count(),
                path = %worktree.path.display(),
                "dependency wait accepted — preserving bead workspace without merge",
            );
            Ok(BatchResult::Waiting {
                bead: bead.id,
                blockers,
            })
        }
        AgentOutcome::WaitingRequested => Err(LoopError::Bug {
            context: format!(
                "unvalidated LOOM_WAITING reached parallel merge for bead {}",
                bead.id,
            ),
        }),
        AgentOutcome::Success | AgentOutcome::Noop => {
            // A3: the worker never pushes; the driver fetches the bead
            // branch from the bead workspace path into the loom workspace,
            // where `merge_branch` rebases + ff's it onto the integration
            // branch.
            git.fetch_bead_branch(&worktree.path, &bead.id).await?;
            if let Some(e) = emit.as_mut() {
                e.emit(
                    DriverKind::BeadBranchPushed,
                    &format!(
                        "bead branch fetched into loom workspace: {}",
                        worktree.branch
                    ),
                    serde_json::json!({
                        "bead_id": bead.id.to_string(),
                        "branch": worktree.branch,
                        "worktree_path": worktree.path.to_string_lossy(),
                    }),
                );
            }
            // Verify signatures (pass 1) on the fetched worker commits
            // before any rebase — the parallel path mirrors the
            // sequential `run_bead` integration step and the spec recipe
            // (`specs/harness.md` § Verdict Gate phases 2-4) rather than
            // collapsing rebase + ff into a single unverified
            // `merge_branch`. A rejected signature routes the bead to
            // `loom:blocked` (worker-side); `verify_pass` deletes the
            // transient ref so a retry's fetch starts clean.
            let integration_branch = git.integration_branch().to_string();
            let pass1_range = format!("{integration_branch}..{}", worktree.branch);
            if let Some(reason) = verify_pass(
                git,
                emit.as_mut(),
                &bead.id,
                &worktree.branch,
                &pass1_range,
                VerifyPass::Worker,
            )
            .await?
            {
                return Ok(BatchResult::AgentBlocked {
                    bead: bead.id,
                    reason,
                });
            }
            // Rebase onto the integration tip (rerere replays any recorded
            // resolution), then verify pass 2 on the rewritten commits
            // BEFORE the ff-merge so a driver-side signature failure leaves
            // the integration branch untouched.
            match git.rebase_onto_integration(&worktree.branch).await? {
                RebaseOutcome::Rebased => {
                    let pass2_range = format!("{integration_branch}..{}", worktree.branch);
                    if let Some(reason) = verify_pass(
                        git,
                        emit.as_mut(),
                        &bead.id,
                        &worktree.branch,
                        &pass2_range,
                        VerifyPass::Driver,
                    )
                    .await?
                    {
                        return Ok(BatchResult::AgentBlocked {
                            bead: bead.id,
                            reason,
                        });
                    }
                    // Per-bead integration never pushes; origin is reached
                    // once per molecule by the molecule-completion push gate
                    // (`specs/harness.md` § Verdict Gate phase 5). Mirrors the
                    // serial `run_bead` integration step.
                    git.ff_merge_integration(&worktree.branch).await?;
                    let main_sha = git.head_commit_sha().await?.to_string();
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
                RebaseOutcome::Conflict {
                    detail,
                    files,
                    new_base_sha,
                } => {
                    // Integration-conflict recovery, parallel-shaped. The
                    // serial path retries by re-dispatching the agent inside
                    // its in-process loop; a one-shot batch has no agent left
                    // to retry, so the single-retry budget lives on the bead
                    // as the [`CONFLICT_RETRY_LABEL`] marker: a
                    // first conflict marks the bead (caller applies the label)
                    // and preserves the workspace so the next `loom loop`
                    // re-dispatches against the moved tip; a second conflict
                    // (marker already present) escalates to `loom:clarify`
                    // with the synthesized Options block. Mirrors the serial
                    // budget in `process_one_bead` (`specs/harness.md`
                    // § Verdict Gate phase 3).
                    let retry_exhausted = bead
                        .labels
                        .iter()
                        .any(|l| l.as_str() == CONFLICT_RETRY_LABEL);
                    warn!(
                        bead = %bead.id,
                        branch = %worktree.branch,
                        path = %worktree.path.display(),
                        detail = %detail,
                        new_base = %new_base_sha,
                        retry_exhausted,
                        "integration conflict — bead workspace preserved for recovery",
                    );
                    if let Some(e) = emit.as_mut() {
                        e.emit(
                            DriverKind::IntegrationConflict,
                            &format!("integration conflict: {}", worktree.branch),
                            serde_json::json!({
                                "bead_id": bead.id.to_string(),
                                "branch": worktree.branch,
                                "worktree_path": worktree.path.to_string_lossy(),
                                "detail": detail,
                                "new_base_sha": new_base_sha.as_str(),
                                "files": files.iter().map(|f| f.to_string_lossy()).collect::<Vec<_>>(),
                            }),
                        );
                    }
                    // `rebase_onto_integration` already ran `git rebase
                    // --abort` (restoring the integration tip) but that
                    // leaves the transient loom-workspace `loom/<id>` ref
                    // dangling. Delete it unconditionally — mirroring the
                    // sequential conflict arm and clean-merge arm — so the
                    // ref is gone on every exit path (`specs/harness.md`
                    // § Bead Dispatch phase 6). The bead clone keeps its
                    // own copy of the branch until the bead is reaped on
                    // `bd close`; only the loom-workspace ref is removed.
                    git.delete_branch(&worktree.branch).await?;
                    if retry_exhausted {
                        Ok(BatchResult::AgentClarify {
                            bead: bead.id,
                            question: synthesize_integration_conflict_options(
                                &files,
                                &new_base_sha,
                            ),
                        })
                    } else {
                        Ok(BatchResult::Conflict {
                            bead: bead.id,
                            worktree_path: worktree.path,
                            branch: worktree.branch,
                        })
                    }
                }
            }
        }
        // Every non-merged exit preserves the bead workspace (and any
        // staged-but-uncommitted diff) on disk: the per-bead-close lifecycle
        // reaps it at `bd close` via `GitClient::sweep_orphan_bead_clones`,
        // and the next `bd ready` re-dispatch reuses it through the idempotent
        // `create_worktree` + `reset_bead_clone`. Removing it here would
        // destroy an agent's partial work and force a full re-implementation
        // (`specs/harness.md` § Verdict Gate — workspace persists on all
        // failure paths).
        AgentOutcome::Failure { error } | AgentOutcome::ZeroProgress { detail: error } => {
            warn!(bead = %bead.id, %error, "agent failed — worktree preserved for recovery");
            Ok(BatchResult::AgentFailed {
                bead: bead.id,
                error,
            })
        }
        AgentOutcome::InfraPreflight { error } => {
            warn!(bead = %bead.id, %error, "agent hit preflight infra — worktree preserved for retry");
            Ok(BatchResult::AgentInfra {
                bead: bead.id,
                failure: BatchInfraFailure::Preflight { error },
            })
        }
        AgentOutcome::InfraMidSession { error } => {
            warn!(bead = %bead.id, %error, "agent hit interrupted infra — worktree preserved for retry");
            Ok(BatchResult::AgentInfra {
                bead: bead.id,
                failure: BatchInfraFailure::Interrupted { error },
            })
        }
        AgentOutcome::StaticInfra { cause, error } => {
            warn!(bead = %bead.id, %cause, %error, "agent hit static infra — worktree preserved for diagnostic");
            Ok(BatchResult::AgentInfra {
                bead: bead.id,
                failure: BatchInfraFailure::Static { cause, error },
            })
        }
        AgentOutcome::UnknownProfile { error } => {
            warn!(bead = %bead.id, %error, "unknown profile — worktree preserved for diagnostic");
            Ok(BatchResult::AgentInfra {
                bead: bead.id,
                failure: BatchInfraFailure::Static {
                    cause: UNKNOWN_PROFILE_CAUSE.to_string(),
                    error,
                },
            })
        }
        AgentOutcome::UnknownRuntimeForProfile { error } => {
            warn!(bead = %bead.id, %error, "unknown runtime for profile — worktree preserved for diagnostic");
            Ok(BatchResult::AgentInfra {
                bead: bead.id,
                failure: BatchInfraFailure::Static {
                    cause: UNKNOWN_RUNTIME_FOR_PROFILE_CAUSE.to_string(),
                    error,
                },
            })
        }
        AgentOutcome::Retry { reason } => {
            // Parallel mode does not run an in-session retry counter
            // (the sequential `process_one_bead` does), so a worker
            // self-reported `LOOM_RETRY` surfaces as a generic
            // `AgentFailed` here. The next `bd ready` poll re-dispatches
            // the bead; the retry-exhausted escalation lives in the
            // sequential path only.
            warn!(bead = %bead.id, %reason, "agent emitted LOOM_RETRY — worktree preserved for recovery");
            Ok(BatchResult::AgentFailed {
                bead: bead.id,
                error: format!("agent-retry: {reason}"),
            })
        }
        AgentOutcome::Blocked { reason } => {
            warn!(bead = %bead.id, %reason, "agent emitted LOOM_BLOCKED — worktree preserved for recovery");
            Ok(BatchResult::AgentBlocked {
                bead: bead.id,
                reason,
            })
        }
        AgentOutcome::Clarify { question } => {
            warn!(bead = %bead.id, %question, "agent emitted LOOM_CLARIFY — worktree preserved for recovery");
            Ok(BatchResult::AgentClarify {
                bead: bead.id,
                question,
            })
        }
        // These two `AgentOutcome` variants are driver-side integration
        // verdicts, never produced by the spawn closure that yields the
        // per-bead session `outcome` matched here: the parallel path runs
        // its own rebase + two-pass signature verification inside the
        // `Success` branch above and surfaces a failure as
        // `BatchResult::AgentBlocked` directly, not by round-tripping
        // through these variants. The arms are kept defensive — map to
        // `AgentBlocked` so the bead workspace is reaped and the next
        // `bd ready` poll re-dispatches.
        AgentOutcome::IntegrationConflict { new_base_sha, .. } => {
            warn!(bead = %bead.id, "integration conflict in parallel dispatch — cleaning up worktree");
            git.remove_worktree(&worktree.path).await?;
            Ok(BatchResult::AgentBlocked {
                bead: bead.id,
                reason: format!("integration-conflict: rebase onto {new_base_sha} conflicted"),
            })
        }
        AgentOutcome::SignatureVerificationFailed { detail } => {
            warn!(bead = %bead.id, %detail, "signature verification failed in parallel dispatch — cleaning up worktree");
            git.remove_worktree(&worktree.path).await?;
            Ok(BatchResult::AgentBlocked {
                bead: bead.id,
                reason: detail,
            })
        }
    }
}

fn inferred_terminal_marker(outcome: &AgentOutcome) -> Option<&'static str> {
    match outcome {
        AgentOutcome::Success => Some("LOOM_COMPLETE"),
        AgentOutcome::Noop => Some("LOOM_NOOP"),
        AgentOutcome::WaitingRequested | AgentOutcome::Waiting { .. } => Some("LOOM_WAITING"),
        AgentOutcome::Retry { .. } => Some("LOOM_RETRY"),
        AgentOutcome::Blocked { .. } => Some("LOOM_BLOCKED"),
        AgentOutcome::Clarify { .. } => Some("LOOM_CLARIFY"),
        AgentOutcome::Failure { error } if error.contains("LOOM_CONCERN") => Some("LOOM_CONCERN"),
        AgentOutcome::Failure { error } if error.contains("LOOM_COMPLETE") => Some("LOOM_COMPLETE"),
        AgentOutcome::Failure { error } if error.contains("LOOM_NOOP") => Some("LOOM_NOOP"),
        AgentOutcome::Failure { .. }
        | AgentOutcome::InfraPreflight { .. }
        | AgentOutcome::InfraMidSession { .. }
        | AgentOutcome::StaticInfra { .. }
        | AgentOutcome::UnknownProfile { .. }
        | AgentOutcome::UnknownRuntimeForProfile { .. } => Some("missing"),
        AgentOutcome::IntegrationConflict { .. }
        | AgentOutcome::SignatureVerificationFailed { .. }
        | AgentOutcome::ZeroProgress { .. } => None,
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

    #[test]
    fn terminal_marker_inference_covers_marker_failures_and_missing_markers() {
        let cases = [
            (
                AgentOutcome::Failure {
                    error: "wrong-phase-marker: LOOM_CONCERN".to_string(),
                },
                "LOOM_CONCERN",
            ),
            (
                AgentOutcome::Failure {
                    error: "agent emitted LOOM_COMPLETE but exited code 1".to_string(),
                },
                "LOOM_COMPLETE",
            ),
            (
                AgentOutcome::Failure {
                    error: "agent swallowed marker".to_string(),
                },
                "missing",
            ),
        ];
        for (outcome, expected) in cases {
            assert_eq!(inferred_terminal_marker(&outcome), Some(expected));
        }
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

    fn fake_slot(id: &str) -> WorktreeBead {
        WorktreeBead {
            bead: fake_bead(id),
            worktree: CreatedWorktree {
                path: PathBuf::from(format!(".loom/beads/{id}")),
                branch: format!("loom/{id}"),
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

    #[tokio::test]
    async fn parallel_waiting_outcome_preserves_workspace_without_merge() {
        let dir = tempfile::tempdir().expect("temporary repository");
        let mut git = loom_driver::git::init_test_repo_with_integration(dir.path())
            .expect("initialize integration repository");
        git.disable_signing_key_resolution();
        let before = git
            .integration_commit_sha()
            .await
            .expect("read integration tip");
        let label = SpecLabel::new("agent");
        let waiting_bead = fake_bead("lm-wait");
        let sibling_bead = fake_bead("lm-next");
        let waiting_workspace = dir.path().join(".loom/beads/lm-wait");

        let outcome = run_parallel_batch(
            &git,
            &label,
            vec![waiting_bead, sibling_bead],
            |slot| async move {
                if slot.bead.id.as_str() == "lm-wait" {
                    AgentOutcome::Waiting {
                        blockers: ActiveBlockers::new(
                            BeadId::new("lm-blocker").expect("valid blocker id"),
                            Vec::new(),
                        ),
                    }
                } else {
                    AgentOutcome::Success
                }
            },
        )
        .await
        .expect("parallel wait succeeds");

        assert_eq!(
            outcome.waiting_ids(),
            vec![BeadId::new("lm-wait").expect("valid id")]
        );
        assert_eq!(
            outcome.merged_ids(),
            vec![BeadId::new("lm-next").expect("valid id")]
        );
        assert!(
            waiting_workspace.exists(),
            "waiting workspace must be preserved"
        );
        assert_eq!(
            git.integration_commit_sha()
                .await
                .expect("read unchanged integration tip"),
            before,
        );
    }
}
