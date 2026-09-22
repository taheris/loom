//! Parallel scheduling and recovery policy; the CLI supplies only worker transport.
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{
    AgentOutcome, BatchInfraFailure, BatchResult, GateOutcome, InfraDiagnostic, InfraRetryPolicy,
    LoopError, LoopOutcome, MoleculePushGateCommands, NoGateReason, Parallelism, WorktreeBead,
    execute_molecule_push_gate,
};
use crate::gate_clarify::ClarifyApplyOutcome;
use loom_driver::agent::AgentKind;
use loom_driver::bd::{BdClient, Bead, ListOpts, UpdateOpts};
use loom_driver::config::LoomTopConfig;
use loom_driver::git::{GitClient, RepoGitPolicy};
use loom_driver::identifier::{BeadId, MoleculeId, SpecLabel};

/// Resolved invocation inputs, independent of rendering and backend implementation.
pub struct Request {
    pub workspace: PathBuf,
    pub label: SpecLabel,
    pub ready_parent: Option<BeadId>,
    pub parallelism: Parallelism,
    pub agent_override: Option<AgentKind>,
    pub wrix_bin: PathBuf,
    pub loom_bin: PathBuf,
    pub loom: LoomTopConfig,
    pub infra_policy: InfraRetryPolicy,
    pub max_iterations: u32,
    pub repo_git_policy: RepoGitPolicy,
}

/// `UpdateOpts` for a parallel-mode `loom:clarify` / `loom:blocked`
/// self-report. Pairs `status=blocked` with the terminal label so
/// `bd ready` excludes the parked bead via its native status filter
/// (`specs/harness.md` § Labels), mirroring the serial
/// `apply_clarify_or_blocked` / `apply_blocked` paths. Without the
/// paired status the escalated bead stays ready and the next
/// `loom loop` re-dispatches it instead of parking for human resolution.
fn parallel_park_update(label: &str, notes: Option<String>) -> UpdateOpts {
    UpdateOpts {
        status: Some(loom_driver::bd::Status::Blocked),
        add_labels: vec![label.to_string()],
        notes,
        ..UpdateOpts::default()
    }
}

/// Run the parallel ready/retry/stabilization loop with the caller's worker transport.
///
/// # Errors
/// Returns Beads, Git, or gate failures without certifying incomplete work.
pub async fn run<S, F>(request: Request, spawn: S) -> Result<LoopOutcome, LoopError>
where
    S: Fn(WorktreeBead) -> F + Send + Sync + 'static,
    F: std::future::Future<Output = AgentOutcome> + Send + 'static,
{
    let Request {
        workspace,
        label,
        ready_parent,
        parallelism,
        agent_override,
        wrix_bin,
        loom_bin,
        loom,
        infra_policy,
        max_iterations,
        repo_git_policy,
    } = request;
    let spawn = Arc::new(spawn);
    let bd = BdClient::new();
    let git = GitClient::open_with_integration_branch(
        workspace.clone(),
        loom.integration_branch.clone(),
    )?
    .with_hook_timeout(loom.git_hook_timeout())
    .with_repo_git_policy(repo_git_policy);
    let logs_root = workspace.join(".loom/logs");
    let batch_limit = parallelism.get() as usize;
    let mut infra_budget = ParallelInfraBudget::new(infra_policy);
    let mut infra_retry_queue: VecDeque<Bead> = VecDeque::new();
    let mut infra_queue_loaded = false;
    let mut finished_ids: HashSet<BeadId> = HashSet::new();
    let mut processed = 0_u32;
    let mut waiting = 0_u32;
    let mut clarified = 0_u32;
    let mut blocked = 0_u32;
    let mut outer_iterations = 0_u32;
    let mut work_since_gate = false;
    let mut last_gate: Option<GateOutcome> = None;

    let gate = 'outer: loop {
        if outer_iterations >= max_iterations && last_gate.is_some() {
            break GateOutcome::Fail(loom_gate::GateFail::stalled(outer_iterations));
        }
        loop {
            let deferred_ids = infra_retry_queue
                .iter()
                .map(|bead| bead.id.clone())
                .collect::<Vec<_>>();
            let mut batch_beads = parallel_ready_batch(
                &bd,
                &label,
                ready_parent.as_ref(),
                batch_limit,
                &deferred_ids,
                &finished_ids,
            )
            .await?;
            if batch_beads.is_empty() {
                if !infra_queue_loaded {
                    infra_retry_queue.extend(
                        load_parallel_infra_queue(&bd, &label, ready_parent.as_ref()).await?,
                    );
                    infra_queue_loaded = true;
                }
                while batch_beads.len() < batch_limit {
                    let Some(bead) = infra_retry_queue.pop_front() else {
                        break;
                    };
                    if finished_ids.contains(&bead.id) {
                        continue;
                    }
                    batch_beads.push(bead);
                }
            }
            if batch_beads.is_empty() {
                break;
            }

            clear_parallel_infra_state(&bd, &batch_beads).await?;
            let batch_by_id = batch_beads
                .iter()
                .map(|bead| (bead.id.clone(), bead.clone()))
                .collect::<HashMap<_, _>>();
            let spawn = Arc::clone(&spawn);
            let outcome = super::run_parallel_batch_with_logs(
                &git,
                &label,
                batch_beads,
                Some(&logs_root),
                move |slot| spawn(slot),
            )
            .await?;

            for result in outcome.results {
                match result {
                    BatchResult::Merged { bead } => {
                        infra_budget.clear(&bead);
                        finished_ids.insert(bead);
                        processed = processed.saturating_add(1);
                        work_since_gate = true;
                    }
                    BatchResult::Waiting { bead, blockers } => {
                        tracing::info!(
                            bead = %bead,
                            blocker_count = blockers.count(),
                            "loom loop: dependency wait accepted; continuing parallel work",
                        );
                        infra_budget.clear(&bead);
                        processed = processed.saturating_add(1);
                        waiting = waiting.saturating_add(1);
                    }
                    BatchResult::Conflict { bead, .. } => {
                        tracing::warn!(
                            bead = %bead,
                            "loom loop: integration conflict — marking for single retry; rerun loom loop to re-dispatch against the moved tip",
                        );
                        bd.update(
                            &bead,
                            UpdateOpts {
                                add_labels: vec![crate::r#loop::CONFLICT_RETRY_LABEL.to_string()],
                                ..UpdateOpts::default()
                            },
                        )
                        .await?;
                        emit_parallel_route_event(
                            &logs_root,
                            &label,
                            &bead,
                            loom_events::DriverKind::BdStateTransition,
                            format!("Beads state updated for {bead}: integration conflict retry"),
                            serde_json::json!({
                                "source_route": "loop-integration-conflict",
                                "identity": "integration-conflict",
                                "bead_id": bead,
                                "mutation": "update",
                                "added_labels": [crate::r#loop::CONFLICT_RETRY_LABEL],
                            }),
                        );
                        infra_budget.clear(&bead);
                        finished_ids.insert(bead);
                        processed = processed.saturating_add(1);
                    }
                    BatchResult::AgentFailed { bead, .. } => {
                        infra_budget.clear(&bead);
                        finished_ids.insert(bead);
                        processed = processed.saturating_add(1);
                    }
                    BatchResult::AgentInfra { bead, failure } => {
                        let route = infra_budget.record(&bead, &failure);
                        match route {
                            ParallelInfraRoute::Retry { diagnostic } => {
                                tracing::warn!(
                                    bead = %bead,
                                    cause = %diagnostic.cause,
                                    attempt = ?diagnostic.attempt,
                                    "loom loop: infra failure queued for retry",
                                );
                                let retry_bead =
                                    batch_by_id.get(&bead).cloned().ok_or_else(|| {
                                        LoopError::Bug {
                                            context: format!(
                                                "parallel infra result missing bead {bead}"
                                            ),
                                        }
                                    })?;
                                infra_retry_queue.push_back(retry_bead);
                            }
                            ParallelInfraRoute::Park { diagnostic } => {
                                bd.update(&bead, parallel_infra_update(&diagnostic)).await?;
                                emit_parallel_route_event(
                                    &logs_root,
                                    &label,
                                    &bead,
                                    loom_events::DriverKind::BdStateTransition,
                                    format!("Beads state updated for {bead}: loom:infra"),
                                    serde_json::json!({
                                        "source_route": "loop-infra",
                                        "identity": diagnostic.cause,
                                        "bead_id": bead,
                                        "mutation": "update",
                                        "status": "blocked",
                                        "added_labels": ["loom:infra"],
                                    }),
                                );
                                finished_ids.insert(bead);
                                processed = processed.saturating_add(1);
                                blocked = blocked.saturating_add(1);
                            }
                        }
                    }
                    BatchResult::AgentBlocked { bead, reason } => {
                        let notes = if reason.is_empty() {
                            "agent-blocked".to_string()
                        } else {
                            format!("agent-blocked: {reason}")
                        };
                        bd.update(&bead, parallel_park_update("loom:blocked", Some(notes)))
                            .await?;
                        emit_parallel_route_event(
                            &logs_root,
                            &label,
                            &bead,
                            loom_events::DriverKind::BdStateTransition,
                            format!("Beads state updated for {bead}: loom:blocked"),
                            serde_json::json!({
                                "source_route": "loop-marker",
                                "identity": "LOOM_BLOCKED",
                                "bead_id": bead,
                                "mutation": "update",
                                "status": "blocked",
                                "added_labels": ["loom:blocked"],
                            }),
                        );
                        infra_budget.clear(&bead);
                        finished_ids.insert(bead);
                        processed = processed.saturating_add(1);
                        blocked = blocked.saturating_add(1);
                    }
                    BatchResult::AgentClarify { bead, question } => {
                        if loom_protocol::gate::options::has_well_formed_block(&question) {
                            bd.update(
                                &bead,
                                UpdateOpts {
                                    notes: Some(question),
                                    ..UpdateOpts::default()
                                },
                            )
                            .await?;
                            emit_parallel_route_event(
                                &logs_root,
                                &label,
                                &bead,
                                loom_events::DriverKind::BdStateTransition,
                                format!("Beads notes updated with clarify options for {bead}"),
                                serde_json::json!({
                                    "source_route": "loop-marker",
                                    "identity": "LOOM_CLARIFY",
                                    "bead_id": bead,
                                    "mutation": "update",
                                    "notes": "clarify-options",
                                }),
                            );
                        }
                        let report =
                            crate::gate_clarify::apply_clarify_or_blocked_report(&bd, &bead)
                                .await?;
                        let context = crate::gate_clarify::ClarifyRouteContext {
                            source_route: crate::gate_clarify::ClarifySourceRoute::LoopMarker,
                            identity: "LOOM_CLARIFY".to_string(),
                            gate_log_path: crate::r#loop::BeadEmit::for_bead(
                                &logs_root, &label, &bead,
                            )
                            .map(|state| state.log_path),
                        };
                        for event in report.routing_events(&bead, &context) {
                            emit_parallel_route_event(
                                &logs_root,
                                &label,
                                &bead,
                                event.driver_kind,
                                event.summary,
                                event.payload,
                            );
                        }
                        infra_budget.clear(&bead);
                        finished_ids.insert(bead);
                        processed = processed.saturating_add(1);
                        match report.outcome {
                            ClarifyApplyOutcome::Clarify => {
                                clarified = clarified.saturating_add(1);
                            }
                            ClarifyApplyOutcome::BlockedClarifyWithoutOptions => {
                                blocked = blocked.saturating_add(1);
                            }
                        }
                    }
                }
            }
        }

        let molecule = match ready_parent.as_ref() {
            Some(parent) => Some(parent.as_str().parse::<MoleculeId>()?),
            None => crate::resolve::resolve_open_epic(&bd, &label).await?,
        };
        if let Some(molecule) = molecule.as_ref() {
            let molecule_bead = BeadId::new(molecule.as_str())?;
            let molecule_exists = match bd.show(&molecule_bead).await {
                Ok(_) => true,
                Err(loom_driver::bd::BdError::ShowEmpty)
                    if !work_since_gate && last_gate.is_none() =>
                {
                    false
                }
                Err(error) => return Err(error.into()),
            };
            let promotion = if molecule_exists {
                crate::mint::promote_deferred(&bd, molecule, false).await
            } else {
                crate::mint::MintSummary::default()
            };
            if promotion.errors > 0 {
                return Err(LoopError::Stabilization {
                    detail: promotion.render(),
                });
            }
            if promotion.refused > 0 {
                bd.update(
                    &molecule_bead,
                    UpdateOpts {
                        status: Some(loom_driver::bd::Status::Blocked),
                        add_labels: vec!["loom:blocked".to_string()],
                        notes: Some(format!(
                            "{}: {}",
                            crate::r#loop::GATE_ROUTING_STRUCTURAL_VIOLATION_CAUSE,
                            promotion.render(),
                        )),
                        ..UpdateOpts::default()
                    },
                )
                .await?;
                blocked = blocked.saturating_add(1);
                let evidence = loom_gate::HandoffEvidence {
                    molecule_state: loom_gate::MoleculeState::Unresolved,
                    ..loom_gate::HandoffEvidence::default()
                };
                break 'outer match loom_gate::GateSuccess::new(&evidence, outer_iterations) {
                    Ok(success) => GateOutcome::Success(success),
                    Err(fail) => GateOutcome::Fail(fail),
                };
            }
            if promotion.promoted_deferred > 0 {
                continue 'outer;
            }
        }

        if !work_since_gate {
            break last_gate.take().unwrap_or(GateOutcome::NoGate {
                beads_processed: processed,
                reason: if processed == 0 {
                    NoGateReason::NoBeadsReady
                } else {
                    NoGateReason::SelectionPartial
                },
            });
        }

        let handoff = execute_molecule_push_gate(
            &bd,
            &label,
            molecule.as_ref(),
            MoleculePushGateCommands::new(agent_override, &loom_bin, &wrix_bin),
            &workspace,
            &git,
        )
        .await?;
        outer_iterations = outer_iterations.saturating_add(1);
        work_since_gate = false;
        let handoff_gate = match loom_gate::GateSuccess::new(&handoff.evidence, outer_iterations) {
            Ok(success) => GateOutcome::Success(success),
            Err(fail) => GateOutcome::Fail(fail),
        };
        if matches!(handoff_gate, GateOutcome::Success(_)) {
            break handoff_gate;
        }
        last_gate = Some(handoff_gate);
    };
    Ok(LoopOutcome {
        beads_processed: processed,
        beads_waiting: waiting,
        beads_clarified: clarified,
        beads_blocked: blocked,
        outer_iterations,
        gate,
    })
}

#[derive(Debug)]
struct ParallelInfraBudget {
    attempts: HashMap<BeadId, u32>,
    max_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParallelInfraRoute {
    Retry { diagnostic: InfraDiagnostic },
    Park { diagnostic: InfraDiagnostic },
}

impl ParallelInfraBudget {
    fn new(policy: InfraRetryPolicy) -> Self {
        Self {
            attempts: HashMap::new(),
            max_attempts: policy.max_attempts.max(1),
        }
    }

    fn record(&mut self, bead: &BeadId, failure: &BatchInfraFailure) -> ParallelInfraRoute {
        if !failure.is_retryable() {
            self.clear(bead);
            return ParallelInfraRoute::Park {
                diagnostic: failure.diagnostic(0, self.max_attempts),
            };
        }
        let attempt = self
            .attempts
            .get(bead)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        self.attempts.insert(bead.clone(), attempt);
        let diagnostic = failure.diagnostic(attempt, self.max_attempts);
        if attempt >= self.max_attempts {
            self.clear(bead);
            ParallelInfraRoute::Park { diagnostic }
        } else {
            ParallelInfraRoute::Retry { diagnostic }
        }
    }

    fn clear(&mut self, bead: &BeadId) {
        self.attempts.remove(bead);
    }
}

async fn parallel_ready_batch(
    bd: &BdClient,
    label: &SpecLabel,
    ready_parent: Option<&BeadId>,
    batch_limit: usize,
    deferred: &[BeadId],
    finished: &HashSet<BeadId>,
) -> Result<Vec<Bead>, LoopError> {
    let beads = bd
        .ready(loom_driver::bd::ReadyOpts {
            limit: None,
            label: ready_parent
                .is_none()
                .then(|| format!("spec:{}", label.as_str())),
            parent: ready_parent.cloned(),
            exclude_label: vec![],
        })
        .await?;
    let mut out = Vec::with_capacity(batch_limit);
    for bead in beads {
        if out.len() >= batch_limit {
            break;
        }
        if deferred.iter().any(|id| id == &bead.id) || finished.contains(&bead.id) {
            continue;
        }
        if bead.issue_type == loom_driver::bd::IssueType::Epic {
            tracing::info!(
                bead = %bead.id,
                spec = %label,
                "loom loop: skipping epic-typed ready bead — workers dispatch leaves only",
            );
            continue;
        }
        out.push(bead);
    }
    Ok(out)
}

async fn load_parallel_infra_queue(
    bd: &BdClient,
    label: &SpecLabel,
    ready_parent: Option<&BeadId>,
) -> Result<VecDeque<Bead>, LoopError> {
    let beads = bd
        .list(ListOpts {
            statuses: vec![loom_driver::bd::Status::Blocked],
            label: ready_parent
                .is_none()
                .then(|| format!("spec:{}", label.as_str())),
            label_any: vec!["loom:infra".to_string()],
            parent: ready_parent.cloned(),
            ..ListOpts::default()
        })
        .await?;
    let mut queue = VecDeque::new();
    for bead in beads {
        if bead.issue_type == loom_driver::bd::IssueType::Epic {
            tracing::info!(
                bead = %bead.id,
                spec = %label,
                "loom loop: skipping epic-typed infra bead — workers dispatch leaves only",
            );
            continue;
        }
        queue.push_back(bead);
    }
    Ok(queue)
}

async fn clear_parallel_infra_state(bd: &BdClient, beads: &[Bead]) -> Result<(), LoopError> {
    for bead in beads {
        if bead.labels.iter().any(loom_driver::bd::Label::is_infra) {
            bd.update(&bead.id, parallel_clear_infra_update()).await?;
        }
    }
    Ok(())
}

fn parallel_clear_infra_update() -> UpdateOpts {
    UpdateOpts {
        status: Some(loom_driver::bd::Status::Open),
        remove_labels: vec!["loom:infra".to_string()],
        ..UpdateOpts::default()
    }
}

fn parallel_infra_update(diagnostic: &InfraDiagnostic) -> UpdateOpts {
    let mut metadata = vec![
        ("loom.infra.cause".to_string(), diagnostic.cause.clone()),
        ("loom.infra.phase".to_string(), "loop".to_string()),
        (
            "loom.infra.class".to_string(),
            diagnostic.infra_class.clone(),
        ),
    ];
    if let Some(first_event_seen) = diagnostic.first_event_seen {
        metadata.push((
            "loom.infra.first_event_seen".to_string(),
            first_event_seen.to_string(),
        ));
    }
    if let Some(attempt) = diagnostic.attempt {
        metadata.push(("loom.infra.attempt".to_string(), attempt.to_string()));
    }
    if let Some(max_attempts) = diagnostic.max_attempts {
        metadata.push((
            "loom.infra.max_attempts".to_string(),
            max_attempts.to_string(),
        ));
    }
    UpdateOpts {
        status: Some(loom_driver::bd::Status::Blocked),
        add_labels: vec!["loom:infra".to_string()],
        notes: Some(parallel_diagnostic_notes(
            &diagnostic.cause,
            &diagnostic.error,
        )),
        set_metadata: metadata,
        ..UpdateOpts::default()
    }
}

fn parallel_diagnostic_notes(cause: &str, error: &str) -> String {
    if error.is_empty() {
        cause.to_string()
    } else {
        format!("{cause}: {error}")
    }
}

fn emit_parallel_route_event(
    logs_root: &Path,
    label: &SpecLabel,
    bead: &BeadId,
    kind: loom_events::DriverKind,
    summary: impl AsRef<str>,
    mut payload: serde_json::Value,
) {
    let Some(mut emit) = crate::r#loop::BeadEmit::for_bead(logs_root, label, bead) else {
        return;
    };
    if matches!(kind, loom_events::DriverKind::ClarifyDowngraded)
        && let Some(object) = payload.as_object_mut()
    {
        object.insert(
            "event_sequence".to_string(),
            serde_json::json!(emit.builder.current_seq()),
        );
        object.insert(
            "gate_log_path".to_string(),
            serde_json::json!(emit.log_path.to_string_lossy()),
        );
    }
    emit.emit(kind, summary.as_ref(), payload);
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Spec contract `specs/harness.md` § Labels: parallel-mode
    /// `loom:clarify` / `loom:blocked` self-reports must pair
    /// `status=blocked` with the label so `bd ready` excludes the parked
    /// bead via its native status filter. Without it the escalated bead
    /// stays ready and the next `loom loop` re-dispatches it instead of
    /// parking for human resolution — the divergence from the serial
    /// `apply_*` paths this guards against.
    #[test]
    fn parallel_park_pairs_status_blocked_with_label() {
        for label in ["loom:clarify", "loom:blocked"] {
            let opts = parallel_park_update(label, Some("a-note".to_string()));
            assert_eq!(
                opts.status.map(loom_driver::bd::Status::as_str),
                Some("blocked"),
                "{label}: must transition status=blocked so `bd ready` excludes it",
            );
            assert!(
                opts.add_labels.iter().any(|l| l == label),
                "{label}: terminal label must be applied: {:?}",
                opts.add_labels,
            );
        }
    }

    #[test]
    fn parallel_infra_budget_retries_then_parks_with_attempt_metadata() {
        let bead = BeadId::new("lm-infra").expect("valid bead id");
        let mut budget = ParallelInfraBudget::new(InfraRetryPolicy { max_attempts: 2 });
        let failure = BatchInfraFailure::Preflight {
            error: "spawn eof".to_string(),
        };

        let first = budget.record(&bead, &failure);
        match first {
            ParallelInfraRoute::Retry { diagnostic } => {
                assert_eq!(diagnostic.cause, "infra-preflight");
                assert_eq!(diagnostic.attempt, Some(1));
                assert_eq!(diagnostic.max_attempts, Some(2));
                assert_eq!(diagnostic.first_event_seen, Some(false));
            }
            other @ ParallelInfraRoute::Park { .. } => {
                panic!("first preflight failure should retry, got {other:?}");
            }
        }
        let second = budget.record(&bead, &failure);
        match second {
            ParallelInfraRoute::Park { diagnostic } => {
                assert_eq!(diagnostic.cause, "infra-preflight");
                assert_eq!(diagnostic.attempt, Some(2));
                assert_eq!(diagnostic.max_attempts, Some(2));
            }
            other @ ParallelInfraRoute::Retry { .. } => {
                panic!("second preflight failure should park, got {other:?}");
            }
        }
    }

    #[test]
    fn parallel_infra_update_pairs_status_label_and_metadata() {
        let diagnostic = InfraDiagnostic::retryable(
            "infra-interrupted",
            "infra-interrupted",
            "stream eof".to_string(),
            2,
            3,
            true,
        );

        let opts = parallel_infra_update(&diagnostic);

        assert_eq!(
            opts.status.map(loom_driver::bd::Status::as_str),
            Some("blocked")
        );
        assert!(opts.add_labels.iter().any(|label| label == "loom:infra"));
        assert_eq!(opts.notes.as_deref(), Some("infra-interrupted: stream eof"),);
        assert!(opts.set_metadata.contains(&(
            "loom.infra.cause".to_string(),
            "infra-interrupted".to_string(),
        )));
        assert!(opts.set_metadata.contains(&(
            "loom.infra.first_event_seen".to_string(),
            "true".to_string(),
        )));
        assert!(
            opts.set_metadata
                .contains(&("loom.infra.attempt".to_string(), "2".to_string(),))
        );
        assert!(
            opts.set_metadata
                .contains(&("loom.infra.max_attempts".to_string(), "3".to_string(),))
        );
    }

    #[test]
    fn parallel_clear_infra_update_reopens_and_removes_label() {
        let opts = parallel_clear_infra_update();

        assert_eq!(
            opts.status.map(loom_driver::bd::Status::as_str),
            Some("open")
        );
        assert!(opts.remove_labels.iter().any(|label| label == "loom:infra"),);
    }
}
