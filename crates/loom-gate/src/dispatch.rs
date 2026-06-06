//! Per-tier dispatcher.
//!
//! Routes each [`Annotation`] to its verifier per the verifier-runner
//! contract in `specs/gate.md`. `[check]` annotations use matched-runner
//! batching with per-annotation fallback; `[system]` annotations spawn one
//! subprocess per annotation; `[test]` annotations collect into a single
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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::annotation::{Annotation, Tier};
use crate::runner::{
    BuiltinParser, RunnerError, RunnerGroup, RunnerSpec, RunnerTemplate, check_zero_match,
    group_by_runner, parse_runner_output,
};

/// JSON-line verdict every verifier returns on stdout, per the
/// verifier-runner contract in `specs/gate.md`. The exit code mirrors
/// `pass` (0 for true, non-zero for false); the gate parses one line of
/// JSON-encoded `VerifierVerdict` from each verifier's stdout. A
/// verifier may exit `77` (GNU test-suite skip convention) or emit
/// `"skipped": true` in the JSON line to report that the prerequisite
/// for running was not met — the dispatcher surfaces those as the
/// third verdict alongside pass/fail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierVerdict {
    pub pass: bool,
    pub evidence: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
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
    /// runner zero-match: {source}
    ZeroMatch {
        #[source]
        source: RunnerError,
    },
    /// runner `{runner}` did not report a verdict for target `{target}`
    MissingFromBatchOutput { runner: String, target: String },
}

/// Options shared by every dispatch entry point: the `--files` scope set
/// (colon-joined into `LOOM_FILES`) and the optional `--spec` label
/// (forwarded as `LOOM_SPEC`). An empty `files` vec means "no `--files`
/// filter" — verifiers see an empty `LOOM_FILES` and batched tiers skip
/// scope intersection.
#[derive(Debug, Default, Clone)]
pub struct DispatchOptions {
    pub files: Vec<PathBuf>,
    pub spec: Option<String>,
}

/// Per-tier default working directories used when neither the matched
/// runner's `cwd` field nor an annotation's explicit override applies.
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

/// Scope impl that reports the empty set for every annotation. With
/// `--files` empty (no filter requested) the dispatcher skips
/// intersection and every annotation passes through; with `--files`
/// set, every annotation is filtered out. This is the safe default
/// before a cargo-metadata-backed scope lands.
pub struct EmptyScope;

impl TestScope for EmptyScope {
    fn scope_for(&self, _annotation: &Annotation) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// Dispatch every `[check]`-tier annotation in `annotations` through
/// the matched-runner / per-annotation fallback composition described
/// in `specs/gate.md` § Runners. `specs` is the resolved
/// `[runner.check]` table; an empty slice degrades to per-annotation
/// spawn for every entry (today's behaviour with no `[runner.check]`
/// config). Returns one result per Check-tier annotation in input
/// order.
///
/// The batched path is the one that pays for itself on real specs:
/// many `cargo run -p loom-walk` invocations collapse to one when the
/// default `[runner.check]` claims them all. Targets no spec matches
/// still spawn their own process via the [`run_with_runners`] fallback.
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

/// Dispatch every `[system]`-tier annotation in `annotations`. One
/// subprocess spawns per annotation — `[system]` execution stays
/// per-annotation per `specs/gate.md` § Runners (system verifiers are slow
/// and self-contained, so batching does not pay). A matched runner owns
/// invocation construction via its `command` template, but the template is
/// rendered for exactly one target per spawn.
pub fn run_system(
    annotations: &[Annotation],
    specs: &[RunnerSpec],
    options: &DispatchOptions,
    repo_root: &Path,
    tier_cwds: &TierCwds,
) -> Vec<Result<DispatchOutcome, DispatchError>> {
    let system_only: Vec<Annotation> = annotations
        .iter()
        .filter(|a| a.tier == Tier::System && !a.pending)
        .cloned()
        .collect();
    run_system_with_runners(&system_only, specs, options, repo_root, tier_cwds)
}

fn run_system_with_runners(
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
        .map(|(i, a)| (a as *const Annotation, i))
        .collect();

    for group in groups {
        for matched in &group.matched {
            let single = RunnerGroup {
                spec: group.spec,
                matched: vec![matched.clone()],
            };
            let result = dispatch_group(&single, options, repo_root, tier_cwds)
                .into_iter()
                .next()
                .unwrap_or_else(|| {
                    Err(DispatchError::MissingFromBatchOutput {
                        runner: group.spec.name.clone(),
                        target: matched.rendered_target.clone(),
                    })
                });
            if let Some(&idx) = position_of.get(&(matched.annotation as *const Annotation)) {
                per_index[idx] = Some(result);
            }
        }
    }
    for ann in unmatched {
        if let Some(&idx) = position_of.get(&(ann as *const Annotation)) {
            let cwd = resolve_cwd(None, tier_cwds.for_tier(ann.tier), repo_root);
            per_index[idx] = Some(run_single_in(ann, options, cwd.as_deref()));
        }
    }

    per_index
        .into_iter()
        .map(|slot| slot.unwrap_or(Err(DispatchError::EmptyTarget { tier: Tier::System })))
        .collect()
}

/// Dispatch every `[test]`-tier annotation in `annotations` as one
/// batched runner subprocess. Targets are filtered by `--files` scope
/// via the [`TestScope`] resolver before being passed to the runner
/// template; an empty filter result returns `Ok(None)` so the caller
/// can distinguish "skipped — no scope match" from a true verdict.
pub fn run_test(
    annotations: &[Annotation],
    options: &DispatchOptions,
    template: &RunnerTemplate,
    scope: &dyn TestScope,
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
    let verdict = run_with_fallback(&command, options, true, None)?;
    Ok(Some(DispatchOutcome {
        annotations: filtered.into_iter().cloned().collect(),
        verdict,
    }))
}

/// Dispatch every `[judge]`-tier annotation in `annotations` as one
/// batched runner subprocess. Judges aren't `--files`-filterable, so
/// every judge annotation is included.
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
    let verdict = run_with_fallback(&command, options, false, None)?;
    Ok(Some(DispatchOutcome {
        annotations: judges.into_iter().cloned().collect(),
        verdict,
    }))
}

/// Dispatch `annotations` through `specs` per the runner-batched
/// contract in `specs/gate.md` § Runners. Annotations are grouped
/// by which spec matches their target (first match wins, declaration
/// order); each group spawns one subprocess and parses per-target
/// verdicts via the spec's [`BuiltinParser`]. Annotations no spec
/// matches fall back to per-annotation spawn (the existing
/// `[check]` / `[system]` semantics).
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
        .map(|(i, a)| (a as *const Annotation, i))
        .collect();

    for group in groups {
        let results = dispatch_group(&group, options, repo_root, tier_cwds);
        for (matched, result) in group.matched.iter().zip(results) {
            if let Some(&idx) = position_of.get(&(matched.annotation as *const Annotation)) {
                per_index[idx] = Some(result);
            }
        }
    }
    for ann in unmatched {
        if let Some(&idx) = position_of.get(&(ann as *const Annotation)) {
            let cwd = resolve_cwd(None, tier_cwds.for_tier(ann.tier), repo_root);
            per_index[idx] = Some(run_single_in(ann, options, cwd.as_deref()));
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
fn resolve_cwd(
    runner_cwd: Option<&Path>,
    tier_cwd: Option<&Path>,
    repo_root: &Path,
) -> Option<PathBuf> {
    let chosen = runner_cwd.or(tier_cwd);
    match chosen {
        Some(p) if p.is_absolute() => Some(p.to_path_buf()),
        Some(p) => Some(repo_root.join(p)),
        None => Some(repo_root.to_path_buf()),
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
    let output = match spawn_in(&command, options, cwd.as_deref()) {
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed = parse_runner_output(group.spec.parse, &stdout, &stderr, output.status.success());

    if matches!(group.spec.parse, BuiltinParser::ExitCode) {
        let pass = output.status.success();
        let evidence = if pass {
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
                    verdict: VerifierVerdict {
                        pass,
                        evidence: evidence.clone(),
                        skipped: false,
                    },
                })
            })
            .collect();
    }

    group
        .matched
        .iter()
        .map(|matched| match parsed.get(&matched.rendered_target) {
            Some(verdict) => Ok(DispatchOutcome {
                annotations: vec![matched.annotation.clone()],
                verdict: VerifierVerdict {
                    pass: verdict.pass,
                    evidence: verdict.evidence.clone(),
                    skipped: false,
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
    let verdict = run_with_fallback(command, options, false, cwd)?;
    Ok(DispatchOutcome {
        annotations: vec![annotation.clone()],
        verdict,
    })
}

fn run_with_fallback(
    command: &str,
    options: &DispatchOptions,
    sniff_zero_match: bool,
    cwd: Option<&Path>,
) -> Result<VerifierVerdict, DispatchError> {
    let output = spawn_in(command, options, cwd)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if sniff_zero_match {
        check_zero_match(command, &stdout, &stderr)
            .map_err(|e| DispatchError::ZeroMatch { source: e })?;
    }
    if let Some(verdict) = parse_verdict_optional(command, &stdout)? {
        return Ok(verdict);
    }
    let exit_code = output.status.code();
    if exit_code == Some(SKIP_EXIT_CODE) {
        let evidence = if stderr.trim().is_empty() {
            stdout.into_owned()
        } else {
            stderr.into_owned()
        };
        return Ok(VerifierVerdict {
            pass: false,
            evidence,
            skipped: true,
        });
    }
    Ok(VerifierVerdict {
        pass: output.status.success(),
        evidence: if output.status.success() {
            stdout.into_owned()
        } else {
            stderr.into_owned()
        },
        skipped: false,
    })
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
    let mut tokens = shlex::split(command)
        .ok_or_else(|| DispatchError::Spawn {
            command: command.to_string(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "unbalanced quotes"),
        })?
        .into_iter();
    let head = tokens.next().ok_or_else(|| DispatchError::Spawn {
        command: command.to_string(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"),
    })?;
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
    cmd.output().map_err(|e| DispatchError::Spawn {
        command: command.to_string(),
        source: e,
    })
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
    for raw in stdout.lines().rev() {
        let line = raw.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        match serde_json::from_str::<VerifierVerdict>(line) {
            Ok(v) => return Ok(Some(v)),
            Err(source) if line_attempts_verdict(line) => {
                return Err(DispatchError::MalformedVerdict {
                    command: command.to_string(),
                    source,
                });
            }
            Err(_) => continue,
        }
    }
    Ok(None)
}

/// `true` when the line parses as a JSON object with a `pass` key —
/// the signal that the verifier is attempting to speak the verdict
/// contract.
fn line_attempts_verdict(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .is_some_and(|o| o.contains_key("pass"))
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
    fn verdict_round_trips_through_json() {
        let v = VerifierVerdict {
            pass: true,
            evidence: "ok".into(),
            skipped: false,
        };
        let s = serde_json::to_string(&v).unwrap();
        assert_eq!(s, r#"{"pass":true,"evidence":"ok"}"#);
        let back: VerifierVerdict = serde_json::from_str(&s).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn skipped_verdict_serializes_with_skipped_flag() {
        let v = VerifierVerdict {
            pass: false,
            evidence: "no image".into(),
            skipped: true,
        };
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
    fn parse_verdict_optional_falls_through_on_unparseable_json() {
        let stdout = "{\"pass\": maybe}\n";
        assert!(parse_verdict_optional("cmd", stdout).unwrap().is_none());
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
        assert!(scope.scope_for(&a).is_empty());
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
        assert!(kept.is_empty());
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
        assert!(opts.files.is_empty());
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
