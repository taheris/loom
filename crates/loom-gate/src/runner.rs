//! Runner discovery for batched tiers (`[test]`, `[judge]`).
//!
//! Toolchain-detection defaults per `specs/gate.md` § Runners
//! (`Cargo.toml` → nextest, `pyproject.toml` → pytest, `go.mod` →
//! `go test`). Per-tier overrides flow in from `LoomConfig`'s
//! `[runner.<tier>.<name>]` blocks at `<workspace>/loom.toml`; this
//! module never reads TOML from disk itself. The module also surfaces
//! silent-zero-match cases in cargo / nextest / pytest output so a
//! filtered run that matches no tests fails loudly rather than passing
//! silently — other runners are expected to fail on zero-match themselves
//! and are passed through.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use displaydoc::Display;
use loom_driver::config::{self, LoomConfig, RunnerEntry};
use regex::Regex;
use thiserror::Error;

use crate::annotation::{Annotation, Tier};

/// Template string for a batched-tier runner with a placeholder
/// substituted at invocation time. Defaults come from toolchain
/// detection; overrides come from `LoomConfig`'s `[runner.<tier>.<name>]`
/// blocks at `<workspace>/loom.toml`.
///
/// Placeholder vocabulary, all rendered by [`RunnerTemplate::render`]:
///
/// - `{paths}` — slot-replicated and joined with ` | `. The slot is the
///   single-quoted phrase containing the placeholder (or the placeholder
///   token itself if no quotes wrap it). Matches `cargo nextest`'s
///   `-E 'test(p1) | test(p2)'` filter-expression shape.
/// - `{paths_or}` — replaced with the target list joined by ` or `
///   (pytest `-k` expression syntax).
/// - `{paths_alt}` — replaced with the target list joined by `|`
///   (regex alternation; `go test -run`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerTemplate {
    pub command: String,
}

impl RunnerTemplate {
    /// Construct from a raw template string.
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
        }
    }

    /// Substitute the placeholder for the joined target list and return
    /// the final command string ready to hand to a subprocess runner.
    pub fn render(&self, paths: &[&str]) -> String {
        render_template(&self.command, paths)
    }
}

/// Resolve the runner template for `tier` rooted at `repo_root` via
/// toolchain detection. Per-tier overrides from `LoomConfig`'s
/// `[runner.<tier>.<name>]` blocks are resolved by the caller (the
/// dispatcher in `loom-workflow` / `main.rs`) and passed in to the
/// dispatch layer directly; this function performs detection only.
///
/// Detection order:
/// 1. `Cargo.toml` → `cargo nextest`.
/// 2. `pyproject.toml` → `pytest`.
/// 3. `go.mod` → `go test`.
/// 4. [`RunnerError::UnknownToolchain`] when nothing matches.
///
/// Only batched tiers ([`Tier::Test`], [`Tier::Judge`]) are supported;
/// other tiers receive [`RunnerError::NotBatched`].
pub fn discover(repo_root: &Path, tier: Tier) -> Result<RunnerTemplate, RunnerError> {
    if !matches!(tier, Tier::Test | Tier::Judge) {
        return Err(RunnerError::NotBatched { tier });
    }

    if let Some(default) = detect_default(repo_root) {
        return Ok(default);
    }

    Err(RunnerError::UnknownToolchain {
        root: repo_root.to_path_buf(),
    })
}

/// Classification of a (template or rendered) command string for the
/// purposes of silent-zero-match sniffing.
///
/// `Unknown` covers any runner the gate does not know how to sniff —
/// the spec documents that those runners are responsible for failing on
/// zero-match themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerKind {
    CargoTest,
    CargoNextest,
    Pytest,
    Unknown,
}

impl RunnerKind {
    /// Inspect the leading tokens of `command` and return the matching
    /// runner kind. The check is token-boundary aware, so a hypothetical
    /// `cargo testify` does not classify as `cargo test`.
    pub fn classify(command: &str) -> Self {
        let trimmed = command.trim_start();
        if starts_with_token(trimmed, "cargo nextest") {
            Self::CargoNextest
        } else if starts_with_token(trimmed, "cargo test") {
            Self::CargoTest
        } else if starts_with_token(trimmed, "pytest") {
            Self::Pytest
        } else {
            Self::Unknown
        }
    }

    /// Human-readable name embedded in zero-match diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Self::CargoTest => "cargo test",
            Self::CargoNextest => "cargo nextest",
            Self::Pytest => "pytest",
            Self::Unknown => "unknown",
        }
    }
}

/// Post-process the runner's stdout / stderr after a successful exit and
/// surface [`RunnerError::ZeroMatch`] when the run silently matched
/// nothing.
///
/// Returns `Ok(())` for [`RunnerKind::Unknown`] — per the spec, the gate
/// documents the fail-on-zero-match expectation for unrecognised runners
/// but does not enforce it.
pub fn check_zero_match(command: &str, stdout: &str, stderr: &str) -> Result<(), RunnerError> {
    let kind = RunnerKind::classify(command);
    if let Some(evidence) = detect_zero_match(kind, stdout, stderr) {
        return Err(RunnerError::ZeroMatch {
            runner: kind.name(),
            evidence,
        });
    }
    Ok(())
}

fn detect_zero_match(kind: RunnerKind, stdout: &str, stderr: &str) -> Option<String> {
    match kind {
        RunnerKind::CargoTest => stdout
            .lines()
            .find(|l| l.trim() == "running 0 tests")
            .map(|l| l.trim().to_string()),
        RunnerKind::CargoNextest => stdout
            .lines()
            .chain(stderr.lines())
            .map(str::trim)
            .find(|t| {
                t.starts_with("Starting 0 tests")
                    || t.contains(" 0 tests run:")
                    || t.contains("no tests to run")
            })
            .map(str::to_string),
        RunnerKind::Pytest => stdout
            .lines()
            .chain(stderr.lines())
            .map(str::trim)
            .find(|t| t.contains("no tests ran") || t.contains("collected 0 items"))
            .map(str::to_string),
        RunnerKind::Unknown => None,
    }
}

fn detect_default(repo_root: &Path) -> Option<RunnerTemplate> {
    if repo_root.join("Cargo.toml").is_file() {
        return Some(RunnerTemplate::new("cargo nextest run -E 'test({paths})'"));
    }
    if repo_root.join("pyproject.toml").is_file() {
        return Some(RunnerTemplate::new("pytest -k '{paths_or}'"));
    }
    if repo_root.join("go.mod").is_file() {
        return Some(RunnerTemplate::new("go test -run '{paths_alt}' ./..."));
    }
    None
}

fn render_template(template: &str, paths: &[&str]) -> String {
    let mut s = template.to_string();
    if s.contains("{paths_or}") {
        s = s.replace("{paths_or}", &paths.join(" or "));
    }
    if s.contains("{paths_alt}") {
        s = s.replace("{paths_alt}", &paths.join("|"));
    }
    if let Some(start) = s.find("{paths}") {
        s = render_slot(&s, start, paths);
    }
    s
}

/// Replicate the slot surrounding `{paths}` per target and join with
/// ` | `. The slot is bounded by the nearest `'` on either side, or by
/// the start / end of the template when no quote is present. This makes
/// `cargo nextest run -E 'test({paths})'` expand to
/// `cargo nextest run -E 'test(p1) | test(p2)'`.
fn render_slot(template: &str, start: usize, paths: &[&str]) -> String {
    const PLACEHOLDER: &str = "{paths}";
    let end = start + PLACEHOLDER.len();

    let before = &template[..start];
    let after = &template[end..];

    let slot_lstart = before.rfind('\'').map_or(0, |i| i + 1);
    let slot_rend_local = after.find('\'').unwrap_or(after.len());

    let slot_prefix = &before[slot_lstart..];
    let slot_suffix = &after[..slot_rend_local];

    let joined = paths
        .iter()
        .map(|p| format!("{slot_prefix}{p}{slot_suffix}"))
        .collect::<Vec<_>>()
        .join(" | ");

    let before_slot = &template[..slot_lstart];
    let after_slot = &template[end + slot_rend_local..];

    format!("{before_slot}{joined}{after_slot}")
}

fn starts_with_token(input: &str, token: &str) -> bool {
    input
        .strip_prefix(token)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

/// Failures surfaced by runner discovery and zero-match sniffing.
#[derive(Debug, Display, Error)]
pub enum RunnerError {
    /// runner discovery only applies to batched tiers (test, judge); got [{tier}]
    NotBatched { tier: Tier },
    /// no Cargo.toml / pyproject.toml / go.mod under {root}
    UnknownToolchain { root: PathBuf },
    /// {runner} reported zero matched tests; filter likely missed every target: {evidence}
    ZeroMatch {
        runner: &'static str,
        evidence: String,
    },
    /// runner `{name}` has invalid match regex `{pattern}`: {source}
    InvalidMatch {
        name: String,
        pattern: String,
        #[source]
        source: regex::Error,
    },
    /// runner `{name}` missing `command`
    MissingCommand { name: String },
}

/// Named built-in parser that extracts per-target verdicts from a runner's
/// stdout. The set is closed by loom; the schema-side equivalent in
/// `loom_driver::config::runner::Parser` round-trips into this enum at the
/// translation boundary (e.g. `loom-workflow` building a [`RunnerSpec`] list).
///
/// Per-tier semantics:
///
/// - [`Self::LibtestJson`] — Rust `cargo test` / `nextest`
///   `--message-format` output. One JSON event per line; this parser folds
///   `{"type":"test","event":"ok|failed|started|ignored","name":"..."}`
///   into a per-target verdict.
/// - [`Self::JunitXml`] — JUnit-XML reports. Each `<testcase classname="..."
///   name="...">` becomes one verdict; a nested `<failure>` / `<error>`
///   produces `pass = false`.
/// - [`Self::NixBuildStatus`] — `nix build`'s per-derivation output.
/// - [`Self::JsonLines`] — one
///   `{"target":"<name>","pass":bool,"evidence":"<msg>"}` per line.
/// - [`Self::ExitCode`] — the runner emits no structured stdout; the
///   process exit code is the verdict. Only meaningful for non-batched
///   runners (one annotation per invocation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinParser {
    LibtestJson,
    JunitXml,
    NixBuildStatus,
    JsonLines,
    ExitCode,
}

/// One per-target verdict recovered from a batched runner's stdout, keyed
/// by the annotation target string the parser found in the output.
///
/// Distinct from `dispatch::VerifierVerdict` so the parser layer stays
/// free of the dispatch error type — the dispatcher walks the parser's
/// map and folds matches into `VerifierVerdict` records on the way out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedVerdict {
    pub target: String,
    pub pass: bool,
    pub evidence: String,
}

/// One `[runner.<tier>.<name>]` entry resolved into runtime form: regex
/// compiled, parser tag normalised, templates kept as raw strings ready
/// for substitution. Consumers translate from the TOML schema in
/// `loom_driver::config::runner::RunnerEntry` to this type at the binary
/// boundary.
#[derive(Debug, Clone)]
pub struct RunnerSpec {
    /// Identifier from the TOML key (`[runner.<tier>.<name>]`). Surfaced in
    /// errors and progress output so consumers can pinpoint the offending
    /// runner.
    pub name: String,
    /// Compiled match regex over the annotation target. `None` means the
    /// runner is the tier default (matches every annotation in the tier).
    pub match_regex: Option<Regex>,
    /// Command template; substitutes `{filter}` / `{targets}` with the
    /// joined per-target string and `{capture_N}` with the corresponding
    /// capture from the first matched target.
    pub command: String,
    /// Per-target template; substitutes `{name}` with the full target and
    /// `{capture_N}` with the corresponding regex capture.
    pub target_template: String,
    /// Separator inserted between formatted targets to build
    /// `{filter}` / `{targets}`.
    pub join: String,
    /// Built-in parser that recovers per-target verdicts from stdout.
    pub parse: BuiltinParser,
    /// Repo-relative cwd override. Resolution against the workspace root
    /// happens in the dispatcher.
    pub cwd: Option<PathBuf>,
    /// Optional command template for the runner's input-query. A
    /// `{print_inputs}` placement marks where the `--print-inputs` flag
    /// lands in the verifier's own argv; omitting it appends the flag after
    /// the verifier's own arguments. `Some(..)` opts the runner's verifiers
    /// into the input-query protocol; `None` leaves them on the
    /// conservative always-run default. See `specs/gate.md` § Runners.
    pub inputs: Option<String>,
}

impl RunnerSpec {
    /// Compile a runner spec from its raw schema fields. Validates the
    /// regex eagerly so dispatch never fails partway through a tier.
    pub fn compile(
        name: impl Into<String>,
        match_regex: Option<&str>,
        command: impl Into<String>,
        target_template: impl Into<String>,
        join: impl Into<String>,
        parse: BuiltinParser,
        cwd: Option<PathBuf>,
    ) -> Result<Self, RunnerError> {
        let name = name.into();
        let match_regex = match match_regex {
            Some(pattern) => {
                Some(
                    Regex::new(pattern).map_err(|source| RunnerError::InvalidMatch {
                        name: name.clone(),
                        pattern: pattern.to_string(),
                        source,
                    })?,
                )
            }
            None => None,
        };
        Ok(Self {
            name,
            match_regex,
            command: command.into(),
            target_template: target_template.into(),
            join: join.into(),
            parse,
            cwd,
            inputs: None,
        })
    }

    /// Attach the input-query template (the `inputs` schema field),
    /// chained after [`Self::compile`]. `None` leaves the runner on the
    /// conservative always-run default.
    #[must_use]
    pub fn with_inputs(mut self, inputs: Option<String>) -> Self {
        self.inputs = inputs;
        self
    }

    /// Whether `target` matches this runner. A default runner
    /// (`match_regex = None`) matches everything; otherwise the compiled
    /// regex decides.
    pub fn matches(&self, target: &str) -> bool {
        self.match_regex
            .as_ref()
            .is_none_or(|re| re.is_match(target))
    }
}

/// Built-in batcher for `cargo run -p loom-walk -- <name>` `[check]`
/// annotations. Ships in code (not `loom.toml`) so the batching is the
/// default behaviour; operators can layer overrides via
/// `[runner.check.<name>]` entries but cannot accidentally remove it.
pub fn builtin_loom_walk_runner() -> Result<RunnerSpec, RunnerError> {
    RunnerSpec::compile(
        "builtin-loom-walk",
        Some(r"^cargo run -p loom-walk -- (\S+)$"),
        "cargo run -p loom-walk -- {targets}",
        "{capture_1}",
        " ",
        BuiltinParser::JsonLines,
        None,
    )
}

/// Compile one `[runner.<tier>.<name>]` schema entry into runtime form.
/// The `command` field is required; missing it is a
/// [`RunnerError::MissingCommand`].
pub fn compile_runner_entry(name: &str, entry: &RunnerEntry) -> Result<RunnerSpec, RunnerError> {
    let command = entry
        .command
        .as_deref()
        .ok_or_else(|| RunnerError::MissingCommand {
            name: name.to_string(),
        })?;
    let target = entry.target.as_deref().unwrap_or("{name}");
    let join = entry.join.as_deref().unwrap_or(" ");
    let parse = match entry.parse {
        Some(config::Parser::LibtestJson) => BuiltinParser::LibtestJson,
        Some(config::Parser::JunitXml) => BuiltinParser::JunitXml,
        Some(config::Parser::NixBuildStatus) => BuiltinParser::NixBuildStatus,
        Some(config::Parser::JsonLines) | None => BuiltinParser::JsonLines,
        Some(config::Parser::ExitCode) => BuiltinParser::ExitCode,
    };
    let cwd = entry.cwd.as_deref().map(PathBuf::from);
    Ok(RunnerSpec::compile(
        name,
        entry.match_regex.as_deref(),
        command,
        target,
        join,
        parse,
        cwd,
    )?
    .with_inputs(entry.inputs.clone()))
}

/// Compile the named `[runner.<tier>.<name>]` runners — and the implicit
/// tier-default runner — declared under `tier` into runtime [`RunnerSpec`]s.
/// Matching keys on target shape, not tier, so the caller decides which
/// annotations flow through the returned specs; the `[check]`-tier builtin
/// batcher is layered by the caller, not here.
pub fn compile_tier_runners(
    config: &LoomConfig,
    tier: &str,
) -> Result<Vec<RunnerSpec>, RunnerError> {
    let mut specs = Vec::new();
    if let Some(tier_block) = config.runner.tier(tier) {
        for (name, entry) in &tier_block.runners {
            specs.push(compile_runner_entry(name, entry)?);
        }
        if let Some(default_entry) = tier_block.default_runner() {
            specs.push(compile_runner_entry("default", &default_entry)?);
        }
    }
    Ok(specs)
}

/// Resolve the runner specs the integrity gate's forward-resolution
/// consults. The gate checks every annotation regardless of tier, so it
/// needs the union of the builtin loom-walk batcher plus `[check]`- and
/// `[system]`-tier runners: a `[system](target)` matched by a
/// `[runner.system.<name>]` block must resolve by runner ownership exactly
/// as a `[check]` target matched by `[runner.check.<name>]` does
/// (`specs/gate.md` § Target resolution). RunnerSpec matching keys on
/// target shape, not tier, so the union never cross-resolves a check target
/// against a system runner or vice versa.
pub fn integrity_runner_specs(config: &LoomConfig) -> Result<Vec<RunnerSpec>, RunnerError> {
    let mut specs = vec![builtin_loom_walk_runner()?];
    specs.extend(compile_tier_runners(config, "check")?);
    specs.extend(compile_tier_runners(config, "system")?);
    Ok(specs)
}

/// One annotation matched by a [`RunnerSpec`] together with its rendered
/// per-target string and the captures the regex produced. Carried through
/// dispatch so the parser can map verdicts back to the original
/// annotation by target name.
#[derive(Debug, Clone)]
pub struct MatchedAnnotation<'a> {
    pub annotation: &'a Annotation,
    /// `target_template` rendered against `{name}` + `{capture_N}`. This is
    /// what appears inside the joined batch string; this is also the key
    /// the parser uses when reporting per-target results.
    pub rendered_target: String,
}

/// First-match-wins grouping of `annotations` against `specs`. Annotations
/// whose target none of `specs` accept fall into `unmatched` and are
/// dispatched by the caller's per-annotation fallback.
///
/// Group ordering follows `specs` declaration order. Within a group,
/// matched annotations preserve their order in `annotations`.
pub fn group_by_runner<'s, 'a>(
    specs: &'s [RunnerSpec],
    annotations: &'a [Annotation],
) -> (Vec<RunnerGroup<'s, 'a>>, Vec<&'a Annotation>) {
    let mut groups: Vec<RunnerGroup<'s, 'a>> = specs
        .iter()
        .map(|spec| RunnerGroup {
            spec,
            matched: Vec::new(),
        })
        .collect();
    let mut unmatched: Vec<&'a Annotation> = Vec::new();

    for annotation in annotations {
        let mut placed = false;
        for group in &mut groups {
            if group.spec.matches(&annotation.target) {
                let rendered_target = render_target(group.spec, &annotation.target);
                group.matched.push(MatchedAnnotation {
                    annotation,
                    rendered_target,
                });
                placed = true;
                break;
            }
        }
        if !placed {
            unmatched.push(annotation);
        }
    }

    groups.retain(|g| !g.matched.is_empty());
    (groups, unmatched)
}

/// All annotations a single [`RunnerSpec`] claimed, in input order.
#[derive(Debug)]
pub struct RunnerGroup<'s, 'a> {
    pub spec: &'s RunnerSpec,
    pub matched: Vec<MatchedAnnotation<'a>>,
}

impl RunnerGroup<'_, '_> {
    /// Render the runner's batch command for this group: join every
    /// matched annotation's `rendered_target` with `spec.join` to form
    /// `{filter}` / `{targets}`, then substitute into `spec.command`.
    /// `{capture_N}` in `command` references the first matched target's
    /// captures so command-template captures are well-defined.
    pub fn render_command(&self) -> String {
        let joined = self
            .matched
            .iter()
            .map(|m| m.rendered_target.as_str())
            .collect::<Vec<_>>()
            .join(&self.spec.join);
        let first_target = self.matched.first().map(|m| m.annotation.target.as_str());
        substitute_command(
            &self.spec.command,
            &joined,
            self.spec.match_regex.as_ref(),
            first_target,
        )
    }

    /// Render the runner's input-query command for this group, or `None`
    /// when the runner declares no `inputs` query and so stays on the
    /// conservative always-run default. `{filter}` / `{targets}` /
    /// `{capture_N}` substitute exactly as in [`Self::render_command`]; the
    /// `--print-inputs` flag lands at the template's `{print_inputs}`
    /// marker, or is appended after the verifier's own arguments when the
    /// marker is absent. Per `specs/gate.md` § Runners the query is issued
    /// through this template — never by prepending the flag to the
    /// command's first token.
    pub fn render_inputs_query(&self) -> Option<String> {
        let template = self.spec.inputs.as_deref()?;
        let joined = self
            .matched
            .iter()
            .map(|m| m.rendered_target.as_str())
            .collect::<Vec<_>>()
            .join(&self.spec.join);
        let first_target = self.matched.first().map(|m| m.annotation.target.as_str());
        let rendered = substitute_command(
            template,
            &joined,
            self.spec.match_regex.as_ref(),
            first_target,
        );
        Some(place_print_inputs(&rendered))
    }
}

/// The `--print-inputs` query flag, placed where the runner's `inputs`
/// template marks it.
const PRINT_INPUTS_FLAG: &str = "--print-inputs";

/// Substitute the `{print_inputs}` marker with the query flag, or append
/// the flag after the verifier's own arguments when the template omits the
/// marker. Per `specs/gate.md` § Runners the flag's placement is the
/// template's decision, never a prepend to `tokens[0]`.
fn place_print_inputs(rendered: &str) -> String {
    const MARKER: &str = "{print_inputs}";
    if rendered.contains(MARKER) {
        rendered.replace(MARKER, PRINT_INPUTS_FLAG)
    } else {
        format!("{rendered} {PRINT_INPUTS_FLAG}")
    }
}

fn render_target(spec: &RunnerSpec, annotation_target: &str) -> String {
    let mut rendered = spec.target_template.replace("{name}", annotation_target);
    if let Some(re) = &spec.match_regex {
        rendered = substitute_captures(&rendered, re, annotation_target);
    }
    rendered
}

fn substitute_command(
    template: &str,
    joined: &str,
    match_regex: Option<&Regex>,
    first_target: Option<&str>,
) -> String {
    let mut out = template
        .replace("{filter}", joined)
        .replace("{targets}", joined);
    if let (Some(re), Some(target)) = (match_regex, first_target) {
        out = substitute_captures(&out, re, target);
    }
    out
}

fn substitute_captures(template: &str, re: &Regex, target: &str) -> String {
    let Some(caps) = re.captures(target) else {
        return template.to_string();
    };
    let mut out = template.to_string();
    for i in 1..caps.len() {
        let needle = format!("{{capture_{i}}}");
        if let Some(m) = caps.get(i) {
            out = out.replace(&needle, m.as_str());
        }
    }
    out
}

/// Parse per-target verdicts out of a batched runner's stdout / stderr
/// according to the runner's built-in parser tag. Each parser is a
/// best-effort recovery layer; targets the parser cannot find are
/// returned as missing so the dispatcher can flag them as dispatch
/// failures.
pub fn parse_runner_output(
    parser: BuiltinParser,
    stdout: &str,
    stderr: &str,
    exit_success: bool,
) -> HashMap<String, ParsedVerdict> {
    match parser {
        BuiltinParser::JsonLines => parse_json_lines(stdout),
        BuiltinParser::LibtestJson => parse_libtest_json(stdout),
        BuiltinParser::JunitXml => parse_junit_xml(stdout),
        BuiltinParser::NixBuildStatus => parse_nix_build_status(stdout, stderr),
        BuiltinParser::ExitCode => parse_exit_code(stdout, stderr, exit_success),
    }
}

fn parse_json_lines(stdout: &str) -> HashMap<String, ParsedVerdict> {
    let mut out = HashMap::new();
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(obj) = value.as_object() else {
            continue;
        };
        let target = obj
            .get("target")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let pass = obj.get("pass").and_then(serde_json::Value::as_bool);
        let evidence = obj
            .get("evidence")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if let (Some(target), Some(pass)) = (target, pass) {
            out.insert(
                target.clone(),
                ParsedVerdict {
                    target,
                    pass,
                    evidence,
                },
            );
        }
    }
    out
}

fn parse_libtest_json(stdout: &str) -> HashMap<String, ParsedVerdict> {
    let mut out = HashMap::new();
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(obj) = value.as_object() else {
            continue;
        };
        if obj.get("type").and_then(|v| v.as_str()) != Some("test") {
            continue;
        }
        let Some(name) = obj.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let event = obj.get("event").and_then(|v| v.as_str()).unwrap_or("");
        let (pass, evidence) = match event {
            "ok" => (true, String::from("ok")),
            "failed" => {
                let stdout_field = obj
                    .get("stdout")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                (false, stdout_field)
            }
            "ignored" => (true, String::from("ignored")),
            _ => continue,
        };
        out.insert(
            name.to_string(),
            ParsedVerdict {
                target: name.to_string(),
                pass,
                evidence,
            },
        );
    }
    out
}

fn parse_junit_xml(stdout: &str) -> HashMap<String, ParsedVerdict> {
    let mut out = HashMap::new();
    let testcase_re = match Regex::new(r#"(?s)<testcase\b([^>]*?)(?:/>|>(.*?)</testcase>)"#) {
        Ok(re) => re,
        Err(_) => return out,
    };
    let classname_re = match Regex::new(r#"\bclassname\s*=\s*"([^"]*)""#) {
        Ok(re) => re,
        Err(_) => return out,
    };
    let name_re = match Regex::new(r#"\bname\s*=\s*"([^"]*)""#) {
        Ok(re) => re,
        Err(_) => return out,
    };
    for cap in testcase_re.captures_iter(stdout) {
        let Some(attrs) = cap.get(1) else { continue };
        let body = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let classname = classname_re
            .captures(attrs.as_str())
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
            .unwrap_or("");
        let name = match name_re
            .captures(attrs.as_str())
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
        {
            Some(s) => s,
            None => continue,
        };
        let key = if classname.is_empty() {
            name.to_string()
        } else {
            format!("{classname}.{name}")
        };
        let failed = body.contains("<failure") || body.contains("<error");
        let evidence = if failed {
            extract_first_attr(body, "message").unwrap_or_else(|| "failed".to_string())
        } else {
            String::from("ok")
        };
        out.insert(
            key.clone(),
            ParsedVerdict {
                target: key,
                pass: !failed,
                evidence,
            },
        );
    }
    out
}

fn extract_first_attr(body: &str, attr: &str) -> Option<String> {
    let pattern = format!(r#"\b{attr}\s*=\s*"([^"]*)""#);
    Regex::new(&pattern)
        .ok()?
        .captures(body)?
        .get(1)
        .map(|m| m.as_str().to_string())
}

fn parse_nix_build_status(stdout: &str, stderr: &str) -> HashMap<String, ParsedVerdict> {
    let mut out = HashMap::new();
    let combined: Vec<&str> = stdout.lines().chain(stderr.lines()).collect();
    let derivation_re = match Regex::new(r#"/nix/store/[a-z0-9]+-([^/'\s]+?)\.drv"#) {
        Ok(re) => re,
        Err(_) => return out,
    };
    let mut failed: HashMap<String, String> = HashMap::new();
    for line in &combined {
        if (line.starts_with("error: builder for ") || line.starts_with("error: build of "))
            && let Some(cap) = derivation_re.captures(line)
            && let Some(name) = cap.get(1)
        {
            failed.insert(name.as_str().to_string(), (*line).to_string());
        }
    }
    let mut built: HashMap<String, ()> = HashMap::new();
    for line in &combined {
        if (line.starts_with("building ") || line.contains("building '"))
            && let Some(cap) = derivation_re.captures(line)
            && let Some(name) = cap.get(1)
        {
            built.insert(name.as_str().to_string(), ());
        }
    }
    for name in built.keys() {
        let (pass, evidence) = match failed.get(name) {
            Some(line) => (false, line.clone()),
            None => (true, String::from("built")),
        };
        out.insert(
            name.clone(),
            ParsedVerdict {
                target: name.clone(),
                pass,
                evidence,
            },
        );
    }
    for (name, line) in failed {
        out.entry(name.clone()).or_insert(ParsedVerdict {
            target: name,
            pass: false,
            evidence: line,
        });
    }
    out
}

fn parse_exit_code(
    stdout: &str,
    stderr: &str,
    exit_success: bool,
) -> HashMap<String, ParsedVerdict> {
    let mut out = HashMap::new();
    let evidence = if exit_success {
        stdout.trim().to_string()
    } else {
        stderr.trim().to_string()
    };
    out.insert(
        String::new(),
        ParsedVerdict {
            target: String::new(),
            pass: exit_success,
            evidence,
        },
    );
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    use std::fs;

    use tempfile::tempdir;

    #[test]
    fn detect_default_for_cargo_repo() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();

        let template = discover(dir.path(), Tier::Test).unwrap();
        assert_eq!(template.command, "cargo nextest run -E 'test({paths})'");

        let judge = discover(dir.path(), Tier::Judge).unwrap();
        assert_eq!(judge.command, "cargo nextest run -E 'test({paths})'");
    }

    #[test]
    fn detect_default_for_python_repo() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "[project]\nname='x'\n").unwrap();

        let template = discover(dir.path(), Tier::Test).unwrap();
        assert_eq!(template.command, "pytest -k '{paths_or}'");
    }

    #[test]
    fn detect_default_for_go_repo() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "module example.com/x\n").unwrap();

        let template = discover(dir.path(), Tier::Test).unwrap();
        assert_eq!(template.command, "go test -run '{paths_alt}' ./...");
    }

    #[test]
    fn unknown_toolchain_errors_cleanly() {
        let dir = tempdir().unwrap();
        let err = discover(dir.path(), Tier::Test).unwrap_err();
        assert!(matches!(err, RunnerError::UnknownToolchain { .. }));
        let msg = err.to_string();
        assert!(
            msg.contains("Cargo.toml") && msg.contains("pyproject.toml") && msg.contains("go.mod"),
            "message names the detected markers: {msg}"
        );
    }

    #[test]
    fn discover_ignores_legacy_loom_config_toml() {
        // The retired second config-file read path used to read
        // `.loom/config.toml`; the module must not see it any more.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let legacy = dir.path().join(".loom");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(
            legacy.join("config.toml"),
            "[runner]\ntest = \"should-never-be-read {paths}\"\n",
        )
        .unwrap();

        let template = discover(dir.path(), Tier::Test).unwrap();
        assert_eq!(
            template.command, "cargo nextest run -E 'test({paths})'",
            "discover must use toolchain detection only; .loom/config.toml is retired"
        );
    }

    #[test]
    fn check_and_system_tiers_are_not_batched() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();

        let check = discover(dir.path(), Tier::Check).unwrap_err();
        assert!(matches!(
            check,
            RunnerError::NotBatched { tier: Tier::Check }
        ));
        let system = discover(dir.path(), Tier::System).unwrap_err();
        assert!(matches!(
            system,
            RunnerError::NotBatched { tier: Tier::System }
        ));
    }

    #[test]
    fn render_substitutes_paths_or_with_or_join() {
        let t = RunnerTemplate::new("pytest -k '{paths_or}'");
        assert_eq!(t.render(&["a", "b", "c"]), "pytest -k 'a or b or c'");
    }

    #[test]
    fn render_substitutes_paths_alt_with_pipe_join() {
        let t = RunnerTemplate::new("go test -run '{paths_alt}' ./...");
        assert_eq!(
            t.render(&["TestA", "TestB"]),
            "go test -run 'TestA|TestB' ./..."
        );
    }

    #[test]
    fn render_slot_replicates_within_single_quotes_for_nextest() {
        let t = RunnerTemplate::new("cargo nextest run -E 'test({paths})'");
        assert_eq!(
            t.render(&["p1", "p2", "p3"]),
            "cargo nextest run -E 'test(p1) | test(p2) | test(p3)'"
        );
    }

    #[test]
    fn render_slot_with_single_target_emits_single_slot() {
        let t = RunnerTemplate::new("cargo nextest run -E 'test({paths})'");
        assert_eq!(t.render(&["solo"]), "cargo nextest run -E 'test(solo)'");
    }

    #[test]
    fn render_slot_without_quotes_uses_whole_template_as_slot() {
        let t = RunnerTemplate::new("mytool {paths}");
        assert_eq!(t.render(&["a", "b"]), "mytool a | mytool b");
    }

    #[test]
    fn render_passes_through_template_with_no_placeholder() {
        let t = RunnerTemplate::new("no-placeholder-here");
        assert_eq!(t.render(&["a"]), "no-placeholder-here");
    }

    #[test]
    fn render_full_toolchain_defaults_round_trip() {
        let cargo = RunnerTemplate::new("cargo nextest run -E 'test({paths})'");
        let pytest = RunnerTemplate::new("pytest -k '{paths_or}'");
        let go = RunnerTemplate::new("go test -run '{paths_alt}' ./...");

        assert_eq!(
            cargo.render(&["mod::a", "mod::b"]),
            "cargo nextest run -E 'test(mod::a) | test(mod::b)'"
        );
        assert_eq!(
            pytest.render(&["test_a", "test_b"]),
            "pytest -k 'test_a or test_b'"
        );
        assert_eq!(
            go.render(&["TestA", "TestB"]),
            "go test -run 'TestA|TestB' ./..."
        );
    }

    #[test]
    fn classify_recognises_cargo_test_cargo_nextest_pytest() {
        assert_eq!(
            RunnerKind::classify("cargo test --workspace"),
            RunnerKind::CargoTest
        );
        assert_eq!(
            RunnerKind::classify("cargo nextest run -E 'test(x)'"),
            RunnerKind::CargoNextest
        );
        assert_eq!(RunnerKind::classify("pytest -k x"), RunnerKind::Pytest);
        assert_eq!(RunnerKind::classify("my-runner"), RunnerKind::Unknown);
        assert_eq!(RunnerKind::classify(""), RunnerKind::Unknown);
    }

    #[test]
    fn classify_token_boundary_avoids_false_positive_on_cargo_testify() {
        assert_eq!(
            RunnerKind::classify("cargo testify --foo"),
            RunnerKind::Unknown
        );
        assert_eq!(
            RunnerKind::classify("pytest-something"),
            RunnerKind::Unknown
        );
    }

    #[test]
    fn zero_match_detects_cargo_test_running_zero_tests() {
        let stdout = "\
running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out
";
        let err = check_zero_match("cargo test -- missing_name", stdout, "").unwrap_err();
        match err {
            RunnerError::ZeroMatch { runner, evidence } => {
                assert_eq!(runner, "cargo test");
                assert_eq!(evidence, "running 0 tests");
            }
            other => panic!("expected ZeroMatch, got {other:?}"),
        }
    }

    #[test]
    fn zero_match_detects_cargo_nextest_starting_zero_tests() {
        let stdout =
            "    Starting 0 tests across 5 binaries (run ID: abc, nextest profile: default)\n";
        let err = check_zero_match("cargo nextest run -E 'test(nope)'", stdout, "").unwrap_err();
        assert!(matches!(
            err,
            RunnerError::ZeroMatch {
                runner: "cargo nextest",
                ..
            }
        ));
    }

    #[test]
    fn zero_match_detects_cargo_nextest_summary_zero_tests_run() {
        let stdout = "------------\n     Summary [   0.011s] 0 tests run: 0 passed, 0 skipped\n";
        let err = check_zero_match("cargo nextest run", stdout, "").unwrap_err();
        assert!(matches!(err, RunnerError::ZeroMatch { .. }));
    }

    #[test]
    fn zero_match_does_not_false_positive_on_nextest_count_ending_in_zero() {
        let stdout =
            "------------\n     Summary [   0.840s] 310 tests run: 310 passed, 1105 skipped\n";
        check_zero_match("cargo nextest run", stdout, "").unwrap();
    }

    #[test]
    fn zero_match_detects_pytest_no_tests_ran() {
        let stdout =
            "collected 0 items\n\n================= no tests ran in 0.01s =================\n";
        let err = check_zero_match("pytest -k missing", stdout, "").unwrap_err();
        assert!(matches!(
            err,
            RunnerError::ZeroMatch {
                runner: "pytest",
                ..
            }
        ));
    }

    #[test]
    fn zero_match_passes_when_runner_actually_ran_tests() {
        let stdout = "\
running 3 tests
test alpha ... ok
test beta ... ok
test gamma ... ok

test result: ok. 3 passed; 0 failed
";
        check_zero_match("cargo test", stdout, "").unwrap();
    }

    #[test]
    fn zero_match_passes_for_unrecognised_runner_even_with_zero_in_output() {
        let stdout = "running 0 tests\n";
        check_zero_match("my-custom-runner --tests x", stdout, "").unwrap();
    }

    #[test]
    fn zero_match_inspects_stderr_for_pytest_and_nextest() {
        let stderr = "Starting 0 tests across 1 binaries (run ID: abc, nextest profile: default)";
        let err = check_zero_match("cargo nextest run", "", stderr).unwrap_err();
        assert!(matches!(err, RunnerError::ZeroMatch { .. }));
    }

    #[test]
    fn not_batched_error_message_names_the_tier() {
        let err = RunnerError::NotBatched { tier: Tier::Check };
        assert_eq!(
            err.to_string(),
            "runner discovery only applies to batched tiers (test, judge); got [check]"
        );
    }

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
    fn runner_spec_compile_rejects_invalid_regex() {
        let err = RunnerSpec::compile(
            "bad",
            Some("("),
            "cmd",
            "{name}",
            " ",
            BuiltinParser::JsonLines,
            None,
        )
        .unwrap_err();
        match err {
            RunnerError::InvalidMatch { name, pattern, .. } => {
                assert_eq!(name, "bad");
                assert_eq!(pattern, "(");
            }
            other => panic!("expected InvalidMatch, got {other:?}"),
        }
    }

    #[test]
    fn runner_spec_compile_defaults_inputs_to_none() {
        let spec = RunnerSpec::compile(
            "default",
            None,
            "cmd {targets}",
            "{name}",
            " ",
            BuiltinParser::JsonLines,
            None,
        )
        .unwrap();
        assert!(
            spec.inputs.is_none(),
            "compile leaves the runner on the always-run default"
        );
    }

    #[test]
    fn runner_spec_with_inputs_attaches_query_template() {
        let spec = RunnerSpec::compile(
            "walk",
            Some(r"^cargo run -p loom-walk -- (\S+)$"),
            "cargo run -p loom-walk -- {targets}",
            "{capture_1}",
            " ",
            BuiltinParser::JsonLines,
            None,
        )
        .unwrap()
        .with_inputs(Some(
            "cargo run -p loom-walk -- {targets} {print_inputs}".to_string(),
        ));
        assert_eq!(
            spec.inputs.as_deref(),
            Some("cargo run -p loom-walk -- {targets} {print_inputs}")
        );
    }

    #[test]
    fn runner_spec_with_inputs_none_keeps_default() {
        let spec = RunnerSpec::compile(
            "default",
            None,
            "cmd {targets}",
            "{name}",
            " ",
            BuiltinParser::JsonLines,
            None,
        )
        .unwrap()
        .with_inputs(None);
        assert!(spec.inputs.is_none());
    }

    #[test]
    fn runner_spec_with_no_regex_matches_every_target() {
        let spec = RunnerSpec::compile(
            "default",
            None,
            "cmd {targets}",
            "{name}",
            " ",
            BuiltinParser::JsonLines,
            None,
        )
        .unwrap();
        assert!(spec.matches("crate::a::ok"));
        assert!(spec.matches("anything"));
    }

    #[test]
    fn runner_spec_with_regex_matches_only_when_pattern_matches() {
        let spec = RunnerSpec::compile(
            "nix",
            Some(r"^nix (build|run) \.#(\S+)$"),
            "nix build {targets}",
            ".#{capture_2}",
            " ",
            BuiltinParser::NixBuildStatus,
            None,
        )
        .unwrap();
        assert!(spec.matches("nix build .#test-loom"));
        assert!(spec.matches("nix run .#test-loom"));
        assert!(!spec.matches("cargo nextest run"));
    }

    #[test]
    fn group_by_runner_first_match_wins_in_declaration_order() {
        let nix = RunnerSpec::compile(
            "nix",
            Some(r"^nix "),
            "nix build {targets}",
            "{name}",
            " ",
            BuiltinParser::NixBuildStatus,
            None,
        )
        .unwrap();
        let fallback = RunnerSpec::compile(
            "default",
            None,
            "default {targets}",
            "{name}",
            " ",
            BuiltinParser::JsonLines,
            None,
        )
        .unwrap();
        let specs = vec![nix, fallback];
        let inputs = vec![
            ann(Tier::System, "nix build .#a"),
            ann(Tier::Check, "other-thing"),
            ann(Tier::System, "nix run .#b"),
        ];
        let (groups, unmatched) = group_by_runner(&specs, &inputs);
        assert!(unmatched.is_empty(), "fallback runner claims everything");
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].spec.name, "nix");
        assert_eq!(groups[0].matched.len(), 2);
        assert_eq!(groups[1].spec.name, "default");
        assert_eq!(groups[1].matched.len(), 1);
        assert_eq!(groups[1].matched[0].annotation.target, "other-thing");
    }

    #[test]
    fn group_by_runner_emits_unmatched_when_no_spec_matches() {
        let nix = RunnerSpec::compile(
            "nix",
            Some(r"^nix "),
            "nix build {targets}",
            "{name}",
            " ",
            BuiltinParser::NixBuildStatus,
            None,
        )
        .unwrap();
        let specs = vec![nix];
        let inputs = vec![ann(Tier::Check, "other-thing")];
        let (groups, unmatched) = group_by_runner(&specs, &inputs);
        assert!(groups.is_empty());
        assert_eq!(unmatched.len(), 1);
        assert_eq!(unmatched[0].target, "other-thing");
    }

    #[test]
    fn render_target_substitutes_name_and_capture_n() {
        let spec = RunnerSpec::compile(
            "nix",
            Some(r"^nix (build|run) \.#(\S+)$"),
            "nix build {targets}",
            ".#{capture_2}",
            " ",
            BuiltinParser::NixBuildStatus,
            None,
        )
        .unwrap();
        assert_eq!(render_target(&spec, "nix build .#test-loom"), ".#test-loom");
        assert_eq!(render_target(&spec, "nix run .#bar"), ".#bar");
    }

    #[test]
    fn render_target_uses_name_placeholder_when_no_regex() {
        let spec = RunnerSpec::compile(
            "default",
            None,
            "cmd {targets}",
            "test({name})",
            " | ",
            BuiltinParser::LibtestJson,
            None,
        )
        .unwrap();
        assert_eq!(render_target(&spec, "crate::a::ok"), "test(crate::a::ok)");
    }

    #[test]
    fn render_command_joins_targets_and_substitutes_filter_and_targets() {
        let spec = RunnerSpec::compile(
            "nextest",
            None,
            "cargo nextest run -E '{filter}' --format={targets}",
            "test({name})",
            " + ",
            BuiltinParser::LibtestJson,
            None,
        )
        .unwrap();
        let inputs = vec![ann(Tier::Test, "a::one"), ann(Tier::Test, "b::two")];
        let specs = [spec];
        let (groups, _) = group_by_runner(&specs, &inputs);
        let rendered = groups[0].render_command();
        assert!(
            rendered.contains("test(a::one) + test(b::two)"),
            "rendered = {rendered}"
        );
        assert!(
            rendered.starts_with("cargo nextest run -E 'test(a::one) + test(b::two)'"),
            "rendered = {rendered}"
        );
    }

    #[test]
    fn render_inputs_query_places_flag_at_marker_not_command_head() {
        let spec = RunnerSpec::compile(
            "walk",
            Some(r"^cargo run -p loom-walk -- (\S+)$"),
            "cargo run -p loom-walk -- {targets}",
            "{capture_1}",
            " ",
            BuiltinParser::JsonLines,
            None,
        )
        .unwrap()
        .with_inputs(Some(
            "cargo run -p loom-walk -- {targets} {print_inputs}".to_string(),
        ));
        let inputs = vec![ann(Tier::Check, "cargo run -p loom-walk -- foo")];
        let specs = [spec];
        let (groups, _) = group_by_runner(&specs, &inputs);
        let query = groups[0].render_inputs_query().unwrap();
        assert_eq!(query, "cargo run -p loom-walk -- foo --print-inputs");
        assert!(
            !query.starts_with("cargo --print-inputs"),
            "flag must not prepend to tokens[0]: {query}"
        );
    }

    #[test]
    fn render_inputs_query_appends_flag_when_template_omits_marker() {
        let spec = RunnerSpec::compile(
            "walk",
            Some(r"^cargo run -p loom-walk -- (\S+)$"),
            "cargo run -p loom-walk -- {targets}",
            "{capture_1}",
            " ",
            BuiltinParser::JsonLines,
            None,
        )
        .unwrap()
        .with_inputs(Some("cargo run -p loom-walk -- {targets}".to_string()));
        let inputs = vec![ann(Tier::Check, "cargo run -p loom-walk -- foo")];
        let specs = [spec];
        let (groups, _) = group_by_runner(&specs, &inputs);
        let query = groups[0].render_inputs_query().unwrap();
        assert_eq!(query, "cargo run -p loom-walk -- foo --print-inputs");
    }

    #[test]
    fn render_inputs_query_is_none_without_inputs_template() {
        let spec = RunnerSpec::compile(
            "walk",
            Some(r"^cargo run -p loom-walk -- (\S+)$"),
            "cargo run -p loom-walk -- {targets}",
            "{capture_1}",
            " ",
            BuiltinParser::JsonLines,
            None,
        )
        .unwrap();
        let inputs = vec![ann(Tier::Check, "cargo run -p loom-walk -- foo")];
        let specs = [spec];
        let (groups, _) = group_by_runner(&specs, &inputs);
        assert!(
            groups[0].render_inputs_query().is_none(),
            "no inputs query template means the runner stays always-run"
        );
    }

    #[test]
    fn render_command_substitutes_capture_n_from_first_matched_target() {
        let spec = RunnerSpec::compile(
            "nix",
            Some(r"^nix (build|run) \.#(\S+)$"),
            "nix {capture_1} {targets}",
            ".#{capture_2}",
            " ",
            BuiltinParser::NixBuildStatus,
            None,
        )
        .unwrap();
        let inputs = vec![
            ann(Tier::System, "nix build .#a"),
            ann(Tier::System, "nix build .#b"),
        ];
        let specs = [spec];
        let (groups, _) = group_by_runner(&specs, &inputs);
        let rendered = groups[0].render_command();
        assert_eq!(rendered, "nix build .#a .#b");
    }

    #[test]
    fn parse_json_lines_recovers_each_target() {
        let stdout = concat!(
            "{\"target\":\"a\",\"pass\":true,\"evidence\":\"ok\"}\n",
            "noise line\n",
            "{\"target\":\"b\",\"pass\":false,\"evidence\":\"bad\"}\n",
        );
        let map = parse_runner_output(BuiltinParser::JsonLines, stdout, "", true);
        assert_eq!(map.len(), 2);
        assert!(map["a"].pass);
        assert_eq!(map["a"].evidence, "ok");
        assert!(!map["b"].pass);
        assert_eq!(map["b"].evidence, "bad");
    }

    #[test]
    fn parse_json_lines_skips_lines_missing_required_fields() {
        let stdout = concat!(
            "{\"target\":\"a\",\"pass\":true}\n",
            "{\"target\":\"missing_pass\"}\n",
            "{\"pass\":true,\"evidence\":\"no target\"}\n",
        );
        let map = parse_runner_output(BuiltinParser::JsonLines, stdout, "", true);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("a"));
        assert_eq!(map["a"].evidence, "");
    }

    #[test]
    fn parse_libtest_json_maps_ok_and_failed_events() {
        let stdout = concat!(
            "{\"type\":\"suite\",\"event\":\"started\"}\n",
            "{\"type\":\"test\",\"event\":\"started\",\"name\":\"crate::a::one\"}\n",
            "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"crate::a::one\"}\n",
            "{\"type\":\"test\",\"event\":\"failed\",\"name\":\"crate::b::two\",\"stdout\":\"boom\"}\n",
            "{\"type\":\"test\",\"event\":\"ignored\",\"name\":\"crate::c::three\"}\n",
        );
        let map = parse_runner_output(BuiltinParser::LibtestJson, stdout, "", true);
        assert!(map["crate::a::one"].pass);
        assert!(!map["crate::b::two"].pass);
        assert_eq!(map["crate::b::two"].evidence, "boom");
        assert!(map["crate::c::three"].pass, "ignored counts as pass");
    }

    #[test]
    fn parse_junit_xml_extracts_testcases_and_failure_marks_fail() {
        let stdout = concat!(
            "<?xml version=\"1.0\"?>\n",
            "<testsuite>\n",
            "  <testcase classname=\"mod\" name=\"ok_one\"/>\n",
            "  <testcase classname=\"mod\" name=\"fail_two\">\n",
            "    <failure message=\"oops\">stack</failure>\n",
            "  </testcase>\n",
            "</testsuite>\n",
        );
        let map = parse_runner_output(BuiltinParser::JunitXml, stdout, "", false);
        assert_eq!(map.len(), 2);
        assert!(map["mod.ok_one"].pass);
        assert!(!map["mod.fail_two"].pass);
        assert_eq!(map["mod.fail_two"].evidence, "oops");
    }

    #[test]
    fn parse_nix_build_status_classifies_built_vs_failed() {
        let stderr = concat!(
            "building '/nix/store/aaa-pkg-good.drv'...\n",
            "building '/nix/store/bbb-pkg-bad.drv'...\n",
            "error: builder for '/nix/store/bbb-pkg-bad.drv' failed with exit code 1\n",
        );
        let map = parse_runner_output(BuiltinParser::NixBuildStatus, "", stderr, false);
        assert_eq!(map.len(), 2);
        assert!(map["pkg-good"].pass);
        assert!(!map["pkg-bad"].pass);
    }

    #[test]
    fn parse_exit_code_uses_status_for_single_verdict() {
        let map = parse_runner_output(BuiltinParser::ExitCode, "stdout body", "", true);
        assert_eq!(map.len(), 1);
        let only = map.values().next().unwrap();
        assert!(only.pass);
        assert_eq!(only.evidence, "stdout body");

        let fail = parse_runner_output(BuiltinParser::ExitCode, "", "stderr body", false);
        let only = fail.values().next().unwrap();
        assert!(!only.pass);
        assert_eq!(only.evidence, "stderr body");
    }

    fn config_with_check_and_system_runners() -> LoomConfig {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("loom.toml"),
            r#"
[runner.check.grep]
match   = '^grep '
command = "{targets}"

[runner.system.nix]
match   = '^nix (build|run) \.#(\S+)$'
command = "nix build {targets}"
parse   = "nix-build-status"
"#,
        )
        .unwrap();
        LoomConfig::load(LoomConfig::resolve_path(dir.path())).unwrap()
    }

    #[test]
    fn integrity_runner_specs_carries_builtin_loom_walk_batcher() {
        let specs = integrity_runner_specs(&LoomConfig::default()).unwrap();
        assert!(
            specs.iter().any(|s| s.name == "builtin-loom-walk"
                && s.matches("cargo run -p loom-walk -- inputs-check")),
            "the builtin loom-walk batcher must always be present and own its target",
        );
    }

    #[test]
    fn integrity_runner_specs_unions_check_and_system_runners() {
        let config = config_with_check_and_system_runners();
        let specs = integrity_runner_specs(&config).unwrap();
        assert!(
            specs.iter().any(|s| s.matches("grep -q X file")),
            "a [check] target must resolve through its [runner.check.<name>] runner",
        );
        assert!(
            specs.iter().any(|s| s.matches("nix run .#test-loom")),
            "a [system] target must resolve through its [runner.system.<name>] runner",
        );
    }

    #[test]
    fn compile_runner_entry_without_command_is_missing_command_error() {
        let entry = RunnerEntry {
            match_regex: Some("^x".into()),
            ..RunnerEntry::default()
        };
        let err = compile_runner_entry("orphan", &entry).unwrap_err();
        assert!(matches!(err, RunnerError::MissingCommand { name } if name == "orphan"));
    }
}
