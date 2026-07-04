//! Production [`ReviewController`] used by the `loom review` binary.
//!
//! Wires `BdClient` for spec-bead snapshots and clarify,
//! `tokio::process::Command` shell-outs for `git push`, `beads-push`, and
//! the auto-iterate `loom loop` handoff, and a caller-provided dispatch
//! closure for the reviewer agent invocation. The closure pattern keeps
//! review backend selection (`PiBackend`, `ClaudeBackend`, or `DirectBackend`)
//! inside the binary's `dispatch` match, mirroring
//! [`ProductionTodoController`](super::super::todo::ProductionTodoController)
//! and [`ProductionAgentLoopController`](super::super::run::ProductionAgentLoopController).
//!
//! Iteration-counter accessors read/write `molecules.iteration_count` for
//! the active molecule of `self.label`. `iteration_count` returns 0 when no
//! molecule has been seeded yet (the auto-iterate gate treats this as the
//! start of a cycle); `set_iteration_count` errors loudly if the active
//! molecule is missing so a misconfigured run cannot loop forever; `reset`
//! is a no-op in that case so the Clean push path is unaffected on a
//! freshly-init'd workspace.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use askama::Template;
use loom_driver::agent::{AgentRuntime, ProtocolError, SessionOutcome, SpawnConfig};
use loom_driver::bd::{BdClient, BdError, Bead, CommandRunner, ListOpts, TokioRunner, UpdateOpts};
use loom_driver::clock::{Clock, SystemClock};
use loom_driver::config::{LoomConfig, Phase, SkillsConfig, SuppressionConfig};
use loom_driver::git::GitClient;
use loom_driver::identifier::{BeadId, MoleculeId, ProfileName, SpecLabel};
use loom_driver::lock::LockGuard;
use loom_driver::logging::phase_log_path;
use loom_driver::logging::{BeadOutcome, LogSink};
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_driver::scratch::resolve_scratch_key;
use loom_driver::state::CacheDb;
use loom_events::identifier::SessionId;
use loom_events::{AgentEvent, DriverKind, EnvelopeBuilder, SessionScope, Source};
use loom_gate::{
    DispatchOptions, DispatchPendingExecutor, FsCommandResolver, GateRun, GateSuccess,
    HandoffEvidence, InputResolver, IntegrityFinding, MarkerProof, TierCwds, annotation,
    append_gate_run_lifecycle_events, compose_clarify_options, integrity,
    parse_gate_runs_from_jsonl,
};
use loom_templates::previous_failure::PreviousFailure;
use loom_templates::review::{ReviewContext, ReviewLane};
use tokio::process::Command;
use tracing::{info, warn};

use super::context::{beads_summary, default_profile_for_spec, load_review_sources_for_lane};
use super::error::ReviewError;
use super::finding::{DispatchScope, FindingValidator, TerminalSurface, WalkOutput};
use super::phase_verdict::{
    GateInputs, PhaseKind, PhaseVerdict, RecoveryCause, decide, decide_for_phase,
};
use super::runner::{ReviewController, ReviewOutcome, RunReviewOutput};
use super::verdict::PushGateRefuseCause;
use super::workspace_validator::WorkspaceFindingValidator;
use crate::skill::SkillPlan;
use crate::spawn::{container_workspace_path, launcher_key_env_for_checkout};
use crate::suppression::{has_ineffective_suppression_match, suppresses_rubric_finding};
use crate::todo::ExitSignal;

/// Non-I/O validator used by matrix tests that isolate stream-shape routing.
pub struct AcceptAllFindingValidator;

impl FindingValidator for AcceptAllFindingValidator {
    fn spec_label_is_known(&self, _label: &SpecLabel) -> bool {
        true
    }
    fn criterion_anchor_resolves(&self, _spec: &SpecLabel, _anchor: &str) -> bool {
        true
    }
    fn annotation_resolves(&self, _target_string: &str) -> bool {
        true
    }
    fn file_exists(&self, _path: &str) -> bool {
        true
    }
    fn invariant_resolves(&self, _spec: &SpecLabel, _section: &str, _tag: &str) -> bool {
        true
    }
}

fn push_refusal_note(existing: Option<&str>, cause: PushGateRefuseCause) -> String {
    let note = format!(
        "push-gate-refused: {}; local bead workspace preserved under .loom/beads for inspection",
        cause.as_str(),
    );
    match existing.filter(|body| !body.trim().is_empty()) {
        Some(body) => format!("{body}\n\n{note}"),
        None => note,
    }
}

fn pre_commit_config_digest(workspace: &Path) -> Result<String, ReviewError> {
    let path = workspace.join(".pre-commit-config.yaml");
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

pub struct ProductionReviewController<S, F, R: CommandRunner = TokioRunner>
where
    S: Fn(SpawnConfig) -> F + Send + Sync,
    F: std::future::Future<
            Output = Result<(SessionOutcome, Option<ExitSignal>, String), ProtocolError>,
        > + Send,
{
    bd: BdClient<R>,
    label: SpecLabel,
    loom_bin: PathBuf,
    workspace: PathBuf,
    state: Arc<CacheDb>,
    manifest: Arc<ProfileImageManifest>,
    phase_default: ProfileName,
    runtime: AgentRuntime,
    spawn: S,
    /// Spec lock dropped before exec'ing `loom loop` so the child can take it.
    lock: Option<LockGuard>,
    /// Phase log root + start timestamp. The verdict gate emits
    /// `push_gate_*` driver events into the same JSONL log file the
    /// reviewer agent writes to, so a replay can replay the full review
    /// phase. Both writers compute the file path from
    /// `(phase_log_root, label, "review", phase_log_when)`, which is
    /// deterministic — append-mode opens share one file.
    phase_log_root: Option<PathBuf>,
    phase_log_when: SystemTime,
    /// Per-phase envelope builder. The review phase isn't bead-scoped,
    /// so the envelope carries a session id without work-routing fields;
    /// the builder tracks `seq` across every `emit_driver_event` call so
    /// replay code can reorder events deterministically. Wrapped in
    /// `Mutex` because `EnvelopeBuilder`'s clock closure is `Send`
    /// but not `Sync` — the trait's `Send`-future bound requires the
    /// controller itself to be `Sync` across `&self` borrows.
    envelope_builder: Mutex<Option<EnvelopeBuilder>>,
    /// Workspace-relative path to the style-rules document pinned in the
    /// review prompt. Sourced from `LoomConfig.style_rules` at construction
    /// via [`Self::with_style_rules`]; defaults to the built-in path so
    /// test fakes that skip the builder still render a valid prompt.
    style_rules: String,
    /// Integration branch the gate's `git push` targets — threaded from
    /// `LoomConfig.loom.integration_branch` via
    /// [`Self::with_integration_branch`]. Defaults to `main` so tests
    /// that skip the builder still push the conventional branch name.
    integration_branch: String,
    /// Timeout for the gate's `git push` (whose pre-push hook runs the
    /// workspace CI stage) — threaded from `[loom] git_hook_timeout_secs`
    /// via [`Self::with_hook_timeout`]. Defaults to the same 600s the
    /// `GitClient` uses so tests skipping the builder keep prior behavior.
    hook_timeout: Duration,
    push_range: Option<String>,
    /// Which lane(s) of the review this controller drives. `Both` is the
    /// `loom gate review` path; `Judge`/`Rubric` are the focused single-
    /// lane re-runs surfaced by `loom gate judge` / `loom gate rubric`.
    lane: ReviewLane,
    dispatch_scope: DispatchScope,
    /// Top-level `[[suppress]]` rubric-finding allowlist entries.
    suppressions: Vec<SuppressionConfig>,
    skills_cfg: SkillsConfig,
    effective_review_marker: Option<ExitSignal>,
    suppressed_review_concern: bool,
}

struct BuiltReviewPrompt {
    prompt: String,
    skill_plan: SkillPlan,
    key: String,
}

fn review_dispatch_scope_pin(scope: DispatchScope, range: Option<&str>) -> String {
    match (scope, range) {
        (DispatchScope::Tree, _) => concat!(
            "Current dispatch scope: `--tree`. The input set is every file in the ",
            "workspace; do not narrow review to a base-to-HEAD diff or log range.\n",
        )
        .to_owned(),
        (DispatchScope::PerBead, Some(range)) => format!(
            "Current dispatch scope: `--diff {range}`. The input set is \
             `git diff {range} --name-only`; use that exact range for diff/log evidence.\n",
        ),
        (DispatchScope::PerBead, None) => concat!(
            "Current dispatch scope: finite diff / per-bead. Use the driver-supplied ",
            "diff range for diff/log evidence; do not infer tree scope.\n",
        )
        .to_owned(),
        (DispatchScope::PushGate, Some(range)) => format!(
            "Current dispatch scope: push-gate `{range}`. Use that push range for \
             diff/log evidence.\n",
        ),
        (DispatchScope::PushGate, None) => concat!(
            "Current dispatch scope: push-gate. Use the driver-supplied push range ",
            "for diff/log evidence.\n",
        )
        .to_owned(),
    }
}

impl<S, F, R: CommandRunner> ProductionReviewController<S, F, R>
where
    S: Fn(SpawnConfig) -> F + Send + Sync,
    F: std::future::Future<
            Output = Result<(SessionOutcome, Option<ExitSignal>, String), ProtocolError>,
        > + Send,
{
    /// Re-resolve the review log path the same way `exec_review` does, from
    /// `(phase_log_root, label, "review", phase_log_when)`, keeping it only
    /// when the file is on disk — so the sealed `GateSuccess` constructor's
    /// evidence check reads the file the reviewer agent actually wrote.
    fn resolve_review_log_for_marker(&self) -> Option<PathBuf> {
        self.phase_log_root
            .as_deref()
            .map(|root| phase_log_path(root, &self.label, "review", self.phase_log_when))
            .filter(|p| p.exists())
    }

    #[expect(clippy::too_many_arguments, reason = "controller construction surface")]
    pub fn new(
        bd: BdClient<R>,
        label: SpecLabel,
        loom_bin: PathBuf,
        workspace: PathBuf,
        state: Arc<CacheDb>,
        manifest: Arc<ProfileImageManifest>,
        phase_default: ProfileName,
        spawn: S,
    ) -> Self {
        Self {
            bd,
            label,
            loom_bin,
            workspace,
            state,
            manifest,
            phase_default,
            runtime: AgentRuntime::Pi,
            spawn,
            lock: None,
            phase_log_root: None,
            phase_log_when: SystemClock::new().wall_now(),
            envelope_builder: Mutex::new(None),
            style_rules: "docs/style-rules.md".to_string(),
            integration_branch: "main".to_string(),
            hook_timeout: Duration::from_secs(loom_driver::config::default_git_hook_timeout_secs()),
            push_range: None,
            lane: ReviewLane::Both,
            dispatch_scope: DispatchScope::PerBead,
            suppressions: Vec::new(),
            skills_cfg: SkillsConfig::default(),
            effective_review_marker: None,
            suppressed_review_concern: false,
        }
    }

    /// Hand the work-root lock to the controller so `exec_run` can drop it
    /// before spawning the `loom loop` child (which acquires the same lock).
    pub fn with_handoff_lock(mut self, guard: LockGuard) -> Self {
        self.lock = Some(guard);
        self
    }

    /// Override the style-rules pin used in the rendered review prompt.
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

    /// Override the integration branch the gate's `git push` targets.
    /// Production callers thread `LoomConfig.loom.integration_branch`;
    /// tests rely on the `main` default.
    pub fn with_integration_branch(mut self, branch: String) -> Self {
        self.integration_branch = branch;
        self
    }

    /// Override the timeout for the gate's `git push`. Production callers
    /// thread `LoomConfig.loom.git_hook_timeout()`; tests rely on the
    /// built-in default.
    pub fn with_hook_timeout(mut self, hook_timeout: Duration) -> Self {
        self.hook_timeout = hook_timeout;
        self
    }

    pub fn with_push_range(mut self, range: Option<String>) -> Self {
        self.push_range = range;
        self
    }

    /// Select which lane(s) of the review this controller will drive.
    /// `Both` keeps the full `loom gate review` path; `Judge`/`Rubric`
    /// narrow the rendered prompt to one lane per `loom gate judge` /
    /// `loom gate rubric`.
    pub fn with_lane(mut self, lane: ReviewLane) -> Self {
        self.lane = lane;
        self
    }

    /// Select the finding-token scope used while parsing reviewer stdout.
    pub fn with_dispatch_scope(mut self, scope: DispatchScope) -> Self {
        self.dispatch_scope = scope;
        self
    }

    /// Override the rubric-finding suppressions used after walk-shape validation.
    pub fn with_suppressions(mut self, suppressions: Vec<SuppressionConfig>) -> Self {
        self.suppressions = suppressions;
        self
    }

    pub fn with_skills_config(mut self, cfg: SkillsConfig) -> Self {
        self.skills_cfg = cfg;
        self
    }

    /// Pin the phase log file the verdict gate's driver events stream
    /// into. The spawn closure inside `run_review` MUST use the same
    /// `when` when it opens its agent-event sink or the two writers
    /// land in separate files. Tests and the CLI share this via
    /// `phase_log_when()`.
    pub fn with_phase_log(mut self, logs_root: PathBuf, when: SystemTime) -> Self {
        self.phase_log_root = Some(logs_root);
        self.phase_log_when = when;
        self
    }

    /// The pinned phase log timestamp — read by the binary's spawn
    /// closure so its agent-event `LogSink` lands in the same file
    /// the controller's driver events append to.
    pub fn phase_log_when(&self) -> SystemTime {
        self.phase_log_when
    }

    fn spec_label_filter(&self) -> String {
        format!("spec:{}", self.label.as_str())
    }

    /// Push gate must invoke `beads-push`, NOT `bd dolt push` — only
    /// `beads-push` syncs the `beads` git branch to GitHub.
    fn beads_push_command(&self) -> Command {
        let mut cmd = Command::new("beads-push");
        cmd.current_dir(&self.workspace);
        cmd
    }

    /// Resolve the spec's open epic via `bd find --type=epic
    /// --label=spec:<X> --status=open`. The at-most-one-open-epic-per-spec
    /// invariant collapses resolution into this single query — no state
    /// DB pointer, no tier walk.
    async fn resolve_molecule_id(&self) -> Result<Option<MoleculeId>, ReviewError> {
        Ok(crate::resolve::resolve_open_epic(&self.bd, &self.label).await?)
    }

    /// Locate the molecule's epic bead via the at-most-one-open-epic-per-spec
    /// resolution. Used by the integrity-clarify path to find the write
    /// target for `bd update --notes ... --add-label loom:clarify`.
    async fn molecule_epic_bead(&self) -> Result<Option<Bead>, ReviewError> {
        let Some(mol_id) = self.resolve_molecule_id().await? else {
            return Ok(None);
        };
        let bead_id = BeadId::new(mol_id.as_str()).map_err(BdError::CreateInvalidId)?;
        match self.bd.show(&bead_id).await {
            Ok(bead) => Ok(Some(bead)),
            Err(BdError::ShowEmpty) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Walk the spec files changed between the active molecule's
    /// `base_commit` and `HEAD`, run the integrity gate's forward
    /// resolution and inputs-protocol direction against the annotations
    /// they declare, and keep only the findings that are terminal at the
    /// push gate
    /// ([`IntegrityFinding::is_push_gate_terminal`]). Workspace-wide
    /// resolver scans (`RustWorkspaceTestResolver`, `RustWorkspaceStubScanner`)
    /// are built once per call. Returns an empty list when no molecule is
    /// active or when no spec files have changed in the molecule's range.
    async fn molecule_integrity_findings(&self) -> Result<Vec<IntegrityFinding>, ReviewError> {
        let Some(mol_id) = self.resolve_molecule_id().await? else {
            return Ok(vec![]);
        };
        let Some(mol) = self.state.molecule(&mol_id)? else {
            return Ok(vec![]);
        };
        let Some(base) = mol.base_commit else {
            return Ok(vec![]);
        };
        let git = GitClient::open(&self.workspace)
            .map_err(|e| ReviewError::Io(std::io::Error::other(e.to_string())))?;
        let changed_specs = git
            .changed_spec_files(&base)
            .await
            .map_err(|e| ReviewError::Io(std::io::Error::other(e.to_string())))?;
        if changed_specs.is_empty() {
            return Ok(vec![]);
        }
        let mut annotations = Vec::new();
        for rel in &changed_specs {
            let abs = self.workspace.join(rel);
            let Ok(body) = std::fs::read_to_string(&abs) else {
                continue;
            };
            let parsed = annotation::parse_content(rel, &body);
            annotations.extend(parsed.annotations);
        }
        if annotations.is_empty() {
            return Ok(vec![]);
        }
        let cmd_resolver = FsCommandResolver::new(&self.workspace);
        let (test_resolver, stub_scanner) =
            loom_gate::integrity::scan_workspace_pair(&self.workspace)
                .map_err(|e| ReviewError::Io(std::io::Error::other(e.to_string())))?;
        let config = LoomConfig::load(LoomConfig::resolve_path(&self.workspace))
            .map_err(|e| ReviewError::Io(std::io::Error::other(e.to_string())))?;
        let runner_specs = loom_gate::runner::integrity_runner_specs(&config)
            .map_err(|e| ReviewError::Io(std::io::Error::other(e.to_string())))?;
        let pending_executor = DispatchPendingExecutor::new(
            &runner_specs,
            DispatchOptions::default(),
            &self.workspace,
            TierCwds::default(),
        );
        let mut findings = integrity::check_forward(
            &annotations,
            &runner_specs,
            &self.workspace,
            &cmd_resolver,
            &test_resolver,
            &stub_scanner,
            &pending_executor,
        );
        let mut input_resolver =
            InputResolver::new(self.workspace.clone()).with_runners(runner_specs.clone());
        findings.extend(integrity::check_inputs_protocol(
            &annotations,
            &mut input_resolver,
        ));
        Ok(findings
            .into_iter()
            .filter(IntegrityFinding::is_push_gate_terminal)
            .collect())
    }

    async fn build_review_prompt(&self) -> Result<BuiltReviewPrompt, ReviewError> {
        let beads = self
            .bd
            .list(ListOpts {
                status: None,
                label: Some(self.spec_label_filter()),
                ..ListOpts::default()
            })
            .await?;
        let molecule_id = self.resolve_molecule_id().await?;
        let base_commit = match molecule_id.as_ref() {
            Some(id) => self.state.molecule(id)?.and_then(|m| m.base_commit),
            None => None,
        };
        let spec_path = format!("specs/{}.md", self.label.as_str());
        let (test_sources, judge_rubrics) = load_review_sources_for_lane(
            &self.workspace,
            &self.workspace.join(&spec_path),
            self.lane,
        )?;
        let key = resolve_scratch_key(Phase::Review, std::slice::from_ref(&self.label), None);
        let scratchpad_path =
            loom_driver::scratch::ScratchSession::scratchpad_path_for(&self.workspace, &key);
        let scratch_dir = scratchpad_path.parent().ok_or_else(|| {
            ReviewError::Protocol(ProtocolError::Io(std::io::Error::other(
                "scratchpad path has no parent",
            )))
        })?;
        let skill_plan = SkillPlan::resolve_from_workspace(
            &self.workspace,
            Phase::Review.as_str(),
            &self.phase_default,
            self.runtime,
            &self.skills_cfg,
        )
        .await?;
        let skill_session = skill_plan.materialize(scratch_dir, &self.workspace)?;
        let prompt_scratchpad_path = container_workspace_path(&self.workspace, &scratchpad_path);
        let ctx = ReviewContext {
            pinned_context: review_dispatch_scope_pin(
                self.dispatch_scope,
                self.push_range.as_deref(),
            ),
            default_profile: default_profile_for_spec(&self.label),
            label: self.label.clone(),
            spec_path,
            companion_paths: vec![],
            beads_summary: beads_summary(&beads),
            base_commit,
            molecule_id,
            test_sources,
            judge_rubrics,
            scratchpad_path: prompt_scratchpad_path.to_string_lossy().into_owned(),
            style_rules: self.style_rules.clone(),
            lane: self.lane,
            skill_index: skill_session.skill_index,
        };
        Ok(BuiltReviewPrompt {
            prompt: ctx.render()?,
            skill_plan,
            key,
        })
    }
}

/// Map the parsed reviewer walk into a [`ReviewOutcome`] (FR12 — single
/// source of truth). The signature takes `&WalkOutput` (not raw `&str`
/// or `Option<&ExitSignal>`) so production callers cannot bypass the
/// parse pipeline and leave the streamed-finding stream at default
/// empty — that silent-loss failure class is structurally
/// unrepresentable per `specs/gate.md` § *Structural enforcement*.
///
/// Marker → outcome routing goes through the canonical [`decide`] gate
/// function. The review phase isn't bead-scoped, so `bd_closed` /
/// `diff_empty` / `verify_failures` / `review_flag` reduce to neutral
/// defaults; per-finding routing now flows through
/// [`GateInputs::streamed_findings`]. The
/// pairing-rule fail cases (
/// `LOOM_COMPLETE` + streamed findings,
/// `LOOM_CONCERN` + zero findings,
/// `LOOM_CONCERN` + malformed payload,
/// any walk + per-line finding errors)
/// surface via the typed `BadWalk` variant on
/// [`RecoveryCause::BadWalk`].
#[cfg(test)]
fn classify_review_phase(walk: &WalkOutput, exit_code: i32) -> ReviewOutcome {
    classify_review_phase_with_suppressions(walk, exit_code, &[])
}

fn classify_review_phase_with_suppressions(
    walk: &WalkOutput,
    exit_code: i32,
    suppressions: &[SuppressionConfig],
) -> ReviewOutcome {
    let marker = exit_signal_from_terminal(walk.terminal());
    if matches!(marker, Some(ExitSignal::Complete)) && exit_code != 0 {
        return ReviewOutcome::Incomplete {
            detail: format!("agent emitted COMPLETE but exited code {exit_code}"),
        };
    }
    match phase_verdict_from_walk_with_suppressions(walk, suppressions) {
        PhaseVerdict::Done => ReviewOutcome::Complete,
        PhaseVerdict::Blocked { reason } => ReviewOutcome::Incomplete {
            detail: format!("LOOM_BLOCKED: {reason}"),
        },
        PhaseVerdict::Clarify { question } => ReviewOutcome::Incomplete {
            detail: format!("LOOM_CLARIFY: {question}"),
        },
        PhaseVerdict::Recovery {
            cause: RecoveryCause::ReviewConcern { summary, findings },
        } => {
            let pf = PreviousFailure::ReviewConcern { summary, findings };
            ReviewOutcome::Incomplete {
                detail: pf.to_string(),
            }
        }
        PhaseVerdict::Recovery {
            cause: RecoveryCause::BadWalk(badwalk),
        } => ReviewOutcome::Incomplete {
            detail: PreviousFailure::BadWalk(badwalk).to_string(),
        },
        PhaseVerdict::Recovery {
            cause: RecoveryCause::SwallowedMarker,
        } => ReviewOutcome::Incomplete {
            detail: if exit_code == 0 {
                "agent exited 0 without LOOM_COMPLETE / LOOM_CONCERN / LOOM_RETRY / \
                 LOOM_BLOCKED marker (swallowed marker)"
                    .to_string()
            } else {
                format!("agent exited with code {exit_code}")
            },
        },
        PhaseVerdict::Recovery {
            cause:
                RecoveryCause::WrongPhaseMarker {
                    marker_name,
                    phase_kind,
                },
        } => ReviewOutcome::Incomplete {
            detail: wrong_phase_marker_detail(marker_name, phase_kind),
        },
        PhaseVerdict::Recovery { cause } => ReviewOutcome::Incomplete {
            detail: format!("unexpected gate verdict: {}", cause.as_str()),
        },
    }
}

fn wrong_phase_marker_detail(marker_name: &str, phase_kind: &str) -> String {
    if phase_kind == "review" && marker_name == "LOOM_CLARIFY" {
        return "wrong-review-path: direct LOOM_CLARIFY is not a review terminal; emit a \
                route=\"clarify\" LOOM_FINDING with the canonical Options block in evidence, \
                then terminate with LOOM_CONCERN"
            .to_string();
    }
    if phase_kind == "review" {
        return format!(
            "wrong-review-path: {marker_name} is not a review terminal; expected \
             LOOM_COMPLETE, LOOM_CONCERN, LOOM_RETRY, or LOOM_BLOCKED",
        );
    }
    format!("wrong-phase-marker: {marker_name} is not valid in {phase_kind} phase")
}

/// Apply the verdict-gate decision table to a parsed [`WalkOutput`],
/// preserving the maximum well-formed context by struct shape per
/// `specs/gate.md` § *Maximum-context preservation invariant*.
///
/// Per-line `LOOM_FINDING:` parse failures route through
/// [`BadWalk::MalformedFinding`] alongside the typed terminal surface
/// before the pairing-rule checks fire. Otherwise the gate's
/// [`decide`] gate function is consulted with the typed marker and any
/// well-formed findings, then the review-only marker restrictions are
/// layered on via [`decide_for_phase`]. A malformed terminal payload paired
/// with well-formed findings threads both into
/// [`BadWalk::Concern { payload, parsed_findings }`].
#[cfg(test)]
fn phase_verdict_from_walk(walk: &WalkOutput) -> PhaseVerdict {
    phase_verdict_from_walk_with_suppressions(walk, &[])
}

fn decide_review_phase(marker: Option<&ExitSignal>, inputs: GateInputs) -> PhaseVerdict {
    let baseline = decide(marker, inputs.clone());
    let review_verdict = decide_for_phase(marker, inputs, PhaseKind::Review);
    if matches!(
        review_verdict,
        PhaseVerdict::Recovery {
            cause: RecoveryCause::WrongPhaseMarker { .. }
        }
    ) {
        review_verdict
    } else {
        baseline
    }
}

fn phase_verdict_from_walk_with_suppressions(
    walk: &WalkOutput,
    suppressions: &[SuppressionConfig],
) -> PhaseVerdict {
    if !walk.finding_errors().is_empty() {
        let badwalk = loom_templates::previous_failure::BadWalk::MalformedFinding {
            errors: walk.finding_errors().to_vec(),
            terminal: walk.terminal().clone(),
        };
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::BadWalk(badwalk),
        };
    }
    let marker = exit_signal_from_terminal(walk.terminal());
    let inputs = GateInputs {
        bd_closed: true,
        diff_empty: false,
        verify_failures: vec![],
        review_flag: None,
        streamed_findings: walk.findings().to_vec(),
        ..GateInputs::default()
    };
    let mut verdict = decide_review_phase(marker.as_ref(), inputs);
    if let PhaseVerdict::Recovery {
        cause:
            RecoveryCause::BadWalk(loom_templates::previous_failure::BadWalk::Concern {
                payload,
                parsed_findings,
            }),
    } = &mut verdict
    {
        if let TerminalSurface::Malformed {
            payload: from_term, ..
        } = walk.terminal()
        {
            *payload = from_term.clone();
        }
        if parsed_findings.is_empty() && !walk.findings().is_empty() {
            *parsed_findings = walk.findings().to_vec();
        }
    }
    suppress_review_concern(verdict, suppressions)
}

fn suppressed_findings(
    walk: &WalkOutput,
    suppressions: &[SuppressionConfig],
) -> Vec<loom_protocol::gate::Finding> {
    walk.findings()
        .iter()
        .filter(|finding| suppresses_rubric_finding(suppressions, finding))
        .cloned()
        .collect()
}

fn ineffective_suppression_matches(
    walk: &WalkOutput,
    suppressions: &[SuppressionConfig],
) -> Vec<loom_protocol::gate::Finding> {
    walk.findings()
        .iter()
        .filter(|finding| has_ineffective_suppression_match(suppressions, finding))
        .cloned()
        .collect()
}

fn all_findings_suppressed(walk: &WalkOutput, suppressions: &[SuppressionConfig]) -> bool {
    matches!(walk.terminal(), TerminalSurface::Concern { .. })
        && !walk.findings().is_empty()
        && walk
            .findings()
            .iter()
            .all(|finding| suppresses_rubric_finding(suppressions, finding))
}

fn suppress_review_concern(
    verdict: PhaseVerdict,
    suppressions: &[SuppressionConfig],
) -> PhaseVerdict {
    let PhaseVerdict::Recovery {
        cause: RecoveryCause::ReviewConcern { summary, findings },
    } = verdict
    else {
        return verdict;
    };
    let unsuppressed: Vec<_> = findings
        .into_iter()
        .filter(|finding| !suppresses_rubric_finding(suppressions, finding))
        .collect();
    if unsuppressed.is_empty() {
        PhaseVerdict::Done
    } else {
        PhaseVerdict::Recovery {
            cause: RecoveryCause::ReviewConcern {
                summary,
                findings: unsuppressed,
            },
        }
    }
}

/// Reverse [`WalkOutput::terminal`] back to the per-phase [`ExitSignal`]
/// the gate's [`decide`] table consumes. A
/// [`TerminalSurface::Malformed`] payload feeds
/// [`ExitSignal::BadWalk`] with [`BadWalk::Concern { payload, .. }`],
/// preserving the literal payload through the gate boundary.
fn exit_signal_from_terminal(terminal: &TerminalSurface) -> Option<ExitSignal> {
    match terminal {
        TerminalSurface::Complete => Some(ExitSignal::Complete),
        TerminalSurface::Noop => Some(ExitSignal::Noop),
        TerminalSurface::Blocked { reason } => Some(ExitSignal::Blocked {
            reason: reason.clone(),
        }),
        TerminalSurface::Clarify { question } => Some(ExitSignal::Clarify {
            question: question.clone(),
        }),
        TerminalSurface::Retry { reason } => Some(ExitSignal::Retry {
            reason: reason.clone(),
        }),
        TerminalSurface::Concern { summary } => Some(ExitSignal::Concern {
            summary: summary.clone(),
        }),
        TerminalSurface::Malformed { payload } => Some(ExitSignal::BadWalk(
            loom_templates::previous_failure::BadWalk::Concern {
                payload: payload.clone(),
                parsed_findings: Vec::new(),
            },
        )),
        TerminalSurface::Missing => None,
    }
}

impl<S, F, R: CommandRunner> ReviewController for ProductionReviewController<S, F, R>
where
    S: Fn(SpawnConfig) -> F + Send + Sync,
    F: std::future::Future<
            Output = Result<(SessionOutcome, Option<ExitSignal>, String), ProtocolError>,
        > + Send,
{
    async fn run_review(&mut self) -> Result<RunReviewOutput, ReviewError> {
        let built_prompt = self.build_review_prompt().await?;
        let prompt = built_prompt.prompt;
        let entry = self.manifest.lookup(&self.phase_default, self.runtime)?;
        let banner = format!("loom review @ {}", self.label);
        let scratch = loom_driver::scratch::ScratchSession::open(
            &self.workspace,
            &built_prompt.key,
            &prompt,
            &banner,
        )
        .map_err(|source| ReviewError::Protocol(ProtocolError::Io(source)))?;
        let mut spawn_config = crate::spawn::build_spawn_config(
            entry,
            self.runtime,
            self.workspace.clone(),
            prompt,
            scratch.path().to_path_buf(),
            vec![],
            vec![],
            vec![],
            launcher_key_env_for_checkout(&self.workspace)?,
        );
        let skill_session = built_prompt
            .skill_plan
            .materialize(scratch.path(), &self.workspace)?;
        spawn_config.skills = Some(skill_session.registered);
        info!(
            label = %self.label,
            image_ref = %spawn_config.image_ref,
            "loom review: dispatching reviewer agent",
        );
        let result = (self.spawn)(spawn_config).await;
        drop(scratch);
        let (outcome, marker, stdout) = result?;
        let validator = WorkspaceFindingValidator::new(&self.workspace);
        let walk = WalkOutput::from_stdout(&stdout, self.dispatch_scope, &validator);
        let suppressed_findings = suppressed_findings(&walk, &self.suppressions);
        let ineffective_suppression_matches =
            ineffective_suppression_matches(&walk, &self.suppressions);
        let suppressed_review_concern = all_findings_suppressed(&walk, &self.suppressions);
        let typed_outcome =
            classify_review_phase_with_suppressions(&walk, outcome.exit_code, &self.suppressions);
        let effective_marker =
            if matches!(typed_outcome, ReviewOutcome::Complete) && suppressed_review_concern {
                Some(ExitSignal::Complete)
            } else {
                marker.clone()
            };
        self.effective_review_marker = effective_marker.clone();
        self.suppressed_review_concern = suppressed_review_concern;
        Ok(RunReviewOutput {
            outcome: typed_outcome,
            marker: effective_marker,
            suppressed_findings,
            ineffective_suppression_matches,
        })
    }

    async fn list_spec_beads(&mut self) -> Result<Vec<Bead>, ReviewError> {
        let beads = self
            .bd
            .list(ListOpts {
                status: None,
                label: Some(self.spec_label_filter()),
                ..ListOpts::default()
            })
            .await?;
        Ok(beads)
    }

    async fn park_closed_unpushed_beads(
        &mut self,
        spec_beads: &[Bead],
        cause: PushGateRefuseCause,
    ) -> Result<Vec<BeadId>, ReviewError> {
        let Some(molecule_id) = self.resolve_molecule_id().await? else {
            return Ok(Vec::new());
        };
        let mut parked = Vec::new();
        for bead in spec_beads {
            let path = self.workspace.join(".loom/beads").join(bead.id.as_str());
            let in_molecule = bead
                .parent
                .as_ref()
                .is_some_and(|parent| parent.as_str() == molecule_id.as_str());
            if bead.status != "closed" || !in_molecule || !path.exists() {
                continue;
            }
            self.bd
                .update(
                    &bead.id,
                    UpdateOpts {
                        status: Some("blocked".to_string()),
                        add_labels: vec!["loom:blocked".to_string()],
                        notes: Some(push_refusal_note(bead.notes.as_deref(), cause)),
                        ..UpdateOpts::default()
                    },
                )
                .await?;
            parked.push(bead.id.clone());
        }
        Ok(parked)
    }

    async fn iteration_count(&mut self) -> Result<u32, ReviewError> {
        let Some(mol_id) = self.resolve_molecule_id().await? else {
            return Ok(0);
        };
        Ok(self
            .state
            .molecule(&mol_id)?
            .map(|m| m.iteration_count)
            .unwrap_or(0))
    }

    async fn set_iteration_count(&mut self, next: u32) -> Result<(), ReviewError> {
        let mol_id = self
            .resolve_molecule_id()
            .await?
            .ok_or_else(|| ReviewError::NoActiveMolecule(self.label.to_string()))?;
        self.state.set_iteration(&mol_id, next)?;
        Ok(())
    }

    async fn reset_iteration_count(&mut self) -> Result<(), ReviewError> {
        if let Some(mol_id) = self.resolve_molecule_id().await? {
            self.state.reset_iteration(&mol_id)?;
        }
        Ok(())
    }

    async fn apply_clarify(&mut self, bead: &BeadId, _reason: &str) -> Result<(), ReviewError> {
        // Verdict-gate direct-emit LOOM_CLARIFY check (specs/gate.md §
        // Options Format Contract): the bead under dispatch must carry a
        // well-formed `## Options — …` block in notes ∪ description.
        // Well-formed → loom:clarify; malformed / absent → loom:blocked
        // with cause `clarify-without-options`.
        crate::gate_clarify::apply_clarify_or_blocked(&self.bd, bead).await?;
        Ok(())
    }

    async fn integrity_findings(&mut self) -> Result<Vec<IntegrityFinding>, ReviewError> {
        self.molecule_integrity_findings().await
    }

    async fn apply_integrity_clarify(
        &mut self,
        findings: &[IntegrityFinding],
    ) -> Result<(), ReviewError> {
        if findings.is_empty() {
            return Ok(());
        }
        let Some(epic) = self.molecule_epic_bead().await? else {
            warn!(
                label = %self.label,
                "integrity findings present but no active molecule epic found",
            );
            return Ok(());
        };
        let notes = compose_clarify_options(findings);
        self.bd
            .update(
                &epic.id,
                UpdateOpts {
                    status: Some("blocked".to_string()),
                    add_labels: vec!["loom:clarify".to_string()],
                    notes: Some(notes),
                    ..UpdateOpts::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn mint_integrity_findings(
        &mut self,
        findings: &[IntegrityFinding],
    ) -> Result<(), ReviewError> {
        if findings.is_empty() {
            return Ok(());
        }
        let git = GitClient::open(&self.workspace)
            .map_err(|e| ReviewError::Io(std::io::Error::other(e.to_string())))?;
        let head = git
            .head_commit_sha()
            .await
            .map_err(|e| ReviewError::Io(std::io::Error::other(e.to_string())))?;
        let summary = crate::mint::mint_integrity_recovery(&self.bd, findings, head.as_str()).await;
        if summary.refused > 0 || summary.errors > 0 {
            warn!(
                label = %self.label,
                refused = summary.refused,
                errors = summary.errors,
                "integrity-recovery mint reported refused/errored batches",
            );
        }
        Ok(())
    }

    async fn mint_marker(&mut self) -> Result<(), ReviewError> {
        // Best-effort: specs/harness.md § Verdict Gate — mint failures fall
        // through to prek's slow tier, never abort the push.
        let review_log_path = self.resolve_review_log_for_marker();
        if let (Some(path), Some(range)) = (review_log_path.as_deref(), self.push_range.as_ref()) {
            let tree =
                loom_driver::git::head_tree_oid_sync(&self.workspace.join(".loom/integration"))
                    .map(|oid| oid.to_string())
                    .unwrap_or_else(|_| String::new());
            let marker = self
                .effective_review_marker
                .clone()
                .unwrap_or(ExitSignal::Complete);
            let config_digest = pre_commit_config_digest(&self.workspace)?;
            append_gate_run_lifecycle_events(
                path,
                &GateRun::successful_review(
                    range.clone(),
                    tree,
                    config_digest,
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
        evidence.review_marker = self
            .effective_review_marker
            .clone()
            .or(Some(ExitSignal::Complete));
        evidence.review_exit = Some(0);
        evidence.suppressed_review_concern = self.suppressed_review_concern;
        if let Some(path) = review_log_path.filter(|p| p.exists())
            && !evidence
                .gate_log_paths
                .iter()
                .any(|candidate| candidate == &path)
        {
            evidence.gate_log_paths.push(path);
        }
        let success = match GateSuccess::new(&evidence, 1) {
            Ok(success) => success,
            Err(fail) => {
                warn!(
                    label = %self.label,
                    reason = ?fail.reason,
                    "marker mint skipped: gate-success evidence incomplete — \
                     prek pre-push falls through to slow tier",
                );
                return Ok(());
            }
        };
        let clock = SystemClock::new();
        match MarkerProof::mint(success, &self.workspace, &clock) {
            Ok(_) => info!(
                label = %self.label,
                workspace = %self.workspace.display(),
                "marker minted: .loom/marker.json",
            ),
            Err(error) => warn!(
                %error,
                label = %self.label,
                "marker mint failed — prek pre-push falls through to slow tier",
            ),
        }
        Ok(())
    }

    async fn git_push(&mut self) -> Result<(), ReviewError> {
        let client = GitClient::open_with_integration_branch(
            &self.workspace,
            self.integration_branch.clone(),
        )
        .map_err(|e| ReviewError::GitPushFailed(e.to_string()))?
        .with_hook_timeout(self.hook_timeout);
        client
            .push()
            .await
            .map_err(|e| ReviewError::GitPushFailed(e.to_string()))?;
        Ok(())
    }

    async fn beads_push(&mut self) -> Result<(), ReviewError> {
        let output = self.beads_push_command().output().await?;
        if !output.status.success() {
            return Err(ReviewError::BeadsPushFailed(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        Ok(())
    }

    async fn show_bead(&mut self, id: &BeadId) -> Result<Option<Bead>, ReviewError> {
        match self.bd.show(id).await {
            Ok(bead) => Ok(Some(bead)),
            Err(BdError::ShowEmpty) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn list_children(&mut self, parent: &BeadId) -> Result<Vec<Bead>, ReviewError> {
        let beads = self
            .bd
            .list(ListOpts {
                parent: Some(parent.clone()),
                ..ListOpts::default()
            })
            .await?;
        Ok(beads)
    }

    async fn close_bead(&mut self, id: &BeadId, reason: &str) -> Result<(), ReviewError> {
        self.bd.close(id, Some(reason)).await?;
        Ok(())
    }

    async fn exec_run(&mut self) -> Result<(), ReviewError> {
        // Release the work-root lock before spawning the child — `loom loop`
        // acquires the same lock and would otherwise time out behind us.
        self.lock.take();
        let status = Command::new(&self.loom_bin)
            .current_dir(&self.workspace)
            .arg("run")
            .arg("-s")
            .arg(self.label.as_str())
            .status()
            .await?;
        if !status.success() {
            return Err(ReviewError::RunHandoff(status.to_string()));
        }
        Ok(())
    }

    fn emit_driver_event(&mut self, kind: DriverKind, summary: &str, payload: serde_json::Value) {
        // Open a transient LogSink at the same phase log path the
        // reviewer agent's sink uses (same `when`, same `phase_log_root`,
        // no renderer), write one `DriverEvent`, finish. The file is
        // opened in append mode so co-writing with the agent-event sink
        // lands both event streams in one file. When no phase log is
        // configured (test fakes, sink-less callers) this is a silent
        // no-op.
        let Some(logs_root) = self.phase_log_root.clone() else {
            return;
        };
        let mut guard = match self.envelope_builder.lock() {
            Ok(g) => g,
            Err(_) => {
                warn!("review controller: envelope builder mutex poisoned");
                return;
            }
        };
        if guard.is_none() {
            let session_ms = self
                .phase_log_when
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis());
            let clock = SystemClock::new();
            *guard = Some(EnvelopeBuilder::new(
                SessionScope::phase(SessionId::new(format!("review-{session_ms}")), None),
                Source::Driver,
                move || {
                    clock
                        .wall_now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_millis() as i64)
                },
            ));
        }
        // Lazy-init above guarantees `guard` is `Some` here; fall back
        // to a silent no-op if a future refactor breaks that invariant
        // rather than panicking inside the verdict-gate hot path.
        let envelope = match guard.as_mut() {
            Some(builder) => builder.build(),
            None => return,
        };
        drop(guard);
        let event = AgentEvent::DriverEvent {
            envelope,
            driver_kind: kind,
            summary: summary.to_string(),
            payload,
        };
        let sink_result =
            LogSink::open_phase_at(&logs_root, &self.label, "review", None, self.phase_log_when);
        match sink_result {
            Ok(mut sink) => {
                if let Err(e) = sink.emit(&event) {
                    warn!(error = %e, "review controller: emit driver event failed");
                }
                // Finish is idempotent — the agent-event sink (opened
                // separately in run_review) reaches the same file and
                // will run finish itself with the bead outcome.
                let _ = sink.finish(BeadOutcome::Done);
            }
            Err(e) => {
                warn!(error = %e, "review controller: open phase sink for driver event failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::finding::{ConcernToken, Finding, FindingTarget};
    use crate::review::runner::ReviewController;
    use loom_driver::bd::RunOutput;
    use loom_driver::identifier::MoleculeId;
    use loom_driver::state::ActiveMolecule;
    use std::ffi::OsStr;
    use std::future::Ready;

    type SpawnFuture = Ready<Result<(SessionOutcome, Option<ExitSignal>, String), ProtocolError>>;
    type NoopSpawn = fn(SpawnConfig) -> SpawnFuture;
    type NoopController = ProductionReviewController<NoopSpawn, SpawnFuture, ScriptedBd>;

    fn noop_spawn(_cfg: SpawnConfig) -> SpawnFuture {
        std::future::ready(Ok((
            SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            Some(ExitSignal::Complete),
            "LOOM_COMPLETE\n".to_string(),
        )))
    }

    /// FR12 — `loom review`'s phase-end MUST route the reviewer's marker
    /// through the canonical [`decide_for_phase`] gate function rather than its own
    /// ad-hoc `match` on `exit_code`. This test pins the marker → outcome
    /// mapping that `decide_for_phase(..., Review)` produces for the review phase:
    /// `COMPLETE` reaches `Complete`, `BLOCKED` surfaces as `Incomplete`,
    /// direct `CLARIFY` is rejected as the wrong review path, and a missing
    /// marker routes to `swallowed-marker` recovery (mapped to `Incomplete`). Combined
    /// with the source-level `decide_for_phase()` import in `classify_review_phase`,
    /// the two together fence the FR12 contract.
    fn walk_with_terminal(terminal: TerminalSurface) -> WalkOutput {
        let stdout = match terminal {
            TerminalSurface::Complete => "LOOM_COMPLETE\n".to_string(),
            TerminalSurface::Noop => "LOOM_NOOP\n".to_string(),
            TerminalSurface::Blocked { reason } => format!("{reason}\nLOOM_BLOCKED\n"),
            TerminalSurface::Clarify { question } => format!("{question}\nLOOM_CLARIFY\n"),
            TerminalSurface::Retry { reason } => format!("{reason}\nLOOM_RETRY\n"),
            TerminalSurface::Concern { summary } => {
                let json = serde_json::json!({ "summary": summary }).to_string();
                format!("LOOM_CONCERN: {json}\n")
            }
            TerminalSurface::Malformed { payload } => format!("LOOM_CONCERN: {payload}\n"),
            TerminalSurface::Missing => "no terminal marker on this line\n".to_string(),
        };
        WalkOutput::from_stdout(&stdout, DispatchScope::PerBead, &AcceptAllFindingValidator)
    }

    #[test]
    fn classify_review_phase_routes_marker_through_phase_verdict_decide_for_review_phase() {
        // `COMPLETE` + clean exit → review phase passes.
        assert_eq!(
            classify_review_phase(&walk_with_terminal(TerminalSurface::Complete), 0),
            ReviewOutcome::Complete,
        );
        // `BLOCKED` self-report surfaces as `Incomplete` carrying the marker.
        match classify_review_phase(
            &walk_with_terminal(TerminalSurface::Blocked {
                reason: "missing schema".into(),
            }),
            0,
        ) {
            ReviewOutcome::Incomplete { detail } => assert!(
                detail.contains("LOOM_BLOCKED") && detail.contains("missing schema"),
                "blocked detail missing reason: {detail}",
            ),
            other => panic!("expected Incomplete, got {other:?}"),
        }
        // `RETRY` routes to an incomplete recovery outcome instead of a
        // clean empty review.
        match classify_review_phase(
            &walk_with_terminal(TerminalSurface::Retry {
                reason: "logs disappeared".into(),
            }),
            0,
        ) {
            ReviewOutcome::Incomplete { detail } => assert!(
                detail.contains("agent-retry"),
                "retry detail should name recovery cause: {detail}",
            ),
            other => panic!("expected Incomplete, got {other:?}"),
        }
        // Direct `CLARIFY` is the wrong review path: review clarifications
        // must route through finding evidence and terminate with CONCERN.
        match classify_review_phase(
            &walk_with_terminal(TerminalSurface::Clarify {
                question: "additive only?".into(),
            }),
            0,
        ) {
            ReviewOutcome::Incomplete { detail } => assert!(
                detail.contains("wrong-review-path")
                    && detail.contains("LOOM_CLARIFY")
                    && detail.contains("route=\"clarify\"")
                    && detail.contains("LOOM_CONCERN"),
                "clarify detail should explain the review-only route: {detail}",
            ),
            other => panic!("expected Incomplete, got {other:?}"),
        }
        // Missing terminal → `Recovery::SwallowedMarker` → `Incomplete` carrying
        // the swallowed-marker phrasing.
        match classify_review_phase(&walk_with_terminal(TerminalSurface::Missing), 0) {
            ReviewOutcome::Incomplete { detail } => assert!(
                detail.contains("swallowed marker"),
                "swallowed-marker text missing: {detail}",
            ),
            other => panic!("expected Incomplete, got {other:?}"),
        }
        // Missing + non-zero exit → exit code surfaces in detail.
        match classify_review_phase(&walk_with_terminal(TerminalSurface::Missing), 7) {
            ReviewOutcome::Incomplete { detail } => assert!(
                detail.contains('7'),
                "exit code missing from detail: {detail}",
            ),
            other => panic!("expected Incomplete, got {other:?}"),
        }
    }

    /// `LOOM_CONCERN` with zero streamed findings trips the pairing rule
    /// from `specs/gate.md` § Streaming + terminator pairing rule and
    /// routes through [`RecoveryCause::BadWalk`]. The classifier renders
    /// the typed `BadWalk` variant into the `Incomplete` detail so the
    /// human sees the summary that was emitted without an accompanying
    /// findings stream.
    #[test]
    fn classify_review_phase_concern_without_findings_routes_through_bad_walk() {
        let walk = walk_with_terminal(TerminalSurface::Concern {
            summary: "scope drift around the mint pipeline".into(),
        });
        match classify_review_phase(&walk, 0) {
            ReviewOutcome::Incomplete { detail } => {
                assert!(
                    detail.contains("LOOM_CONCERN"),
                    "BadWalk framing missing from detail: {detail}",
                );
                assert!(
                    detail.contains("scope drift around the mint pipeline"),
                    "summary missing from detail: {detail}",
                );
                assert!(
                    detail.contains("LOOM_FINDING"),
                    "pairing-rule cue missing from detail: {detail}",
                );
            }
            other => panic!("expected Incomplete, got {other:?}"),
        }
    }

    /// Compile-time signature pin (criterion
    /// `classify_review_phase_signature_requires_typed_walk_output`):
    /// the classifier consumes `&WalkOutput`, not raw `&str` or
    /// `Option<&ExitSignal>`. The function-pointer assignment below is
    /// the load-bearing assertion — if the signature ever drifts to
    /// accept raw stdout, this fails to compile.
    #[test]
    fn classify_review_phase_signature_requires_typed_walk_output() {
        let _: fn(&WalkOutput, i32) -> ReviewOutcome = classify_review_phase;
    }

    #[test]
    fn all_suppressed_concern_walk_exits_clean_after_shape_validation() {
        let finding_line = r#"LOOM_FINDING: {"token":"verifier-bypass","route":"deferred","bonds":["harness"],"target":{"kind":"Annotation","target_string":"cargo test --lib sample"},"evidence":"test mocks the agent backend"}"#;
        let concern_line = r#"LOOM_CONCERN: {"summary":"reviewer flagged a verifier-bypass"}"#;
        let stdout = format!("{finding_line}\n{concern_line}\n");
        let walk =
            WalkOutput::from_stdout(&stdout, DispatchScope::PerBead, &AcceptAllFindingValidator);
        let suppression = SuppressionConfig {
            id: Some(walk.findings()[0].id()),
            hash: None,
            reason: "false positive".to_owned(),
        };
        assert_eq!(
            classify_review_phase_with_suppressions(&walk, 0, &[suppression]),
            ReviewOutcome::Complete,
        );

        let mismatched_stdout = format!("{finding_line}\nLOOM_COMPLETE\n");
        let mismatched = WalkOutput::from_stdout(
            &mismatched_stdout,
            DispatchScope::PerBead,
            &AcceptAllFindingValidator,
        );
        let suppression = SuppressionConfig {
            id: Some(mismatched.findings()[0].id()),
            hash: None,
            reason: "false positive".to_owned(),
        };
        assert!(
            matches!(
                phase_verdict_from_walk_with_suppressions(&mismatched, &[suppression]),
                PhaseVerdict::Recovery {
                    cause: RecoveryCause::BadWalk(_)
                }
            ),
            "suppression must not forgive malformed stream/terminator pairing",
        );
    }

    /// Criterion
    /// `classify_review_phase_invokes_parse_walk_output_and_threads_findings_through_gate_inputs`:
    /// the molecule-completion review path runs `parse_walk_output`
    /// (via [`WalkOutput::from_stdout`]) against the agent's combined
    /// stdout and threads the resulting `Vec<Finding>` through
    /// `GateInputs`. A well-formed `LOOM_CONCERN` paired with one or
    /// more streamed `LOOM_FINDING:` lines MUST route to
    /// `RecoveryCause::ReviewConcern { summary, findings }` (rendered
    /// via [`PreviousFailure::ReviewConcern`]), NOT
    /// `BadWalk::ConcernWithoutFindings`. Without the wiring, the
    /// `..GateInputs::default()` shape leaves the streamed-findings vec
    /// empty and the `LOOM_CONCERN` collapses to the
    /// `ConcernWithoutFindings` BadWalk variant.
    #[test]
    fn classify_review_phase_invokes_parse_walk_output_and_threads_findings_through_gate_inputs() {
        let finding_line = r#"LOOM_FINDING: {"token":"verifier-bypass","route":"deferred","bonds":["harness"],"target":{"kind":"Annotation","target_string":"cargo test --lib sample"},"evidence":"test mocks the agent backend"}"#;
        let concern_line = r#"LOOM_CONCERN: {"summary":"reviewer flagged a verifier-bypass"}"#;
        let stdout = format!("{finding_line}\n{concern_line}\n");
        let walk =
            WalkOutput::from_stdout(&stdout, DispatchScope::PerBead, &AcceptAllFindingValidator);
        assert_eq!(
            walk.findings().len(),
            1,
            "parse_walk_output must thread the streamed finding through WalkOutput",
        );
        match classify_review_phase(&walk, 0) {
            ReviewOutcome::Incomplete { detail } => {
                assert!(
                    detail.contains("reviewer flagged a verifier-bypass"),
                    "ReviewConcern summary missing from detail: {detail}",
                );
                assert!(
                    detail.contains("verifier-bypass"),
                    "ReviewConcern finding token missing from detail: {detail}",
                );
                assert!(
                    detail.contains("test mocks the agent backend"),
                    "ReviewConcern finding evidence missing from detail: {detail}",
                );
                assert!(
                    !detail.contains("ConcernWithoutFindings"),
                    "must NOT collapse to BadWalk::ConcernWithoutFindings — \
                     streamed_findings were threaded through GateInputs: {detail}",
                );
            }
            other => panic!("expected Incomplete (ReviewConcern), got {other:?}"),
        }
    }

    /// Behavioral matrix for the verification surface: the 24-cell
    /// (stream-shape ×
    /// terminal-shape) cross-product is the load-bearing class
    /// coverage for the wire-format pipeline. Every cell asserts
    /// (a) the typed [`PhaseVerdict`] variant, (b) the maximum-context
    /// preservation invariant — every parseable Finding and every
    /// well-formed terminal surface that the variant can structurally
    /// carry, does — and (c) the `Display for PreviousFailure`
    /// rendering is non-empty and references both pieces when both
    /// are structurally present.
    #[test]
    fn walk_output_failure_matrix_routes_every_cell_with_typed_outcome_and_preserves_max_context() {
        use loom_templates::previous_failure::BadWalk;

        for cell in matrix_cells() {
            let walk = WalkOutput::from_stdout(
                &cell.stdout,
                DispatchScope::PerBead,
                &AcceptAllFindingValidator,
            );
            assert_eq!(
                walk.findings().len(),
                cell.expected_well_formed_findings,
                "[{}] WalkOutput.findings count: stdout={:?}",
                cell.name,
                cell.stdout,
            );
            assert_eq!(
                walk.finding_errors().len(),
                cell.expected_malformed_findings,
                "[{}] WalkOutput.finding_errors count: stdout={:?}",
                cell.name,
                cell.stdout,
            );
            let verdict = phase_verdict_from_walk(&walk);
            match (&cell.expect, &verdict) {
                (CellExpect::Done, PhaseVerdict::Done) => {}
                (
                    CellExpect::SwallowedMarker,
                    PhaseVerdict::Recovery {
                        cause: RecoveryCause::SwallowedMarker,
                    },
                ) => {}
                (
                    CellExpect::ConcernWithoutFindings { summary: expected },
                    PhaseVerdict::Recovery {
                        cause: RecoveryCause::BadWalk(BadWalk::ConcernWithoutFindings { summary }),
                    },
                ) => {
                    assert_eq!(summary, expected, "[{}]", cell.name);
                }
                (
                    CellExpect::BadWalkConcern {
                        payload: expected_payload,
                        parsed_findings_tokens,
                    },
                    PhaseVerdict::Recovery {
                        cause:
                            RecoveryCause::BadWalk(BadWalk::Concern {
                                payload,
                                parsed_findings,
                            }),
                    },
                ) => {
                    assert_eq!(payload, expected_payload, "[{}] payload", cell.name);
                    let tokens: Vec<_> = parsed_findings.iter().map(|f| f.token).collect();
                    assert_eq!(
                        &tokens, parsed_findings_tokens,
                        "[{}] parsed_findings tokens",
                        cell.name,
                    );
                }
                (
                    CellExpect::FindingsWithoutConcern {
                        finding_tokens: expected,
                    },
                    PhaseVerdict::Recovery {
                        cause:
                            RecoveryCause::BadWalk(BadWalk::FindingsWithoutConcern {
                                finding_count,
                                findings,
                            }),
                    },
                ) => {
                    assert_eq!(*finding_count, expected.len(), "[{}]", cell.name);
                    let tokens: Vec<_> = findings.iter().map(|f| f.token).collect();
                    assert_eq!(&tokens, expected, "[{}]", cell.name);
                }
                (
                    CellExpect::MalformedFinding {
                        error_count,
                        terminal,
                    },
                    PhaseVerdict::Recovery {
                        cause:
                            RecoveryCause::BadWalk(BadWalk::MalformedFinding {
                                errors,
                                terminal: actual_terminal,
                            }),
                    },
                ) => {
                    assert_eq!(errors.len(), *error_count, "[{}] errors", cell.name);
                    assert_eq!(actual_terminal, terminal, "[{}] terminal", cell.name);
                }
                (
                    CellExpect::ReviewConcern {
                        summary: expected_summary,
                        finding_tokens: expected_tokens,
                    },
                    PhaseVerdict::Recovery {
                        cause: RecoveryCause::ReviewConcern { summary, findings },
                    },
                ) => {
                    assert_eq!(summary, expected_summary, "[{}] summary", cell.name);
                    let tokens: Vec<_> = findings.iter().map(|f| f.token).collect();
                    assert_eq!(&tokens, expected_tokens, "[{}] findings", cell.name);
                }
                (
                    CellExpect::WrongPhaseMarker {
                        marker_name: expected_marker,
                        phase_kind: expected_phase,
                    },
                    PhaseVerdict::Recovery {
                        cause:
                            RecoveryCause::WrongPhaseMarker {
                                marker_name,
                                phase_kind,
                            },
                    },
                ) => {
                    assert_eq!(marker_name, expected_marker, "[{}] marker", cell.name);
                    assert_eq!(phase_kind, expected_phase, "[{}] phase", cell.name);
                }
                (_, actual) => panic!(
                    "[{}] verdict mismatch: expected {:?}, got {actual:?}",
                    cell.name, cell.expect,
                ),
            }
            if let Some(rendered) = render_for_display_check(&verdict) {
                assert!(
                    !rendered.is_empty(),
                    "[{}] Display rendering is empty: verdict={verdict:?}",
                    cell.name,
                );
                for needle in &cell.display_contains {
                    assert!(
                        rendered.contains(needle.as_str()),
                        "[{}] Display rendering missing {needle:?}: rendered={rendered}",
                        cell.name,
                    );
                }
            }
            for token in &cell.both_pieces_tokens {
                let rendered = render_for_display_check(&verdict)
                    .unwrap_or_else(|| panic!("[{}] expected renderable variant", cell.name));
                assert!(
                    rendered.contains(token.as_wire()),
                    "[{}] Display rendering must reference finding token {} (both pieces present): \
                     rendered={rendered}",
                    cell.name,
                    token.as_wire(),
                );
            }
        }
    }

    #[derive(Debug)]
    struct MatrixCell {
        name: &'static str,
        stdout: String,
        expected_well_formed_findings: usize,
        expected_malformed_findings: usize,
        expect: CellExpect,
        display_contains: Vec<String>,
        both_pieces_tokens: Vec<loom_templates::finding::ConcernToken>,
    }

    #[derive(Debug)]
    enum CellExpect {
        Done,
        SwallowedMarker,
        ConcernWithoutFindings {
            summary: String,
        },
        BadWalkConcern {
            payload: String,
            parsed_findings_tokens: Vec<loom_templates::finding::ConcernToken>,
        },
        FindingsWithoutConcern {
            finding_tokens: Vec<loom_templates::finding::ConcernToken>,
        },
        MalformedFinding {
            error_count: usize,
            terminal: TerminalSurface,
        },
        ReviewConcern {
            summary: String,
            finding_tokens: Vec<loom_templates::finding::ConcernToken>,
        },
        WrongPhaseMarker {
            marker_name: &'static str,
            phase_kind: &'static str,
        },
    }

    /// Render the variant through `Display for PreviousFailure` for the
    /// matrix's check (c). `PhaseVerdict::Done` and `SwallowedMarker`
    /// have no `PreviousFailure` mapping at this layer, so the matrix
    /// skips Display rendering for those cells.
    fn render_for_display_check(verdict: &PhaseVerdict) -> Option<String> {
        match verdict {
            PhaseVerdict::Done => None,
            PhaseVerdict::Blocked { reason } => Some(format!("LOOM_BLOCKED: {reason}")),
            PhaseVerdict::Clarify { question } => Some(format!("LOOM_CLARIFY: {question}")),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ReviewConcern { summary, findings },
            } => Some(
                PreviousFailure::ReviewConcern {
                    summary: summary.clone(),
                    findings: findings.clone(),
                }
                .to_string(),
            ),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::BadWalk(badwalk),
            } => Some(PreviousFailure::BadWalk(badwalk.clone()).to_string()),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::SwallowedMarker,
            } => None,
            PhaseVerdict::Recovery {
                cause:
                    RecoveryCause::WrongPhaseMarker {
                        marker_name,
                        phase_kind,
                    },
            } => Some(wrong_phase_marker_detail(marker_name, phase_kind)),
            PhaseVerdict::Recovery { .. } => None,
        }
    }

    const FINDING_F1_LINE: &str = r#"LOOM_FINDING: {"token":"verifier-bypass","route":"deferred","bonds":["gate"],"target":{"kind":"Annotation","target_string":"cargo test --lib f1"},"evidence":"first finding"}"#;
    const FINDING_F2_LINE: &str = r#"LOOM_FINDING: {"token":"weak-assertion","route":"deferred","bonds":["gate"],"target":{"kind":"Annotation","target_string":"cargo test --lib f2"},"evidence":"second finding"}"#;
    const BACKTICK_BAD_FINDING_LINE_A: &str = "`LOOM_FINDING: {not valid json A}`";
    const BACKTICK_BAD_FINDING_LINE_B: &str = "`LOOM_FINDING: {not valid json B}`";

    const T_COMPLETE: &str = "LOOM_COMPLETE";
    const T_NOOP: &str = "LOOM_NOOP";
    const T_CONCERN_OK_PAYLOAD: &str = r#"{"summary":"valid summary text"}"#;
    const T_CONCERN_LEGACY_PAYLOAD: &str = "verifier-bypass -- legacy free form";
    const T_CONCERN_MALFORMED_PAYLOAD: &str = r#"{"summary":""}"#;
    const T_MISSING_TAIL: &str = "trailing prose without a marker";

    fn stream_lines(name: &str) -> (Vec<&'static str>, usize, usize) {
        match name {
            "S0" => (vec![], 0, 0),
            "S1" => (vec![FINDING_F1_LINE, FINDING_F2_LINE], 2, 0),
            "S2" => (vec![FINDING_F1_LINE, BACKTICK_BAD_FINDING_LINE_A], 1, 1),
            "S3" => (
                vec![BACKTICK_BAD_FINDING_LINE_A, BACKTICK_BAD_FINDING_LINE_B],
                0,
                2,
            ),
            other => panic!("unknown stream label: {other}"),
        }
    }

    fn terminal_tail(name: &str) -> String {
        match name {
            "T_complete" => T_COMPLETE.to_string(),
            "T_noop" => T_NOOP.to_string(),
            "T_concern_ok" => format!("LOOM_CONCERN: {T_CONCERN_OK_PAYLOAD}"),
            "T_concern_legacy" => format!("LOOM_CONCERN: {T_CONCERN_LEGACY_PAYLOAD}"),
            "T_concern_malformed" => format!("LOOM_CONCERN: {T_CONCERN_MALFORMED_PAYLOAD}"),
            "T_missing" => T_MISSING_TAIL.to_string(),
            other => panic!("unknown terminal label: {other}"),
        }
    }

    fn make_stdout(stream: &str, terminal: &str) -> String {
        let (lines, _, _) = stream_lines(stream);
        let mut out = String::new();
        for l in lines {
            out.push_str(l);
            out.push('\n');
        }
        out.push_str(&terminal_tail(terminal));
        out.push('\n');
        out
    }

    fn matrix_cells() -> Vec<MatrixCell> {
        use loom_templates::finding::ConcernToken;
        let f1 = ConcernToken::VerifierBypass;
        let f2 = ConcernToken::WeakAssertion;
        let mut cells = Vec::with_capacity(24);

        // --- S0: zero LOOM_FINDING lines. ---
        cells.push(MatrixCell {
            name: "S0×T_complete",
            stdout: make_stdout("S0", "T_complete"),
            expected_well_formed_findings: 0,
            expected_malformed_findings: 0,
            expect: CellExpect::Done,
            display_contains: vec![],
            both_pieces_tokens: vec![],
        });
        cells.push(MatrixCell {
            name: "S0×T_noop",
            stdout: make_stdout("S0", "T_noop"),
            expected_well_formed_findings: 0,
            expected_malformed_findings: 0,
            expect: CellExpect::WrongPhaseMarker {
                marker_name: "LOOM_NOOP",
                phase_kind: "review",
            },
            display_contains: vec!["wrong-review-path".to_string(), "LOOM_NOOP".to_string()],
            both_pieces_tokens: vec![],
        });
        cells.push(MatrixCell {
            name: "S0×T_concern_ok",
            stdout: make_stdout("S0", "T_concern_ok"),
            expected_well_formed_findings: 0,
            expected_malformed_findings: 0,
            expect: CellExpect::ConcernWithoutFindings {
                summary: "valid summary text".to_string(),
            },
            display_contains: vec!["valid summary text".to_string()],
            both_pieces_tokens: vec![],
        });
        cells.push(MatrixCell {
            name: "S0×T_concern_legacy",
            stdout: make_stdout("S0", "T_concern_legacy"),
            expected_well_formed_findings: 0,
            expected_malformed_findings: 0,
            expect: CellExpect::BadWalkConcern {
                payload: T_CONCERN_LEGACY_PAYLOAD.to_string(),
                parsed_findings_tokens: vec![],
            },
            display_contains: vec![T_CONCERN_LEGACY_PAYLOAD.to_string()],
            both_pieces_tokens: vec![],
        });
        cells.push(MatrixCell {
            name: "S0×T_concern_malformed",
            stdout: make_stdout("S0", "T_concern_malformed"),
            expected_well_formed_findings: 0,
            expected_malformed_findings: 0,
            expect: CellExpect::BadWalkConcern {
                payload: T_CONCERN_MALFORMED_PAYLOAD.to_string(),
                parsed_findings_tokens: vec![],
            },
            display_contains: vec![T_CONCERN_MALFORMED_PAYLOAD.to_string()],
            both_pieces_tokens: vec![],
        });
        cells.push(MatrixCell {
            name: "S0×T_missing",
            stdout: make_stdout("S0", "T_missing"),
            expected_well_formed_findings: 0,
            expected_malformed_findings: 0,
            expect: CellExpect::SwallowedMarker,
            display_contains: vec![],
            both_pieces_tokens: vec![],
        });

        // --- S1: 2 well-formed findings. ---
        cells.push(MatrixCell {
            name: "S1×T_complete",
            stdout: make_stdout("S1", "T_complete"),
            expected_well_formed_findings: 2,
            expected_malformed_findings: 0,
            expect: CellExpect::FindingsWithoutConcern {
                finding_tokens: vec![f1, f2],
            },
            display_contains: vec!["LOOM_COMPLETE".to_string(), "LOOM_FINDING".to_string()],
            both_pieces_tokens: vec![f1, f2],
        });
        cells.push(MatrixCell {
            name: "S1×T_noop",
            stdout: make_stdout("S1", "T_noop"),
            expected_well_formed_findings: 2,
            expected_malformed_findings: 0,
            expect: CellExpect::WrongPhaseMarker {
                marker_name: "LOOM_NOOP",
                phase_kind: "review",
            },
            display_contains: vec!["wrong-review-path".to_string(), "LOOM_NOOP".to_string()],
            both_pieces_tokens: vec![],
        });
        cells.push(MatrixCell {
            name: "S1×T_concern_ok",
            stdout: make_stdout("S1", "T_concern_ok"),
            expected_well_formed_findings: 2,
            expected_malformed_findings: 0,
            expect: CellExpect::ReviewConcern {
                summary: "valid summary text".to_string(),
                finding_tokens: vec![f1, f2],
            },
            display_contains: vec!["valid summary text".to_string()],
            both_pieces_tokens: vec![f1, f2],
        });
        cells.push(MatrixCell {
            name: "S1×T_concern_legacy",
            stdout: make_stdout("S1", "T_concern_legacy"),
            expected_well_formed_findings: 2,
            expected_malformed_findings: 0,
            expect: CellExpect::BadWalkConcern {
                payload: T_CONCERN_LEGACY_PAYLOAD.to_string(),
                parsed_findings_tokens: vec![f1, f2],
            },
            display_contains: vec![
                T_CONCERN_LEGACY_PAYLOAD.to_string(),
                "parsed cleanly".to_string(),
            ],
            both_pieces_tokens: vec![f1, f2],
        });
        cells.push(MatrixCell {
            name: "S1×T_concern_malformed",
            stdout: make_stdout("S1", "T_concern_malformed"),
            expected_well_formed_findings: 2,
            expected_malformed_findings: 0,
            expect: CellExpect::BadWalkConcern {
                payload: T_CONCERN_MALFORMED_PAYLOAD.to_string(),
                parsed_findings_tokens: vec![f1, f2],
            },
            display_contains: vec![
                T_CONCERN_MALFORMED_PAYLOAD.to_string(),
                "parsed cleanly".to_string(),
            ],
            both_pieces_tokens: vec![f1, f2],
        });
        // Missing marker always routes to SwallowedMarker per the
        // pairing-rule table; findings are dropped by spec.
        cells.push(MatrixCell {
            name: "S1×T_missing",
            stdout: make_stdout("S1", "T_missing"),
            expected_well_formed_findings: 2,
            expected_malformed_findings: 0,
            expect: CellExpect::SwallowedMarker,
            display_contains: vec![],
            both_pieces_tokens: vec![],
        });

        // --- S2: 1 well-formed + 1 malformed. ---
        for (tname, terminal_label, terminal_surface, display_extras) in [
            (
                "T_complete",
                "LOOM_COMPLETE",
                TerminalSurface::Complete,
                vec!["LOOM_COMPLETE".to_string()],
            ),
            (
                "T_noop",
                "LOOM_NOOP",
                TerminalSurface::Noop,
                vec!["LOOM_NOOP".to_string()],
            ),
            (
                "T_concern_ok",
                "LOOM_CONCERN",
                TerminalSurface::Concern {
                    summary: "valid summary text".to_string(),
                },
                vec!["LOOM_CONCERN".to_string()],
            ),
            (
                "T_concern_legacy",
                "LOOM_CONCERN: <malformed>",
                TerminalSurface::Malformed {
                    payload: T_CONCERN_LEGACY_PAYLOAD.to_string(),
                },
                vec![T_CONCERN_LEGACY_PAYLOAD.to_string()],
            ),
            (
                "T_concern_malformed",
                "LOOM_CONCERN: <malformed>",
                TerminalSurface::Malformed {
                    payload: T_CONCERN_MALFORMED_PAYLOAD.to_string(),
                },
                vec![T_CONCERN_MALFORMED_PAYLOAD.to_string()],
            ),
            (
                "T_missing",
                "no terminal on the final non-empty line",
                TerminalSurface::Missing,
                vec![],
            ),
        ] {
            let _ = terminal_label;
            let mut needles = vec!["not valid json".to_string()];
            needles.extend(display_extras.clone());
            cells.push(MatrixCell {
                name: leak_cell_name(&format!("S2×{tname}")),
                stdout: make_stdout("S2", tname),
                expected_well_formed_findings: 1,
                expected_malformed_findings: 1,
                expect: CellExpect::MalformedFinding {
                    error_count: 1,
                    terminal: terminal_surface,
                },
                display_contains: needles,
                both_pieces_tokens: vec![],
            });
        }

        // --- S3: all malformed. ---
        for (tname, terminal_surface, display_extras) in [
            (
                "T_complete",
                TerminalSurface::Complete,
                vec!["LOOM_COMPLETE".to_string()],
            ),
            (
                "T_noop",
                TerminalSurface::Noop,
                vec!["LOOM_NOOP".to_string()],
            ),
            (
                "T_concern_ok",
                TerminalSurface::Concern {
                    summary: "valid summary text".to_string(),
                },
                vec!["LOOM_CONCERN".to_string()],
            ),
            (
                "T_concern_legacy",
                TerminalSurface::Malformed {
                    payload: T_CONCERN_LEGACY_PAYLOAD.to_string(),
                },
                vec![T_CONCERN_LEGACY_PAYLOAD.to_string()],
            ),
            (
                "T_concern_malformed",
                TerminalSurface::Malformed {
                    payload: T_CONCERN_MALFORMED_PAYLOAD.to_string(),
                },
                vec![T_CONCERN_MALFORMED_PAYLOAD.to_string()],
            ),
            ("T_missing", TerminalSurface::Missing, vec![]),
        ] {
            let mut needles = vec!["not valid json".to_string()];
            needles.extend(display_extras.clone());
            cells.push(MatrixCell {
                name: leak_cell_name(&format!("S3×{tname}")),
                stdout: make_stdout("S3", tname),
                expected_well_formed_findings: 0,
                expected_malformed_findings: 2,
                expect: CellExpect::MalformedFinding {
                    error_count: 2,
                    terminal: terminal_surface,
                },
                display_contains: needles,
                both_pieces_tokens: vec![],
            });
        }

        assert_eq!(cells.len(), 24, "matrix must cover all 24 cells");
        cells
    }

    fn leak_cell_name(s: &str) -> &'static str {
        Box::leak(s.to_owned().into_boxed_str())
    }

    fn stub_manifest(dir: &std::path::Path) -> Arc<ProfileImageManifest> {
        let body = r#"{
          "base": { "pi": { "ref": "localhost/wrix-base-pi:abc", "source": "/nix/store/aaa-image-base-pi", "source_kind": "nix-descriptor" }, "claude": { "ref": "localhost/wrix-base-claude:abc", "source": "/nix/store/aaa-image-base-claude", "source_kind": "nix-descriptor" }, "direct": { "ref": "localhost/wrix-base-direct:abc", "source": "/nix/store/aaa-image-base-direct", "source_kind": "nix-descriptor" } }
        }"#;
        let path = dir.join("profile-images.json");
        std::fs::write(&path, body).unwrap();
        Arc::new(ProfileImageManifest::from_path(&path).unwrap())
    }

    fn empty_state(workspace: &std::path::Path) -> Arc<CacheDb> {
        Arc::new(CacheDb::open(workspace.join(".loom/cache.db")).unwrap())
    }

    fn seeded_state(workspace: &std::path::Path, label: &str, mol: &str) -> Arc<CacheDb> {
        std::fs::create_dir_all(workspace.join("specs")).unwrap();
        std::fs::write(
            workspace.join(format!("specs/{label}.md")),
            format!("# {label}\n"),
        )
        .unwrap();
        let db = CacheDb::open(workspace.join(".loom/cache.db")).unwrap();
        db.rebuild(
            workspace,
            &[ActiveMolecule {
                id: MoleculeId::new(mol),
                spec_label: SpecLabel::new(label),
                base_commit: None,
            }],
        )
        .unwrap();
        Arc::new(db)
    }

    fn epic_body(id: &str, label: &str) -> Vec<u8> {
        format!(
            r#"[{{"id":"{id}","title":"molecule","status":"open","priority":2,"issue_type":"epic","description":"","labels":["spec:{label}"]}}]"#,
        )
        .into_bytes()
    }

    fn ok_stdout(stdout: Vec<u8>) -> RunOutput {
        RunOutput {
            status: 0,
            stdout,
            stderr: Vec::new(),
        }
    }

    fn no_beads_bd() -> BdClient<ScriptedBd> {
        BdClient::with_runner(ScriptedBd::new([
            ok_stdout(b"[]".to_vec()),
            ok_stdout(b"[]".to_vec()),
        ]))
    }

    fn scripted_controller(
        workspace: PathBuf,
        label: &str,
        state: Arc<CacheDb>,
        responses: impl IntoIterator<Item = Vec<u8>>,
    ) -> ProductionReviewController<NoopSpawn, SpawnFuture, ScriptedBd> {
        let manifest = stub_manifest(&workspace);
        ProductionReviewController::new(
            BdClient::with_runner(ScriptedBd::new(responses.into_iter().map(ok_stdout))),
            SpecLabel::new(label),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            noop_spawn,
        )
    }

    fn controller(workspace: PathBuf) -> NoopController {
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        ProductionReviewController::new(
            no_beads_bd(),
            SpecLabel::new("harness"),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            noop_spawn,
        )
    }

    #[test]
    fn beads_push_argv_invokes_beads_push_not_bd_dolt_push() {
        let dir = tempfile::tempdir().unwrap();
        let ctrl = controller(dir.path().to_path_buf());
        let cmd = ctrl.beads_push_command();
        let std_cmd = cmd.as_std();

        assert_eq!(
            std_cmd.get_program(),
            OsStr::new("beads-push"),
            "push gate must shell out to beads-push, not bd",
        );
        let argv: Vec<&OsStr> = std_cmd.get_args().collect();
        assert!(
            argv.is_empty(),
            "no extra args; `bd dolt push` would surface as program=bd args=[dolt, push]: argv={argv:?}",
        );
        assert_eq!(std_cmd.get_current_dir(), Some(dir.path()));
    }

    #[tokio::test]
    async fn iteration_counter_round_trips_through_cache_db() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        let state = seeded_state(workspace, "alpha", "lm-alpha");
        let epic = epic_body("lm-alpha", "alpha");
        let mut ctrl = scripted_controller(
            workspace.to_path_buf(),
            "alpha",
            state,
            // Each public method invokes one bd-find call.
            std::iter::repeat_n(epic, 5),
        );

        assert_eq!(ctrl.iteration_count().await.unwrap(), 0);
        ctrl.set_iteration_count(3).await.unwrap();
        assert_eq!(ctrl.iteration_count().await.unwrap(), 3);
        ctrl.reset_iteration_count().await.unwrap();
        assert_eq!(ctrl.iteration_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn iteration_count_is_zero_when_no_active_molecule() {
        let dir = tempfile::tempdir().unwrap();
        let state = empty_state(dir.path());
        let mut ctrl =
            scripted_controller(dir.path().to_path_buf(), "harness", state, [b"[]".to_vec()]);
        assert_eq!(ctrl.iteration_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn set_iteration_errors_when_no_active_molecule() {
        let dir = tempfile::tempdir().unwrap();
        let state = empty_state(dir.path());
        let mut ctrl =
            scripted_controller(dir.path().to_path_buf(), "harness", state, [b"[]".to_vec()]);
        let err = ctrl.set_iteration_count(1).await.unwrap_err();
        assert!(
            matches!(err, ReviewError::NoActiveMolecule(ref s) if s == "harness"),
            "expected NoActiveMolecule(harness), got {err:?}",
        );
    }

    #[tokio::test]
    async fn reset_iteration_is_no_op_when_no_active_molecule() {
        let dir = tempfile::tempdir().unwrap();
        let state = empty_state(dir.path());
        let mut ctrl =
            scripted_controller(dir.path().to_path_buf(), "harness", state, [b"[]".to_vec()]);
        ctrl.reset_iteration_count().await.unwrap();
    }

    /// `integrity_findings` returns an empty list when no molecule is
    /// active for the controller's spec — the push-gate condition trivially
    /// passes rather than surfacing an error. The four-condition AND still
    /// gates on the remaining inputs.
    #[tokio::test]
    async fn integrity_findings_empty_when_no_active_molecule() {
        let dir = tempfile::tempdir().unwrap();
        let state = empty_state(dir.path());
        let mut ctrl =
            scripted_controller(dir.path().to_path_buf(), "harness", state, [b"[]".to_vec()]);
        let findings = ctrl.integrity_findings().await.unwrap();
        assert!(findings.is_empty(), "no active molecule => no findings");
    }

    /// `integrity_findings` returns an empty list when the active molecule
    /// has no recorded `base_commit` — there is no diff range to walk so the
    /// integrity input is vacuously empty. Avoids fabricating a `HEAD..HEAD`
    /// scope that would parse every spec file in the tree.
    #[tokio::test]
    async fn integrity_findings_empty_when_molecule_lacks_base_commit() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        let state = seeded_state(workspace, "alpha", "lm-alpha");
        let epic = epic_body("lm-alpha", "alpha");
        let mut ctrl = scripted_controller(workspace.to_path_buf(), "alpha", state, [epic]);
        let findings = ctrl.integrity_findings().await.unwrap();
        assert!(findings.is_empty(), "no base_commit => no findings");
    }

    /// Spec contract `specs/gate.md` § Runners, *Runner-owned resolution*:
    /// at the push gate a `[check]` target resolves **because a runner
    /// claims it**, not because its first token is on PATH. The production
    /// controller's `molecule_integrity_findings` threads the resolved
    /// `[runner.check.<name>]` specs into the integrity gate's
    /// forward-resolution.
    ///
    /// Behavioral assertion: a `[check]` target whose `tokens[0]` is **not**
    /// on PATH but is matched by a `[runner.check]` block produces no
    /// push-gate-terminal `UnresolvedAnnotation`. With the runner specs
    /// withheld (the pre-fix `&[]` path), forward-resolution falls through
    /// to the `tokens[0]`-on-PATH probe and surfaces a push-blocking false
    /// positive, so this test pins the driver-side `check_forward` wiring.
    #[tokio::test]
    async fn molecule_integrity_resolves_runner_owned_target_without_unresolved() {
        // tokens[0] is deliberately absent from PATH; only the runner match
        // can resolve it.
        const RUNNER_OWNED_TARGET: &str = "loom-runner-only-fixture-7c3a-not-on-path";

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        loom_driver::git::init_test_repo(&workspace).expect("init repo");
        let base = loom_driver::git::sync_head_commit_sha(&workspace)
            .expect("base sha")
            .to_string();

        std::fs::write(
            workspace.join("loom.toml"),
            "[runner.check.fixture]\nmatch   = '^loom-runner-only-fixture'\ncommand = \"true {targets}\"\nparse   = \"exit-code\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(workspace.join("specs")).unwrap();
        std::fs::write(
            workspace.join("specs/alpha.md"),
            format!("# alpha\n\n- exercise runner dispatch [check]({RUNNER_OWNED_TARGET})\n"),
        )
        .unwrap();
        loom_driver::git::commit_all_in(&workspace, "add runner-owned spec").expect("commit");

        let db = CacheDb::open(workspace.join(".loom/cache.db")).unwrap();
        db.rebuild(
            &workspace,
            &[ActiveMolecule {
                id: MoleculeId::new("lm-alpha"),
                spec_label: SpecLabel::new("alpha"),
                base_commit: Some(base),
            }],
        )
        .unwrap();
        let state = Arc::new(db);

        let epic = epic_body("lm-alpha", "alpha");
        let mut ctrl = scripted_controller(workspace, "alpha", state, [epic]);

        let findings = ctrl.integrity_findings().await.unwrap();
        let offenders: Vec<&IntegrityFinding> = findings
            .iter()
            .filter(|f| {
                matches!(
                    f,
                    IntegrityFinding::UnresolvedAnnotation { target, .. }
                        if target == RUNNER_OWNED_TARGET
                )
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "runner-owned [check] target must resolve via its runner at the push gate, not surface an unresolved-annotation finding; got {offenders:?}",
        );
    }

    /// `apply_integrity_clarify` is a no-op when handed an empty findings
    /// list — there is no clarify to apply, and the controller must not
    /// query `bd` for a molecule that may not exist.
    #[tokio::test]
    async fn apply_integrity_clarify_is_noop_for_empty_findings() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctrl = controller(dir.path().to_path_buf());
        ctrl.apply_integrity_clarify(&[]).await.unwrap();
    }

    /// Seed a stub spec file at `specs/<label>.md` with an empty
    /// `## Success Criteria` section so `load_review_sources` succeeds in
    /// tests that don't exercise verify/judge bodies.
    fn seed_empty_spec(workspace: &std::path::Path, label: &str) {
        let path = workspace.join(format!("specs/{label}.md"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "## Success Criteria\n\n").unwrap();
    }

    #[tokio::test]
    async fn run_review_translates_zero_exit_into_complete() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        seed_empty_spec(&workspace, "harness");
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let mut ctrl = ProductionReviewController::new(
            no_beads_bd(),
            SpecLabel::new("harness"),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            |_cfg: SpawnConfig| async move {
                Ok((
                    SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    },
                    Some(ExitSignal::Complete),
                    "LOOM_COMPLETE\n".to_string(),
                ))
            },
        );
        let result = ctrl.run_review().await;
        if let Err(ReviewError::Bd(_)) = result {
            return;
        }
        assert!(
            matches!(
                result,
                Ok(RunReviewOutput {
                    outcome: ReviewOutcome::Complete,
                    ..
                }),
            ),
            "expected Complete, got {result:?}",
        );
    }

    #[tokio::test]
    async fn run_review_tree_scope_admits_tree_only_findings() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        seed_empty_spec(&workspace, "gate");
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let gate = SpecLabel::new("gate");
        let findings = [
            Finding {
                token: ConcernToken::TemplateSpecDrift,
                route: crate::review::FindingRoute::Deferred,
                bonds: vec![gate.clone()],
                target: FindingTarget::Template {
                    path: "crates/loom-templates/templates/review.md".to_owned(),
                },
                evidence: "tree-scope template drift".to_owned(),
            },
            Finding {
                token: ConcernToken::CrossSpecClash,
                route: crate::review::FindingRoute::Deferred,
                bonds: vec![gate.clone()],
                target: FindingTarget::Criterion {
                    spec: gate.clone(),
                    anchor: "standing-safety-net-checks".to_owned(),
                },
                evidence: "tree-scope cross-spec clash".to_owned(),
            },
            Finding {
                token: ConcernToken::SpecConventionsViolation,
                route: crate::review::FindingRoute::Deferred,
                bonds: vec![gate.clone()],
                target: FindingTarget::Criterion {
                    spec: gate,
                    anchor: "standing-safety-net-checks".to_owned(),
                },
                evidence: "tree-scope spec convention violation".to_owned(),
            },
        ];
        let finding_lines = findings
            .iter()
            .map(|finding| {
                format!(
                    "LOOM_FINDING: {}",
                    serde_json::to_string(finding).expect("finding json"),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let stdout = format!("{finding_lines}\nLOOM_CONCERN: {{\"summary\":\"tree drift\"}}\n");
        let mut ctrl = ProductionReviewController::new(
            no_beads_bd(),
            SpecLabel::new("gate"),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            move |_cfg: SpawnConfig| {
                let stdout = stdout.clone();
                async move {
                    Ok((
                        SessionOutcome {
                            exit_code: 0,
                            cost_usd: None,
                        },
                        Some(ExitSignal::Concern {
                            summary: "tree drift".to_owned(),
                        }),
                        stdout,
                    ))
                }
            },
        )
        .with_dispatch_scope(DispatchScope::Tree);
        let result = ctrl.run_review().await;
        if let Err(ReviewError::Bd(_)) = result {
            return;
        }
        match result {
            Ok(RunReviewOutput {
                outcome: ReviewOutcome::Incomplete { detail },
                ..
            }) => {
                for token in [
                    "template-spec-drift",
                    "cross-spec-clash",
                    "spec-conventions-violation",
                ] {
                    assert!(
                        detail.contains(token),
                        "tree-only finding must survive review parsing: {detail}",
                    );
                }
                assert!(
                    !detail.contains("scope mismatch"),
                    "tree-scope review must not parse as per-bead scope: {detail}",
                );
            }
            other => panic!("expected Incomplete, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_review_rejects_unresolved_finding_anchor_as_malformed_bad_walk() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        seed_empty_spec(&workspace, "gate");
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let gate = SpecLabel::new("gate");
        let finding = Finding {
            token: ConcernToken::SpecCoherenceFail,
            route: crate::review::FindingRoute::Deferred,
            bonds: vec![gate.clone()],
            target: FindingTarget::Criterion {
                spec: gate,
                anchor: "missing-anchor".to_owned(),
            },
            evidence: "anchor does not resolve".to_owned(),
        };
        let stdout = format!(
            "LOOM_FINDING: {}\nLOOM_CONCERN: {{\"summary\":\"bad anchor\"}}\n",
            serde_json::to_string(&finding).expect("finding json"),
        );
        let scripted = ScriptedBd::new([
            loom_driver::bd::RunOutput {
                status: 0,
                stdout: b"[]".to_vec(),
                stderr: Vec::new(),
            },
            loom_driver::bd::RunOutput {
                status: 0,
                stdout: b"[]".to_vec(),
                stderr: Vec::new(),
            },
        ]);
        let bd = BdClient::with_runner(scripted);
        let mut ctrl = ProductionReviewController::new(
            bd,
            SpecLabel::new("gate"),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            move |_cfg: SpawnConfig| {
                let stdout = stdout.clone();
                async move {
                    Ok((
                        SessionOutcome {
                            exit_code: 0,
                            cost_usd: None,
                        },
                        Some(ExitSignal::Concern {
                            summary: "bad anchor".to_owned(),
                        }),
                        stdout,
                    ))
                }
            },
        );
        match ctrl.run_review().await {
            Ok(RunReviewOutput {
                outcome: ReviewOutcome::Incomplete { detail },
                ..
            }) => {
                assert!(
                    detail.contains("strict validation"),
                    "unresolved anchor must route to BadWalk::MalformedFinding: {detail}",
                );
                assert!(
                    detail.contains("missing-anchor"),
                    "parse error detail must name unresolved anchor: {detail}",
                );
                assert!(
                    detail.contains("LOOM_CONCERN"),
                    "well-formed terminal must be preserved: {detail}",
                );
            }
            other => panic!("expected Incomplete, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_review_translates_nonzero_exit_into_incomplete_with_code() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        seed_empty_spec(&workspace, "harness");
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let mut ctrl = ProductionReviewController::new(
            no_beads_bd(),
            SpecLabel::new("harness"),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            |_cfg: SpawnConfig| async move {
                // No marker + non-zero exit: the gate routes via
                // SwallowedMarker, and the review classifier folds the exit
                // code into the detail body for human triage.
                Ok((
                    SessionOutcome {
                        exit_code: 7,
                        cost_usd: None,
                    },
                    None,
                    String::new(),
                ))
            },
        );
        let result = ctrl.run_review().await;
        if let Err(ReviewError::Bd(_)) = result {
            return;
        }
        match result {
            Ok(RunReviewOutput {
                outcome: ReviewOutcome::Incomplete { detail },
                ..
            }) => {
                assert!(
                    detail.contains('7'),
                    "detail should mention exit 7: {detail}"
                );
            }
            other => panic!("expected Incomplete, got {other:?}"),
        }
    }

    /// The review prompt must instruct the reviewer to walk
    /// `docs/style-rules.md` rule by rule and cite rule id + file/line for
    /// each violation. This is the load-bearing surface for style-rule
    /// conformance — `loom gate verify`'s deterministic audits cannot enforce
    /// the prose rules, so the LLM-judged rubric is the only line of defence.
    #[tokio::test]
    async fn build_review_prompt_includes_style_rule_conformance_walkthrough() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        let label = "alpha";
        seed_empty_spec(workspace, label);
        let state = empty_state(workspace);
        let manifest = stub_manifest(workspace);
        let ctrl = ProductionReviewController::new(
            no_beads_bd(),
            SpecLabel::new(label),
            PathBuf::from("/usr/bin/loom"),
            workspace.to_path_buf(),
            state,
            manifest,
            ProfileName::new("base"),
            noop_spawn,
        );
        let prompt = match ctrl.build_review_prompt().await {
            Ok(p) => p.prompt,
            Err(ReviewError::Bd(_)) => return,
            Err(e) => panic!("unexpected error: {e:?}"),
        };
        assert!(
            prompt.contains("## Style-Rule Conformance"),
            "rubric heading missing: {prompt}",
        );
        assert!(
            prompt.contains("docs/style-rules.md"),
            "style_rules path not pinned in review prompt: {prompt}",
        );
        assert!(
            prompt.contains("Discover the families")
                && prompt.contains("do not assume a fixed prefix list"),
            "family-discovery instruction missing: {prompt}",
        );
        for forbidden in ["**SH-**", "**NX-**", "**RS-**", "**COM-**", "**CLI-**"] {
            assert!(
                !prompt.contains(forbidden),
                "rule-family marker {forbidden} leaked into review prompt: {prompt}",
            );
        }
        assert!(
            prompt.contains("style-rule-violation"),
            "per-finding 'style-rule-violation' token not documented in review prompt: {prompt}",
        );
        assert!(
            prompt.contains("rule id"),
            "citation contract (rule id) not described: {prompt}",
        );
        assert!(
            prompt.contains("file and line range") || prompt.contains("file/line range"),
            "citation contract (file/line range) not described: {prompt}",
        );
    }

    #[tokio::test]
    async fn build_review_prompt_pins_dispatch_scope_without_reusing_diff_for_tree() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        let label = "alpha";
        seed_empty_spec(workspace, label);

        let diff_ctrl = ProductionReviewController::new(
            no_beads_bd(),
            SpecLabel::new(label),
            PathBuf::from("/usr/bin/loom"),
            workspace.to_path_buf(),
            empty_state(workspace),
            stub_manifest(workspace),
            ProfileName::new("base"),
            noop_spawn,
        )
        .with_push_range(Some("abc123..def456".to_owned()));
        let diff_prompt = match diff_ctrl.build_review_prompt().await {
            Ok(p) => p.prompt,
            Err(ReviewError::Bd(_)) => return,
            Err(e) => panic!("unexpected error: {e:?}"),
        };
        assert!(
            diff_prompt.contains("Current dispatch scope: `--diff abc123..def456`"),
            "diff prompt must pin the exact diff scope: {diff_prompt}",
        );
        assert!(
            diff_prompt.contains("`git diff abc123..def456 --name-only`"),
            "diff prompt must name the exact scope input set: {diff_prompt}",
        );

        let tree_ctrl = ProductionReviewController::new(
            no_beads_bd(),
            SpecLabel::new(label),
            PathBuf::from("/usr/bin/loom"),
            workspace.to_path_buf(),
            empty_state(workspace),
            stub_manifest(workspace),
            ProfileName::new("base"),
            noop_spawn,
        )
        .with_dispatch_scope(DispatchScope::Tree)
        .with_push_range(Some("abc123..def456".to_owned()));
        let tree_prompt = match tree_ctrl.build_review_prompt().await {
            Ok(p) => p.prompt,
            Err(ReviewError::Bd(_)) => return,
            Err(e) => panic!("unexpected error: {e:?}"),
        };
        assert!(
            tree_prompt.contains("Current dispatch scope: `--tree`")
                && tree_prompt.contains("every file in the workspace"),
            "tree prompt must pin whole-workspace scope: {tree_prompt}",
        );
        assert!(
            tree_prompt.contains("do not narrow review to a base-to-HEAD diff or log range"),
            "tree prompt must reject diff/log as scope: {tree_prompt}",
        );
        assert!(
            !tree_prompt.contains("`git diff abc123..def456 --name-only`"),
            "tree prompt must not reuse the diff range as its input set: {tree_prompt}",
        );
    }

    #[tokio::test]
    async fn build_review_prompt_inlines_test_and_judge_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        let label = "alpha";
        std::fs::create_dir_all(workspace.join("specs")).unwrap();
        std::fs::create_dir_all(workspace.join("tests/judges")).unwrap();
        std::fs::write(
            workspace.join(format!("specs/{label}.md")),
            "## Success Criteria\n\n\
             - one [test](../tests/alpha.sh#test_one)\n\
             - two [judge](../tests/judges/alpha.sh#judge_two)\n",
        )
        .unwrap();
        std::fs::write(workspace.join("tests/alpha.sh"), "TEST_BODY_MARKER\n").unwrap();
        std::fs::write(
            workspace.join("tests/judges/alpha.sh"),
            "JUDGE_BODY_MARKER\n",
        )
        .unwrap();
        let state = empty_state(workspace);
        let manifest = stub_manifest(workspace);
        let ctrl = ProductionReviewController::new(
            no_beads_bd(),
            SpecLabel::new(label),
            PathBuf::from("/usr/bin/loom"),
            workspace.to_path_buf(),
            state,
            manifest,
            ProfileName::new("base"),
            noop_spawn,
        );
        let prompt = match ctrl.build_review_prompt().await {
            Ok(p) => p.prompt,
            Err(ReviewError::Bd(_)) => return,
            Err(e) => panic!("unexpected error: {e:?}"),
        };
        assert!(prompt.contains("TEST_BODY_MARKER"), "{prompt}");
        assert!(prompt.contains("JUDGE_BODY_MARKER"), "{prompt}");
        assert!(prompt.contains("tests/alpha.sh"), "{prompt}");
        assert!(prompt.contains("tests/judges/alpha.sh"), "{prompt}");
    }

    /// `with_lane` plumbs the requested [`ReviewLane`] into the rendered
    /// prompt so `loom gate judge` / `loom gate rubric` invocations actually
    /// surface narrower prompts to the agent rather than silently running
    /// the full `loom gate review` template. Pins the wiring contract for
    /// the per-lane subcommands; the per-section render contract is owned
    /// by the template-level tests in `loom-templates::tests::render`.
    #[tokio::test]
    async fn build_review_prompt_honors_with_lane_judge_and_rubric() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        let label = "alpha";
        std::fs::create_dir_all(workspace.join("specs")).unwrap();
        std::fs::create_dir_all(workspace.join("tests/judges")).unwrap();
        std::fs::write(
            workspace.join(format!("specs/{label}.md")),
            "## Success Criteria\n\n\
             - one [judge](../tests/judges/alpha.sh#judge_two)\n",
        )
        .unwrap();
        std::fs::write(
            workspace.join("tests/judges/alpha.sh"),
            "JUDGE_BODY_MARKER\n",
        )
        .unwrap();

        let judge_ctrl = ProductionReviewController::new(
            no_beads_bd(),
            SpecLabel::new(label),
            PathBuf::from("/usr/bin/loom"),
            workspace.to_path_buf(),
            empty_state(workspace),
            stub_manifest(workspace),
            ProfileName::new("base"),
            noop_spawn,
        )
        .with_lane(ReviewLane::Judge);
        let judge_prompt = match judge_ctrl.build_review_prompt().await {
            Ok(p) => p.prompt,
            Err(ReviewError::Bd(_)) => return,
            Err(e) => panic!("unexpected error: {e:?}"),
        };
        assert!(
            judge_prompt.contains("JUDGE_BODY_MARKER"),
            "judge lane keeps [judge] rubric bodies: {judge_prompt}",
        );
        assert!(
            !judge_prompt.contains("## Review Dimensions"),
            "judge lane suppresses Review Dimensions: {judge_prompt}",
        );
        assert!(
            !judge_prompt.contains("## Style-Rule Conformance"),
            "judge lane suppresses style-rule walk: {judge_prompt}",
        );

        let rubric_ctrl = ProductionReviewController::new(
            no_beads_bd(),
            SpecLabel::new(label),
            PathBuf::from("/usr/bin/loom"),
            workspace.to_path_buf(),
            empty_state(workspace),
            stub_manifest(workspace),
            ProfileName::new("base"),
            noop_spawn,
        )
        .with_lane(ReviewLane::Rubric);
        let rubric_prompt = match rubric_ctrl.build_review_prompt().await {
            Ok(p) => p.prompt,
            Err(ReviewError::Bd(_)) => return,
            Err(e) => panic!("unexpected error: {e:?}"),
        };
        assert!(
            !rubric_prompt.contains("JUDGE_BODY_MARKER"),
            "rubric lane suppresses [judge] rubric bodies: {rubric_prompt}",
        );
        assert!(
            !rubric_prompt.contains("## `[judge]` Rubrics"),
            "rubric lane suppresses [judge] rubrics heading: {rubric_prompt}",
        );
        assert!(
            rubric_prompt.contains("## Review Dimensions"),
            "rubric lane keeps Review Dimensions: {rubric_prompt}",
        );
        assert!(
            rubric_prompt.contains("## Style-Rule Conformance"),
            "rubric lane keeps style-rule walk: {rubric_prompt}",
        );
    }

    /// `loom gate audit --tree` is inspection-only, so bonding targets
    /// are not minted or surfaced to the reviewer. The driver-side
    /// `loom gate mint` command consumes findings and bonds fix-ups
    /// itself. The agent's prompt only needs to stream `LOOM_FINDING:`
    /// lines naming the spec via `bonds`.
    #[tokio::test]
    async fn tree_scope_review_prompt_omits_bd_recovery_block_under_inspection_only_contract() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        let label = "alpha";
        seed_empty_spec(workspace, label);
        let state = empty_state(workspace);
        let manifest = stub_manifest(workspace);
        let ctrl = ProductionReviewController::new(
            no_beads_bd(),
            SpecLabel::new(label),
            PathBuf::from("/usr/bin/loom"),
            workspace.to_path_buf(),
            state,
            manifest,
            ProfileName::new("base"),
            noop_spawn,
        );
        let prompt = match ctrl.build_review_prompt().await {
            Ok(p) => p.prompt,
            Err(ReviewError::Bd(_)) => return,
            Err(e) => panic!("unexpected error: {e:?}"),
        };
        assert!(
            !prompt.contains("bd find --type=epic"),
            "review prompt must not instruct `bd find --type=epic` recovery — driver mints under the new contract: {prompt}",
        );
        assert!(
            !prompt.contains("```bash"),
            "review prompt must contain no bash code blocks under the inspection-only contract: {prompt}",
        );
        assert!(
            prompt.contains("LOOM_FINDING:"),
            "review prompt must document the streaming `LOOM_FINDING:` emit shape: {prompt}",
        );
    }

    /// `loom review` must dispatch with the rendered `ReviewContext`
    /// template — `# Post-Epic Review` heading, spec_path, and
    /// scratchpad path all reach the agent prompt — and the same body
    /// must land in `<scratch_dir>/prompt.txt` so post-compaction
    /// `repin.sh` can re-emit the actual phase prompt. Mirror of the
    /// run-side test in `run/production.rs`.
    #[tokio::test]
    async fn run_review_dispatches_rendered_review_template_and_writes_prompt_txt() {
        use std::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let label = "harness";
        seed_empty_spec(&workspace, label);
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let captured: Arc<Mutex<Option<SpawnConfig>>> = Arc::new(Mutex::new(None));
        let captured_for_closure = Arc::clone(&captured);
        let prompt_seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let prompt_seen_inner = Arc::clone(&prompt_seen);
        let mut ctrl = ProductionReviewController::new(
            no_beads_bd(),
            SpecLabel::new(label),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            move |cfg: SpawnConfig| {
                let captured = Arc::clone(&captured_for_closure);
                let prompt_seen = Arc::clone(&prompt_seen_inner);
                async move {
                    let txt = std::fs::read_to_string(cfg.scratch_dir.join("prompt.txt"))
                        .expect("prompt.txt readable");
                    *prompt_seen.lock().unwrap() = Some(txt);
                    *captured.lock().unwrap() = Some(cfg);
                    Ok((
                        SessionOutcome {
                            exit_code: 0,
                            cost_usd: None,
                        },
                        Some(ExitSignal::Complete),
                        "LOOM_COMPLETE\n".to_string(),
                    ))
                }
            },
        );
        let outcome = ctrl.run_review().await;
        if let Err(ReviewError::Bd(_)) = outcome {
            return;
        }
        outcome.expect("run_review ok");
        let cfg = captured.lock().unwrap().take().expect("closure called");
        assert_eq!(
            cfg.image_source_kind,
            Some(loom_driver::agent::ImageSourceKind::NixDescriptor),
            "review SpawnConfig must copy manifest source_kind for image_source overrides",
        );
        assert!(
            cfg.initial_prompt.contains("# Post-Epic Review"),
            "prompt missing template heading: {}",
            cfg.initial_prompt,
        );
        assert!(
            cfg.initial_prompt.contains("specs/harness.md"),
            "prompt missing spec path: {}",
            cfg.initial_prompt,
        );
        assert!(
            cfg.initial_prompt.contains(".loom/scratch"),
            "prompt missing scratchpad partial: {}",
            cfg.initial_prompt,
        );
        assert!(
            cfg.repin.orientation.is_empty()
                && cfg.repin.pinned_context.is_empty()
                && cfg.repin.partial_bodies.is_empty(),
            "RePinContent must be empty placeholder; rendered template lives in prompt.txt: {:?}",
            cfg.repin,
        );
        let written = prompt_seen.lock().unwrap().take().expect("prompt.txt seen");
        assert_eq!(written, cfg.initial_prompt);
    }

    /// Regression: `exec_run` (the review → run handoff for auto-iterate)
    /// must release the work-root lock before spawning, so the `loom loop`
    /// child can acquire it. Mirror of the run-side test.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_run_releases_lock_before_spawning_child() {
        use loom_driver::clock::SystemClock;
        use loom_driver::lock::LockManager;
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let state_home = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let mgr = LockManager::with_state_home(&workspace, state_home.path()).unwrap();
        let label = SpecLabel::new("alpha");
        let root = BeadId::new("lm-review").unwrap();
        let clock = SystemClock::new();
        let guard = mgr.acquire_work_root_async(&root, &clock).await.unwrap();

        // Stand-in for the `loom` binary; /bin/true is absent on NixOS.
        let stub = dir.path().join("loom-stub.sh");
        std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut ctrl = ProductionReviewController::new(
            no_beads_bd(),
            label.clone(),
            stub,
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            |_cfg: SpawnConfig| async move {
                Ok((
                    SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    },
                    Some(ExitSignal::Complete),
                    "LOOM_COMPLETE\n".to_string(),
                ))
            },
        )
        .with_handoff_lock(guard);

        ctrl.exec_run().await.expect("exec_run ok");

        let _reacquired = mgr
            .acquire_work_root_with_timeout_async(&root, &clock, Duration::from_millis(100))
            .await
            .expect("lock must be reacquirable after exec_run");
    }

    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    /// Captures argv on every `bd` invocation while returning canned
    /// stdout. Mirrors the runner in `run/production.rs` but kept local
    /// because the `tests` module is private.
    struct ScriptedBd {
        responses: StdMutex<VecDeque<RunOutput>>,
        calls: Arc<StdMutex<Vec<Vec<OsString>>>>,
    }

    impl ScriptedBd {
        fn new(responses: impl IntoIterator<Item = RunOutput>) -> Self {
            Self {
                responses: StdMutex::new(responses.into_iter().collect()),
                calls: Arc::new(StdMutex::new(Vec::new())),
            }
        }

        fn calls_handle(&self) -> Arc<StdMutex<Vec<Vec<OsString>>>> {
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
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                }))
        }
    }

    /// Build a `Bead` snapshot directly without going through `bd-shim`.
    /// specs/gate.md § "Persistence boundary: agent narrates, agent persists":
    /// when the bead under dispatch carries a well-formed `## Options — …`
    /// block, the review controller's `apply_clarify` only stamps the
    /// `loom:clarify` label — the canonical block belongs to the agent,
    /// written to bead state *before* `LOOM_CLARIFY` is emitted. If the
    /// controller also wrote the agent's reason via `bd update --notes`,
    /// every re-emit would clobber the canonical block and leave `loom
    /// inbox`'s queue empty.
    #[tokio::test]
    async fn apply_clarify_does_not_write_notes() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let well_formed = "## Options — pick a path\\n\\n### Option 1 — first\\nbody";
        let show_row = format!(
            r#"[{{"id":"lm-clarify.2","title":"t","status":"open","priority":2,"issue_type":"task","description":"{well_formed}"}}]"#,
        );
        let scripted = ScriptedBd::new([
            RunOutput {
                status: 0,
                stdout: show_row.into_bytes(),
                stderr: Vec::new(),
            },
            RunOutput {
                status: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        ]);
        let calls = scripted.calls_handle();
        let bd = BdClient::with_runner(scripted);
        let mut ctrl = ProductionReviewController::new(
            bd,
            SpecLabel::new("gate"),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            noop_spawn,
        );
        let bead_id = BeadId::new("lm-clarify.2").expect("bead id");
        ctrl.apply_clarify(&bead_id, "iteration-cap escalation reason")
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
        assert_eq!(update_argv[1], "lm-clarify.2");
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
            "review apply_clarify must pair --status blocked with --add-label so \
             `bd ready` excludes via its native status filter: {update_argv:?}",
        );
    }

    /// Dedup contract: `apply_integrity_clarify` (push-gate path that
    /// promotes integrity findings into a clarify) must also pair
    /// `--status blocked` with `--add-label loom:clarify` on the molecule
    /// epic, so the epic falls out of `bd ready` for both the epic-owning
    /// spec and any spec whose ready queue would otherwise pick it up.
    #[tokio::test]
    async fn apply_integrity_clarify_pairs_status_blocked_with_add_label() {
        use loom_gate::IntegrityFinding;

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        seed_empty_spec(&workspace, "gate");
        let state = seeded_state(&workspace, "gate", "lm-mol.1");
        let manifest = stub_manifest(&workspace);

        let show_body = br#"[{
            "id": "lm-mol.1",
            "title": "epic",
            "status": "open",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["spec:gate"]
        }]"#;
        let scripted = ScriptedBd::new([
            RunOutput {
                status: 0,
                stdout: show_body.to_vec(), // bd list (resolve_open_epic)
                stderr: Vec::new(),
            },
            RunOutput {
                status: 0,
                stdout: show_body.to_vec(), // bd show
                stderr: Vec::new(),
            },
            RunOutput {
                status: 0,
                stdout: Vec::new(), // bd update
                stderr: Vec::new(),
            },
        ]);
        let calls = scripted.calls_handle();
        let bd = BdClient::with_runner(scripted);
        let mut ctrl = ProductionReviewController::new(
            bd,
            SpecLabel::new("gate"),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            noop_spawn,
        );
        let findings = vec![IntegrityFinding::UnresolvedAnnotation {
            spec: std::path::PathBuf::from("specs/gate.md"),
            line: 1,
            tier: loom_gate::annotation::Tier::Check,
            target: "nonexistent-binary".to_string(),
        }];
        ctrl.apply_integrity_clarify(&findings)
            .await
            .expect("apply_integrity_clarify ok");

        let captured = calls.lock().unwrap();
        let update = captured
            .iter()
            .find(|argv| {
                argv.first().map(|s| s.to_string_lossy().into_owned()) == Some("update".into())
            })
            .expect("update invocation recorded");
        let argv: Vec<String> = update
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(
            argv.iter().any(|a| a == "loom:clarify"),
            "missing loom:clarify in argv: {argv:?}",
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--status" && w[1] == "blocked"),
            "apply_integrity_clarify must pair --status blocked with --add-label: {argv:?}",
        );
    }
}
