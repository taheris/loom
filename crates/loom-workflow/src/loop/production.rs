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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use loom_driver::agent::{AgentRuntime, ProtocolError, SpawnConfig};
use loom_driver::bd::{
    BdClient, Bead, CommandRunner, ListOpts, ReadyOpts, TokioRunner, UpdateOpts,
};
use loom_driver::clock::{Clock, SystemClock};
use loom_driver::config::{LoomConfig, LoomTopConfig, Phase, SkillsConfig};
use loom_driver::git::{CreatedWorktree, GitClient, GitOid, RebaseOutcome};
use loom_driver::identifier::{BeadId, ProfileName, SpecLabel};
use loom_driver::lock::LockGuard;
use loom_driver::logging::phase_log_path;
use loom_driver::profile_manifest::{ProfileError, ProfileImageManifest};
use loom_driver::scratch::resolve_scratch_key;
use loom_events::DriverKind;
use tokio::process::Command;
use tracing::{info, warn};

use loom_gate::{
    GateRun, HandoffEvidence, append_gate_run_lifecycle_events, parse_gate_runs_from_jsonl,
};

use super::context::{LoopContextInputs, render_loop_prompt};
use super::driver_emit::BeadEmit;
use super::error::LoopError;
use super::outcome::{AgentOutcome, SessionResult};
use super::runner::{AgentLoopController, PerBeadGateOutcome};
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
use loom_templates::run::PreviousFailure;

/// Env var the molecule-completion handoff sets when spawning `loom gate
/// review` so the child re-uses the parent's pinned `phase_when`
/// timestamp. With both sides computing the JSONL log path from the
/// same `(logs_root, label, "review", when)` tuple, the parent can
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
    fixed_queue: Option<VecDeque<Bead>>,
    /// Optional work-epic scope for active/explicit epic loops. When set,
    /// ready lookup is constrained to descendants of this work root rather
    /// than to a single `spec:<label>` so multi-spec todo batches can run.
    ready_parent: Option<BeadId>,
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
            fixed_queue: None,
            ready_parent: None,
        }
    }

    pub fn with_fixed_queue(mut self, queue: VecDeque<Bead>) -> Self {
        self.fixed_queue = Some(queue);
        self
    }

    pub fn with_ready_parent(mut self, parent: BeadId) -> Self {
        self.ready_parent = Some(parent);
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
    fn emit_to_log(&mut self, kind: DriverKind, summary: &str, payload: serde_json::Value) {
        if let Some(state) = self.current_emit.as_mut() {
            state.emit(kind, summary, payload);
        }
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
}

impl<S, F, R: CommandRunner> AgentLoopController for ProductionAgentLoopController<S, F, R>
where
    S: Fn(SpawnConfig, BeadId) -> F + Send,
    F: std::future::Future<Output = (SessionResult, Option<ExitSignal>)> + Send,
{
    async fn next_ready_bead(&mut self) -> Result<Option<Bead>, LoopError> {
        if let Some(queue) = self.fixed_queue.as_mut() {
            return Ok(queue.pop_front());
        }
        // Dedup of clarify/blocked beads relies on the paired
        // `status=blocked` transition that `apply_clarify` / `apply_blocked`
        // write alongside the label. `bd ready` natively excludes
        // status=blocked, so no exclude-label flag is needed.
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
        Ok(None)
    }

    async fn run_bead(
        &mut self,
        bead: &Bead,
        previous_failure: Option<String>,
    ) -> Result<AgentOutcome, LoopError> {
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
        info!(
            bead = %bead.id,
            path = %worktree.path.display(),
            branch = %worktree.branch,
            "dispatching agent against per-bead workspace",
        );
        // Drop any uncommitted mid-session leftovers from a prior attempt
        // while preserving the bead branch's HEAD (i.e. the agent's prior
        // commits) and the warm caches under `target/` + `.wrix/`. No-op
        // on the first attempt against a freshly-cloned tree.
        self.git.reset_bead_clone(&worktree.path).await?;

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
        let attempt = u32::from(is_retry);
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
            Err(e) => {
                drop(scratch);
                return Err(LoopError::Profile(e));
            }
        };
        let skill_session = skill_plan.materialize(scratch.path(), &worktree.path)?;
        spawn_config.skills = Some(skill_session.registered);
        info!(
            bead = %bead.id,
            image_ref = %spawn_config.image_ref,
            worktree = %worktree.path.display(),
            retry = is_retry,
            "loom loop: dispatching agent",
        );
        let (session, marker) = (self.spawn)(spawn_config, bead.id.clone()).await;
        let marker_is_noop = matches!(marker.as_ref(), Some(ExitSignal::Noop));
        drop(scratch);
        // Resolve the per-bead log file the closure just finished writing
        // to so subsequent driver events (tree-not-clean / merge / push /
        // cleanup) land in the same JSONL the agent's events live in.
        self.prepare_emit_state(&bead.id);

        let outcome = classify_session(session, marker);
        if outcome == AgentOutcome::Success {
            // Tree-clean precedes verify-fail / review-concern per
            // `specs/harness.md` § Verdict Gate. The pre-attempt
            // `reset_bead_clone` (run before dispatch) is what
            // guarantees the agent saw an empty starting tree, so any
            // dirty porcelain entry is necessarily an agent leftover
            // or a reset-step bug — running verifiers against a
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
                // `reset_bead_clone`, which drops these dirty leftovers while
                // keeping any committed work on the bead branch.
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
                            "bead workspace preserved until durable push for {}",
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
        }
        // Verdict-gate direct-emit LOOM_CLARIFY check (specs/gate.md §
        // Options Format Contract): inspect the bead under dispatch for a
        // well-formed `## Options — …` block. Well-formed → loom:clarify;
        // malformed / absent → loom:blocked with cause
        // `clarify-without-options` so `loom inbox`'s queue is not handed
        // an empty options block.
        crate::gate_clarify::apply_clarify_or_blocked(&self.bd, bead).await?;
        Ok(())
    }

    async fn apply_blocked(
        &mut self,
        bead: &BeadId,
        cause: &str,
        error: &str,
    ) -> Result<(), LoopError> {
        // Notes layout pins the cause string at the head so `bd show
        // --notes` greps cleanly for `infra-preflight` / `infra-repeated`
        // even when the raw error body is multi-line. Spec
        // (`harness.md` §"Verdict Gate · Infra failures") names the
        // cause as the routing identifier; the error detail is for human
        // triage only.
        let notes = if error.is_empty() {
            cause.to_string()
        } else {
            format!("{cause}: {error}")
        };
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
        Ok(())
    }

    async fn exec_review(&mut self) -> Result<HandoffEvidence, LoopError> {
        self.release_handoff_lock_for_child_gate();
        let actual = self.git.prepare_actual_push_range().await?;
        let diff_range = actual.range.clone();
        let phase_when = SystemClock::new().wall_now();
        let phase_when_millis = phase_when
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let review_log_path = self
            .logs_root
            .as_deref()
            .map(|root| phase_log_path(root, &self.label, "review", phase_when));
        self.git.run_pre_push_chain().await?;
        let config_digest = pre_commit_config_digest(&self.workspace)?;
        if let Some(path) = review_log_path.as_deref() {
            append_gate_run_lifecycle_events(
                path,
                &GateRun::successful_verify(
                    diff_range.clone(),
                    actual.tree_oid.to_string(),
                    config_digest.clone(),
                    path.to_path_buf(),
                    pre_push_hook_coverage(),
                ),
            )?;
        }
        info!(
            spec = %self.label.as_str(),
            diff = %diff_range,
            "loom loop: molecule handoff — pre-push chain finished",
        );
        let gate_workspace = self.git.loom_workspace();
        let review_output = Command::new(&self.loom_bin)
            .current_dir(gate_workspace)
            .env(REVIEW_PHASE_WHEN_ENV, phase_when_millis.to_string())
            .env(REVIEW_EMIT_STDOUT_ENV, "1")
            .env(REVIEW_SPEC_LABEL_ENV, self.label.as_str())
            .arg("gate")
            .arg("review")
            .arg("--diff")
            .arg(&diff_range)
            .output()
            .await?;
        let review_status = review_output.status;
        info!(
            spec = %self.label.as_str(),
            diff = %diff_range,
            exit_code = review_status.code().unwrap_or(-1),
            "loom loop: molecule handoff — loom gate review --diff finished",
        );
        let review_stdout = String::from_utf8_lossy(&review_output.stdout);
        let review_stderr = String::from_utf8_lossy(&review_output.stderr);
        if !review_status.success() {
            return Err(LoopError::ReviewHandoff {
                detail: format!(
                    "loom gate review --diff {diff_range} exited {code}\nstdout:\n{stdout}\nstderr:\n{stderr}",
                    code = review_status.code().unwrap_or(-1),
                    stdout = review_stdout,
                    stderr = review_stderr,
                ),
            });
        }
        let raw_review_marker = parse_exit_signal(&review_stdout);
        // Parse the typed walk product once: streamed `LOOM_FINDING:`
        // lines + the terminal marker shape. When the terminal is a
        // well-formed `LOOM_CONCERN` AND at least one finding rode
        // through, stash `PreviousFailure::ReviewConcern { summary,
        // findings }` so the next `run_bead` call (typically a
        // fix-up bead's first attempt) renders the parsed findings
        // verbatim in the recovery prompt per `specs/harness.md`
        // § *molecule_completion_review_threads_findings_into_previous_failure_review_concern*.
        // The mint path is NOT fired here — push-stage is `audit`,
        // inspection-only per `specs/gate.md` § *Stages*.
        let validator = WorkspaceFindingValidator::new(&self.workspace);
        let walk = WalkOutput::from_stdout(&review_stdout, DispatchScope::PerBead, &validator);
        let config = LoomConfig::load(LoomConfig::resolve_path(&self.workspace))?;
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
            raw_review_marker
        };
        if let TerminalSurface::Concern { summary } = walk.terminal()
            && !unsuppressed_findings.is_empty()
        {
            self.stashed_review_concern = Some(PreviousFailure::ReviewConcern {
                summary: summary.clone(),
                findings: unsuppressed_findings,
            });
        }
        if let Some(path) = review_log_path.as_deref() {
            let marker = review_marker.clone().unwrap_or(ExitSignal::Complete);
            append_gate_run_lifecycle_events(
                path,
                &GateRun::successful_review(
                    diff_range.clone(),
                    actual.tree_oid.to_string(),
                    config_digest.clone(),
                    path.to_path_buf(),
                    marker,
                ),
            )?;
        }
        let mut evidence = review_log_path
            .as_deref()
            .map(parse_gate_runs_from_jsonl)
            .map(HandoffEvidence::from_runs)
            .unwrap_or_default();
        evidence.push_range = Some(diff_range);
        evidence.tree_oid = Some(actual.tree_oid.to_string());
        evidence.review_marker = review_marker;
        evidence.review_exit = review_status.code();
        evidence.suppressed_review_concern = suppressed_review_concern;
        if let Some(path) = review_log_path.filter(|p| p.exists())
            && !evidence
                .gate_log_paths
                .iter()
                .any(|candidate| candidate == &path)
        {
            evidence.gate_log_paths.push(path);
        }
        Ok(evidence)
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
            .current_dir(gate_workspace)
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
            let rollback_planned = self.pre_integration_tip.is_some();
            let rollback_state = if rollback_planned {
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
                    scope_flag: "--diff",
                    scope: diff_range.clone(),
                    exit_code: verify_code,
                    stdout: stdout_tail.clone(),
                    stderr: stderr_tail.clone(),
                    terminal_marker: parse_exit_signal(&stdout_tail)
                        .map(|m| terminal_marker_json(&m)),
                    integration_sha,
                    bead_id: bead.clone(),
                    retry_attempt: self.current_attempt,
                    rollback_state: rollback_state.to_string(),
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

        Ok(PerBeadGateOutcome::Clean)
    }

    fn emit_driver_event(&mut self, kind: DriverKind, summary: &str, payload: serde_json::Value) {
        self.emit_to_log(kind, summary, payload);
    }
}

fn bead_has_parked_state(bead: &Bead) -> bool {
    bead.status == "blocked"
        || bead
            .labels
            .iter()
            .any(|label| label.is_blocked() || label.is_clarify())
}

fn gate_log_root(logs_root: Option<&std::path::Path>, workspace: &std::path::Path) -> PathBuf {
    logs_root.map_or_else(
        || workspace.join(".loom/logs/gate"),
        |root| root.join("gate"),
    )
}

fn pre_push_hook_coverage() -> Vec<loom_gate::HookCoverage> {
    [
        ("nix-flake-check", "skip-if-missing nix -- nix flake check"),
        (
            "cargo-clippy",
            "cargo clippy --workspace --all-targets -- -D warnings",
        ),
        (
            "loom-gate-verify-diff",
            "loom gate verify --diff @{u}..HEAD",
        ),
        ("container-smoke", "skip-if-missing nix -- nix run .#test"),
    ]
    .into_iter()
    .map(|(id, entry)| loom_gate::HookCoverage {
        id: id.to_owned(),
        entry: entry.to_owned(),
    })
    .collect()
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
    use std::io::Write as _;

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
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(LoopError::Io(err)),
        };
        let log_path = path.to_string_lossy().to_string();
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
        let body = serde_json::json!({
            "kind": "post_integrate_gate",
            "argv": record.argv,
            "scope_flag": record.scope_flag,
            "scope": record.scope,
            "exit_code": record.exit_code,
            "stdout": record.stdout,
            "stderr": record.stderr,
            "terminal_marker": record.terminal_marker,
            "integration_sha": record.integration_sha,
            "bead_id": record.bead_id.to_string(),
            "retry_attempt": record.retry_attempt,
            "rollback_state": record.rollback_state,
            "failures": failures,
            "log_path": log_path,
        });
        serde_json::to_writer(&mut file, &body).map_err(std::io::Error::other)?;
        writeln!(&mut file)?;
        file.sync_all()?;
        return Ok(path);
    }
    Err(LoopError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate unique post-integrate gate log path",
    )))
}

struct PostIntegrateGateLog {
    argv: Vec<String>,
    scope_flag: &'static str,
    scope: String,
    exit_code: i32,
    stdout: String,
    stderr: String,
    terminal_marker: Option<serde_json::Value>,
    integration_sha: String,
    bead_id: BeadId,
    retry_attempt: u32,
    rollback_state: String,
    failures: Vec<VerifierFailure>,
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
            if matches!(marker, Some(ExitSignal::Complete | ExitSignal::Noop))
                && outcome.exit_code != 0
            {
                return AgentOutcome::Failure {
                    error: format!(
                        "agent emitted COMPLETE/NOOP but exited code {}",
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

    /// Return a `ScriptedBd` matching the two `bd` calls
    /// `fetch_molecule_base_commit` issues under the at-most-one-open-
    /// epic-per-spec resolution: (1) `bd list --type=epic
    /// --label=spec:<X> --status=open` returns the single epic, then
    /// (2) `bd show <mol_id>` returns the same epic with
    /// `loom.base_commit = <base>` metadata.
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
        ScriptedBd::new([
            ok_stdout(body.as_bytes()), // bd list (resolve_open_epic)
            ok_stdout(body.as_bytes()), // bd show
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

        let next = controller.next_ready_bead().await.expect("ready lookup ok");
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
    /// closure must surface as [`AgentOutcome::InfraPreflight`] so
    /// `process_one_bead` routes it straight to `loom:blocked` cause
    /// `infra-preflight`. Dual to the run-loop unit test — verifies the
    /// production controller plumbing carries the variant intact.
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
    /// driver-memory budget can absorb one occurrence per `loom loop`.
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
    /// `loom:blocked` cause `unknown-profile` without consuming a retry slot.
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

    /// Regression: `loom loop` used to hold the work-root lock for its whole
    /// lifetime, so the `loom gate review` child it spawned at the molecule-complete
    /// handoff timed out trying to acquire the same lock. `exec_review` must
    /// drop the held [`LockGuard`] before spawning, leaving the kernel-level
    /// `flock(2)` available to the child. Verified end-to-end: after a stub
    /// child exits, the lock is reacquirable on a fresh attempt.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_releases_lock_before_spawning_child() {
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

        // The child has exited and the controller's guard was dropped before
        // the spawn — the lock must be free. A short timeout keeps the test
        // fast on the regression (held-lock) path: it would error in <100ms
        // rather than wait the default 5s.
        let _reacquired = mgr
            .acquire_work_root_with_timeout_async(&root, &clock, Duration::from_millis(100))
            .await
            .expect("lock must be reacquirable after exec_review");
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
        controller.exec_review().await.expect("exec_review ok");

        let recorded = std::fs::read_to_string(&argv_log).expect("argv log readable");
        let lines: Vec<String> = recorded.lines().map(ToOwned::to_owned).collect();
        assert_eq!(
            lines,
            vec![format!(
                "{}\talpha\tgate review --diff origin/main..HEAD",
                gate_workspace.display()
            )],
            "review must run from the integration checkout over the actual origin push range; the spec label travels as env context, not as a gate filter: {recorded:?}",
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_surfaces_child_failure_detail() {
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

        let err = controller
            .exec_review()
            .await
            .expect_err("child review failure must not mint successful evidence");
        match err {
            LoopError::ReviewHandoff { detail } => {
                assert!(detail.contains("exited 17"), "missing exit code: {detail}");
                assert!(detail.contains("child stdout"), "missing stdout: {detail}");
                assert!(detail.contains("child stderr"), "missing stderr: {detail}");
            }
            other => panic!("expected ReviewHandoff, got {other:?}"),
        }
    }

    /// `specs/harness.md` § *handoff_evidence_populates_marker_and_log_path*:
    /// every `HandoffEvidence` field MUST ride out populated from the
    /// actual reviewer subprocess outputs — `review_marker` parsed from
    /// the agent's terminal stdout marker, `review_log_path` resolved
    /// from the JSONL file the child wrote. A regression that hard-codes
    /// `None` would only fail this test by surfacing as
    /// `GateFail::ReviewEvidenceMissing` downstream, so the per-field
    /// assertions here are load-bearing.
    #[tokio::test(flavor = "multi_thread")]
    async fn handoff_evidence_populates_marker_and_log_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("gamma");
        let workspace = dir.path().to_path_buf();
        let logs_root = workspace.join(".loom/logs");

        // Stub: `gate review` emits the agent's terminal marker on stdout
        // and appends to the pinned review JSONL log.
        let argv_log = dir.path().join("argv.log");
        let stub = dir.path().join("loom-stub.sh");
        let stub_body = format!(
            "#!/bin/sh\n\
             set -eu\n\
             printf '%s\\n' \"$*\" >> {argv}\n\
             case \"$1 $2\" in\n\
               'gate review')\n\
                 stamp=$(date -u -d @$(($LOOM_REVIEW_PHASE_WHEN_MILLIS / 1000)) +%Y%m%dT%H%M%SZ)\n\
                 mkdir -p {logs_root}/{label}\n\
                 log={logs_root}/{label}/review-${{stamp}}.jsonl\n\
                 printf '{{\"event\":\"review-start\"}}\\n' >> \"$log\"\n\
                 printf 'LOOM_COMPLETE\\n'\n\
                 ;;\n\
             esac\n\
             exit 0\n",
            argv = argv_log.to_string_lossy(),
            logs_root = logs_root.to_string_lossy(),
            label = label.as_str(),
        );
        std::fs::write(&stub, stub_body).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let bd = BdClient::with_runner(molecule_lookup_script(
            dir.path(),
            "gamma",
            "lm-mol.9",
            "abc12345",
        ));
        let git = git_workspace(&workspace);
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
        .with_phase_log_root(logs_root.clone());

        let handoff = controller
            .exec_review()
            .await
            .expect("exec_review must succeed with populated evidence");

        assert!(
            handoff.verified.is_some(),
            "verified scope must be parsed from gate JSONL evidence",
        );
        assert!(
            handoff.reviewed.is_some(),
            "reviewed scope must be parsed from gate JSONL evidence",
        );
        assert_eq!(
            handoff.review_marker,
            Some(ExitSignal::Complete),
            "review_marker MUST be parsed from the child's stdout — not left at None",
        );
        let log_path = handoff
            .gate_log_paths
            .first()
            .expect("gate_log_paths MUST reference the JSONL file the child wrote");
        assert!(
            log_path.starts_with(&logs_root),
            "log path lives under the per-spec logs root: {log_path:?}",
        );
        assert!(
            log_path.exists(),
            "review_log_path MUST point at the file the child actually wrote: {log_path:?}",
        );
        let body = std::fs::read_to_string(log_path).expect("log readable");
        assert!(
            body.contains("gate_run_end"),
            "log file body must carry typed gate events: {body:?}",
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

        controller.exec_review().await.expect("exec_review ok");

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

    /// `specs/harness.md`
    /// § *molecule_completion_review_threads_findings_into_previous_failure_review_concern*:
    /// when the molecule-completion review's stdout carries ≥1
    /// `LOOM_FINDING:` line and a well-formed `LOOM_CONCERN:` terminator,
    /// `exec_review` MUST parse the streamed `Vec<Finding>` via
    /// `WalkOutput::from_stdout` and stash a typed
    /// `PreviousFailure::ReviewConcern { summary, findings }` so the next
    /// `run_bead` call (the fix-up bead's first attempt — a fresh
    /// dispatch, not a retry) consumes it and the parsed findings ride
    /// through into the rendered recovery prompt verbatim. Mint does NOT
    /// fire here — push is `audit`, inspection-only per `specs/gate.md`
    /// § *Stages*.
    #[tokio::test(flavor = "multi_thread")]
    async fn molecule_completion_review_threads_findings_into_previous_failure_review_concern() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("alpha");
        let workspace = dir.path().to_path_buf();
        seed_spec(&workspace, "alpha");

        // Stub: `gate verify` is a no-op (exit 0); `gate review` emits one
        // streamed `LOOM_FINDING:` line and a well-formed `LOOM_CONCERN:`
        // terminator on stdout. The parent captures stdout via
        // `Command::output` and runs `WalkOutput::from_stdout` on it.
        let argv_log = dir.path().join("argv.log");
        let stub = dir.path().join("loom-stub.sh");
        let finding_payload = r#"{"token":"verifier-bypass","route":"deferred","bonds":["alpha"],"target":{"kind":"Annotation","target_string":"cargo test --lib sample"},"evidence":"test mocks the agent backend"}"#;
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

        let bd = BdClient::with_runner(molecule_lookup_script(
            dir.path(),
            "alpha",
            "lm-mol.1",
            "deadbeef",
        ));
        let git = git_workspace(&workspace);
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
            .apply_blocked(&bead_id, "infra-preflight", "podman load failed")
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
        let _ = controller.next_ready_bead().await.expect("ready ok");
        let captured = calls.lock().unwrap();
        assert_eq!(captured.len(), 1, "exactly one bd invocation expected");
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
        let _ = controller.next_ready_bead().await.expect("ready ok");
        let captured = calls.lock().unwrap();
        assert_eq!(captured.len(), 1, "exactly one bd invocation expected");
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
            .next_ready_bead()
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
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let stub = dir.path().join("loom-stub.sh");
        std::fs::write(
            &stub,
            "#!/usr/bin/env bash\n\
             set -euo pipefail\n\
             echo \"$*\" >> \"$PWD/argv.log\"\n\
             case \"$2\" in\n\
                 review|mint) echo \"$2 must not run in the per-bead hot path\" >&2; exit 99 ;;\n\
             esac\n\
             exit 0\n",
        )
        .expect("write stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");

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
        use std::os::unix::fs::PermissionsExt;

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
        std::fs::write(
            &stub,
            format!(
                "#!/usr/bin/env bash\n\
                 set -euo pipefail\n\
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
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");

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
        use std::os::unix::fs::PermissionsExt;
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
        assert_ne!(
            merged_tip.to_string(),
            base_tip.to_string(),
            "the ff-merge stand-in must advance the integration branch",
        );

        let stub = dir.path().join("loom-stub.sh");
        std::fs::write(
            &stub,
            "#!/usr/bin/env bash\n\
             set -euo pipefail\n\
             case \"$2\" in\n\
                 verify) echo 'verifier failed: cargo test' >&2; exit 1 ;;\n\
                 review|mint) echo \"$2 must not run after verify-fail\" ; exit 99 ;;\n\
             esac\n\
             exit 0\n",
        )
        .expect("write stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");

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
                let gate_log_json: serde_json::Value = serde_json::from_str(
                    gate_log
                        .lines()
                        .next()
                        .expect("gate log contains one JSONL event"),
                )
                .expect("gate log json");
                assert_eq!(gate_log_json["kind"], "post_integrate_gate");
                assert_eq!(
                    gate_log_json["argv"],
                    serde_json::json!(["gate", "verify", "--diff", format!("{}..HEAD", base_tip)])
                );
                assert_eq!(gate_log_json["scope_flag"], "--diff");
                assert_eq!(gate_log_json["exit_code"], 1);
                assert_eq!(gate_log_json["integration_sha"], merged_tip.to_string());
                assert_eq!(gate_log_json["rollback_state"], "pending");
                assert_eq!(gate_log_json["failures"][0]["exit_code"], 1);
                assert_eq!(
                    gate_log_json["failures"][0]["target"],
                    serde_json::json!(format!("loom gate verify --diff {}..HEAD", base_tip))
                );
                assert_eq!(
                    gate_log_json["log_path"],
                    serde_json::json!(gate_log_path.to_string_lossy())
                );
                assert!(
                    gate_log_json["stderr"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("cargo test")
                );
            }
            other => panic!("expected stashed PostIntegrateFail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn gate_invocations_write_separate_jsonl_logs_with_parent_breadcrumb() {
        use std::os::unix::fs::PermissionsExt;

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
        std::fs::write(
            &stub,
            "#!/usr/bin/env bash\n\
             set -euo pipefail\n\
             case \"$2\" in\n\
                 verify) echo 'verify stdout'; echo 'verify stderr' >&2; exit 2 ;;\n\
             esac\n\
             exit 0\n",
        )
        .expect("write stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");

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
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let loom_ws = git.loom_workspace();
        let root_tip = loom_driver::git::sync_head_commit_sha(&loom_ws)
            .expect("root sha")
            .to_string();

        let stub = dir.path().join("loom-stub.sh");
        std::fs::write(
            &stub,
            "#!/usr/bin/env bash\n\
             set -euo pipefail\n\
             case \"$2\" in\n\
                 verify) echo 'verifier failed' >&2; exit 1 ;;\n\
             esac\n\
             exit 0\n",
        )
        .expect("write stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");

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
