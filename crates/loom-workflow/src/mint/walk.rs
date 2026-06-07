//! Walk orchestration that drives the mint pipeline's two Finding sources.
//!
//! Per `specs/gate.md` § *Findings and Minting* and § *Scope-dependent walk*
//! the mint walk varies by scope:
//!
//! - `--bead <id>` / `--diff <range>` / `--files <paths>` — invoke ONLY the
//!   LLM rubric agent process. Deterministic verifier failures are NOT
//!   normalised into Finding records at these scopes; the loop's preceding
//!   `verify --bead <id>` step has already handled them as `previous_failure`
//!   recovery context.
//! - `--tree` — invoke the deterministic verifier dispatcher first,
//!   collecting failed verdicts; then invoke the LLM rubric. Both sources
//!   flow into the same per-Finding pipeline. There is no shell-level
//!   `LOOM_FINDING` line for verify-side findings — only the in-driver
//!   record, normalised per the mapping table at `specs/gate.md` § *Emit
//!   shape* (verifier outcome → token/target/bonds).
//!
//! The orchestration layer is the seam between [`crate::mint`]'s
//! `mint_findings_with_options` and the two emit sources; it returns a
//! single ordered `Vec<Finding>` ready for the dedup → bonding-lead → mint
//! state machine. Idempotency vs partial failure is structural: the mint
//! pipeline's live-status dedup query is what skips already-minted findings
//! on re-run, so the walk doesn't carry state across invocations.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use askama::Template;
use displaydoc::Display;
use loom_driver::agent::{
    ProtocolError, RePinContent, SessionOutcome, SpawnConfig, set_loom_inside,
};
use loom_driver::bd::{BdClient, CommandRunner, ListOpts, TokioRunner};
use loom_driver::config::{LoomConfig, Phase};
use loom_driver::identifier::{BeadId, ProfileName, SpecLabel};
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_driver::scratch::{ScratchSession, resolve_scratch_key};
use loom_driver::state::StateDb;
use loom_gate::{
    Annotation, DispatchOptions, DispatchPendingExecutor, EmptyScope, FsCommandResolver,
    IntegrityFinding, Tier, TierCwds, annotation as gate_annotation, integrity, run_check,
    run_system, run_test_in,
};
use loom_templates::review::{ReviewContext, ReviewLane};
use thiserror::Error;

use crate::review::{
    ConcernToken, DispatchScope, Finding, FindingTarget, FindingValidator, WalkOutputError,
    beads_summary, default_profile_for_spec, load_review_sources, parse_walk_output,
};
use crate::todo::ExitSignal;

/// Resolved mint walk scope. Mirrors the `--bead` / `--diff` / `--files` /
/// `--tree` CLI flag the operator passed to `loom gate mint`; the
/// orchestration layer dispatches per variant. The CLI is responsible
/// for resolving defaults (per `specs/gate.md` § *Default for bare
/// invocation*) before constructing this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MintScope {
    Bead(BeadId),
    Diff(String),
    Files(Vec<PathBuf>),
    Tree,
}

impl MintScope {
    /// True iff the scope walks deterministic verifiers in addition to the
    /// LLM rubric. Only `--tree` does (per `specs/gate.md` § *Scope-
    /// dependent walk*).
    #[must_use]
    pub fn runs_verifiers(&self) -> bool {
        matches!(self, Self::Tree)
    }

    /// Project to the wire-level [`DispatchScope`] the parse pipeline
    /// uses for token-scope enforcement. `--bead` / `--diff` / `--files`
    /// collapse to [`DispatchScope::PerBead`]; `--tree` is
    /// [`DispatchScope::Tree`].
    #[must_use]
    pub fn dispatch_scope(&self) -> DispatchScope {
        match self {
            Self::Tree => DispatchScope::Tree,
            Self::Bead(_) | Self::Diff(_) | Self::Files(_) => DispatchScope::PerBead,
        }
    }
}

/// One deterministic-verifier failure surfaced by the dispatch layer at
/// tree scope. The orchestration normalises each into a typed
/// [`Finding`] via [`verifier_failure_to_finding`] per the mapping table
/// at `specs/gate.md` § *Emit shape*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierFailure {
    /// The annotation whose verifier ran (or could not run). Carries the
    /// owning spec via `source_spec`, which is what populates `bonds`.
    pub annotation: Annotation,
    pub kind: VerifierFailureKind,
    /// Evidence text from the verifier's JSON `evidence` field (else
    /// stderr tail / dispatch-error message). Stored verbatim on the
    /// minted fix-up bead's description.
    pub evidence: String,
}

/// Categorised failure mode for a deterministic verifier dispatch
/// outcome. Variants correspond row-by-row to the verifier-outcome
/// mapping table at `specs/gate.md` § *Emit shape*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierFailureKind {
    /// `[check]` / `[test]` / `[system]` exit ≠ 0 (and ≠ 2, ≠ 77).
    Failed,
    /// Dispatch error — exit code 2: command not found, missing
    /// prerequisite, runner unknown.
    DispatchError,
    /// Integrity gate forward-resolution failure: the annotation's
    /// target does not resolve for its tier.
    UnresolvedAnnotation,
    /// Integrity gate stub-pointing: the annotation's verifier body
    /// invokes the `_pending_stub` sigil.
    StubPointing,
    /// Integrity gate stale pending modifier: the annotation carries
    /// the `?` modifier but its target now resolves (and, for `[test?]`,
    /// has a non-stub body). The marker must be dropped in the same diff
    /// that landed the verifier.
    UnneededPendingMarker,
    /// Integrity gate inputs-protocol error: an opted-in input-query (a
    /// `[judge]` collect mode, or a `[check]` / `[system]` runner that
    /// declares an `inputs` query) exited non-zero or emitted a malformed
    /// inputs document.
    InputsProtocolError,
    /// Integrity gate atomic-acceptance violation: the criterion at
    /// `criterion_anchor` carries `count` annotations (expected 1).
    MultipleAnnotations {
        count: usize,
        criterion_anchor: String,
    },
}

/// Errors raised by the walk orchestration. Variants carry the
/// underlying failure source so the CLI can route specifics back to the
/// operator (e.g. spec-label parse error name + offending path).
#[derive(Debug, Display, Error)]
pub enum WalkError {
    /// LLM rubric agent process failed: {0}
    Rubric(String),
    /// deterministic verifier dispatch failed: {0}
    Verifiers(String),
    /// rubric stdout parse failed
    Parse(#[from] WalkOutputError),
    /// owning spec file `{path}` has no parseable spec label
    SpecLabel { path: PathBuf },
}

/// Abstracts the two side-effect-bearing surfaces the orchestration
/// depends on so the walk logic stays pure and is exercised under fakes
/// in tests. Production wires `run_rubric` to the existing review-agent
/// invocation in [`crate::review::runner`] and `run_verifiers` to the
/// deterministic dispatcher in [`loom_gate::dispatch`] plus the
/// integrity gate's resolver chain.
pub trait MintWalker: Send {
    /// Run the LLM rubric agent for `scope` and return its raw stdout
    /// (the buffer the `LOOM_FINDING:` / terminal-marker parsers consume).
    fn run_rubric(
        &mut self,
        scope: &MintScope,
    ) -> impl std::future::Future<Output = Result<String, WalkError>> + Send;

    /// Run every deterministic verifier in scope at tree scope and
    /// return one [`VerifierFailure`] per failed dispatch outcome.
    /// Implementations MUST NOT invoke this when `scope` is not
    /// [`MintScope::Tree`]; the orchestration only calls it on tree
    /// scope (the trait method takes `scope` so production
    /// implementations can fan out per-spec, not because non-tree
    /// scopes are valid here).
    fn run_verifiers(
        &mut self,
        scope: &MintScope,
    ) -> impl std::future::Future<Output = Result<Vec<VerifierFailure>, WalkError>> + Send;
}

/// Top-level walk: run the configured sources for `scope`, normalise
/// any verifier failures into typed `Finding` records, parse the rubric
/// stdout, and return the combined ordered vector for the mint pipeline.
///
/// Order: verifier-side findings come first (in dispatch order), rubric
/// findings come second (in stdout order). Both share the same finding
/// hash scheme, so order only affects the end-of-run summary's
/// per-finding lines — not which beads end up minted.
pub async fn walk<W: MintWalker, V: FindingValidator + ?Sized>(
    walker: &mut W,
    scope: &MintScope,
    validator: &V,
) -> Result<Vec<Finding>, WalkError> {
    let mut findings = Vec::new();
    if scope.runs_verifiers() {
        let failures = walker.run_verifiers(scope).await?;
        for failure in failures {
            findings.push(verifier_failure_to_finding(failure)?);
        }
    }
    let rubric_stdout = walker.run_rubric(scope).await?;
    let parsed = parse_walk_output(&rubric_stdout, scope.dispatch_scope(), validator)?;
    findings.extend(parsed);
    Ok(findings)
}

/// Normalise one [`VerifierFailure`] into a typed [`Finding`] per the
/// mapping at `specs/gate.md` § *Emit shape*. The owning spec for
/// `bonds` is derived from the annotation's `source_spec` path
/// (basename minus `.md`) — the same spec-section auto-include the
/// verifier's input set uses.
pub fn verifier_failure_to_finding(failure: VerifierFailure) -> Result<Finding, WalkError> {
    let owning = spec_label_from_path(&failure.annotation.source_spec)?;
    let target_string = failure.annotation.target.clone();
    let (token, target) = match failure.kind {
        VerifierFailureKind::Failed => (
            ConcernToken::VerifierFailed,
            FindingTarget::Annotation { target_string },
        ),
        VerifierFailureKind::DispatchError => (
            ConcernToken::DispatchError,
            FindingTarget::Annotation { target_string },
        ),
        VerifierFailureKind::UnresolvedAnnotation => (
            ConcernToken::UnresolvedAnnotation,
            FindingTarget::Annotation { target_string },
        ),
        VerifierFailureKind::StubPointing => (
            ConcernToken::StubPointing,
            FindingTarget::Annotation { target_string },
        ),
        VerifierFailureKind::UnneededPendingMarker => (
            ConcernToken::UnneededPendingMarker,
            FindingTarget::Annotation { target_string },
        ),
        VerifierFailureKind::InputsProtocolError => (
            ConcernToken::InputsProtocolError,
            FindingTarget::Annotation { target_string },
        ),
        VerifierFailureKind::MultipleAnnotations {
            criterion_anchor, ..
        } => (
            ConcernToken::MultipleAnnotations,
            FindingTarget::Criterion {
                spec: owning.clone(),
                anchor: criterion_anchor,
            },
        ),
    };
    Ok(Finding {
        token,
        bonds: vec![owning],
        target,
        evidence: failure.evidence,
    })
}

fn spec_label_from_path(path: &std::path::Path) -> Result<SpecLabel, WalkError> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.parse::<SpecLabel>().ok())
        .ok_or_else(|| WalkError::SpecLabel {
            path: path.to_path_buf(),
        })
}

fn tier_cwds_from_config(config: &LoomConfig) -> TierCwds {
    TierCwds {
        check: tier_cwd(config, "check"),
        test: tier_cwd(config, "test"),
        system: tier_cwd(config, "system"),
        judge: tier_cwd(config, "judge"),
    }
}

fn tier_cwd(config: &LoomConfig, tier: &str) -> Option<PathBuf> {
    config
        .runner
        .tier(tier)
        .and_then(|t| t.cwd.clone())
        .map(PathBuf::from)
}

fn test_runner_template(
    config: &LoomConfig,
    workspace: &Path,
) -> Result<Option<loom_gate::RunnerTemplate>, WalkError> {
    if let Some(tier) = config.runner.tier("test")
        && let Some(command) = tier.command.as_deref()
    {
        return Ok(Some(loom_gate::RunnerTemplate::new(command)));
    }
    match loom_gate::runner::discover(workspace, Tier::Test) {
        Ok(template) => Ok(Some(template)),
        Err(loom_gate::RunnerError::UnknownToolchain { .. }) => Ok(None),
        Err(e) => Err(WalkError::Verifiers(e.to_string())),
    }
}

fn resolved_cwd(cwd: Option<&Path>, workspace: &Path) -> PathBuf {
    match cwd {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => workspace.join(path),
        None => workspace.to_path_buf(),
    }
}

/// Production [`MintWalker`] used by the `loom gate mint` CLI arm.
///
/// `run_rubric` mirrors the [`crate::review::ProductionReviewController`]
/// setup: it renders the review prompt via [`ReviewContext`] in the
/// [`ReviewLane::Rubric`] lane, spawns the reviewer agent via a
/// caller-supplied closure (so backend selection — `PiBackend` vs
/// `ClaudeBackend` — stays in the binary), and returns the agent's
/// combined stdout for [`parse_walk_output`] to consume.
///
/// `run_verifiers` (called only at [`MintScope::Tree`]) parses workspace
/// annotations, runs the integrity gate's forward-resolution check
/// followed by the deterministic verifier set (`[check]` / `[test]` /
/// `[system]`), and returns one [`VerifierFailure`] per failed dispatch
/// outcome per `specs/gate.md` § *Emit shape* mapping table.
pub struct ProductionMintWalker<S, F, R: CommandRunner = TokioRunner>
where
    S: Fn(SpawnConfig) -> F + Send + Sync,
    F: std::future::Future<
            Output = Result<(SessionOutcome, Option<ExitSignal>, String), ProtocolError>,
        > + Send,
{
    bd: BdClient<R>,
    label: SpecLabel,
    workspace: PathBuf,
    state: Arc<StateDb>,
    manifest: Arc<ProfileImageManifest>,
    phase_default: ProfileName,
    spawn: S,
    style_rules: String,
}

impl<S, F, R: CommandRunner> ProductionMintWalker<S, F, R>
where
    S: Fn(SpawnConfig) -> F + Send + Sync,
    F: std::future::Future<
            Output = Result<(SessionOutcome, Option<ExitSignal>, String), ProtocolError>,
        > + Send,
{
    pub fn new(
        bd: BdClient<R>,
        label: SpecLabel,
        workspace: PathBuf,
        state: Arc<StateDb>,
        manifest: Arc<ProfileImageManifest>,
        phase_default: ProfileName,
        spawn: S,
    ) -> Self {
        Self {
            bd,
            label,
            workspace,
            state,
            manifest,
            phase_default,
            spawn,
            style_rules: "docs/style-rules.md".to_string(),
        }
    }

    /// Override the style-rules pin used in the rendered rubric prompt.
    /// Production callers thread `LoomConfig.style_rules`; tests rely on
    /// the built-in default.
    #[must_use]
    pub fn with_style_rules(mut self, path: String) -> Self {
        self.style_rules = path;
        self
    }

    fn spec_label_filter(&self) -> String {
        format!("spec:{}", self.label.as_str())
    }

    async fn resolve_molecule_id(
        &self,
    ) -> Result<Option<loom_driver::identifier::MoleculeId>, WalkError> {
        crate::resolve::resolve_open_epic(&self.bd, &self.label)
            .await
            .map_err(|e| WalkError::Rubric(e.to_string()))
    }

    async fn build_rubric_prompt(&self) -> Result<String, WalkError> {
        let beads = self
            .bd
            .list(ListOpts {
                status: None,
                label: Some(self.spec_label_filter()),
                ..ListOpts::default()
            })
            .await
            .map_err(|e| WalkError::Rubric(e.to_string()))?;
        let molecule_id = self.resolve_molecule_id().await?;
        let base_commit = match molecule_id.as_ref() {
            Some(id) => self
                .state
                .molecule(id)
                .map_err(|e| WalkError::Rubric(e.to_string()))?
                .and_then(|m| m.base_commit),
            None => None,
        };
        let spec_path_rel = format!("specs/{}.md", self.label.as_str());
        let (test_sources, judge_rubrics) =
            load_review_sources(&self.workspace, &self.workspace.join(&spec_path_rel))
                .map_err(|e| WalkError::Rubric(e.to_string()))?;
        let key = resolve_scratch_key(Phase::Review, &self.label, None);
        let scratchpad_path = ScratchSession::scratchpad_path_for(&self.workspace, &key)
            .to_string_lossy()
            .into_owned();
        let ctx = ReviewContext {
            pinned_context: String::new(),
            default_profile: default_profile_for_spec(&self.label),
            label: self.label.clone(),
            spec_path: spec_path_rel,
            companion_paths: vec![],
            beads_summary: beads_summary(&beads),
            base_commit,
            molecule_id,
            test_sources,
            judge_rubrics,
            scratchpad_path,
            style_rules: self.style_rules.clone(),
            lane: ReviewLane::Rubric,
        };
        ctx.render().map_err(|e| WalkError::Rubric(e.to_string()))
    }
}

impl<S, F, R: CommandRunner> MintWalker for ProductionMintWalker<S, F, R>
where
    S: Fn(SpawnConfig) -> F + Send + Sync,
    F: std::future::Future<
            Output = Result<(SessionOutcome, Option<ExitSignal>, String), ProtocolError>,
        > + Send,
{
    async fn run_rubric(&mut self, _scope: &MintScope) -> Result<String, WalkError> {
        let prompt = self.build_rubric_prompt().await?;
        let entry = self
            .manifest
            .lookup(&self.phase_default)
            .map_err(|e| WalkError::Rubric(e.to_string()))?;
        let banner = format!("loom gate mint @ {}", self.label);
        let key = resolve_scratch_key(Phase::Review, &self.label, None);
        let scratch = ScratchSession::open(&self.workspace, &key, &prompt, &banner)
            .map_err(|e| WalkError::Rubric(format!("scratch: {e}")))?;
        let mut env = Vec::new();
        set_loom_inside(&mut env);
        let spawn_config = SpawnConfig {
            image_ref: entry.r#ref.clone(),
            image_source: entry.source.clone(),
            image_digest_path: entry.digest.clone(),
            workspace: self.workspace.clone(),
            env,
            mounts: vec![],
            initial_prompt: prompt,
            agent_args: vec![],
            repin: RePinContent {
                orientation: String::new(),
                pinned_context: String::new(),
                partial_bodies: vec![],
            },
            scratch_dir: scratch.path().to_path_buf(),
            model: None,
            thinking_level: None,
            output_limits: None,
            shutdown_grace: None,
            handshake_timeout: None,
            stall_warn_interval: None,
            launcher_env: Vec::new(),
        };
        let result = (self.spawn)(spawn_config).await;
        drop(scratch);
        let (_outcome, _marker, stdout) = result.map_err(|e| WalkError::Rubric(e.to_string()))?;
        Ok(stdout)
    }

    async fn run_verifiers(
        &mut self,
        _scope: &MintScope,
    ) -> Result<Vec<VerifierFailure>, WalkError> {
        let specs_dir = self.workspace.join("specs");
        if !specs_dir.exists() {
            return Ok(Vec::new());
        }
        let parsed =
            gate_annotation::parse(&specs_dir).map_err(|e| WalkError::Verifiers(e.to_string()))?;
        let annotations = parsed.annotations;
        if annotations.is_empty() {
            return Ok(Vec::new());
        }
        let mut failures = Vec::new();
        let cmd_resolver = FsCommandResolver::new(&self.workspace);
        let (test_resolver, stub_scanner) = integrity::scan_workspace_pair(&self.workspace)
            .map_err(|e| WalkError::Verifiers(e.to_string()))?;
        let config = LoomConfig::load(LoomConfig::resolve_path(&self.workspace))
            .map_err(|e| WalkError::Verifiers(e.to_string()))?;
        let runner_specs = loom_gate::runner::integrity_runner_specs(&config)
            .map_err(|e| WalkError::Verifiers(e.to_string()))?;
        let options = DispatchOptions::default();
        let tier_cwds = tier_cwds_from_config(&config);
        let pending_executor = DispatchPendingExecutor::new(
            &runner_specs,
            options.clone(),
            &self.workspace,
            tier_cwds.clone(),
        );
        let integrity_findings = integrity::check(
            &annotations,
            &runner_specs,
            &self.workspace,
            &cmd_resolver,
            &test_resolver,
            &stub_scanner,
            &pending_executor,
        );
        for finding in integrity_findings {
            if let Some(failure) = integrity_to_verifier_failure(&finding, &annotations)? {
                failures.push(failure);
            }
        }
        for outcome in run_check(
            &annotations,
            &runner_specs,
            &options,
            &self.workspace,
            &tier_cwds,
        ) {
            failures.extend(dispatch_outcome_to_failures(outcome));
        }
        for outcome in run_system(
            &annotations,
            &runner_specs,
            &options,
            &self.workspace,
            &tier_cwds,
        ) {
            failures.extend(dispatch_outcome_to_failures(outcome));
        }
        if let Some(template) = test_runner_template(&config, &self.workspace)? {
            let test_cwd = resolved_cwd(tier_cwds.for_tier(Tier::Test), &self.workspace);
            match run_test_in(
                &annotations,
                &options,
                &template,
                &EmptyScope,
                Some(&test_cwd),
            ) {
                Ok(Some(outcome)) => failures.extend(dispatch_outcome_to_failures(Ok(outcome))),
                Ok(None) => {}
                Err(e) => return Err(WalkError::Verifiers(e.to_string())),
            }
        }
        Ok(failures)
    }
}

/// Normalise one [`IntegrityFinding`] into a [`VerifierFailure`] per
/// the mapping at `specs/gate.md` § *Emit shape*. Returns `None` for
/// [`IntegrityFinding::UnresolvedCargoTestName`], which has no in-table
/// mapping (it is emitted by the verify lane's stderr surface only).
fn integrity_to_verifier_failure(
    finding: &IntegrityFinding,
    annotations: &[Annotation],
) -> Result<Option<VerifierFailure>, WalkError> {
    match finding {
        IntegrityFinding::UnresolvedAnnotation {
            spec, line, target, ..
        } => Ok(
            match_annotation(annotations, spec, *line, target).map(|annotation| VerifierFailure {
                annotation,
                kind: VerifierFailureKind::UnresolvedAnnotation,
                evidence: finding.to_string(),
            }),
        ),
        IntegrityFinding::StubTestFunction {
            spec, line, target, ..
        } => Ok(
            match_annotation(annotations, spec, *line, target).map(|annotation| VerifierFailure {
                annotation,
                kind: VerifierFailureKind::StubPointing,
                evidence: finding.to_string(),
            }),
        ),
        IntegrityFinding::UnneededPendingMarker {
            spec, line, target, ..
        } => Ok(
            match_annotation(annotations, spec, *line, target).map(|annotation| VerifierFailure {
                annotation,
                kind: VerifierFailureKind::UnneededPendingMarker,
                evidence: finding.to_string(),
            }),
        ),
        IntegrityFinding::InputsProtocolError {
            spec, line, target, ..
        } => Ok(
            match_annotation(annotations, spec, *line, target).map(|annotation| VerifierFailure {
                annotation,
                kind: VerifierFailureKind::InputsProtocolError,
                evidence: finding.to_string(),
            }),
        ),
        IntegrityFinding::MultipleAnnotations { spec, line, count } => annotations
            .iter()
            .find(|a| a.source_spec == *spec && a.criterion_line == *line)
            .cloned()
            .map(|annotation| {
                Ok(VerifierFailure {
                    kind: VerifierFailureKind::MultipleAnnotations {
                        count: *count,
                        criterion_anchor: multiple_annotations_anchor(&annotation, *line)?,
                    },
                    annotation,
                    evidence: finding.to_string(),
                })
            })
            .transpose(),
        IntegrityFinding::UnresolvedCargoTestName { .. } => Ok(None),
    }
}

fn multiple_annotations_anchor(
    annotation: &Annotation,
    criterion_line: u32,
) -> Result<String, WalkError> {
    if !annotation.source_spec.is_file() {
        return Ok(criterion_line_anchor(&annotation.target));
    }
    let body = std::fs::read_to_string(&annotation.source_spec)
        .map_err(|e| WalkError::Verifiers(e.to_string()))?;
    Ok(body
        .lines()
        .nth(criterion_line.saturating_sub(1) as usize)
        .map(criterion_line_anchor)
        .filter(|anchor| !anchor.is_empty())
        .unwrap_or_else(|| criterion_line_anchor(&annotation.target)))
}

fn match_annotation(
    annotations: &[Annotation],
    spec: &Path,
    line: u32,
    target: &str,
) -> Option<Annotation> {
    annotations
        .iter()
        .find(|a| a.source_spec == spec && a.line == line && a.target == target)
        .cloned()
}

fn criterion_line_anchor(line: &str) -> String {
    let marker_index = ["[check", "[test", "[system", "[judge"]
        .into_iter()
        .filter_map(|marker| line.find(marker))
        .min()
        .unwrap_or(line.len());
    let prefix = line[..marker_index].trim();
    let candidate = if prefix.is_empty() { line } else { prefix };
    lower_kebab(candidate)
}

fn lower_kebab(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Convert one dispatcher outcome into the [`VerifierFailure`]s the mint
/// pipeline emits. A passing verdict yields no failures; a failing or
/// dispatcher-error outcome yields one [`VerifierFailure`] per annotation
/// the outcome covers (batched runners cover N annotations in one
/// outcome, so the fan-out preserves per-annotation granularity at the
/// finding level).
fn dispatch_outcome_to_failures(
    outcome: Result<loom_gate::DispatchOutcome, loom_gate::DispatchError>,
) -> Vec<VerifierFailure> {
    match outcome {
        Ok(out) if out.verdict.skipped || out.verdict.pass => Vec::new(),
        Ok(out) => out
            .annotations
            .into_iter()
            .map(|annotation| VerifierFailure {
                annotation,
                kind: VerifierFailureKind::Failed,
                evidence: out.verdict.evidence.clone(),
            })
            .collect(),
        Err(err) => vec![VerifierFailure {
            annotation: dispatch_error_annotation(&err),
            kind: VerifierFailureKind::DispatchError,
            evidence: err.to_string(),
        }],
    }
}

/// Reconstruct a [`Annotation`] from a dispatcher [`loom_gate::DispatchError`]
/// when the dispatcher couldn't tie the error to a concrete annotation
/// (e.g. an empty-target failure). Falls back to a synthetic annotation
/// pointing at `specs/gate.md` so the resulting [`VerifierFailure`]'s
/// `source_spec`-derived bonding still resolves to a real spec.
fn dispatch_error_annotation(err: &loom_gate::DispatchError) -> Annotation {
    let target = match err {
        loom_gate::DispatchError::Spawn { command, .. } => command.clone(),
        loom_gate::DispatchError::MalformedVerdict { command, .. } => command.clone(),
        loom_gate::DispatchError::MissingFromBatchOutput { target, .. } => target.clone(),
        loom_gate::DispatchError::EmptyTarget { .. } => String::new(),
        loom_gate::DispatchError::ZeroMatch { .. } => String::new(),
    };
    Annotation {
        tier: Tier::Check,
        target,
        source_spec: PathBuf::from("specs/gate.md"),
        line: 0,
        criterion_line: 0,
        pending: false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use loom_driver::bd::{BdClient, BdError, CommandRunner, RunOutput};
    use loom_driver::identifier::BeadId;
    use loom_gate::{Annotation, Tier};

    use super::*;
    use crate::mint::{
        BatchOutcome, DEDUP_STATUSES, FINDING_LABEL_PREFIX, MintOptions, batch_fingerprint,
        mint_findings_with_options,
    };
    use crate::review::{LOOM_FINDING_PREFIX, TargetKind};

    fn spec(s: &str) -> SpecLabel {
        s.parse().expect("valid spec label")
    }

    fn annotation(tier: Tier, target: &str, source_spec: &str) -> Annotation {
        Annotation {
            tier,
            target: target.to_owned(),
            source_spec: PathBuf::from(source_spec),
            line: 1,
            criterion_line: 1,
            pending: false,
        }
    }

    /// `FindingValidator` implementation that admits everything. The walk
    /// orchestration tests don't care about Layer-3/Layer-5 resolution —
    /// they care about *which sources* contribute findings under each
    /// scope. The pure parse/validation tests live in
    /// [`crate::review::finding::tests`] and exercise the strict
    /// validator paths separately.
    struct AlwaysValid;

    impl FindingValidator for AlwaysValid {
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

    /// Walker fake that returns canned outputs and tracks per-method call
    /// counts so tests can assert which sources fired for a given scope.
    #[derive(Default)]
    struct FakeWalker {
        rubric_stdout: String,
        verifier_failures: Vec<VerifierFailure>,
        rubric_calls: usize,
        verifier_calls: usize,
        observed_scopes: Vec<MintScope>,
    }

    impl MintWalker for FakeWalker {
        async fn run_rubric(&mut self, scope: &MintScope) -> Result<String, WalkError> {
            self.rubric_calls += 1;
            self.observed_scopes.push(scope.clone());
            Ok(self.rubric_stdout.clone())
        }

        async fn run_verifiers(
            &mut self,
            scope: &MintScope,
        ) -> Result<Vec<VerifierFailure>, WalkError> {
            self.verifier_calls += 1;
            self.observed_scopes.push(scope.clone());
            Ok(self.verifier_failures.clone())
        }
    }

    fn finding_line(payload: &str) -> String {
        format!("{LOOM_FINDING_PREFIX} {payload}")
    }

    /// Spec contract `specs/gate.md` § *Scope-dependent walk*
    /// (criterion `mint_bead_scope_walks_llm_rubric_only_not_verifiers`):
    /// at `--bead <id>` / `--diff <range>` /
    /// `--files <paths>` scope, the walk runs ONLY the LLM rubric agent;
    /// the deterministic verifier dispatcher MUST NOT fire because the
    /// loop's preceding `verify --bead <id>` step has already handled
    /// verify-side failures as `previous_failure` recovery context.
    ///
    /// Pinning the no-call here is what makes the scope-dependent
    /// behaviour structural: if a future refactor accidentally always
    /// invoked the verifier dispatcher, this test catches it before
    /// per-bead findings start double-reporting.
    #[tokio::test]
    async fn mint_bead_scope_walks_llm_rubric_only_not_verifiers() {
        let rubric = format!(
            "preamble\n{}\nLOOM_CONCERN: {{\"summary\":\"found one\"}}\n",
            finding_line(
                r#"{"token":"spec-coherence-fail","bonds":["gate"],"target":{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"},"evidence":"e"}"#
            ),
        );
        let mut walker = FakeWalker {
            rubric_stdout: rubric,
            // verifier_failures must NEVER reach the per-Finding loop on
            // bead scope — populate the slot anyway so the assertion
            // proves the dispatcher was skipped (not just empty).
            verifier_failures: vec![VerifierFailure {
                annotation: annotation(Tier::Check, "would-not-fire", "specs/gate.md"),
                kind: VerifierFailureKind::Failed,
                evidence: "should not appear".into(),
            }],
            ..FakeWalker::default()
        };
        let scope = MintScope::Bead(BeadId::new("lm-loop.1").expect("valid"));
        let findings = walk(&mut walker, &scope, &AlwaysValid)
            .await
            .expect("walk succeeds");
        assert_eq!(walker.rubric_calls, 1, "rubric ran exactly once");
        assert_eq!(
            walker.verifier_calls, 0,
            "deterministic verifiers MUST NOT run on bead scope",
        );
        assert_eq!(
            findings.len(),
            1,
            "only the rubric finding reaches the mint pipeline: {findings:?}",
        );
        assert_eq!(findings[0].token, ConcernToken::SpecCoherenceFail);

        // The same property holds for --diff and --files scopes.
        for scope in [
            MintScope::Diff("HEAD".into()),
            MintScope::Files(vec![PathBuf::from("src/lib.rs")]),
        ] {
            let mut w = FakeWalker {
                rubric_stdout: "no findings\n".into(),
                verifier_failures: vec![VerifierFailure {
                    annotation: annotation(Tier::Check, "x", "specs/gate.md"),
                    kind: VerifierFailureKind::Failed,
                    evidence: "y".into(),
                }],
                ..FakeWalker::default()
            };
            let _ = walk(&mut w, &scope, &AlwaysValid).await.expect("walk");
            assert_eq!(
                w.verifier_calls, 0,
                "non-tree scope skips verifiers: {scope:?}"
            );
        }
    }

    /// Spec contract `specs/gate.md` § *Scope-dependent walk* (criterion
    /// `mint_tree_scope_walks_verifiers_and_rubric_emitting_findings_from_both`):
    /// at `--tree` scope, the walk invokes BOTH the
    /// deterministic verifier dispatcher and the LLM rubric. Both
    /// sources feed into the same per-Finding loop; the verifier failures
    /// are normalised in-driver into typed Finding records (no
    /// shell-level `LOOM_FINDING:` line for them) per the mapping table
    /// in *Concern tokens and target variants*.
    #[tokio::test]
    async fn mint_tree_scope_walks_verifiers_and_rubric_emitting_findings_from_both() {
        let rubric = format!(
            "preamble\n{}\n{}\nLOOM_CONCERN: {{\"summary\":\"two findings\"}}\n",
            finding_line(
                r#"{"token":"orphan-integration","bonds":["harness"],"target":{"kind":"Contract","id":"molecule-lifecycle"},"evidence":"contract"}"#
            ),
            finding_line(
                r#"{"token":"style-rule-violation","bonds":["gate"],"target":{"kind":"StyleRule","rule_id":"RS-19","subject":"crates/loom-workflow/src/mint/walk.rs"},"evidence":"style"}"#
            ),
        );
        // Two verifier-side failures across different categories — one
        // dispatch error, one verify failure — so the mapping table's
        // multi-token coverage gets exercised.
        let mut walker = FakeWalker {
            rubric_stdout: rubric,
            verifier_failures: vec![
                VerifierFailure {
                    annotation: annotation(
                        Tier::Check,
                        "cargo run -p loom-walk -- nonexistent",
                        "specs/gate.md",
                    ),
                    kind: VerifierFailureKind::DispatchError,
                    evidence: "command not found".into(),
                },
                VerifierFailure {
                    annotation: annotation(
                        Tier::Test,
                        "crate::module::failing_test",
                        "specs/harness.md",
                    ),
                    kind: VerifierFailureKind::Failed,
                    evidence: "assertion failed".into(),
                },
            ],
            ..FakeWalker::default()
        };
        let findings = walk(&mut walker, &MintScope::Tree, &AlwaysValid)
            .await
            .expect("walk succeeds");
        assert_eq!(walker.rubric_calls, 1, "rubric ran exactly once");
        assert_eq!(walker.verifier_calls, 1, "verifiers ran exactly once");
        assert_eq!(findings.len(), 4, "both sources contribute: {findings:?}");

        // Verifier-side findings appear first (dispatch order), each
        // normalised to the spec-mandated token + target shape.
        assert_eq!(findings[0].token, ConcernToken::DispatchError);
        assert_eq!(findings[0].target.kind(), TargetKind::Annotation);
        assert_eq!(
            findings[0].bonds,
            vec![spec("gate")],
            "owning spec is derived from annotation.source_spec",
        );
        assert_eq!(findings[1].token, ConcernToken::VerifierFailed);
        assert_eq!(findings[1].target.kind(), TargetKind::Annotation);
        assert_eq!(findings[1].bonds, vec![spec("harness")]);

        // Rubric-side findings follow, in stdout order.
        assert_eq!(findings[2].token, ConcernToken::OrphanIntegration);
        assert_eq!(findings[3].token, ConcernToken::StyleRuleViolation);
    }

    /// Spec contract `specs/gate.md` § *Findings and Minting* (criterion
    /// `mint_walk_emits_loom_finding_json_lines_streamed_per_finding`):
    /// the walk's stdout emits `LOOM_FINDING: <json>`
    /// lines one-per-finding as findings are identified (not batched at
    /// end-of-walk). The parser side is unit-tested in
    /// [`crate::review::finding::tests`]; this is the integration side
    /// that drives a real walk through the orchestration and asserts the
    /// stream-shape semantics — every line in the stdout buffer becomes
    /// one typed Finding, in stdout order.
    #[tokio::test]
    async fn mint_walk_emits_loom_finding_json_lines_streamed_per_finding() {
        // Three findings interleaved with prose so the test pins
        // "one line per finding, stdout-order" rather than "batched at
        // end-of-walk". A batched emit would either lose interleaved
        // ordering or collapse adjacent payloads.
        let rubric = format!(
            "preamble before any findings\n\
             {a}\n\
             intermediate prose between findings\n\
             {b}\n\
             still more prose\n\
             {c}\n\
             trailing summary\n\
             LOOM_CONCERN: {{\"summary\":\"three findings\"}}\n",
            a = finding_line(
                r#"{"token":"spec-coherence-fail","bonds":["gate"],"target":{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"},"evidence":"first"}"#
            ),
            b = finding_line(
                r#"{"token":"orphan-integration","bonds":["harness"],"target":{"kind":"Contract","id":"molecule-lifecycle"},"evidence":"second"}"#
            ),
            c = finding_line(
                r#"{"token":"style-rule-violation","bonds":["gate"],"target":{"kind":"StyleRule","rule_id":"COM-1","subject":"crates/loom-workflow/src/mint/walk.rs#stream"},"evidence":"third"}"#
            ),
        );
        let mut walker = FakeWalker {
            rubric_stdout: rubric,
            ..FakeWalker::default()
        };
        let findings = walk(&mut walker, &MintScope::Diff("HEAD".into()), &AlwaysValid)
            .await
            .expect("walk succeeds");

        assert_eq!(
            findings.len(),
            3,
            "every LOOM_FINDING line becomes one Finding: {findings:?}",
        );
        // Stable stdout order — first finding emitted is findings[0].
        assert_eq!(findings[0].evidence, "first");
        assert_eq!(findings[1].evidence, "second");
        assert_eq!(findings[2].evidence, "third");
        // Tagged-target enum deserialised by `kind`.
        assert_eq!(findings[0].target.kind(), TargetKind::Criterion);
        assert_eq!(findings[1].target.kind(), TargetKind::Contract);
        assert_eq!(findings[2].target.kind(), TargetKind::StyleRule);
    }

    /// Bd runner that hands back canned [`RunOutput`]s in order and
    /// records every argv it spawned. Used by the idempotency test to
    /// script a partial-failure first pass followed by a successful
    /// retry; the per-call assertions key off the recorded argv.
    struct ScriptedRunner {
        responses: Mutex<Vec<Result<RunOutput, BdError>>>,
        invocations: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl ScriptedRunner {
        fn new(responses: Vec<Result<RunOutput, BdError>>) -> Self {
            Self {
                responses: Mutex::new(responses),
                invocations: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn invocations_handle(&self) -> Arc<Mutex<Vec<Vec<String>>>> {
            Arc::clone(&self.invocations)
        }
    }

    impl CommandRunner for ScriptedRunner {
        async fn run(
            &self,
            args: Vec<std::ffi::OsString>,
            _t: std::time::Duration,
        ) -> Result<RunOutput, BdError> {
            self.invocations.lock().expect("not poisoned").push(
                args.iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect(),
            );
            let mut responses = self.responses.lock().expect("not poisoned");
            assert!(
                !responses.is_empty(),
                "ScriptedRunner: no more responses queued (got args {args:?})",
            );
            responses.remove(0)
        }
    }

    fn ok_stdout(body: &str) -> Result<RunOutput, BdError> {
        Ok(RunOutput {
            status: 0,
            stdout: body.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    fn epic_list(id: &str, label: &str) -> String {
        format!(
            r#"[{{"id":"{id}","title":"{label}: epic","status":"open","priority":2,"issue_type":"epic","labels":["spec:{label}"]}}]"#,
        )
    }

    fn fixup_row(id: &str, hash: &str) -> String {
        format!(
            r#"{{"id":"{id}","title":"existing","status":"open","priority":2,"issue_type":"task","labels":["{FINDING_LABEL_PREFIX}{hash}"]}}"#,
        )
    }

    /// Spec contract `specs/gate.md` § *Findings and Minting* (criterion
    /// `mint_idempotent_after_partial_failure_retries_only_unfinished_batches`):
    /// a crash mid-run leaves successfully-minted findings with their
    /// `finding:<hash>` labels; the next mint invocation's live-status
    /// dedup query matches those findings and skips them, retrying only
    /// findings that did not reach `bd create` on the prior run.
    #[tokio::test]
    async fn mint_idempotent_after_partial_failure_retries_only_unfinished_batches() {
        // Three findings split across two lead specs ⇒ two batches.
        let finding_a = Finding {
            token: ConcernToken::SpecCoherenceFail,
            bonds: vec![spec("gate")],
            target: FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "verifier-honesty".into(),
            },
            evidence: "A".into(),
        };
        let finding_b = Finding {
            token: ConcernToken::OrphanIntegration,
            bonds: vec![spec("harness")],
            target: FindingTarget::Contract {
                id: "molecule-lifecycle".into(),
            },
            evidence: "B".into(),
        };
        let finding_c = Finding {
            token: ConcernToken::StyleRuleViolation,
            bonds: vec![spec("gate")],
            target: FindingTarget::StyleRule {
                rule_id: "RS-19".into(),
                subject: "crates/loom-workflow/src/mint/walk.rs".into(),
            },
            evidence: "C".into(),
        };
        let fp_gate_batch = batch_fingerprint(&[finding_a.clone(), finding_c.clone()]);
        let fp_harness_batch = batch_fingerprint(std::slice::from_ref(&finding_b));
        let findings = vec![finding_a.clone(), finding_b.clone(), finding_c.clone()];

        let pass1_responses: Vec<Result<RunOutput, BdError>> = vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-harnessepic", "harness")),
            ok_stdout("[]"),
            ok_stdout("lm-gatebatch.1\n"),
            Err(BdError::Spawn(std::io::Error::other(
                "simulated mid-run crash",
            ))),
        ];
        let runner = ScriptedRunner::new(pass1_responses);
        let bd = BdClient::with_runner(runner);
        let summary1 =
            mint_findings_with_options(&bd, &findings, "head-sha", &MintOptions::default()).await;
        assert_eq!(summary1.minted, 1, "gate batch succeeds on pass 1");
        assert_eq!(summary1.errors, 1, "harness batch fails on pass 1");
        assert!(
            summary1.batches.iter().any(|o| matches!(
                o,
                BatchOutcome::Minted { fingerprint, .. } if fingerprint == &fp_gate_batch
            )),
            "gate batch minted on pass 1: {:?}",
            summary1.batches,
        );
        assert!(
            summary1.batches.iter().any(|o| matches!(
                o,
                BatchOutcome::Errored { fingerprint, .. } if fingerprint == &fp_harness_batch
            )),
            "harness batch errored on pass 1: {:?}",
            summary1.batches,
        );

        let pass2_responses: Vec<Result<RunOutput, BdError>> = vec![
            ok_stdout(&format!(
                "[{}]",
                fixup_row("lm-gatebatch.1", &finding_a.hash())
            )),
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-harnessepic", "harness")),
            ok_stdout(&format!(
                "[{}]",
                fixup_row("lm-gatebatch.1", &finding_c.hash())
            )),
            ok_stdout("lm-harnessbatch.1\n"),
        ];
        let runner2 = ScriptedRunner::new(pass2_responses);
        let invocations2 = runner2.invocations_handle();
        let bd2 = BdClient::with_runner(runner2);
        let summary2 =
            mint_findings_with_options(&bd2, &findings, "head-sha", &MintOptions::default()).await;
        assert_eq!(
            summary2.minted, 1,
            "only harness batch mints on pass 2 (retry)"
        );
        assert_eq!(
            summary2.skipped, 2,
            "gate findings are dedup-skipped on pass 2"
        );
        assert_eq!(summary2.errors, 0, "no errors on pass 2");
        assert_eq!(summary2.refused, 0);

        // Pass 2 must call `bd create` exactly once (the harness batch).
        let pass2_calls: Vec<Vec<String>> = invocations2.lock().expect("not poisoned").clone();
        let create_calls = pass2_calls
            .iter()
            .filter(|c| c.iter().any(|a| a == "create"))
            .count();
        assert_eq!(
            create_calls, 1,
            "pass 2 retries ONLY the unfinished batch (harness): {pass2_calls:?}",
        );
        let dedup_calls = pass2_calls
            .iter()
            .filter(|c| {
                c.iter().any(|a| a == "list")
                    && c.iter().any(|a| a == &format!("--status={DEDUP_STATUSES}"))
                    && c.iter().any(|a| a.starts_with("--label=finding:"))
            })
            .count();
        assert!(
            dedup_calls >= 3,
            "dedup query runs once per finding on pass 2: {pass2_calls:?}",
        );

        let pass2_gate_skip = summary2.batches.iter().find(|o| matches!(
            o,
            BatchOutcome::SkippedDedup { existing_bead, .. } if existing_bead.as_str() == "lm-gatebatch.1"
        ));
        assert!(
            pass2_gate_skip.is_some(),
            "pass 2 must dedup against the original lm-gatebatch.1 bead: {:?}",
            summary2.batches,
        );

        // The walk-orchestration story end-to-end: a walker returning the
        // same Findings on both passes leaves the system in the same
        // converged state.
        let mut walker_pass2 = FakeWalker {
            rubric_stdout: rubric_for_three(&finding_a, &finding_b, &finding_c),
            ..FakeWalker::default()
        };
        let parsed = walk(&mut walker_pass2, &MintScope::Tree, &AlwaysValid)
            .await
            .expect("walk parses");
        let hashes: std::collections::HashSet<String> = parsed.iter().map(Finding::hash).collect();
        let expected: std::collections::HashSet<String> =
            [finding_a.hash(), finding_b.hash(), finding_c.hash()]
                .into_iter()
                .collect();
        assert_eq!(
            hashes, expected,
            "walk emits the same per-finding hashes across runs — the dedup key is stable",
        );

        let identifier_to_bead: HashMap<String, BeadId> = summary2
            .batches
            .iter()
            .filter_map(|o| match o {
                BatchOutcome::Minted {
                    fingerprint,
                    bead_id,
                    ..
                }
                | BatchOutcome::SkippedDedup {
                    fingerprint,
                    existing_bead: bead_id,
                    ..
                } => Some((fingerprint.clone(), bead_id.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            identifier_to_bead.len(),
            3,
            "every finding or minted batch terminates in pass 2: {identifier_to_bead:?}",
        );
    }

    fn rubric_for_three(a: &Finding, b: &Finding, c: &Finding) -> String {
        let line_a = finding_line(&serde_json::to_string(a).expect("serialize"));
        let line_b = finding_line(&serde_json::to_string(b).expect("serialize"));
        let line_c = finding_line(&serde_json::to_string(c).expect("serialize"));
        format!("{line_a}\n{line_b}\n{line_c}\nLOOM_CONCERN: {{\"summary\":\"three findings\"}}\n")
    }

    /// `verifier_failure_to_finding` covers every spec-mandated mapping
    /// row in *Concern tokens and target variants*. Pinned per-row so
    /// adding a new `VerifierFailureKind` variant without updating the
    /// mapping fails here rather than in a downstream consumer.
    #[test]
    fn verifier_failure_mapping_per_spec_table() {
        let ann = annotation(
            Tier::Check,
            "cargo run -p loom-walk -- foo",
            "specs/gate.md",
        );
        let cases = [
            (VerifierFailureKind::Failed, ConcernToken::VerifierFailed),
            (
                VerifierFailureKind::DispatchError,
                ConcernToken::DispatchError,
            ),
            (
                VerifierFailureKind::UnresolvedAnnotation,
                ConcernToken::UnresolvedAnnotation,
            ),
            (
                VerifierFailureKind::StubPointing,
                ConcernToken::StubPointing,
            ),
            (
                VerifierFailureKind::UnneededPendingMarker,
                ConcernToken::UnneededPendingMarker,
            ),
            (
                VerifierFailureKind::InputsProtocolError,
                ConcernToken::InputsProtocolError,
            ),
        ];
        for (kind, expected_token) in cases {
            let finding = verifier_failure_to_finding(VerifierFailure {
                annotation: ann.clone(),
                kind,
                evidence: "e".into(),
            })
            .expect("ok");
            assert_eq!(finding.token, expected_token);
            assert_eq!(finding.target.kind(), TargetKind::Annotation);
            assert_eq!(finding.bonds, vec![spec("gate")]);
        }

        let finding = verifier_failure_to_finding(VerifierFailure {
            annotation: ann,
            kind: VerifierFailureKind::MultipleAnnotations {
                count: 2,
                criterion_anchor: "some-anchor".into(),
            },
            evidence: "criterion carries 2 annotations".into(),
        })
        .expect("ok");
        assert_eq!(finding.token, ConcernToken::MultipleAnnotations);
        assert_eq!(finding.target.kind(), TargetKind::Criterion);
        match &finding.target {
            FindingTarget::Criterion { spec: s, anchor } => {
                assert_eq!(s, &spec("gate"));
                assert_eq!(anchor, "some-anchor");
            }
            other => panic!("expected Criterion target, got {other:?}"),
        }
    }

    /// Spec contract `specs/gate.md` § *Emit shape* (table row "Integrity
    /// gate: atomic-acceptance violation"): an
    /// [`IntegrityFinding::MultipleAnnotations`] normalises into a
    /// [`VerifierFailure`] carrying [`VerifierFailureKind::MultipleAnnotations`]
    /// with a stable criterion-text anchor and the integrity finding's
    /// `Display` text as evidence.
    #[test]
    fn integrity_multiple_annotations_maps_to_stable_criterion_anchor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let specs = dir.path().join("specs");
        std::fs::create_dir_all(&specs).expect("specs dir");
        let spec_path = specs.join("gate.md");
        std::fs::write(
            &spec_path,
            "# Gate\n\n- Finding id finding hash suppression and dedup [check](cargo run -p a) [test](crate::t::b)\n",
        )
        .expect("spec");
        let ann_a = Annotation {
            tier: Tier::Check,
            target: "cargo run -p a".into(),
            source_spec: spec_path.clone(),
            line: 3,
            criterion_line: 3,
            pending: false,
        };
        let ann_b = Annotation {
            tier: Tier::Test,
            target: "crate::t::b".into(),
            source_spec: spec_path.clone(),
            line: 3,
            criterion_line: 3,
            pending: false,
        };
        let finding = IntegrityFinding::MultipleAnnotations {
            spec: spec_path,
            line: 3,
            count: 2,
        };

        let failure = integrity_to_verifier_failure(&finding, &[ann_a.clone(), ann_b])
            .expect("normalisation succeeds")
            .expect("MultipleAnnotations integrity finding produces a VerifierFailure");
        match &failure.kind {
            VerifierFailureKind::MultipleAnnotations {
                count,
                criterion_anchor,
            } => {
                assert_eq!(*count, 2);
                assert_eq!(
                    criterion_anchor,
                    "finding-id-finding-hash-suppression-and-dedup"
                );
            }
            other => panic!("expected MultipleAnnotations kind, got {other:?}"),
        }
        assert_eq!(failure.annotation.source_spec, ann_a.source_spec);
        assert!(failure.evidence.contains("criterion carries 2 annotations"));

        let typed = verifier_failure_to_finding(failure).expect("normalises into Finding");
        assert_eq!(typed.token, ConcernToken::MultipleAnnotations);
        assert_eq!(typed.bonds, vec![spec("gate")]);
        match &typed.target {
            FindingTarget::Criterion { spec: s, anchor } => {
                assert_eq!(s, &spec("gate"));
                assert_eq!(anchor, "finding-id-finding-hash-suppression-and-dedup");
            }
            other => panic!("expected Criterion target, got {other:?}"),
        }
        assert_eq!(
            typed.id(),
            "v1:criterion:multiple-annotations:gate#finding-id-finding-hash-suppression-and-dedup"
        );
    }

    /// Spec contract `specs/gate.md` § *Production walker wiring*
    /// (criterion `production_mint_walker_exists_and_dispatches_rubric_and_verifiers`):
    /// a production [`MintWalker`] implementation
    /// exists in `loom-workflow::mint::walk` alongside the trait. Its
    /// `run_rubric` spawns the reviewer agent subprocess against the
    /// rendered review prompt and returns the agent's combined stdout;
    /// its `run_verifiers` (called only at `MintScope::Tree`) dispatches
    /// the deterministic verifier set + the integrity gate forward-
    /// resolution check.
    ///
    /// Behavioral assertion: at `--tree` scope, the production walker's
    /// spawn closure is invoked, an unresolved `[check]` emits a typed
    /// integrity finding, and the configured `[runner.test]` command runs
    /// from its configured cwd.
    #[tokio::test]
    async fn production_mint_walker_exists_and_dispatches_rubric_and_verifiers() {
        use loom_driver::profile_manifest::ProfileImageManifest;
        use loom_driver::state::StateDb;

        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().to_path_buf();

        const UNRESOLVED_TARGET: &str = "loom-verifier-bypass-fixture-9b8d-does-not-exist";

        std::fs::create_dir_all(workspace.join("specs")).expect("specs dir");
        std::fs::create_dir_all(workspace.join("verifier-cwd")).expect("cwd dir");
        std::fs::write(workspace.join("verifier-cwd/cwd-sentinel"), "ok").expect("sentinel");
        std::fs::write(
            workspace.join("specs/test-mint.md"),
            format!(
                "# test-mint\n\n- exercise verifier dispatch [check]({UNRESOLVED_TARGET})\n- exercise configured test runner [test](crate::configured::runner)\n"
            ),
        )
        .expect("spec");
        std::fs::write(
            workspace.join("loom.toml"),
            r#"[runner.test]
command = "bash -c 'test -f cwd-sentinel && printf ok > ran.txt'"
cwd = "verifier-cwd"
"#,
        )
        .expect("loom.toml");
        let manifest_path = workspace.join("profile-images.json");
        std::fs::write(
            &manifest_path,
            r#"{"base":{"ref":"localhost/wrix-base:abc","source":"/nix/store/aaa"}}"#,
        )
        .expect("manifest");
        let manifest =
            Arc::new(ProfileImageManifest::from_path(&manifest_path).expect("manifest parse"));
        let state = Arc::new(StateDb::open(workspace.join(".loom/state.db")).expect("state db"));

        // bd.list calls during prompt build: (1) spec-label bead summary,
        // (2) resolve_open_epic. Both return `[]` so the walker proceeds
        // with no molecule_id and no beads_summary.
        let responses = vec![ok_stdout("[]"), ok_stdout("[]")];
        let runner = ScriptedRunner::new(responses);
        let bd = BdClient::with_runner(runner);

        let spawn_called = Arc::new(Mutex::new(0_usize));
        let captured_scope_dir = Arc::new(Mutex::new(None::<PathBuf>));
        let spawn_called_inner = Arc::clone(&spawn_called);
        let captured_inner = Arc::clone(&captured_scope_dir);
        let spawn = move |cfg: SpawnConfig| {
            let called = Arc::clone(&spawn_called_inner);
            let captured = Arc::clone(&captured_inner);
            let scratch = cfg.scratch_dir.clone();
            async move {
                *called.lock().expect("not poisoned") += 1;
                *captured.lock().expect("not poisoned") = Some(scratch);
                Ok((
                    SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    },
                    Some(ExitSignal::Complete),
                    "LOOM_COMPLETE\n".to_string(),
                ))
            }
        };

        let mut walker = ProductionMintWalker::new(
            bd,
            SpecLabel::new("test-mint"),
            workspace.clone(),
            state,
            manifest,
            ProfileName::new("base"),
            spawn,
        );

        let findings = walk(&mut walker, &MintScope::Tree, &AlwaysValid)
            .await
            .expect("walk succeeds end-to-end");

        assert_eq!(
            *spawn_called.lock().expect("not poisoned"),
            1,
            "rubric agent spawn closure must fire exactly once on Tree scope",
        );
        assert!(
            captured_scope_dir.lock().expect("not poisoned").is_some(),
            "spawn closure must receive a scratch_dir from ScratchSession::open",
        );
        let unresolved: Vec<&Finding> = findings
            .iter()
            .filter(|f| {
                f.token == ConcernToken::UnresolvedAnnotation
                    && matches!(
                        &f.target,
                        FindingTarget::Annotation { target_string }
                            if target_string == UNRESOLVED_TARGET
                    )
            })
            .collect();
        assert_eq!(
            unresolved.len(),
            1,
            "integrity dispatch must emit one UnresolvedAnnotation finding for the fixture target; got findings={findings:?}",
        );
        assert_eq!(
            unresolved[0].bonds,
            vec![spec("test-mint")],
            "unresolved-annotation finding must bond to its owning spec",
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("verifier-cwd/ran.txt"))
                .expect("configured test runner wrote cwd marker"),
            "ok",
        );

        // Compile-time trait-bound pin: `ProductionMintWalker<...>` MUST
        // implement [`MintWalker`]. Reading the type's trait bounds via
        // a generic helper that requires the bound is the load-bearing
        // assertion — if the impl block ever drifts, this fails to
        // compile rather than at runtime.
        fn assert_is_mint_walker<W: MintWalker>(_w: &W) {}
        assert_is_mint_walker(&walker);
    }

    /// Spec contract `specs/gate.md` § Runners, *Runner-owned resolution*:
    /// a tree-scope `[check]` target resolves and dispatches **because a
    /// runner claims it**, not because its first token is on PATH. The
    /// production walker's `run_verifiers` threads the resolved
    /// `[runner.check.<name>]` specs into both the integrity gate
    /// forward-resolution *and* the `run_check` batched dispatch.
    ///
    /// Behavioral assertion: a `[check]` target whose `tokens[0]` is **not**
    /// on PATH but is matched by a `[runner.check]` block resolves cleanly —
    /// no `UnresolvedAnnotation` and no `DispatchError` finding tagged with
    /// the target. With the runner specs withheld from `run_check` (the
    /// pre-fix `&[]` path), the unmatched fallback spawns the missing binary
    /// literally and a `DispatchError` finding fires, so this test pins the
    /// driver-side `run_check` wiring.
    #[tokio::test]
    async fn mint_tree_scope_check_dispatches_runner_owned_target_without_finding() {
        use loom_driver::profile_manifest::ProfileImageManifest;
        use loom_driver::state::StateDb;

        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().to_path_buf();

        // tokens[0] is deliberately absent from PATH; only the runner match
        // can resolve and dispatch it.
        const RUNNER_OWNED_TARGET: &str = "loom-runner-only-fixture-7c3a-not-on-path";

        std::fs::create_dir_all(workspace.join("specs")).expect("specs dir");
        std::fs::write(
            workspace.join("specs/test-mint.md"),
            format!("# test-mint\n\n- exercise runner dispatch [check]({RUNNER_OWNED_TARGET})\n"),
        )
        .expect("spec");
        std::fs::write(
            workspace.join("loom.toml"),
            "[runner.check.fixture]\nmatch   = '^loom-runner-only-fixture'\ncommand = \"true {targets}\"\nparse   = \"exit-code\"\n",
        )
        .expect("loom.toml");
        let manifest_path = workspace.join("profile-images.json");
        std::fs::write(
            &manifest_path,
            r#"{"base":{"ref":"localhost/wrix-base:abc","source":"/nix/store/aaa"}}"#,
        )
        .expect("manifest");
        let manifest =
            Arc::new(ProfileImageManifest::from_path(&manifest_path).expect("manifest parse"));
        let state = Arc::new(StateDb::open(workspace.join(".loom/state.db")).expect("state db"));

        let responses = vec![ok_stdout("[]"), ok_stdout("[]")];
        let bd = BdClient::with_runner(ScriptedRunner::new(responses));

        let spawn = move |_cfg: SpawnConfig| async move {
            Ok((
                SessionOutcome {
                    exit_code: 0,
                    cost_usd: None,
                },
                Some(ExitSignal::Complete),
                "LOOM_COMPLETE\n".to_string(),
            ))
        };

        let mut walker = ProductionMintWalker::new(
            bd,
            SpecLabel::new("test-mint"),
            workspace.clone(),
            state,
            manifest,
            ProfileName::new("base"),
            spawn,
        );

        let findings = walk(&mut walker, &MintScope::Tree, &AlwaysValid)
            .await
            .expect("walk succeeds end-to-end");

        let target_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| {
                matches!(
                    &f.target,
                    FindingTarget::Annotation { target_string }
                        if target_string == RUNNER_OWNED_TARGET
                )
            })
            .collect();
        assert!(
            target_findings.is_empty(),
            "runner-owned [check] target must resolve and dispatch via its runner, not surface a finding; got {target_findings:?}",
        );
    }
}
