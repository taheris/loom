//! Production [`AgentLoopController`] used by the `loom loop` binary.
//!
//! Wires `BdClient` for bead lookup/close/clarify, a `tokio::process::Command`
//! shell-out for `exec_review`, and a caller-provided dispatch closure for the
//! actual agent invocation. The closure pattern keeps backend selection
//! (`PiBackend`, `ClaudeBackend`, or `DirectBackend`) inside the binary's
//! `dispatch` match — `loom-workflow` never sees the concrete backend types,
//! mirroring the shape
//! used by `ProductionTodoController` and `run_parallel_batch`.
//!
//! Per-bead profile dispatch is wired through [`build_spawn_config_from_manifest`]:
//! the manifest, CLI `--profile` override, and per-phase fallback all flow
//! into the controller at construction time so `run_bead` resolves the
//! per-bead `image_ref` + `image_source` against the parsed manifest before
//! the agent invocation. A missing manifest entry surfaces as
//! [`LoopError::Profile`] — no silent fallback.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use loom_driver::agent::{AgentRuntime, ProtocolError, SpawnConfig};
use loom_driver::bd::{
    BdClient, Bead, CommandRunner, ListOpts, ReadyOpts, TokioRunner, UpdateOpts,
};
use loom_driver::clock::{Clock, SystemClock};
use loom_driver::config::{LoomConfig, LoomTopConfig, Phase, SkillsConfig};
use loom_driver::git::{
    BeadCloneAlignment, BeadClonePreparation, CreatedWorktree, GitClient, GitOid, RebaseOutcome,
};
use loom_driver::identifier::{BeadId, MoleculeId, ProfileName, SpecLabel};
use loom_driver::lock::LockGuard;
use loom_driver::logging::phase_log_path;
use loom_driver::profile_manifest::{ProfileError, ProfileImageManifest};
use loom_driver::scratch::resolve_scratch_key;
use loom_events::{
    AgentEvent, AgentStartMetadata, DriverEventPayload, DriverKind, EnvelopeBuilder, SessionScope,
    Source,
};
use tokio::process::Command;
use tracing::{info, warn};

use loom_gate::{
    GatePhase, GateRun, GateRunStatus, GateSuccess, HandoffEvidence, MarkerProof, MoleculeState,
    append_gate_run_lifecycle_events, parse_gate_runs_from_jsonl,
};

use super::context::{LoopContextInputs, render_loop_prompt};
use super::driver_emit::BeadEmit;
use super::error::LoopError;
use super::outcome::{AgentOutcome, InfraDiagnostic, SessionResult};
use super::runner::{
    AgentLoopController, INVALID_SPAWN_CONFIG_CAUSE, PerBeadGateOutcome, StabilizationOutcome,
    WORKSPACE_RECOVERY_FAILED_CAUSE,
};
use crate::spawn::container_workspace_path;

use super::spawn::{build_spawn_config_from_manifest, dolt_socket_mount, sccache_mount};
use super::tree_clean::dirty_paths_from_porcelain;
use super::verify::{VerifyPass, verify_pass};
use crate::review::{
    DispatchScope, GateInputs, PhaseVerdict, RecoveryCause, WalkOutput, WorkspaceFindingValidator,
    decide,
};
use crate::skill::SkillPlan;
use crate::suppression::suppresses_rubric_finding;
use crate::todo::{ExitSignal, parse_exit_signal};
use loom_templates::previous_failure::{TerminalSurface, VerifierFailure};
use loom_templates::run::{PreviousFailure, RecoveryStash, WorkspaceAlignment, WorkspaceRecovery};

/// Env var the molecule-completion handoff sets when spawning `loom gate
/// review` so the child re-uses the parent's pinned `phase_when`
/// timestamp. With both sides computing the JSONL log path from the
/// same `(logs_root, "review", when)` tuple, the parent can
/// thread `review_log_path` into [`HandoffEvidence`] without scanning
/// directories.
pub const REVIEW_PHASE_WHEN_ENV: &str = "LOOM_REVIEW_PHASE_WHEN_MILLIS";

/// Env var the molecule-completion handoff sets when spawning `loom gate
/// review` so the child emits the agent's combined stdout to its own
/// stdout. The parent captures it via [`Command::output`] and runs
/// [`parse_exit_signal`] on the final non-empty line.
pub const REVIEW_EMIT_STDOUT_ENV: &str = "LOOM_REVIEW_EMIT_STDOUT";

/// Env var the molecule-completion handoff sets when spawning `loom gate
/// review` so the child can select the same spec context/log namespace as
/// the parent controller without adding a public `--spec` gate filter. The
/// value is context only: `--diff` remains the trust-bearing scope.
pub const REVIEW_SPEC_LABEL_ENV: &str = "LOOM_REVIEW_SPEC_LABEL";

/// Internal handoff flag that keeps `loom gate review` inspection-only when
/// the loop-owned molecule controller is responsible for marker minting and
/// push side effects.
pub const REVIEW_INSPECTION_ONLY_ENV: &str = "LOOM_REVIEW_INSPECTION_ONLY";

/// Internal handoff path to the completed deterministic gate log whose
/// [`loom_gate::VerifiedScope`] the push-eligible review must consume.
pub const REVIEW_VERIFIED_LOG_ENV: &str = "LOOM_REVIEW_VERIFIED_LOG";

struct DriverEventDraft {
    summary: String,
    payload: serde_json::Value,
}

fn workspace_recovery_from_preparation(
    preparation: &BeadClonePreparation,
) -> Option<WorkspaceRecovery> {
    let recovery = preparation.recovery.as_ref()?;
    Some(WorkspaceRecovery {
        pre_stash_status: recovery.pre_stash_status.clone(),
        stash: RecoveryStash {
            selector: recovery.stash.selector.clone(),
            commit: recovery.stash.commit.clone(),
            message: recovery.stash.message.clone(),
        },
        integration_tip: preparation.integration_tip.clone(),
        alignment: workspace_alignment_from_git(&preparation.alignment),
    })
}

fn workspace_alignment_from_git(alignment: &BeadCloneAlignment) -> WorkspaceAlignment {
    match alignment {
        BeadCloneAlignment::Clean => WorkspaceAlignment::Clean,
        BeadCloneAlignment::Rebased {
            previous_head,
            current_head,
        } => WorkspaceAlignment::Rebased {
            previous_head: previous_head.clone(),
            current_head: current_head.clone(),
        },
        BeadCloneAlignment::Conflict { files } => WorkspaceAlignment::Conflict {
            files: files
                .iter()
                .map(|file| file.to_string_lossy().into_owned())
                .collect(),
        },
    }
}

fn workspace_recovery_event_draft(
    bead_id: &BeadId,
    recovery: &WorkspaceRecovery,
) -> DriverEventDraft {
    let (alignment_outcome, previous_head, current_head) = match &recovery.alignment {
        WorkspaceAlignment::Clean => ("clean", None, None),
        WorkspaceAlignment::Rebased {
            previous_head,
            current_head,
        } => (
            "rebased",
            Some(previous_head.as_str()),
            Some(current_head.as_str()),
        ),
        WorkspaceAlignment::Conflict { .. } => ("conflict", None, None),
    };
    DriverEventDraft {
        summary: format!("workspace recovery stash preserved for bead {bead_id}"),
        payload: serde_json::json!({
            "bead_id": bead_id.to_string(),
            "pre_stash_status": recovery.pre_stash_status.as_str(),
            "stash_selector": recovery.stash.selector.as_str(),
            "stash_message": recovery.stash.message.as_str(),
            "stash_commit": recovery.stash.commit.as_str(),
            "integration_tip": recovery.integration_tip.as_str(),
            "alignment_outcome": alignment_outcome,
            "alignment_previous_head": previous_head,
            "alignment_current_head": current_head,
            "conflict_files": recovery.alignment.conflict_files(),
        }),
    }
}

/// Wires the [`AgentLoopController`] trait against the real `BdClient`, a
/// caller-provided agent dispatch closure, and a child `loom review` exec for
/// handoff.
///
/// `manifest` / `cli_profile` / `phase_default` are the inputs the per-bead
/// profile resolver chain needs (see
/// [`super::resolve_profile_image`]). They are stored on the controller so
/// every `run_bead` call resolves the bead's `image_ref` + `image_source`
/// from the same parsed manifest, never re-reading it from disk.
///
/// `spawn` is the per-phase dispatch closure: the binary builds it from
/// `dispatch(kind, &spawn_config)` so the workflow stays backend-agnostic.
/// `run_bead` calls it on every retry attempt, so the closure must be `Fn`
/// (callable repeatedly). It receives `(SpawnConfig, BeadId)` — the bead id
/// is passed alongside the spawn config so the closure can open the per-bead
/// JSONL [`LogSink`](loom_driver::logging::LogSink) before dispatch.
pub struct ProductionAgentLoopController<S, F, R: CommandRunner = TokioRunner>
where
    S: Fn(SpawnConfig, BeadId) -> F + Send,
    F: std::future::Future<Output = (SessionResult, Option<ExitSignal>)> + Send,
{
    bd: BdClient<R>,
    label: SpecLabel,
    loom_bin: PathBuf,
    beads_push_bin: PathBuf,
    workspace: PathBuf,
    git: GitClient,
    manifest: Arc<ProfileImageManifest>,
    cli_profile: Option<ProfileName>,
    phase_default: ProfileName,
    runtime: AgentRuntime,
    spawn: S,
    /// Spec lock dropped before exec'ing child `loom gate` commands.
    lock: Option<LockGuard>,
    /// Workspace-relative path to the style-rules document pinned in the
    /// run prompt. Sourced from `LoomConfig.style_rules` at construction
    /// time via [`Self::with_style_rules`]; defaults to the built-in path
    /// so test fakes that skip the builder still render a valid prompt.
    style_rules: String,
    /// Typed `PreviousFailure` to thread on the next attempt, set by the
    /// verdict-gate tree-not-clean dispatcher so a dirty worktree on attempt
    /// N renders the rich `TreeNotClean { dirty_paths }` body on attempt
    /// N+1 instead of the opaque agent-error string returned through the
    /// runner. Cleared on a fresh bead dispatch (`previous_failure = None`).
    stashed_previous_failure: Option<PreviousFailure>,
    /// Typed [`PreviousFailure::ReviewConcern`] (or `BadWalk`) parsed
    /// from the molecule-completion review's stdout. Distinct from
    /// [`Self::stashed_previous_failure`] because its scope is the
    /// molecule, not a single bead's retry chain: the next `run_bead`
    /// call — typically a fresh fix-up bead, not a retry — consumes it
    /// so the parsed `Vec<Finding>` rides through into the recovery
    /// prompt per `specs/harness.md`
    /// § *molecule_completion_review_threads_findings_into_previous_failure_review_concern*.
    /// Consumed once (take); cleared on first read.
    stashed_review_concern: Option<PreviousFailure>,
    /// Per-bead JSONL log root. When set, the controller appends driver
    /// events (`bead_branch_pushed`, `merge_ok`, `tree_not_clean`,
    /// `retry_dispatch`, …) into the current bead's `.jsonl` so the
    /// dispatch-to-dispatch gap surfaces in the same file as the agent's
    /// own events. `None` is a silent no-op for tests that don't wire
    /// the phase log.
    logs_root: Option<PathBuf>,
    /// `[loom]` block snapshot. The sccache fields are consulted at every
    /// dispatch to decide whether the bead container picks up the shared
    /// cache mount + env. `LoomTopConfig::default()` is harmless — both
    /// sccache fields are `None`/`/sccache` so no mount is emitted.
    loom_cfg: LoomTopConfig,
    skills_cfg: SkillsConfig,
    /// State for the bead currently being processed by `run_bead`. The
    /// envelope builder is shared across every driver event emitted
    /// during one attempt so seq stays strictly increasing across the
    /// post-session merge/push/cleanup window. Reset at the start of
    /// each `run_bead` call.
    current_emit: Option<BeadEmit>,
    /// Integration tip the current bead's ff-merge advanced past, captured
    /// in `run_bead` only when the merge actually moved the integration
    /// branch. `exec_per_bead_gate`'s audit-fail rollback consumes it: a
    /// bead that added no commit leaves this `None`, so the rollback is
    /// skipped rather than unwinding a prior bead's commit
    /// (`specs/harness.md` § Verdict Gate — `post-integrate-fail`).
    pre_integration_tip: Option<GitOid>,
    /// Attempt number rendered into the current bead's prompt and copied
    /// into durable post-integrate gate logs.
    current_attempt: u32,
    /// Bead workspace awaiting lifecycle cleanup after its integrated diff
    /// clears the deterministic per-bead gate.
    current_worktree_path: Option<PathBuf>,
    fixed_queue: Option<VecDeque<Bead>>,
    infra_queue: VecDeque<Bead>,
    infra_queue_loaded: bool,
    /// Optional work-epic scope for active/explicit epic loops. When set,
    /// ready lookup is constrained to descendants of this work root rather
    /// than to a single `spec:<label>` so multi-spec todo batches can run.
    ready_parent: Option<BeadId>,
    /// Molecule selected by the CLI work root. Explicit epic roots use their
    /// own id; explicit task roots use their Beads parent.
    handoff_molecule: Option<MoleculeId>,
}

impl<S, F, R: CommandRunner> ProductionAgentLoopController<S, F, R>
where
    S: Fn(SpawnConfig, BeadId) -> F + Send,
    F: std::future::Future<Output = (SessionResult, Option<ExitSignal>)> + Send,
{
    #[expect(clippy::too_many_arguments, reason = "controller construction surface")]
    pub fn new(
        bd: BdClient<R>,
        label: SpecLabel,
        loom_bin: PathBuf,
        workspace: PathBuf,
        git: GitClient,
        manifest: Arc<ProfileImageManifest>,
        cli_profile: Option<ProfileName>,
        phase_default: ProfileName,
        spawn: S,
    ) -> Self {
        Self {
            bd,
            label,
            loom_bin,
            beads_push_bin: PathBuf::from("beads-push"),
            workspace,
            git,
            manifest,
            cli_profile,
            phase_default,
            runtime: AgentRuntime::Pi,
            spawn,
            lock: None,
            style_rules: "docs/style-rules.md".to_string(),
            stashed_previous_failure: None,
            stashed_review_concern: None,
            logs_root: None,
            current_emit: None,
            loom_cfg: LoomTopConfig::default(),
            skills_cfg: SkillsConfig::default(),
            pre_integration_tip: None,
            current_attempt: 0,
            current_worktree_path: None,
            fixed_queue: None,
            infra_queue: VecDeque::new(),
            infra_queue_loaded: false,
            ready_parent: None,
            handoff_molecule: None,
        }
    }

    pub fn with_beads_push_bin(mut self, path: PathBuf) -> Self {
        self.beads_push_bin = path;
        self
    }

    pub fn with_fixed_queue(mut self, queue: VecDeque<Bead>) -> Self {
        self.fixed_queue = Some(queue);
        self
    }

    pub fn with_ready_parent(mut self, parent: BeadId) -> Self {
        self.ready_parent = Some(parent);
        self
    }

    pub fn with_handoff_molecule(mut self, molecule: MoleculeId) -> Self {
        self.handoff_molecule = Some(molecule);
        self
    }

    /// Snapshot the `[loom]` config block onto the controller so the
    /// per-bead dispatch picks up the shared sccache mount + env when
    /// [`LoomTopConfig::sccache_dir`] is set. Defaults to
    /// `LoomTopConfig::default()` when unset, which emits no mount.
    pub fn with_loom_config(mut self, cfg: LoomTopConfig) -> Self {
        self.loom_cfg = cfg;
        self
    }

    pub fn with_skills_config(mut self, cfg: SkillsConfig) -> Self {
        self.skills_cfg = cfg;
        self
    }

    /// Pin the per-bead JSONL log root the controller appends
    /// driver-side merge/push/cleanup events into. Production callers
    /// thread `<workspace>/.loom/logs`; tests that don't exercise
    /// the driver-event channel leave it unset and the emit path is a
    /// silent no-op.
    pub fn with_phase_log_root(mut self, logs_root: PathBuf) -> Self {
        self.logs_root = Some(logs_root);
        self
    }

    /// Hand the work-root lock to the controller so `exec_review` can drop it
    /// before spawning child `loom gate` commands that acquire the same lock.
    pub fn with_handoff_lock(mut self, guard: LockGuard) -> Self {
        self.lock = Some(guard);
        self
    }

    fn release_handoff_lock_for_child_gate(&mut self) {
        self.lock.take();
    }

    async fn load_infra_queue(&mut self) -> Result<(), LoopError> {
        if self.infra_queue_loaded {
            return Ok(());
        }
        let beads = self
            .bd
            .list(ListOpts {
                status: Some("blocked".to_string()),
                label: self
                    .ready_parent
                    .is_none()
                    .then(|| self.spec_label_filter()),
                label_any: vec!["loom:infra".to_string()],
                parent: self.ready_parent.clone(),
                ..ListOpts::default()
            })
            .await?;
        self.infra_queue = beads.into_iter().collect();
        self.infra_queue_loaded = true;
        Ok(())
    }

    /// Override the style-rules pin used in the rendered run prompt.
    /// Production callers thread this from `LoomConfig.style_rules`; tests
    /// rely on the built-in default.
    pub fn with_style_rules(mut self, path: String) -> Self {
        self.style_rules = path;
        self
    }

    pub fn with_agent_runtime(mut self, runtime: AgentRuntime) -> Self {
        self.runtime = runtime;
        self
    }

    fn spec_label_filter(&self) -> String {
        format!("spec:{}", self.label.as_str())
    }

    /// Resolve the per-bead log file the spawn closure just finished
    /// writing to and seed the driver-event emit channel so subsequent
    /// merge/push/cleanup events land in the same JSONL. Silent no-op
    /// when no log root is configured or the file is not resolvable.
    fn prepare_emit_state(&mut self, bead: &BeadId) {
        self.current_emit = self
            .logs_root
            .as_deref()
            .and_then(|root| BeadEmit::for_bead(root, &self.label, bead));
    }

    /// Append a single driver event to the current bead's log file.
    /// Silent no-op when [`Self::current_emit`] is unset.
    fn emit_to_log(&mut self, kind: DriverKind, summary: &str, mut payload: serde_json::Value) {
        if let Some(state) = self.current_emit.as_mut() {
            if matches!(kind, DriverKind::ClarifyDowngraded)
                && let Some(object) = payload.as_object_mut()
            {
                object.insert(
                    "event_sequence".to_string(),
                    serde_json::json!(state.builder.current_seq()),
                );
                object.insert(
                    "gate_log_path".to_string(),
                    serde_json::json!(state.log_path.to_string_lossy()),
                );
            }
            state.emit(kind, summary, payload);
        }
    }

    fn emit_payload(&mut self, event: DriverEventPayload) {
        self.emit_to_log(event.driver_kind, &event.summary, event.payload);
    }

    /// Run one `git verify-commit` pass over `range` in the loom
    /// workspace via the shared [`verify_pass`] helper. `pass` is
    /// [`VerifyPass::Worker`] (pass 1, fetched commits) or
    /// [`VerifyPass::Driver`] (pass 2, rebased commits) — it rides into
    /// the `signature-verification-failed` detail so the operator knows
    /// whether to investigate the wrix container's signing setup or the
    /// loom-workspace gitconfig + key resolution.
    ///
    /// Returns `Ok(None)` when the pass verified (or was skipped because
    /// no signing key resolved). Returns `Ok(Some(SignatureVerificationFailed))`
    /// on a rejected signature — the transient `loom/<id>` ref is deleted
    /// unconditionally first so a later dispatch's fetch starts clean.
    async fn verify_or_block(
        &mut self,
        bead: &BeadId,
        worktree: &CreatedWorktree,
        range: &str,
        pass: VerifyPass,
    ) -> Result<Option<AgentOutcome>, LoopError> {
        // Disjoint field borrows: `&self.git` (shared) alongside
        // `self.current_emit.as_mut()` (exclusive) is sound because they
        // are distinct fields accessed directly rather than through a
        // method.
        let reason = verify_pass(
            &self.git,
            self.current_emit.as_mut(),
            bead,
            &worktree.branch,
            range,
            pass,
        )
        .await?;
        Ok(reason.map(|detail| AgentOutcome::SignatureVerificationFailed { detail }))
    }

    async fn rollback_if_bead_advanced_integration(&mut self) -> Result<bool, LoopError> {
        let advanced = self.pre_integration_tip.take().is_some();
        if advanced {
            self.git.rollback_integration().await?;
        }
        Ok(advanced)
    }

    async fn reap_closed_worktree(&mut self, bead: &BeadId) -> Result<(), LoopError> {
        let Some(path) = self.current_worktree_path.clone() else {
            return Ok(());
        };
        if self.bd.show(bead).await?.status != "closed" {
            return Ok(());
        }
        self.git.remove_worktree(&path).await?;
        self.current_worktree_path = None;
        self.emit_to_log(
            DriverKind::WorktreeCleanupOk,
            &format!("closed bead workspace reaped for {bead}"),
            serde_json::json!({
                "bead_id": bead.to_string(),
                "worktree_path": path.to_string_lossy(),
            }),
        );
        Ok(())
    }
}

impl<S, F, R: CommandRunner> AgentLoopController for ProductionAgentLoopController<S, F, R>
where
    S: Fn(SpawnConfig, BeadId) -> F + Send,
    F: std::future::Future<Output = (SessionResult, Option<ExitSignal>)> + Send,
{
    async fn next_ready_bead(&mut self, deferred: &[BeadId]) -> Result<Option<Bead>, LoopError> {
        if let Some(queue) = self.fixed_queue.as_mut() {
            return Ok(queue.pop_front());
        }
        // Dedup of parked beads primarily relies on their paired blocked or
        // deferred status. The explicit label guard below catches stale or
        // partially-written `loom:*` parked state, so no exclude-label flag is
        // needed here.
        //
        // Epic-typed beads are skipped: workers dispatch leaf work, not
        // molecule containers. A stray ready epic surfaces as one info-log
        // line per skip so the operator sees the routing decision.
        let beads = self
            .bd
            .ready(ReadyOpts {
                limit: Some(8),
                label: self
                    .ready_parent
                    .is_none()
                    .then(|| self.spec_label_filter()),
                parent: self.ready_parent.clone(),
                exclude_label: vec![],
            })
            .await?;
        for bead in beads {
            if deferred.iter().any(|id| id == &bead.id) {
                continue;
            }
            if bead.issue_type == "epic" {
                info!(
                    bead = %bead.id,
                    spec = %self.label,
                    "loom loop: skipping epic-typed ready bead — workers dispatch leaves only",
                );
                continue;
            }
            if bead_has_parked_state(&bead) {
                info!(
                    bead = %bead.id,
                    spec = %self.label,
                    "loom loop: skipping parked ready bead",
                );
                continue;
            }
            let parent_is_parked = if let Some(parent_id) = bead.parent.clone() {
                let parent = self.bd.show(&parent_id).await?;
                bead_has_parked_state(&parent)
            } else {
                false
            };
            if parent_is_parked {
                info!(
                    bead = %bead.id,
                    spec = %self.label,
                    "loom loop: skipping parked ready bead",
                );
                continue;
            }
            return Ok(Some(bead));
        }
        self.load_infra_queue().await?;
        while let Some(bead) = self.infra_queue.pop_front() {
            if deferred.iter().any(|id| id == &bead.id) {
                continue;
            }
            if bead.issue_type == "epic" {
                info!(
                    bead = %bead.id,
                    spec = %self.label,
                    "loom loop: skipping epic-typed infra bead — workers dispatch leaves only",
                );
                continue;
            }
            return Ok(Some(bead));
        }
        Ok(None)
    }

    async fn run_bead(
        &mut self,
        bead: &Bead,
        previous_failure: Option<String>,
    ) -> Result<AgentOutcome, LoopError> {
        self.current_emit = None;
        self.current_worktree_path = None;
        if bead.labels.iter().any(loom_driver::bd::Label::is_infra) {
            self.bd
                .update(
                    &bead.id,
                    UpdateOpts {
                        status: Some("open".to_string()),
                        remove_labels: vec!["loom:infra".to_string()],
                        ..UpdateOpts::default()
                    },
                )
                .await?;
        }
        let banner = format!("loom loop @ {}", bead.id);
        let is_retry = previous_failure.is_some();
        // The stash is per-retry-sequence: a fresh dispatch
        // (`previous_failure = None`) means any leftover variant from a
        // prior bead's retry chain is stale. Resolve typed `PreviousFailure`
        // by preferring the stashed variant (set by the tree-not-clean
        // dispatcher) over the opaque runner-supplied error string.
        // The molecule-scoped review-concern stash (set by `exec_review`
        // when the review walk emits ≥1 `LOOM_FINDING:` + a
        // `LOOM_CONCERN:` terminator per `specs/harness.md`
        // § *molecule_completion_review_threads_findings_into_previous_failure_review_concern*)
        // is consumed first regardless of `is_retry` — fix-up beads
        // typically arrive as fresh dispatches, not retries, and the
        // parsed `Vec<Finding>` must ride into the next prompt either way.
        let typed_previous_failure = if let Some(concern) = self.stashed_review_concern.take() {
            Some(concern)
        } else if is_retry {
            self.stashed_previous_failure
                .take()
                .or_else(|| previous_failure.map(PreviousFailure::from_agent_error))
        } else {
            self.stashed_previous_failure = None;
            None
        };
        // Dispatch each bead against its own clone-backed workspace under
        // `.loom/beads/<bead-id>/` (per `specs/harness.md`
        // § Bead dispatch — Path A). The clone's `.git/` is a regular
        // directory inside the bind-mounted path, so workers inside the
        // wrix container can commit and the driver can fold the work
        // back via push + merge_branch on clean exit.
        let worktree = self.git.create_worktree(&self.label, &bead.id).await?;
        self.current_worktree_path = Some(worktree.path.clone());
        info!(
            bead = %bead.id,
            path = %worktree.path.display(),
            branch = %worktree.branch,
            "dispatching agent against per-bead workspace",
        );
        let preparation = match self.git.prepare_bead_clone(&worktree.path, &bead.id).await {
            Ok(preparation) => preparation,
            Err(source) => {
                return Ok(AgentOutcome::StaticInfra {
                    cause: WORKSPACE_RECOVERY_FAILED_CAUSE.to_string(),
                    error: format!(
                        "workspace recovery pre-dispatch prepare failed before cleanup: {source}"
                    ),
                });
            }
        };
        let workspace_recovery = workspace_recovery_from_preparation(&preparation);
        let workspace_recovery_event = workspace_recovery
            .as_ref()
            .map(|recovery| workspace_recovery_event_draft(&bead.id, recovery));

        let key = resolve_scratch_key(
            Phase::Loop,
            std::slice::from_ref(&self.label),
            Some(&bead.id),
        );
        let scratchpad_path =
            loom_driver::scratch::ScratchSession::scratchpad_path_for(&worktree.path, &key);
        let scratch_dir = scratchpad_path.parent().ok_or_else(|| {
            LoopError::Protocol(ProtocolError::Io(std::io::Error::other(
                "scratchpad path has no parent",
            )))
        })?;
        let bead_git = GitClient::open(&worktree.path)?;
        let tracked_files = bead_git.tracked_files().await?;
        let skill_profile = super::profile::resolve_profile(
            &bead.labels,
            self.cli_profile.as_ref(),
            &self.phase_default,
        );
        let skill_plan = SkillPlan::resolve(
            &worktree.path,
            &tracked_files,
            Phase::Loop.as_str(),
            &skill_profile,
            self.runtime,
            &self.skills_cfg,
        )?;
        let skill_session = skill_plan.materialize(scratch_dir, &worktree.path)?;
        let attempt = if is_retry {
            self.current_attempt.saturating_add(1)
        } else {
            0
        };
        self.current_attempt = attempt;
        let prompt_scratchpad_path = container_workspace_path(&worktree.path, &scratchpad_path);
        let initial_prompt = match render_loop_prompt(LoopContextInputs {
            label: self.label.clone(),
            spec_path: format!("specs/{}.md", self.label.as_str()),
            pinned_context: String::new(),
            companion_paths: vec![],
            molecule_id: None,
            issue_id: bead.id.clone(),
            title: bead.title.clone(),
            description: bead.description.clone(),
            previous_failure: typed_previous_failure,
            workspace_recovery,
            review_notes: None,
            attempt,
            scratchpad_path: prompt_scratchpad_path.to_string_lossy().into_owned(),
            style_rules: self.style_rules.clone(),
            skill_index: skill_session.skill_index,
        }) {
            Ok(p) => p,
            Err(e) => {
                return Err(LoopError::Protocol(ProtocolError::Io(
                    std::io::Error::other(e),
                )));
            }
        };
        let scratch = match loom_driver::scratch::ScratchSession::open(
            &worktree.path,
            &key,
            &initial_prompt,
            &banner,
        ) {
            Ok(s) => s,
            Err(source) => {
                return Err(LoopError::Protocol(ProtocolError::Io(source)));
            }
        };
        let mut mounts: Vec<_> = dolt_socket_mount(&self.workspace).into_iter().collect();
        if let Some(spec) = sccache_mount(&self.loom_cfg)
            .map_err(|source| LoopError::Protocol(ProtocolError::Io(source)))?
        {
            mounts.push(spec);
        }
        let extra_env = self.loom_cfg.container_sccache_env();
        // Host key paths handed to the `wrix spawn` launcher so it mounts
        // the deploy + signing keys into the bead container; the agent boots
        // without git keys otherwise (`specs/harness.md` § Commit signing).
        let launcher_env = self.git.launcher_key_env()?;
        let mut spawn_config = match build_spawn_config_from_manifest(
            &self.manifest,
            bead,
            self.cli_profile.as_ref(),
            &self.phase_default,
            self.runtime,
            worktree.path.clone(),
            initial_prompt,
            scratch.path().to_path_buf(),
            extra_env,
            vec![],
            mounts,
            launcher_env,
        ) {
            Ok(cfg) => cfg,
            Err(ProfileError::UnknownProfile { name, .. }) => {
                drop(scratch);
                return Ok(AgentOutcome::UnknownProfile {
                    error: format_unknown_profile_error(&name, &self.manifest),
                });
            }
            Err(ProfileError::UnknownRuntimeForProfile {
                profile,
                runtime,
                declared_runtimes,
                ..
            }) => {
                drop(scratch);
                return Ok(AgentOutcome::UnknownRuntimeForProfile {
                    error: format_unknown_runtime_for_profile_error(
                        &profile,
                        runtime,
                        &declared_runtimes,
                    ),
                });
            }
            Err(e @ ProfileError::InvalidSpawnConfig { .. })
            | Err(e @ ProfileError::RuntimeMetadataMismatch { .. }) => {
                drop(scratch);
                return Ok(AgentOutcome::StaticInfra {
                    cause: INVALID_SPAWN_CONFIG_CAUSE.to_string(),
                    error: e.to_string(),
                });
            }
            Err(e) => {
                drop(scratch);
                return Err(LoopError::Profile(e));
            }
        };
        let skill_session = skill_plan.materialize(scratch.path(), &worktree.path)?;
        spawn_config.skills = Some(skill_session.registered);
        spawn_config.event_metadata = Some(AgentStartMetadata {
            title: bead.title.clone(),
            profile: super::profile::resolve_profile(
                &bead.labels,
                self.cli_profile.as_ref(),
                &self.phase_default,
            ),
            spec_label: self.label.clone(),
            parent_tool_call_id: None,
        });
        info!(
            bead = %bead.id,
            image_ref = %spawn_config.image_ref,
            worktree = %worktree.path.display(),
            retry = is_retry,
            "loom loop: dispatching agent",
        );
        let (session, marker) = (self.spawn)(spawn_config, bead.id.clone()).await;
        let marker_is_noop = matches!(marker.as_ref(), Some(ExitSignal::Noop));
        let marker_for_event = marker.clone();
        let session_exit_code = session_exit_code(&session);
        drop(scratch);
        // Resolve the per-bead log file the closure just finished writing
        // to so subsequent driver events (tree-not-clean / merge / push /
        // cleanup) land in the same JSONL the agent's events live in.
        self.prepare_emit_state(&bead.id);
        if let Some(event) = workspace_recovery_event {
            self.emit_to_log(DriverKind::WorkspaceRecovery, &event.summary, event.payload);
        }

        let outcome = classify_session(session, marker);
        self.emit_to_log(
            DriverKind::MarkerRouted,
            &format!(
                "terminal marker {} routed to {} for bead {}",
                marker_name(marker_for_event.as_ref()),
                agent_outcome_route(&outcome),
                bead.id,
            ),
            serde_json::json!({
                "source_route": "loop-marker",
                "identity": marker_name(marker_for_event.as_ref()),
                "route": agent_outcome_route(&outcome),
                "bead_id": bead.id.to_string(),
                "exit_code": session_exit_code,
            }),
        );
        if outcome == AgentOutcome::Success {
            // Tree-clean precedes verify-fail / review-concern per
            // `specs/harness.md` § Verdict Gate. The pre-attempt
            // `prepare_bead_clone` path stashes preserved dirty work and
            // resets non-conflicted dispatches, so any dirty porcelain
            // entry is necessarily an agent leftover, unresolved recovery
            // conflict, or prepare-step bug — running verifiers against a
            // half-staged tree would conflate the agent's intended
            // diff with its leftover scratch.
            let porcelain = self.git.status_porcelain_at(&worktree.path).await?;
            let dirty = dirty_paths_from_porcelain(&porcelain);
            if !dirty.is_empty() {
                warn!(
                    bead = %bead.id,
                    dirty_count = dirty.len(),
                    "tree-not-clean: preserving worktree and stashing TreeNotClean for the next attempt",
                );
                self.emit_to_log(
                    DriverKind::TreeNotClean,
                    &format!(
                        "tree-not-clean: {} dirty path(s) for bead {}",
                        dirty.len(),
                        bead.id,
                    ),
                    serde_json::json!({
                        "bead_id": bead.id.to_string(),
                        "dirty_paths": dirty,
                    }),
                );
                // The bead workspace persists on every non-merged exit
                // (`specs/harness.md` § Verdict Gate — the per-bead-close
                // lifecycle preserves the clone until `bd close`). The next
                // attempt reuses it via the idempotent `create_worktree` +
                // `prepare_bead_clone`, which preserves these dirty leftovers
                // in recovery context before cleanup while keeping any
                // committed work on the bead branch.
                self.stashed_previous_failure =
                    Some(PreviousFailure::TreeNotClean { dirty_paths: dirty });
                return Ok(AgentOutcome::Failure {
                    error: "tree-not-clean".to_string(),
                });
            }
            // A3: the worker never pushes; the driver fetches the bead
            // branch from the bead workspace path into the loom workspace
            // so `merge_branch` can rebase + ff it onto the integration
            // branch.
            self.git.fetch_bead_branch(&worktree.path, &bead.id).await?;
            self.emit_to_log(
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
            // Verify signatures (pass 1) on the fetched worker commits.
            // Conditional on a signing key resolving (allowed_signers file
            // present); skipped otherwise. A rejected signature routes the
            // bead to `loom:blocked` (worker-side) — the transient
            // `loom/<id>` ref is deleted unconditionally first so a retry's
            // fetch starts clean.
            let integration_branch = self.git.integration_branch().to_string();
            let pass1_range = format!("{integration_branch}..{}", worktree.branch);
            if let Some(outcome) = self
                .verify_or_block(&bead.id, &worktree, &pass1_range, VerifyPass::Worker)
                .await?
            {
                return Ok(outcome);
            }
            // Rebase the bead branch onto the integration tip (rerere
            // replays any recorded conflict resolution). Pass-2
            // verification runs on the rewritten commits BEFORE the
            // ff-merge, matching the spec recipe (specs/harness.md
            // § Bead dispatch — Bead branch flow; § Verdict Gate phases
            // 3-4): a driver-side signature failure leaves the integration
            // branch untouched because the ff-merge has not run yet.
            match self.git.rebase_onto_integration(&worktree.branch).await? {
                RebaseOutcome::Rebased => {
                    // Verify signatures (pass 2) on the rewritten commits
                    // the rebase produced, before the ff-merge. The range
                    // is `<integration-branch>..<branch>` evaluated in the
                    // loom workspace where the rebase ran — not the
                    // operator workdir's HEAD. Conditional on the same
                    // signing-key resolution as pass 1; a rejected
                    // signature here means loom's own (driver-side) signing
                    // setup is broken. `verify_or_block` deletes the
                    // transient ref, so a failure rolls nothing onto the
                    // integration line.
                    let pass2_range = format!("{integration_branch}..{}", worktree.branch);
                    if let Some(outcome) = self
                        .verify_or_block(&bead.id, &worktree, &pass2_range, VerifyPass::Driver)
                        .await?
                    {
                        return Ok(outcome);
                    }
                    let pre_tip = self.git.integration_commit_sha().await?;
                    self.git.ff_merge_integration(&worktree.branch).await?;
                    let main_sha = self.git.integration_commit_sha().await?;
                    if main_sha == pre_tip {
                        self.git.delete_branch(&worktree.branch).await?;
                        if marker_is_noop {
                            self.emit_to_log(
                                DriverKind::VerdictGate,
                                &format!("noop: bead {} did not advance integration", bead.id),
                                serde_json::json!({
                                    "bead_id": bead.id.to_string(),
                                    "branch": worktree.branch,
                                    "worktree_path": worktree.path.to_string_lossy(),
                                    "cause": "noop",
                                }),
                            );
                            return Ok(AgentOutcome::Noop);
                        }
                        let detail = format!(
                            "{bead_id} emitted success but branch {branch} did not advance {integration_branch}; preserved workspace {path}",
                            bead_id = bead.id,
                            branch = worktree.branch,
                            path = worktree.path.display(),
                        );
                        self.emit_to_log(
                            DriverKind::VerdictGate,
                            &format!(
                                "zero-progress: bead {} did not advance integration",
                                bead.id
                            ),
                            serde_json::json!({
                                "bead_id": bead.id.to_string(),
                                "branch": worktree.branch,
                                "worktree_path": worktree.path.to_string_lossy(),
                                "cause": "zero-progress",
                            }),
                        );
                        return Ok(AgentOutcome::ZeroProgress { detail });
                    }
                    self.pre_integration_tip = Some(pre_tip);
                    self.emit_to_log(
                        DriverKind::MergeOk,
                        &format!("merge ok: {} → main", worktree.branch),
                        serde_json::json!({
                            "bead_id": bead.id.to_string(),
                            "branch": worktree.branch,
                            "main_sha": main_sha.to_string(),
                        }),
                    );
                    self.git.delete_branch(&worktree.branch).await?;
                    self.emit_to_log(
                        DriverKind::Other("bead_workspace_preserved".to_string()),
                        &format!(
                            "bead workspace preserved through per-bead gate for {}",
                            bead.id
                        ),
                        serde_json::json!({
                            "bead_id": bead.id.to_string(),
                            "branch": worktree.branch,
                            "worktree_path": worktree.path.to_string_lossy(),
                        }),
                    );
                    Ok(AgentOutcome::Success)
                }
                RebaseOutcome::Conflict {
                    detail,
                    files,
                    new_base_sha,
                } => {
                    // Rebase conflict. The bead workspace is preserved
                    // (per the per-bead-close lifecycle); the transient
                    // loom-workspace `loom/<id>` ref is deleted
                    // unconditionally so the integration-conflict retry's
                    // fetch starts clean (a rebased bead branch would not
                    // fast-forward the stale ref). Routes to a single
                    // integration-conflict retry — `run_loop` threads the
                    // stashed typed `IntegrationConflict` into the next
                    // dispatch; a second conflict escalates to clarify.
                    warn!(
                        bead = %bead.id,
                        branch = %worktree.branch,
                        path = %worktree.path.display(),
                        detail = %detail,
                        files = files.len(),
                        new_base = %new_base_sha,
                        "rebase conflict — bead workspace preserved, routing to integration-conflict recovery",
                    );
                    self.emit_to_log(
                        DriverKind::IntegrationConflict,
                        &format!("rebase conflict: {}", worktree.branch),
                        serde_json::json!({
                            "bead_id": bead.id.to_string(),
                            "branch": worktree.branch,
                            "worktree_path": worktree.path.to_string_lossy(),
                            "detail": detail,
                            "new_base_sha": new_base_sha.as_str(),
                            "files": files.iter().map(|f| f.to_string_lossy()).collect::<Vec<_>>(),
                        }),
                    );
                    self.git.delete_branch(&worktree.branch).await?;
                    self.stashed_previous_failure = Some(PreviousFailure::IntegrationConflict {
                        files: files.clone(),
                        new_base_sha: new_base_sha.clone(),
                    });
                    Ok(AgentOutcome::IntegrationConflict {
                        files,
                        new_base_sha,
                    })
                }
            }
        } else {
            // Stash typed `PreviousFailure::AgentRetry { reason }` so the
            // next attempt's `run_bead` resolves the rich variant from
            // `stashed_previous_failure` (consumed via `.take()`) rather
            // than the opaque `PreviousFailure::from_agent_error`
            // fallback. Mirrors the tree-not-clean stash pattern.
            if let AgentOutcome::Retry { reason } = &outcome {
                self.stashed_previous_failure = Some(PreviousFailure::AgentRetry {
                    reason: reason.clone(),
                });
            }
            // Preserve the bead workspace (and any staged-but-uncommitted
            // diff) on every non-merged exit — agent failure, retry, block,
            // or clarify. The per-bead-close lifecycle reaps it at `bd close`
            // (`GitClient::sweep_orphan_bead_clones`); a retry reuses it via
            // the idempotent `create_worktree`. Removing it here would force a
            // full re-implementation of an agent that blocked mid-edit
            // (`specs/harness.md` § Verdict Gate — workspace persists on all
            // failure paths).
            Ok(outcome)
        }
    }

    async fn apply_clarify(&mut self, bead: &BeadId, question: &str) -> Result<(), LoopError> {
        // Driver-authored clarify (e.g. the integration-conflict
        // escalation): when `question` is itself a well-formed
        // `## Options — …` block, the driver — not the agent — is the
        // author, so persist it to bead state before the validation
        // pass. The agent-self-report path passes a plain question with
        // no options block; the guard skips persisting and the agent's
        // own prior write is what gets validated.
        if loom_protocol::gate::options::has_well_formed_block(question) {
            self.bd
                .update(
                    bead,
                    UpdateOpts {
                        notes: Some(question.to_string()),
                        ..UpdateOpts::default()
                    },
                )
                .await?;
            self.emit_to_log(
                DriverKind::BdStateTransition,
                &format!("Beads notes updated with clarify options for {bead}"),
                serde_json::json!({
                    "source_route": "loop-marker",
                    "identity": "LOOM_CLARIFY",
                    "bead_id": bead,
                    "mutation": "update",
                    "notes": "clarify-options",
                }),
            );
        }
        // Verdict-gate direct-emit LOOM_CLARIFY check (specs/gate.md §
        // Options Format Contract): inspect the bead under dispatch for a
        // well-formed `## Options — …` block. Well-formed → loom:clarify;
        // malformed / absent → loom:blocked with cause
        // `clarify-without-options` so `loom inbox`'s queue is not handed
        // an empty options block.
        let report = crate::gate_clarify::apply_clarify_or_blocked_report(&self.bd, bead).await?;
        let context = crate::gate_clarify::ClarifyRouteContext {
            source_route: crate::gate_clarify::ClarifySourceRoute::LoopMarker,
            identity: "LOOM_CLARIFY".to_string(),
            gate_log_path: self.current_emit.as_ref().map(|emit| emit.log_path.clone()),
        };
        for event in report.routing_events(bead, &context) {
            self.emit_payload(event);
        }
        Ok(())
    }

    async fn apply_blocked(
        &mut self,
        bead: &BeadId,
        cause: &str,
        error: &str,
    ) -> Result<(), LoopError> {
        let notes = diagnostic_notes(cause, error);
        self.bd
            .update(
                bead,
                UpdateOpts {
                    status: Some("blocked".to_string()),
                    add_labels: vec!["loom:blocked".to_string()],
                    notes: Some(notes),
                    ..UpdateOpts::default()
                },
            )
            .await?;
        self.emit_to_log(
            DriverKind::BdStateTransition,
            &format!("Beads state updated for {bead}: loom:blocked"),
            serde_json::json!({
                "source_route": "loop-marker",
                "identity": "LOOM_BLOCKED",
                "bead_id": bead,
                "mutation": "update",
                "status": "blocked",
                "added_labels": ["loom:blocked"],
                "notes_cause": cause,
            }),
        );
        Ok(())
    }

    async fn apply_infra(
        &mut self,
        bead: &BeadId,
        diagnostic: &InfraDiagnostic,
    ) -> Result<(), LoopError> {
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
        self.bd
            .update(
                bead,
                UpdateOpts {
                    status: Some("blocked".to_string()),
                    add_labels: vec!["loom:infra".to_string()],
                    notes: Some(diagnostic_notes(&diagnostic.cause, &diagnostic.error)),
                    set_metadata: metadata,
                    ..UpdateOpts::default()
                },
            )
            .await?;
        self.emit_to_log(
            DriverKind::BdStateTransition,
            &format!("Beads state updated for {bead}: loom:infra"),
            serde_json::json!({
                "source_route": "loop-infra",
                "identity": diagnostic.cause,
                "bead_id": bead,
                "mutation": "update",
                "status": "blocked",
                "added_labels": ["loom:infra"],
                "notes_cause": diagnostic.cause,
            }),
        );
        Ok(())
    }

    async fn exec_review(&mut self) -> Result<HandoffEvidence, LoopError> {
        let handoff = execute_molecule_push_gate(
            &self.bd,
            &self.label,
            self.handoff_molecule.as_ref(),
            &self.loom_bin,
            &self.beads_push_bin,
            &self.workspace,
            &self.git,
        )
        .await?;
        self.stashed_review_concern = handoff.review_concern;
        Ok(handoff.evidence)
    }

    async fn exec_per_bead_gate(&mut self, bead: &BeadId) -> Result<PerBeadGateOutcome, LoopError> {
        self.release_handoff_lock_for_child_gate();
        let diff_range = self
            .pre_integration_tip
            .as_ref()
            .map_or_else(|| "HEAD..HEAD".to_owned(), |tip| format!("{tip}..HEAD"));
        let verify_args = vec![
            "gate".to_string(),
            "verify".to_string(),
            "--diff".to_string(),
            diff_range.clone(),
        ];
        let gate_workspace = self.git.loom_workspace();
        let verify_output = Command::new(&self.loom_bin)
            .current_dir(&gate_workspace)
            .args(&verify_args)
            .output()
            .await?;
        let verify_code = verify_output.status.code().unwrap_or(1);
        info!(
            bead = %bead,
            spec = %self.label.as_str(),
            exit_code = verify_code,
            "loom loop: per-bead gate — loom gate verify --diff finished",
        );
        if verify_code != 0 {
            let stderr_tail = String::from_utf8_lossy(&verify_output.stderr).to_string();
            let stdout_tail = String::from_utf8_lossy(&verify_output.stdout).to_string();
            let failure = VerifierFailure::new(
                format!("loom gate verify --diff {diff_range}"),
                verify_code,
                format!("{stdout_tail}\n{stderr_tail}"),
            );
            let integration_sha = self.git.integration_commit_sha().await?.to_string();
            let tree_oid = loom_driver::git::head_tree_oid_sync(&gate_workspace)?.to_string();
            let rollback_state = if self.pre_integration_tip.is_some() {
                "pending"
            } else {
                "skipped-no-integration-advance"
            };
            let gate_log_dir =
                gate_log_root(self.logs_root.as_deref(), &self.workspace).join(self.label.as_str());
            let gate_log_path = write_post_integrate_gate_log(
                gate_log_dir,
                PostIntegrateGateLog {
                    argv: verify_args,
                    scope: diff_range.clone(),
                    exit_code: verify_code,
                    stdout: stdout_tail.clone(),
                    stderr: stderr_tail.clone(),
                    terminal_marker: parse_exit_signal(&stdout_tail),
                    integration_sha,
                    tree_oid,
                    config_digest: pre_commit_config_digest(&gate_workspace)?,
                    bead_id: bead.clone(),
                    retry_attempt: self.current_attempt,
                    rollback_state,
                    failures: vec![failure.clone()],
                },
            )?;
            let gate_log = gate_log_path.to_string_lossy().to_string();
            let detail = format!(
                "loom gate verify --diff {diff_range} exited {verify_code}\n\
                 gate log: {gate_log}\nstdout:\n{stdout_tail}\nstderr:\n{stderr_tail}",
            );
            let rolled_back = self.rollback_if_bead_advanced_integration().await?;
            self.stashed_previous_failure = Some(PreviousFailure::PostIntegrateFail {
                failures: vec![failure],
                gate_log_path: gate_log_path.clone(),
            });
            self.emit_to_log(
                DriverKind::VerdictGate,
                &format!(
                    "post-integrate-fail: integration audit failed for bead {bead}; gate log: {gate_log}"
                ),
                serde_json::json!({
                    "bead_id": bead.to_string(),
                    "cause": "post-integrate-fail",
                    "verify_code": verify_code,
                    "rolled_back": rolled_back,
                    "gate_log_path": gate_log,
                }),
            );
            return Ok(PerBeadGateOutcome::Recovery { detail });
        }

        self.reap_closed_worktree(bead).await?;
        Ok(PerBeadGateOutcome::Clean)
    }

    async fn promote_deferred(&mut self) -> Result<StabilizationOutcome, LoopError> {
        let molecule = if let Some(molecule) = self.handoff_molecule.as_ref() {
            Some(molecule.clone())
        } else if let Some(parent) = self.ready_parent.as_ref() {
            Some(MoleculeId::new(parent.as_str()))
        } else {
            crate::resolve::resolve_open_epic(&self.bd, &self.label).await?
        };
        let Some(molecule) = molecule else {
            return Ok(StabilizationOutcome::NoDeferred);
        };
        let summary = crate::mint::promote_deferred(&self.bd, &molecule, false).await;
        for event in summary.routing_events() {
            self.emit_payload(event);
        }
        if summary.refused > 0 {
            let molecule = BeadId::new(molecule.as_str()).map_err(|_| LoopError::Bug {
                context: format!("molecule id `{molecule}` is not a bead id"),
            })?;
            return Ok(StabilizationOutcome::StructuralConflict {
                molecule,
                detail: summary.render(),
            });
        }
        if summary.errors > 0 {
            return Ok(StabilizationOutcome::TransientFailure {
                detail: summary.render(),
            });
        }
        if summary.promoted_deferred == 0 {
            Ok(StabilizationOutcome::NoDeferred)
        } else {
            Ok(StabilizationOutcome::Promoted {
                count: summary.promoted_deferred,
            })
        }
    }

    fn emit_driver_event(&mut self, kind: DriverKind, summary: &str, payload: serde_json::Value) {
        self.emit_to_log(kind, summary, payload);
    }
}

pub struct MoleculeGateHandoff {
    pub evidence: HandoffEvidence,
    pub review_concern: Option<PreviousFailure>,
    pub mint_summary: Option<crate::mint::MintSummary>,
}

pub async fn execute_molecule_push_gate<R: CommandRunner>(
    bd: &BdClient<R>,
    label: &SpecLabel,
    selected_molecule: Option<&MoleculeId>,
    loom_bin: &Path,
    beads_push_bin: &Path,
    workspace: &Path,
    git: &GitClient,
) -> Result<MoleculeGateHandoff, LoopError> {
    let molecule = match selected_molecule {
        Some(molecule) => molecule.clone(),
        None => crate::resolve::resolve_open_epic(bd, label)
            .await?
            .ok_or_else(|| LoopError::NoActiveMolecule {
                label: label.to_string(),
            })?,
    };
    let molecule_state = molecule_state(bd, &molecule).await?;
    if molecule_state != MoleculeState::Clean {
        return Ok(MoleculeGateHandoff {
            evidence: HandoffEvidence {
                molecule_state,
                ..HandoffEvidence::default()
            },
            review_concern: None,
            mint_summary: None,
        });
    }

    let actual = git.prepare_actual_push_range().await?;
    let gate_workspace = git.loom_workspace();
    let diff_range = actual.range.clone();
    let config_digest = pre_commit_config_digest(&gate_workspace)?;
    let hook_coverage = loom_gate::pre_push_hook_coverage_from_config(&gate_workspace)?;
    let verify_log_path = allocate_gate_log_path(&gate_workspace, "push-verify")?;
    let verify_result = git.run_pre_push_chain().await;
    let verify_run = match &verify_result {
        Ok(()) => GateRun::successful_verify(
            diff_range.clone(),
            actual.tree_oid.to_string(),
            config_digest.clone(),
            verify_log_path.clone(),
            hook_coverage,
        ),
        Err(_) => GateRun {
            phase: GatePhase::Verify,
            push_range: diff_range.clone(),
            tree_oid: actual.tree_oid.to_string(),
            config_digest: config_digest.clone(),
            log_path: verify_log_path.clone(),
            exit_code: Some(1),
            status: GateRunStatus::Failed,
            marker: None,
            covered_hooks: Vec::new(),
        },
    };
    append_gate_run_lifecycle_events(&verify_log_path, &verify_run)?;
    if let Err(error) = verify_result {
        warn!(
            spec = %label,
            error = ?error,
            gate_log_path = %verify_log_path.display(),
            "loom loop: molecule push gate refused by pre-push verification",
        );
        let mut evidence = HandoffEvidence::from_runs(vec![verify_run]);
        evidence.molecule_state = MoleculeState::Clean;
        return Ok(MoleculeGateHandoff {
            evidence,
            review_concern: None,
            mint_summary: None,
        });
    }

    let (review_log_path, phase_when) = allocate_review_log_path(&gate_workspace)?;
    let phase_when_millis = phase_when
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let review_output = Command::new(loom_bin)
        .current_dir(&gate_workspace)
        .env(REVIEW_PHASE_WHEN_ENV, phase_when_millis.to_string())
        .env(REVIEW_EMIT_STDOUT_ENV, "1")
        .env(REVIEW_INSPECTION_ONLY_ENV, "1")
        .env(REVIEW_VERIFIED_LOG_ENV, &verify_log_path)
        .env(REVIEW_SPEC_LABEL_ENV, label.as_str())
        .arg("gate")
        .arg("review")
        .arg("--diff")
        .arg(&diff_range)
        .output()
        .await?;
    let review_status = review_output.status;
    let review_stdout = String::from_utf8_lossy(&review_output.stdout);
    let validator = WorkspaceFindingValidator::new(&gate_workspace);
    let walk = WalkOutput::from_stdout(&review_stdout, DispatchScope::PerBead, &validator);
    let config = LoomConfig::load(LoomConfig::resolve_path(&gate_workspace))?;
    let unsuppressed_findings = walk
        .findings()
        .iter()
        .filter(|finding| !suppresses_rubric_finding(&config.suppress, finding))
        .cloned()
        .collect::<Vec<_>>();
    let suppressed_review_concern = matches!(walk.terminal(), TerminalSurface::Concern { .. })
        && !walk.findings().is_empty()
        && unsuppressed_findings.is_empty();
    let review_marker = if suppressed_review_concern {
        Some(ExitSignal::Complete)
    } else {
        parse_exit_signal(&review_stdout)
    };
    let review_concern = match walk.terminal() {
        TerminalSurface::Concern { summary } if !unsuppressed_findings.is_empty() => {
            Some(PreviousFailure::ReviewConcern {
                summary: summary.clone(),
                findings: unsuppressed_findings.clone(),
            })
        }
        _ => None,
    };
    let mint_summary = if review_concern.is_some() {
        let options = crate::mint::MintOptions {
            suppressions: config.suppress.clone(),
            suppress_closed_same_molecule: true,
            ..crate::mint::MintOptions::default()
        };
        let summary =
            crate::mint::route_molecule_findings(bd, &molecule, &unsuppressed_findings, &options)
                .await;
        if summary.errors > 0 {
            return Err(LoopError::ReviewHandoff {
                detail: summary.render(),
            });
        }
        Some(summary)
    } else {
        None
    };

    let mut runs = parse_gate_runs_from_jsonl(&verify_log_path);
    if review_log_path.exists() {
        runs.extend(parse_gate_runs_from_jsonl(&review_log_path));
    }
    let mut evidence = HandoffEvidence::from_runs(runs);
    evidence.molecule_state = if mint_summary.is_some() {
        MoleculeState::Unresolved
    } else {
        MoleculeState::Clean
    };
    evidence.push_range = Some(diff_range.clone());
    evidence.tree_oid = Some(actual.tree_oid.to_string());
    evidence.review_marker = review_marker;
    evidence.review_exit = review_status.code();
    evidence.suppressed_review_concern = suppressed_review_concern;

    if !review_status.success() {
        warn!(
            spec = %label,
            diff = %diff_range,
            exit_code = review_status.code().unwrap_or(-1),
            gate_log_path = %review_log_path.display(),
            "loom loop: molecule push gate refused by review process",
        );
        return Ok(MoleculeGateHandoff {
            evidence,
            review_concern,
            mint_summary,
        });
    }

    let success = match GateSuccess::new(&evidence, 1) {
        Ok(success) => success,
        Err(fail) => {
            info!(
                spec = %label,
                reason = ?fail.reason,
                "loom loop: molecule push gate refused by typed evidence",
            );
            return Ok(MoleculeGateHandoff {
                evidence,
                review_concern,
                mint_summary,
            });
        }
    };
    MarkerProof::mint(success, &gate_workspace, &SystemClock::new()).map_err(|error| {
        LoopError::ReviewHandoff {
            detail: format!("marker mint failed before push: {error}"),
        }
    })?;
    git.push().await?;
    let beads_output = Command::new(beads_push_bin)
        .current_dir(workspace)
        .output()
        .await?;
    if !beads_output.status.success() {
        return Err(LoopError::ReviewHandoff {
            detail: format!(
                "beads-push failed after git push: {}",
                String::from_utf8_lossy(&beads_output.stderr),
            ),
        });
    }
    Ok(MoleculeGateHandoff {
        evidence,
        review_concern,
        mint_summary,
    })
}

async fn molecule_state<R: CommandRunner>(
    bd: &BdClient<R>,
    molecule: &MoleculeId,
) -> Result<MoleculeState, LoopError> {
    let parent = BeadId::new(molecule.as_str()).map_err(|_| LoopError::Bug {
        context: format!("molecule id `{molecule}` is not a bead id"),
    })?;
    let molecule_bead = bd.show(&parent).await?;
    let progress = bd.mol_progress(molecule).await?;
    let beads = bd
        .list(ListOpts {
            status: Some("open,in_progress,blocked,deferred".to_string()),
            parent: Some(parent),
            ..ListOpts::default()
        })
        .await?;
    if bead_has_parked_state(&molecule_bead)
        || progress.completed < progress.total
        || beads.iter().any(bead_has_parked_state)
    {
        Ok(MoleculeState::Unresolved)
    } else {
        Ok(MoleculeState::Clean)
    }
}

fn allocate_gate_log_path(workspace: &Path, stem: &str) -> Result<PathBuf, LoopError> {
    let dir = workspace.join(".loom/logs/gate");
    std::fs::create_dir_all(&dir)?;
    let nanos = SystemClock::new()
        .wall_now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    for suffix in 0..1000_u16 {
        let name = if suffix == 0 {
            format!("{stem}-{nanos}.jsonl")
        } else {
            format!("{stem}-{nanos}-{suffix}.jsonl")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                drop(file);
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique molecule gate log",
    )
    .into())
}

fn allocate_review_log_path(
    workspace: &Path,
) -> Result<(PathBuf, std::time::SystemTime), LoopError> {
    let logs_root = workspace.join(".loom/logs");
    let mut phase_when = SystemClock::new().wall_now();
    let mut path = phase_log_path(&logs_root, "review", phase_when);
    while path.exists() {
        phase_when += Duration::from_secs(1);
        path = phase_log_path(&logs_root, "review", phase_when);
    }
    Ok((path, phase_when))
}

fn bead_has_parked_state(bead: &Bead) -> bool {
    bead.status == "blocked"
        || bead.labels.iter().any(|label| {
            label.is_blocked() || label.is_clarify() || label.is_deferred() || label.is_infra()
        })
}

fn diagnostic_notes(cause: &str, error: &str) -> String {
    if error.is_empty() {
        cause.to_string()
    } else {
        format!("{cause}: {error}")
    }
}

fn gate_log_root(logs_root: Option<&std::path::Path>, workspace: &std::path::Path) -> PathBuf {
    logs_root.map_or_else(
        || workspace.join(".loom/logs/gate"),
        |root| root.join("gate"),
    )
}

fn pre_commit_config_digest(workspace: &std::path::Path) -> Result<String, LoopError> {
    let path = workspace.join(".pre-commit-config.yaml");
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn write_post_integrate_gate_log(
    dir: PathBuf,
    record: PostIntegrateGateLog,
) -> Result<PathBuf, LoopError> {
    std::fs::create_dir_all(&dir)?;
    let clock = SystemClock::new();
    let stamp = clock
        .wall_now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let stem = format!(
        "{}-attempt-{}-{stamp}",
        record.bead_id.as_str(),
        record.retry_attempt,
    );
    for suffix in 0..1000_u16 {
        let file_name = if suffix == 0 {
            format!("{stem}.jsonl")
        } else {
            format!("{stem}-{suffix}.jsonl")
        };
        let path = dir.join(file_name);
        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(LoopError::Io(err)),
        };
        drop(file);
        let run = GateRun {
            phase: GatePhase::Verify,
            push_range: record.scope.clone(),
            tree_oid: record.tree_oid.clone(),
            config_digest: record.config_digest.clone(),
            log_path: path.clone(),
            exit_code: Some(record.exit_code),
            status: GateRunStatus::Failed,
            marker: record.terminal_marker.clone(),
            covered_hooks: Vec::new(),
        };
        append_gate_run_lifecycle_events(&path, &run)?;
        append_post_integrate_diagnostics(&path, &record)?;
        return Ok(path);
    }
    Err(LoopError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate unique post-integrate gate log path",
    )))
}

struct PostIntegrateGateLog {
    argv: Vec<String>,
    scope: String,
    exit_code: i32,
    stdout: String,
    stderr: String,
    terminal_marker: Option<ExitSignal>,
    integration_sha: String,
    tree_oid: String,
    config_digest: String,
    bead_id: BeadId,
    retry_attempt: u32,
    rollback_state: &'static str,
    failures: Vec<VerifierFailure>,
}

fn append_post_integrate_diagnostics(
    path: &std::path::Path,
    record: &PostIntegrateGateLog,
) -> Result<(), LoopError> {
    use std::io::Write as _;

    let contents = std::fs::read_to_string(path)?;
    let last_line = contents
        .lines()
        .next_back()
        .ok_or_else(|| std::io::Error::other("gate lifecycle log did not contain an event"))?;
    let last_event = serde_json::from_str::<AgentEvent>(last_line)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let last_envelope = last_event.envelope();
    let clock = SystemClock::new();
    let mut builder = EnvelopeBuilder::with_seq_start(
        SessionScope::phase(
            last_envelope.session_id.clone(),
            last_envelope.molecule_id.clone(),
        ),
        Source::Driver,
        last_envelope.seq + 1,
        move || {
            clock
                .wall_now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis() as i64)
        },
    );
    let failures = record
        .failures
        .iter()
        .map(|failure| {
            serde_json::json!({
                "target": failure.target,
                "exit_code": failure.exit_code,
                "stderr_tail": failure.stderr_tail,
            })
        })
        .collect::<Vec<_>>();
    let event = AgentEvent::DriverEvent {
        envelope: builder.build(),
        driver_kind: DriverKind::VerdictGate,
        summary: format!("post-integration verify failed for bead {}", record.bead_id),
        payload: serde_json::json!({
            "phase": "post_integrate_verify",
            "argv": record.argv,
            "scope_flag": "--diff",
            "scope": record.scope,
            "exit_code": record.exit_code,
            "stdout": record.stdout,
            "stderr": record.stderr,
            "terminal_marker": record.terminal_marker.as_ref().map(terminal_marker_json),
            "integration_sha": record.integration_sha,
            "bead_id": record.bead_id.to_string(),
            "retry_attempt": record.retry_attempt,
            "rollback_state": record.rollback_state,
            "failures": failures,
            "log_path": path.to_string_lossy(),
        }),
    };
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    serde_json::to_writer(&mut file, &event).map_err(std::io::Error::other)?;
    writeln!(&mut file)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

fn terminal_marker_json(marker: &ExitSignal) -> serde_json::Value {
    match marker {
        ExitSignal::Complete => serde_json::json!({ "kind": "complete" }),
        ExitSignal::Noop => serde_json::json!({ "kind": "noop" }),
        ExitSignal::Blocked { reason } => {
            serde_json::json!({ "kind": "blocked", "reason": reason })
        }
        ExitSignal::Clarify { question } => {
            serde_json::json!({ "kind": "clarify", "question": question })
        }
        ExitSignal::Retry { reason } => serde_json::json!({ "kind": "retry", "reason": reason }),
        ExitSignal::Concern { summary } => {
            serde_json::json!({ "kind": "concern", "summary": summary })
        }
        ExitSignal::BadWalk(badwalk) => {
            serde_json::json!({ "kind": "bad-walk", "detail": format!("{badwalk:?}") })
        }
    }
}

/// Render the operator-facing note body for the `unknown-profile`
/// blocked-cause path. Names the requested label as it appears on the
/// bead (`profile:X`) and the manifest's declared set in the same form,
/// so the human can relabel the bead without re-reading the manifest.
pub fn format_unknown_profile_error(
    requested: &ProfileName,
    manifest: &ProfileImageManifest,
) -> String {
    let declared: Vec<String> = manifest
        .declared_profiles()
        .map(|p| format!("profile:{p}"))
        .collect();
    let declared_part = if declared.is_empty() {
        "manifest declares no profiles".to_string()
    } else {
        format!("manifest declares: {}", declared.join(", "))
    };
    format!("requested profile:{requested} not declared; {declared_part}")
}

pub fn format_unknown_runtime_for_profile_error(
    profile: &ProfileName,
    runtime: AgentRuntime,
    declared_runtimes: &[AgentRuntime],
) -> String {
    let declared: Vec<String> = declared_runtimes
        .iter()
        .map(|runtime| runtime.as_str().to_string())
        .collect();
    let declared_part = if declared.is_empty() {
        format!("profile:{profile} declares no runtimes")
    } else {
        format!(
            "profile:{profile} declares runtimes: {}",
            declared.join(", ")
        )
    };
    format!("requested profile:{profile} runtime:{runtime} not declared; {declared_part}")
}

fn session_exit_code(session: &SessionResult) -> Option<i32> {
    match session {
        SessionResult::Complete(outcome) => Some(outcome.exit_code),
        SessionResult::PreflightFailed { .. }
        | SessionResult::MidSessionFailed { .. }
        | SessionResult::StaticInfra { .. }
        | SessionResult::ObserverAbort { .. } => None,
    }
}

pub(super) fn marker_name(marker: Option<&ExitSignal>) -> &'static str {
    marker.map_or("missing", ExitSignal::identity)
}

pub(super) fn agent_outcome_route(outcome: &AgentOutcome) -> &'static str {
    match outcome {
        AgentOutcome::Success => "success",
        AgentOutcome::Noop => "noop",
        AgentOutcome::Failure { .. } | AgentOutcome::ZeroProgress { .. } => "recovery",
        AgentOutcome::Retry { .. } => "retry",
        AgentOutcome::Blocked { .. } | AgentOutcome::SignatureVerificationFailed { .. } => {
            "blocked"
        }
        AgentOutcome::Clarify { .. } => "clarify",
        AgentOutcome::IntegrationConflict { .. } => "integration-conflict-retry",
        AgentOutcome::InfraPreflight { .. } | AgentOutcome::InfraMidSession { .. } => "infra-retry",
        AgentOutcome::StaticInfra { .. }
        | AgentOutcome::UnknownProfile { .. }
        | AgentOutcome::UnknownRuntimeForProfile { .. } => "infra-blocked",
    }
}

/// Translate a `(SessionResult, Option<ExitSignal>)` pair into an
/// [`AgentOutcome`]. Marker → outcome routing goes through the canonical
/// [`crate::review::decide`] gate function (FR12 — single source of truth);
/// `bd_closed` / `diff_empty` / verify / review observables are not queried
/// at the per-bead exit (they belong to `loom gate verify`'s deterministic
/// pass), so neutral inputs are passed and the gate's output reduces to
/// marker-only routing. A defensive guard for `LOOM_COMPLETE`/`LOOM_NOOP`
/// paired with a non-zero exit code predates the gate call because the
/// spec's decision table does not consider exit code: a marker that
/// disagrees with the kernel's view is surfaced as a failure rather than
/// trusted blindly.
pub fn classify_session(session: SessionResult, marker: Option<ExitSignal>) -> AgentOutcome {
    match session {
        SessionResult::PreflightFailed { error } => AgentOutcome::InfraPreflight { error },
        SessionResult::MidSessionFailed { error } => AgentOutcome::InfraMidSession { error },
        SessionResult::StaticInfra { cause, error } => AgentOutcome::StaticInfra { cause, error },
        SessionResult::ObserverAbort { reason } => verdict_to_outcome(
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ObserverAbort { reason },
            },
            0,
        ),
        SessionResult::Complete(outcome) => {
            if let Some(ExitSignal::Concern { summary }) = marker.as_ref() {
                return AgentOutcome::Failure {
                    error: format!(
                        "wrong-phase-marker: LOOM_CONCERN ({summary}) is review-phase only",
                    ),
                };
            }
            if matches!(marker, Some(ExitSignal::BadWalk(_))) {
                return AgentOutcome::Failure {
                    error: "wrong-phase-marker: LOOM_CONCERN is review-phase only".to_string(),
                };
            }
            if let Some(marker @ (ExitSignal::Complete | ExitSignal::Noop)) = marker.as_ref()
                && outcome.exit_code != 0
            {
                return AgentOutcome::Failure {
                    error: format!(
                        "agent emitted {} but exited code {}",
                        marker.identity(),
                        outcome.exit_code,
                    ),
                };
            }
            verdict_to_outcome(
                decide(marker.as_ref(), neutral_gate_inputs()),
                outcome.exit_code,
            )
        }
    }
}

/// Inputs threaded into [`decide`] when classifying the per-bead exit. The
/// run-phase classifier only knows the marker; bd-closed, diff, verify, and
/// review live in `loom gate verify`'s downstream pass. Passing neutral
/// defaults reduces the gate to marker-only routing — the spec table rows
/// for `COMPLETE`/`NOOP` collapse to `Done` and `None` to `SwallowedMarker`,
/// which is what the in-session classifier needs.
fn neutral_gate_inputs() -> GateInputs {
    GateInputs {
        bd_closed: true,
        diff_empty: false,
        verify_failures: vec![],
        review_flag: None,
        ..GateInputs::default()
    }
}

fn verdict_to_outcome(verdict: PhaseVerdict, exit_code: i32) -> AgentOutcome {
    match verdict {
        PhaseVerdict::Done => AgentOutcome::Success,
        PhaseVerdict::Blocked { reason } => AgentOutcome::Blocked { reason },
        PhaseVerdict::Clarify { question } => AgentOutcome::Clarify { question },
        PhaseVerdict::Recovery {
            cause: RecoveryCause::SwallowedMarker,
        } => AgentOutcome::Failure {
            error: if exit_code == 0 {
                "agent exited 0 without LOOM_COMPLETE / LOOM_NOOP / LOOM_BLOCKED / \
                 LOOM_CLARIFY marker (swallowed marker)"
                    .to_string()
            } else {
                format!("agent exited with code {exit_code}")
            },
        },
        PhaseVerdict::Recovery {
            cause: RecoveryCause::ObserverAbort { reason },
        } => AgentOutcome::Failure {
            error: format!("Session aborted by observer: {reason}."),
        },
        PhaseVerdict::Recovery {
            cause: RecoveryCause::AgentRetry { reason },
        } => AgentOutcome::Retry { reason },
        PhaseVerdict::Recovery {
            cause:
                RecoveryCause::WrongPhaseMarker {
                    marker_name,
                    phase_kind,
                },
        } => AgentOutcome::Failure {
            error: format!(
                "wrong-phase-marker: {marker_name} is not admitted in {phase_kind} phases",
            ),
        },
        PhaseVerdict::Recovery { cause } => AgentOutcome::Failure {
            error: format!("unexpected gate verdict: {}", cause.as_str()),
        },
    }
}

/// Helper used by `main.rs` to fetch the spec-filtered open list when the
/// caller needs the typed [`Bead`] slice (e.g. to print a status line).
/// Surfacing this here keeps the BdClient list-shape next to the controller.
pub async fn list_open_for_spec(bd: &BdClient, label: &SpecLabel) -> Result<Vec<Bead>, LoopError> {
    let beads = bd
        .list(ListOpts {
            status: Some("open".to_string()),
            label: Some(format!("spec:{}", label.as_str())),
            ..ListOpts::default()
        })
        .await?;
    Ok(beads)
}

#[cfg(test)]
mod tests {
    use super::super::runner::MISSING_AGENT_BINARY_CAUSE;
    use super::*;
    use loom_driver::agent::SessionOutcome;
    use loom_driver::bd::{BdError, Label, RunOutput};
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::sync::Mutex;
    use std::time::{Duration, SystemTime};

    /// Replays a scripted sequence of `bd` responses so the controller's
    /// `exec_review` can resolve the active molecule's `loom.base_commit`
    /// without spawning the real `bd` binary. Each entry feeds one
    /// `BdClient` call in order.
    struct ScriptedBd {
        responses: Mutex<VecDeque<RunOutput>>,
        /// Capture handle shared with the test, so an `Arc::clone` snapshot
        /// is held while the inner `ScriptedBd` is moved into `BdClient`.
        calls: Arc<Mutex<Vec<Vec<OsString>>>>,
    }

    impl ScriptedBd {
        fn new(responses: impl IntoIterator<Item = RunOutput>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn calls_handle(&self) -> Arc<Mutex<Vec<Vec<OsString>>>> {
            Arc::clone(&self.calls)
        }
    }

    impl CommandRunner for ScriptedBd {
        async fn run(&self, args: Vec<OsString>, _timeout: Duration) -> Result<RunOutput, BdError> {
            self.calls.lock().unwrap().push(args);
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(RunOutput {
                    status: 0,
                    stdout: b"null\n".to_vec(),
                    stderr: Vec::new(),
                }))
        }
    }

    fn ok_stdout(stdout: &[u8]) -> RunOutput {
        RunOutput {
            status: 0,
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    fn seed_spec(workspace: &std::path::Path, label: &str) {
        let path = workspace.join(format!("specs/{label}.md"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("# {label}\n")).unwrap();
    }

    /// Return a `ScriptedBd` that resolves one active molecule and has
    /// enough successful responses for a single deferred review finding to
    /// travel through the live molecule-routing seam.
    fn molecule_lookup_script(
        _workspace: &std::path::Path,
        spec_label: &str,
        mol_id: &str,
        base: &str,
    ) -> ScriptedBd {
        let body = format!(
            r#"[{{
                "id": "{mol_id}",
                "title": "{spec_label}: pending decomposition",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["spec:{spec_label}"],
                "metadata": {{ "loom.base_commit": "{base}" }}
            }}]"#,
        );
        let progress =
            r#"{"molecule_id":"lm-3hhwq","completed":1,"in_progress":0,"total":1,"percent":100.0}"#;
        ScriptedBd::new([
            ok_stdout(body.as_bytes()),
            ok_stdout(body.as_bytes()),
            ok_stdout(progress.as_bytes()),
            ok_stdout(b"[]"),
            ok_stdout(body.as_bytes()),
            ok_stdout(b"[]"),
            ok_stdout(b"[]"),
            ok_stdout(b"[]"),
            ok_stdout(b"lm-routed.1\n"),
            ok_stdout(b""),
            ok_stdout(b""),
        ])
    }

    /// FR12 — `loom loop`'s per-bead exit MUST route the agent's marker
    /// through the canonical [`crate::review::decide`] gate function rather
    /// than its own ad-hoc `match`. This test pins the marker → outcome
    /// mapping that `decide()` produces under neutral run-phase inputs:
    /// `BLOCKED`/`CLARIFY` short-circuit, `COMPLETE`/`NOOP` reach Done, and
    /// a missing marker routes to `swallowed-marker` recovery (mapped to
    /// `Failure`). A regression that resurrects an inline classifier here
    /// would only fail this test if it diverged from `decide()`'s output —
    /// but combined with the source-level `decide()` import in
    /// `classify_session`, the two together fence the FR12 contract.
    #[test]
    fn classify_session_routes_marker_through_phase_verdict_decide() {
        let session_ok = || {
            SessionResult::Complete(SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            })
        };
        // `BLOCKED` self-report → terminal `Blocked` (gate row 1).
        match classify_session(
            session_ok(),
            Some(ExitSignal::Blocked {
                reason: "missing schema".into(),
            }),
        ) {
            AgentOutcome::Blocked { reason } => assert_eq!(reason, "missing schema"),
            other => panic!("expected Blocked, got {other:?}"),
        }
        // `CLARIFY` self-report → terminal `Clarify` (gate row 2).
        match classify_session(
            session_ok(),
            Some(ExitSignal::Clarify {
                question: "additive only?".into(),
            }),
        ) {
            AgentOutcome::Clarify { question } => assert_eq!(question, "additive only?"),
            other => panic!("expected Clarify, got {other:?}"),
        }
        // `COMPLETE` + clean exit → `Success` (gate row "Done" with neutral inputs).
        assert_eq!(
            classify_session(session_ok(), Some(ExitSignal::Complete)),
            AgentOutcome::Success,
        );
        // `NOOP` + clean exit → `Success` (gate row "Done" with neutral inputs).
        assert_eq!(
            classify_session(session_ok(), Some(ExitSignal::Noop)),
            AgentOutcome::Success,
        );
        // None marker → `Recovery::SwallowedMarker` → `Failure` carrying
        // the spec's swallowed-marker phrasing.
        match classify_session(session_ok(), None) {
            AgentOutcome::Failure { error } => assert!(
                error.contains("swallowed marker"),
                "swallowed-marker text missing: {error}",
            ),
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    #[test]
    fn agent_stream_failure_classifier_distinguishes_preflight_interrupted_and_blocked() {
        let outcomes = [
            classify_session(
                SessionResult::PreflightFailed {
                    error: "unexpected EOF before first agent event".to_string(),
                },
                None,
            ),
            classify_session(
                SessionResult::MidSessionFailed {
                    error: "unexpected EOF after text_delta".to_string(),
                },
                None,
            ),
            classify_session(
                SessionResult::Complete(SessionOutcome {
                    exit_code: 0,
                    cost_usd: None,
                }),
                Some(ExitSignal::Blocked {
                    reason: "semantic dead end with no safe options".to_string(),
                }),
            ),
        ];

        assert!(
            matches!(
                &outcomes[0],
                AgentOutcome::InfraPreflight { error }
                    if error == "unexpected EOF before first agent event"
            ),
            "preflight EOF must stay retryable infra: {:?}",
            outcomes[0]
        );
        assert!(
            matches!(
                &outcomes[1],
                AgentOutcome::InfraMidSession { error }
                    if error == "unexpected EOF after text_delta"
            ),
            "interrupted EOF must stay retryable infra: {:?}",
            outcomes[1]
        );
        assert!(
            matches!(
                &outcomes[2],
                AgentOutcome::Blocked { reason }
                    if reason == "semantic dead end with no safe options"
            ),
            "LOOM_BLOCKED must remain semantic: {:?}",
            outcomes[2]
        );
    }

    #[test]
    fn classify_session_preserves_static_infra_cause() {
        let outcome = classify_session(
            SessionResult::StaticInfra {
                cause: MISSING_AGENT_BINARY_CAUSE.to_string(),
                error: "agent process exited with code 127".into(),
            },
            None,
        );

        match outcome {
            AgentOutcome::StaticInfra { cause, error } => {
                assert_eq!(cause, MISSING_AGENT_BINARY_CAUSE);
                assert!(error.contains("code 127"));
            }
            other => panic!("expected StaticInfra, got {other:?}"),
        }
    }

    /// Spec gate (§"Marker definitions"): `LOOM_CONCERN` is
    /// review-phase-only. The run phase emitting it is a
    /// `wrong-phase-marker` error — neither `Success` nor a generic
    /// swallowed-marker; the detail names the concern token so triage can
    /// see which path the agent tried to flag.
    #[test]
    fn concern_marker_in_run_phase_is_wrong_phase_marker_failure() {
        let session = SessionResult::Complete(SessionOutcome {
            exit_code: 0,
            cost_usd: None,
        });
        match classify_session(
            session,
            Some(ExitSignal::Concern {
                summary: "verifier-bypass on the agent backend mock".into(),
            }),
        ) {
            AgentOutcome::Failure { error } => {
                assert!(
                    error.contains("wrong-phase-marker"),
                    "wrong-phase-marker prefix missing: {error}",
                );
                assert!(
                    error.contains("LOOM_CONCERN"),
                    "marker name must appear in error: {error}",
                );
                assert!(
                    error.contains("verifier-bypass"),
                    "concern token must appear in error: {error}",
                );
            }
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    /// Spec gate (§"Disambiguating no marker"): a session aborted by an
    /// observer's `SessionCommand::Abort` must classify as `observer-abort`
    /// rather than `swallowed-marker`, even though no exit marker was
    /// emitted. The detail string must carry the observer's verbatim
    /// reason so human triage sees what tripped the kill. The two branches
    /// — observer-aborted vs. plain no-marker — share the same `marker=None`
    /// input, so distinctness is the load-bearing property: the choice of
    /// recovery cause must come from the `SessionResult` discriminator, not
    /// the marker shape.
    #[test]
    fn observer_abort_routes_to_observer_abort_distinct_from_swallowed_marker() {
        let observer_session = SessionResult::ObserverAbort {
            reason: "doom-loop: 3 identical tool calls".into(),
        };
        let observer_error = match classify_session(observer_session, None) {
            AgentOutcome::Failure { error } => error,
            other => panic!("expected Failure for ObserverAbort, got {other:?}"),
        };
        assert!(
            observer_error.contains("Session aborted by observer"),
            "observer-abort error must carry the spec format prefix: {observer_error}",
        );
        assert!(
            observer_error.contains("doom-loop: 3 identical tool calls"),
            "observer-abort error must preserve verbatim observer reason: {observer_error}",
        );
        assert!(
            !observer_error.contains("swallowed marker"),
            "observer-abort must NOT degrade to swallowed-marker: {observer_error}",
        );

        let plain_session = SessionResult::Complete(SessionOutcome {
            exit_code: 0,
            cost_usd: None,
        });
        let plain_error = match classify_session(plain_session, None) {
            AgentOutcome::Failure { error } => error,
            other => panic!("expected Failure for marker-less Complete, got {other:?}"),
        };
        assert!(
            plain_error.contains("swallowed marker"),
            "marker-less Complete must route to swallowed-marker: {plain_error}",
        );
        assert!(
            !plain_error.contains("Session aborted by observer"),
            "swallowed-marker must NOT borrow observer-abort phrasing: {plain_error}",
        );

        assert_ne!(
            observer_error, plain_error,
            "observer-abort and swallowed-marker must yield distinct error bodies under marker=None",
        );
    }

    fn write_manifest(dir: &std::path::Path) -> Arc<ProfileImageManifest> {
        let body = r#"{
          "base": { "pi": { "ref": "localhost/wrix-base-pi:abc", "source": "/nix/store/aaa-image-base-pi", "source_kind": "nix-descriptor" }, "claude": { "ref": "localhost/wrix-base-claude:abc", "source": "/nix/store/aaa-image-base-claude", "source_kind": "nix-descriptor" }, "direct": { "ref": "localhost/wrix-base-direct:abc", "source": "/nix/store/aaa-image-base-direct", "source_kind": "nix-descriptor" } }
        }"#;
        let path = dir.join("profile-images.json");
        std::fs::write(&path, body).expect("write manifest");
        Arc::new(ProfileImageManifest::from_path(&path).expect("parse manifest"))
    }

    /// Initialize a git repository at `workspace` (creating the directory if
    /// missing) and open a [`GitClient`] rooted there. Used by every
    /// controller-construction site so `run_bead`'s per-bead worktree
    /// dispatch has a real repo to bind against.
    fn git_workspace(workspace: &std::path::Path) -> loom_driver::git::GitClient {
        loom_driver::git::init_test_repo_with_integration(workspace).expect("init test repo")
    }

    fn bead(id: &str) -> Bead {
        Bead {
            id: BeadId::new(id).expect("valid bead id"),
            title: format!("title-{id}"),
            description: "desc".into(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: vec![Label::new("profile:base")],
            parent: None,
            metadata: Default::default(),
            notes: None,
        }
    }

    #[tokio::test]
    async fn clarify_or_infra_present_stops_without_pushing() {
        for label in ["loom:clarify", "loom:infra"] {
            let dir = tempfile::tempdir().expect("tempdir");
            let workspace = dir.path().to_path_buf();
            let git = git_workspace(&workspace);
            let epic = br#"[{
                "id": "lm-held",
                "title": "held molecule",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["spec:alpha"]
            }]"#;
            let child = format!(
                r#"[{{
                    "id": "lm-held.1",
                    "title": "held molecule work",
                    "status": "blocked",
                    "priority": 2,
                    "issue_type": "task",
                    "parent": "lm-held",
                    "labels": ["spec:alpha", "{label}"]
                }}]"#,
            );
            let progress = br#"{"molecule_id":"lm-3hhwq","completed":0,"in_progress":0,"total":1,"percent":0.0}"#;
            let bd = BdClient::with_runner(ScriptedBd::new([
                ok_stdout(epic),
                ok_stdout(epic),
                ok_stdout(progress),
                ok_stdout(child.as_bytes()),
            ]));
            let before = loom_driver::git::sync_rev_parse(&git.loom_workspace(), "origin/main")
                .expect("origin tip before");
            let handoff = execute_molecule_push_gate(
                &bd,
                &SpecLabel::new("alpha"),
                None,
                Path::new("must-not-run-review"),
                Path::new("must-not-run-beads-push"),
                &workspace,
                &git,
            )
            .await
            .expect("unresolved state is a typed refusal");
            assert_eq!(handoff.evidence.molecule_state, MoleculeState::Unresolved);
            assert!(handoff.evidence.gate_runs.is_empty());
            assert!(!git.loom_workspace().join(".loom/marker.json").exists());
            let after = loom_driver::git::sync_rev_parse(&git.loom_workspace(), "origin/main")
                .expect("origin tip after");
            assert_eq!(before, after, "{label} must refuse the push");
        }
    }

    #[tokio::test]
    async fn molecule_state_refuses_a_parked_molecule_epic() {
        let epic = br#"[{
            "id": "lm-parked",
            "title": "parked molecule",
            "status": "blocked",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["spec:agent", "loom:blocked"]
        }]"#;
        let progress = br#"{"molecule_id":"lm-parked","completed":1,"in_progress":0,"total":1,"percent":100.0}"#;
        let bd = BdClient::with_runner(ScriptedBd::new([
            ok_stdout(epic),
            ok_stdout(progress),
            ok_stdout(b"[]"),
        ]));

        let state = molecule_state(&bd, &MoleculeId::new("lm-parked"))
            .await
            .expect("read molecule state");

        assert_eq!(state, MoleculeState::Unresolved);
    }

    #[test]
    fn parked_state_includes_semantic_deferred_and_infra_labels() {
        let mut deferred = bead("lm-deferred");
        deferred.labels.push(Label::new("loom:deferred"));
        assert!(bead_has_parked_state(&deferred));

        let mut infra = bead("lm-infra");
        infra.labels.push(Label::new("loom:infra"));
        assert!(bead_has_parked_state(&infra));
    }

    #[tokio::test]
    async fn next_ready_bead_loads_blocked_infra_when_ready_queue_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let infra = br#"[{
            "id":"lm-infra",
            "title":"infra",
            "status":"blocked",
            "priority":2,
            "issue_type":"task",
            "labels":["spec:gate","profile:base","loom:infra"]
        }]"#;
        let scripted = ScriptedBd::new([ok_stdout(b"[]\n"), ok_stdout(infra)]);
        let calls = scripted.calls_handle();
        let bd = BdClient::with_runner(scripted);
        let mut controller = ProductionAgentLoopController::new(
            bd,
            SpecLabel::new("gate"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        );

        let picked = controller
            .next_ready_bead(&[])
            .await
            .expect("ready lookup ok")
            .expect("infra bead selected");

        assert_eq!(picked.id, BeadId::new("lm-infra").expect("valid"));
        let captured = calls.lock().unwrap();
        assert_eq!(captured.len(), 2);
        let infra_argv: Vec<String> = captured[1]
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(infra_argv[0], "list");
        assert!(infra_argv.contains(&"--status=blocked".to_string()));
        assert!(infra_argv.contains(&"--label=spec:gate".to_string()));
        assert!(infra_argv.contains(&"--label-any=loom:infra".to_string()));
    }

    #[tokio::test]
    async fn run_bead_clears_stale_infra_state_before_dispatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let scripted = ScriptedBd::new([ok_stdout(b"")]);
        let calls = scripted.calls_handle();
        let bd = BdClient::with_runner(scripted);
        let mut controller = ProductionAgentLoopController::new(
            bd,
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |cfg: SpawnConfig, _bead_id: BeadId| async move {
                std::fs::write(cfg.workspace.join("spawn.txt"), "spawned\n")
                    .expect("write spawn file");
                loom_driver::git::commit_all_in(&cfg.workspace, "spawn work")
                    .expect("commit spawn work");
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        );
        let mut infra_bead = bead("lm-infra");
        infra_bead.status = "blocked".to_string();
        infra_bead.labels.push(Label::new("loom:infra"));

        let outcome = controller
            .run_bead(&infra_bead, None)
            .await
            .expect("run_bead ok");

        assert_eq!(outcome, AgentOutcome::Success);
        let captured = calls.lock().unwrap();
        let update: Vec<String> = captured[0]
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(update[0], "update");
        assert_eq!(update[1], "lm-infra");
        assert!(
            update
                .windows(2)
                .any(|w| w[0] == "--status" && w[1] == "open"),
            "redispatch must reopen stale infra bead: {update:?}",
        );
        assert!(
            update
                .windows(2)
                .any(|w| w[0] == "--remove-label" && w[1] == "loom:infra"),
            "redispatch must clear stale infra label: {update:?}",
        );
    }

    #[tokio::test]
    async fn retry_attempt_counter_increments_per_bead_retry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let captured_prompts = Arc::new(Mutex::new(Vec::new()));
        let captured_prompts_inner = Arc::clone(&captured_prompts);
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("templates"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            move |cfg: SpawnConfig, _bead_id: BeadId| {
                let captured_prompts = Arc::clone(&captured_prompts_inner);
                async move {
                    captured_prompts.lock().unwrap().push(cfg.initial_prompt);
                    (
                        SessionResult::Complete(SessionOutcome {
                            exit_code: 1,
                            cost_usd: None,
                        }),
                        None,
                    )
                }
            },
        );
        let bead = bead("lm-retry");

        for previous_failure in [
            None,
            Some("failed once".to_string()),
            Some("failed twice".to_string()),
        ] {
            let outcome = controller
                .run_bead(&bead, previous_failure)
                .await
                .expect("run_bead ok");
            assert!(matches!(outcome, AgentOutcome::Failure { .. }));
        }

        let prompts = captured_prompts.lock().unwrap();
        assert_eq!(prompts.len(), 3);
        assert!(!prompts[0].contains("Retry attempt"));
        assert!(
            prompts[1].contains("Retry attempt 1 — previous attempt failed with:"),
            "first retry prompt must render attempt 1: {}",
            prompts[1],
        );
        assert!(
            prompts[2].contains("Retry attempt 2 — previous attempt failed with:"),
            "second retry prompt must render attempt 2: {}",
            prompts[2],
        );
    }

    #[tokio::test]
    async fn next_ready_skips_child_of_parked_parent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let ready = br#"[{
            "id":"lm-child",
            "title":"child",
            "status":"open",
            "priority":2,
            "issue_type":"task",
            "labels":["spec:harness","profile:base"],
            "parent":"lm-parent"
        }]"#;
        let parent = br#"[{
            "id":"lm-parent",
            "title":"parent",
            "status":"open",
            "priority":2,
            "issue_type":"epic",
            "labels":["spec:harness","loom:blocked"]
        }]"#;
        let bd = BdClient::with_runner(ScriptedBd::new([ok_stdout(ready), ok_stdout(parent)]));
        let mut controller = ProductionAgentLoopController::new(
            bd,
            SpecLabel::new("harness"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        );

        let next = controller
            .next_ready_bead(&[])
            .await
            .expect("ready lookup ok");
        assert!(next.is_none(), "parked parent suppresses child dispatch");
    }

    #[tokio::test]
    async fn run_bead_invokes_dispatch_closure_with_resolved_spawn_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let captured: Arc<Mutex<Option<SpawnConfig>>> = Arc::new(Mutex::new(None));
        let captured_for_closure = Arc::clone(&captured);
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            move |cfg: SpawnConfig, _bead_id: BeadId| {
                let captured = Arc::clone(&captured_for_closure);
                async move {
                    std::fs::write(cfg.workspace.join("spawn.txt"), "spawned\n")
                        .expect("write spawn file");
                    loom_driver::git::commit_all_in(&cfg.workspace, "spawn work")
                        .expect("commit spawn work");
                    *captured.lock().unwrap() = Some(cfg);
                    (
                        SessionResult::Complete(SessionOutcome {
                            exit_code: 0,
                            cost_usd: None,
                        }),
                        Some(ExitSignal::Complete),
                    )
                }
            },
        );
        let outcome = controller
            .run_bead(&bead("lm-1"), None)
            .await
            .expect("run_bead ok");
        assert_eq!(outcome, AgentOutcome::Success);
        let cfg = captured.lock().unwrap().take().expect("closure called");
        assert_eq!(cfg.image_ref, "localhost/wrix-base-pi:abc");
        assert!(cfg.initial_prompt.contains("lm-1"));
    }

    #[tokio::test]
    async fn run_bead_blocks_success_that_does_not_advance_branch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            workspace.clone(),
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        );

        let outcome = controller
            .run_bead(&bead("lm-zero"), None)
            .await
            .expect("run_bead ok");
        match outcome {
            AgentOutcome::ZeroProgress { detail } => {
                assert!(detail.contains("lm-zero"), "detail names bead: {detail}");
                assert!(
                    workspace.join(".loom/beads/lm-zero").exists(),
                    "workspace preserved for inspection",
                );
            }
            other => panic!("expected ZeroProgress, got {other:?}"),
        }
    }

    /// `loom loop` must dispatch with the rendered
    /// [`LoopContext`] template — bead title/description, scratchpad path,
    /// and spec_path all reach the agent prompt — and the same body must
    /// land in `<scratch_dir>/prompt.txt` so post-compaction `repin.sh`
    /// can re-emit the actual phase prompt.
    #[tokio::test]
    async fn run_bead_dispatches_rendered_run_template_and_writes_prompt_txt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let captured: Arc<Mutex<Option<SpawnConfig>>> = Arc::new(Mutex::new(None));
        let captured_for_closure = Arc::clone(&captured);
        let prompt_seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let prompt_seen_inner = Arc::clone(&prompt_seen);
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("harness"),
            PathBuf::from("/loom/bin"),
            workspace.clone(),
            git,
            manifest,
            None,
            ProfileName::new("base"),
            move |cfg: SpawnConfig, _bead_id: BeadId| {
                let captured = Arc::clone(&captured_for_closure);
                let prompt_seen = Arc::clone(&prompt_seen_inner);
                async move {
                    // Read prompt.txt mid-session, while the ScratchSession
                    // guard is still alive — Drop removes the dir on return.
                    let txt = std::fs::read_to_string(cfg.scratch_dir.join("prompt.txt"))
                        .expect("prompt.txt readable");
                    *prompt_seen.lock().unwrap() = Some(txt);
                    *captured.lock().unwrap() = Some(cfg);
                    (
                        SessionResult::Complete(SessionOutcome {
                            exit_code: 0,
                            cost_usd: None,
                        }),
                        Some(ExitSignal::Complete),
                    )
                }
            },
        );
        let bead = Bead {
            id: BeadId::new("lm-99").expect("bead id"),
            title: "Implement the harness".into(),
            description: "wire the per-bead loop".into(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: vec![Label::new("profile:base")],
            parent: None,
            metadata: Default::default(),
            notes: None,
        };
        controller.run_bead(&bead, None).await.expect("run_bead ok");
        let cfg = captured.lock().unwrap().take().expect("closure called");
        assert!(
            cfg.initial_prompt.contains("# Implementation Step"),
            "prompt missing template heading: {}",
            cfg.initial_prompt,
        );
        assert!(
            cfg.initial_prompt.contains("Implement the harness"),
            "prompt missing bead title: {}",
            cfg.initial_prompt,
        );
        assert!(
            cfg.initial_prompt.contains("wire the per-bead loop"),
            "prompt missing bead description: {}",
            cfg.initial_prompt,
        );
        assert!(
            cfg.initial_prompt.contains("specs/harness.md"),
            "prompt missing spec path: {}",
            cfg.initial_prompt,
        );
        // prompt.txt must hold the same rendered body so repin.sh
        // surfaces the phase prompt under compaction recovery.
        let written = prompt_seen.lock().unwrap().take().expect("prompt.txt seen");
        assert_eq!(written, cfg.initial_prompt);
    }

    #[tokio::test]
    async fn loop_complete_does_not_require_recovery_stash_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("harness");
        let bead_id = BeadId::new("lm-stashok").expect("valid bead id");
        let created = git
            .create_worktree(&label, &bead_id)
            .await
            .expect("create worktree");
        std::fs::write(created.path.join("dirty.txt"), "preserved local work\n")
            .expect("write dirty file");
        let captured_prompt: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured_prompt_inner = Arc::clone(&captured_prompt);
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            label,
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            move |cfg: SpawnConfig, _bead_id: BeadId| {
                let captured_prompt = Arc::clone(&captured_prompt_inner);
                async move {
                    *captured_prompt.lock().unwrap() = Some(cfg.initial_prompt.clone());
                    std::fs::write(cfg.workspace.join("spawn.txt"), "spawned\n")
                        .expect("write spawn file");
                    loom_driver::git::commit_all_in(&cfg.workspace, "spawn work")
                        .expect("commit spawn work");
                    (
                        SessionResult::Complete(SessionOutcome {
                            exit_code: 0,
                            cost_usd: None,
                        }),
                        Some(ExitSignal::Complete),
                    )
                }
            },
        );

        let outcome = controller
            .run_bead(&bead("lm-stashok"), None)
            .await
            .expect("run_bead ok");

        assert_eq!(outcome, AgentOutcome::Success);
        let prompt = captured_prompt
            .lock()
            .unwrap()
            .take()
            .expect("prompt captured");
        assert!(
            prompt.contains("## Workspace Recovery"),
            "workspace recovery context missing: {prompt}",
        );
        assert!(
            prompt.contains("git stash show --stat"),
            "stash inspection command missing: {prompt}",
        );
        assert!(
            !prompt.contains("Retry attempt 1"),
            "recovery stash context must not consume retry attempt: {prompt}",
        );
        let stash_reflog = std::fs::read_to_string(created.path.join(".git/logs/refs/stash"))
            .expect("stash reflog readable");
        assert!(
            stash_reflog.contains("loom workspace-recovery lm-stashok"),
            "successful completion must not require dropping the recovery stash: {stash_reflog}",
        );
    }

    #[tokio::test]
    async fn workspace_recovery_rebase_conflict_dispatches_agent_with_context() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("harness");
        let bead_id = BeadId::new("lm-conflict").expect("valid bead id");
        let created = git
            .create_worktree(&label, &bead_id)
            .await
            .expect("create worktree");
        std::fs::write(created.path.join("README.md"), "bead edit\n").expect("write bead edit");
        loom_driver::git::commit_all_in(&created.path, "bead edit").expect("commit bead edit");
        let loom = git.loom_workspace();
        std::fs::write(loom.join("README.md"), "integration edit\n")
            .expect("write integration edit");
        loom_driver::git::commit_all_in(&loom, "integration edit")
            .expect("commit integration edit");
        std::fs::write(created.path.join("scratch.txt"), "dirty scratch\n")
            .expect("write dirty scratch");
        let captured_prompt: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let conflict_body: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured_prompt_inner = Arc::clone(&captured_prompt);
        let conflict_body_inner = Arc::clone(&conflict_body);
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            label,
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            move |cfg: SpawnConfig, _bead_id: BeadId| {
                let captured_prompt = Arc::clone(&captured_prompt_inner);
                let conflict_body = Arc::clone(&conflict_body_inner);
                async move {
                    *captured_prompt.lock().unwrap() = Some(cfg.initial_prompt.clone());
                    *conflict_body.lock().unwrap() = Some(
                        std::fs::read_to_string(cfg.workspace.join("README.md"))
                            .expect("conflict file readable"),
                    );
                    (
                        SessionResult::Complete(SessionOutcome {
                            exit_code: 1,
                            cost_usd: None,
                        }),
                        None,
                    )
                }
            },
        );

        let outcome = controller
            .run_bead(&bead("lm-conflict"), None)
            .await
            .expect("run_bead ok");

        assert!(matches!(outcome, AgentOutcome::Failure { .. }));
        let prompt = captured_prompt
            .lock()
            .unwrap()
            .take()
            .expect("prompt captured");
        assert!(
            prompt.contains("Alignment is in conflict"),
            "conflict guidance missing: {prompt}",
        );
        assert!(
            prompt.contains("- `README.md`"),
            "conflict file missing from prompt: {prompt}",
        );
        assert!(
            prompt.contains("LOOM_CLARIFY"),
            "prompt must route human-needed conflicts through clarify: {prompt}",
        );
        assert!(
            !prompt.contains("Retry attempt 1"),
            "workspace recovery conflict dispatch must not consume retry attempt: {prompt}",
        );
        let readme = conflict_body
            .lock()
            .unwrap()
            .take()
            .expect("conflict file captured");
        assert!(
            readme.contains("<<<<<<<"),
            "worker saw no conflict markers: {readme}"
        );
    }

    #[tokio::test]
    async fn workspace_recovery_event_records_stash_and_alignment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("harness");
        let bead_id = BeadId::new("lm-event").expect("valid bead id");
        let created = git
            .create_worktree(&label, &bead_id)
            .await
            .expect("create worktree");
        std::fs::write(created.path.join("README.md"), "bead edit\n").expect("write bead edit");
        loom_driver::git::commit_all_in(&created.path, "bead edit").expect("commit bead edit");
        let loom = git.loom_workspace();
        std::fs::write(loom.join("README.md"), "integration edit\n")
            .expect("write integration edit");
        loom_driver::git::commit_all_in(&loom, "integration edit")
            .expect("commit integration edit");
        std::fs::write(created.path.join("scratch.txt"), "dirty scratch\n")
            .expect("write dirty scratch");
        let logs_root = dir.path().join("logs");
        let logs_root_for_closure = logs_root.clone();
        let label_for_closure = label.clone();
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            label.clone(),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            move |_cfg: SpawnConfig, bead_id: BeadId| {
                let logs_root = logs_root_for_closure.clone();
                let label = label_for_closure.clone();
                async move {
                    let mut sink = loom_driver::logging::LogSink::open_in_at(
                        &logs_root,
                        &label,
                        &bead_id,
                        None,
                        SystemTime::UNIX_EPOCH,
                    )
                    .expect("open bead log");
                    sink.finish(loom_driver::logging::BeadOutcome::Failed)
                        .expect("finish bead log");
                    (
                        SessionResult::Complete(SessionOutcome {
                            exit_code: 1,
                            cost_usd: None,
                        }),
                        None,
                    )
                }
            },
        )
        .with_phase_log_root(logs_root.clone());

        let outcome = controller
            .run_bead(&bead("lm-event"), None)
            .await
            .expect("run_bead ok");

        assert!(matches!(outcome, AgentOutcome::Failure { .. }));
        let log_path = loom_driver::logging::bead_log_path(
            &logs_root,
            &label,
            &bead_id,
            SystemTime::UNIX_EPOCH,
        );
        let body = std::fs::read_to_string(&log_path).expect("driver event log readable");
        let event = body
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("event json"))
            .find(|event| event["driver_kind"] == "workspace_recovery")
            .unwrap_or_else(|| panic!("workspace_recovery event missing from {body}"));
        assert_eq!(event["source"], "driver");
        assert_eq!(event["payload"]["bead_id"], "lm-event");
        assert!(
            event["payload"]["pre_stash_status"]
                .as_str()
                .unwrap_or_default()
                .contains("scratch.txt"),
            "pre-stash status missing dirty file: {event}",
        );
        assert_eq!(event["payload"]["stash_selector"], "stash@{0}");
        assert!(
            event["payload"]["stash_message"]
                .as_str()
                .unwrap_or_default()
                .contains("loom workspace-recovery lm-event"),
            "stash message missing bead id: {event}",
        );
        assert!(
            event["payload"]["stash_commit"]
                .as_str()
                .is_some_and(|s| s.len() == 40 || s.len() == 64),
            "stash commit must be a stable git oid: {event}",
        );
        assert!(
            event["payload"]["integration_tip"]
                .as_str()
                .is_some_and(|s| s.len() == 40 || s.len() == 64),
            "integration tip must be recorded: {event}",
        );
        assert_eq!(event["payload"]["alignment_outcome"], "conflict");
        assert_eq!(
            event["payload"]["conflict_files"],
            serde_json::json!(["README.md"]),
        );
    }

    #[tokio::test]
    async fn workspace_recovery_stash_failure_routes_static_infra_without_cleanup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("harness");
        let bead_id = BeadId::new("lm-stashfail").expect("valid bead id");
        let created = git
            .create_worktree(&label, &bead_id)
            .await
            .expect("create worktree");
        std::fs::write(created.path.join("README.md"), "bead edit\n").expect("write bead edit");
        loom_driver::git::commit_all_in(&created.path, "bead edit").expect("commit bead edit");
        let loom = git.loom_workspace();
        std::fs::write(loom.join("README.md"), "integration edit\n")
            .expect("write integration edit");
        loom_driver::git::commit_all_in(&loom, "integration edit")
            .expect("commit integration edit");
        let preexisting_conflict = GitClient::open(&created.path)
            .expect("open bead git")
            .prepare_bead_clone(&created.path, &bead_id)
            .await
            .expect("seed pre-existing conflict");
        assert!(
            matches!(
                preexisting_conflict.alignment,
                BeadCloneAlignment::Conflict { .. }
            ),
            "setup prepare should leave an unmerged index",
        );
        let before = std::fs::read_to_string(created.path.join("README.md"))
            .expect("conflict file readable");
        assert!(
            before.contains("<<<<<<<"),
            "conflict setup failed: {before}"
        );
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            label,
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                panic!("spawn closure must not run after recovery stash failure");
            },
        );

        let outcome = controller
            .run_bead(&bead("lm-stashfail"), None)
            .await
            .expect("run_bead ok");

        match outcome {
            AgentOutcome::StaticInfra { cause, error } => {
                assert_eq!(cause, WORKSPACE_RECOVERY_FAILED_CAUSE);
                assert!(
                    error.contains("workspace recovery pre-dispatch prepare failed"),
                    "diagnostic must name prepare failure: {error}",
                );
            }
            other => panic!("expected StaticInfra, got {other:?}"),
        }
        let after = std::fs::read_to_string(created.path.join("README.md"))
            .expect("conflict file readable after failure");
        assert!(
            after.contains("<<<<<<<"),
            "stash failure must abort before destructive cleanup: {after}",
        );
    }

    #[tokio::test]
    async fn run_bead_translates_nonzero_exit_code_into_failure_with_error_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                // Nonzero exit + no marker = swallowed marker; we want to
                // verify the exit_code path. Pass None marker so the
                // classifier hits the `(None, code) => Failure` branch.
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 42,
                        cost_usd: None,
                    }),
                    None,
                )
            },
        );
        let outcome = controller
            .run_bead(&bead("lm-2"), None)
            .await
            .expect("run_bead ok");
        match outcome {
            AgentOutcome::Failure { error } => {
                assert!(
                    error.contains("42"),
                    "error body should mention exit code 42: {error}"
                );
            }
            other => panic!("non-zero exit must produce Failure, got {other:?}"),
        }
    }

    /// Spec gate: a [`SessionResult::PreflightFailed`] from the dispatch
    /// closure must surface as [`AgentOutcome::InfraPreflight`] so the
    /// run-loop infra budget can retry it before any `loom:infra` route.
    #[tokio::test]
    async fn run_bead_translates_preflight_failure_into_infra_preflight() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::PreflightFailed {
                        error: "podman load failed: image archive missing".into(),
                    },
                    None,
                )
            },
        );
        let outcome = controller
            .run_bead(&bead("lm-3"), None)
            .await
            .expect("run_bead ok");
        match outcome {
            AgentOutcome::InfraPreflight { error } => {
                assert!(
                    error.contains("podman load"),
                    "preflight error must carry detail: {error}",
                );
            }
            other => panic!("expected InfraPreflight, got {other:?}"),
        }
    }

    /// Spec gate: a [`SessionResult::MidSessionFailed`] from the dispatch
    /// closure must surface as [`AgentOutcome::InfraMidSession`] so the
    /// per-bead infra budget can retry interrupted streams.
    #[tokio::test]
    async fn run_bead_translates_midsession_failure_into_infra_midsession() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::MidSessionFailed {
                        error: "agent stdout closed: exit 137 (OOM)".into(),
                    },
                    None,
                )
            },
        );
        let outcome = controller
            .run_bead(&bead("lm-4"), None)
            .await
            .expect("run_bead ok");
        match outcome {
            AgentOutcome::InfraMidSession { error } => {
                assert!(
                    error.contains("OOM"),
                    "mid-session error must carry detail: {error}",
                );
            }
            other => panic!("expected InfraMidSession, got {other:?}"),
        }
    }

    /// Spec gate (Implementation Note 6): a bead whose `profile:X` label
    /// is missing from the manifest must surface as
    /// [`AgentOutcome::UnknownProfile`] (NOT [`AgentOutcome::Failure`])
    /// so [`process_one_bead`](crate::r#loop::run_loop) routes it straight to
    /// `loom:infra` cause `unknown-profile` without consuming a retry slot.
    /// The error string must name the requested profile and the manifest's
    /// declared set so the operator can relabel without re-reading the
    /// manifest.
    #[tokio::test]
    async fn run_bead_translates_unknown_profile_into_unknown_profile_outcome() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        // Manifest declares only `base` — the bead asks for `nonexistent`.
        let manifest = write_manifest(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                panic!("spawn closure must not be invoked when profile resolution fails");
            },
        );
        let bad_bead = Bead {
            id: BeadId::new("lm-5").expect("valid bead id"),
            title: "needs a profile we do not have".into(),
            description: "desc".into(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: vec![Label::new("profile:nonexistent")],
            parent: None,
            metadata: Default::default(),
            notes: None,
        };
        let outcome = controller
            .run_bead(&bad_bead, None)
            .await
            .expect("run_bead must NOT bubble UnknownProfile up as LoopError");
        match outcome {
            AgentOutcome::UnknownProfile { error } => {
                assert!(
                    error.contains("profile:nonexistent"),
                    "error must name the requested profile (as it appears on the bead label): {error}",
                );
                assert!(
                    error.contains("profile:base"),
                    "error must name at least one declared profile so the operator can relabel: {error}",
                );
            }
            other => panic!("expected UnknownProfile, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_bead_translates_invalid_spawn_config_into_static_infra() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let path = dir.path().join("profile-images.json");
        std::fs::write(
            &path,
            r#"{
              "base": { "pi": { "ref": "", "source": "/nix/store/aaa-image-base-pi", "source_kind": "nix-descriptor" } }
            }"#,
        )
        .expect("write manifest");
        let manifest = Arc::new(ProfileImageManifest::from_path(&path).expect("parse manifest"));
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                panic!("spawn closure must not run for invalid spawn config");
            },
        );

        let outcome = controller
            .run_bead(&bead("lm-invalid"), None)
            .await
            .expect("run_bead ok");

        match outcome {
            AgentOutcome::StaticInfra { cause, error } => {
                assert_eq!(cause, INVALID_SPAWN_CONFIG_CAUSE);
                assert!(error.contains("image ref"), "{error}");
            }
            other => panic!("expected StaticInfra, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_holds_lock_through_marker_and_push() {
        use loom_driver::clock::SystemClock;
        use loom_driver::lock::LockManager;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let mgr = LockManager::new(dir.path()).expect("lock manager");
        let label = SpecLabel::new("alpha");
        let root = BeadId::new("lm-lock").expect("valid bead id");
        let clock = SystemClock::new();
        let guard = mgr
            .acquire_work_root_async(&root, &clock)
            .await
            .expect("first acquire");

        // Stand-in for the `loom` binary: ignores all args and exits 0.
        // /bin/true does not exist on NixOS, so we ship a script.
        let stub = dir.path().join("loom-stub.sh");
        std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let bd = BdClient::with_runner(molecule_lookup_script(
            dir.path(),
            "alpha",
            "lm-mol.1",
            "deadbeef",
        ));
        let git = git_workspace(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            bd,
            label.clone(),
            stub,
            dir.path().to_path_buf(),
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        )
        .with_handoff_lock(guard);

        controller.exec_review().await.expect("exec_review ok");

        let held = mgr
            .acquire_work_root_with_timeout_async(&root, &clock, Duration::from_millis(100))
            .await;
        assert!(
            held.is_err(),
            "push critical section must retain the work-root lock"
        );
        drop(controller);
        let _reacquired = mgr
            .acquire_work_root_with_timeout_async(&root, &clock, Duration::from_millis(100))
            .await
            .expect("lock must be reacquirable after the controller drops");
    }

    /// Molecule-completion handoff computes the origin-synchronized push
    /// range and invokes review without scalar handoff flags.
    #[tokio::test(flavor = "multi_thread")]
    async fn molecule_push_gate_verifies_and_reviews_actual_push_range() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("alpha");

        // Recording stub: appends every invocation's argv (one per line,
        // tab-separated) to argv.log so the test can replay the call order.
        let argv_log = dir.path().join("argv.log");
        let stub = dir.path().join("loom-stub.sh");
        let stub_body = format!(
            "#!/bin/sh\nprintf '%s\\t%s\\t%s\\n' \"$PWD\" \"$LOOM_REVIEW_SPEC_LABEL\" \"$*\" >> {log}\nexit 0\n",
            log = argv_log.to_string_lossy(),
        );
        std::fs::write(&stub, stub_body).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let bd = BdClient::with_runner(molecule_lookup_script(
            dir.path(),
            "alpha",
            "lm-mol.1",
            "deadbeef",
        ));
        let git = git_workspace(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            bd,
            label.clone(),
            stub,
            dir.path().to_path_buf(),
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        );

        let gate_workspace = controller.git.loom_workspace();
        let from_oid = loom_driver::git::sync_rev_parse(&gate_workspace, "origin/main")
            .expect("resolve origin tip");
        let to_oid =
            loom_driver::git::sync_rev_parse(&gate_workspace, "HEAD").expect("resolve HEAD");
        controller.exec_review().await.expect("exec_review ok");

        let recorded = std::fs::read_to_string(&argv_log).expect("argv log readable");
        let lines: Vec<String> = recorded.lines().map(ToOwned::to_owned).collect();
        assert_eq!(
            lines,
            vec![format!(
                "{}\talpha\tgate review --diff {from_oid}..{to_oid}",
                gate_workspace.display()
            )],
            "review must run from the integration checkout over the actual origin push range; the spec label travels as env context, not as a gate filter: {recorded:?}",
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_child_failure_returns_incomplete_typed_evidence() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("alpha");
        let stub = dir.path().join("loom-stub.sh");
        std::fs::write(
            &stub,
            "#!/bin/sh\nprintf 'child stdout\\n'\nprintf 'child stderr\\n' >&2\nexit 17\n",
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let bd = BdClient::with_runner(molecule_lookup_script(
            dir.path(),
            "alpha",
            "lm-mol.1",
            "deadbeef",
        ));
        let git = git_workspace(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            bd,
            label,
            stub,
            dir.path().to_path_buf(),
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        );

        let evidence = controller
            .exec_review()
            .await
            .expect("review failure is a typed gate refusal");
        assert_eq!(evidence.review_exit, Some(17));
        assert!(evidence.verified.is_some());
        assert!(evidence.reviewed.is_none());
        assert!(matches!(
            GateSuccess::new(&evidence, 1),
            Err(loom_gate::GateFail {
                reason: loom_gate::GateFailReason::ReviewEvidenceMissing,
                ..
            })
        ));
        assert!(
            !controller
                .git
                .loom_workspace()
                .join(".loom/marker.json")
                .exists()
        );
    }

    async fn clean_push_fixture() -> (tempfile::TempDir, HandoffEvidence, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("gamma");
        let workspace = dir.path().to_path_buf();
        let git = git_workspace(&workspace);
        let gate_workspace = git.loom_workspace();
        let argv_log = dir.path().join("argv.log");
        let stub = dir.path().join("loom-stub.sh");
        let stub_body = format!(
            "set -euo pipefail\n\
             printf '%s\\n' \"$*\" >> {argv}\n\
             range=\"$4\"\n\
             tree_oid=$(git rev-parse 'HEAD^{{tree}}')\n\
             verify_log=\"${{LOOM_REVIEW_VERIFIED_LOG:?}}\"\n\
             config_digest=$(grep '\"driver_kind\":\"gate_run_end\"' \"$verify_log\" | tail -n 1 | sed -E 's/.*\"config_digest\":\"([^\"]*)\".*/\\1/')\n\
             stamp=$(date -u -d @$(($LOOM_REVIEW_PHASE_WHEN_MILLIS / 1000)) +%Y%m%dT%H%M%SZ)\n\
             log=\"$PWD/.loom/logs/review/review-${{stamp}}.jsonl\"\n\
             mkdir -p \"$(dirname \"$log\")\"\n\
             payload=$(printf '{{\"phase\":\"review\",\"push_range\":\"%s\",\"tree_oid\":\"%s\",\"config_digest\":\"%s\",\"log_path\":\"%s\",\"exit_code\":0,\"status\":\"success\",\"marker\":\"complete\",\"covered_hooks\":[]}}' \"$range\" \"$tree_oid\" \"$config_digest\" \"$log\")\n\
             printf '{{\"kind\":\"driver_event\",\"driver_kind\":\"gate_run_start\",\"payload\":%s}}\\n' \"$payload\" >> \"$log\"\n\
             printf '{{\"kind\":\"driver_event\",\"driver_kind\":\"gate_run_end\",\"payload\":%s}}\\n' \"$payload\" >> \"$log\"\n\
             printf 'LOOM_COMPLETE\\n'\n",
            argv = argv_log.to_string_lossy(),
        );
        loom_test_support::write_executable_bash_script(&stub, &stub_body)
            .expect("write loom review stub");

        let bd = BdClient::with_runner(molecule_lookup_script(
            dir.path(),
            "gamma",
            "lm-mol.9",
            "abc12345",
        ));
        let beads_push = dir.path().join("beads-push.sh");
        loom_test_support::write_executable_bash_script(&beads_push, "set -euo pipefail\nexit 0\n")
            .expect("write beads-push stub");
        let mut controller = ProductionAgentLoopController::new(
            bd,
            label.clone(),
            stub,
            workspace.clone(),
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        )
        .with_beads_push_bin(beads_push);

        let handoff = controller
            .exec_review()
            .await
            .expect("exec_review must succeed with populated evidence");
        (dir, handoff, gate_workspace)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn handoff_evidence_populates_typed_gate_scope_values() {
        let (_dir, handoff, gate_workspace) = clean_push_fixture().await;
        assert_eq!(handoff.molecule_state, MoleculeState::Clean);
        assert_eq!(handoff.gate_runs.len(), 2, "{:#?}", handoff.gate_runs);
        assert_eq!(handoff.gate_log_paths.len(), 2);
        for log_path in &handoff.gate_log_paths {
            assert!(
                log_path.starts_with(gate_workspace.join(".loom/logs")),
                "gate evidence must live with the integration checkout: {log_path:?}",
            );
            let body = std::fs::read_to_string(log_path).expect("log readable");
            assert!(body.contains("gate_run_start"), "{body}");
            assert!(body.contains("gate_run_end"), "{body}");
        }
        let verified = handoff.verified.as_ref().expect("verified scope");
        let reviewed = handoff.reviewed.as_ref().expect("reviewed scope");
        assert_eq!(verified.push_range(), reviewed.push_range());
        assert_eq!(verified.tree_oid(), reviewed.tree_oid());
        assert_eq!(verified.config_digest(), reviewed.config_digest());
        assert_eq!(handoff.review_marker, Some(ExitSignal::Complete));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn clean_push_mints_marker_after_covered_verify_and_review() {
        let (_dir, handoff, gate_workspace) = clean_push_fixture().await;
        assert!(handoff.verified.is_some());
        assert!(handoff.reviewed.is_some());
        let marker = loom_gate::verify_marker(&gate_workspace).expect("current marker");
        assert_eq!(
            marker.push_range(),
            handoff.push_range.as_deref().expect("range")
        );
        let local = loom_driver::git::sync_rev_parse(&gate_workspace, "HEAD").expect("local head");
        let remote =
            loom_driver::git::sync_rev_parse(&gate_workspace, "origin/main").expect("remote head");
        assert_eq!(
            local, remote,
            "clean gate must push after minting the marker"
        );
    }

    /// `specs/gate.md` § Rubric suppression registry: molecule handoff
    /// re-parses review findings after shape validation and removes
    /// suppressed rubric findings from recovery context while keeping
    /// unsuppressed siblings live.
    #[tokio::test(flavor = "multi_thread")]
    async fn molecule_completion_review_stashes_only_unsuppressed_findings() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("alpha");
        let workspace = dir.path().to_path_buf();
        seed_spec(&workspace, "alpha");
        let suppressed = crate::review::Finding {
            token: crate::review::ConcernToken::VerifierBypass,
            route: crate::review::FindingRoute::Deferred,
            bonds: vec![label.clone()],
            target: crate::review::FindingTarget::Annotation {
                target_string: "cargo test --lib suppressed".to_owned(),
            },
            evidence: "suppressed finding".to_owned(),
        };
        let unsuppressed = crate::review::Finding {
            token: crate::review::ConcernToken::VerifierBypass,
            route: crate::review::FindingRoute::Deferred,
            bonds: vec![label.clone()],
            target: crate::review::FindingTarget::Annotation {
                target_string: "cargo test --lib live".to_owned(),
            },
            evidence: "live finding".to_owned(),
        };
        std::fs::write(
            workspace.join("loom.toml"),
            format!(
                "[[suppress]]\nid = {:?}\nreason = \"false positive\"\n",
                suppressed.id()
            ),
        )
        .unwrap();
        std::fs::write(
            workspace.join("specs/alpha.md"),
            "# alpha\n\n- suppressed [check](cargo test --lib suppressed)\n- live [check](cargo test --lib live)\n",
        )
        .unwrap();
        let stub = dir.path().join("loom-stub.sh");
        let suppressed_payload = serde_json::to_string(&suppressed).unwrap();
        let unsuppressed_payload = serde_json::to_string(&unsuppressed).unwrap();
        let concern_payload = r#"{"summary":"reviewer flagged mixed findings"}"#;
        let stub_body = format!(
            "#!/bin/sh\n\
             case \"$1 $2\" in\n  \
               'gate review')\n    \
                 printf 'LOOM_FINDING: {suppressed_payload}\nLOOM_FINDING: {unsuppressed_payload}\nLOOM_CONCERN: {concern_payload}\n'\n    \
                 ;;\n\
             esac\n\
             exit 0\n",
        );
        std::fs::write(&stub, stub_body).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let bd = BdClient::with_runner(molecule_lookup_script(
            dir.path(),
            "alpha",
            "lm-mol.1",
            "deadbeef",
        ));
        let git = git_workspace(&workspace);
        std::fs::create_dir_all(git.loom_workspace().join("specs")).unwrap();
        std::fs::copy(
            workspace.join("specs/alpha.md"),
            git.loom_workspace().join("specs/alpha.md"),
        )
        .unwrap();
        std::fs::copy(
            workspace.join("loom.toml"),
            git.loom_workspace().join("loom.toml"),
        )
        .unwrap();
        let expected_stdout = format!(
            "LOOM_FINDING: {suppressed_payload}\nLOOM_FINDING: {unsuppressed_payload}\nLOOM_CONCERN: {concern_payload}\n"
        );
        let gate_workspace = git.loom_workspace();
        let validator = WorkspaceFindingValidator::new(&gate_workspace);
        let expected_walk =
            WalkOutput::from_stdout(&expected_stdout, DispatchScope::PerBead, &validator);
        assert_eq!(
            expected_walk.findings().len(),
            2,
            "fixture findings must validate: {:?}",
            expected_walk.finding_errors(),
        );
        let mut controller = ProductionAgentLoopController::new(
            bd,
            label,
            stub,
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        );

        let handoff = controller.exec_review().await.expect("exec_review ok");
        assert!(
            matches!(handoff.review_marker, Some(ExitSignal::Concern { .. })),
            "review output must preserve concern marker: {handoff:?}",
        );

        let stashed = controller
            .stashed_review_concern
            .as_ref()
            .expect("unsuppressed finding keeps review concern live");
        match stashed {
            PreviousFailure::ReviewConcern { findings, .. } => {
                assert_eq!(findings.len(), 1);
                assert_eq!(findings[0].id(), unsuppressed.id());
            }
            other => panic!("expected ReviewConcern, got {other:?}"),
        }
    }

    /// A molecule-completion concern travels through the production review
    /// subprocess, typed walk parser, molecule mint route, and stabilization
    /// prompt context in one controller invocation.
    #[tokio::test(flavor = "multi_thread")]
    async fn molecule_completion_review_routes_findings_to_stabilization_or_clarify() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("alpha");
        let workspace = dir.path().to_path_buf();
        seed_spec(&workspace, "alpha");

        // The stub emits one review finding so the production parser must preserve it.
        let argv_log = dir.path().join("argv.log");
        let stub = dir.path().join("loom-stub.sh");
        let finding_payload = r#"{"token":"verifier-bypass","route":"deferred","bonds":["alpha"],"target":{"kind":"Annotation","target_string":"cargo test --lib sample"},"evidence":"test mocks the agent backend"}"#;
        std::fs::write(
            workspace.join("specs/alpha.md"),
            "# alpha\n\n- sample [check](cargo test --lib sample)\n",
        )
        .unwrap();
        let concern_payload = r#"{"summary":"reviewer flagged a verifier-bypass"}"#;
        let stub_body = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> {argv}\n\
             case \"$1 $2\" in\n  \
               'gate review')\n    \
                 printf 'LOOM_FINDING: {finding}\\nLOOM_CONCERN: {concern}\\n'\n    \
                 ;;\n\
             esac\n\
             exit 0\n",
            argv = argv_log.to_string_lossy(),
            finding = finding_payload,
            concern = concern_payload,
        );
        std::fs::write(&stub, stub_body).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let bd_runner = molecule_lookup_script(dir.path(), "alpha", "lm-mol.1", "deadbeef");
        let bd_calls = bd_runner.calls_handle();
        let bd = BdClient::with_runner(bd_runner);
        let git = git_workspace(&workspace);
        std::fs::create_dir_all(git.loom_workspace().join("specs")).unwrap();
        std::fs::copy(
            workspace.join("specs/alpha.md"),
            git.loom_workspace().join("specs/alpha.md"),
        )
        .unwrap();
        // Capture the SpawnConfig the next `run_bead` synthesizes so we
        // can assert the rendered prompt carried the ReviewConcern body.
        let captured: Arc<Mutex<Option<SpawnConfig>>> = Arc::new(Mutex::new(None));
        let captured_inner = Arc::clone(&captured);
        let mut controller = ProductionAgentLoopController::new(
            bd,
            label.clone(),
            stub,
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            move |cfg: SpawnConfig, _bead_id: BeadId| {
                let captured = Arc::clone(&captured_inner);
                async move {
                    *captured.lock().unwrap() = Some(cfg);
                    (
                        SessionResult::Complete(SessionOutcome {
                            exit_code: 0,
                            cost_usd: None,
                        }),
                        Some(ExitSignal::Complete),
                    )
                }
            },
        );

        // Pre-condition: nothing stashed yet.
        assert!(controller.stashed_review_concern.is_none());

        controller.exec_review().await.expect("exec_review ok");

        {
            let calls = bd_calls.lock().expect("bd calls lock");
            let calls = calls
                .iter()
                .map(|args| {
                    args.iter()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let create = calls
                .iter()
                .find(|call| call.first().is_some_and(|arg| arg == "create"))
                .expect("review concern creates remediation");
            assert!(
                create
                    .windows(2)
                    .any(|pair| pair[0] == "--parent" && pair[1] == "lm-mol.1")
            );
            assert!(create.iter().any(|arg| arg.contains("loom:deferred")));
            assert!(
                calls
                    .iter()
                    .any(|call| { call == &["mol", "bond", "lm-mol.1", "lm-routed.1"] })
            );
            assert!(calls.iter().any(|call| {
                call.windows(2)
                    .any(|pair| pair[0] == "--status" && pair[1] == "deferred")
            }));
        }

        // The streamed findings + the terminal concern summary land on
        // the molecule-scoped stash as a typed
        // `PreviousFailure::ReviewConcern { summary, findings }`.
        let stashed = controller
            .stashed_review_concern
            .as_ref()
            .expect("stashed_review_concern populated after concern terminator");
        match stashed {
            PreviousFailure::ReviewConcern { summary, findings } => {
                assert_eq!(
                    summary, "reviewer flagged a verifier-bypass",
                    "terminal summary threaded verbatim",
                );
                assert_eq!(findings.len(), 1, "one streamed finding parsed");
                assert_eq!(
                    findings[0].token,
                    crate::review::ConcernToken::VerifierBypass,
                );
                assert_eq!(findings[0].evidence, "test mocks the agent backend");
            }
            other => panic!("expected ReviewConcern, got {other:?}"),
        }

        // The next `run_bead` call (fresh dispatch — `previous_failure =
        // None`) consumes the molecule-scoped stash; the typed
        // `PreviousFailure::ReviewConcern` rides into the rendered
        // prompt verbatim. The dispatch closure captures the
        // `SpawnConfig.initial_prompt` so the assertions can scan the
        // exact prompt body the agent would receive.
        let fixup_bead = bead("lm-fixup");
        let _ = controller
            .run_bead(&fixup_bead, None)
            .await
            .expect("run_bead ok");
        assert!(
            controller.stashed_review_concern.is_none(),
            "stash consumed by the next run_bead call",
        );
        let cfg = captured
            .lock()
            .unwrap()
            .take()
            .expect("dispatch closure invoked");
        let prompt = &cfg.initial_prompt;
        assert!(
            prompt.contains("Review raised a concern"),
            "rendered prompt must carry the ReviewConcern framing: {prompt}",
        );
        assert!(
            prompt.contains("verifier-bypass"),
            "rendered prompt must name the finding's token: {prompt}",
        );
        assert!(
            prompt.contains("test mocks the agent backend"),
            "rendered prompt must include the finding's evidence: {prompt}",
        );
    }

    /// FR1 negative: when no `current_molecule` pointer exists for the
    /// spec, `exec_review` MUST surface `NoActiveMolecule` rather than
    /// silently falling back to `--tree` — the push-gate scope is
    /// load-bearing and a missing molecule means the run is
    /// specs/gate.md § "Persistence boundary: agent narrates, agent persists":
    /// when the bead under dispatch carries a well-formed `## Options — …`
    /// block, the runner's `apply_clarify` only stamps the `loom:clarify`
    /// label — the canonical block belongs to the agent, written to bead
    /// state *before* `LOOM_CLARIFY` is emitted. If the runner also wrote
    /// the agent's stdout reason-line via `bd update --notes`, every
    /// re-emit would clobber the canonical block and leave `loom inbox`'s
    /// queue empty.
    #[tokio::test(flavor = "multi_thread")]
    async fn apply_clarify_does_not_write_notes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let well_formed = "## Options — pick a path\\n\\n### Option 1 — first\\nbody";
        let show_row = format!(
            r#"[{{"id":"lm-clarify.1","title":"t","status":"open","priority":2,"issue_type":"task","description":"{well_formed}"}}]"#,
        );
        let scripted = ScriptedBd::new([ok_stdout(show_row.as_bytes()), ok_stdout(b"")]);
        let calls = scripted.calls_handle();
        let bd = BdClient::with_runner(scripted);
        let mut controller = ProductionAgentLoopController::new(
            bd,
            SpecLabel::new("gate"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        );
        let bead_id = BeadId::new("lm-clarify.1").expect("bead id");
        controller
            .apply_clarify(
                &bead_id,
                "agent's one-line reason that must not clobber notes",
            )
            .await
            .expect("apply_clarify ok");
        let captured = calls.lock().unwrap();
        assert_eq!(
            captured.len(),
            2,
            "expected one bd show + one bd update invocation",
        );
        let update_argv: Vec<String> = captured[1]
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(update_argv[0], "update");
        assert_eq!(update_argv[1], "lm-clarify.1");
        assert!(
            update_argv.iter().any(|a| a == "--add-label"),
            "missing --add-label in argv: {update_argv:?}",
        );
        assert!(
            update_argv.iter().any(|a| a == "loom:clarify"),
            "missing loom:clarify label in argv: {update_argv:?}",
        );
        assert!(
            !update_argv.iter().any(|a| a == "--notes"),
            "apply_clarify must not forward --notes when options block is \
             well-formed (persistence boundary): {update_argv:?}",
        );
        assert!(
            update_argv
                .windows(2)
                .any(|w| w[0] == "--status" && w[1] == "blocked"),
            "apply_clarify must pair --status blocked with --add-label so \
             `bd ready` excludes via its native status filter: {update_argv:?}",
        );
    }

    /// Dedup contract: `apply_blocked` must set `--status blocked` in the
    /// same `bd update` invocation as `--add-label loom:blocked`. Without
    /// this, the run loop would re-dispatch the blocked bead every pass —
    /// the regression that motivated lm-uzrc.
    #[tokio::test]
    async fn apply_blocked_pairs_status_blocked_with_add_label() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let scripted = ScriptedBd::new([RunOutput {
            status: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }]);
        let calls = scripted.calls_handle();
        let bd = BdClient::with_runner(scripted);
        let mut controller = ProductionAgentLoopController::new(
            bd,
            SpecLabel::new("gate"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        );
        let bead_id = BeadId::new("lm-blocked.1").expect("bead id");
        controller
            .apply_blocked(&bead_id, "agent-blocked", "operator decision needed")
            .await
            .expect("apply_blocked ok");
        let captured = calls.lock().unwrap();
        assert_eq!(captured.len(), 1, "exactly one bd invocation expected");
        let argv: Vec<String> = captured[0]
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv[0], "update");
        assert_eq!(argv[1], "lm-blocked.1");
        assert!(
            argv.iter().any(|a| a == "loom:blocked"),
            "missing loom:blocked label in argv: {argv:?}",
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--status" && w[1] == "blocked"),
            "apply_blocked must pair --status blocked with --add-label so \
             `bd ready` excludes via its native status filter: {argv:?}",
        );
    }

    #[tokio::test]
    async fn apply_infra_pairs_status_blocked_with_infra_label_and_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let scripted = ScriptedBd::new([RunOutput {
            status: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }]);
        let calls = scripted.calls_handle();
        let bd = BdClient::with_runner(scripted);
        let mut controller = ProductionAgentLoopController::new(
            bd,
            SpecLabel::new("gate"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        );
        let bead_id = BeadId::new("lm-infra.1").expect("bead id");
        let diagnostic = InfraDiagnostic::retryable(
            "infra-preflight",
            "infra-preflight",
            "podman load failed".to_string(),
            2,
            3,
            false,
        );

        controller
            .apply_infra(&bead_id, &diagnostic)
            .await
            .expect("apply_infra ok");

        let captured = calls.lock().unwrap();
        let argv: Vec<String> = captured[0]
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(argv.iter().any(|a| a == "loom:infra"), "{argv:?}");
        assert!(!argv.iter().any(|a| a == "loom:blocked"), "{argv:?}");
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--status" && w[1] == "blocked"),
            "{argv:?}",
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--set-metadata" && w[1] == "loom.infra.attempt=2"),
            "{argv:?}",
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--set-metadata" && w[1] == "loom.infra.class=infra-preflight"),
            "{argv:?}",
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--notes"
                    && w[1].starts_with("infra-preflight: podman load failed")),
            "{argv:?}",
        );
    }

    /// Dedup contract: `next_ready_bead` must NOT forward `--exclude-label`
    /// any more. The argv to `bd ready` carries `--label` for the spec
    /// filter and no exclude flags — clarify/blocked dedup is now anchored
    /// in the paired `status=blocked` transition.
    #[tokio::test]
    async fn next_ready_bead_does_not_forward_exclude_label() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let scripted = ScriptedBd::new([ok_stdout(b"[]\n")]);
        let calls = scripted.calls_handle();
        let bd = BdClient::with_runner(scripted);
        let mut controller = ProductionAgentLoopController::new(
            bd,
            SpecLabel::new("gate"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        );
        let _ = controller.next_ready_bead(&[]).await.expect("ready ok");
        let captured = calls.lock().unwrap();
        assert_eq!(
            captured.len(),
            2,
            "ready lookup then infra fallback expected"
        );
        let argv: Vec<String> = captured[0]
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv[0], "ready");
        assert!(
            !argv.iter().any(|a| a.starts_with("--exclude-label")),
            "next_ready_bead must NOT forward --exclude-label; argv={argv:?}",
        );
    }

    #[tokio::test]
    async fn next_ready_bead_with_ready_parent_omits_spec_filter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let scripted = ScriptedBd::new([ok_stdout(b"[]\n")]);
        let calls = scripted.calls_handle();
        let bd = BdClient::with_runner(scripted);
        let mut controller = ProductionAgentLoopController::new(
            bd,
            SpecLabel::new("agent"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        )
        .with_ready_parent(BeadId::new("lm-root").expect("valid bead id"));
        let _ = controller.next_ready_bead(&[]).await.expect("ready ok");
        let captured = calls.lock().unwrap();
        assert_eq!(
            captured.len(),
            2,
            "ready lookup then infra fallback expected"
        );
        let argv: Vec<String> = captured[0]
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(
            argv.iter().any(|arg| arg == "--parent=lm-root"),
            "ready lookup must be scoped to the active work epic: {argv:?}",
        );
        assert!(
            !argv.iter().any(|arg| arg.starts_with("--label=")),
            "multi-spec work epic lookup must not narrow by spec: {argv:?}",
        );
    }

    /// Spec gate (`specs/harness.md` § Worker-queue resolution): the
    /// per-bead loop dispatches LEAVES, not molecule containers. A `bd
    /// ready` response carrying a `type=epic` bead must be skipped over
    /// and the next non-epic bead surfaced; the epic-skip emits an
    /// info-level log line so operators see the routing decision.
    #[tokio::test]
    async fn worker_queue_skips_epic_type_beads_with_info_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let body = br#"[
            {"id":"lm-epic.1","title":"epic","description":"","status":"open","priority":2,"issue_type":"epic","labels":["spec:gate","profile:base"]},
            {"id":"lm-leaf.2","title":"leaf","description":"","status":"open","priority":2,"issue_type":"task","labels":["spec:gate","profile:base"]}
        ]"#;
        let scripted = ScriptedBd::new([ok_stdout(body)]);
        let bd = BdClient::with_runner(scripted);
        let mut controller = ProductionAgentLoopController::new(
            bd,
            SpecLabel::new("gate"),
            PathBuf::from("/loom/bin"),
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        );
        let picked = controller
            .next_ready_bead(&[])
            .await
            .expect("ready ok")
            .expect("a non-epic bead must surface");
        assert_eq!(
            picked.id.as_str(),
            "lm-leaf.2",
            "worker queue must skip the epic and return the leaf bead",
        );
    }

    /// Spec criterion (`specs/gate.md` § *Production walker wiring*):
    /// the production per-bead gate invokes deterministic verify as the
    /// only real subprocess against `loom_bin`.
    #[tokio::test]
    async fn exec_per_bead_gate_invokes_post_integration_verify_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let stub = dir.path().join("loom-stub.sh");
        loom_test_support::write_executable_bash_script(
            &stub,
            "set -euo pipefail\n\
             echo \"$*\" >> \"$PWD/argv.log\"\n\
             case \"$2\" in\n\
                 review|mint) echo \"$2 must not run in the per-bead hot path\" >&2; exit 99 ;;\n\
             esac\n\
             exit 0\n",
        )
        .expect("write stub");

        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("gate"),
            stub.clone(),
            workspace.clone(),
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                panic!("spawn closure must not fire during exec_per_bead_gate");
            },
        );

        let bead_id = BeadId::new("lm-1").expect("valid bead id");
        let pre_tip = loom_driver::git::sync_head_commit_sha(&controller.git.loom_workspace())
            .expect("pre-integration sha");
        controller.pre_integration_tip = Some(pre_tip.clone());
        let outcome = controller
            .exec_per_bead_gate(&bead_id)
            .await
            .expect("exec_per_bead_gate ok");
        assert_eq!(outcome, PerBeadGateOutcome::Clean);

        let operator_log = workspace.join("argv.log");
        assert!(
            !operator_log.exists(),
            "post-integration gate must not run in stale operator checkout",
        );
        let log_path = controller.git.loom_workspace().join("argv.log");
        let log = std::fs::read_to_string(log_path).expect("argv.log readable");
        let calls: Vec<&str> = log.lines().collect();
        assert_eq!(
            calls.len(),
            1,
            "exec_per_bead_gate must spawn exactly one subprocess: {calls:?}",
        );
        assert_eq!(
            calls[0],
            format!("gate verify --diff {pre_tip}..HEAD"),
            "subprocess argv must be exactly `loom gate verify --diff <pre-integration-head>..HEAD`",
        );
    }

    #[tokio::test]
    async fn exec_per_bead_gate_releases_lock_before_spawning_children() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("gate");
        let root = BeadId::new("lm-gate").expect("valid bead id");
        let mgr = loom_driver::lock::LockManager::with_state_home(&workspace, dir.path())
            .expect("lock manager");
        let clock = loom_driver::clock::SystemClock::new();
        let guard = mgr
            .acquire_work_root_async(&root, &clock)
            .await
            .expect("first acquire");
        let lock_path = mgr.locks_dir().join("lm-gate.lock");
        let stub = dir.path().join("loom-stub.sh");
        loom_test_support::write_executable_bash_script(
            &stub,
            &format!(
                "set -euo pipefail\n\
                 exec 9>\"{}\"\n\
                 flock -n 9\n\
                 if [[ \"$2\" == \"review\" ]]; then\n\
                     exit 99\n\
                 fi\n\
                 exit 0\n",
                lock_path.display(),
            ),
        )
        .expect("write stub");

        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            label,
            stub,
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                panic!("spawn closure must not fire during exec_per_bead_gate");
            },
        )
        .with_handoff_lock(guard);

        let bead_id = BeadId::new("lm-1").expect("valid bead id");
        let outcome = controller
            .exec_per_bead_gate(&bead_id)
            .await
            .expect("exec_per_bead_gate ok");
        assert_eq!(outcome, PerBeadGateOutcome::Clean);
    }

    /// A non-zero `loom gate verify --diff` against the integrated tree is
    /// the `post-integrate-fail` audit failure: the integration is rolled
    /// back (`git reset --hard HEAD~1`), `PreviousFailure::PostIntegrateFail`
    /// is stashed for the next dispatch, the review step never runs, and the
    /// bead routes to recovery (specs/harness.md § Verdict Gate).
    #[tokio::test]
    async fn post_integrate_verify_failure_writes_durable_gate_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let loom_ws = git.loom_workspace();
        let base_tip = loom_driver::git::sync_head_commit_sha(&loom_ws).expect("base sha");

        // Stand in for the just-ff'd bead commit so `reset --hard HEAD~1`
        // has a commit to unwind back to the pre-merge tip.
        std::fs::write(loom_ws.join("integrated.txt"), "merged\n").expect("write");
        loom_driver::git::commit_all_in(&loom_ws, "integrated bead").expect("commit");
        let merged_tip = loom_driver::git::sync_head_commit_sha(&loom_ws).expect("ff'd sha");
        let merged_tree = loom_driver::git::head_tree_oid_sync(&loom_ws).expect("merged tree");
        assert_ne!(
            merged_tip.to_string(),
            base_tip.to_string(),
            "the ff-merge stand-in must advance the integration branch",
        );

        let stub = dir.path().join("loom-stub.sh");
        loom_test_support::write_executable_bash_script(
            &stub,
            "set -euo pipefail\n\
             case \"$2\" in\n\
                 verify) echo 'verifier failed: cargo test' >&2; exit 1 ;;\n\
                 review|mint) echo \"$2 must not run after verify-fail\" ; exit 99 ;;\n\
             esac\n\
             exit 0\n",
        )
        .expect("write stub");

        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("gate"),
            stub.clone(),
            workspace.clone(),
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                panic!("spawn closure must not fire during exec_per_bead_gate");
            },
        );

        // `run_bead` would set this before the ff-merge; the test drives
        // `exec_per_bead_gate` directly, so seed the pre-merge tip the
        // advancing commit moved past.
        controller.pre_integration_tip = Some(base_tip.clone());

        let bead_id = BeadId::new("lm-1").expect("valid bead id");
        let outcome = controller
            .exec_per_bead_gate(&bead_id)
            .await
            .expect("exec_per_bead_gate ok");

        assert!(
            matches!(outcome, PerBeadGateOutcome::Recovery { .. }),
            "verify-fail must route to Recovery, got {outcome:?}",
        );
        let rolled_back = loom_driver::git::sync_head_commit_sha(&loom_ws)
            .expect("rolled-back sha")
            .to_string();
        assert_eq!(
            rolled_back,
            base_tip.to_string(),
            "audit-fail must reset the integration branch back to its pre-merge tip",
        );
        assert!(
            !loom_ws.join("integrated.txt").exists(),
            "reset --hard must drop the rolled-back commit's content",
        );
        match controller.stashed_previous_failure {
            Some(PreviousFailure::PostIntegrateFail {
                ref failures,
                ref gate_log_path,
            }) => {
                assert_eq!(failures.len(), 1, "one verifier-failure block expected");
                assert_eq!(failures[0].exit_code, 1);
                assert!(
                    failures[0].target.contains("loom gate verify --diff"),
                    "failure block names the failing verifier: {:?}",
                    failures[0].target,
                );
                assert!(
                    gate_log_path.exists(),
                    "durable gate log must exist before rollback: {gate_log_path:?}",
                );
                assert_eq!(
                    gate_log_path.extension().and_then(|ext| ext.to_str()),
                    Some("jsonl"),
                    "durable post-integrate log must be JSONL: {gate_log_path:?}",
                );
                let gate_log = std::fs::read_to_string(gate_log_path).expect("gate log readable");
                let events = gate_log
                    .lines()
                    .map(|line| {
                        serde_json::from_str::<loom_events::AgentEvent>(line)
                            .expect("every gate JSONL line is a canonical AgentEvent")
                    })
                    .collect::<Vec<_>>();
                assert_eq!(events.len(), 5, "{events:?}");
                let lifecycle_kinds = events[..4]
                    .iter()
                    .map(|event| match event {
                        loom_events::AgentEvent::DriverEvent { driver_kind, .. } => {
                            driver_kind.as_wire()
                        }
                        other => panic!("gate lifecycle must use driver events, got {other:?}"),
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    lifecycle_kinds,
                    [
                        "gate_run_start",
                        "gate_run_scope",
                        "gate_run_lane",
                        "gate_run_end",
                    ]
                );
                let loom_events::AgentEvent::DriverEvent {
                    driver_kind,
                    payload,
                    ..
                } = &events[4]
                else {
                    panic!("post-integrate diagnostics must be a driver event");
                };
                assert_eq!(*driver_kind, DriverKind::VerdictGate);
                assert_eq!(
                    payload["argv"],
                    serde_json::json!(["gate", "verify", "--diff", format!("{}..HEAD", base_tip)])
                );
                assert_eq!(payload["scope_flag"], "--diff");
                assert_eq!(payload["exit_code"], 1);
                assert_eq!(payload["integration_sha"], merged_tip.to_string());
                assert_eq!(payload["bead_id"], "lm-1");
                assert_eq!(payload["retry_attempt"], 0);
                assert_eq!(payload["rollback_state"], "pending");
                assert_eq!(
                    payload["log_path"].as_str(),
                    Some(gate_log_path.to_string_lossy().as_ref())
                );
                assert_eq!(payload["stdout"], "");
                assert!(
                    payload["stderr"]
                        .as_str()
                        .is_some_and(|output| output.contains("cargo test")),
                    "{payload:?}",
                );
                assert_eq!(payload["failures"][0]["exit_code"], 1);
                assert!(
                    payload["failures"][0]["target"]
                        .as_str()
                        .is_some_and(|target| target.contains("loom gate verify --diff"))
                );
                let runs = parse_gate_runs_from_jsonl(gate_log_path);
                assert_eq!(runs.len(), 1, "{runs:?}");
                assert_eq!(runs[0].status, GateRunStatus::Failed);
                assert_eq!(runs[0].exit_code, Some(1));
                assert_eq!(runs[0].push_range, format!("{}..HEAD", base_tip));
                assert_eq!(runs[0].tree_oid, merged_tree.to_string());
                assert_eq!(runs[0].log_path, *gate_log_path);
            }
            other => panic!("expected stashed PostIntegrateFail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn gate_invocations_write_separate_jsonl_logs_with_parent_breadcrumb() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("gate");
        let bead_id = BeadId::new("lm-1").expect("valid bead id");
        let logs_root = dir.path().join("logs");
        let mut sink = loom_driver::logging::LogSink::open_in_at(
            &logs_root,
            &label,
            &bead_id,
            None,
            SystemTime::UNIX_EPOCH,
        )
        .expect("open bead log");
        let bead_log_path = sink.log_path().to_path_buf();
        sink.finish(loom_driver::logging::BeadOutcome::Done)
            .expect("finish bead log");

        let stub = dir.path().join("loom-stub.sh");
        loom_test_support::write_executable_bash_script(
            &stub,
            "set -euo pipefail\n\
             case \"$2\" in\n\
                 verify) echo 'verify stdout'; echo 'verify stderr' >&2; exit 2 ;;\n\
             esac\n\
             exit 0\n",
        )
        .expect("write stub");

        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            label,
            stub,
            workspace,
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                panic!("spawn closure must not fire during exec_per_bead_gate");
            },
        )
        .with_phase_log_root(logs_root);
        controller.prepare_emit_state(&bead_id);

        let outcome = controller
            .exec_per_bead_gate(&bead_id)
            .await
            .expect("exec_per_bead_gate ok");
        assert!(
            matches!(outcome, PerBeadGateOutcome::Recovery { .. }),
            "verify-fail must route to Recovery, got {outcome:?}",
        );
        let Some(PreviousFailure::PostIntegrateFail { gate_log_path, .. }) =
            &controller.stashed_previous_failure
        else {
            panic!("expected PostIntegrateFail with gate_log_path");
        };
        let first_gate_log_path = gate_log_path.clone();
        controller.current_attempt = 1;
        let second_outcome = controller
            .exec_per_bead_gate(&bead_id)
            .await
            .expect("second exec_per_bead_gate ok");
        assert!(
            matches!(second_outcome, PerBeadGateOutcome::Recovery { .. }),
            "second verify-fail must route to Recovery, got {second_outcome:?}",
        );
        let Some(PreviousFailure::PostIntegrateFail {
            gate_log_path: second_gate_log_path,
            ..
        }) = &controller.stashed_previous_failure
        else {
            panic!("expected second PostIntegrateFail with gate_log_path");
        };
        assert_ne!(
            &first_gate_log_path, second_gate_log_path,
            "retry attempts must write distinct durable gate logs",
        );
        for path in [&first_gate_log_path, second_gate_log_path] {
            let child_log = std::fs::read_to_string(path).expect("gate child log readable");
            for line in child_log.lines() {
                serde_json::from_str::<loom_events::AgentEvent>(line)
                    .expect("gate child log line is a canonical AgentEvent");
            }
            let runs = parse_gate_runs_from_jsonl(path);
            assert_eq!(runs.len(), 1, "{runs:?}");
            assert_eq!(runs[0].status, GateRunStatus::Failed);
        }
        let first_gate_log = first_gate_log_path.to_string_lossy().to_string();
        let second_gate_log = second_gate_log_path.to_string_lossy().to_string();
        let body = std::fs::read_to_string(bead_log_path).expect("driver event log readable");
        let events: Vec<serde_json::Value> = body
            .lines()
            .map(|line| serde_json::from_str(line).expect("driver event json"))
            .collect();
        let gate_events = events
            .iter()
            .filter(|event| event["payload"].get("gate_log_path").is_some())
            .collect::<Vec<_>>();
        assert_eq!(
            gate_events.len(),
            2,
            "each verify-fail retry must emit a driver_event carrying gate_log_path: {body}",
        );
        for gate_log in [first_gate_log, second_gate_log] {
            let expected = serde_json::json!(&gate_log);
            let event = gate_events
                .iter()
                .find(|event| event["payload"]["gate_log_path"] == expected)
                .unwrap_or_else(|| {
                    panic!("driver_event payload must name gate_log_path {gate_log}: {body}")
                });
            assert!(
                event["summary"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(&format!("gate log: {gate_log}")),
                "driver_event summary must render gate log path {gate_log}: {body}",
            );
        }
    }

    /// A verify-fail on a bead whose ff advanced nothing
    /// (`pre_integration_tip == None`) must NOT roll back — there is no
    /// bead commit to unwind, and `reset --hard HEAD~1` against a tip with
    /// no parent would abort the loop. The bead still stashes
    /// `PostIntegrateFail` and routes to recovery.
    #[tokio::test]
    async fn exec_per_bead_gate_verify_fail_skips_rollback_when_integration_did_not_advance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let loom_ws = git.loom_workspace();
        let root_tip = loom_driver::git::sync_head_commit_sha(&loom_ws)
            .expect("root sha")
            .to_string();

        let stub = dir.path().join("loom-stub.sh");
        loom_test_support::write_executable_bash_script(
            &stub,
            "set -euo pipefail\n\
             case \"$2\" in\n\
                 verify) echo 'verifier failed' >&2; exit 1 ;;\n\
             esac\n\
             exit 0\n",
        )
        .expect("write stub");

        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("gate"),
            stub.clone(),
            workspace.clone(),
            git,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                panic!("spawn closure must not fire during exec_per_bead_gate");
            },
        );
        // `pre_integration_tip` stays None: the bead committed nothing.

        let bead_id = BeadId::new("lm-1").expect("valid bead id");
        let outcome = controller
            .exec_per_bead_gate(&bead_id)
            .await
            .expect("exec_per_bead_gate must not abort when there is nothing to roll back");

        assert!(
            matches!(outcome, PerBeadGateOutcome::Recovery { .. }),
            "verify-fail still routes to Recovery, got {outcome:?}",
        );
        assert_eq!(
            loom_driver::git::sync_head_commit_sha(&loom_ws)
                .expect("post sha")
                .to_string(),
            root_tip,
            "no-advance verify-fail must leave the integration tip untouched",
        );
        assert!(
            matches!(
                controller.stashed_previous_failure,
                Some(PreviousFailure::PostIntegrateFail { .. })
            ),
            "PostIntegrateFail is stashed even when nothing was rolled back",
        );
    }
}
