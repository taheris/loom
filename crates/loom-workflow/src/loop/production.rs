//! Production [`AgentLoopController`] used by the `loom loop` binary.
//!
//! Wires `BdClient` for bead lookup/close/clarify, a `tokio::process::Command`
//! shell-out for `exec_review`, and a caller-provided dispatch closure for the
//! actual agent invocation. The closure pattern keeps backend selection
//! (`PiBackend` vs `ClaudeBackend`) inside the binary's `dispatch` match —
//! `loom-workflow` never sees the concrete backend types, mirroring the shape
//! used by `ProductionTodoController` and `run_parallel_batch`.
//!
//! Per-bead profile dispatch is wired through [`build_spawn_config_from_manifest`]:
//! the manifest, CLI `--profile` override, and per-phase fallback all flow
//! into the controller at construction time so `run_bead` resolves the
//! per-bead `image_ref` + `image_source` against the parsed manifest before
//! the agent invocation. A missing manifest entry surfaces as
//! [`LoopError::Profile`] — no silent fallback.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use loom_driver::agent::{ProtocolError, SpawnConfig};
use loom_driver::bd::{
    BdClient, Bead, CommandRunner, ListOpts, ReadyOpts, TokioRunner, UpdateOpts,
};
use loom_driver::clock::{Clock, SystemClock};
use loom_driver::config::{LoomTopConfig, Phase};
use loom_driver::git::{CreatedWorktree, GitClient, GitOid, RebaseOutcome};
use loom_driver::identifier::{BeadId, ProfileName, SpecLabel};
use loom_driver::lock::LockGuard;
use loom_driver::logging::phase_log_path;
use loom_driver::profile_manifest::{ProfileError, ProfileImageManifest};
use loom_driver::scratch::resolve_scratch_key;
use loom_events::DriverKind;
use tokio::process::Command;
use tracing::{info, warn};

use loom_gate::HandoffEvidence;

use super::context::{LoopContextInputs, render_loop_prompt};
use super::driver_emit::BeadEmit;
use super::error::LoopError;
use super::outcome::{AgentOutcome, SessionResult};
use super::runner::{AgentLoopController, PerBeadGateOutcome};
use super::spawn::{build_spawn_config_from_manifest, dolt_socket_mount, sccache_mount};
use super::tree_clean::dirty_paths_from_porcelain;
use super::verify::{VerifyPass, verify_pass};
use crate::review::{
    AcceptAllFindingValidator, DispatchScope, GateInputs, PhaseVerdict, RecoveryCause, WalkOutput,
    decide,
};
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
    spawn: S,
    /// Spec lock dropped before exec'ing `loom review` so the child can take it.
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
            spawn,
            lock: None,
            style_rules: "docs/style-rules.md".to_string(),
            stashed_previous_failure: None,
            stashed_review_concern: None,
            logs_root: None,
            current_emit: None,
            loom_cfg: LoomTopConfig::default(),
            pre_integration_tip: None,
        }
    }

    /// Snapshot the `[loom]` config block onto the controller so the
    /// per-bead dispatch picks up the shared sccache mount + env when
    /// [`LoomTopConfig::sccache_dir`] is set. Defaults to
    /// `LoomTopConfig::default()` when unset, which emits no mount.
    pub fn with_loom_config(mut self, cfg: LoomTopConfig) -> Self {
        self.loom_cfg = cfg;
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

    /// Hand the spec lock to the controller so `exec_review` can drop it
    /// before spawning the `loom review` child (which acquires the same lock).
    pub fn with_handoff_lock(mut self, guard: LockGuard) -> Self {
        self.lock = Some(guard);
        self
    }

    /// Override the style-rules pin used in the rendered run prompt.
    /// Production callers thread this from `LoomConfig.style_rules`; tests
    /// rely on the built-in default.
    pub fn with_style_rules(mut self, path: String) -> Self {
        self.style_rules = path;
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
}

impl<S, F, R: CommandRunner> AgentLoopController for ProductionAgentLoopController<S, F, R>
where
    S: Fn(SpawnConfig, BeadId) -> F + Send,
    F: std::future::Future<Output = (SessionResult, Option<ExitSignal>)> + Send,
{
    async fn next_ready_bead(&mut self) -> Result<Option<Bead>, LoopError> {
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
                label: Some(self.spec_label_filter()),
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

        let key = resolve_scratch_key(Phase::Run, &self.label, Some(&bead.id));
        let scratchpad_path =
            loom_driver::scratch::ScratchSession::scratchpad_path_for(&worktree.path, &key)
                .to_string_lossy()
                .into_owned();
        let attempt = u32::from(is_retry);
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
            scratchpad_path,
            style_rules: self.style_rules.clone(),
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
        if let Some(spec) = sccache_mount(&self.loom_cfg) {
            mounts.push(spec);
        }
        let extra_env = self.loom_cfg.container_sccache_env();
        // Host key paths handed to the `wrix spawn` launcher so it mounts
        // the deploy + signing keys into the bead container; the agent boots
        // without git keys otherwise (`specs/harness.md` § Commit signing).
        let launcher_env = self.git.launcher_key_env()?;
        let spawn_config = match build_spawn_config_from_manifest(
            &self.manifest,
            bead,
            self.cli_profile.as_ref(),
            &self.phase_default,
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
            Err(e) => {
                drop(scratch);
                return Err(LoopError::Profile(e));
            }
        };
        info!(
            bead = %bead.id,
            image_ref = %spawn_config.image_ref,
            worktree = %worktree.path.display(),
            retry = is_retry,
            "loom loop: dispatching agent",
        );
        let (session, marker) = (self.spawn)(spawn_config, bead.id.clone()).await;
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
                    // Capture the pre-merge integration tip so the per-bead
                    // audit can tell whether this bead actually advanced the
                    // line; a no-op ff (bead committed nothing) leaves the
                    // rollback target unset so `exec_per_bead_gate` does not
                    // unwind a prior bead's commit.
                    let pre_tip = self.git.integration_commit_sha().await?;
                    self.git.ff_merge_integration(&worktree.branch).await?;
                    let main_sha = self.git.integration_commit_sha().await?;
                    self.pre_integration_tip = (main_sha != pre_tip).then_some(pre_tip);
                    self.emit_to_log(
                        DriverKind::MergeOk,
                        &format!("merge ok: {} → main", worktree.branch),
                        serde_json::json!({
                            "bead_id": bead.id.to_string(),
                            "branch": worktree.branch,
                            "main_sha": main_sha.to_string(),
                        }),
                    );
                    // Per-bead integration never pushes; origin is reached
                    // once per molecule, post-audit, by the push gate
                    // (specs/harness.md § Verdict Gate, phase 5).
                    self.git.remove_worktree(&worktree.path).await?;
                    self.git.delete_branch(&worktree.branch).await?;
                    self.emit_to_log(
                        DriverKind::WorktreeCleanupOk,
                        &format!("worktree + branch cleanup ok for bead {}", bead.id),
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
        // `clarify-without-options` so `loom msg`'s queue is not handed
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
        // Release the spec lock before spawning the child — `loom gate
        // verify` and `loom gate review` acquire the same lock and would
        // otherwise time out behind us.
        self.lock.take();
        // Molecule-completion handoff (FR1 / FR9): scope the verify and
        // review children to the molecule's own diff
        // (`<molecule.base_commit>..HEAD`) so push-gate cost is
        // proportional to the molecule's work rather than `--tree`.
        // Deterministic verify first then LLM review; non-zero exit
        // codes are NOT fatal to `run_loop` (they drive fix-up beads on
        // the next outer-loop pass), but spawn failures and missing
        // molecule metadata DO surface as `LoopError`.
        let base = fetch_molecule_base_commit(&self.bd, &self.workspace, &self.label).await?;
        let diff_range = format!("{base}..HEAD");
        let verify_status = Command::new(&self.loom_bin)
            .current_dir(&self.workspace)
            .arg("gate")
            .arg("verify")
            .arg("--diff")
            .arg(&diff_range)
            .arg("-s")
            .arg(self.label.as_str())
            .status()
            .await?;
        info!(
            spec = %self.label.as_str(),
            diff = %diff_range,
            exit_code = verify_status.code().unwrap_or(-1),
            "loom loop: molecule handoff — loom gate verify --diff finished",
        );
        // Thread the verify exit into the child via `--verify-exit <CODE>`
        // so the push gate's four-condition AND (FR9 condition 2) consumes
        // it. Signal-terminated children surface `None`; the spec treats no
        // exit code as "no clean success" — use a non-zero sentinel so the
        // gate routes through `verifier-failed` rather than skipping the
        // condition.
        let verify_exit_arg = verify_status.code().unwrap_or(1);
        // Pin `phase_when` and pass it to the child so both sides
        // resolve the same JSONL log path under
        // `<logs_root>/<label>/review-<utc>.jsonl`. The child opts in to
        // emitting the agent's combined stdout to its own stdout via
        // `LOOM_REVIEW_EMIT_STDOUT` so the parent can `parse_exit_signal`
        // the captured output.
        let phase_when = SystemClock::new().wall_now();
        let phase_when_millis = phase_when
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let review_output = Command::new(&self.loom_bin)
            .current_dir(&self.workspace)
            .env(REVIEW_PHASE_WHEN_ENV, phase_when_millis.to_string())
            .env(REVIEW_EMIT_STDOUT_ENV, "1")
            .arg("gate")
            .arg("review")
            .arg("--diff")
            .arg(&diff_range)
            .arg("-s")
            .arg(self.label.as_str())
            .arg("--verify-exit")
            .arg(verify_exit_arg.to_string())
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
        let review_marker = parse_exit_signal(&review_stdout);
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
        let walk = WalkOutput::from_stdout(
            &review_stdout,
            DispatchScope::PerBead,
            &AcceptAllFindingValidator,
        );
        if let TerminalSurface::Concern { summary } = walk.terminal()
            && !walk.findings().is_empty()
        {
            self.stashed_review_concern = Some(PreviousFailure::ReviewConcern {
                summary: summary.clone(),
                findings: walk.findings().to_vec(),
            });
        }
        let review_log_path = self
            .logs_root
            .as_deref()
            .map(|root| phase_log_path(root, &self.label, "review", phase_when))
            .filter(|p| p.exists());
        Ok(HandoffEvidence {
            verify_exit: verify_status.code(),
            review_exit: review_status.code(),
            review_marker,
            review_log_path,
        })
    }

    async fn exec_per_bead_gate(&mut self, bead: &BeadId) -> Result<PerBeadGateOutcome, LoopError> {
        // Per-diff stage (`specs/gate.md` § *Per-diff stage checks*): the
        // run-phase agent's `LOOM_COMPLETE` is *necessary but not
        // sufficient* — `loom gate verify --bead <id>` runs deterministic
        // audits, then `loom gate mint --bead <id>` walks the LLM rubric
        // and mints typed fix-ups. Routing per Decision 7 collapses two
        // recovery paths (verify failure, mint errors) into one
        // `PerBeadGateOutcome::Recovery` variant; `refused > 0` is the
        // only path that surfaces as a structural-violation block.
        let verify_output = Command::new(&self.loom_bin)
            .current_dir(&self.workspace)
            .arg("gate")
            .arg("verify")
            .arg("--bead")
            .arg(bead.as_str())
            .arg("-s")
            .arg(self.label.as_str())
            .output()
            .await?;
        let verify_exit = verify_output.status.code().unwrap_or(1);
        info!(
            bead = %bead,
            spec = %self.label.as_str(),
            exit_code = verify_exit,
            "loom loop: per-bead gate — loom gate verify --bead finished",
        );
        if verify_exit != 0 {
            let stderr_tail = String::from_utf8_lossy(&verify_output.stderr).to_string();
            let stdout_tail = String::from_utf8_lossy(&verify_output.stdout).to_string();
            let detail = format!(
                "loom gate verify --bead {bead} exited {verify_exit}\n\
                 stdout:\n{stdout_tail}\nstderr:\n{stderr_tail}",
            );
            // Audit-fail rolls the ff-merge back one commit and threads the
            // failing verifier into the next dispatch (specs/harness.md
            // § Verdict Gate — post-integrate-fail). The rollback only fires
            // when this bead's ff actually advanced the integration tip — a
            // bead that committed nothing leaves nothing to unwind.
            let rolled_back = self.pre_integration_tip.take().is_some();
            if rolled_back {
                self.git.rollback_integration().await?;
            }
            self.stashed_previous_failure = Some(PreviousFailure::PostIntegrateFail {
                failures: vec![VerifierFailure::new(
                    format!("loom gate verify --bead {bead}"),
                    verify_exit,
                    format!("{stdout_tail}\n{stderr_tail}"),
                )],
            });
            self.emit_to_log(
                DriverKind::VerdictGate,
                &format!("post-integrate-fail: integration audit failed for bead {bead}"),
                serde_json::json!({
                    "bead_id": bead.to_string(),
                    "cause": "post-integrate-fail",
                    "verify_exit": verify_exit,
                    "rolled_back": rolled_back,
                }),
            );
            return Ok(PerBeadGateOutcome::Recovery { detail });
        }

        let mint_output = Command::new(&self.loom_bin)
            .current_dir(&self.workspace)
            .arg("gate")
            .arg("mint")
            .arg("--bead")
            .arg(bead.as_str())
            .arg("-s")
            .arg(self.label.as_str())
            .output()
            .await?;
        let mint_exit = mint_output.status.code().unwrap_or(1);
        let mint_stdout = String::from_utf8_lossy(&mint_output.stdout).to_string();
        info!(
            bead = %bead,
            spec = %self.label.as_str(),
            exit_code = mint_exit,
            "loom loop: per-bead gate — loom gate mint --bead finished",
        );
        Ok(classify_mint_summary(mint_exit, &mint_stdout))
    }

    fn emit_driver_event(&mut self, kind: DriverKind, summary: &str, payload: serde_json::Value) {
        self.emit_to_log(kind, summary, payload);
    }
}

/// Parse the mint summary stdout printed by `run_gate_mint` and translate
/// the `refused` / `errors` counts into a [`PerBeadGateOutcome`] per
/// `specs/gate.md` Decision 7. Header shape:
/// `minted M, skipped K (dedup), refused R, errors E` (see
/// `MintSummary::render`). The conflicting `bd` ids surface in the
/// per-finding `refused {fingerprint}: {reason}` lines and ride out in
/// the `StructuralViolation::detail` field; the error detail similarly
/// rides out as `Recovery::detail` so the next agent attempt sees it as
/// `previous_failure`.
fn classify_mint_summary(exit_code: i32, stdout: &str) -> PerBeadGateOutcome {
    let (refused, errors) = parse_mint_counts(stdout);
    if refused > 0 {
        return PerBeadGateOutcome::StructuralViolation {
            detail: extract_lines_with_prefix(stdout, "refused ").unwrap_or_else(|| stdout.into()),
        };
    }
    if errors > 0 || exit_code != 0 {
        return PerBeadGateOutcome::Recovery {
            detail: extract_lines_with_prefix(stdout, "error ").unwrap_or_else(|| stdout.into()),
        };
    }
    PerBeadGateOutcome::Clean
}

/// Extract `refused` and `errors` counts from the mint summary header
/// line. Tolerant: a header that does not parse returns `(0, 0)`; the
/// caller falls back on the exit code to decide between Clean and
/// Recovery.
fn parse_mint_counts(stdout: &str) -> (usize, usize) {
    let header = stdout.lines().next().unwrap_or("");
    let mut refused = 0usize;
    let mut errors = 0usize;
    for part in header.split(',') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("refused ") {
            refused = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        } else if let Some(rest) = part.strip_prefix("errors ") {
            errors = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        }
    }
    (refused, errors)
}

/// Collect summary body lines whose first non-whitespace token starts
/// with `prefix` (e.g. `"refused "` or `"error "`) into one
/// newline-joined block — the conflicting `bd` ids / error reasons
/// surface verbatim for `bd update --notes` and `previous_failure`.
/// Returns `None` if no matching line is present so the caller can fall
/// back on the full summary.
fn extract_lines_with_prefix(stdout: &str, prefix: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for line in stdout.lines() {
        if line.trim_start().starts_with(prefix) {
            out.push(line.trim_start());
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join("\n"))
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

/// Resolve the spec's open epic via `bd find` and return its
/// `loom.base_commit` metadata. Used by `exec_review` to scope the
/// molecule-completion handoff to the molecule's own diff rather than
/// `--tree`. Delegates metadata resolution to
/// [`crate::init::resolve_base_commit`] so the run-phase and rebuild-phase
/// resolutions share parent inheritance + write-back behaviour verbatim.
async fn fetch_molecule_base_commit<R: CommandRunner>(
    bd: &BdClient<R>,
    _workspace: &std::path::Path,
    label: &SpecLabel,
) -> Result<String, LoopError> {
    let mol_id = crate::resolve::resolve_open_epic(bd, label)
        .await?
        .ok_or_else(|| LoopError::NoActiveMolecule {
            label: label.to_string(),
        })?;
    let bead_id =
        BeadId::new(mol_id.as_str()).map_err(loom_driver::bd::BdError::CreateInvalidId)?;
    let detail = bd.show(&bead_id).await?;
    crate::init::resolve_base_commit(bd, &detail)
        .await
        .map_err(|e| {
            use crate::init::InitError;
            match e {
                InitError::Bd(e) => LoopError::Bd(e),
                InitError::MoleculeMissingBaseCommit { id } => {
                    LoopError::MoleculeMissingBaseCommit { id }
                }
                InitError::MoleculeMissingBaseCommitNoParentMetadata { id, parent } => {
                    LoopError::MoleculeMissingBaseCommitNoParentMetadata { id, parent }
                }
                other => LoopError::Bug {
                    context: format!(
                        "resolve_base_commit emits only Bd / MoleculeMissingBaseCommit / \
                         MoleculeMissingBaseCommitNoParentMetadata; got {other:?}",
                    ),
                },
            }
        })
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
    use std::time::Duration;

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

    /// `ScriptedBd` returning an empty list for `resolve_open_epic` —
    /// the spec has no open epic so the caller surfaces
    /// [`LoopError::NoActiveMolecule`].
    fn empty_open_epic_script() -> ScriptedBd {
        ScriptedBd::new([ok_stdout(b"[]")])
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
          "base": { "ref": "localhost/wrix-base:abc", "source": "/nix/store/aaa-image-base" }
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
        assert_eq!(cfg.image_ref, "localhost/wrix-base:abc");
        assert!(cfg.initial_prompt.contains("lm-1"));
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

    /// Regression: `loom loop` used to hold the spec lock for its whole
    /// lifetime, so the `loom review` child it spawned at the molecule-complete
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
        let clock = SystemClock::new();
        let guard = mgr
            .acquire_spec_async(&label, &clock)
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
            .acquire_spec_with_timeout_async(&label, &clock, Duration::from_millis(100))
            .await
            .expect("lock must be reacquirable after exec_review");
    }

    /// FR1: the molecule-completion handoff invokes `loom gate verify
    /// --diff <molecule.base_commit>..HEAD` THEN `loom gate review --diff
    /// <molecule.base_commit>..HEAD` — both scoped to the molecule's
    /// own diff (proportional to the molecule's work, not `--tree`), in
    /// that order, and both with the spec label threaded through `-s`.
    /// The stub script records each invocation so the test can assert
    /// on the exact argv sequence the production controller emits.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_invokes_gate_verify_then_gate_review_with_molecule_diff() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("alpha");

        // Recording stub: appends every invocation's argv (one per line,
        // tab-separated) to argv.log so the test can replay the call order.
        let argv_log = dir.path().join("argv.log");
        let stub = dir.path().join("loom-stub.sh");
        let stub_body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\nexit 0\n",
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

        controller.exec_review().await.expect("exec_review ok");

        let recorded = std::fs::read_to_string(&argv_log).expect("argv log readable");
        let lines: Vec<&str> = recorded.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "exec_review must spawn exactly two children (gate verify then gate review): {recorded:?}",
        );
        assert_eq!(
            lines[0], "gate verify --diff deadbeef..HEAD -s alpha",
            "first child must be `loom gate verify --diff <base>..HEAD -s <label>`",
        );
        assert_eq!(
            lines[1], "gate review --diff deadbeef..HEAD -s alpha --verify-exit 0",
            "second child must be `loom gate review --diff <base>..HEAD -s <label> --verify-exit <code>` \
             (FR9 condition 2: push gate consumes verify exit, not the default None)",
        );
    }

    /// FR1: non-zero exit from `loom gate verify` MUST NOT abort the
    /// handoff — it signals concerns that the outer loop drives toward
    /// via fix-up beads on the next pass. The production controller
    /// still spawns `loom gate review --diff <base>..HEAD` after verify
    /// fails, and `exec_review` returns `Ok` so `run_loop` can re-poll
    /// `bd ready` rather than tearing down the whole `loom loop`.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_continues_to_review_when_verify_exits_nonzero() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("beta");

        // Stub: `gate verify` exits 1 (concerns), every other invocation
        // exits 0. The first two argv tokens (`gate verify`) select the
        // branch.
        let argv_log = dir.path().join("argv.log");
        let stub = dir.path().join("loom-stub.sh");
        let stub_body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
             case \"$1 $2\" in\n  'gate verify') exit 1 ;;\n  *) exit 0 ;;\nesac\n",
            log = argv_log.to_string_lossy(),
        );
        std::fs::write(&stub, stub_body).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let bd = BdClient::with_runner(molecule_lookup_script(
            dir.path(),
            "beta",
            "lm-mol.7",
            "cafef00d",
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

        let handoff = controller
            .exec_review()
            .await
            .expect("non-zero verify exit must not produce LoopError");

        let recorded = std::fs::read_to_string(&argv_log).expect("argv log readable");
        let lines: Vec<&str> = recorded.lines().collect();
        assert_eq!(
            lines,
            vec![
                "gate verify --diff cafef00d..HEAD -s beta",
                "gate review --diff cafef00d..HEAD -s beta --verify-exit 1"
            ],
            "review must still run even when verify signals concerns — and the \
             verify exit code rides through to the child's push gate via \
             `--verify-exit` per FR9 condition 2",
        );

        // FR9 four-condition AND wiring: the verify exit must ride out
        // through `HandoffEvidence` so the push-gate verdict can refuse
        // the push on `Some(n)` with `n != 0`.
        assert_eq!(
            handoff.verify_exit,
            Some(1),
            "verify child exit code threaded through HandoffEvidence",
        );
        assert_eq!(
            handoff.review_exit,
            Some(0),
            "review child exit code threaded through HandoffEvidence",
        );
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

        // Stub: `gate verify` is a no-op; `gate review` emits the
        // agent's terminal marker on stdout and lands a non-empty JSONL
        // log under `<logs_root>/<label>/review-<utc>.jsonl` keyed to
        // the parent-pinned `LOOM_REVIEW_PHASE_WHEN_MILLIS`. The stub
        // mirrors the production child's contract (env var threaded
        // timestamp + emit-stdout signalling) so the test exercises the
        // real two-process resolution path.
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

        assert_eq!(
            handoff.verify_exit,
            Some(0),
            "verify exit threaded through HandoffEvidence",
        );
        assert_eq!(
            handoff.review_exit,
            Some(0),
            "review exit threaded through HandoffEvidence",
        );
        assert_eq!(
            handoff.review_marker,
            Some(ExitSignal::Complete),
            "review_marker MUST be parsed from the child's stdout — not left at None",
        );
        let log_path = handoff
            .review_log_path
            .as_ref()
            .expect("review_log_path MUST be populated from the JSONL file the child wrote");
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
            body.contains("review-start"),
            "log file body must be non-empty: {body:?}",
        );
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

        // Stub: `gate verify` is a no-op (exit 0); `gate review` emits one
        // streamed `LOOM_FINDING:` line and a well-formed `LOOM_CONCERN:`
        // terminator on stdout. The parent captures stdout via
        // `Command::output` and runs `WalkOutput::from_stdout` on it.
        let argv_log = dir.path().join("argv.log");
        let stub = dir.path().join("loom-stub.sh");
        let finding_payload = r#"{"token":"verifier-bypass","bonds":["alpha"],"target":{"kind":"Annotation","target_string":"cargo test --lib sample"},"evidence":"test mocks the agent backend"}"#;
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
    /// misconfigured, not "scope unknown, push the whole tree".
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_errors_when_no_active_molecule_for_spec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        // bd find returns an empty list → no open epic → NoActiveMolecule.
        let bd = BdClient::with_runner(empty_open_epic_script());
        let git = git_workspace(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            bd,
            SpecLabel::new("orphan-spec"),
            PathBuf::from("/nonexistent/loom"),
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
            .expect_err("exec_review must error when no active molecule");
        match err {
            LoopError::NoActiveMolecule { label } => assert_eq!(label, "orphan-spec"),
            other => panic!("expected NoActiveMolecule, got {other:?}"),
        }
    }

    /// FR1 negative: when `bd find` returns an open epic whose bead
    /// lacks `loom.base_commit` metadata and no parent to inherit from,
    /// `exec_review` MUST surface `MoleculeMissingBaseCommit` rather than
    /// fabricate a diff range. The metadata key is set unconditionally on
    /// every molecule `loom todo` creates; the absence is a bd corruption
    /// signal worth surfacing loudly.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_errors_when_molecule_missing_base_commit_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let body = br#"[{
            "id": "lm-mol.99",
            "title": "gamma: pending decomposition",
            "status": "open",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["spec:gamma"],
            "metadata": {}
        }]"#;
        let bd = BdClient::with_runner(ScriptedBd::new([ok_stdout(body), ok_stdout(body)]));
        let git = git_workspace(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            bd,
            SpecLabel::new("gamma"),
            PathBuf::from("/nonexistent/loom"),
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
            .expect_err("exec_review must error when molecule lacks base_commit");
        match err {
            LoopError::MoleculeMissingBaseCommit { id } => assert_eq!(id, "lm-mol.99"),
            other => panic!("expected MoleculeMissingBaseCommit, got {other:?}"),
        }
    }

    /// An epic returned by `bd find` may lack its own `loom.base_commit`
    /// metadata if it was created out-of-band. `fetch_molecule_base_commit`
    /// MUST mirror `init::fetch_active_molecules`'s self-heal: read the
    /// parent's `loom.base_commit`, persist it on the epic via
    /// `bd update --set-metadata`, and continue the molecule-completion
    /// handoff using the inherited value.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_inherits_base_commit_from_parent_when_child_lacks_metadata() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("delta");

        let argv_log = dir.path().join("argv.log");
        let stub = dir.path().join("loom-stub.sh");
        let stub_body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\nexit 0\n",
            log = argv_log.to_string_lossy(),
        );
        std::fs::write(&stub, stub_body).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let epic_show = br#"[{
            "id": "lm-child.7",
            "title": "delta follow-up",
            "status": "open",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["spec:delta"],
            "parent": "lm-epicd",
            "metadata": {}
        }]"#;
        let parent_show = br#"[{
            "id": "lm-epicd",
            "title": "delta: pending decomposition",
            "status": "open",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["spec:delta"],
            "metadata": {"loom.base_commit": "feed0042"}
        }]"#;
        let bd = BdClient::with_runner(ScriptedBd::new([
            ok_stdout(epic_show),   // bd list (resolve_open_epic)
            ok_stdout(epic_show),   // bd show (fetch detail)
            ok_stdout(parent_show), // bd show parent
            ok_stdout(b""),         // bd update --set-metadata
        ]));
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

        controller
            .exec_review()
            .await
            .expect("exec_review must succeed when base_commit is inheritable from parent");

        let recorded = std::fs::read_to_string(&argv_log).expect("argv log readable");
        let lines: Vec<&str> = recorded.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "exec_review must still spawn verify + review after inheritance: {recorded:?}",
        );
        assert_eq!(
            lines[0], "gate verify --diff feed0042..HEAD -s delta",
            "verify child must use the inherited base_commit",
        );
        assert_eq!(
            lines[1], "gate review --diff feed0042..HEAD -s delta --verify-exit 0",
            "review child must use the inherited base_commit",
        );
    }

    /// When neither the child nor its parent carries `loom.base_commit`,
    /// `fetch_molecule_base_commit` surfaces the distinct
    /// `MoleculeMissingBaseCommitNoParentMetadata` variant so the error
    /// text can name the parent — the operator's first repair hop is to
    /// fix the epic, not the child.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_errors_when_parent_also_lacks_base_commit_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let epic_show = br#"[{
            "id": "lm-child.8",
            "title": "epsilon follow-up",
            "status": "open",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["spec:epsilon"],
            "parent": "lm-epice",
            "metadata": {}
        }]"#;
        let parent_show = br#"[{
            "id": "lm-epice",
            "title": "epsilon: pending decomposition",
            "status": "open",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["spec:epsilon"]
        }]"#;
        let bd = BdClient::with_runner(ScriptedBd::new([
            ok_stdout(epic_show),   // bd list (resolve_open_epic)
            ok_stdout(epic_show),   // bd show
            ok_stdout(parent_show), // bd show parent
        ]));
        let git = git_workspace(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            bd,
            SpecLabel::new("epsilon"),
            PathBuf::from("/nonexistent/loom"),
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
            .expect_err("exec_review must error when both child and parent lack base_commit");
        let msg = err.to_string();
        assert!(
            msg.contains("bd update lm-child.8 --set-metadata loom.base_commit="),
            "error must surface the fix command: {msg}",
        );
        match err {
            LoopError::MoleculeMissingBaseCommitNoParentMetadata { id, parent } => {
                assert_eq!(id, "lm-child.8");
                assert_eq!(parent, "lm-epice");
            }
            other => panic!("expected MoleculeMissingBaseCommitNoParentMetadata, got {other:?}"),
        }
    }

    /// specs/gate.md § "Persistence boundary: agent narrates, agent persists":
    /// when the bead under dispatch carries a well-formed `## Options — …`
    /// block, the runner's `apply_clarify` only stamps the `loom:clarify`
    /// label — the canonical block belongs to the agent, written to bead
    /// state *before* `LOOM_CLARIFY` is emitted. If the runner also wrote
    /// the agent's stdout reason-line via `bd update --notes`, every
    /// re-emit would clobber the canonical block and leave `loom msg`'s
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

    /// Mint summary parser routes a clean header (`refused 0, errors
    /// 0`) to [`PerBeadGateOutcome::Clean`] regardless of `minted` /
    /// `skipped` counts, per `specs/gate.md` § *Per-bead mint summary
    /// semantics*.
    #[test]
    fn classify_mint_summary_clean_header_routes_to_clean() {
        let stdout = "minted 2, skipped 1 (dedup), refused 0, errors 0\n\
                      minted abcd1234 → lm-fix.7 (spec:gate)\n\
                      skipped efgh5678 (existing lm-fix.4)\n";
        assert_eq!(classify_mint_summary(0, stdout), PerBeadGateOutcome::Clean,);
    }

    /// Mint summary parser routes `refused > 0` to
    /// [`PerBeadGateOutcome::StructuralViolation`] with the conflicting
    /// `bd` ids surfaced verbatim from the summary's refused lines —
    /// per Decision 7, this is the structural-violation path that
    /// surfaces as a labelled bead the operator unblocks.
    #[test]
    fn classify_mint_summary_refused_routes_to_structural_violation_with_conflicting_ids() {
        let stdout = "minted 0, skipped 0 (dedup), refused 1, errors 0\n\
                      refused fp-aabb: more than one open epic for spec `gate` — \
                      close all but one before re-running (ids: lm-mol.4, lm-mol.7)\n";
        match classify_mint_summary(1, stdout) {
            PerBeadGateOutcome::StructuralViolation { detail } => {
                assert!(
                    detail.contains("lm-mol.4") && detail.contains("lm-mol.7"),
                    "detail must carry conflicting bd ids: {detail:?}",
                );
            }
            other => panic!("expected StructuralViolation, got {other:?}"),
        }
    }

    /// Mint summary parser routes `errors > 0` to
    /// [`PerBeadGateOutcome::Recovery`] so the runner threads the error
    /// detail into `previous_failure` and re-runs the agent through the
    /// existing per-bead recovery loop.
    #[test]
    fn classify_mint_summary_errors_routes_to_recovery_with_error_detail() {
        let stdout = "minted 0, skipped 0 (dedup), refused 0, errors 1\n\
                      error fp-ccdd: bd create exited 2 (dolt socket timeout)\n";
        match classify_mint_summary(1, stdout) {
            PerBeadGateOutcome::Recovery { detail } => {
                assert!(
                    detail.contains("fp-ccdd"),
                    "detail must carry the error fingerprint: {detail:?}",
                );
            }
            other => panic!("expected Recovery, got {other:?}"),
        }
    }

    /// `refused > 0` AND `errors > 0` simultaneously routes to
    /// [`PerBeadGateOutcome::StructuralViolation`] — structural
    /// violations take precedence over recoverable errors because the
    /// agent cannot fix duplicate-epic conflicts from inside the loop.
    #[test]
    fn classify_mint_summary_refused_wins_over_errors() {
        let stdout = "minted 0, skipped 0 (dedup), refused 1, errors 1\n\
                      refused fp-aabb: duplicate mint label (ids: lm-x, lm-y)\n\
                      error fp-ccdd: transient bd failure\n";
        assert!(matches!(
            classify_mint_summary(1, stdout),
            PerBeadGateOutcome::StructuralViolation { .. },
        ));
    }

    /// Spec criterion (`specs/gate.md` § *Production walker wiring*):
    /// the production `exec_per_bead_gate` invokes `loom gate verify
    /// --bead <id> -s <spec>` then `loom gate mint --bead <id> -s
    /// <spec>` as real subprocesses against `loom_bin`. The
    /// mock-controller test in `runner.rs` covers the runner-side
    /// routing on `PerBeadGateOutcome`; this pins the subprocess
    /// shape the mock bypasses by pointing `loom_bin` at a stub
    /// that records each invocation's argv and emits a clean mint
    /// summary so the real `classify_mint_summary` parser
    /// roundtrips to `Clean`.
    #[tokio::test]
    async fn exec_per_bead_gate_invokes_loom_gate_verify_then_mint_subprocesses() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let manifest = write_manifest(dir.path());
        let stub = dir.path().join("loom-stub.sh");
        std::fs::write(
            &stub,
            "#!/bin/sh\n\
             set -euo pipefail\n\
             echo \"$*\" >> \"$PWD/argv.log\"\n\
             case \"$2\" in\n\
                 mint) echo 'minted 0, skipped 0 (dedup), refused 0, errors 0' ;;\n\
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
        let outcome = controller
            .exec_per_bead_gate(&bead_id)
            .await
            .expect("exec_per_bead_gate ok");
        assert_eq!(outcome, PerBeadGateOutcome::Clean);

        let log = std::fs::read_to_string(workspace.join("argv.log")).expect("argv.log readable");
        let calls: Vec<&str> = log.lines().collect();
        assert_eq!(
            calls.len(),
            2,
            "exec_per_bead_gate must spawn exactly two subprocesses: {calls:?}",
        );
        assert_eq!(
            calls[0], "gate verify --bead lm-1 -s gate",
            "first subprocess argv must be `loom gate verify --bead <id> -s <spec>`",
        );
        assert_eq!(
            calls[1], "gate mint --bead lm-1 -s gate",
            "second subprocess argv must be `loom gate mint --bead <id> -s <spec>` after verify",
        );
    }

    /// A non-zero `loom gate verify --bead` against the integrated tree is
    /// the `post-integrate-fail` audit failure: the integration is rolled
    /// back (`git reset --hard HEAD~1`), `PreviousFailure::PostIntegrateFail`
    /// is stashed for the next dispatch, the mint step never runs, and the
    /// bead routes to recovery (specs/harness.md § Verdict Gate).
    #[tokio::test]
    async fn exec_per_bead_gate_verify_fail_rolls_back_and_stashes_post_integrate_fail() {
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
        assert_ne!(
            loom_driver::git::sync_head_commit_sha(&loom_ws)
                .expect("ff'd sha")
                .to_string(),
            base_tip.to_string(),
            "the ff-merge stand-in must advance the integration branch",
        );

        let stub = dir.path().join("loom-stub.sh");
        std::fs::write(
            &stub,
            "#!/bin/sh\n\
             set -euo pipefail\n\
             case \"$2\" in\n\
                 verify) echo 'verifier failed: cargo test' >&2; exit 1 ;;\n\
                 mint) echo 'mint must not run after verify-fail' ; exit 99 ;;\n\
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
            Some(PreviousFailure::PostIntegrateFail { ref failures }) => {
                assert_eq!(failures.len(), 1, "one verifier-failure block expected");
                assert_eq!(failures[0].exit_code, 1);
                assert!(
                    failures[0].target.contains("loom gate verify --bead lm-1"),
                    "failure block names the failing verifier: {:?}",
                    failures[0].target,
                );
            }
            other => panic!("expected stashed PostIntegrateFail, got {other:?}"),
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
            "#!/bin/sh\n\
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
