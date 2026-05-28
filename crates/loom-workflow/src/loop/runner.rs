use loom_driver::bd::Bead;
use loom_driver::identifier::BeadId;
use loom_events::DriverKind;
use tracing::info;

use super::error::LoopError;
use super::gate_outcome::{
    GateFail, GateOutcome, GateSuccess, HandoffEvidence, LoopOutcome, NoGateReason,
};
use super::outcome::{AgentOutcome, BeadResult};
use super::retry::{RetryDecision, RetryPolicy};

/// Loop-termination policy for `loom loop`. `Continuous` is the default — the
/// loop pulls beads until the molecule is complete, then hands off to
/// `loom review`. `Once` exits after the first bead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopMode {
    Once,
    Continuous,
}

/// Spec-table cause string written to `bd update --notes` when a pre-flight
/// infra failure routes a bead to `loom:blocked`.
pub const INFRA_PREFLIGHT_CAUSE: &str = "infra-preflight";

/// Spec-table cause string written to `bd update --notes` when the
/// driver-memory infra-retry budget is exhausted by a second mid-session
/// infra failure inside the same `loom loop` invocation.
pub const INFRA_REPEATED_CAUSE: &str = "infra-repeated";

/// Spec-table cause string written to `bd update --notes` when a bead's
/// requested `profile:X` label is not declared in the profile-image
/// manifest. Same routing pattern as [`INFRA_PREFLIGHT_CAUSE`]: no retry,
/// the loop continues with the next ready bead.
pub const UNKNOWN_PROFILE_CAUSE: &str = "unknown-profile";

/// Driver-memory budget for mid-session infra retries. Spec
/// (`specs/harness.md` §"Verdict Gate · Infra failures bypass the gate"):
/// "one free retry per `loom loop`". The counter is separate from
/// `[loop] max_iterations` and resets on every fresh `loom loop` invocation.
const INFRA_MIDSESSION_RETRY_BUDGET: u32 = 1;

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
    /// Pull the next ready bead. Returns `None` when the molecule is done.
    fn next_ready_bead(
        &mut self,
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
    /// reason-line would leave `loom msg`'s queue empty.
    fn apply_clarify(
        &mut self,
        bead: &BeadId,
        question: &str,
    ) -> impl std::future::Future<Output = Result<(), LoopError>> + Send;

    /// Add the `loom:blocked` label and write `cause` (plus any error
    /// detail) to `bd update --notes`. Called when an infra failure or
    /// an agent `LOOM_BLOCKED` self-report routes the bead straight to
    /// blocked per the verdict-gate spec.
    fn apply_blocked(
        &mut self,
        bead: &BeadId,
        cause: &str,
        error: &str,
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

/// Run the per-bead loop.
///
/// The function is deliberately not generic over `RetryPolicy` (the policy is
/// a small `Copy` value) but it is generic over [`AgentLoopController`] so the
/// binary and tests can supply different concrete impls. Returns when:
///
/// - `mode == Once` and one bead finished (success / clarify / blocked), or
/// - `mode == Continuous` and the molecule-completion handoff produced no
///   new ready beads (push gate fired clean or molecule fully stuck), or
/// - `mode == Continuous` and the outer-loop counter reached
///   `max_iterations` per FR1 (each pass = process ready queue + invoke
///   `exec_review`).
///
/// `infra_retries_used` is driver-memory only: it lives on the stack of
/// this single `run_loop` invocation and is **not** persisted. A new
/// `loom loop` starts with a fresh budget per spec §"Verdict Gate · Infra
/// failures bypass the gate".
pub async fn run_loop<C: AgentLoopController>(
    controller: &mut C,
    mode: LoopMode,
    policy: RetryPolicy,
    max_iterations: u32,
) -> Result<LoopOutcome, LoopError> {
    let mut progress = LoopProgress::default();
    let mut infra_retries_used: u32 = 0;
    let mut stalled_at_max_iterations = false;
    'outer: loop {
        let mut beads_this_pass: u32 = 0;
        // Drain the ready queue; fix-up beads bonded during this pass become
        // eligible on the next `bd ready` call.
        loop {
            let bead = match controller.next_ready_bead().await? {
                Some(b) => b,
                None => break,
            };

            let result =
                process_one_bead(controller, &bead, policy, &mut infra_retries_used).await?;
            progress.beads_processed += 1;
            beads_this_pass += 1;

            match result {
                BeadResult::Done => {
                    // No driver-side `bd close`. The agent owns closure (per
                    // the verdict-gate table's `bd-closed` observable); if
                    // it forgot to call `bd close` on `LOOM_COMPLETE`,
                    // `loom review` routes that to `incomplete-signaling`
                    // recovery on its next walk.
                }
                BeadResult::Clarified { note } => {
                    controller.apply_clarify(&bead.id, &note).await?;
                    progress.beads_clarified += 1;
                }
                BeadResult::Blocked { cause, error } => {
                    controller.apply_blocked(&bead.id, &cause, &error).await?;
                    progress.beads_blocked += 1;
                }
            }

            if matches!(mode, LoopMode::Once) {
                return Ok(finalize(progress, stalled_at_max_iterations));
            }
        }

        if !matches!(mode, LoopMode::Continuous) {
            break 'outer;
        }

        // Stall: a prior handoff produced no fix-ups → molecule is either
        // fully done (push fired clean inside `loom gate verify`) or fully
        // stuck (remaining work parked under `loom:blocked` / `loom:clarify`).
        if beads_this_pass == 0 && progress.outer_iterations > 0 {
            info!(
                outer_iterations = progress.outer_iterations,
                "loom loop: outer loop exiting — no new ready beads after handoff",
            );
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

        progress.last_evidence = controller.exec_review().await?;
        progress.outer_iterations += 1;
    }
    Ok(finalize(progress, stalled_at_max_iterations))
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
    } = progress;

    let gate = if outer_iterations == 0 {
        let reason = if beads_processed == 0 {
            NoGateReason::NoBeadsReady
        } else {
            NoGateReason::OncePartial
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

/// Run a single bead through the retry state machine.
///
/// Pre-flight infra failures exit immediately as
/// [`BeadResult::Blocked`] with cause [`INFRA_PREFLIGHT_CAUSE`]; agent
/// output is never evaluated. Mid-session infra failures consume a slot in
/// the caller-owned `infra_retries_used` counter (capped at
/// [`INFRA_MIDSESSION_RETRY_BUDGET`] across the entire `loom loop`); a
/// second occurrence routes to [`BeadResult::Blocked`] with cause
/// [`INFRA_REPEATED_CAUSE`]. Neither path consumes the agent-side
/// `[loop] max_iterations` retry budget owned by [`RetryPolicy`].
async fn process_one_bead<C: AgentLoopController>(
    controller: &mut C,
    bead: &Bead,
    policy: RetryPolicy,
    infra_retries_used: &mut u32,
) -> Result<BeadResult, LoopError> {
    let mut retries_used: u32 = 0;
    let mut previous_failure: Option<String> = None;
    loop {
        match controller.run_bead(bead, previous_failure.clone()).await? {
            AgentOutcome::Success => return Ok(BeadResult::Done),
            AgentOutcome::Failure { error } => match policy.decide(retries_used, error) {
                RetryDecision::Retry {
                    previous_failure: pf,
                } => {
                    retries_used += 1;
                    controller.emit_driver_event(
                        DriverKind::RetryDispatch,
                        &format!(
                            "retry dispatch — attempt {retries_used}/{max} for bead {bead_id}",
                            max = policy.max_retries,
                            bead_id = bead.id,
                        ),
                        serde_json::json!({
                            "bead_id": bead.id.to_string(),
                            "attempt": retries_used,
                            "max_attempts": policy.max_retries,
                        }),
                    );
                    previous_failure = Some(pf);
                }
                RetryDecision::GiveUp => {
                    return Ok(BeadResult::Clarified {
                        note: previous_failure.unwrap_or_default(),
                    });
                }
            },
            AgentOutcome::Blocked { reason } => {
                return Ok(BeadResult::Blocked {
                    cause: AGENT_BLOCKED_CAUSE.to_string(),
                    error: reason,
                });
            }
            AgentOutcome::Clarify { question } => {
                return Ok(BeadResult::Clarified { note: question });
            }
            AgentOutcome::InfraPreflight { error } => {
                return Ok(BeadResult::Blocked {
                    cause: INFRA_PREFLIGHT_CAUSE.to_string(),
                    error,
                });
            }
            AgentOutcome::UnknownProfile { error } => {
                return Ok(BeadResult::Blocked {
                    cause: UNKNOWN_PROFILE_CAUSE.to_string(),
                    error,
                });
            }
            AgentOutcome::InfraMidSession { error } => {
                if *infra_retries_used >= INFRA_MIDSESSION_RETRY_BUDGET {
                    return Ok(BeadResult::Blocked {
                        cause: INFRA_REPEATED_CAUSE.to_string(),
                        error,
                    });
                }
                *infra_retries_used += 1;
                // Infra retry does NOT consume `policy.max_retries` and
                // does NOT thread `previous_failure` — the agent never
                // produced a meaningful failure body, the container died.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::gate_outcome::GateFailReason;
    use super::*;
    use crate::todo::ExitSignal;
    use loom_driver::bd::{Bead, Label};
    use loom_driver::identifier::BeadId;
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
        driver_events: Vec<(String, String, serde_json::Value)>,
    }

    impl AgentLoopController for FakeController {
        async fn next_ready_bead(&mut self) -> Result<Option<Bead>, LoopError> {
            Ok(self.ready_queue.pop_front())
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

        async fn exec_review(&mut self) -> Result<HandoffEvidence, LoopError> {
            self.review_calls += 1;
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
    async fn once_mode_processes_single_bead() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("wx-1", &[]));
        c.ready_queue.push_back(bead("wx-2", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(&mut c, LoopMode::Once, RetryPolicy::default(), 10).await?;

        assert_eq!(summary.beads_processed, 1);
        assert_eq!(c.run_calls.len(), 1);
        // Driver does NOT call bd close — closure is the agent's job.
        // Done is verified by exclusion: not clarified, not blocked.
        assert!(c.clarified.is_empty());
        assert!(c.blocked.is_empty());
        assert_eq!(c.review_calls, 0, "once mode never execs review");
        // Second bead remains in the queue; run_loop did not pull it.
        assert_eq!(c.ready_queue.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn continuous_loops_until_molecule_complete() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("wx-1", &[]));
        c.ready_queue.push_back(bead("wx-2", &[]));
        c.ready_queue.push_back(bead("wx-3", &[]));
        for _ in 0..3 {
            c.agent_outcomes.push_back(AgentOutcome::Success);
        }

        let summary = run_loop(&mut c, LoopMode::Continuous, RetryPolicy::default(), 10).await?;

        assert_eq!(summary.beads_processed, 3);
        // All three reach Done; driver does not call bd close.
        assert!(c.clarified.is_empty());
        assert!(c.blocked.is_empty());
        assert!(summary.outer_iterations >= 1);
        Ok(())
    }

    #[tokio::test]
    async fn continuous_execs_review_on_molecule_complete() -> Result<(), LoopError> {
        // Empty ready queue → first iteration sees None → exec review.
        let mut c = FakeController::default();
        let summary = run_loop(&mut c, LoopMode::Continuous, RetryPolicy::default(), 10).await?;
        assert_eq!(summary.beads_processed, 0);
        assert!(summary.outer_iterations >= 1);
        assert_eq!(c.review_calls, 1);
        Ok(())
    }

    #[tokio::test]
    async fn once_mode_does_not_exec_review_on_empty_queue() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        let summary = run_loop(&mut c, LoopMode::Once, RetryPolicy::default(), 10).await?;
        assert_eq!(summary.outer_iterations, 0);
        assert_eq!(c.review_calls, 0);
        Ok(())
    }

    #[tokio::test]
    async fn failed_bead_retries_with_previous_failure_then_clarifies() -> Result<(), LoopError> {
        // max_retries = 2 → attempts = initial + 2 retries = 3 failures triggers clarify.
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("wx-1", &[]));
        for i in 0..3 {
            c.agent_outcomes.push_back(AgentOutcome::Failure {
                error: format!("err-{i}"),
            });
        }

        let summary = run_loop(&mut c, LoopMode::Once, RetryPolicy { max_retries: 2 }, 10).await?;

        assert_eq!(c.run_calls.len(), 3, "initial + 2 retries");
        // Attempt 1 has no previous_failure.
        assert_eq!(c.run_calls[0].1, None);
        // Attempts 2 and 3 carry the prior error verbatim.
        assert_eq!(c.run_calls[1].1.as_deref(), Some("err-0"));
        assert_eq!(c.run_calls[2].1.as_deref(), Some("err-1"));

        assert_eq!(c.clarified.len(), 1);
        assert_eq!(c.clarified[0].0, BeadId::new("wx-1").expect("valid"));
        assert_eq!(summary.beads_clarified, 1);
        Ok(())
    }

    #[tokio::test]
    async fn retry_succeeds_within_budget_reaches_done() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("wx-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Failure {
            error: "boom".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(&mut c, LoopMode::Once, RetryPolicy { max_retries: 2 }, 10).await?;

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
        c.ready_queue.push_back(bead("wx-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Failure {
            error: "err-0".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Failure {
            error: "err-1".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let _ = run_loop(&mut c, LoopMode::Once, RetryPolicy { max_retries: 3 }, 10).await?;

        let kinds: Vec<&str> = c.driver_events.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["retry_dispatch", "retry_dispatch"],
            "two retries → two retry_dispatch events; success is not announced",
        );
        let first = &c.driver_events[0];
        assert_eq!(first.2["bead_id"].as_str(), Some("wx-1"));
        assert_eq!(first.2["attempt"].as_u64(), Some(1));
        assert_eq!(first.2["max_attempts"].as_u64(), Some(3));
        Ok(())
    }

    /// Spec gate: pre-flight infra failures bypass retry entirely and
    /// route the bead to `loom:blocked` cause `infra-preflight` on the
    /// first occurrence. No agent output is ever evaluated.
    #[tokio::test]
    async fn infra_preflight_routes_to_blocked_without_retry() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("wx-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::InfraPreflight {
            error: "image load failed".into(),
        });
        // If the gate ever falls through, this Success would close the bead
        // and the assertion below would fail.
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(&mut c, LoopMode::Once, RetryPolicy { max_retries: 2 }, 10).await?;

        assert_eq!(c.run_calls.len(), 1, "preflight must not retry");
        assert!(c.clarified.is_empty());
        assert_eq!(c.blocked.len(), 1);
        assert_eq!(c.blocked[0].0, BeadId::new("wx-1").expect("valid"));
        assert_eq!(c.blocked[0].1, INFRA_PREFLIGHT_CAUSE);
        assert!(
            c.blocked[0].2.contains("image load failed"),
            "blocked notes must carry the raw error: {:?}",
            c.blocked[0].2,
        );
        assert_eq!(summary.beads_blocked, 1);
        Ok(())
    }

    /// Spec gate (Implementation Note 6): a bead whose `profile:X` label
    /// is missing from the manifest exits immediately as `loom:blocked`
    /// cause `unknown-profile` — no retry — and the loop continues with
    /// the next ready bead so a stray label on one bead does not stall
    /// the molecule. The note carries enough detail (requested profile +
    /// declared set) for the operator to relabel without re-reading the
    /// manifest.
    #[tokio::test]
    async fn unknown_profile_routes_to_blocked_without_retry_then_continues()
    -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue
            .push_back(bead("wx-bad", &["profile:nonexistent"]));
        c.ready_queue.push_back(bead("wx-ok", &["profile:base"]));
        c.agent_outcomes.push_back(AgentOutcome::UnknownProfile {
            error: "requested profile:nonexistent not declared; manifest declares: profile:base"
                .into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(
            &mut c,
            LoopMode::Continuous,
            RetryPolicy { max_retries: 2 },
            10,
        )
        .await?;

        // Bad bead: one attempt, no retry, routed to blocked.
        // Good bead: one attempt, reaches Done.
        assert_eq!(
            c.run_calls.len(),
            2,
            "unknown-profile must not retry and must not prevent the next bead from dispatching"
        );
        assert_eq!(c.run_calls[0].0, BeadId::new("wx-bad").expect("valid"));
        assert_eq!(c.run_calls[1].0, BeadId::new("wx-ok").expect("valid"));
        assert_eq!(
            c.run_calls[0].1, None,
            "unknown-profile must not thread a previous-failure body — there is no agent output",
        );

        assert_eq!(c.blocked.len(), 1, "exactly one bead blocked");
        assert_eq!(c.blocked[0].0, BeadId::new("wx-bad").expect("valid"));
        assert_eq!(c.blocked[0].1, UNKNOWN_PROFILE_CAUSE);
        // The note must contain the unknown-profile cause token, the
        // requested profile name, and at least one declared profile name
        // so the operator can relabel without re-reading the manifest.
        let note = &c.blocked[0].2;
        assert!(
            note.contains("profile:nonexistent"),
            "blocked notes must name the requested profile: {note}",
        );
        assert!(
            note.contains("profile:base"),
            "blocked notes must name at least one declared profile: {note}",
        );

        assert!(
            c.clarified.is_empty(),
            "unknown-profile must not route through the clarify branch",
        );
        assert_eq!(summary.beads_blocked, 1);
        Ok(())
    }

    /// Spec gate: the first mid-session infra failure inside a `loom loop`
    /// gets one free retry; the second one routes to `loom:blocked`
    /// cause `infra-repeated`. Both occurrences here happen on the same
    /// bead so the per-run counter is the only thing distinguishing them.
    #[tokio::test]
    async fn infra_midsession_one_retry_then_blocks_on_repeat() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("wx-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::InfraMidSession {
            error: "process exit 137 (OOM)".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::InfraMidSession {
            error: "io timeout".into(),
        });

        let summary = run_loop(&mut c, LoopMode::Once, RetryPolicy { max_retries: 2 }, 10).await?;

        assert_eq!(
            c.run_calls.len(),
            2,
            "first mid-session failure consumes the one free retry"
        );
        // Infra retries do NOT thread previous_failure into the agent
        // prompt — the spec calls them out as driver-memory state, not
        // agent-visible signal.
        assert_eq!(c.run_calls[0].1, None);
        assert_eq!(c.run_calls[1].1, None);
        assert_eq!(c.blocked.len(), 1);
        assert_eq!(c.blocked[0].1, INFRA_REPEATED_CAUSE);
        assert!(
            c.blocked[0].2.contains("io timeout"),
            "blocked notes must carry the second error body: {:?}",
            c.blocked[0].2,
        );
        assert_eq!(summary.beads_blocked, 1);
        Ok(())
    }

    /// Spec gate: a successful retry after one mid-session failure consumes
    /// the budget without touching `[loop] max_iterations`. Verifies the
    /// happy path of the one-free-retry rule.
    #[tokio::test]
    async fn infra_midsession_retry_succeeds_within_budget() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("wx-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::InfraMidSession {
            error: "stdout closed early".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(&mut c, LoopMode::Once, RetryPolicy { max_retries: 2 }, 10).await?;

        assert_eq!(c.run_calls.len(), 2);
        // Done — driver does not close, no blocked.
        assert!(c.clarified.is_empty());
        assert!(c.blocked.is_empty(), "successful retry must not block");
        assert_eq!(summary.beads_blocked, 0);
        Ok(())
    }

    /// Spec gate: the infra-retry counter is driver-memory and does NOT
    /// consume slots in `[loop] max_iterations`. After absorbing one
    /// mid-session infra failure, the agent-side retry policy still has
    /// its full budget for genuine `AgentOutcome::Failure` retries.
    #[tokio::test]
    async fn infra_retry_counter_does_not_consume_max_retries() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("wx-1", &[]));
        // 1 infra mid-session, then `max_retries=2` worth of agent failures
        // (initial attempt + 2 retries = 3 agent attempts) before clarify.
        c.agent_outcomes.push_back(AgentOutcome::InfraMidSession {
            error: "kernel oom".into(),
        });
        for i in 0..3 {
            c.agent_outcomes.push_back(AgentOutcome::Failure {
                error: format!("agent-err-{i}"),
            });
        }

        let summary = run_loop(&mut c, LoopMode::Once, RetryPolicy { max_retries: 2 }, 10).await?;

        assert_eq!(
            c.run_calls.len(),
            4,
            "1 infra retry + 3 agent attempts (initial + 2 max_retries)",
        );
        // First attempt: no previous_failure.
        assert_eq!(c.run_calls[0].1, None);
        // Second attempt is the infra retry — also no previous_failure
        // (driver-memory only, never threaded to agent).
        assert_eq!(c.run_calls[1].1, None);
        // Third attempt sees the first agent-side failure body.
        assert_eq!(c.run_calls[2].1.as_deref(), Some("agent-err-0"));
        assert_eq!(c.run_calls[3].1.as_deref(), Some("agent-err-1"));
        // The bead exhausts agent retries and clarifies — never blocks.
        assert!(c.blocked.is_empty(), "clarify path must not block");
        assert_eq!(c.clarified.len(), 1);
        assert_eq!(c.clarified[0].0, BeadId::new("wx-1").expect("valid"));
        assert_eq!(summary.beads_clarified, 1);
        Ok(())
    }

    /// Companion to the counter-separate test: the budget is per
    /// `loom loop` invocation, not per bead. A second bead's first
    /// mid-session failure inside the same run hits the spent budget
    /// and routes straight to `infra-repeated`.
    #[tokio::test]
    async fn infra_budget_is_per_run_not_per_bead() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("wx-a", &[]));
        c.ready_queue.push_back(bead("wx-b", &[]));
        // Bead A: one infra mid-session, then succeeds (consumes budget).
        c.agent_outcomes.push_back(AgentOutcome::InfraMidSession {
            error: "first".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);
        // Bead B: first attempt is a mid-session infra failure with no
        // budget left → blocked cause `infra-repeated`.
        c.agent_outcomes.push_back(AgentOutcome::InfraMidSession {
            error: "second".into(),
        });

        let summary = run_loop(&mut c, LoopMode::Continuous, RetryPolicy::default(), 10).await?;

        assert_eq!(c.run_calls.len(), 3);
        // Bead A reaches Done (no clarify, no blocked for it).
        assert!(c.clarified.is_empty());
        assert_eq!(c.blocked.len(), 1);
        assert_eq!(c.blocked[0].0, BeadId::new("wx-b").expect("valid"));
        assert_eq!(c.blocked[0].1, INFRA_REPEATED_CAUSE);
        assert_eq!(summary.beads_blocked, 1);
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
        c.ready_queue.push_back(bead("wx-initial", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Success);
        // First handoff injects a fix-up bead; second handoff produces nothing
        // (push gate clean), so the outer loop stalls and exits.
        c.review_injects
            .push_back(vec![bead("wx-fixup", &["loom:fixup"])]);
        c.review_injects.push_back(vec![]);
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(&mut c, LoopMode::Continuous, RetryPolicy::default(), 10).await?;

        assert_eq!(c.run_calls.len(), 2, "initial + fix-up processed");
        assert_eq!(c.run_calls[0].0, BeadId::new("wx-initial").expect("valid"),);
        assert_eq!(c.run_calls[1].0, BeadId::new("wx-fixup").expect("valid"));
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
        c.ready_queue.push_back(bead("wx-0", &[]));
        // Three passes scripted: each handoff injects one more fix-up bead.
        // With max_iterations = 3 the loop processes 3 fix-ups (passes 2-4)
        // plus the initial pass — but only 3 exec_review calls fire.
        for i in 1..=5 {
            c.review_injects
                .push_back(vec![bead(&format!("wx-{i}"), &[])]);
        }
        // Agent always succeeds.
        for _ in 0..6 {
            c.agent_outcomes.push_back(AgentOutcome::Success);
        }

        let summary = run_loop(&mut c, LoopMode::Continuous, RetryPolicy::default(), 3).await?;

        // Pass 1 processes wx-0; exec_review 1 injects wx-1.
        // Pass 2 processes wx-1; exec_review 2 injects wx-2.
        // Pass 3 processes wx-2; exec_review 3 injects wx-3.
        // Pass 4 processes wx-3; counter (3) reached → no exec_review 4 → break.
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
    /// `attempt` starts at zero (the production controller derives
    /// `attempt = u32::from(previous_failure.is_some())`).
    #[tokio::test]
    async fn fix_up_bead_starts_at_attempt_zero() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("wx-orig", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Failure {
            error: "first failure".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Failure {
            error: "second failure".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);
        c.review_injects
            .push_back(vec![bead("wx-fixup", &["loom:fixup"])]);
        c.review_injects.push_back(vec![]);
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let _ = run_loop(&mut c, LoopMode::Continuous, RetryPolicy::default(), 10).await?;

        let fixup_id = BeadId::new("wx-fixup").expect("valid bead id");
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

    /// FR1 outer-loop stall. A fully-clarified (or fully-stuck) molecule
    /// MUST exit on the second pass: the first pass drains the ready
    /// queue (which may be empty from the start), invokes `exec_review`,
    /// the second pass observes no new fix-ups and breaks. No spurious
    /// extra `exec_review` after the stall trigger.
    #[tokio::test]
    async fn continuous_outer_loop_exits_on_stall_when_no_fixups_appear() -> Result<(), LoopError> {
        let mut c = FakeController::default();
        // Empty ready queue; no fix-ups scripted on either review call.
        c.review_injects.push_back(vec![]);
        c.review_injects.push_back(vec![]);

        let summary = run_loop(&mut c, LoopMode::Continuous, RetryPolicy::default(), 10).await?;

        assert_eq!(summary.beads_processed, 0);
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
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut empty = FakeController::default();
        let outcome_empty =
            run_loop(&mut empty, LoopMode::Once, RetryPolicy::default(), 10).await?;
        assert!(
            matches!(
                outcome_empty.gate,
                GateOutcome::NoGate {
                    reason: NoGateReason::NoBeadsReady,
                    ..
                }
            ),
            "empty queue in --once must surface NoGate(NoBeadsReady), got {:?}",
            outcome_empty.gate,
        );

        let mut stalled = FakeController::default();
        stalled.ready_queue.push_back(bead("wx-0", &[]));
        for i in 1..=4 {
            stalled
                .review_injects
                .push_back(vec![bead(&format!("wx-{i}"), &[])]);
        }
        for _ in 0..5 {
            stalled.agent_outcomes.push_back(AgentOutcome::Success);
        }
        let outcome_stalled = run_loop(
            &mut stalled,
            LoopMode::Continuous,
            RetryPolicy::default(),
            2,
        )
        .await?;
        match outcome_stalled.gate {
            GateOutcome::Fail(GateFail {
                reason: GateFailReason::StalledMaxIterations,
                stalled_at_max_iterations,
                ..
            }) => assert!(stalled_at_max_iterations),
            other => panic!("expected Fail(StalledMaxIterations), got {other:?}"),
        }

        let mut log = NamedTempFile::new().expect("tempfile");
        log.write_all(b"event-1\nevent-2\nLOOM_COMPLETE\n").unwrap();
        let log_path = log.path().to_path_buf();

        let mut success = FakeController::default();
        success.review_evidence.push_back(HandoffEvidence {
            verify_exit: Some(0),
            review_exit: Some(0),
            review_marker: Some(ExitSignal::Complete),
            review_log_path: Some(log_path),
        });
        let outcome_success = run_loop(
            &mut success,
            LoopMode::Continuous,
            RetryPolicy::default(),
            10,
        )
        .await?;
        match outcome_success.gate {
            GateOutcome::Success(receipt) => {
                assert_eq!(receipt.verify_exit, 0);
                assert_eq!(receipt.review_exit, 0);
                assert_eq!(receipt.review_marker, ExitSignal::Complete);
                assert!(receipt.total_handoffs >= 1);
            }
            other => panic!("expected Success(_), got {other:?}"),
        }
        Ok(())
    }

    /// Spec criterion (`specs/harness.md` § Loop Outcome Types): every
    /// successful `loom loop` invocation produces a non-empty
    /// `review-*.jsonl` at `r.review_log_path` ending in a terminal
    /// `AgentEvent` whose marker equals `r.review_marker`. The sealed
    /// constructor enforces this structurally — any caller that holds a
    /// `GateSuccess` has, by definition, evidence that the on-disk log
    /// exists and ends with `LOOM_COMPLETE`. This test exercises the
    /// guarantee end-to-end through `run_loop`.
    #[tokio::test]
    async fn every_successful_loom_loop_writes_a_review_log_with_terminal_marker()
    -> Result<(), LoopError> {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut log = NamedTempFile::new().expect("tempfile");
        log.write_all(b"{\"kind\":\"text_delta\"}\nLOOM_COMPLETE\n")
            .unwrap();
        let path = log.path().to_path_buf();

        let mut c = FakeController::default();
        c.review_evidence.push_back(HandoffEvidence {
            verify_exit: Some(0),
            review_exit: Some(0),
            review_marker: Some(ExitSignal::Complete),
            review_log_path: Some(path.clone()),
        });
        let outcome = run_loop(&mut c, LoopMode::Continuous, RetryPolicy::default(), 10).await?;
        let receipt = match outcome.gate {
            GateOutcome::Success(r) => r,
            other => panic!("expected Success, got {other:?}"),
        };
        assert_eq!(receipt.review_log_path, path);
        let contents = std::fs::read_to_string(&receipt.review_log_path).expect("log readable");
        let last = contents
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .expect("non-empty line");
        assert!(
            last.contains("LOOM_COMPLETE"),
            "last log line must carry LOOM_COMPLETE: {last:?}",
        );
        assert_eq!(receipt.review_marker, ExitSignal::Complete);
        Ok(())
    }
}
