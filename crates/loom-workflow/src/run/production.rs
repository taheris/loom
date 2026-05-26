//! Production [`AgentLoopController`] used by the `loom run` binary.
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
//! [`RunError::Profile`] — no silent fallback.

use std::path::PathBuf;
use std::sync::Arc;

use loom_driver::agent::{ProtocolError, SpawnConfig};
use loom_driver::bd::{
    BdClient, Bead, CommandRunner, ListOpts, ReadyOpts, TokioRunner, UpdateOpts,
};
use loom_driver::config::Phase;
use loom_driver::git::{CreatedWorktree, GitClient, MergeResult};
use loom_driver::identifier::{BeadId, ProfileName, SpecLabel};
use loom_driver::lock::LockGuard;
use loom_driver::profile_manifest::{ProfileError, ProfileImageManifest};
use loom_driver::scratch::resolve_scratch_key;
use tokio::process::Command;
use tracing::{info, warn};

use super::context::{RunContextInputs, render_run_prompt};
use super::error::RunError;
use super::outcome::{AgentOutcome, SessionResult};
use super::runner::{AgentLoopController, ExecReviewOutcome};
use super::spawn::build_spawn_config_from_manifest;
use super::tree_clean::dirty_paths_from_porcelain;
use crate::review::{GateInputs, PhaseVerdict, RecoveryCause, decide};
use crate::todo::ExitSignal;
use loom_templates::run::PreviousFailure;

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
        }
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
}

impl<S, F, R: CommandRunner> AgentLoopController for ProductionAgentLoopController<S, F, R>
where
    S: Fn(SpawnConfig, BeadId) -> F + Send,
    F: std::future::Future<Output = (SessionResult, Option<ExitSignal>)> + Send,
{
    async fn next_ready_bead(&mut self) -> Result<Option<Bead>, RunError> {
        // Dedup of clarify/blocked beads relies on the paired
        // `status=blocked` transition that `apply_clarify` / `apply_blocked`
        // write alongside the label. `bd ready` natively excludes
        // status=blocked, so no exclude-label flag is needed.
        let beads = self
            .bd
            .ready(ReadyOpts {
                limit: Some(1),
                label: Some(self.spec_label_filter()),
                exclude_label: vec![],
            })
            .await?;
        Ok(beads.into_iter().next())
    }

    async fn run_bead(
        &mut self,
        bead: &Bead,
        previous_failure: Option<String>,
    ) -> Result<AgentOutcome, RunError> {
        let banner = format!("loom run @ {}", bead.id);
        let is_retry = previous_failure.is_some();
        // The stash is per-retry-sequence: a fresh dispatch
        // (`previous_failure = None`) means any leftover variant from a
        // prior bead's retry chain is stale. Resolve typed `PreviousFailure`
        // by preferring the stashed variant (set by the tree-not-clean
        // dispatcher) over the opaque runner-supplied error string.
        let typed_previous_failure = if is_retry {
            self.stashed_previous_failure
                .take()
                .or_else(|| previous_failure.map(PreviousFailure::from_agent_error))
        } else {
            self.stashed_previous_failure = None;
            None
        };
        // Per-bead worktree creation precedes scratch/prompt setup so the
        // scratchpad and spawn workspace both live inside the worktree (the
        // agent's view of the workspace at dispatch time).
        let worktree = self.git.create_worktree(&self.label, &bead.id).await?;
        info!(
            bead = %bead.id,
            path = %worktree.path.display(),
            branch = %worktree.branch,
            "worktree created",
        );

        let key = resolve_scratch_key(Phase::Run, &self.label, Some(&bead.id));
        let scratchpad_path =
            loom_driver::scratch::ScratchSession::scratchpad_path_for(&worktree.path, &key)
                .to_string_lossy()
                .into_owned();
        let attempt = u32::from(is_retry);
        let initial_prompt = match render_run_prompt(RunContextInputs {
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
                cleanup_worktree(&self.git, &worktree).await?;
                return Err(RunError::Protocol(ProtocolError::Io(
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
                cleanup_worktree(&self.git, &worktree).await?;
                return Err(RunError::Protocol(ProtocolError::Io(source)));
            }
        };
        let spawn_config = match build_spawn_config_from_manifest(
            &self.manifest,
            bead,
            self.cli_profile.as_ref(),
            &self.phase_default,
            worktree.path.clone(),
            initial_prompt,
            scratch.path().to_path_buf(),
            vec![],
            vec![],
        ) {
            Ok(cfg) => cfg,
            Err(ProfileError::UnknownProfile { name, .. }) => {
                drop(scratch);
                cleanup_worktree(&self.git, &worktree).await?;
                return Ok(AgentOutcome::UnknownProfile {
                    error: format_unknown_profile_error(&name, &self.manifest),
                });
            }
            Err(e) => {
                drop(scratch);
                cleanup_worktree(&self.git, &worktree).await?;
                return Err(RunError::Profile(e));
            }
        };
        info!(
            bead = %bead.id,
            image_ref = %spawn_config.image_ref,
            worktree = %worktree.path.display(),
            retry = is_retry,
            "loom run: dispatching agent",
        );
        let (session, marker) = (self.spawn)(spawn_config, bead.id.clone()).await;
        drop(scratch);

        let outcome = classify_session(session, marker);
        if outcome == AgentOutcome::Success {
            // Tree-clean precedes verify-fail / review-concern per
            // `specs/harness.md` § Verdict Gate. Running verifiers against
            // a half-staged tree would conflate the agent's intended diff
            // with its leftover scratch.
            let porcelain = self.git.status_porcelain_at(&worktree.path).await?;
            let dirty = dirty_paths_from_porcelain(&porcelain);
            if !dirty.is_empty() {
                warn!(
                    bead = %bead.id,
                    dirty_count = dirty.len(),
                    "tree-not-clean: cleaning worktree and stashing TreeNotClean for the next attempt",
                );
                cleanup_worktree(&self.git, &worktree).await?;
                self.stashed_previous_failure =
                    Some(PreviousFailure::TreeNotClean { dirty_paths: dirty });
                return Ok(AgentOutcome::Failure {
                    error: "tree-not-clean".to_string(),
                });
            }
            match self.git.merge_branch(&worktree.branch).await? {
                MergeResult::Ok => {
                    cleanup_worktree(&self.git, &worktree).await?;
                    Ok(AgentOutcome::Success)
                }
                MergeResult::Conflict => {
                    // Worktree preserved for human resolution per
                    // parallel-path semantics; only the merge-conflict
                    // failure body is returned.
                    warn!(
                        bead = %bead.id,
                        branch = %worktree.branch,
                        path = %worktree.path.display(),
                        "merge conflict — worktree preserved for inspection",
                    );
                    Ok(AgentOutcome::Failure {
                        error: format!(
                            "merge conflict: worktree preserved at {} on branch {} for human resolution",
                            worktree.path.display(),
                            worktree.branch,
                        ),
                    })
                }
            }
        } else {
            cleanup_worktree(&self.git, &worktree).await?;
            Ok(outcome)
        }
    }

    async fn apply_clarify(&mut self, bead: &BeadId, _question: &str) -> Result<(), RunError> {
        // Persistence boundary (specs/gate.md § "Persistence boundary:
        // agent narrates, agent persists"): the gate / runner only stamps
        // the `loom:clarify` label. The canonical `## Options — …` block
        // is written by the agent via `bd update --notes` *before* it
        // emits `LOOM_CLARIFY`; clobbering that block with the agent's
        // one-line stdout reason would leave `loom msg`'s queue empty.
        //
        // The status transition pairs with the label so `bd ready` excludes
        // the bead via its native status filter — see `next_ready_bead`.
        self.bd
            .update(
                bead,
                UpdateOpts {
                    status: Some("blocked".to_string()),
                    add_labels: vec!["loom:clarify".to_string()],
                    ..UpdateOpts::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn apply_blocked(
        &mut self,
        bead: &BeadId,
        cause: &str,
        error: &str,
    ) -> Result<(), RunError> {
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

    async fn exec_review(&mut self) -> Result<ExecReviewOutcome, RunError> {
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
        // molecule metadata DO surface as `RunError`.
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
            "loom run: molecule handoff — loom gate verify --diff finished",
        );
        // Thread the verify exit into the child via `--verify-exit <CODE>`
        // so the push gate's four-condition AND (FR9 condition 2) consumes
        // it. Signal-terminated children surface `None`; the spec treats no
        // exit code as "no clean success" — use a non-zero sentinel so the
        // gate routes through `verifier-failed` rather than skipping the
        // condition.
        let verify_exit_arg = verify_status.code().unwrap_or(1);
        let review_status = Command::new(&self.loom_bin)
            .current_dir(&self.workspace)
            .arg("gate")
            .arg("review")
            .arg("--diff")
            .arg(&diff_range)
            .arg("-s")
            .arg(self.label.as_str())
            .arg("--verify-exit")
            .arg(verify_exit_arg.to_string())
            .status()
            .await?;
        info!(
            spec = %self.label.as_str(),
            diff = %diff_range,
            exit_code = review_status.code().unwrap_or(-1),
            "loom run: molecule handoff — loom gate review --diff finished",
        );
        Ok(ExecReviewOutcome {
            verify_exit: verify_status.code(),
            review_exit: review_status.code(),
            review_marker: None,
        })
    }
}

/// Remove the per-bead worktree and force-delete its branch. Used by the
/// run-phase verdict gate on every non-merged exit path (agent failure,
/// tree-not-clean recovery, profile-resolution failure) so a stale worktree
/// or branch never survives a bead's retry chain.
async fn cleanup_worktree(git: &GitClient, worktree: &CreatedWorktree) -> Result<(), RunError> {
    git.remove_worktree(&worktree.path).await?;
    git.delete_branch(&worktree.branch).await?;
    Ok(())
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

/// Look up the spec's epic via `current_molecule[<label>]` in the
/// workspace state DB and return its `loom.base_commit` metadata. Used
/// by `exec_review` to scope the molecule-completion handoff to the
/// molecule's own diff rather than `--tree`. Delegates metadata
/// resolution to [`crate::init::resolve_base_commit`] so the run-phase
/// and rebuild-phase resolutions share parent inheritance + write-back
/// behaviour verbatim.
async fn fetch_molecule_base_commit<R: CommandRunner>(
    bd: &BdClient<R>,
    workspace: &std::path::Path,
    label: &SpecLabel,
) -> Result<String, RunError> {
    let db = loom_driver::state::StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let mol_id = db
        .current_molecule(label)?
        .ok_or_else(|| RunError::NoActiveMolecule {
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
                InitError::Bd(e) => RunError::Bd(e),
                InitError::MoleculeMissingBaseCommit { id } => {
                    RunError::MoleculeMissingBaseCommit { id }
                }
                InitError::MoleculeMissingBaseCommitNoParentMetadata { id, parent } => {
                    RunError::MoleculeMissingBaseCommitNoParentMetadata { id, parent }
                }
                other => RunError::Bug {
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
            if let Some(ExitSignal::Concern { token, .. }) = marker.as_ref() {
                return AgentOutcome::Failure {
                    error: format!(
                        "wrong-phase-marker: LOOM_CONCERN ({token}) is review-phase only",
                    ),
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
        PhaseVerdict::Recovery { cause } => AgentOutcome::Failure {
            error: format!("unexpected gate verdict: {}", cause.as_str()),
        },
    }
}

/// Helper used by `main.rs` to fetch the spec-filtered open list when the
/// caller needs the typed [`Bead`] slice (e.g. to print a status line).
/// Surfacing this here keeps the BdClient list-shape next to the controller.
pub async fn list_open_for_spec(bd: &BdClient, label: &SpecLabel) -> Result<Vec<Bead>, RunError> {
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

    /// Seed `current_molecule[<spec_label>] = <mol_id>` in
    /// `<workspace>/.wrapix/loom/state.db` and return a `ScriptedBd`
    /// matching the single `bd show <mol_id>` call
    /// `fetch_molecule_base_commit` issues against the looked-up epic
    /// (with `loom.base_commit = <base>` metadata).
    fn molecule_lookup_script(
        workspace: &std::path::Path,
        spec_label: &str,
        mol_id: &str,
        base: &str,
    ) -> ScriptedBd {
        seed_current_molecule(workspace, spec_label, mol_id);
        let show_body = format!(
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
        ScriptedBd::new([ok_stdout(show_body.as_bytes())])
    }

    /// Seed `current_molecule[<spec_label>] = <mol_id>` in the
    /// workspace state DB. Required by every test that exercises
    /// `fetch_molecule_base_commit`'s state-DB read.
    fn seed_current_molecule(workspace: &std::path::Path, spec_label: &str, mol_id: &str) {
        let db = loom_driver::state::StateDb::open(workspace.join(".wrapix/loom/state.db"))
            .expect("open state.db");
        let label = SpecLabel::new(spec_label);
        db.set_current_molecule(&label, &loom_driver::identifier::MoleculeId::new(mol_id))
            .expect("seed current_molecule");
    }

    /// FR12 — `loom run`'s per-bead exit MUST route the agent's marker
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
                token: "verifier-bypass".into(),
                reason: "test mocks the agent backend".into(),
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
          "base": { "ref": "localhost/wrapix-base:abc", "source": "/nix/store/aaa-image-base" }
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
        loom_driver::git::init_test_repo(workspace).expect("init test repo")
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
            .run_bead(&bead("wx-1"), None)
            .await
            .expect("run_bead ok");
        assert_eq!(outcome, AgentOutcome::Success);
        let cfg = captured.lock().unwrap().take().expect("closure called");
        assert_eq!(cfg.image_ref, "localhost/wrapix-base:abc");
        assert!(cfg.initial_prompt.contains("wx-1"));
    }

    /// `loom run` must dispatch with the rendered
    /// [`RunContext`] template — bead title/description, scratchpad path,
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
            id: BeadId::new("wx-99").expect("bead id"),
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
        // Rendered template body, not the legacy "loom run: bead <id>" stub.
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
            .run_bead(&bead("wx-2"), None)
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
            .run_bead(&bead("wx-3"), None)
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
    /// driver-memory budget can absorb one occurrence per `loom run`.
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
            .run_bead(&bead("wx-4"), None)
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
    /// so [`process_one_bead`](crate::run::run_loop) routes it straight to
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
            id: BeadId::new("wx-5").expect("valid bead id"),
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
            .expect("run_bead must NOT bubble UnknownProfile up as RunError");
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

    /// Regression: `loom run` used to hold the spec lock for its whole
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
            "wx-mol.1",
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
            "wx-mol.1",
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
    /// `bd ready` rather than tearing down the whole `loom run`.
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
            "wx-mol.7",
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
            .expect("non-zero verify exit must not produce RunError");

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
        // through `ExecReviewOutcome` so the push-gate verdict can
        // refuse the push on `Some(n)` with `n != 0`.
        assert_eq!(
            handoff.verify_exit,
            Some(1),
            "verify child exit code threaded through ExecReviewOutcome",
        );
        assert_eq!(
            handoff.review_exit,
            Some(0),
            "review child exit code threaded through ExecReviewOutcome",
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
        // state.db has no current_molecule[orphan-spec] entry → NoActiveMolecule.
        let bd = BdClient::with_runner(ScriptedBd::new([]));
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
            RunError::NoActiveMolecule { label } => assert_eq!(label, "orphan-spec"),
            other => panic!("expected NoActiveMolecule, got {other:?}"),
        }
    }

    /// FR1 negative: an epic recorded in `current_molecule` whose bead
    /// lacks `loom.base_commit` metadata MUST surface
    /// `MoleculeMissingBaseCommit` rather than fabricate a diff range.
    /// The metadata key is set unconditionally on every molecule the
    /// harness creates; the absence is a state-DB corruption signal
    /// worth surfacing loudly.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_errors_when_molecule_missing_base_commit_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        seed_current_molecule(dir.path(), "gamma", "wx-mol.99");
        let show_body = br#"[{
            "id": "wx-mol.99",
            "title": "gamma: pending decomposition",
            "status": "open",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["spec:gamma"],
            "metadata": {}
        }]"#;
        let bd = BdClient::with_runner(ScriptedBd::new([ok_stdout(show_body)]));
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
            RunError::MoleculeMissingBaseCommit { id } => assert_eq!(id, "wx-mol.99"),
            other => panic!("expected MoleculeMissingBaseCommit, got {other:?}"),
        }
    }

    /// `current_molecule` may point at an epic that lacks its own
    /// `loom.base_commit` — typical when the pointer was seeded out-of-band.
    /// `fetch_molecule_base_commit` MUST mirror `init::fetch_active_molecules`'s
    /// self-heal: read the parent's `loom.base_commit`, persist it on the
    /// child via `bd update --set-metadata`, and continue the
    /// molecule-completion handoff using the inherited value. Without this,
    /// `exec_review` would surface `RunError::MoleculeMissingBaseCommit` for
    /// a state the spec calls valid.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_inherits_base_commit_from_parent_when_child_lacks_metadata() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("delta");
        seed_current_molecule(dir.path(), "delta", "wx-child.7");

        let argv_log = dir.path().join("argv.log");
        let stub = dir.path().join("loom-stub.sh");
        let stub_body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\nexit 0\n",
            log = argv_log.to_string_lossy(),
        );
        std::fs::write(&stub, stub_body).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let child_show = br#"[{
            "id": "wx-child.7",
            "title": "delta follow-up",
            "status": "open",
            "priority": 2,
            "issue_type": "bug",
            "labels": ["spec:delta"],
            "parent": "wx-epicd",
            "metadata": {}
        }]"#;
        let parent_show = br#"[{
            "id": "wx-epicd",
            "title": "delta: pending decomposition",
            "status": "open",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["spec:delta"],
            "metadata": {"loom.base_commit": "feed0042"}
        }]"#;
        let bd = BdClient::with_runner(ScriptedBd::new([
            ok_stdout(child_show),
            ok_stdout(parent_show),
            ok_stdout(b""), // bd update --set-metadata (inheritance write-back)
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
        seed_current_molecule(dir.path(), "epsilon", "wx-child.8");
        let child_show = br#"[{
            "id": "wx-child.8",
            "title": "epsilon follow-up",
            "status": "open",
            "priority": 2,
            "issue_type": "bug",
            "labels": ["spec:epsilon"],
            "parent": "wx-epice",
            "metadata": {}
        }]"#;
        let parent_show = br#"[{
            "id": "wx-epice",
            "title": "epsilon: pending decomposition",
            "status": "open",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["spec:epsilon"]
        }]"#;
        let bd = BdClient::with_runner(ScriptedBd::new([
            ok_stdout(child_show),
            ok_stdout(parent_show),
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
            msg.contains("bd update wx-child.8 --set-metadata loom.base_commit="),
            "error must surface the fix command: {msg}",
        );
        match err {
            RunError::MoleculeMissingBaseCommitNoParentMetadata { id, parent } => {
                assert_eq!(id, "wx-child.8");
                assert_eq!(parent, "wx-epice");
            }
            other => panic!("expected MoleculeMissingBaseCommitNoParentMetadata, got {other:?}"),
        }
    }

    /// specs/gate.md § "Persistence boundary: agent narrates, agent persists":
    /// the runner's `apply_clarify` only stamps the `loom:clarify` label —
    /// the canonical `## Options — …` block belongs to the agent, written
    /// to bead state *before* `LOOM_CLARIFY` is emitted. If the runner
    /// also wrote the agent's stdout reason-line via `bd update --notes`,
    /// every re-emit would clobber the canonical block and leave
    /// `loom msg`'s queue empty.
    #[tokio::test(flavor = "multi_thread")]
    async fn apply_clarify_does_not_write_notes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let workspace = dir.path().join("ws");
        let git = git_workspace(&workspace);
        let scripted = ScriptedBd::new([ok_stdout(b"")]);
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
        let bead_id = BeadId::new("wx-clarify.1").expect("bead id");
        controller
            .apply_clarify(
                &bead_id,
                "agent's one-line reason that must not clobber notes",
            )
            .await
            .expect("apply_clarify ok");
        let captured = calls.lock().unwrap();
        assert_eq!(captured.len(), 1, "exactly one bd invocation expected");
        let argv: Vec<String> = captured[0]
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv[0], "update");
        assert_eq!(argv[1], "wx-clarify.1");
        assert!(
            argv.iter().any(|a| a == "--add-label"),
            "missing --add-label in argv: {argv:?}",
        );
        assert!(
            argv.iter().any(|a| a == "loom:clarify"),
            "missing loom:clarify label in argv: {argv:?}",
        );
        assert!(
            !argv.iter().any(|a| a == "--notes"),
            "apply_clarify must not forward --notes (persistence boundary): {argv:?}",
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--status" && w[1] == "blocked"),
            "apply_clarify must pair --status blocked with --add-label so \
             `bd ready` excludes via its native status filter: {argv:?}",
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
        let bead_id = BeadId::new("wx-blocked.1").expect("bead id");
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
        assert_eq!(argv[1], "wx-blocked.1");
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
}
