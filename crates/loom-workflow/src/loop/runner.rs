use std::collections::{HashMap, VecDeque};

use loom_driver::bd::Bead;
use loom_driver::identifier::BeadId;
use loom_events::DriverKind;
use tracing::info;

use loom_gate::{GateFail, GateOutcome, GateSuccess, HandoffEvidence, LoopOutcome, NoGateReason};

use super::error::LoopError;
use super::outcome::{AgentOutcome, BeadResult, InfraDiagnostic};
use super::retry::{RetryDecision, RetryPolicy};

/// Spec-table cause string written to `bd update --notes` when pre-stream
/// infra attempts exhaust their per-bead budget.
pub const INFRA_PREFLIGHT_CAUSE: &str = "infra-preflight";

/// Spec-table cause string written to `bd update --notes` when interrupted
/// infra attempts exhaust their per-bead budget.
pub const INFRA_INTERRUPTED_CAUSE: &str = "infra-interrupted";

/// Spec-table cause string written to `bd update --notes` when a bead's
/// requested `profile:X` label is not declared in the profile-image
/// manifest. Same routing pattern as [`INFRA_PREFLIGHT_CAUSE`]: no retry,
/// the loop continues with the next ready bead.
pub const UNKNOWN_PROFILE_CAUSE: &str = "unknown-profile";

/// Spec-table cause string written when a profile exists but lacks the
/// selected agent runtime in the profile-image manifest.
pub const UNKNOWN_RUNTIME_FOR_PROFILE_CAUSE: &str = "unknown-agent-runtime-for-profile";

/// Spec-table cause string for static spawn-config diagnostics.
pub const INVALID_SPAWN_CONFIG_CAUSE: &str = "invalid-spawn-config";

/// Spec-table cause string for a selected image/runtime missing its agent binary.
pub const MISSING_AGENT_BINARY_CAUSE: &str = "missing-agent-binary";

/// Spec-table cause string for failed dirty-work preservation before cleanup.
pub const WORKSPACE_RECOVERY_FAILED_CAUSE: &str = "workspace-recovery-failed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InfraRetryPolicy {
    pub max_attempts: u32,
}

impl InfraRetryPolicy {
    fn effective_max_attempts(self) -> u32 {
        self.max_attempts.max(1)
    }
}

impl Default for InfraRetryPolicy {
    fn default() -> Self {
        Self { max_attempts: 3 }
    }
}

#[derive(Debug, Default, Clone)]
struct BeadRunState {
    retries_used: u32,
    previous_failure: Option<String>,
    integration_conflict_used: bool,
    infra_attempts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryableInfraClass {
    Preflight,
    Interrupted,
}

impl RetryableInfraClass {
    const fn cause(self) -> &'static str {
        match self {
            Self::Preflight => INFRA_PREFLIGHT_CAUSE,
            Self::Interrupted => INFRA_INTERRUPTED_CAUSE,
        }
    }

    const fn first_event_seen(self) -> bool {
        matches!(self, Self::Interrupted)
    }

    const fn infra_class(self) -> &'static str {
        match self {
            Self::Preflight => "infra-preflight",
            Self::Interrupted => "infra-interrupted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProcessOneResult {
    Terminal(BeadResult),
    RetryInfra,
}

/// Running tally [`run_loop`] threads through the outer loop while the
/// final [`LoopOutcome`] is being assembled. Distinct from `LoopOutcome` —
/// the public surface has no `Default`, holds `gate: GateOutcome`, and is
/// minted only at the end through the sealed [`GateSuccess`] constructor.
#[derive(Debug, Default, Clone)]
struct LoopProgress {
    beads_processed: u32,
    beads_clarified: u32,
    beads_blocked: u32,
    outer_iterations: u32,
    last_evidence: HandoffEvidence,
    partial_without_handoff: bool,
}

/// Side-effect surface the [`run_loop`] driver depends on.
///
/// The trait abstracts the concrete BdClient + AgentBackend + LogSink wiring
/// so the loop logic stays pure-ish and can be exercised under a fake without
/// spawning a real container. The binary wires this to:
///
/// - `next_ready_bead` → `BdClient::list` filtered by ready label
/// - `run_bead` → render template, build SpawnConfig, drive `AgentBackend`,
///   tee `AgentEvent` stream into `LogSink`, parse exit signal
/// - `apply_clarify` → `BdClient::update --add-label loom:clarify`
/// - `apply_blocked` → `BdClient::update --add-label loom:blocked --notes <cause>`
/// - `apply_infra` → `BdClient::update --add-label loom:infra --notes <cause>`
/// - `exec_review` → `tokio::process::Command` invocations of
///   `loom gate verify --diff <molecule.base_commit>..HEAD` then
///   `loom gate review --diff <molecule.base_commit>..HEAD` (FR1
///   molecule-completion handoff; scope is the molecule's own diff,
///   not `--tree`).
///
/// **No `close_bead`.** `bd close` is the agent's responsibility, not the
/// driver's, per `specs/harness.md`'s verdict-gate table where
/// `bd-closed` is treated as an *observable* (the gate checks whether the
/// agent did it). A driver that auto-closes on `exit_code == 0` collapses
/// every marker into `done` and silently masks `LOOM_BLOCKED` /
/// `LOOM_CLARIFY` self-reports — the bug that motivated this trait shape.
pub trait AgentLoopController: Send {
    /// Pull the next ready bead. `deferred` contains bead ids already parked
    /// in the in-memory infra retry queue for this loop invocation.
    /// Returns `None` when no non-deferred ready work is available.
    fn next_ready_bead(
        &mut self,
        deferred: &[BeadId],
    ) -> impl std::future::Future<Output = Result<Option<Bead>, LoopError>> + Send;

    /// Run one agent attempt against `bead`, threading `previous_failure` if
    /// any (the wrapped truncation lives in `templates`).
    fn run_bead(
        &mut self,
        bead: &Bead,
        previous_failure: Option<String>,
    ) -> impl std::future::Future<Output = Result<AgentOutcome, LoopError>> + Send;

    /// Add the `loom:clarify` label. `question` is the agent's clarify
    /// detail (or the last retry's failure body when retries were
    /// exhausted); it is **not** persisted to `bd update --notes` —
    /// per specs/gate.md § "Persistence boundary: agent narrates, agent
    /// persists", the canonical `## Options — …` block lives in bead
    /// state only when the agent writes it there *before* emitting
    /// `LOOM_CLARIFY`. Overwriting that block with the agent's stdout
    /// reason-line would leave `loom inbox`'s queue empty.
    fn apply_clarify(
        &mut self,
        bead: &BeadId,
        question: &str,
    ) -> impl std::future::Future<Output = Result<(), LoopError>> + Send;

    /// Add the `loom:blocked` label and write `cause` (plus any error
    /// detail) to `bd update --notes`. Called when an agent
    /// `LOOM_BLOCKED` self-report or semantic driver failure routes the
    /// bead straight to blocked per the verdict-gate spec.
    fn apply_blocked(
        &mut self,
        bead: &BeadId,
        cause: &str,
        error: &str,
    ) -> impl std::future::Future<Output = Result<(), LoopError>> + Send;

    /// Add the `loom:infra` label, pause the bead, and persist the latest
    /// infra diagnostic details.
    fn apply_infra(
        &mut self,
        bead: &BeadId,
        diagnostic: &InfraDiagnostic,
    ) -> impl std::future::Future<Output = Result<(), LoopError>> + Send;

    /// Molecule-completion handoff (FR1). Invokes `loom gate verify
    /// --diff <molecule.base_commit>..HEAD` followed by `loom gate
    /// review --diff <molecule.base_commit>..HEAD`; scope is the
    /// molecule's own diff (not `--tree`), so push-gate cost is
    /// proportional to the molecule's work. The non-zero exit codes
    /// that signal concerns do not bubble up as errors here — they
    /// drive fix-up beads onto the next outer-loop pass.
    ///
    /// The verify and review exit codes, the review's parsed exit
    /// marker, and the review log path ride out in
    /// [`HandoffEvidence`] so [`run_loop`] can feed them to the sealed
    /// [`GateSuccess`] constructor that asserts the FR9 four-condition
    /// AND.
    fn exec_review(
        &mut self,
    ) -> impl std::future::Future<Output = Result<HandoffEvidence, LoopError>> + Send;

    /// Per-bead gate invoked after the run-phase agent signals
    /// [`AgentOutcome::Success`].
    ///
    /// Spawns the deterministic verify subcommand and returns a typed
    /// [`PerBeadGateOutcome`] the runner maps to done, blocked, or recovery.
    fn exec_per_bead_gate(
        &mut self,
        bead: &BeadId,
    ) -> impl std::future::Future<Output = Result<PerBeadGateOutcome, LoopError>> + Send;

    /// Emit a driver-side event into the controller's event sink. The
    /// run loop fires `retry_dispatch` here when it re-dispatches a bead
    /// after a recoverable failure; production controllers thread an
    /// envelope builder + phase log sink, while test fakes default to a
    /// no-op so most call sites stay terse.
    fn emit_driver_event(
        &mut self,
        _kind: DriverKind,
        _summary: &str,
        _payload: serde_json::Value,
    ) {
    }
}

/// Stable cause string for an agent self-reported `LOOM_BLOCKED`. Pinned at
/// the head of the notes string so `bd show --notes` greps cleanly. The raw
/// reason from the agent follows after a `:` separator (or stands alone if
/// the agent did not provide one).
pub const AGENT_BLOCKED_CAUSE: &str = "agent-blocked";

/// Re-export the spec-table cause string for `loom:blocked` escalation
/// when consecutive `LOOM_RETRY` exits exhaust the `[loop] max_retries`
/// counter. The canonical definition lives in
/// [`crate::review::recovery::RETRY_EXHAUSTED_CAUSE`]
/// (`DriverNoticeCause::RetryExhausted` in `loom-templates`); reusing it
/// here keeps both the recovery-loop exhaustion path and the
/// `process_one_bead` exhaustion path on a single label string.
pub use crate::review::RETRY_EXHAUSTED_CAUSE;

/// Spec-table cause string written to `bd update --notes` when a
/// driver-side per-bead gate detects a structural invariant violation
/// the agent cannot resolve from inside the loop. The bead's run-phase
/// commit is NOT unwound — the integration is already durable; the
/// structural violation surfaces as a labelled bead the operator
/// unblocks via `loom inbox`.
pub const MINT_STRUCTURAL_VIOLATION_CAUSE: &str = "mint-structural-violation";

/// Spec-table cause string written to `bd update --notes` when the
/// per-bead integration step's `git verify-commit` rejects a fetched
/// (pass 1, worker-side) or rebased (pass 2, driver-side) commit. The
/// bead routes straight to `loom:blocked` with no retry — re-running the
/// agent cannot re-sign existing commits; the operator investigates the
/// signing setup (wrix container for pass 1, loom-workspace gitconfig +
/// key resolution for pass 2). Per `specs/harness.md` § Verdict Gate.
pub const SIGNATURE_VERIFICATION_FAILED_CAUSE: &str = "signature-verification-failed";

/// Spec-table cause string written when the driver-side rebase of a bead
/// branch onto the integration branch conflicts textually and the single
/// integration-conflict retry also conflicts. The bead escalates to
/// `loom:clarify` carrying a synthesized Options block. Per
/// `specs/harness.md` § Verdict Gate.
pub const INTEGRATION_CONFLICT_CAUSE: &str = "integration-conflict";

/// Cause written when an agent reports success but the outer bead branch
/// contributes no commit to the integration line.
pub const ZERO_PROGRESS_CAUSE: &str = "zero-progress";

/// Non-terminal bead label tracking the parallel path's single
/// integration-conflict retry budget. The serial path holds this counter
/// in `process_one_bead`'s stack, but a one-shot `--parallel` batch has no
/// agent left to retry once `merge_back` runs, so the budget lives on the
/// bead instead: a first conflict applies this label (the bead stays ready
/// and is re-dispatched against the moved tip next `loom loop`), a second
/// conflict — observed by `merge_back_one` reading the label off the
/// re-fetched bead — escalates to `loom:clarify`. Unlike `loom:clarify` /
/// `loom:blocked` it does **not** pair with a `status=blocked` transition,
/// so `bd ready` keeps surfacing the bead for its one retry.
pub const CONFLICT_RETRY_LABEL: &str = "loom:conflict";

/// Synthesize the canonical `## Options — …` block a driver-applied
/// `integration-conflict` clarify bead carries when the single
/// integration-conflict retry also conflicts. Satisfies the Options
/// Format Contract (`specs/gate.md` § *Options Format Contract*): a
/// `## Options — <summary>` heading plus two `### Option N — <title>`
/// subsections (resolve-in-bead-clone and abandon-the-bead), each naming
/// its cost, so `loom inbox` can render the SUMMARY column and resolve
/// integer fast-replies. The driver is the author here (not the agent),
/// so the per-bead path persists this block to bead state before
/// applying `loom:clarify` (see the production `apply_clarify`).
pub fn synthesize_integration_conflict_options(
    files: &[std::path::PathBuf],
    new_base_sha: &loom_driver::git::GitOid,
) -> String {
    let file_list = if files.is_empty() {
        "(no unmerged paths reported)".to_string()
    } else {
        files
            .iter()
            .map(|f| format!("`{}`", f.display()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "## Options — integration-conflict needs human resolution\n\
         \n\
         The driver-side rebase onto the integration tip `{sha}` conflicted \
         twice (the single automatic retry is exhausted). Conflicting files: \
         {files}.\n\
         \n\
         ### Option 1 — Resolve in the bead clone\n\
         `cd .loom/beads/{{bead-id}}`, `git rebase {sha}`, resolve the \
         conflicts by hand, and re-commit on the bead branch. Cost: manual \
         git work in the preserved bead workspace; the next `loom loop` pass \
         re-attempts integration from the resolved branch.\n\
         \n\
         ### Option 2 — Abandon the bead\n\
         Close the bead without integrating (`bd close`) and re-decompose the \
         work against the moved integration tip. Cost: the bead's commits are \
         discarded; any still-needed work must be re-planned into fresh beads.\n",
        sha = new_base_sha.as_str(),
        files = file_list,
    )
}

/// Outcome of [`AgentLoopController::exec_per_bead_gate`]. Routes per
/// `specs/gate.md` § *Per-diff stage checks* / `specs/harness.md`
/// § *Functional* — the runner's per-bead state machine consumes this
/// after [`AgentOutcome::Success`] to decide between Done, Blocked, or
/// re-entering the agent retry loop with the gate's error detail as
/// `previous_failure`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerBeadGateOutcome {
    /// `loom gate verify --diff <pre-integration-head>..HEAD` exited 0.
    /// The bead is done.
    Clean,
    /// The per-bead gate saw a structural invariant violation that the
    /// agent cannot resolve from inside the loop. Routes to
    /// [`BeadResult::Blocked`] with cause
    /// [`MINT_STRUCTURAL_VIOLATION_CAUSE`]; the bead's run-phase commit
    /// is NOT unwound — the integration is already durable. `detail`
    /// carries operator-facing diagnostics for `bd update --notes`.
    StructuralViolation { detail: String },
    /// `loom gate verify --diff <pre-integration-head>..HEAD` exited non-zero.
    /// The failure routes through the existing per-bead recovery loop bounded by
    /// `RetryPolicy::max_retries`: `detail` is threaded as
    /// `previous_failure` into the next agent attempt; exhaustion
    /// routes to [`BeadResult::Blocked`] with the `retry-exhausted`
    /// cause.
    Recovery { detail: String },
}

/// Run the per-bead loop with the default infra budget.
pub async fn run_loop<C: AgentLoopController>(
    controller: &mut C,
    policy: RetryPolicy,
    max_iterations: u32,
) -> Result<LoopOutcome, LoopError> {
    run_loop_with_infra_policy(
        controller,
        policy,
        InfraRetryPolicy::default(),
        max_iterations,
    )
    .await
}

/// Run the per-bead loop.
///
/// The function is deliberately not generic over `RetryPolicy` (the policy is
/// a small `Copy` value) but it is generic over [`AgentLoopController`] so the
/// binary and tests can supply different concrete impls. Returns when the
/// molecule-completion handoff produced no new ready beads (push gate fired
/// clean or molecule fully stuck), the initial ready queue is empty, or the
/// outer-loop counter reached `max_iterations` per FR1 (each pass = process
/// ready queue + invoke `exec_review`).
///
/// Infra retry state is driver-memory only: it lives on the stack of this
/// single invocation and is **not** persisted. A new `loom loop` starts with a
/// fresh per-bead budget per spec §"Verdict Gate · Infra failures bypass the
/// gate".
pub async fn run_loop_with_infra_policy<C: AgentLoopController>(
    controller: &mut C,
    policy: RetryPolicy,
    infra_policy: InfraRetryPolicy,
    max_iterations: u32,
) -> Result<LoopOutcome, LoopError> {
    let mut progress = LoopProgress::default();
    let mut bead_states: HashMap<BeadId, BeadRunState> = HashMap::new();
    let mut infra_retry_queue: VecDeque<Bead> = VecDeque::new();
    let mut stalled_at_max_iterations = false;
    'outer: loop {
        let mut beads_this_pass: u32 = 0;
        let mut handoff_this_pass = false;
        loop {
            let deferred = deferred_ids(&infra_retry_queue);
            let bead = match controller.next_ready_bead(&deferred).await? {
                Some(b) => b,
                None => match infra_retry_queue.pop_front() {
                    Some(b) => b,
                    None => break,
                },
            };

            let state = bead_states.entry(bead.id.clone()).or_default();
            let result = process_one_bead(controller, &bead, policy, infra_policy, state).await?;

            match result {
                ProcessOneResult::RetryInfra => {
                    infra_retry_queue.push_back(bead);
                }
                ProcessOneResult::Terminal(result) => {
                    bead_states.remove(&bead.id);
                    progress.beads_processed += 1;
                    beads_this_pass += 1;
                    match result {
                        BeadResult::Done => {
                            handoff_this_pass = true;
                        }
                        BeadResult::Noop => {}
                        BeadResult::Clarified { note } => {
                            controller.apply_clarify(&bead.id, &note).await?;
                            progress.beads_clarified += 1;
                        }
                        BeadResult::Blocked { cause, error } => {
                            controller.apply_blocked(&bead.id, &cause, &error).await?;
                            progress.beads_blocked += 1;
                        }
                        BeadResult::Infra { diagnostic } => {
                            controller.apply_infra(&bead.id, &diagnostic).await?;
                            progress.beads_blocked += 1;
                        }
                    }
                }
            }
        }

        if beads_this_pass == 0 {
            if progress.outer_iterations > 0 {
                info!(
                    outer_iterations = progress.outer_iterations,
                    "loom loop: outer loop exiting — no new ready beads after handoff",
                );
            } else {
                info!("loom loop: outer loop exiting — no ready beads");
            }
            break 'outer;
        }

        if !handoff_this_pass {
            info!(
                beads_this_pass,
                outer_iterations = progress.outer_iterations,
                "loom loop: outer loop exiting — ready work produced no integrated commits for handoff",
            );
            progress.partial_without_handoff = true;
            break 'outer;
        }

        if progress.outer_iterations >= max_iterations {
            info!(
                outer_iterations = progress.outer_iterations,
                max_iterations, "loom loop: outer-loop counter exhausted",
            );
            stalled_at_max_iterations = true;
            break 'outer;
        }

        match controller.exec_review().await {
            Ok(evidence) => {
                progress.last_evidence = evidence;
            }
            Err(e) => {
                let question = e.to_string();
                match e {
                    LoopError::MoleculeMissingBaseCommit { id }
                    | LoopError::MoleculeMissingBaseCommitNoParentMetadata { id, .. } => {
                        let epic_id = BeadId::new(&id).map_err(|_| LoopError::Bug {
                            context: format!(
                                "missing-base_commit error carries malformed bead id: {id}",
                            ),
                        })?;
                        controller.apply_clarify(&epic_id, &question).await?;
                    }
                    other => return Err(other),
                }
            }
        }
        progress.outer_iterations += 1;
    }
    Ok(finalize(progress, stalled_at_max_iterations))
}

fn deferred_ids(queue: &VecDeque<Bead>) -> Vec<BeadId> {
    queue.iter().map(|bead| bead.id.clone()).collect()
}

/// Build the final [`LoopOutcome`] from the running tally + final handoff
/// evidence. Mints the sealed [`GateSuccess`] through its `pub(crate)`
/// constructor; falls back to [`GateOutcome::Fail`] / [`GateOutcome::NoGate`]
/// per the spec's exit-code table.
fn finalize(progress: LoopProgress, stalled_at_max_iterations: bool) -> LoopOutcome {
    let LoopProgress {
        beads_processed,
        beads_clarified,
        beads_blocked,
        outer_iterations,
        last_evidence,
        partial_without_handoff,
    } = progress;

    let gate = if outer_iterations == 0 || partial_without_handoff {
        let reason = if beads_processed == 0 {
            NoGateReason::NoBeadsReady
        } else {
            NoGateReason::SelectionPartial
        };
        GateOutcome::NoGate {
            beads_processed,
            reason,
        }
    } else if stalled_at_max_iterations {
        GateOutcome::Fail(GateFail::stalled(outer_iterations))
    } else {
        match GateSuccess::new(&last_evidence, outer_iterations) {
            Ok(success) => GateOutcome::Success(success),
            Err(fail) => GateOutcome::Fail(fail),
        }
    };

    LoopOutcome {
        beads_processed,
        beads_clarified,
        beads_blocked,
        outer_iterations,
        gate,
    }
}

/// Run a single bead through the retry state machine until it reaches a
/// terminal result or one retryable infra attempt needs to move to the
/// run-loop's tail queue.
async fn process_one_bead<C: AgentLoopController>(
    controller: &mut C,
    bead: &Bead,
    policy: RetryPolicy,
    infra_policy: InfraRetryPolicy,
    state: &mut BeadRunState,
) -> Result<ProcessOneResult, LoopError> {
    loop {
        match controller
            .run_bead(bead, state.previous_failure.clone())
            .await?
        {
            AgentOutcome::Noop => return terminal(BeadResult::Noop),
            AgentOutcome::Success => match controller.exec_per_bead_gate(&bead.id).await? {
                PerBeadGateOutcome::Clean => return terminal(BeadResult::Done),
                PerBeadGateOutcome::StructuralViolation { detail } => {
                    return terminal(BeadResult::Blocked {
                        cause: MINT_STRUCTURAL_VIOLATION_CAUSE.to_string(),
                        error: detail,
                    });
                }
                PerBeadGateOutcome::Recovery { detail } => {
                    let exhausted_detail = detail.clone();
                    match policy.decide(state.retries_used, detail) {
                        RetryDecision::Retry {
                            previous_failure: pf,
                        } => {
                            state.retries_used += 1;
                            emit_retry_dispatch(controller, bead, state.retries_used, policy);
                            state.previous_failure = Some(pf);
                        }
                        RetryDecision::GiveUp => {
                            return terminal(BeadResult::Blocked {
                                cause: RETRY_EXHAUSTED_CAUSE.to_string(),
                                error: exhausted_detail,
                            });
                        }
                    }
                }
            },
            AgentOutcome::Failure { error } => {
                let exhausted_detail = error.clone();
                match policy.decide(state.retries_used, error) {
                    RetryDecision::Retry {
                        previous_failure: pf,
                    } => {
                        state.retries_used += 1;
                        emit_retry_dispatch(controller, bead, state.retries_used, policy);
                        state.previous_failure = Some(pf);
                    }
                    RetryDecision::GiveUp => {
                        return terminal(BeadResult::Blocked {
                            cause: RETRY_EXHAUSTED_CAUSE.to_string(),
                            error: exhausted_detail,
                        });
                    }
                }
            }
            AgentOutcome::Retry { reason } => {
                match policy.decide(state.retries_used, reason.clone()) {
                    RetryDecision::Retry {
                        previous_failure: pf,
                    } => {
                        state.retries_used += 1;
                        controller.emit_driver_event(
                            DriverKind::RetryDispatch,
                            &format!(
                                "retry dispatch (LOOM_RETRY) — attempt {attempt}/{max} for bead {bead_id}",
                                attempt = state.retries_used,
                                max = policy.max_retries,
                                bead_id = bead.id,
                            ),
                            serde_json::json!({
                                "bead_id": bead.id.to_string(),
                                "attempt": state.retries_used,
                                "max_attempts": policy.max_retries,
                                "cause": "agent-retry",
                            }),
                        );
                        state.previous_failure = Some(pf);
                    }
                    RetryDecision::GiveUp => {
                        return terminal(BeadResult::Blocked {
                            cause: RETRY_EXHAUSTED_CAUSE.to_string(),
                            error: reason,
                        });
                    }
                }
            }
            AgentOutcome::IntegrationConflict {
                files,
                new_base_sha,
            } => {
                if state.integration_conflict_used {
                    return terminal(BeadResult::Clarified {
                        note: synthesize_integration_conflict_options(&files, &new_base_sha),
                    });
                }
                state.integration_conflict_used = true;
                controller.emit_driver_event(
                    DriverKind::RetryDispatch,
                    &format!(
                        "integration-conflict retry — single attempt for bead {bead_id}",
                        bead_id = bead.id,
                    ),
                    serde_json::json!({
                        "bead_id": bead.id.to_string(),
                        "cause": INTEGRATION_CONFLICT_CAUSE,
                        "new_base_sha": new_base_sha.as_str(),
                    }),
                );
                state.previous_failure = Some(format!(
                    "{INTEGRATION_CONFLICT_CAUSE}: rebase onto {} conflicted",
                    new_base_sha.as_str(),
                ));
            }
            AgentOutcome::SignatureVerificationFailed { detail } => {
                return terminal(BeadResult::Blocked {
                    cause: SIGNATURE_VERIFICATION_FAILED_CAUSE.to_string(),
                    error: detail,
                });
            }
            AgentOutcome::ZeroProgress { detail } => {
                let error = zero_progress_recovery_detail(&detail);
                let exhausted_detail = error.clone();
                match policy.decide(state.retries_used, error) {
                    RetryDecision::Retry {
                        previous_failure: pf,
                    } => {
                        state.retries_used += 1;
                        emit_retry_dispatch(controller, bead, state.retries_used, policy);
                        state.previous_failure = Some(pf);
                    }
                    RetryDecision::GiveUp => {
                        return terminal(BeadResult::Blocked {
                            cause: RETRY_EXHAUSTED_CAUSE.to_string(),
                            error: exhausted_detail,
                        });
                    }
                }
            }
            AgentOutcome::Blocked { reason } => {
                return terminal(BeadResult::Blocked {
                    cause: AGENT_BLOCKED_CAUSE.to_string(),
                    error: reason,
                });
            }
            AgentOutcome::Clarify { question } => {
                return terminal(BeadResult::Clarified { note: question });
            }
            AgentOutcome::InfraPreflight { error } => {
                return Ok(handle_retryable_infra(
                    controller,
                    bead,
                    state,
                    infra_policy,
                    RetryableInfraClass::Preflight,
                    error,
                ));
            }
            AgentOutcome::StaticInfra { cause, error } => {
                let diagnostic = InfraDiagnostic::static_diagnostic(&cause, error);
                emit_infra_failure(controller, bead, &diagnostic);
                return terminal(BeadResult::Infra { diagnostic });
            }
            AgentOutcome::UnknownProfile { error } => {
                let diagnostic = InfraDiagnostic::static_diagnostic(UNKNOWN_PROFILE_CAUSE, error);
                emit_infra_failure(controller, bead, &diagnostic);
                return terminal(BeadResult::Infra { diagnostic });
            }
            AgentOutcome::UnknownRuntimeForProfile { error } => {
                let diagnostic =
                    InfraDiagnostic::static_diagnostic(UNKNOWN_RUNTIME_FOR_PROFILE_CAUSE, error);
                emit_infra_failure(controller, bead, &diagnostic);
                return terminal(BeadResult::Infra { diagnostic });
            }
            AgentOutcome::InfraMidSession { error } => {
                return Ok(handle_retryable_infra(
                    controller,
                    bead,
                    state,
                    infra_policy,
                    RetryableInfraClass::Interrupted,
                    error,
                ));
            }
        }
    }
}

fn terminal(result: BeadResult) -> Result<ProcessOneResult, LoopError> {
    Ok(ProcessOneResult::Terminal(result))
}

fn zero_progress_recovery_detail(detail: &str) -> String {
    if detail.is_empty() {
        ZERO_PROGRESS_CAUSE.to_string()
    } else {
        format!("{ZERO_PROGRESS_CAUSE}: {detail}")
    }
}

fn emit_retry_dispatch<C: AgentLoopController>(
    controller: &mut C,
    bead: &Bead,
    attempt: u32,
    policy: RetryPolicy,
) {
    controller.emit_driver_event(
        DriverKind::RetryDispatch,
        &format!(
            "retry dispatch — attempt {attempt}/{max} for bead {bead_id}",
            max = policy.max_retries,
            bead_id = bead.id,
        ),
        serde_json::json!({
            "bead_id": bead.id.to_string(),
            "attempt": attempt,
            "max_attempts": policy.max_retries,
        }),
    );
}

fn handle_retryable_infra<C: AgentLoopController>(
    controller: &mut C,
    bead: &Bead,
    state: &mut BeadRunState,
    infra_policy: InfraRetryPolicy,
    class: RetryableInfraClass,
    error: String,
) -> ProcessOneResult {
    state.infra_attempts += 1;
    let max_attempts = infra_policy.effective_max_attempts();
    let diagnostic = InfraDiagnostic::retryable(
        class.cause(),
        class.infra_class(),
        error,
        state.infra_attempts,
        max_attempts,
        class.first_event_seen(),
    );
    emit_infra_failure(controller, bead, &diagnostic);
    if state.infra_attempts >= max_attempts {
        ProcessOneResult::Terminal(BeadResult::Infra { diagnostic })
    } else {
        ProcessOneResult::RetryInfra
    }
}

fn emit_infra_failure<C: AgentLoopController>(
    controller: &mut C,
    bead: &Bead,
    diagnostic: &InfraDiagnostic,
) {
    controller.emit_driver_event(
        DriverKind::InfraFailure,
        &format!(
            "infra failure — {cause} for bead {bead_id}",
            cause = diagnostic.cause,
            bead_id = bead.id,
        ),
        serde_json::json!({
            "bead_id": bead.id.to_string(),
            "phase": "loop",
            "first_event_seen": diagnostic.first_event_seen,
            "attempt": diagnostic.attempt,
            "max_attempts": diagnostic.max_attempts,
            "infra_class": diagnostic.infra_class,
            "cause": diagnostic.cause,
            "error": diagnostic.error,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::bd::{Bead, Label};
    use loom_driver::identifier::BeadId;
    use loom_gate::GateFailReason;
    use std::collections::VecDeque;

    /// Capturing fake controller. Drives [`run_loop`] without touching real
    /// bd / agent / review binaries.
    ///
    /// `closed` is deliberately absent: the driver no longer calls
    /// `bd close` on dispatched beads (closure is the agent's
    /// responsibility per spec). Tests verify Done by exclusion: a bead
    /// processed without entries in `clarified` or `blocked` reached Done.
    #[derive(Default)]
    struct FakeController {
        ready_queue: VecDeque<Bead>,
        agent_outcomes: VecDeque<AgentOutcome>,
        run_calls: Vec<(BeadId, Option<String>)>,
        clarified: Vec<(BeadId, String)>,
        blocked: Vec<(BeadId, String, String)>,
        infra: Vec<(BeadId, InfraDiagnostic)>,
        review_calls: u32,
        /// Beads pushed onto `ready_queue` on each `exec_review` call. One
        /// entry per call; an empty entry means the handoff produced no
        /// fix-ups (e.g., push gate fired clean). Excess `exec_review`
        /// calls beyond the scripted plan inject nothing.
        review_injects: VecDeque<Vec<Bead>>,
        /// Scripted evidence each successive `exec_review` call returns.
        /// Tests that exercise the gate-outcome path push here to control
        /// what `GateSuccess::new` sees.
        review_evidence: VecDeque<HandoffEvidence>,
        /// Scripted errors each successive `exec_review` call surfaces
        /// before consulting `review_injects` / `review_evidence`. Tests
        /// that exercise the run-loop's tolerance of handoff failures
        /// (e.g. `MoleculeMissingBaseCommit`) push here so a single call
        /// can fail while subsequent calls fall through to the
        /// happy-path script.
        review_errors: VecDeque<LoopError>,
        /// Per-bead gate outcomes scripted by tests that exercise the
        /// post-Success integration step. Empty queue defaults to
        /// `PerBeadGateOutcome::Clean` so legacy tests that don't
        /// exercise the gate path keep their original "Success → Done"
        /// shape.
        per_bead_gate_outcomes: VecDeque<PerBeadGateOutcome>,
        /// Bead ids each `exec_per_bead_gate` call was invoked against,
        /// in dispatch order. Tests assert on this to confirm the
        /// post-Success step actually fired for a given bead.
        per_bead_gate_calls: Vec<BeadId>,
        driver_events: Vec<(String, String, serde_json::Value)>,
    }

    fn write_gate_log(path: &std::path::Path) {
        let verify = loom_gate::GateRun::successful_verify(
            "origin/main..HEAD".to_string(),
            "tree-a".to_string(),
            "config-a".to_string(),
            path.to_path_buf(),
            vec![loom_gate::HookCoverage {
                id: "pre-push".to_string(),
                entry: "loom gate verify --diff @{u}..HEAD".to_string(),
            }],
        );
        loom_gate::append_gate_run_lifecycle_events(path, &verify)
            .expect("write verify gate events");
        let review = loom_gate::GateRun::successful_review(
            "origin/main..HEAD".to_string(),
            "tree-a".to_string(),
            "config-a".to_string(),
            path.to_path_buf(),
            loom_protocol::gate::ExitSignal::Complete,
        );
        loom_gate::append_gate_run_lifecycle_events(path, &review)
            .expect("write review gate events");
    }

    fn typed_evidence(path: &std::path::Path) -> HandoffEvidence {
        let runs = loom_gate::parse_gate_runs_from_jsonl(path);
        let mut evidence = HandoffEvidence::from_runs(runs);
        evidence.molecule_state = loom_gate::MoleculeState::Clean;
        evidence
    }

    impl AgentLoopController for FakeController {
        async fn next_ready_bead(
            &mut self,
            deferred: &[BeadId],
        ) -> Result<Option<Bead>, LoopError> {
            let Some(index) = self
                .ready_queue
                .iter()
                .position(|bead| !deferred.iter().any(|id| id == &bead.id))
            else {
                return Ok(None);
            };
            Ok(self.ready_queue.remove(index))
        }

        async fn run_bead(
            &mut self,
            bead: &Bead,
            previous_failure: Option<String>,
        ) -> Result<AgentOutcome, LoopError> {
            self.run_calls.push((bead.id.clone(), previous_failure));
            Ok(self
                .agent_outcomes
                .pop_front()
                .unwrap_or(AgentOutcome::Success))
        }

        async fn apply_clarify(&mut self, bead: &BeadId, question: &str) -> Result<(), LoopError> {
            self.clarified.push((bead.clone(), question.to_string()));
            Ok(())
        }

        async fn apply_blocked(
            &mut self,
            bead: &BeadId,
            cause: &str,
            error: &str,
        ) -> Result<(), LoopError> {
            self.blocked
                .push((bead.clone(), cause.to_string(), error.to_string()));
            Ok(())
        }

        async fn apply_infra(
            &mut self,
            bead: &BeadId,
            diagnostic: &InfraDiagnostic,
        ) -> Result<(), LoopError> {
            self.infra.push((bead.clone(), diagnostic.clone()));
            Ok(())
        }

        async fn exec_per_bead_gate(
            &mut self,
            bead: &BeadId,
        ) -> Result<PerBeadGateOutcome, LoopError> {
            self.per_bead_gate_calls.push(bead.clone());
            Ok(self
                .per_bead_gate_outcomes
                .pop_front()
                .unwrap_or(PerBeadGateOutcome::Clean))
        }

        async fn exec_review(&mut self) -> Result<HandoffEvidence, LoopError> {
            self.review_calls += 1;
            if let Some(err) = self.review_errors.pop_front() {
                return Err(err);
            }
            if let Some(fixups) = self.review_injects.pop_front() {
                for b in fixups {
                    self.ready_queue.push_back(b);
                }
            }
            Ok(self.review_evidence.pop_front().unwrap_or_default())
        }

        fn emit_driver_event(
            &mut self,
            kind: DriverKind,
            summary: &str,
            payload: serde_json::Value,
        ) {
            self.driver_events
                .push((kind.as_wire().to_string(), summary.to_string(), payload));
        }
    }

    fn bead(id: &str, labels: &[&str]) -> Bead {
        Bead {
            id: BeadId::new(id).expect("valid bead id"),
            title: format!("title for {id}"),
            description: "desc".into(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: labels.iter().map(|s| Label::new(*s)).collect(),
            parent: None,
            metadata: Default::default(),
            notes: None,
        }
    }

    #[tokio::test]
    async fn loop_runs_handoff_after_ready_queue_drains() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        assert_eq!(summary.beads_processed, 1);
        assert_eq!(c.run_calls.len(), 1);
        assert!(c.clarified.is_empty());
        assert!(c.blocked.is_empty());
        assert_eq!(c.review_calls, 1, "successful work must run the push gate");
        Ok(())
    }

    #[tokio::test]
    async fn noop_outcome_finishes_without_per_bead_gate() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-noop", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Noop);

        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        assert_eq!(summary.beads_processed, 1);
        assert_eq!(c.review_calls, 0, "noop does not run the push gate");
        assert!(matches!(
            summary.gate,
            GateOutcome::NoGate {
                reason: NoGateReason::SelectionPartial,
                ..
            }
        ));
        assert!(c.per_bead_gate_calls.is_empty());
        assert!(c.clarified.is_empty());
        assert!(c.blocked.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn blocked_only_pass_finishes_without_handoff() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-blocked", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Blocked {
            reason: "waiting on operator".to_string(),
        });

        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        assert_eq!(summary.beads_processed, 1);
        assert_eq!(summary.beads_blocked, 1);
        assert_eq!(c.review_calls, 0, "blocked work has no integration to push");
        assert!(matches!(
            summary.gate,
            GateOutcome::NoGate {
                reason: NoGateReason::SelectionPartial,
                ..
            }
        ));
        Ok(())
    }

    #[tokio::test]
    async fn continuous_loops_until_molecule_complete() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.ready_queue.push_back(bead("lm-2", &[]));
        c.ready_queue.push_back(bead("lm-3", &[]));
        for _ in 0..3 {
            c.agent_outcomes.push_back(AgentOutcome::Success);
        }

        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        assert_eq!(summary.beads_processed, 3);
        // All three reach Done; driver does not call bd close.
        assert!(c.clarified.is_empty());
        assert!(c.blocked.is_empty());
        assert!(summary.outer_iterations >= 1);
        Ok(())
    }

    #[tokio::test]
    async fn loop_execs_review_after_molecule_work_completes() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        assert_eq!(summary.beads_processed, 1);
        assert!(summary.outer_iterations >= 1);
        assert_eq!(c.review_calls, 1);
        Ok(())
    }

    #[tokio::test]
    async fn no_ready_beads_returns_no_gate_without_review() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;
        assert_eq!(summary.outer_iterations, 0);
        assert_eq!(c.review_calls, 0);
        Ok(())
    }

    #[tokio::test]
    async fn failed_bead_retries_with_previous_failure_then_blocks() -> Result<(), LoopError> {
        // max_retries = 2 → attempts = initial + 2 retries = 3 failures triggers retry-exhausted.
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        for i in 0..3 {
            c.agent_outcomes.push_back(AgentOutcome::Failure {
                error: format!("err-{i}"),
            });
        }

        let summary = run_loop(&mut c, RetryPolicy { max_retries: 2 }, 10).await?;

        assert_eq!(c.run_calls.len(), 3, "initial + 2 retries");
        // Attempt 1 has no previous_failure.
        assert_eq!(c.run_calls[0].1, None);
        // Attempts 2 and 3 carry the prior error verbatim.
        assert_eq!(c.run_calls[1].1.as_deref(), Some("err-0"));
        assert_eq!(c.run_calls[2].1.as_deref(), Some("err-1"));

        assert!(c.clarified.is_empty());
        assert_eq!(c.blocked.len(), 1);
        assert_eq!(c.blocked[0].0, BeadId::new("lm-1").expect("valid"));
        assert_eq!(c.blocked[0].1, RETRY_EXHAUSTED_CAUSE);
        assert_eq!(c.blocked[0].2, "err-2");
        assert_eq!(summary.beads_blocked, 1);
        Ok(())
    }

    /// Spec criterion (`specs/templates.md` § Typed `PreviousFailure`
    /// + `specs/harness.md` § Marker definitions): a worker phase
    /// emitting `LOOM_RETRY` consumes one slot in
    /// `[loop] max_retries`, threads the verbatim reason into the next
    /// attempt's `previous_failure`, and emits a `retry_dispatch`
    /// driver event tagged with the `agent-retry` cause so a replay
    /// surface can distinguish the LOOM_RETRY path from generic
    /// `Failure` retries without re-deriving it from the reason body.
    #[tokio::test]
    async fn agent_retry_consumes_max_retries_slot_and_threads_reason() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Retry {
            reason: "cwd unlinked mid-session".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let _summary = run_loop(&mut c, RetryPolicy { max_retries: 2 }, 10).await?;

        assert_eq!(c.run_calls.len(), 2, "Retry consumes one max_retries slot");
        assert_eq!(c.run_calls[0].1, None);
        assert_eq!(
            c.run_calls[1].1.as_deref(),
            Some("cwd unlinked mid-session"),
            "second attempt threads the LOOM_RETRY reason verbatim",
        );
        // Done — Retry succeeded on retry, no escalation.
        assert!(c.clarified.is_empty());
        assert!(c.blocked.is_empty());
        let kinds: Vec<&str> = c.driver_events.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(kinds, vec!["retry_dispatch"]);
        assert_eq!(c.driver_events[0].2["cause"].as_str(), Some("agent-retry"));
        Ok(())
    }

    /// Spec criterion (`specs/harness.md` § Marker definitions):
    /// exhausting `[loop] max_retries` on consecutive `LOOM_RETRY`
    /// exits labels the bead `loom:blocked` with cause
    /// `retry-exhausted`, matching generic driver-detected retry exhaustion.
    #[tokio::test]
    async fn consecutive_agent_retry_exhaustion_routes_to_loom_blocked_retry_exhausted()
    -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        // max_retries = 2 → initial + 2 retries = 3 consecutive Retry
        // outcomes triggers the retry-exhausted escalation.
        for i in 0..3 {
            c.agent_outcomes.push_back(AgentOutcome::Retry {
                reason: format!("retry-reason-{i}"),
            });
        }

        let summary = run_loop(&mut c, RetryPolicy { max_retries: 2 }, 10).await?;

        assert_eq!(
            c.run_calls.len(),
            3,
            "initial + 2 retries before exhaustion"
        );
        assert!(
            c.clarified.is_empty(),
            "retry-exhausted does NOT route through clarify",
        );
        assert_eq!(c.blocked.len(), 1);
        assert_eq!(c.blocked[0].0, BeadId::new("lm-1").expect("valid"));
        assert_eq!(c.blocked[0].1, RETRY_EXHAUSTED_CAUSE);
        assert!(
            c.blocked[0].2.contains("retry-reason-2"),
            "blocked notes carry the final retry reason: {:?}",
            c.blocked[0].2,
        );
        assert_eq!(summary.beads_blocked, 1);
        Ok(())
    }

    #[tokio::test]
    async fn complete_with_empty_diff_routes_to_zero_progress() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::ZeroProgress {
            detail: "preserved workspace".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(&mut c, RetryPolicy { max_retries: 2 }, 10).await?;

        assert_eq!(c.run_calls.len(), 2, "zero-progress re-enters recovery");
        assert_eq!(
            c.run_calls[1].1.as_deref(),
            Some("zero-progress: preserved workspace"),
        );
        assert!(c.blocked.is_empty());
        assert_eq!(summary.beads_blocked, 0);
        Ok(())
    }

    #[tokio::test]
    async fn retry_succeeds_within_budget_reaches_done() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Failure {
            error: "boom".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(&mut c, RetryPolicy { max_retries: 2 }, 10).await?;

        assert_eq!(c.run_calls.len(), 2);
        assert_eq!(c.run_calls[1].1.as_deref(), Some("boom"));
        // Done — driver does not close, no clarify, no blocked.
        assert!(c.clarified.is_empty());
        assert!(c.blocked.is_empty());
        assert_eq!(summary.beads_clarified, 0);
        Ok(())
    }

    /// Every retry inside the run loop emits a `retry_dispatch` driver
    /// event carrying the bead id + attempt count, so a replay surface
    /// can show which retry round triggered the next dispatch without
    /// re-deriving it from `previous_failure` heuristics.
    #[tokio::test]
    async fn retry_emits_retry_dispatch_driver_event() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Failure {
            error: "err-0".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Failure {
            error: "err-1".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let _ = run_loop(&mut c, RetryPolicy { max_retries: 3 }, 10).await?;

        let kinds: Vec<&str> = c.driver_events.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["retry_dispatch", "retry_dispatch"],
            "two retries → two retry_dispatch events; success is not announced",
        );
        let first = &c.driver_events[0];
        assert_eq!(first.2["bead_id"].as_str(), Some("lm-1"));
        assert_eq!(first.2["attempt"].as_u64(), Some(1));
        assert_eq!(first.2["max_attempts"].as_u64(), Some(3));
        Ok(())
    }

    #[tokio::test]
    async fn preflight_eof_retries_then_surfaces_infra_not_semantic_blocked()
    -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::InfraPreflight {
            error: "pre-stream EOF".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::InfraPreflight {
            error: "pre-stream EOF again".into(),
        });

        let summary = run_loop_with_infra_policy(
            &mut c,
            RetryPolicy { max_retries: 2 },
            InfraRetryPolicy { max_attempts: 2 },
            10,
        )
        .await?;

        assert_eq!(c.run_calls.len(), 2, "initial attempt + one infra retry");
        assert!(c.blocked.is_empty(), "infra must not apply loom:blocked");
        assert_eq!(c.infra.len(), 1);
        assert_eq!(c.infra[0].0, BeadId::new("lm-1").expect("valid"));
        let diagnostic = &c.infra[0].1;
        assert_eq!(diagnostic.cause, INFRA_PREFLIGHT_CAUSE);
        assert_eq!(diagnostic.infra_class, "infra-preflight");
        assert_eq!(diagnostic.attempt, Some(2));
        assert_eq!(diagnostic.max_attempts, Some(2));
        assert_eq!(diagnostic.first_event_seen, Some(false));
        assert!(diagnostic.error.contains("again"));
        assert_eq!(summary.beads_blocked, 1);
        Ok(())
    }

    #[tokio::test]
    async fn static_dispatch_failures_surface_as_infra_without_retry() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue
            .push_back(bead("lm-bad", &["profile:nonexistent"]));
        c.ready_queue.push_back(bead("lm-ok", &["profile:base"]));
        c.agent_outcomes.push_back(AgentOutcome::UnknownProfile {
            error: "requested profile:nonexistent not declared; manifest declares: profile:base"
                .into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(&mut c, RetryPolicy { max_retries: 2 }, 10).await?;

        assert_eq!(c.run_calls.len(), 2);
        assert_eq!(c.run_calls[0].0, BeadId::new("lm-bad").expect("valid"));
        assert_eq!(c.run_calls[1].0, BeadId::new("lm-ok").expect("valid"));
        assert_eq!(c.run_calls[0].1, None);
        assert!(
            c.blocked.is_empty(),
            "static infra must not be semantic blocked"
        );
        assert_eq!(c.infra.len(), 1);
        assert_eq!(c.infra[0].0, BeadId::new("lm-bad").expect("valid"));
        let diagnostic = &c.infra[0].1;
        assert_eq!(diagnostic.cause, UNKNOWN_PROFILE_CAUSE);
        assert_eq!(diagnostic.infra_class, "static");
        assert_eq!(diagnostic.attempt, None);
        assert!(diagnostic.error.contains("profile:nonexistent"));
        assert!(diagnostic.error.contains("profile:base"));
        assert_eq!(summary.beads_blocked, 1);
        Ok(())
    }

    #[tokio::test]
    async fn missing_runtime_for_profile_surfaces_as_infra_without_retry() -> Result<(), LoopError>
    {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-bad", &["profile:rust"]));
        c.agent_outcomes
            .push_back(AgentOutcome::UnknownRuntimeForProfile {
                error: "profile:rust lacks runtime direct; declared: claude, pi".into(),
            });

        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        assert_eq!(c.run_calls.len(), 1);
        assert!(c.blocked.is_empty());
        assert_eq!(c.infra.len(), 1);
        assert_eq!(c.infra[0].1.cause, UNKNOWN_RUNTIME_FOR_PROFILE_CAUSE);
        assert_eq!(c.infra[0].1.infra_class, "static");
        assert!(c.infra[0].1.error.contains("runtime direct"));
        assert_eq!(summary.beads_blocked, 1);
        Ok(())
    }

    #[tokio::test]
    async fn static_infra_outcome_surfaces_without_retry() -> Result<(), LoopError> {
        for (cause, error) in [
            (INVALID_SPAWN_CONFIG_CAUSE, "spawn config omitted image_ref"),
            (
                MISSING_AGENT_BINARY_CAUSE,
                "agent process exited with code 127",
            ),
            (
                WORKSPACE_RECOVERY_FAILED_CAUSE,
                "git stash failed before cleanup",
            ),
        ] {
            let mut c = FakeController::default();
            c.ready_queue.push_back(bead("lm-static", &[]));
            c.agent_outcomes.push_back(AgentOutcome::StaticInfra {
                cause: cause.to_string(),
                error: error.to_string(),
            });

            let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

            assert_eq!(c.run_calls.len(), 1, "{cause} must not retry");
            assert!(c.blocked.is_empty(), "{cause} must not be loom:blocked");
            assert_eq!(c.infra.len(), 1, "{cause} must park as loom:infra");
            assert_eq!(c.infra[0].1.cause, cause);
            assert_eq!(c.infra[0].1.infra_class, "static");
            assert_eq!(c.infra[0].1.attempt, None);
            assert_eq!(c.infra[0].1.error, error);
            assert_eq!(summary.beads_blocked, 1);
        }
        Ok(())
    }

    #[tokio::test]
    async fn partial_stream_eof_classifies_interrupted_infra() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::InfraMidSession {
            error: "stream ended after text_delta".into(),
        });

        let summary = run_loop_with_infra_policy(
            &mut c,
            RetryPolicy::default(),
            InfraRetryPolicy { max_attempts: 1 },
            10,
        )
        .await?;

        assert_eq!(c.run_calls.len(), 1);
        assert!(c.blocked.is_empty());
        assert_eq!(c.infra.len(), 1);
        let diagnostic = &c.infra[0].1;
        assert_eq!(diagnostic.cause, INFRA_INTERRUPTED_CAUSE);
        assert_eq!(diagnostic.first_event_seen, Some(true));
        assert_eq!(diagnostic.attempt, Some(1));
        assert_eq!(diagnostic.max_attempts, Some(1));
        assert_eq!(summary.beads_blocked, 1);
        Ok(())
    }

    #[tokio::test]
    async fn infra_failure_driver_event_payload_carries_stream_diagnostics() -> Result<(), LoopError>
    {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::InfraMidSession {
            error: "exit 137".into(),
        });

        let _summary = run_loop_with_infra_policy(
            &mut c,
            RetryPolicy::default(),
            InfraRetryPolicy { max_attempts: 1 },
            10,
        )
        .await?;

        let event = c
            .driver_events
            .iter()
            .find(|(kind, _, _)| kind == "infra_failure")
            .expect("infra_failure event emitted");
        assert_eq!(event.2["phase"].as_str(), Some("loop"));
        assert_eq!(event.2["first_event_seen"].as_bool(), Some(true));
        assert_eq!(event.2["attempt"].as_u64(), Some(1));
        assert_eq!(event.2["max_attempts"].as_u64(), Some(1));
        assert_eq!(event.2["infra_class"].as_str(), Some("infra-interrupted"));
        assert_eq!(event.2["cause"].as_str(), Some(INFRA_INTERRUPTED_CAUSE));
        assert_eq!(event.2["error"].as_str(), Some("exit 137"));
        Ok(())
    }

    #[tokio::test]
    async fn infra_failures_round_robin_per_bead_without_cooldown() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-a", &[]));
        c.ready_queue.push_back(bead("lm-b", &[]));
        c.agent_outcomes.push_back(AgentOutcome::InfraPreflight {
            error: "a transient".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop_with_infra_policy(
            &mut c,
            RetryPolicy::default(),
            InfraRetryPolicy { max_attempts: 2 },
            10,
        )
        .await?;

        let order: Vec<String> = c.run_calls.iter().map(|(id, _)| id.to_string()).collect();
        assert_eq!(order, vec!["lm-a", "lm-b", "lm-a"]);
        assert!(c.infra.is_empty());
        assert!(c.blocked.is_empty());
        assert_eq!(summary.beads_processed, 2);
        Ok(())
    }

    #[tokio::test]
    async fn infra_midsession_retry_succeeds_within_budget() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::InfraMidSession {
            error: "stdout closed early".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(&mut c, RetryPolicy { max_retries: 2 }, 10).await?;

        assert_eq!(c.run_calls.len(), 2);
        assert!(c.clarified.is_empty());
        assert!(c.blocked.is_empty());
        assert!(c.infra.is_empty());
        assert_eq!(summary.beads_blocked, 0);
        Ok(())
    }

    #[tokio::test]
    async fn infra_retry_counter_does_not_consume_max_retries() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::InfraMidSession {
            error: "kernel oom".into(),
        });
        for i in 0..3 {
            c.agent_outcomes.push_back(AgentOutcome::Failure {
                error: format!("agent-err-{i}"),
            });
        }

        let summary = run_loop(&mut c, RetryPolicy { max_retries: 2 }, 10).await?;

        assert_eq!(c.run_calls.len(), 4);
        assert_eq!(c.run_calls[0].1, None);
        assert_eq!(c.run_calls[1].1, None);
        assert_eq!(c.run_calls[2].1.as_deref(), Some("agent-err-0"));
        assert_eq!(c.run_calls[3].1.as_deref(), Some("agent-err-1"));
        assert!(c.clarified.is_empty());
        assert!(c.infra.is_empty());
        assert_eq!(c.blocked.len(), 1);
        assert_eq!(c.blocked[0].0, BeadId::new("lm-1").expect("valid"));
        assert_eq!(c.blocked[0].1, RETRY_EXHAUSTED_CAUSE);
        assert_eq!(c.blocked[0].2, "agent-err-2");
        assert_eq!(summary.beads_blocked, 1);
        Ok(())
    }

    #[tokio::test]
    async fn prior_session_complete_not_overwritten_by_later_preflight_eof() -> Result<(), LoopError>
    {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Success);
        c.per_bead_gate_outcomes
            .push_back(PerBeadGateOutcome::Recovery {
                detail: "verify failed".into(),
            });
        c.agent_outcomes.push_back(AgentOutcome::InfraPreflight {
            error: "retry spawn EOF".into(),
        });

        let summary = run_loop_with_infra_policy(
            &mut c,
            RetryPolicy { max_retries: 2 },
            InfraRetryPolicy { max_attempts: 1 },
            10,
        )
        .await?;

        assert_eq!(c.run_calls.len(), 2);
        assert_eq!(c.run_calls[1].1.as_deref(), Some("verify failed"));
        assert!(c.blocked.is_empty());
        assert_eq!(c.infra.len(), 1);
        assert_eq!(c.infra[0].1.cause, INFRA_PREFLIGHT_CAUSE);
        assert_eq!(summary.beads_blocked, 1);
        Ok(())
    }

    #[tokio::test]
    async fn fresh_loop_retries_loom_infra_beads_with_fresh_budget() -> Result<(), LoopError> {
        let mut first = FakeController::default();
        first.ready_queue.push_back(bead("lm-1", &["loom:infra"]));
        first
            .agent_outcomes
            .push_back(AgentOutcome::InfraPreflight {
                error: "eof".into(),
            });
        let _ = run_loop_with_infra_policy(
            &mut first,
            RetryPolicy::default(),
            InfraRetryPolicy { max_attempts: 1 },
            10,
        )
        .await?;
        assert_eq!(first.infra.len(), 1);

        let mut second = FakeController::default();
        second.ready_queue.push_back(bead("lm-1", &["loom:infra"]));
        second.agent_outcomes.push_back(AgentOutcome::Success);
        let summary = run_loop_with_infra_policy(
            &mut second,
            RetryPolicy::default(),
            InfraRetryPolicy { max_attempts: 1 },
            10,
        )
        .await?;

        assert_eq!(second.run_calls.len(), 1);
        assert!(second.infra.is_empty());
        assert_eq!(summary.beads_processed, 1);
        Ok(())
    }

    /// FR1 outer loop. After the molecule-completion handoff produces a
    /// fix-up bead, `run_loop` MUST re-poll `bd ready` and process it —
    /// not break after the first `exec_review` call. The push gate fires
    /// clean (no fix-ups) only after the second handoff, at which point
    /// the loop exits via stall detection. Both passes consume one
    /// `[loop] max_iterations` slot.
    #[tokio::test]
    async fn continuous_outer_loop_processes_fix_up_bead_then_exits_on_stall()
    -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-initial", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Success);
        // First handoff injects a fix-up bead; second handoff produces nothing
        // (push gate clean), so the outer loop stalls and exits.
        c.review_injects
            .push_back(vec![bead("lm-fixup", &["loom:fixup"])]);
        c.review_injects.push_back(vec![]);
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        assert_eq!(c.run_calls.len(), 2, "initial + fix-up processed");
        assert_eq!(c.run_calls[0].0, BeadId::new("lm-initial").expect("valid"),);
        assert_eq!(c.run_calls[1].0, BeadId::new("lm-fixup").expect("valid"));
        assert_eq!(summary.beads_processed, 2);
        assert_eq!(
            c.review_calls, 2,
            "one handoff per pass (initial + fix-up pass)",
        );
        assert_eq!(summary.outer_iterations, 2);
        assert!(c.clarified.is_empty());
        assert!(c.blocked.is_empty());
        Ok(())
    }

    /// FR1 outer-loop bound. When every handoff continues to produce fresh
    /// fix-up beads, the loop MUST stop after `max_iterations` passes
    /// rather than spinning forever — the spec calls this out as
    /// "counter exhaustion" as an exit condition.
    #[tokio::test]
    async fn continuous_outer_loop_bounded_by_max_iterations() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-0", &[]));
        // Three passes scripted: each handoff injects one more fix-up bead.
        // With max_iterations = 3 the loop processes 3 fix-ups (passes 2-4)
        // plus the initial pass — but only 3 exec_review calls fire.
        for i in 1..=5 {
            c.review_injects
                .push_back(vec![bead(&format!("lm-{i}"), &[])]);
        }
        // Agent always succeeds.
        for _ in 0..6 {
            c.agent_outcomes.push_back(AgentOutcome::Success);
        }

        let summary = run_loop(&mut c, RetryPolicy::default(), 3).await?;

        // Pass 1 processes lm-0; exec_review 1 injects lm-1.
        // Pass 2 processes lm-1; exec_review 2 injects lm-2.
        // Pass 3 processes lm-2; exec_review 3 injects lm-3.
        // Pass 4 processes lm-3; counter (3) reached → no exec_review 4 → break.
        assert_eq!(summary.outer_iterations, 3);
        assert_eq!(c.review_calls, 3);
        assert_eq!(summary.beads_processed, 4);
        assert!(matches!(
            summary.gate,
            GateOutcome::Fail(GateFail {
                reason: GateFailReason::StalledMaxIterations,
                ..
            })
        ));
        Ok(())
    }

    /// Per `specs/templates.md` § Attempt counter, the per-bead
    /// attempt counter resets on fresh bead dispatch. A fix-up bead
    /// emitted by the review handoff carries no retry state from the
    /// failing bead that spawned it; its first `run_bead` call must
    /// thread `previous_failure = None` so the rendered prompt's
    /// `attempt` starts at zero.
    #[tokio::test]
    async fn fix_up_bead_starts_at_attempt_zero() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-orig", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Failure {
            error: "first failure".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Failure {
            error: "second failure".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);
        c.review_injects
            .push_back(vec![bead("lm-fixup", &["loom:fixup"])]);
        c.review_injects.push_back(vec![]);
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let _ = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        let fixup_id = BeadId::new("lm-fixup").expect("valid bead id");
        let fixup_calls: Vec<&(BeadId, Option<String>)> = c
            .run_calls
            .iter()
            .filter(|(id, _)| *id == fixup_id)
            .collect();
        assert_eq!(
            fixup_calls.len(),
            1,
            "fix-up bead dispatched exactly once: {:?}",
            c.run_calls,
        );
        assert!(
            fixup_calls[0].1.is_none(),
            "fix-up bead's first dispatch must carry no previous_failure \
             (proving attempt=0 in the rendered prompt): {:?}",
            fixup_calls[0].1,
        );
        Ok(())
    }

    /// Regression: a hand-authored epic missing `loom.base_commit`
    /// surfaces `LoopError::MoleculeMissingBaseCommit` from
    /// `exec_review`. `run_loop` MUST route the diagnostic through
    /// `apply_clarify` on the epic so the operator can repair the
    /// metadata via `loom inbox`, rather than bubbling the error out and
    /// killing the entire loop process. After the clarify lands the loop
    /// re-polls `bd ready` (now skipping the parked epic via the status
    /// filter) and exits cleanly via stall detection.
    #[tokio::test]
    async fn missing_molecule_base_commit_clarifies_epic_instead_of_propagating()
    -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Success);
        c.review_errors
            .push_back(LoopError::MoleculeMissingBaseCommit {
                id: "lm-mol.1".to_string(),
            });

        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        assert_eq!(
            c.review_calls, 1,
            "exec_review fires after work drains and routes the epic clarify",
        );
        assert_eq!(
            c.clarified.len(),
            1,
            "molecule epic must be clarified, not propagated",
        );
        assert_eq!(c.clarified[0].0, BeadId::new("lm-mol.1").expect("valid"));
        let question = &c.clarified[0].1;
        assert!(
            question.contains("bd update lm-mol.1 --set-metadata loom.base_commit="),
            "clarify body must carry the `bd update` hint verbatim: {question:?}",
        );
        assert!(
            question.contains("no parent to inherit from"),
            "clarify body must surface the no-parent diagnostic: {question:?}",
        );
        assert_eq!(summary.beads_processed, 1);
        assert_eq!(
            summary.outer_iterations, 1,
            "the failed handoff still consumes one outer-loop slot so the stall check fires next pass",
        );
        Ok(())
    }

    /// Companion regression: when the molecule's epic has a parent that
    /// also lacks `loom.base_commit`, `fetch_molecule_base_commit` surfaces
    /// the distinct `MoleculeMissingBaseCommitNoParentMetadata` variant —
    /// the diagnostic names the parent so the operator's first repair hop
    /// is unambiguous. `run_loop` must route this body through
    /// `apply_clarify` on the child epic too.
    #[tokio::test]
    async fn missing_molecule_base_commit_no_parent_metadata_clarifies_epic()
    -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Success);
        c.review_errors
            .push_back(LoopError::MoleculeMissingBaseCommitNoParentMetadata {
                id: "lm-child.7".to_string(),
                parent: "lm-epic.3".to_string(),
            });

        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        assert_eq!(c.review_calls, 1);
        assert_eq!(c.clarified.len(), 1);
        assert_eq!(
            c.clarified[0].0,
            BeadId::new("lm-child.7").expect("valid"),
            "clarify lands on the molecule epic (the bead id carried by the error), not on the parent",
        );
        let question = &c.clarified[0].1;
        assert!(
            question.contains("bd update lm-child.7 --set-metadata loom.base_commit="),
            "clarify body must carry the `bd update` hint scoped to the child: {question:?}",
        );
        assert!(
            question.contains("lm-epic.3"),
            "clarify body must name the parent so the operator can fix the epic first: {question:?}",
        );
        assert_eq!(summary.beads_processed, 1);
        assert_eq!(summary.outer_iterations, 1);
        Ok(())
    }

    /// FR1 outer-loop stall. A fully-clarified (or fully-stuck) molecule
    /// MUST exit on the second pass: the first pass drains ready work,
    /// invokes `exec_review`, and the second pass observes no new fix-ups.
    /// No spurious extra `exec_review` fires after the stall trigger.
    #[tokio::test]
    async fn continuous_outer_loop_exits_on_stall_when_no_fixups_appear() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Success);
        c.review_injects.push_back(vec![]);

        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        assert_eq!(summary.beads_processed, 1);
        assert_eq!(
            c.review_calls, 1,
            "one handoff fires; the stall blocks a second",
        );
        assert_eq!(summary.outer_iterations, 1);
        Ok(())
    }

    /// Spec criterion (`specs/harness.md` § Loop Outcome Types): the
    /// binary's exit code is a pure function of the `GateOutcome`
    /// variant — `Success` and `NoGate` exit 0; `Fail` exits non-zero.
    /// This test pins the mapping at the workflow boundary by walking
    /// `run_loop` through three paths — empty queue → `NoGate`,
    /// stalled max_iterations → `Fail`, scripted-success evidence →
    /// `Success` — and asserts on the variant each produces. The
    /// binary's `exit_code_for_gate` consumes the same `GateOutcome`
    /// so as long as this test holds, the exit code does too.
    #[tokio::test]
    async fn loom_loop_exit_code_is_function_of_gate_outcome_variant() -> Result<(), LoopError> {
        use tempfile::NamedTempFile;

        let mut empty = FakeController::default();
        let outcome_empty = run_loop(&mut empty, RetryPolicy::default(), 10).await?;
        assert!(
            matches!(
                outcome_empty.gate,
                GateOutcome::NoGate {
                    reason: NoGateReason::NoBeadsReady,
                    ..
                }
            ),
            "empty fixed queue must surface NoGate(NoBeadsReady), got {:?}",
            outcome_empty.gate,
        );

        let mut stalled = FakeController::default();
        stalled.ready_queue.push_back(bead("lm-0", &[]));
        for i in 1..=4 {
            stalled
                .review_injects
                .push_back(vec![bead(&format!("lm-{i}"), &[])]);
        }
        for _ in 0..5 {
            stalled.agent_outcomes.push_back(AgentOutcome::Success);
        }
        let outcome_stalled = run_loop(&mut stalled, RetryPolicy::default(), 2).await?;
        match outcome_stalled.gate {
            GateOutcome::Fail(GateFail {
                reason: GateFailReason::StalledMaxIterations,
                stalled_at_max_iterations,
                ..
            }) => assert!(stalled_at_max_iterations),
            other => panic!("expected Fail(StalledMaxIterations), got {other:?}"),
        }

        let log = NamedTempFile::new().expect("tempfile");
        write_gate_log(log.path());

        let mut success = FakeController::default();
        success.ready_queue.push_back(bead("lm-success", &[]));
        success.agent_outcomes.push_back(AgentOutcome::Success);
        success
            .review_evidence
            .push_back(typed_evidence(log.path()));
        let outcome_success = run_loop(&mut success, RetryPolicy::default(), 10).await?;
        match outcome_success.gate {
            GateOutcome::Success(receipt) => {
                assert_eq!(receipt.push_range, "origin/main..HEAD");
                assert_eq!(receipt.tree_oid, "tree-a");
                assert!(receipt.total_handoffs >= 1);
            }
            other => panic!("expected Success(_), got {other:?}"),
        }
        Ok(())
    }

    /// Spec criterion (`specs/harness.md` § Loop Outcome Types): every
    /// successful `loom loop` invocation references non-empty JSONL logs
    /// carrying typed successful gate-run events.
    #[tokio::test]
    async fn every_successful_loom_loop_references_completed_gate_logs() -> Result<(), LoopError> {
        use tempfile::NamedTempFile;

        let log = NamedTempFile::new().expect("tempfile");
        let path = log.path().to_path_buf();
        write_gate_log(&path);

        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-success", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Success);
        c.review_evidence.push_back(typed_evidence(&path));
        let outcome = run_loop(&mut c, RetryPolicy::default(), 10).await?;
        let receipt = match outcome.gate {
            GateOutcome::Success(r) => r,
            other => panic!("expected Success, got {other:?}"),
        };
        assert_eq!(receipt.gate_log_paths, vec![path.clone()]);
        let contents = std::fs::read_to_string(&path).expect("log readable");
        assert!(
            contents.contains("gate_run_start") && contents.contains("gate_run_end"),
            "log must carry completed typed gate events: {contents:?}",
        );
        let runs = loom_gate::parse_gate_runs_from_jsonl(&path);
        assert_eq!(runs.len(), 2, "{runs:?}");
        assert!(runs.iter().all(loom_gate::GateRun::is_success));
        assert!(runs.iter().any(|run| {
            run.phase == loom_gate::GatePhase::Review
                && run.marker == Some(loom_protocol::gate::ExitSignal::Complete)
        }));
        Ok(())
    }

    /// Spec criterion (`specs/gate.md` § *Production walker wiring*):
    /// after the run-phase agent signals
    /// [`AgentOutcome::Success`], the loop's per-bead path routes the
    /// bead through exactly one
    /// [`AgentLoopController::exec_per_bead_gate`] call on the typed
    /// [`PerBeadGateOutcome`]; a [`PerBeadGateOutcome::Clean`] result
    /// resolves the bead to `BeadResult::Done` (neither clarified nor
    /// blocked). The subprocess shape `exec_per_bead_gate` resolves to
    /// is pinned by the production test
    /// `exec_per_bead_gate_invokes_post_integration_verify_only`.
    #[tokio::test]
    async fn loop_per_bead_routes_run_phase_success_through_exec_per_bead_gate()
    -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Success);
        // Default outcome is Clean — no scripted entry needed, but pin
        // it explicitly so the assertion below names the routing path.
        c.per_bead_gate_outcomes
            .push_back(PerBeadGateOutcome::Clean);

        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        assert_eq!(
            c.per_bead_gate_calls.len(),
            1,
            "exec_per_bead_gate must fire exactly once after run-phase Success: {:?}",
            c.per_bead_gate_calls,
        );
        assert_eq!(
            c.per_bead_gate_calls[0],
            BeadId::new("lm-1").expect("valid")
        );
        // Clean outcome → Done (verified by exclusion: not clarified, not blocked).
        assert!(c.clarified.is_empty());
        assert!(c.blocked.is_empty());
        assert_eq!(summary.beads_processed, 1);
        Ok(())
    }

    /// Spec criterion (`specs/harness.md` § Functional): after each
    /// per-bead agent run signals `Success` and the bead's branch is
    /// rebased + ff'd at the loom workspace, the loop invokes the per-bead
    /// gate (`loom gate verify --diff <range>` only). This pins the
    /// run-phase-Success → per-bead-gate edge; the subprocess shape is
    /// pinned by the production test
    /// `exec_per_bead_gate_invokes_post_integration_verify_only`.
    #[tokio::test]
    async fn per_bead_path_invokes_verify_only_after_run_phase_success() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Success);
        c.per_bead_gate_outcomes
            .push_back(PerBeadGateOutcome::Clean);

        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        // The per-bead gate fires exactly once, after the run-phase
        // Success, against the bead under dispatch.
        assert_eq!(
            c.per_bead_gate_calls,
            vec![BeadId::new("lm-1").expect("valid")]
        );
        assert_eq!(c.run_calls.len(), 1, "gate runs after a single agent run");
        assert!(c.clarified.is_empty());
        assert!(c.blocked.is_empty());
        assert_eq!(summary.beads_processed, 1);
        Ok(())
    }

    /// Spec criterion (`specs/harness.md` § Verdict Gate): a driver-side
    /// rebase conflict surfaces as `AgentOutcome::IntegrationConflict` and
    /// routes the bead through the single integration-conflict retry — not
    /// an immediate block or clarify. The retry that succeeds resolves the
    /// bead to `Done`; the recovery hop emits a `retry_dispatch` driver
    /// event carrying the `integration-conflict` cause.
    #[tokio::test]
    async fn rebase_conflict_routes_to_integration_conflict() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes
            .push_back(AgentOutcome::IntegrationConflict {
                files: vec![std::path::PathBuf::from("README.md")],
                new_base_sha: loom_driver::git::GitOid::new(
                    "0123456789abcdef0123456789abcdef01234567",
                )
                .expect("oid"),
            });
        // The single retry succeeds + the per-bead gate is Clean → Done.
        c.agent_outcomes.push_back(AgentOutcome::Success);
        c.per_bead_gate_outcomes
            .push_back(PerBeadGateOutcome::Clean);

        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        assert_eq!(
            c.run_calls.len(),
            2,
            "conflict consumes the single integration-conflict retry: initial + 1 retry",
        );
        assert!(
            c.run_calls[1].1.is_some(),
            "the retry dispatch must be flagged as a retry (previous_failure set)",
        );
        assert!(c.blocked.is_empty(), "first conflict must not block");
        assert!(c.clarified.is_empty(), "first conflict must not clarify");
        let causes: Vec<&str> = c
            .driver_events
            .iter()
            .filter_map(|(_, _, p)| p.get("cause").and_then(|v| v.as_str()))
            .collect();
        assert!(
            causes.contains(&INTEGRATION_CONFLICT_CAUSE),
            "recovery hop must carry the integration-conflict cause: {causes:?}",
        );
        assert_eq!(summary.beads_processed, 1);
        Ok(())
    }

    /// Spec criterion (`specs/harness.md` § Verdict Gate):
    /// `integration-conflict` recovery dispatches the agent at most once;
    /// a second rebase-conflict on the retry escalates to `loom:clarify`
    /// carrying a synthesized Options block. The retry budget is
    /// independent of `[loop] max_retries`.
    #[tokio::test]
    async fn integration_conflict_one_retry_then_clarify() -> Result<(), LoopError> {
        let conflict = || AgentOutcome::IntegrationConflict {
            files: vec![std::path::PathBuf::from("crates/loom-gate/src/marker.rs")],
            new_base_sha: loom_driver::git::GitOid::new("0123456789abcdef0123456789abcdef01234567")
                .expect("oid"),
        };
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(conflict());
        c.agent_outcomes.push_back(conflict());

        // `max_retries: 5` proves the cap is the integration-conflict
        // single-retry budget, not the ordinary agent-retry budget.
        let summary = run_loop(&mut c, RetryPolicy { max_retries: 5 }, 10).await?;

        assert_eq!(
            c.run_calls.len(),
            2,
            "integration-conflict allows exactly one retry regardless of max_retries",
        );
        assert!(
            c.blocked.is_empty(),
            "second conflict escalates to clarify, not block"
        );
        assert_eq!(c.clarified.len(), 1, "second conflict escalates to clarify");
        assert_eq!(c.clarified[0].0, BeadId::new("lm-1").expect("valid"));
        assert!(
            loom_protocol::gate::options::has_well_formed_block(&c.clarified[0].1),
            "clarify note must carry a well-formed Options block: {:?}",
            c.clarified[0].1,
        );
        assert_eq!(summary.beads_clarified, 1);
        Ok(())
    }

    /// Spec criterion (`specs/harness.md` § Verdict Gate): driver-applied
    /// `integration-conflict` clarify beads carry a synthesized
    /// `## Options — …` block satisfying the Options Format Contract with
    /// two `### Option N — …` subsections (resolve-in-bead-clone and
    /// abandon-the-bead), each naming its cost.
    #[test]
    fn driver_applied_integration_conflict_clarify_carries_synthesized_options() {
        let block = synthesize_integration_conflict_options(
            &[
                std::path::PathBuf::from("crates/loom-gate/src/marker.rs"),
                std::path::PathBuf::from("README.md"),
            ],
            &loom_driver::git::GitOid::new("0123456789abcdef0123456789abcdef01234567")
                .expect("oid"),
        );
        assert!(
            loom_protocol::gate::options::has_well_formed_block(&block),
            "synthesized block must satisfy the Options Format Contract: {block}",
        );
        assert!(
            block.contains("## Options —"),
            "canonical heading missing: {block}"
        );
        assert!(
            block.contains("### Option 1 — Resolve in the bead clone"),
            "resolve-in-bead-clone option missing: {block}",
        );
        assert!(
            block.contains("### Option 2 — Abandon the bead"),
            "abandon-the-bead option missing: {block}",
        );
        // Each option names its cost.
        assert!(
            block.matches("Cost:").count() >= 2,
            "each option must name a cost: {block}"
        );
        // The new integration tip + conflicting files ride through.
        assert!(
            block.contains("0123456789abcdef0123456789abcdef01234567"),
            "new integration tip missing: {block}",
        );
        assert!(
            block.contains("crates/loom-gate/src/marker.rs"),
            "conflicting files missing: {block}",
        );
    }

    /// Spec criterion (`specs/harness.md` § Functional): a structural
    /// per-bead gate violation routes the bead to `loom:blocked` with
    /// cause [`MINT_STRUCTURAL_VIOLATION_CAUSE`] and operator-facing
    /// diagnostics in the notes detail. The bead's run-phase commit is
    /// NOT unwound — the integration is already durable.
    #[tokio::test]
    async fn loop_per_bead_routes_structural_gate_violation_to_loom_blocked()
    -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Success);
        let detail =
            "structural gate violation: conflicting bd ids (ids: lm-mol.4, lm-mol.7)".to_string();
        c.per_bead_gate_outcomes
            .push_back(PerBeadGateOutcome::StructuralViolation {
                detail: detail.clone(),
            });

        let summary = run_loop(&mut c, RetryPolicy::default(), 10).await?;

        assert_eq!(c.per_bead_gate_calls.len(), 1);
        assert_eq!(c.blocked.len(), 1, "refused must route to blocked");
        assert_eq!(c.blocked[0].0, BeadId::new("lm-1").expect("valid"));
        assert_eq!(
            c.blocked[0].1, MINT_STRUCTURAL_VIOLATION_CAUSE,
            "cause must be the structural-violation token",
        );
        assert!(
            c.blocked[0].2.contains("lm-mol.4") && c.blocked[0].2.contains("lm-mol.7"),
            "blocked notes must carry the conflicting bd ids verbatim: {:?}",
            c.blocked[0].2,
        );
        assert!(c.clarified.is_empty(), "refused must not route to clarify");
        // The run-phase commit is not unwound: agent ran once, gate ran once.
        assert_eq!(c.run_calls.len(), 1);
        assert_eq!(summary.beads_blocked, 1);
        Ok(())
    }

    /// Spec criterion (`specs/harness.md` § Functional): a recoverable
    /// post-integration gate failure threads its gate-log detail into
    /// `previous_failure` and re-runs through the existing per-bead
    /// recovery loop bounded by `RetryPolicy::max_retries`. After
    /// exhaustion the bead routes to `loom:blocked` with cause
    /// `retry-exhausted` and the current gate-log detail.
    #[tokio::test]
    async fn loop_per_bead_routes_gate_recording_errors_through_recovery_loop_bounded_by_max_retries()
    -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("lm-1", &[]));
        // max_retries = 2 → initial attempt + 2 retries = 3 Success +
        // 3 post-integrate failures → blocked with the last error body.
        for _ in 0..3 {
            c.agent_outcomes.push_back(AgentOutcome::Success);
        }
        for i in 0..3 {
            c.per_bead_gate_outcomes
                .push_back(PerBeadGateOutcome::Recovery {
                    detail: format!(
                        "post-integrate-fail {i}: gate log: .loom/logs/gate/harness/lm-1-attempt-{i}.jsonl",
                    ),
                });
        }

        let summary = run_loop(&mut c, RetryPolicy { max_retries: 2 }, 10).await?;

        assert_eq!(
            c.run_calls.len(),
            3,
            "agent re-runs through the existing retry loop: initial + 2 retries",
        );
        // First attempt: no previous_failure. Subsequent attempts thread
        // the prior post-integrate gate detail verbatim into
        // `previous_failure`, including the durable gate log path.
        assert_eq!(c.run_calls[0].1, None);
        assert_eq!(
            c.run_calls[1].1.as_deref(),
            Some("post-integrate-fail 0: gate log: .loom/logs/gate/harness/lm-1-attempt-0.jsonl"),
        );
        assert_eq!(
            c.run_calls[2].1.as_deref(),
            Some("post-integrate-fail 1: gate log: .loom/logs/gate/harness/lm-1-attempt-1.jsonl"),
        );
        // The gate fired once per agent attempt.
        assert_eq!(c.per_bead_gate_calls.len(), 3);
        // Exhausted retries → blocked with the current failure body.
        assert!(c.clarified.is_empty(), "gate exhaustion must not clarify");
        assert_eq!(c.blocked.len(), 1);
        assert_eq!(c.blocked[0].0, BeadId::new("lm-1").expect("valid"));
        assert_eq!(c.blocked[0].1, RETRY_EXHAUSTED_CAUSE);
        assert!(
            c.blocked[0].2.contains("lm-1-attempt-2.jsonl"),
            "blocked note must carry the current gate-log detail: {:?}",
            c.blocked[0].2,
        );
        // `retry_dispatch` driver events fire on each recovery hop.
        let kinds: Vec<&str> = c.driver_events.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["retry_dispatch", "retry_dispatch"],
            "two recovery hops → two retry_dispatch events",
        );
        assert_eq!(summary.beads_blocked, 1);
        Ok(())
    }
}
