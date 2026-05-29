use loom_driver::bd::{Bead, Label};
use loom_driver::identifier::BeadId;
use loom_events::DriverKind;
use loom_gate::IntegrityFinding;

use super::error::ReviewError;
use super::iteration::IterationCap;
use super::verdict::{PushGateRefuseCause, ReviewVerdict, diff_new_bead_ids};
use crate::todo::ExitSignal;

/// Side-effect surface the [`review_loop`] driver depends on.
///
/// The trait abstracts the BdClient + AgentBackend + git wiring so the
/// verdict logic stays pure-ish and is exercised under a fake without
/// spawning a real container or touching the working tree. The binary wires
/// the methods to:
///
/// - `pre_snapshot` / `post_snapshot` → `BdClient::list { label: "spec:<L>" }`
/// - `blocked_ids` / `clarify_ids` → filter the same list for `loom:blocked`
///   and `loom:clarify` respectively
/// - `run_review` → render review.md, build SpawnConfig, drive
///   `AgentBackend`, tee the event stream into the log sink, parse the
///   exit signal
/// - `iteration_count` / `set_iteration_count` / `reset_iteration_count` →
///   the `iteration_count` column in `loom-driver`'s state DB
/// - `apply_clarify` → `BdClient::update --add-label loom:clarify`
/// - `git_push` / `beads_push` → `tokio::process::Command` shell-outs
/// - `exec_run` → `tokio::process::Command::new("loom").arg("run")…`
pub trait ReviewController: Send {
    /// Run the reviewer agent. Returns when the agent emits a terminal
    /// signal or fails. The implementation tees the event stream into the
    /// per-bead JSONL log alongside the terminal renderer. The parsed
    /// exit marker rides alongside the outcome so the push-gate verdict
    /// can refuse on `LOOM_CONCERN` without re-parsing the agent output.
    fn run_review(
        &mut self,
    ) -> impl std::future::Future<Output = Result<RunReviewOutput, ReviewError>> + Send;

    /// Return every bead carrying `spec:<label>` at this moment. Order is
    /// stable (creation order) so the driver's `before`/`after` diff is
    /// deterministic.
    fn list_spec_beads(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Vec<Bead>, ReviewError>> + Send;

    /// Read the persisted iteration counter for the active spec.
    fn iteration_count(
        &mut self,
    ) -> impl std::future::Future<Output = Result<u32, ReviewError>> + Send;

    /// Persist the next iteration counter value.
    fn set_iteration_count(
        &mut self,
        next: u32,
    ) -> impl std::future::Future<Output = Result<(), ReviewError>> + Send;

    /// Reset the iteration counter to zero (clean push path).
    fn reset_iteration_count(
        &mut self,
    ) -> impl std::future::Future<Output = Result<(), ReviewError>> + Send;

    /// Add the `loom:clarify` label to a fix-up bead. `reason` is
    /// informational only and is **not** persisted to `bd update --notes`
    /// — per specs/gate.md § "Persistence boundary: agent narrates, agent
    /// persists", the canonical `## Options — …` block lives in bead
    /// state only when written by the reviewer agent itself before
    /// emitting `LOOM_CLARIFY`; the runner overwriting it would leave
    /// `loom msg`'s queue empty.
    fn apply_clarify(
        &mut self,
        bead: &BeadId,
        reason: &str,
    ) -> impl std::future::Future<Output = Result<(), ReviewError>> + Send;

    /// `git push` — code-only push. Errors map to
    /// [`ReviewError::GitPushFailed`] or [`ReviewError::DetachedHead`].
    fn git_push(&mut self) -> impl std::future::Future<Output = Result<(), ReviewError>> + Send;

    /// `beads-push` (Dolt branch sync). Errors map to
    /// [`ReviewError::BeadsPushFailed`] — `git push` already succeeded by
    /// the time this runs, so the caller treats this as a separate exit.
    fn beads_push(&mut self) -> impl std::future::Future<Output = Result<(), ReviewError>> + Send;

    /// `exec loom loop -s <label>` for auto-iteration. Implementations
    /// `exec` (replace process) on success; the future resolves only on
    /// failure to launch.
    fn exec_run(&mut self) -> impl std::future::Future<Output = Result<(), ReviewError>> + Send;

    /// Emit a driver-side event into the controller's event sink (the
    /// per-spec phase JSONL log + terminal renderer). Driver events
    /// carry `Source::Driver` and a free-form `kind` so the renderer's
    /// fallback path can show them without a per-kind handler. The
    /// verdict gate routes the four spec'd `push_gate_*` kinds through
    /// here. Production callers thread an `EnvelopeBuilder` for the
    /// live envelope; the default impl is a no-op so test fakes that
    /// don't care about event emission keep working.
    fn emit_driver_event(
        &mut self,
        _kind: DriverKind,
        _summary: &str,
        _payload: serde_json::Value,
    ) {
    }

    /// Exit code from the molecule-final `loom gate verify --diff
    /// <molecule.base_commit>..HEAD` invocation, or `None` when no
    /// verify run is in scope for this push-gate evaluation. The
    /// four-condition AND refuses the push when this is `Some(n)` with
    /// `n != 0`. The default impl returns `None` so test fakes and
    /// pre-wiring production callers compile; the production controller
    /// overrides this with the actual verify exit threaded from the
    /// parent `loom loop`.
    fn verify_exit(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Option<i32>, ReviewError>> + Send {
        async { Ok(None) }
    }

    /// Integrity-gate findings across the molecule's diff scope. The
    /// four-condition AND refuses the push on any non-empty result. The
    /// default impl returns the empty list so test fakes and pre-wiring
    /// production callers compile; the production controller overrides
    /// this once the integrity gate is wired into the push-gate walk.
    fn integrity_findings(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Vec<IntegrityFinding>, ReviewError>> + Send {
        async { Ok(vec![]) }
    }

    /// Apply `loom:clarify` to the molecule's epic with the
    /// auto-generated `## Options — …` block per `specs/gate.md`
    /// § Integrity gate when the push-gate verdict refuses with cause
    /// `integrity-finding`. Production wires this to find the active
    /// molecule's epic and call `bd update --notes <options> --add-label
    /// loom:clarify`. The default impl is a no-op so test fakes that
    /// don't exercise the integrity-clarify path keep working.
    fn apply_integrity_clarify(
        &mut self,
        _findings: &[IntegrityFinding],
    ) -> impl std::future::Future<Output = Result<(), ReviewError>> + Send {
        async { Ok(()) }
    }

    /// Fetch a single bead by id, used by the epic auto-close walk to
    /// inspect `issue_type`, `status`, and `parent` as it walks up the
    /// ancestry chain. Production wires this to `BdClient::show`. The
    /// default impl returns `None` so test fakes that don't exercise the
    /// auto-close walk keep working — `auto_close_completed_epics`
    /// treats `None` as "epic not in scope" and stops the walk.
    fn show_bead(
        &mut self,
        _id: &BeadId,
    ) -> impl std::future::Future<Output = Result<Option<Bead>, ReviewError>> + Send {
        async { Ok(None) }
    }

    /// List the direct children of `parent` (`bd list --parent=<id>`).
    /// Used by the epic auto-close walk to decide whether every child of
    /// a candidate epic has reached `status == "closed"`. The default
    /// impl returns an empty list so test fakes opt out of the walk by
    /// default — the walk's "no children present" branch refuses to
    /// auto-close (an epic with no children is not what the gate is
    /// trying to retire).
    fn list_children(
        &mut self,
        _parent: &BeadId,
    ) -> impl std::future::Future<Output = Result<Vec<Bead>, ReviewError>> + Send {
        async { Ok(Vec::new()) }
    }

    /// Close a bead via `bd close <id> --reason=<reason>`. The epic
    /// auto-close walk calls this once per epic that qualifies. The
    /// default impl is a no-op so test fakes that don't drive the walk
    /// keep working.
    fn close_bead(
        &mut self,
        _id: &BeadId,
        _reason: &str,
    ) -> impl std::future::Future<Output = Result<(), ReviewError>> + Send {
        async { Ok(()) }
    }
}

/// Reviewer agent run result. Carries the typed [`ReviewOutcome`]
/// alongside the parsed exit marker so the push-gate verdict can
/// inspect the marker (refusing on `LOOM_CONCERN`) without re-parsing
/// the agent output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReviewOutput {
    pub outcome: ReviewOutcome,
    pub marker: Option<ExitSignal>,
}

/// What the reviewer agent produced. The driver only branches on
/// `Complete`; anything else aborts the gate before the post-snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewOutcome {
    /// `LOOM_COMPLETE` observed; the reviewer finished cleanly.
    Complete,

    /// Agent terminated without `LOOM_COMPLETE` (crashed, hit budget,
    /// emitted `LOOM_BLOCKED`/`LOOM_CLARIFY`). String body is surfaced
    /// in the [`ReviewError::ReviewIncomplete`] variant.
    Incomplete { detail: String },
}

/// Final state after the gate runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewResult {
    /// Push succeeded; iteration counter was reset.
    Pushed,

    /// `PushBlocked` verdict — gate stopped without pushing because at
    /// least one molecule bead carries `loom:blocked` or `loom:clarify`.
    /// Caller surfaces both ID lists to the user via the `loom msg`
    /// pointer.
    PushBlocked {
        blocked_ids: Vec<BeadId>,
        clarify_ids: Vec<BeadId>,
    },

    /// Auto-iteration was triggered. The driver execs `loom loop`; if the
    /// `exec` future resolves at all (i.e. didn't replace this process)
    /// the caller receives this variant so it can surface the failure or
    /// continue testing under a fake.
    AutoIterated { next_iteration: u32 },

    /// Iteration cap reached; newest fix-up bead got `loom:clarify`.
    Escalated { escalate_id: BeadId, cap: u32 },
}

/// Drive one `loom review` invocation through the gate.
///
/// 1. Snapshot beads carrying `spec:<label>` (`pre`).
/// 2. Run the reviewer agent.
/// 3. Snapshot again (`post`); compute new bead IDs and clarify membership.
/// 4. Apply the verdict (push / clarify-stop / auto-iterate / escalate).
pub async fn review_loop<C: ReviewController>(
    controller: &mut C,
    cap: IterationCap,
) -> Result<ReviewResult, ReviewError> {
    let pre = controller.list_spec_beads().await?;
    let pre_ids: Vec<BeadId> = pre.iter().map(|b| b.id.clone()).collect();

    let RunReviewOutput { outcome, marker } = controller.run_review().await?;
    match outcome {
        ReviewOutcome::Complete => {}
        ReviewOutcome::Incomplete { detail } => {
            if !matches!(marker, Some(ExitSignal::Concern { .. })) {
                return Err(ReviewError::ReviewIncomplete(detail));
            }
        }
    }

    let post = controller.list_spec_beads().await?;
    let post_ids: Vec<BeadId> = post.iter().map(|b| b.id.clone()).collect();
    let new_ids = diff_new_bead_ids(&pre_ids, &post_ids);
    let blocked_ids: Vec<BeadId> = post
        .iter()
        .filter(|b| b.labels.iter().any(Label::is_blocked))
        .map(|b| b.id.clone())
        .collect();
    let clarify_ids: Vec<BeadId> = post
        .iter()
        .filter(|b| b.labels.iter().any(Label::is_clarify))
        .map(|b| b.id.clone())
        .collect();

    let verify_exit = controller.verify_exit().await?;
    let integrity_findings = controller.integrity_findings().await?;
    let verdict = decide_verdict(
        &new_ids,
        &blocked_ids,
        &clarify_ids,
        verify_exit,
        marker.as_ref(),
        &integrity_findings,
        cap,
        controller,
    )
    .await?;
    apply_verdict(controller, verdict, &post).await
}

/// Walk up from every spec-bead parent, closing each epic whose direct
/// children are all `status == "closed"`. Nested epics close inside-out
/// in one pass: closing the immediate-parent epic enqueues its own
/// parent for re-evaluation, and the next iteration sees the just-
/// closed epic and decides whether the grandparent now qualifies.
///
/// Returns the list of epics closed (child-before-parent order) so the
/// caller — or a test — can pin the close sequence. Each close also
/// emits a [`DriverKind::EpicAutoClosed`] driver event onto the
/// controller's sink chain.
async fn auto_close_completed_epics<C: ReviewController>(
    controller: &mut C,
    spec_beads: &[Bead],
) -> Result<Vec<BeadId>, ReviewError> {
    use std::collections::{HashSet, VecDeque};
    let mut closed: Vec<BeadId> = Vec::new();
    let mut visited: HashSet<BeadId> = HashSet::new();
    let mut frontier: VecDeque<BeadId> =
        spec_beads.iter().filter_map(|b| b.parent.clone()).collect();
    while let Some(candidate) = frontier.pop_front() {
        if !visited.insert(candidate.clone()) {
            continue;
        }
        let Some(epic) = controller.show_bead(&candidate).await? else {
            continue;
        };
        if epic.issue_type != "epic" {
            continue;
        }
        // Skip already-closed epics, but still enqueue *their* parents:
        // a leaf bead's immediate parent may already be closed while a
        // higher ancestor still qualifies for this pass.
        if epic.status == "closed" {
            if let Some(parent) = epic.parent.clone() {
                frontier.push_back(parent);
            }
            continue;
        }
        let children = controller.list_children(&candidate).await?;
        if children.is_empty() {
            continue;
        }
        if children.iter().any(|c| c.status != "closed") {
            continue;
        }
        controller
            .close_bead(
                &candidate,
                "all children complete; auto-closed by review gate",
            )
            .await?;
        controller.emit_driver_event(
            DriverKind::EpicAutoClosed,
            &format!("epic {candidate} auto-closed: all children complete"),
            serde_json::json!({ "epic_id": candidate.to_string() }),
        );
        closed.push(candidate.clone());
        if let Some(parent) = epic.parent {
            frontier.push_back(parent);
        }
    }
    Ok(closed)
}

/// Pure-ish branch picker: resolves the verdict shape from the snapshot
/// diff plus the four push-gate inputs (bead labels, verify exit, review
/// marker, integrity findings) and the persisted iteration counter.
///
/// The four-condition AND refuses the push as soon as any one input
/// fails; the order of checks pins the refusal cause but otherwise has
/// no behavioural effect — every refusing input yields a `PushBlocked`
/// verdict tagged with its own [`PushGateRefuseCause`].
#[expect(clippy::too_many_arguments, reason = "push-gate input surface")]
async fn decide_verdict<C: ReviewController>(
    new_ids: &[BeadId],
    blocked_ids: &[BeadId],
    clarify_ids: &[BeadId],
    verify_exit: Option<i32>,
    review_marker: Option<&ExitSignal>,
    integrity_findings: &[IntegrityFinding],
    cap: IterationCap,
    controller: &mut C,
) -> Result<ReviewVerdict, ReviewError> {
    if !blocked_ids.is_empty() || !clarify_ids.is_empty() {
        return Ok(ReviewVerdict::PushBlocked {
            cause: PushGateRefuseCause::BeadNotDone,
            blocked_ids: blocked_ids.to_vec(),
            clarify_ids: clarify_ids.to_vec(),
            integrity_findings: vec![],
        });
    }

    if matches!(verify_exit, Some(code) if code != 0) {
        return Ok(ReviewVerdict::PushBlocked {
            cause: PushGateRefuseCause::VerifierFailed,
            blocked_ids: vec![],
            clarify_ids: vec![],
            integrity_findings: vec![],
        });
    }

    if matches!(review_marker, Some(ExitSignal::Concern { .. })) {
        return Ok(ReviewVerdict::PushBlocked {
            cause: PushGateRefuseCause::ReviewConcern,
            blocked_ids: vec![],
            clarify_ids: vec![],
            integrity_findings: vec![],
        });
    }

    if !integrity_findings.is_empty() {
        return Ok(ReviewVerdict::PushBlocked {
            cause: PushGateRefuseCause::IntegrityFinding,
            blocked_ids: vec![],
            clarify_ids: vec![],
            integrity_findings: integrity_findings.to_vec(),
        });
    }

    let Some(newest) = new_ids.last() else {
        return Ok(ReviewVerdict::Clean);
    };

    let current = controller.iteration_count().await?;
    if cap.is_exhausted(current) {
        return Ok(ReviewVerdict::IterationCap {
            new_bead_ids: new_ids.to_vec(),
            escalate_id: newest.clone(),
            cap: cap.max,
        });
    }

    Ok(ReviewVerdict::AutoIterate {
        new_bead_ids: new_ids.to_vec(),
        next_iteration: current + 1,
    })
}

async fn apply_verdict<C: ReviewController>(
    controller: &mut C,
    verdict: ReviewVerdict,
    spec_beads: &[Bead],
) -> Result<ReviewResult, ReviewError> {
    // Every gate walk emits `push_gate_walk` first so the JSONL replay
    // carries a fence between the reviewer's output and the verdict-
    // application sequence below. The four kind-specific events follow
    // per the push-gate event table in specs/harness.md.
    controller.emit_driver_event(
        DriverKind::PushGateWalk,
        "push gate evaluating verdict",
        serde_json::json!({"verdict": verdict_label(&verdict)}),
    );
    // The verdict_gate event surfaces the decision itself, separate from
    // the push_gate_walk fence. Consumers that index on the four-kind
    // verdict table see one row per check-loop run regardless of which
    // push_gate_* branch follows.
    controller.emit_driver_event(
        DriverKind::VerdictGate,
        &format!("verdict gate → {}", verdict_label(&verdict)),
        serde_json::json!({"outcome": verdict_label(&verdict)}),
    );
    match verdict {
        ReviewVerdict::Clean => {
            controller.emit_driver_event(
                DriverKind::PushGateClean,
                "verdict clean — pushing code + beads, resetting iteration counter",
                serde_json::json!({}),
            );
            controller.reset_iteration_count().await?;
            controller.git_push().await?;
            controller.beads_push().await?;
            // Auto-close every epic whose direct children are all closed.
            // Runs *after* both pushes succeed so a push failure cannot
            // leave a closed-locally / open-on-remote epic stranded; on
            // push failure the function returns early above and the walk
            // is skipped.
            auto_close_completed_epics(controller, spec_beads).await?;
            Ok(ReviewResult::Pushed)
        }
        ReviewVerdict::PushBlocked {
            cause,
            blocked_ids,
            clarify_ids,
            integrity_findings,
        } => {
            controller.emit_driver_event(
                DriverKind::PushGateRefuse,
                &format!("verdict push-blocked — cause {}", cause.as_str()),
                serde_json::json!({
                    "cause": cause.as_str(),
                    "blocked_ids": blocked_ids.iter().map(|b| b.to_string()).collect::<Vec<_>>(),
                    "clarify_ids": clarify_ids.iter().map(|b| b.to_string()).collect::<Vec<_>>(),
                }),
            );
            if cause == PushGateRefuseCause::IntegrityFinding {
                controller
                    .apply_integrity_clarify(&integrity_findings)
                    .await?;
            }
            Ok(ReviewResult::PushBlocked {
                blocked_ids,
                clarify_ids,
            })
        }
        ReviewVerdict::AutoIterate {
            next_iteration,
            new_bead_ids,
        } => {
            controller.emit_driver_event(
                DriverKind::PushGateWalk,
                "verdict auto-iterate — fix-up beads detected, re-entering loom loop",
                serde_json::json!({
                    "next_iteration": next_iteration,
                    "new_bead_ids": new_bead_ids.iter().map(|b| b.to_string()).collect::<Vec<_>>(),
                }),
            );
            controller.set_iteration_count(next_iteration).await?;
            controller.exec_run().await?;
            Ok(ReviewResult::AutoIterated { next_iteration })
        }
        ReviewVerdict::IterationCap {
            escalate_id,
            cap: cap_value,
            ..
        } => {
            let reason = format!(
                "Iteration cap ({cap_value}) reached: review kept finding fix-up work. Human input needed before resuming."
            );
            controller.emit_driver_event(
                DriverKind::Other("push_gate_exhausted".to_string()),
                "verdict cap-reached — escalating to clarify",
                serde_json::json!({
                    "escalate_id": escalate_id.to_string(),
                    "cap": cap_value,
                }),
            );
            controller.apply_clarify(&escalate_id, &reason).await?;
            Ok(ReviewResult::Escalated {
                escalate_id,
                cap: cap_value,
            })
        }
    }
}

/// Compact label describing the verdict shape — used as the `verdict`
/// field on the leading `push_gate_walk` event so a replay can tell at
/// a glance which branch the gate took.
fn verdict_label(verdict: &ReviewVerdict) -> &'static str {
    match verdict {
        ReviewVerdict::Clean => "clean",
        ReviewVerdict::PushBlocked { .. } => "push_blocked",
        ReviewVerdict::AutoIterate { .. } => "auto_iterate",
        ReviewVerdict::IterationCap { .. } => "iteration_cap",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::bd::Bead;

    #[derive(Default)]
    struct FakeController {
        review: Option<ReviewOutcome>,
        review_marker: Option<ExitSignal>,
        pre_beads: Vec<Bead>,
        post_beads: Vec<Bead>,
        list_calls: u32,
        iter_count: u32,
        set_iter_calls: Vec<u32>,
        reset_iter_calls: u32,
        apply_clarify_calls: Vec<(BeadId, String)>,
        apply_integrity_clarify_calls: Vec<Vec<IntegrityFinding>>,
        git_push_calls: u32,
        beads_push_calls: u32,
        exec_run_calls: u32,
        /// Capture the (kind, summary, payload) tuple for every
        /// `emit_driver_event` so tests can pin the verdict-gate
        /// emission sequence.
        driver_events: Vec<(String, String, serde_json::Value)>,
        verify_exit: Option<i32>,
        integrity_findings: Vec<IntegrityFinding>,
        /// Bead store used by `show_bead` / `list_children` to simulate
        /// the epic ancestry walk. Children are derived from each
        /// stored bead's `parent` field.
        bead_store: std::collections::HashMap<BeadId, Bead>,
        /// `(bead_id, reason)` for every `close_bead` invocation. Order
        /// pins the inside-out close sequence asserted in the
        /// nested-epic test.
        close_calls: Vec<(BeadId, String)>,
    }

    impl ReviewController for FakeController {
        async fn run_review(&mut self) -> Result<RunReviewOutput, ReviewError> {
            Ok(RunReviewOutput {
                outcome: self.review.clone().unwrap_or(ReviewOutcome::Complete),
                marker: self.review_marker.clone(),
            })
        }

        async fn verify_exit(&mut self) -> Result<Option<i32>, ReviewError> {
            Ok(self.verify_exit)
        }

        async fn integrity_findings(&mut self) -> Result<Vec<IntegrityFinding>, ReviewError> {
            Ok(self.integrity_findings.clone())
        }

        async fn list_spec_beads(&mut self) -> Result<Vec<Bead>, ReviewError> {
            self.list_calls += 1;
            if self.list_calls == 1 {
                Ok(self.pre_beads.clone())
            } else {
                Ok(self.post_beads.clone())
            }
        }

        async fn iteration_count(&mut self) -> Result<u32, ReviewError> {
            Ok(self.iter_count)
        }

        async fn set_iteration_count(&mut self, next: u32) -> Result<(), ReviewError> {
            self.set_iter_calls.push(next);
            self.iter_count = next;
            Ok(())
        }

        async fn reset_iteration_count(&mut self) -> Result<(), ReviewError> {
            self.reset_iter_calls += 1;
            self.iter_count = 0;
            Ok(())
        }

        async fn apply_clarify(&mut self, bead: &BeadId, reason: &str) -> Result<(), ReviewError> {
            self.apply_clarify_calls
                .push((bead.clone(), reason.to_string()));
            Ok(())
        }

        async fn apply_integrity_clarify(
            &mut self,
            findings: &[IntegrityFinding],
        ) -> Result<(), ReviewError> {
            self.apply_integrity_clarify_calls.push(findings.to_vec());
            Ok(())
        }

        async fn git_push(&mut self) -> Result<(), ReviewError> {
            self.git_push_calls += 1;
            Ok(())
        }

        async fn beads_push(&mut self) -> Result<(), ReviewError> {
            self.beads_push_calls += 1;
            Ok(())
        }

        async fn exec_run(&mut self) -> Result<(), ReviewError> {
            self.exec_run_calls += 1;
            Ok(())
        }

        async fn show_bead(&mut self, id: &BeadId) -> Result<Option<Bead>, ReviewError> {
            Ok(self.bead_store.get(id).cloned())
        }

        async fn list_children(&mut self, parent: &BeadId) -> Result<Vec<Bead>, ReviewError> {
            Ok(self
                .bead_store
                .values()
                .filter(|b| b.parent.as_ref() == Some(parent))
                .cloned()
                .collect())
        }

        async fn close_bead(&mut self, id: &BeadId, reason: &str) -> Result<(), ReviewError> {
            self.close_calls.push((id.clone(), reason.to_string()));
            // Reflect the close in the store so an inside-out walk sees
            // the just-closed child when it evaluates the parent epic.
            if let Some(b) = self.bead_store.get_mut(id) {
                b.status = "closed".into();
            }
            Ok(())
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
            description: String::new(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: labels.iter().map(|s| Label::new(*s)).collect(),
            parent: None,
            metadata: Default::default(),
            notes: None,
        }
    }

    /// Build a bead with a typed `issue_type`, `status`, and an
    /// optional `parent`. Used by the epic auto-close tests to populate
    /// the FakeController's bead store with realistic ancestry.
    fn shaped_bead(id: &str, issue_type: &str, status: &str, parent: Option<&str>) -> Bead {
        let mut b = bead(id, &[]);
        b.issue_type = issue_type.into();
        b.status = status.into();
        b.parent = parent.map(|p| BeadId::new(p).expect("valid bead id"));
        b
    }

    #[tokio::test]
    async fn clean_review_pushes_and_resets_counter() -> Result<(), ReviewError> {
        let mut c = FakeController {
            iter_count: 2,
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![bead("lm-1", &["spec:harness"])],
            ..FakeController::default()
        };

        let result = review_loop(&mut c, IterationCap::default()).await?;
        assert_eq!(result, ReviewResult::Pushed);
        assert_eq!(c.git_push_calls, 1);
        assert_eq!(c.beads_push_calls, 1);
        assert_eq!(c.reset_iter_calls, 1, "counter resets on clean push");
        assert_eq!(c.exec_run_calls, 0, "no auto-iterate on clean push");
        // The verdict-gate fence emits `push_gate_walk` first, then the
        // `verdict_gate` decision event, then `push_gate_clean` for the
        // clean-push branch.
        let kinds: Vec<&str> = c.driver_events.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["push_gate_walk", "verdict_gate", "push_gate_clean"],
        );
        Ok(())
    }

    /// The `PushBlocked` verdict emits `push_gate_walk` then
    /// `push_gate_refuse` carrying both ID lists in its payload.
    #[tokio::test]
    async fn push_blocked_emits_refuse_with_id_payload() -> Result<(), ReviewError> {
        let mut c = FakeController {
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![
                bead("lm-1", &["spec:harness"]),
                bead("lm-2", &["spec:harness", "loom:blocked"]),
                bead("lm-3", &["spec:harness", "loom:clarify"]),
            ],
            ..FakeController::default()
        };
        let _ = review_loop(&mut c, IterationCap::default()).await?;
        let kinds: Vec<&str> = c.driver_events.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["push_gate_walk", "verdict_gate", "push_gate_refuse"],
        );
        let refuse = c
            .driver_events
            .iter()
            .find(|(k, _, _)| k == "push_gate_refuse")
            .expect("refuse event present");
        assert!(
            refuse.2["blocked_ids"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == "lm-2")),
        );
        assert!(
            refuse.2["clarify_ids"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == "lm-3")),
        );
        Ok(())
    }

    /// The `IterationCap` verdict emits `push_gate_walk` then
    /// `push_gate_exhausted` carrying the escalate-id and the cap.
    #[tokio::test]
    async fn iteration_cap_emits_exhausted_with_cap_payload() -> Result<(), ReviewError> {
        let mut c = FakeController {
            iter_count: 3,
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![
                bead("lm-1", &["spec:harness"]),
                bead("lm-cap", &["spec:harness"]),
            ],
            ..FakeController::default()
        };
        let _ = review_loop(&mut c, IterationCap { max: 3 }).await?;
        let kinds: Vec<&str> = c.driver_events.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["push_gate_walk", "verdict_gate", "push_gate_exhausted"],
        );
        let exhausted = c
            .driver_events
            .iter()
            .find(|(k, _, _)| k == "push_gate_exhausted")
            .expect("exhausted event present");
        assert_eq!(exhausted.2["escalate_id"].as_str(), Some("lm-cap"));
        assert_eq!(exhausted.2["cap"].as_u64(), Some(3));
        Ok(())
    }

    #[tokio::test]
    async fn clarify_present_stops_without_pushing() -> Result<(), ReviewError> {
        let mut c = FakeController {
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![
                bead("lm-1", &["spec:harness"]),
                bead("lm-2", &["spec:harness", "loom:clarify"]),
            ],
            ..FakeController::default()
        };

        let result = review_loop(&mut c, IterationCap::default()).await?;
        match result {
            ReviewResult::PushBlocked {
                blocked_ids,
                clarify_ids,
            } => {
                assert!(blocked_ids.is_empty(), "no blocked beads in this scenario");
                assert_eq!(clarify_ids, vec![BeadId::new("lm-2").expect("valid")]);
            }
            other => panic!("expected PushBlocked, got {other:?}"),
        }
        assert_eq!(c.git_push_calls, 0, "clarify never pushes");
        assert_eq!(c.beads_push_calls, 0, "clarify never beads-pushes");
        assert_eq!(c.exec_run_calls, 0, "clarify never auto-iterates");
        Ok(())
    }

    #[tokio::test]
    async fn pre_existing_clarify_blocks_push_even_when_no_new_beads() -> Result<(), ReviewError> {
        let mut c = FakeController {
            pre_beads: vec![bead("lm-1", &["spec:harness", "loom:clarify"])],
            post_beads: vec![bead("lm-1", &["spec:harness", "loom:clarify"])],
            ..FakeController::default()
        };

        let result = review_loop(&mut c, IterationCap::default()).await?;
        assert!(matches!(result, ReviewResult::PushBlocked { .. }));
        assert_eq!(c.git_push_calls, 0);
        Ok(())
    }

    #[tokio::test]
    async fn blocked_present_stops_without_pushing() -> Result<(), ReviewError> {
        let mut c = FakeController {
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![
                bead("lm-1", &["spec:harness"]),
                bead("lm-2", &["spec:harness", "loom:blocked"]),
            ],
            ..FakeController::default()
        };

        let result = review_loop(&mut c, IterationCap::default()).await?;
        match result {
            ReviewResult::PushBlocked {
                blocked_ids,
                clarify_ids,
            } => {
                assert_eq!(blocked_ids, vec![BeadId::new("lm-2").expect("valid")]);
                assert!(clarify_ids.is_empty(), "no clarify beads in this scenario");
            }
            other => panic!("expected PushBlocked, got {other:?}"),
        }
        assert_eq!(c.git_push_calls, 0, "blocked never pushes");
        assert_eq!(c.beads_push_calls, 0, "blocked never beads-pushes");
        assert_eq!(c.exec_run_calls, 0, "blocked never auto-iterates");
        Ok(())
    }

    #[tokio::test]
    async fn pre_existing_blocked_blocks_push_even_when_no_new_beads() -> Result<(), ReviewError> {
        let mut c = FakeController {
            pre_beads: vec![bead("lm-1", &["spec:harness", "loom:blocked"])],
            post_beads: vec![bead("lm-1", &["spec:harness", "loom:blocked"])],
            ..FakeController::default()
        };

        let result = review_loop(&mut c, IterationCap::default()).await?;
        match result {
            ReviewResult::PushBlocked {
                blocked_ids,
                clarify_ids,
            } => {
                assert_eq!(blocked_ids, vec![BeadId::new("lm-1").expect("valid")]);
                assert!(clarify_ids.is_empty());
            }
            other => panic!("expected PushBlocked, got {other:?}"),
        }
        assert_eq!(c.git_push_calls, 0);
        Ok(())
    }

    #[tokio::test]
    async fn blocked_and_clarify_together_surface_both_lists() -> Result<(), ReviewError> {
        let mut c = FakeController {
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![
                bead("lm-1", &["spec:harness"]),
                bead("lm-2", &["spec:harness", "loom:blocked"]),
                bead("lm-3", &["spec:harness", "loom:clarify"]),
            ],
            ..FakeController::default()
        };

        let result = review_loop(&mut c, IterationCap::default()).await?;
        match result {
            ReviewResult::PushBlocked {
                blocked_ids,
                clarify_ids,
            } => {
                assert_eq!(blocked_ids, vec![BeadId::new("lm-2").expect("valid")]);
                assert_eq!(clarify_ids, vec![BeadId::new("lm-3").expect("valid")]);
            }
            other => panic!("expected PushBlocked, got {other:?}"),
        }
        assert_eq!(c.git_push_calls, 0);
        assert_eq!(c.beads_push_calls, 0);
        assert_eq!(c.exec_run_calls, 0);
        Ok(())
    }

    #[tokio::test]
    async fn fix_up_beads_under_cap_auto_iterate() -> Result<(), ReviewError> {
        let mut c = FakeController {
            iter_count: 0,
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![
                bead("lm-1", &["spec:harness"]),
                bead("lm-2", &["spec:harness"]),
            ],
            ..FakeController::default()
        };

        let result = review_loop(&mut c, IterationCap::new(3)).await?;
        match result {
            ReviewResult::AutoIterated { next_iteration } => {
                assert_eq!(next_iteration, 1);
            }
            other => panic!("expected AutoIterated, got {other:?}"),
        }
        assert_eq!(c.set_iter_calls, vec![1], "counter incremented before exec");
        assert_eq!(c.exec_run_calls, 1, "exec loom loop on auto-iterate");
        assert_eq!(c.git_push_calls, 0, "auto-iterate never pushes");
        assert!(c.apply_clarify_calls.is_empty(), "no escalation under cap");
        Ok(())
    }

    #[tokio::test]
    async fn iteration_cap_escalates_newest_fix_up_to_clarify() -> Result<(), ReviewError> {
        let mut c = FakeController {
            iter_count: 3,
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![
                bead("lm-1", &["spec:harness"]),
                bead("lm-2", &["spec:harness"]),
                bead("lm-3", &["spec:harness"]),
            ],
            ..FakeController::default()
        };

        let result = review_loop(&mut c, IterationCap::new(3)).await?;
        match result {
            ReviewResult::Escalated { escalate_id, cap } => {
                assert_eq!(
                    escalate_id,
                    BeadId::new("lm-3").expect("valid"),
                    "newest fix-up"
                );
                assert_eq!(cap, 3);
            }
            other => panic!("expected Escalated, got {other:?}"),
        }
        assert_eq!(c.apply_clarify_calls.len(), 1);
        assert_eq!(
            c.apply_clarify_calls[0].0,
            BeadId::new("lm-3").expect("valid")
        );
        assert!(
            c.apply_clarify_calls[0].1.contains("Iteration cap"),
            "reason names the cap"
        );
        assert_eq!(c.git_push_calls, 0);
        assert_eq!(c.exec_run_calls, 0);
        Ok(())
    }

    #[tokio::test]
    async fn review_incomplete_aborts_before_post_snapshot() -> Result<(), ReviewError> {
        let mut c = FakeController {
            review: Some(ReviewOutcome::Incomplete {
                detail: "no result line".into(),
            }),
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![bead("lm-1", &["spec:harness"])],
            ..FakeController::default()
        };

        let err = review_loop(&mut c, IterationCap::default()).await.err();
        assert!(matches!(err, Some(ReviewError::ReviewIncomplete(_))));
        assert_eq!(c.list_calls, 1, "post snapshot not taken on review failure");
        assert_eq!(c.git_push_calls, 0);
        Ok(())
    }

    fn unresolved_finding() -> IntegrityFinding {
        IntegrityFinding::UnresolvedAnnotation {
            spec: std::path::PathBuf::from("specs/harness.md"),
            line: 42,
            tier: loom_gate::Tier::Check,
            target: "missing-runner".to_string(),
        }
    }

    /// FR9 — push-gate verifier branch: a non-zero `loom gate verify`
    /// exit refuses the push with cause `verifier-failed`, even when
    /// every bead in the molecule is otherwise done.
    #[tokio::test]
    async fn push_gate_refuses_when_verify_exit_is_nonzero() -> Result<(), ReviewError> {
        let mut c = FakeController {
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![bead("lm-1", &["spec:harness"])],
            verify_exit: Some(1),
            ..FakeController::default()
        };
        let result = review_loop(&mut c, IterationCap::default()).await?;
        assert!(matches!(result, ReviewResult::PushBlocked { .. }));
        assert_eq!(c.git_push_calls, 0, "verify failure must refuse push");
        let refuse = c
            .driver_events
            .iter()
            .find(|(k, _, _)| k == "push_gate_refuse")
            .expect("refuse event present");
        assert_eq!(refuse.2["cause"].as_str(), Some("verifier-failed"));
        Ok(())
    }

    /// FR9.2 — verify-tier dispatch errors (exit code 2 = unknown verifier,
    /// command not found) must refuse the push with cause `verifier-failed`,
    /// i.e. count as fails rather than skips. Exit codes 1 (verifier ran
    /// and said "no") and 2 (verifier could not run at all) collapse onto
    /// the same push-gate refusal so a broken verifier never tunnels a
    /// silent pass through the four-condition AND.
    #[tokio::test]
    async fn push_blocked_on_verify_dispatch_error() -> Result<(), ReviewError> {
        let mut c = FakeController {
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![bead("lm-1", &["spec:harness"])],
            verify_exit: Some(2),
            ..FakeController::default()
        };
        let result = review_loop(&mut c, IterationCap::default()).await?;
        assert!(
            matches!(result, ReviewResult::PushBlocked { .. }),
            "dispatch error (exit 2) must refuse push, not pass through: {result:?}",
        );
        assert_eq!(
            c.git_push_calls, 0,
            "dispatch error must NOT push — counts as fail, not skip",
        );
        let refuse = c
            .driver_events
            .iter()
            .find(|(k, _, _)| k == "push_gate_refuse")
            .expect("refuse event present");
        assert_eq!(
            refuse.2["cause"].as_str(),
            Some("verifier-failed"),
            "dispatch error must route through verifier-failed cause",
        );
        assert_eq!(
            c.apply_integrity_clarify_calls.len(),
            0,
            "verify-fail branch must not invoke the integrity-clarify writer",
        );
        Ok(())
    }

    /// FR9 — push-gate review branch: a `LOOM_CONCERN` exit marker
    /// refuses the push with cause `review-concern`. The reviewer's
    /// `Incomplete` outcome must NOT short-circuit `review_loop` into
    /// an error when the marker is a structured concern; the verdict
    /// gate has to render it as a `push_gate_refuse` event so the
    /// downstream UI sees the four-condition AND fire. The driver-event
    /// payload carries the typed cause so consumers can route off it
    /// without re-deriving the refusal reason from event order.
    #[tokio::test]
    async fn push_blocked_on_review_concern_with_id_payload() -> Result<(), ReviewError> {
        let mut c = FakeController {
            review: Some(ReviewOutcome::Incomplete {
                detail: "LOOM_CONCERN: spec-conventions-violation -- bad diff".into(),
            }),
            review_marker: Some(ExitSignal::Concern {
                summary: "spec-conventions-violation in the diff".into(),
            }),
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![
                bead("lm-1", &["spec:harness"]),
                bead("lm-fix", &["spec:harness"]),
            ],
            ..FakeController::default()
        };
        let result = review_loop(&mut c, IterationCap::default()).await?;
        assert!(matches!(result, ReviewResult::PushBlocked { .. }));
        assert_eq!(c.git_push_calls, 0, "concern marker must refuse push");
        let refuse = c
            .driver_events
            .iter()
            .find(|(k, _, _)| k == "push_gate_refuse")
            .expect("refuse event present");
        assert_eq!(refuse.2["cause"].as_str(), Some("review-concern"));
        // The id-shape sub-fields are present (empty for this cause) so
        // the wire format stays stable across causes.
        assert!(refuse.2["blocked_ids"].is_array());
        assert!(refuse.2["clarify_ids"].is_array());
        Ok(())
    }

    /// Per criterion `no_path_constructs_concern_without_bead_deltas_in_production`:
    /// the legacy `ConcernWithoutBeadDeltas` guard is excised. Under the
    /// new streaming-finding contract, `LOOM_CONCERN` is a wire-format
    /// terminal whose payload is `{"summary": "..."}`; per-finding
    /// routing happens on streamed `LOOM_FINDING:` lines via mint, not
    /// by counting bead deltas inside this gate. A reviewer that emits
    /// `LOOM_CONCERN` with no streamed findings now routes to
    /// `BadWalk::ConcernWithoutFindings` at the gate boundary
    /// (`phase_verdict::decide_concern`); the runner here trusts the
    /// classifier's typed verdict and does not re-derive the protocol
    /// violation by counting beads.
    #[tokio::test]
    async fn no_path_constructs_concern_without_bead_deltas_in_production()
    -> Result<(), ReviewError> {
        let mut c = FakeController {
            review: Some(ReviewOutcome::Incomplete {
                detail: "LOOM_CONCERN: scope -- diff strays".into(),
            }),
            review_marker: Some(ExitSignal::Concern {
                summary: "scope drift across the diff".into(),
            }),
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![
                bead("lm-1", &["spec:harness"]),
                bead("lm-fix", &["spec:harness"]),
            ],
            ..FakeController::default()
        };
        let result = review_loop(&mut c, IterationCap::default()).await?;
        assert!(matches!(result, ReviewResult::PushBlocked { .. }));
        assert!(
            c.driver_events
                .iter()
                .all(|(k, _, _)| k != "review_protocol_violation"),
            "guard must not fire when bead deltas exist",
        );
        Ok(())
    }

    /// FR9 — push-gate integrity branch: any integrity-gate finding
    /// refuses the push with cause `integrity-finding`.
    #[tokio::test]
    async fn push_gate_refuses_on_integrity_finding() -> Result<(), ReviewError> {
        let mut c = FakeController {
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![bead("lm-1", &["spec:harness"])],
            integrity_findings: vec![unresolved_finding()],
            ..FakeController::default()
        };
        let result = review_loop(&mut c, IterationCap::default()).await?;
        assert!(matches!(result, ReviewResult::PushBlocked { .. }));
        assert_eq!(c.git_push_calls, 0, "integrity finding must refuse push");
        let refuse = c
            .driver_events
            .iter()
            .find(|(k, _, _)| k == "push_gate_refuse")
            .expect("refuse event present");
        assert_eq!(refuse.2["cause"].as_str(), Some("integrity-finding"));
        Ok(())
    }

    /// FR9 — push-gate integrity terminal: when integrity findings refuse
    /// the push, the gate also threads the findings into
    /// `apply_integrity_clarify` so the production controller can stamp
    /// `loom:clarify` on the molecule's epic with the auto-generated
    /// `## Options — …` block. The test fixes the spec-named contract
    /// from `specs/harness.md` FR9 condition 4 — finding present →
    /// apply_integrity_clarify called with the same findings, no push.
    #[tokio::test]
    async fn push_blocked_on_integrity_finding_applies_clarify() -> Result<(), ReviewError> {
        let findings = vec![unresolved_finding()];
        let mut c = FakeController {
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![bead("lm-1", &["spec:harness"])],
            integrity_findings: findings.clone(),
            ..FakeController::default()
        };
        let result = review_loop(&mut c, IterationCap::default()).await?;
        assert!(matches!(result, ReviewResult::PushBlocked { .. }));
        assert_eq!(c.git_push_calls, 0, "integrity finding never pushes");
        assert_eq!(
            c.apply_integrity_clarify_calls.len(),
            1,
            "apply_integrity_clarify called exactly once on the IntegrityFinding branch",
        );
        assert_eq!(
            c.apply_integrity_clarify_calls[0], findings,
            "findings threaded through verdict to controller",
        );
        Ok(())
    }

    /// The `apply_integrity_clarify` hook fires ONLY on the
    /// `IntegrityFinding` branch — not on `BeadNotDone`, `VerifierFailed`,
    /// or `ReviewConcern`. Other branches reach the molecule's epic via
    /// their own paths (recovery, blocked) and must not collide with the
    /// integrity-clarify writer.
    #[tokio::test]
    async fn apply_integrity_clarify_is_not_called_for_non_integrity_causes()
    -> Result<(), ReviewError> {
        let scenarios: Vec<(&str, FakeController)> = vec![
            (
                "bead-not-done",
                FakeController {
                    pre_beads: vec![bead("lm-1", &["spec:harness"])],
                    post_beads: vec![bead("lm-1", &["spec:harness", "loom:blocked"])],
                    ..FakeController::default()
                },
            ),
            (
                "verifier-failed",
                FakeController {
                    pre_beads: vec![bead("lm-1", &["spec:harness"])],
                    post_beads: vec![bead("lm-1", &["spec:harness"])],
                    verify_exit: Some(1),
                    ..FakeController::default()
                },
            ),
            (
                "review-concern",
                FakeController {
                    review: Some(ReviewOutcome::Incomplete {
                        detail: "LOOM_CONCERN: scope -- bad".into(),
                    }),
                    review_marker: Some(ExitSignal::Concern {
                        summary: "scope drift in the diff".into(),
                    }),
                    pre_beads: vec![bead("lm-1", &["spec:harness"])],
                    post_beads: vec![
                        bead("lm-1", &["spec:harness"]),
                        bead("lm-fix", &["spec:harness"]),
                    ],
                    ..FakeController::default()
                },
            ),
        ];
        for (label, mut c) in scenarios {
            review_loop(&mut c, IterationCap::default()).await?;
            assert!(
                c.apply_integrity_clarify_calls.is_empty(),
                "{label}: apply_integrity_clarify must not fire for non-integrity causes",
            );
        }
        Ok(())
    }

    /// FR9 — bead-labels branch: the pre-existing refusal path tags
    /// its event with cause `bead-not-done` so callers can disambiguate
    /// from the three new causes.
    #[tokio::test]
    async fn push_gate_refusal_for_bead_labels_tags_cause_bead_not_done() -> Result<(), ReviewError>
    {
        let mut c = FakeController {
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![
                bead("lm-1", &["spec:harness"]),
                bead("lm-2", &["spec:harness", "loom:blocked"]),
            ],
            ..FakeController::default()
        };
        let _ = review_loop(&mut c, IterationCap::default()).await?;
        let refuse = c
            .driver_events
            .iter()
            .find(|(k, _, _)| k == "push_gate_refuse")
            .expect("refuse event present");
        assert_eq!(refuse.2["cause"].as_str(), Some("bead-not-done"));
        Ok(())
    }

    /// FR9 four-condition AND — every push-gate input must pass for
    /// `Clean`. Each of the four inputs that fails routes to its own
    /// `PushBlocked` cause; this test pins the truth table by toggling
    /// one input at a time and asserting the cause string.
    #[tokio::test]
    async fn push_gate_evaluates_all_four_conditions() -> Result<(), ReviewError> {
        // Baseline: every input passes → push fires clean.
        let mut clean = FakeController {
            pre_beads: vec![bead("lm-1", &["spec:harness"])],
            post_beads: vec![bead("lm-1", &["spec:harness"])],
            ..FakeController::default()
        };
        assert_eq!(
            review_loop(&mut clean, IterationCap::default()).await?,
            ReviewResult::Pushed,
        );

        let cases: Vec<(&str, FakeController)> = vec![
            (
                "bead-not-done",
                FakeController {
                    pre_beads: vec![bead("lm-1", &["spec:harness"])],
                    post_beads: vec![bead("lm-1", &["spec:harness", "loom:blocked"])],
                    ..FakeController::default()
                },
            ),
            (
                "verifier-failed",
                FakeController {
                    pre_beads: vec![bead("lm-1", &["spec:harness"])],
                    post_beads: vec![bead("lm-1", &["spec:harness"])],
                    verify_exit: Some(2),
                    ..FakeController::default()
                },
            ),
            (
                "review-concern",
                FakeController {
                    review: Some(ReviewOutcome::Incomplete {
                        detail: "LOOM_CONCERN: scope -- bad".into(),
                    }),
                    review_marker: Some(ExitSignal::Concern {
                        summary: "scope drift in the diff".into(),
                    }),
                    pre_beads: vec![bead("lm-1", &["spec:harness"])],
                    post_beads: vec![
                        bead("lm-1", &["spec:harness"]),
                        bead("lm-fix", &["spec:harness"]),
                    ],
                    ..FakeController::default()
                },
            ),
            (
                "integrity-finding",
                FakeController {
                    pre_beads: vec![bead("lm-1", &["spec:harness"])],
                    post_beads: vec![bead("lm-1", &["spec:harness"])],
                    integrity_findings: vec![unresolved_finding()],
                    ..FakeController::default()
                },
            ),
        ];
        for (expected_cause, mut c) in cases {
            let result = review_loop(&mut c, IterationCap::default()).await?;
            assert!(
                matches!(result, ReviewResult::PushBlocked { .. }),
                "{expected_cause}: expected PushBlocked",
            );
            let refuse = c
                .driver_events
                .iter()
                .find(|(k, _, _)| k == "push_gate_refuse")
                .unwrap_or_else(|| panic!("{expected_cause}: refuse event present"));
            assert_eq!(
                refuse.2["cause"].as_str(),
                Some(expected_cause),
                "cause string in push_gate_refuse payload",
            );
            assert_eq!(c.git_push_calls, 0, "{expected_cause}: never pushes");
        }
        Ok(())
    }

    /// Helper: build a FakeController whose `post_beads` carry the
    /// `parent` field set so the auto-close walk has parent candidates
    /// to enumerate. The bead_store maps every id (epics + leaves) so
    /// `show_bead` / `list_children` can resolve the ancestry.
    fn controller_with_ancestry(leaves: Vec<Bead>, epics: Vec<Bead>) -> FakeController {
        let mut store: std::collections::HashMap<BeadId, Bead> = std::collections::HashMap::new();
        for b in leaves.iter().chain(epics.iter()) {
            store.insert(b.id.clone(), b.clone());
        }
        FakeController {
            pre_beads: leaves.clone(),
            post_beads: leaves,
            bead_store: store,
            ..FakeController::default()
        }
    }

    /// Trigger pin: every leaf closed + parent epic open + review
    /// LOOM_COMPLETE → epic auto-closes. Emits a single
    /// `epic_auto_closed` driver event carrying the epic id.
    #[tokio::test]
    async fn epic_auto_closes_when_all_children_closed_and_review_passes() -> Result<(), ReviewError>
    {
        let leaf = shaped_bead("lm-leaf.1", "task", "closed", Some("lm-epic"));
        let epic = shaped_bead("lm-epic", "epic", "open", None);
        let mut c = controller_with_ancestry(vec![leaf], vec![epic]);
        let result = review_loop(&mut c, IterationCap::default()).await?;
        assert_eq!(result, ReviewResult::Pushed);
        assert_eq!(
            c.close_calls,
            vec![(
                BeadId::new("lm-epic").expect("valid"),
                "all children complete; auto-closed by review gate".to_string(),
            )],
            "epic closed exactly once with the spec'd reason",
        );
        let auto_closed = c
            .driver_events
            .iter()
            .find(|(k, _, _)| k == "epic_auto_closed")
            .expect("epic_auto_closed event emitted");
        assert_eq!(auto_closed.2["epic_id"].as_str(), Some("lm-epic"));
        Ok(())
    }

    /// Universal no-fire rule: any non-`closed` child status blocks
    /// auto-close. Parameterised across the full non-closed status set
    /// (`open`, `in_progress`, `deferred`) so the criterion's "any
    /// direct child" claim is exercised once per status, not once per
    /// status that someone remembered to add a test for.
    #[tokio::test]
    async fn epic_does_not_auto_close_when_any_child_non_closed() -> Result<(), ReviewError> {
        for status in ["open", "in_progress", "deferred"] {
            let leaf_closed = shaped_bead("lm-leaf.1", "task", "closed", Some("lm-epic"));
            let leaf_non_closed = shaped_bead("lm-leaf.2", "task", status, Some("lm-epic"));
            let epic = shaped_bead("lm-epic", "epic", "open", None);
            let mut c = controller_with_ancestry(vec![leaf_closed, leaf_non_closed], vec![epic]);
            let _ = review_loop(&mut c, IterationCap::default()).await?;
            assert!(
                c.close_calls.is_empty(),
                "{status} child must block auto-close: {:?}",
                c.close_calls,
            );
            assert!(
                !c.driver_events
                    .iter()
                    .any(|(k, _, _)| k == "epic_auto_closed"),
                "{status}: no epic_auto_closed event when any child is non-closed",
            );
        }
        Ok(())
    }

    /// No-fire #3: when the push-gate refuses (any non-Clean verdict),
    /// the auto-close walk does not run even if children happen to be
    /// closed. Pinned across all three non-Clean push-refusal markers
    /// the review phase can produce: a bead carrying `loom:clarify`
    /// (bead-not-done), a `loom:blocked` bead, and a `LOOM_CONCERN`
    /// review marker.
    #[tokio::test]
    async fn epic_does_not_auto_close_on_non_clean_review_verdict() -> Result<(), ReviewError> {
        let leaf_clarify = {
            let mut b = shaped_bead("lm-leaf.1", "task", "closed", Some("lm-epic"));
            b.labels = vec![Label::new("loom:clarify")];
            b
        };
        let leaf_blocked = {
            let mut b = shaped_bead("lm-leaf.1", "task", "closed", Some("lm-epic"));
            b.labels = vec![Label::new("loom:blocked")];
            b
        };
        let leaf_clean = shaped_bead("lm-leaf.1", "task", "closed", Some("lm-epic"));
        let epic = shaped_bead("lm-epic", "epic", "open", None);

        let cases: Vec<(&str, FakeController)> = vec![
            ("clarify-on-leaf", {
                controller_with_ancestry(vec![leaf_clarify], vec![epic.clone()])
            }),
            ("blocked-on-leaf", {
                controller_with_ancestry(vec![leaf_blocked], vec![epic.clone()])
            }),
            ("loom_concern-marker", {
                let mut c = controller_with_ancestry(vec![leaf_clean], vec![epic.clone()]);
                c.review = Some(ReviewOutcome::Incomplete {
                    detail: "LOOM_CONCERN: scope -- nope".into(),
                });
                c.review_marker = Some(ExitSignal::Concern {
                    summary: "scope drift in the diff".into(),
                });
                c.post_beads.push(bead("lm-fix", &["spec:harness"]));
                c
            }),
        ];
        for (label, mut c) in cases {
            let _ = review_loop(&mut c, IterationCap::default()).await?;
            assert!(
                c.close_calls.is_empty(),
                "{label}: non-Clean verdict must skip auto-close, got {:?}",
                c.close_calls,
            );
            assert!(
                !c.driver_events
                    .iter()
                    .any(|(k, _, _)| k == "epic_auto_closed"),
                "{label}: no epic_auto_closed event on non-Clean verdict",
            );
        }
        Ok(())
    }

    /// Nested-epic inside-out close: parent epic has one child epic
    /// whose own children are all closed. One review-phase pass closes
    /// the inner epic first, then the outer epic, in that order.
    #[tokio::test]
    async fn nested_epics_close_inside_out_in_one_pass() -> Result<(), ReviewError> {
        let leaf = shaped_bead("lm-leaf.1", "task", "closed", Some("lm-inner"));
        let inner_epic = shaped_bead("lm-inner", "epic", "open", Some("lm-outer"));
        let outer_epic = shaped_bead("lm-outer", "epic", "open", None);
        let mut c = controller_with_ancestry(vec![leaf], vec![inner_epic, outer_epic]);
        let result = review_loop(&mut c, IterationCap::default()).await?;
        assert_eq!(result, ReviewResult::Pushed);
        let closed_ids: Vec<&str> = c.close_calls.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            closed_ids,
            vec!["lm-inner", "lm-outer"],
            "inner epic closes before outer in one pass",
        );
        let auto_closed_ids: Vec<&str> = c
            .driver_events
            .iter()
            .filter(|(k, _, _)| k == "epic_auto_closed")
            .map(|(_, _, p)| p["epic_id"].as_str().expect("epic_id payload"))
            .collect();
        assert_eq!(
            auto_closed_ids,
            vec!["lm-inner", "lm-outer"],
            "one event per closed epic, inside-out order",
        );
        Ok(())
    }

    /// Auto-close runs only after both pushes succeed: when `git_push`
    /// errors, the walk is skipped because `apply_verdict` returns
    /// early. Verified by erroring `git_push` from the controller and
    /// asserting no close occurred.
    #[tokio::test]
    async fn auto_close_skipped_when_git_push_fails() -> Result<(), ReviewError> {
        struct PushFailController(FakeController);
        impl ReviewController for PushFailController {
            async fn run_review(&mut self) -> Result<RunReviewOutput, ReviewError> {
                self.0.run_review().await
            }
            async fn list_spec_beads(&mut self) -> Result<Vec<Bead>, ReviewError> {
                self.0.list_spec_beads().await
            }
            async fn iteration_count(&mut self) -> Result<u32, ReviewError> {
                self.0.iteration_count().await
            }
            async fn set_iteration_count(&mut self, n: u32) -> Result<(), ReviewError> {
                self.0.set_iteration_count(n).await
            }
            async fn reset_iteration_count(&mut self) -> Result<(), ReviewError> {
                self.0.reset_iteration_count().await
            }
            async fn apply_clarify(&mut self, b: &BeadId, r: &str) -> Result<(), ReviewError> {
                self.0.apply_clarify(b, r).await
            }
            async fn git_push(&mut self) -> Result<(), ReviewError> {
                Err(ReviewError::GitPushFailed("simulated".into()))
            }
            async fn beads_push(&mut self) -> Result<(), ReviewError> {
                self.0.beads_push().await
            }
            async fn exec_run(&mut self) -> Result<(), ReviewError> {
                self.0.exec_run().await
            }
            async fn show_bead(&mut self, id: &BeadId) -> Result<Option<Bead>, ReviewError> {
                self.0.show_bead(id).await
            }
            async fn list_children(&mut self, p: &BeadId) -> Result<Vec<Bead>, ReviewError> {
                self.0.list_children(p).await
            }
            async fn close_bead(&mut self, id: &BeadId, r: &str) -> Result<(), ReviewError> {
                self.0.close_bead(id, r).await
            }
            fn emit_driver_event(&mut self, k: DriverKind, s: &str, p: serde_json::Value) {
                self.0.emit_driver_event(k, s, p);
            }
        }
        let leaf = shaped_bead("lm-leaf.1", "task", "closed", Some("lm-epic"));
        let epic = shaped_bead("lm-epic", "epic", "open", None);
        let mut c = PushFailController(controller_with_ancestry(vec![leaf], vec![epic]));
        let err = review_loop(&mut c, IterationCap::default()).await;
        assert!(matches!(err, Err(ReviewError::GitPushFailed(_))));
        assert!(
            c.0.close_calls.is_empty(),
            "auto-close must not fire when git push fails",
        );
        Ok(())
    }
}
