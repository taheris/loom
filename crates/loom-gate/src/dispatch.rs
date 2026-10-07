//! Per-tier dispatcher.
//!
//! Routes each [`Annotation`] to its verifier per the verifier-runner
//! contract in `specs/gate.md`. `[check]` annotations use matched-runner
//! batching with per-annotation fallback; `[system]` annotations share
//! equivalent executions within a run; `[test]` annotations collect into a single
//! batched runner invocation (filtered against `--files` scope via the
//! [`TestScope`] trait); `[judge]` annotations collect into a single
//! batched runner invocation (no scope filter — judges are LLM-driven
//! and don't reduce to file ownership).
//!
//! Each verifier subprocess receives `LOOM_FILES` (colon-joined paths)
//! and `LOOM_SPEC` (when set) on its environment and is expected to
//! emit one `{"pass": bool, "evidence": "<msg>"}` JSON line on stdout
//! with an exit code mirroring `pass`. For verifiers that do not
//! conform to the JSON-line contract (raw `cargo nextest`, bare
//! `grep -q`, `nix build`, etc.), the dispatcher falls back to
//! exit-code interpretation with the verifier's stdout surfaced as
//! evidence on pass and stderr on fail. The `[test]` runner
//! additionally undergoes silent-zero-match sniffing so a filtered
//! cargo / nextest / pytest invocation that matches no targets fails
//! loudly instead of passing on an empty selection.
//!
//! Pending-marked annotations (`[tier?](target)`) are filtered out
//! before any subprocess spawn per `specs/gate.md` § Pending modifier
//! — *Dispatch-side skip*. The verifier doesn't run, the dispatcher
//! emits no result for the entry, and the only enforcement on a
//! `?`-marked annotation comes from the integrity gate's
//! forward-resolution check (which fires `UnneededPendingMarker`
//! once the target resolves).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use displaydoc::Display;
use loom_driver::config::LoomConfig;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::annotation::{Annotation, Tier};
use crate::cache::Verdict;
use crate::runner::{
    BuiltinParser, RunnerError, RunnerGroup, RunnerSpec, RunnerTemplate, check_zero_match,
    group_by_runner, parse_runner_output,
};

/// JSON-line verdict returned by a verifier.
///
/// Per the verifier-runner contract in `specs/gate.md`, the exit code mirrors
/// `pass` (0 for true, non-zero for false); the gate parses one line of
/// JSON-encoded `VerifierVerdict` from each verifier's stdout. A
/// verifier may exit `77` (GNU test-suite skip convention) or emit
/// `"skipped": true` in the JSON line to report that the prerequisite
/// for running was not met — the dispatcher surfaces those as the
/// third verdict alongside pass/fail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "WireVerifierVerdict")]
pub struct VerifierVerdict {
    pub pass: bool,
    pub evidence: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<crate::runner::result::Execution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<crate::runner::result::SkipReason>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skip_permitted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub producer_error: Option<String>,
}

#[derive(Deserialize)]
struct WireVerifierVerdict {
    pass: bool,
    evidence: String,
    #[serde(default)]
    skipped: bool,
}

impl TryFrom<WireVerifierVerdict> for VerifierVerdict {
    type Error = String;

    fn try_from(wire: WireVerifierVerdict) -> Result<Self, Self::Error> {
        if wire.pass && wire.skipped {
            return Err("contradictory pass=true and skipped=true".into());
        }
        Ok(Self::from_outcome(
            if wire.skipped {
                Verdict::Skipped
            } else if wire.pass {
                Verdict::Pass
            } else {
                Verdict::Fail
            },
            wire.evidence,
        ))
    }
}

impl VerifierVerdict {
    /// Semantic outcome of the backward-compatible JSON flag representation.
    pub const fn outcome(&self) -> Verdict {
        if self.skipped {
            Verdict::Skipped
        } else if self.pass {
            Verdict::Pass
        } else {
            Verdict::Fail
        }
    }

    pub fn from_outcome(outcome: Verdict, evidence: String) -> Self {
        Self {
            pass: outcome == Verdict::Pass,
            skipped: outcome == Verdict::Skipped,
            evidence,
            execution: None,
            skip_reason: None,
            skip_permitted: false,
            producer_error: None,
        }
    }

    pub const fn accepted(&self) -> bool {
        self.producer_error.is_none() && (self.pass || (self.skipped && self.skip_permitted))
    }

    /// Structured metadata is retained in cache evidence, never in a passing skip row.
    ///
    /// # Errors
    /// Returns a serialization error if the evidence cannot be encoded.
    pub fn cache_evidence(&self) -> Result<String, serde_json::Error> {
        if self.execution.is_some() || self.producer_error.is_some() {
            serde_json::to_string(self)
        } else {
            Ok(self.evidence.clone())
        }
    }
}

/// GNU test-suite skip exit code (`AM_TESTS_ENVIRONMENT` / TAP-13).
/// Verifiers exit with this when a prerequisite (env var, tool, image)
/// is missing — distinct from a real failure.
pub const SKIP_EXIT_CODE: i32 = 77;

/// Failures the dispatcher surfaces. Per RS-4 each variant carries the
/// command string and original error so callers can route the error back
/// to a specific annotation source line.
#[derive(Debug, Display, Error)]
pub enum DispatchError {
    /// annotation target was empty for tier [{tier}]
    EmptyTarget { tier: Tier },
    /// failed to spawn verifier `{command}`: {source}
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    /// verifier `{command}` produced malformed JSON verdict: {source}
    MalformedVerdict {
        command: String,
        #[source]
        source: serde_json::Error,
    },
    /// verifier `{command}` produced an invalid verdict: {detail}
    InvalidVerdict { command: String, detail: String },
    /// runner zero-match: {source}
    ZeroMatch {
        #[source]
        source: RunnerError,
    },
    /// runner `{runner}` produced an invalid report: {source}
    RunnerOutput {
        runner: String,
        #[source]
        source: RunnerError,
    },
    /// runner `{runner}` did not report a verdict for target `{target}`
    MissingFromBatchOutput { runner: String, target: String },
}

/// Options shared by every dispatch entry point.
///
/// Carries the `--files` scope set (colon-joined into `LOOM_FILES`) and the optional `--spec` label
/// (forwarded as `LOOM_SPEC`). An empty `files` vec means "no `--files`
/// filter" — verifiers see an empty `LOOM_FILES` and batched tiers skip
/// scope intersection.
#[derive(Debug, Default, Clone)]
pub struct DispatchOptions {
    pub files: Vec<PathBuf>,
    pub spec: Option<String>,
}

/// Per-tier default working directories.
///
/// Used when neither the matched runner's `cwd` field nor an annotation's explicit override applies.
/// The dispatcher resolves the per-spawn cwd by walking
/// matched-runner > tier-default > repo-root in that order; this
/// struct carries the middle source. Repo-relative paths in this
/// struct are joined to the dispatcher's `repo_root` parameter at
/// spawn time.
#[derive(Debug, Default, Clone)]
pub struct TierCwds {
    pub check: Option<PathBuf>,
    pub test: Option<PathBuf>,
    pub system: Option<PathBuf>,
    pub judge: Option<PathBuf>,
}

impl TierCwds {
    /// Look up the default cwd configured for `tier`, if any.
    pub fn for_tier(&self, tier: Tier) -> Option<&Path> {
        match tier {
            Tier::Check => self.check.as_deref(),
            Tier::Test => self.test.as_deref(),
            Tier::System => self.system.as_deref(),
            Tier::Judge => self.judge.as_deref(),
        }
    }
}

/// Result of dispatching one verifier — either a single annotation
/// ([`run_check`] / [`run_system`]) or a batch of annotations sharing one
/// subprocess ([`run_test`] / [`run_judge`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchOutcome {
    pub annotations: Vec<Annotation>,
    pub verdict: VerifierVerdict,
}

/// Map a `[test]` annotation to its source-file scope so the dispatcher
/// can intersect against `--files` before issuing the batched runner.
///
/// Production implementations consult cargo metadata to walk the
/// transitive dependency graph for the annotation's owning crate; tests
/// substitute a deterministic stub. See [`EmptyScope`] for the
/// no-filtering default.
pub trait TestScope {
    /// Source files inside the annotation's scope (its owning crate plus
    /// transitive deps for Rust workspaces; toolchain-specific analogues
    /// elsewhere).
    fn scope_for(&self, annotation: &Annotation) -> Vec<PathBuf>;
}

/// Scope implementation that reports an empty set for every annotation.
///
/// With `--files` empty (no filter requested) the dispatcher skips
/// intersection and every annotation passes through; with `--files`
/// set, every annotation is filtered out. This is the safe default
/// before a cargo-metadata-backed scope lands.
pub struct EmptyScope;

impl TestScope for EmptyScope {
    fn scope_for(&self, _annotation: &Annotation) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// Dispatch all `[check]`-tier annotations.
///
/// Uses the matched-runner / per-annotation fallback composition described
/// in `specs/gate.md` § Runners. `specs` is the resolved
/// `[runner.check]` table; an empty slice degrades to per-annotation
/// spawn for every entry. Returns one result per Check-tier annotation
/// in input order.
///
/// When a runner claims several `cargo run -p loom-walk` targets, they
/// collapse to one subprocess. Targets no spec matches still spawn
/// their own process via the [`run_with_runners`] fallback.
pub fn run_check(
    annotations: &[Annotation],
    specs: &[RunnerSpec],
    options: &DispatchOptions,
    repo_root: &Path,
    tier_cwds: &TierCwds,
) -> Vec<Result<DispatchOutcome, DispatchError>> {
    let check_only: Vec<Annotation> = annotations
        .iter()
        .filter(|a| a.tier == Tier::Check && !a.pending)
        .cloned()
        .collect();
    run_with_runners(&check_only, specs, options, repo_root, tier_cwds)
}

/// Dispatch all `[system]`-tier annotations.
///
/// Equivalent invocations execute once per run, retaining one result per
/// annotation. Distinct targets remain separate scenarios even when a runner
/// renders identical commands. No execution is reused across gate runs.
pub fn run_system(
    annotations: &[Annotation],
    specs: &[RunnerSpec],
    options: &DispatchOptions,
    repo_root: &Path,
    tier_cwds: &TierCwds,
) -> Vec<Result<DispatchOutcome, DispatchError>> {
    iter_system(annotations, specs, options, repo_root, tier_cwds).collect()
}

#[derive(PartialEq, Eq, Hash)]
struct SystemInvocation {
    command: String,
    cwd: PathBuf,
    runner: Option<usize>,
    scenario: String,
    files: Vec<PathBuf>,
    spec: Option<String>,
    environment: BTreeMap<OsString, OsString>,
}

/// Lazily dispatch system annotations with criterion-level evidence.
/// Scope, environment and scenario identity bound run-local sharing.
pub fn iter_system<'a>(
    annotations: &'a [Annotation],
    specs: &'a [RunnerSpec],
    options: &'a DispatchOptions,
    repo_root: &'a Path,
    tier_cwds: &'a TierCwds,
) -> impl Iterator<Item = Result<DispatchOutcome, DispatchError>> + 'a {
    let mut executions = HashMap::new();
    annotations
        .iter()
        .filter(|ann| ann.tier == Tier::System && !ann.pending)
        .map(move |ann| {
            let (groups, _) = group_by_runner(specs, std::slice::from_ref(ann));
            let group = groups.first();
            let command =
                group.map_or_else(|| ann.target.trim().to_owned(), RunnerGroup::render_command);
            if command.is_empty() {
                return Err(DispatchError::EmptyTarget { tier: Tier::System });
            }
            let runner = specs
                .iter()
                .position(|spec| spec.applies_to(ann.tier) && spec.matches(&ann.target));
            let cwd = resolve_cwd(
                group.and_then(|group| group.spec.cwd.as_deref()),
                tier_cwds.system.as_deref(),
                repo_root,
            );
            let invocation = SystemInvocation {
                command: command.clone(),
                cwd: cwd.clone(),
                runner,
                scenario: ann.target.clone(),
                files: options.files.clone(),
                spec: options.spec.clone(),
                environment: std::env::vars_os().collect(),
            };
            let output = executions.entry(invocation).or_insert_with(|| {
                spawn_command(&command, options, Some(&cwd))
                    .map_err(|error| (error.kind(), error.to_string()))
            });
            let output = output
                .as_ref()
                .map_err(|(kind, message)| DispatchError::Spawn {
                    command: command.clone(),
                    source: std::io::Error::new(*kind, message.clone()),
                })?;
            if let Some(group) = group {
                return interpret_group_output(group, &command, output)
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| {
                        Err(DispatchError::MissingFromBatchOutput {
                            runner: group.spec.name.clone(),
                            target: group.matched[0].rendered_target.clone(),
                        })
                    });
            }
            Ok(DispatchOutcome {
                annotations: vec![ann.clone()],
                verdict: interpret_fallback_output(&command, None, output)?,
            })
        })
}

/// Dispatch all `[test]` annotations as one batched runner subprocess.
///
/// Targets are filtered by `--files` scope
/// via the [`TestScope`] resolver before being passed to the runner
/// template; an empty filter result returns `Ok(None)` so the caller
/// can distinguish "skipped — no scope match" from a true verdict.
///
/// # Errors
///
/// Returns an error when a verifier cannot be selected, prepared, or dispatched.
pub fn run_test(
    annotations: &[Annotation],
    options: &DispatchOptions,
    template: &RunnerTemplate,
    scope: &dyn TestScope,
) -> Result<Option<DispatchOutcome>, DispatchError> {
    run_test_in(annotations, options, template, scope, None)
}

/// Dispatch every `[test]` annotation with an explicit runner cwd.
///
/// # Errors
///
/// Returns an error when a verifier cannot be selected, prepared, or dispatched.
pub fn run_test_in(
    annotations: &[Annotation],
    options: &DispatchOptions,
    template: &RunnerTemplate,
    scope: &dyn TestScope,
    cwd: Option<&Path>,
) -> Result<Option<DispatchOutcome>, DispatchError> {
    let candidates: Vec<&Annotation> = annotations
        .iter()
        .filter(|a| a.tier == Tier::Test && !a.pending)
        .collect();
    let filtered = filter_by_files(&candidates, &options.files, scope);
    if filtered.is_empty() {
        return Ok(None);
    }
    let targets: Vec<&str> = filtered.iter().map(|a| a.target.as_str()).collect();
    let command = template.render(&targets);
    let verdict = run_with_fallback(&command, options, Some(&targets), cwd)?;
    Ok(Some(DispatchOutcome {
        annotations: filtered.into_iter().cloned().collect(),
        verdict,
    }))
}

/// Run test annotations with configured per-target parsers or legacy batch templates.
///
/// Explicit parsers and named runners retain individual test outcomes. Command-only
/// legacy configurations keep their aggregate wire contract and toolchain discovery.
/// Empty scoped selections do not discover runners or spawn processes.
///
/// # Errors
/// Returns an error for invalid runner configuration or an unclaimed test target.
pub fn run_configured_tests(
    annotations: &[Annotation],
    options: &DispatchOptions,
    config: &LoomConfig,
    repo_root: &Path,
    scope: &dyn TestScope,
) -> Result<Vec<Result<DispatchOutcome, DispatchError>>, RunnerError> {
    let candidates = annotations
        .iter()
        .filter(|ann| ann.tier == Tier::Test && !ann.pending)
        .collect::<Vec<_>>();
    let selected = filter_by_files(&candidates, &options.files, scope)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let tier = config.runner.tier("test");
    let tier_cwds = TierCwds {
        test: tier.and_then(|tier| tier.cwd.as_ref()).map(PathBuf::from),
        ..TierCwds::default()
    };
    let structured = tier.is_some_and(|tier| {
        tier.parse.is_some()
            || tier.target.is_some()
            || tier.join.is_some()
            || tier.match_regex.is_some()
            || !tier.runners.is_empty()
    });
    if structured {
        let specs = crate::runner::compile_tier_runners(config, "test")?;
        let (_, unmatched) = group_by_runner(&specs, &selected);
        if let Some(annotation) = unmatched.first() {
            return Err(RunnerError::UnclaimedTestTarget {
                target: annotation.target.clone(),
            });
        }
        return Ok(run_with_runners(
            &selected, &specs, options, repo_root, &tier_cwds,
        ));
    }
    let template = match tier.and_then(|tier| tier.command.as_ref()) {
        Some(command) => RunnerTemplate::new(command),
        None => crate::runner::discover(repo_root, Tier::Test)?,
    };
    let cwd = resolve_cwd(None, tier_cwds.test.as_deref(), repo_root);
    Ok(
        match run_test_in(&selected, options, &template, scope, Some(&cwd)) {
            Ok(Some(outcome)) => vec![Ok(outcome)],
            Ok(None) => Vec::new(),
            Err(error) => vec![Err(error)],
        },
    )
}

/// Dispatch every `[judge]`-tier annotation in `annotations` as one
/// batched runner subprocess. Judges aren't `--files`-filterable, so
/// every judge annotation is included.
///
/// # Errors
///
/// Returns an error when a verifier cannot be selected, prepared, or dispatched.
pub fn run_judge(
    annotations: &[Annotation],
    options: &DispatchOptions,
    template: &RunnerTemplate,
) -> Result<Option<DispatchOutcome>, DispatchError> {
    let judges: Vec<&Annotation> = annotations
        .iter()
        .filter(|a| a.tier == Tier::Judge && !a.pending)
        .collect();
    if judges.is_empty() {
        return Ok(None);
    }
    let targets: Vec<&str> = judges.iter().map(|a| a.target.as_str()).collect();
    let command = template.render(&targets);
    let verdict = run_with_fallback(&command, options, None, None)?;
    Ok(Some(DispatchOutcome {
        annotations: judges.into_iter().cloned().collect(),
        verdict,
    }))
}

/// Dispatch annotations through batched runner specifications.
///
/// Per `specs/gate.md` § Runners, annotations are grouped
/// by which spec matches their target (first match wins, declaration
/// order); each group spawns one subprocess and parses per-target
/// verdicts via the spec's [`BuiltinParser`]. Annotations no spec
/// matches fall back to per-annotation spawn. System callers use
/// [`iter_system`] for equivalent-execution sharing.
///
/// Results are returned in input order, one per input annotation:
///
/// - `Ok(outcome)` — the runner produced a verdict for this target.
/// - `Err(DispatchError::MissingFromBatchOutput { .. })` — the runner
///   ran but emitted no row covering this target. Treated as a
///   dispatch failure (exit-code-2 semantics) at the push gate.
/// - `Err(DispatchError::Spawn { .. })` / other variants — the runner
///   could not be spawned at all; every annotation it claimed
///   shares the same error.
///
/// `repo_root` is the workspace root used to resolve per-runner
/// [`RunnerSpec::cwd`] overrides; an absolute `cwd` is honoured as-is.
/// `tier_cwds` carries the `[runner.<tier>] cwd = "..."` defaults; per
/// `specs/gate.md` § Runners the per-spawn cwd resolves as
/// matched-runner > tier-default > repo-root.
pub fn run_with_runners(
    annotations: &[Annotation],
    specs: &[RunnerSpec],
    options: &DispatchOptions,
    repo_root: &Path,
    tier_cwds: &TierCwds,
) -> Vec<Result<DispatchOutcome, DispatchError>> {
    let (groups, unmatched) = group_by_runner(specs, annotations);

    let mut per_index: Vec<Option<Result<DispatchOutcome, DispatchError>>> =
        (0..annotations.len()).map(|_| None).collect();
    let position_of: std::collections::HashMap<*const Annotation, usize> = annotations
        .iter()
        .enumerate()
        .map(|(i, a)| (std::ptr::from_ref::<Annotation>(a), i))
        .collect();

    for group in groups {
        let results = dispatch_group(&group, options, repo_root, tier_cwds);
        for (matched, result) in group.matched.iter().zip(results) {
            if let Some(&idx) =
                position_of.get(&std::ptr::from_ref::<Annotation>(matched.annotation))
            {
                per_index[idx] = Some(result);
            }
        }
    }
    for ann in unmatched {
        if let Some(&idx) = position_of.get(&std::ptr::from_ref::<Annotation>(ann)) {
            let cwd = resolve_cwd(None, tier_cwds.for_tier(ann.tier), repo_root);
            per_index[idx] = Some(run_single_in(ann, options, Some(cwd.as_path())));
        }
    }
    per_index
        .into_iter()
        .map(|slot| slot.unwrap_or(Err(DispatchError::EmptyTarget { tier: Tier::Check })))
        .collect()
}

/// Compute the effective working directory for one spawn per the
/// matched-runner > tier-default > repo-root chain in
/// `specs/gate.md` § Runners. Repo-relative paths are joined to
/// `repo_root`; absolute paths round-trip unchanged.
fn resolve_cwd(runner_cwd: Option<&Path>, tier_cwd: Option<&Path>, repo_root: &Path) -> PathBuf {
    let chosen = runner_cwd.or(tier_cwd);
    match chosen {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        Some(p) => repo_root.join(p),
        None => repo_root.to_path_buf(),
    }
}

fn dispatch_group(
    group: &RunnerGroup<'_, '_>,
    options: &DispatchOptions,
    repo_root: &Path,
    tier_cwds: &TierCwds,
) -> Vec<Result<DispatchOutcome, DispatchError>> {
    let command = group.render_command();
    let tier_default = group
        .matched
        .first()
        .map(|m| m.annotation.tier)
        .and_then(|t| tier_cwds.for_tier(t));
    let cwd = resolve_cwd(group.spec.cwd.as_deref(), tier_default, repo_root);
    let output = match spawn_in(&command, options, Some(cwd.as_path())) {
        Ok(o) => o,
        Err(err) => {
            let message = err.to_string();
            return group
                .matched
                .iter()
                .map(|_| {
                    Err(DispatchError::Spawn {
                        command: command.clone(),
                        source: std::io::Error::other(message.clone()),
                    })
                })
                .collect();
        }
    };
    interpret_group_output(group, &command, &output)
}

fn interpret_group_output(
    group: &RunnerGroup<'_, '_>,
    command: &str,
    output: &Output,
) -> Vec<Result<DispatchOutcome, DispatchError>> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let skipped = output.status.code() == Some(SKIP_EXIT_CODE);
    if matches!(group.spec.parse, BuiltinParser::ExitCode) {
        let pass = output.status.success();
        let evidence = if pass || (skipped && stderr.trim().is_empty()) {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        return group
            .matched
            .iter()
            .map(|matched| {
                Ok(DispatchOutcome {
                    annotations: vec![matched.annotation.clone()],
                    verdict: VerifierVerdict::from_outcome(
                        if skipped {
                            Verdict::Skipped
                        } else if pass {
                            Verdict::Pass
                        } else {
                            Verdict::Fail
                        },
                        evidence.clone(),
                    ),
                })
            })
            .collect();
    }

    let parsed =
        match parse_runner_output(group.spec.parse, &stdout, &stderr, output.status.success()) {
            Ok(parsed) => parsed,
            Err(source) => {
                return group
                    .matched
                    .iter()
                    .map(|_| {
                        Err(DispatchError::RunnerOutput {
                            runner: group.spec.name.clone(),
                            source: source.clone(),
                        })
                    })
                    .collect();
            }
        };
    let requested = group
        .matched
        .iter()
        .map(|matched| matched.rendered_target.as_str())
        .collect::<HashSet<_>>();
    if group.spec.parse == BuiltinParser::JsonLines
        && let Some(unexpected) = parsed
            .keys()
            .find(|target| !requested.contains(target.as_str()))
    {
        return group
            .matched
            .iter()
            .map(|_| {
                Err(DispatchError::InvalidVerdict {
                    command: command.to_owned(),
                    detail: format!("unexpected target `{unexpected}`"),
                })
            })
            .collect();
    }
    let has_failure = parsed
        .values()
        .any(|verdict| verdict.outcome == Verdict::Fail);
    let has_skip = parsed
        .values()
        .any(|verdict| verdict.outcome == Verdict::Skipped);
    let code = output.status.code();
    let consistent = if has_failure {
        code.is_some_and(|code| code != 0 && code != SKIP_EXIT_CODE)
    } else if has_skip {
        code == Some(SKIP_EXIT_CODE)
    } else {
        output.status.success()
    };
    let producer_error = if (group.spec.parse == BuiltinParser::JsonLines && !consistent)
        || (!output.status.success() && !skipped && !has_failure)
        || (skipped && !has_skip)
    {
        Some(format!(
            "producer exit {code:?} contradicts per-target outcomes"
        ))
    } else {
        None
    };
    group
        .matched
        .iter()
        .map(|matched| match parsed.get(&matched.rendered_target) {
            Some(verdict) => Ok(DispatchOutcome {
                annotations: vec![matched.annotation.clone()],
                verdict: VerifierVerdict {
                    execution: verdict.execution.clone(),
                    skip_reason: verdict.skip_reason.clone(),
                    skip_permitted: verdict.outcome == Verdict::Skipped
                        && crate::runner::result::permits_skip(
                            group.spec.skip_policy,
                            &group.spec.skip_capabilities,
                            verdict.execution.as_ref(),
                            verdict.skip_reason.as_ref(),
                        ),
                    producer_error: producer_error.clone(),
                    ..VerifierVerdict::from_outcome(verdict.outcome, verdict.evidence.clone())
                },
            }),
            None => Err(DispatchError::MissingFromBatchOutput {
                runner: group.spec.name.clone(),
                target: matched.rendered_target.clone(),
            }),
        })
        .collect()
}

fn run_single_in(
    annotation: &Annotation,
    options: &DispatchOptions,
    cwd: Option<&Path>,
) -> Result<DispatchOutcome, DispatchError> {
    let command = annotation.target.trim();
    if command.is_empty() {
        return Err(DispatchError::EmptyTarget {
            tier: annotation.tier,
        });
    }
    let verdict = run_with_fallback(command, options, None, cwd)?;
    Ok(DispatchOutcome {
        annotations: vec![annotation.clone()],
        verdict,
    })
}

fn run_with_fallback(
    command: &str,
    options: &DispatchOptions,
    test_targets: Option<&[&str]>,
    cwd: Option<&Path>,
) -> Result<VerifierVerdict, DispatchError> {
    let output = spawn_in(command, options, cwd)?;
    interpret_fallback_output(command, test_targets, &output)
}

fn interpret_fallback_output(
    command: &str,
    test_targets: Option<&[&str]>,
    output: &Output,
) -> Result<VerifierVerdict, DispatchError> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let skipped = output.status.code() == Some(SKIP_EXIT_CODE);
    let reported = parse_verdict_optional(command, &stdout)?;
    if let Some(targets) = test_targets.filter(|_| !skipped) {
        let filtered_nextest_skips = crate::runner::RunnerKind::classify(command)
            == crate::runner::RunnerKind::CargoNextest
            && command
                .split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .any(|pair| pair == ["--status-level", "skip"])
            && crate::runner::nextest_targets_passed(targets, &stdout, &stderr);
        if let Some((outcome, evidence)) =
            crate::runner::unstructured_test_outcome(command, &stdout, &stderr)
            && !(outcome == Verdict::Skipped && filtered_nextest_skips)
            && (outcome == Verdict::Fail
                || (output.status.success()
                    && !reported
                        .as_ref()
                        .is_some_and(|verdict| verdict.outcome() == Verdict::Fail)))
        {
            return Ok(VerifierVerdict::from_outcome(outcome, evidence));
        }
        check_zero_match(command, &stdout, &stderr)
            .map_err(|e| DispatchError::ZeroMatch { source: e })?;
    }
    if let Some(verdict) = reported {
        let mut verdict = verdict;
        if (skipped && !verdict.skipped)
            || (!output.status.success() && !skipped && verdict.pass)
            || (output.status.success() && verdict.skipped)
        {
            verdict.producer_error = Some(format!(
                "producer exit {:?} contradicts JSON verdict",
                output.status.code()
            ));
        }
        return Ok(verdict);
    }
    if skipped {
        let evidence = if stderr.trim().is_empty() {
            stdout.into_owned()
        } else {
            stderr.into_owned()
        };
        return Ok(VerifierVerdict::from_outcome(Verdict::Skipped, evidence));
    }
    Ok(VerifierVerdict::from_outcome(
        if output.status.success() {
            Verdict::Pass
        } else {
            Verdict::Fail
        },
        if output.status.success() {
            stdout.into_owned()
        } else {
            stderr.into_owned()
        },
    ))
}

fn filter_by_files<'a>(
    candidates: &[&'a Annotation],
    files: &[PathBuf],
    scope: &dyn TestScope,
) -> Vec<&'a Annotation> {
    if files.is_empty() {
        return candidates.to_vec();
    }
    let file_set: HashSet<&PathBuf> = files.iter().collect();
    candidates
        .iter()
        .copied()
        .filter(|a| {
            scope
                .scope_for(a)
                .iter()
                .any(|path| file_set.contains(path))
        })
        .collect()
}

fn spawn_in(
    command: &str,
    options: &DispatchOptions,
    cwd: Option<&std::path::Path>,
) -> Result<Output, DispatchError> {
    spawn_command(command, options, cwd).map_err(|source| DispatchError::Spawn {
        command: command.to_string(),
        source,
    })
}

fn spawn_command(
    command: &str,
    options: &DispatchOptions,
    cwd: Option<&Path>,
) -> std::io::Result<Output> {
    let invalid = |message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message);
    let mut tokens = shlex::split(command)
        .ok_or_else(|| invalid("unbalanced quotes"))?
        .into_iter();
    let head = tokens.next().ok_or_else(|| invalid("empty command"))?;
    let tail: Vec<String> = tokens.collect();
    let mut cmd = Command::new(head);
    cmd.args(&tail);
    cmd.env("LOOM_FILES", encode_files(&options.files));
    if let Some(spec) = &options.spec {
        cmd.env("LOOM_SPEC", spec);
    }
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.output()
}

fn encode_files(files: &[PathBuf]) -> String {
    files
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(":")
}

fn parse_verdict_optional(
    command: &str,
    stdout: &str,
) -> Result<Option<VerifierVerdict>, DispatchError> {
    let mut reported = None;
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        match serde_json::from_str::<VerifierVerdict>(line) {
            Ok(v) => {
                if reported.is_some() {
                    return Err(DispatchError::InvalidVerdict {
                        command: command.to_string(),
                        detail: "duplicate verdict".into(),
                    });
                }
                reported = Some(v);
            }
            Err(source) if line_attempts_verdict(line) => {
                return Err(DispatchError::MalformedVerdict {
                    command: command.to_string(),
                    source,
                });
            }
            Err(_) => {}
        }
    }
    Ok(reported)
}

/// `true` when the line parses as a JSON object with a `pass` key —
/// the signal that the verifier is attempting to speak the verdict
/// contract.
fn line_attempts_verdict(line: &str) -> bool {
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(value) => value.as_object().is_some_and(|object| {
            object.contains_key("pass")
                || object.contains_key("outcome")
                || object.contains_key("skipped")
        }),
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn ann(tier: Tier, target: &str) -> Annotation {
        Annotation {
            tier,
            target: target.into(),
            source_spec: PathBuf::from("specs/a.md"),
            line: 1,
            criterion_line: 1,
            pending: false,
        }
    }

    #[test]
    fn system_invocation_equivalence_includes_command_cwd_environment_scope_and_scenario() {
        let invocation = || SystemInvocation {
            command: "bash verify.sh".into(),
            cwd: "scenario".into(),
            runner: Some(0),
            scenario: "fixture:a".into(),
            files: vec!["a.rs".into()],
            spec: Some("a".into()),
            environment: [(OsString::from("FIXTURE"), OsString::from("a"))].into(),
        };
        assert!(invocation() == invocation());
        let mut different = invocation();
        different.command = "bash other.sh".into();
        assert!(invocation() != different);
        different = invocation();
        different.cwd = "other".into();
        assert!(invocation() != different);
        different = invocation();
        different.environment.insert("FIXTURE".into(), "b".into());
        assert!(invocation() != different);
        different = invocation();
        different.files = vec!["b.rs".into()];
        assert!(invocation() != different);
        different = invocation();
        different.spec = Some("b".into());
        assert!(invocation() != different);
        different = invocation();
        different.scenario = "fixture:b".into();
        assert!(invocation() != different);
        different = invocation();
        different.runner = Some(1);
        assert!(invocation() != different);
    }

    #[test]
    fn skipped_wire_verdict_cannot_claim_an_observed_pass() {
        assert!(
            serde_json::from_str::<VerifierVerdict>(
                r#"{"pass":true,"evidence":"not run","skipped":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn verdict_round_trips_through_json() {
        let v = VerifierVerdict::from_outcome(Verdict::Pass, "ok".into());
        let s = serde_json::to_string(&v).unwrap();
        assert_eq!(s, r#"{"pass":true,"evidence":"ok"}"#);
        let back: VerifierVerdict = serde_json::from_str(&s).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn skipped_verdict_serializes_with_skipped_flag() {
        let v = VerifierVerdict::from_outcome(Verdict::Skipped, "no image".into());
        let s = serde_json::to_string(&v).unwrap();
        assert_eq!(s, r#"{"pass":false,"evidence":"no image","skipped":true}"#);
        let back: VerifierVerdict = serde_json::from_str(&s).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn legacy_two_field_json_deserialises_with_skipped_false() {
        let json = r#"{"pass": true, "evidence": "ok"}"#;
        let v: VerifierVerdict = serde_json::from_str(json).unwrap();
        assert!(v.pass);
        assert!(!v.skipped);
    }

    #[test]
    fn parse_verdict_optional_picks_last_json_line() {
        let stdout = "warning: deprecation\nfoo bar\n{\"pass\": true, \"evidence\": \"ok\"}\n";
        let v = parse_verdict_optional("cmd", stdout).unwrap().unwrap();
        assert!(v.pass);
        assert_eq!(v.evidence, "ok");
    }

    #[test]
    fn parse_verdict_optional_returns_none_when_no_json() {
        let stdout = "no JSON here\nrunning some tests\nall good\n";
        assert!(parse_verdict_optional("cmd", stdout).unwrap().is_none());
    }

    #[test]
    fn parse_verdict_optional_surfaces_malformed_verdict_with_wrong_type() {
        let stdout = "{\"pass\": \"yes\", \"evidence\": \"ok\"}\n";
        let err = parse_verdict_optional("cmd", stdout).unwrap_err();
        assert!(matches!(err, DispatchError::MalformedVerdict { .. }));
    }

    #[test]
    fn parse_verdict_optional_rejects_unparseable_json() {
        let stdout = "{\"pass\": maybe}\n";
        assert!(matches!(
            parse_verdict_optional("cmd", stdout),
            Err(DispatchError::MalformedVerdict { .. })
        ));
    }

    #[test]
    fn fallback_duplicate_verdict_cannot_overwrite_failure() {
        let stdout = "{\"pass\":false,\"evidence\":\"failed\"}\n{\"pass\":true,\"evidence\":\"overwritten\"}\n";
        assert!(matches!(
            parse_verdict_optional("cmd", stdout),
            Err(DispatchError::InvalidVerdict { .. })
        ));
    }

    #[test]
    fn parse_verdict_optional_skips_incidental_json_without_pass_key() {
        let stdout = "{\"some\": \"data\", \"more\": true}\n";
        assert!(parse_verdict_optional("cmd", stdout).unwrap().is_none());
    }

    #[test]
    fn parse_verdict_optional_finds_verdict_above_incidental_trailing_json() {
        let stdout = concat!(
            "{\"pass\": true, \"evidence\": \"ok\"}\n",
            "{\"unrelated\": \"trailing output\"}\n",
        );
        let v = parse_verdict_optional("cmd", stdout).unwrap().unwrap();
        assert!(v.pass);
        assert_eq!(v.evidence, "ok");
    }

    #[test]
    fn parse_verdict_optional_errors_when_verdict_attempt_missing_evidence() {
        let stdout = "{\"pass\": true}\n";
        let err = parse_verdict_optional("cmd", stdout).unwrap_err();
        assert!(matches!(err, DispatchError::MalformedVerdict { .. }));
    }

    #[test]
    fn empty_scope_returns_empty_for_every_annotation() {
        let scope = EmptyScope;
        let a = ann(Tier::Test, "crate::a::ok");
        assert_eq!(scope.scope_for(&a).len(), 0);
    }

    struct StubScope(std::collections::HashMap<String, Vec<PathBuf>>);

    impl TestScope for StubScope {
        fn scope_for(&self, a: &Annotation) -> Vec<PathBuf> {
            self.0.get(&a.target).cloned().unwrap_or_default()
        }
    }

    #[test]
    fn filter_by_files_keeps_intersecting_annotations() {
        let a = ann(Tier::Test, "crate::a::keep");
        let b = ann(Tier::Test, "crate::b::drop");
        let candidates = vec![&a, &b];
        let scope = StubScope(
            [
                (
                    "crate::a::keep".to_string(),
                    vec![PathBuf::from("src/a.rs")],
                ),
                (
                    "crate::b::drop".to_string(),
                    vec![PathBuf::from("src/b.rs")],
                ),
            ]
            .into_iter()
            .collect(),
        );
        let files = vec![PathBuf::from("src/a.rs")];
        let kept = filter_by_files(&candidates, &files, &scope);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].target, "crate::a::keep");
    }

    #[test]
    fn filter_by_files_with_empty_filter_passes_everything_through() {
        let a = ann(Tier::Test, "x");
        let b = ann(Tier::Test, "y");
        let candidates = vec![&a, &b];
        let scope = EmptyScope;
        let kept = filter_by_files(&candidates, &[], &scope);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn filter_by_files_with_empty_scope_drops_everything_when_filter_set() {
        let a = ann(Tier::Test, "x");
        let candidates = vec![&a];
        let scope = EmptyScope;
        let files = vec![PathBuf::from("src/a.rs")];
        let kept = filter_by_files(&candidates, &files, &scope);
        assert_eq!(kept.len(), 0);
    }

    #[test]
    fn encode_files_joins_with_colon() {
        let files = vec![
            PathBuf::from("a/b.rs"),
            PathBuf::from("c.rs"),
            PathBuf::from("d/e/f.rs"),
        ];
        assert_eq!(encode_files(&files), "a/b.rs:c.rs:d/e/f.rs");
    }

    #[test]
    fn encode_files_empty_yields_empty_string() {
        assert_eq!(encode_files(&[]), "");
    }

    #[test]
    fn dispatch_options_default_is_unfiltered() {
        let opts = DispatchOptions::default();
        assert_eq!(opts.files.len(), 0);
        assert!(opts.spec.is_none());
    }

    #[test]
    fn empty_target_error_message_names_the_tier() {
        let e = DispatchError::EmptyTarget { tier: Tier::Check };
        assert_eq!(
            e.to_string(),
            "annotation target was empty for tier [check]"
        );
    }

    #[test]
    fn run_check_returns_one_result_per_check_annotation() {
        let inputs = vec![
            ann(Tier::Check, "/no/such/script-1"),
            ann(Tier::System, "/no/such/system"),
            ann(Tier::Check, "/no/such/script-2"),
        ];
        let opts = DispatchOptions::default();
        let dir = tempfile::tempdir().unwrap();
        let results = run_check(&inputs, &[], &opts, dir.path(), &TierCwds::default());
        assert_eq!(results.len(), 2, "filters to Check tier only");
        for r in &results {
            assert!(matches!(r, Err(DispatchError::Spawn { .. })));
        }
    }

    #[test]
    fn run_system_returns_one_result_per_system_annotation() {
        let inputs = vec![
            ann(Tier::System, "/no/such/system-1"),
            ann(Tier::Check, "/no/such/check"),
            ann(Tier::System, "/no/such/system-2"),
        ];
        let opts = DispatchOptions::default();
        let dir = tempfile::tempdir().unwrap();
        let results = run_system(&inputs, &[], &opts, dir.path(), &TierCwds::default());
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn run_test_returns_none_when_no_test_annotations_present() {
        let inputs = vec![ann(Tier::Check, "x")];
        let template = RunnerTemplate::new("cargo nextest run -E 'test({paths})'");
        let opts = DispatchOptions::default();
        let result = run_test(&inputs, &opts, &template, &EmptyScope).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn run_test_returns_none_when_scope_filter_excludes_every_annotation() {
        let inputs = vec![ann(Tier::Test, "crate::a::ok")];
        let template = RunnerTemplate::new("cargo nextest run -E 'test({paths})'");
        let opts = DispatchOptions {
            files: vec![PathBuf::from("src/a.rs")],
            spec: None,
        };
        let result = run_test(&inputs, &opts, &template, &EmptyScope).unwrap();
        assert!(
            result.is_none(),
            "EmptyScope intersected against non-empty files filter excludes all"
        );
    }

    #[test]
    fn run_judge_returns_none_when_no_judge_annotations_present() {
        let inputs = vec![ann(Tier::Test, "crate::a::ok")];
        let template = RunnerTemplate::new("loom-judge {paths}");
        let opts = DispatchOptions::default();
        let result = run_judge(&inputs, &opts, &template).unwrap();
        assert!(result.is_none());
    }

    /// Lightweight subprocess assertion: verify [`run_check`] invokes
    /// the annotation target with the env contract and parses the JSON
    /// verdict line. Targets `sh <script>` rather than executing the
    /// script directly so the test doesn't depend on a chmod race with
    /// the kernel's `ETXTBSY` guard on freshly-written executables.
    #[test]
    fn run_check_spawns_subprocess_and_parses_verdict_from_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("verifier.sh");
        std::fs::write(
            &script,
            "printf '{\"pass\": true, \"evidence\": \"hello\"}\\n'\n",
        )
        .unwrap();

        let target = format!("sh {}", script.display());
        let inputs = vec![ann(Tier::Check, &target)];
        let opts = DispatchOptions::default();
        let results = run_check(&inputs, &[], &opts, dir.path(), &TierCwds::default());
        assert_eq!(results.len(), 1);
        let outcome = results.into_iter().next().unwrap().unwrap();
        assert!(outcome.verdict.pass);
        assert_eq!(outcome.verdict.evidence, "hello");
    }

    #[test]
    fn run_check_propagates_env_loom_files_and_loom_spec_to_subprocess() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("envcheck.sh");
        std::fs::write(
            &script,
            "printf '{\"pass\": true, \"evidence\": \"FILES=%s SPEC=%s\"}\\n' \"$LOOM_FILES\" \"$LOOM_SPEC\"\n",
        )
        .unwrap();

        let target = format!("sh {}", script.display());
        let inputs = vec![ann(Tier::Check, &target)];
        let opts = DispatchOptions {
            files: vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")],
            spec: Some("tests".into()),
        };
        let results = run_check(&inputs, &[], &opts, dir.path(), &TierCwds::default());
        let outcome = results.into_iter().next().unwrap().unwrap();
        assert_eq!(outcome.verdict.evidence, "FILES=a.rs:b.rs SPEC=tests");
    }
}
