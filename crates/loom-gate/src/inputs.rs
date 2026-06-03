//! Verifier-input declarations.
//!
//! Each verifier's declared inputs (gitignore-style globs relative to the
//! repo root) decide whether the gate runs the verifier under a given
//! scope. Per `specs/gate.md` § Verifier inputs the declarations
//! come from one of four sources, with the spec section the annotation
//! lives in auto-included on every resolution:
//!
//! 1. **`[test]` test-framework metadata** — for Rust workspaces, the
//!    annotation's owning crate's source files via [`TestScope`]; for
//!    other toolchains, the `inputs_for_test` config override invokes a
//!    consumer-supplied helper.
//! 2. **`[judge]` collect mode** — the rubric script is run through the
//!    loom judge-harness preamble, which defines `judge_files` to *record*
//!    its path arguments and `judge_criterion` (with any LLM call) as a
//!    no-op. One `<script> --print-inputs` spawn emits the batch map
//!    `{"inputs": {"<fn>": ["glob", ...], ...}}` for every rubric the
//!    script defines; the per-function entries are cached per session so a
//!    script referenced from N criteria spawns the harness once.
//! 3. **`[check]` / `[system]` `--print-inputs` protocol** — a target a
//!    runner `match`es is owned by that runner: the query is issued
//!    through the runner's `inputs` command template (with the
//!    `{print_inputs}` flag placed where the template dictates), batched
//!    so one spawn returns the matched group's per-target map. An
//!    unmatched target keeps literal-command semantics — its first token
//!    is spawned with `--print-inputs` after the verifier's own argv.
//!    Stdout is parsed as the single `{"inputs": ["glob1", ...]}` or batch
//!    `{"inputs": {"<target>": [...]}}` form; results are cached per
//!    session.
//! 4. **Heuristic fallback** — best-effort path extraction from the
//!    command tokens. Recognises `grep`-style file arguments and
//!    `cargo test -p <crate>` patterns.
//!
//! A verifier that declares no inputs of its own — every source above
//! yielded nothing — is not an error. Per `specs/gate.md` § Verifier
//! inputs (*Conservative default*) an undeterminable input set is never
//! grounds to skip: "inputs unknown" resolves to *run*, not to narrow to
//! the spec section. [`InputResolver::declares_no_inputs`] reports that
//! state and [`filter_by_files`] honours it by always retaining such a
//! verifier under any finite scope.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use displaydoc::Display;
use serde::Deserialize;
use thiserror::Error;

use crate::annotation::{Annotation, Tier};
use crate::dispatch::TestScope;
use crate::runner::{RunnerSpec, group_by_runner};

/// Repo-relative paths/globs declared as the verifier's inputs. The
/// gate filters verifiers by intersecting these with the scope's
/// `--files` input set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifierInputs {
    pub paths: Vec<PathBuf>,
}

/// Failures surfaced while reading or invoking input-declaration
/// sources. Surfaced individually so the resolver can fall through to
/// the next source rather than failing the gate over a misdeclared
/// helper.
#[derive(Debug, Display, Error)]
pub enum InputsError {
    /// failed to read script `{path}`: {source}
    ReadScript {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// failed to spawn `{command}` for input discovery: {source}
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    /// `{command}` did not return JSON with an `inputs` array: {detail}
    BadProtocol { command: String, detail: String },
}

/// JSON document `--print-inputs` and `inputs_for_test` helpers emit.
#[derive(Debug, Deserialize)]
struct PrintInputsDoc {
    inputs: Vec<String>,
}

/// JSON document the judge collect-mode batch query emits — a per-rubric
/// map rather than a flat list. Sorted so the resolver's cache population
/// and any rendered diagnostics are deterministic.
#[derive(Debug, Deserialize)]
struct PrintInputsBatchDoc {
    inputs: std::collections::BTreeMap<String, Vec<String>>,
}

/// Shell preamble that makes a judge rubric script runnable in *collect
/// mode*. `judge_files` records its path arguments and `judge_criterion`
/// (standing in for the evaluation / LLM call) is a no-op, so *running* a
/// rubric reports the files it examines instead of judging them. The
/// rubric set is the functions the sourced script defines minus the
/// harness's own. Dispatches on `--print-inputs [<fn>]`: with a function
/// name it emits that rubric's `{"inputs":[...]}`; with none it emits the
/// batch map `{"inputs":{"<fn>":[...]}}` for every rubric in one spawn.
/// Invoked as `bash -c <harness> loom-judge-harness <script> --print-inputs
/// [<fn>]`, so `$1` is the script path and the dispatch args follow `shift`.
const JUDGE_COLLECT_HARNESS: &str = r#"set -euo pipefail
__loom_files=()
judge_files() { __loom_files+=("$@"); }
judge_criterion() { :; }
__loom_json_str() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  printf '"%s"' "$s"
}
__loom_files_json() {
  local out="[" first=1 p
  for p in ${__loom_files[@]+"${__loom_files[@]}"}; do
    if [[ $first -eq 1 ]]; then first=0; else out+=","; fi
    out+=$(__loom_json_str "$p")
  done
  printf '%s]' "$out"
}
__loom_run_one() {
  __loom_files=()
  "$1"
  __loom_files_json
}
__loom_script=$1
shift
__loom_harness=" judge_files judge_criterion __loom_json_str __loom_files_json __loom_run_one "
source "$__loom_script"
__loom_rubrics=()
while read -r _ _ __loom_name; do
  case "$__loom_harness" in
    *" $__loom_name "*) continue ;;
  esac
  case "$__loom_name" in
    __loom_*) continue ;;
  esac
  __loom_rubrics+=("$__loom_name")
done < <(declare -F)
if [[ ${1:-} != "--print-inputs" ]]; then
  exit 0
fi
if [[ -n ${2:-} ]]; then
  printf '{"inputs":%s}\n' "$(__loom_run_one "$2")"
  exit 0
fi
out='{"inputs":{'
first=1
for __loom_r in ${__loom_rubrics[@]+"${__loom_rubrics[@]}"}; do
  if [[ $first -eq 1 ]]; then first=0; else out+=","; fi
  out+="$(__loom_json_str "$__loom_r"):$(__loom_run_one "$__loom_r")"
done
out+="}}"
printf '%s\n' "$out"
"#;

/// Stateful resolver — `--print-inputs` invocations are cached per
/// session so the same binary is not spawned twice for two annotations
/// that share a command prefix.
pub struct InputResolver {
    repo_root: PathBuf,
    test_scope: Option<Box<dyn TestScope>>,
    inputs_for_test_command: Option<String>,
    runners: Vec<RunnerSpec>,
    print_inputs_cache: HashMap<String, Vec<PathBuf>>,
}

impl InputResolver {
    /// Build a resolver rooted at `repo_root`. The repo root determines
    /// what counts as an in-repo script (Source 2 of the priority
    /// order) and where helper subprocesses are spawned from.
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            test_scope: None,
            inputs_for_test_command: None,
            runners: Vec::new(),
            print_inputs_cache: HashMap::new(),
        }
    }

    /// Attach the `[check]` / `[system]` runner specs the resolver
    /// consults to route the input-query. A target a runner `match`es is
    /// owned by that runner — its inputs come from the runner's `inputs`
    /// query template, never from argv-mangling `tokens[0]`. Targets no
    /// runner matches keep literal-command semantics. See
    /// `specs/gate.md` § Runners.
    #[must_use]
    pub fn with_runners(mut self, runners: Vec<RunnerSpec>) -> Self {
        self.runners = runners;
        self
    }

    /// Attach a [`TestScope`] used for `[test]`-tier resolution and
    /// for the `cargo test -p <crate>` heuristic. Calling consumers
    /// typically pass a `CargoMetadataScope`.
    #[must_use]
    pub fn with_test_scope(mut self, scope: Box<dyn TestScope>) -> Self {
        self.test_scope = Some(scope);
        self
    }

    /// Override the cargo-metadata source for `[test]`-tier resolution
    /// with a consumer-supplied helper. The helper is invoked with the
    /// annotation target appended to the command string and must emit
    /// `{"inputs": [...]}` on stdout.
    #[must_use]
    pub fn with_inputs_for_test_command(mut self, command: String) -> Self {
        self.inputs_for_test_command = Some(command);
        self
    }

    /// Resolve declared inputs for one annotation. The annotation's
    /// `source_spec` is folded into the result as an *additional* input
    /// (the spec-section auto-include rule), so the returned set is never
    /// observably empty. The auto-include is not the resolution floor: a
    /// verifier that declares nothing of its own is the *Conservative
    /// default* case, handled by [`filter_by_files`], not narrowed to the
    /// spec section here.
    pub fn resolve(&mut self, annotation: &Annotation) -> VerifierInputs {
        self.resolve_with_provenance(annotation).0
    }

    /// Resolve declared inputs and report whether the verifier declared
    /// any of its own (`true` in the second field iff at least one
    /// declaration source yielded a path before the spec-section
    /// auto-include). Walks the declaration chain once, so callers that
    /// need both the resolved set and the *Conservative default* signal
    /// pay for resolution a single time.
    pub fn resolve_with_provenance(&mut self, annotation: &Annotation) -> (VerifierInputs, bool) {
        let mut paths: Vec<PathBuf> = self.collect_declared(annotation);
        let declared_own = !paths.is_empty();
        let spec = annotation.source_spec.clone();
        if !paths.iter().any(|p| p == &spec) {
            paths.push(spec);
        }
        (VerifierInputs { paths }, declared_own)
    }

    /// True iff the verifier declares no inputs of its own — every
    /// declaration source (test-framework metadata, judge collect mode,
    /// `--print-inputs`, heuristic fallback) yielded nothing, so the
    /// resolved input set would be the spec-section auto-include alone.
    /// Per `specs/gate.md` § Verifier inputs (*Conservative default*)
    /// such a verifier always runs; [`filter_by_files`] consumes this to
    /// retain it under any finite scope rather than skip it.
    pub fn declares_no_inputs(&mut self, annotation: &Annotation) -> bool {
        self.collect_declared(annotation).is_empty()
    }

    fn collect_declared(&mut self, annotation: &Annotation) -> Vec<PathBuf> {
        match annotation.tier {
            Tier::Test => self.declared_for_test(annotation),
            Tier::Judge => self.declared_for_judge(annotation),
            Tier::Check | Tier::System => self.declared_for_command(annotation),
        }
    }

    /// `[judge]` targets are a single spec-relative script path carrying a
    /// `#fn`/`::fn` selector (e.g. `../tests/judges/loom.sh#judge_x`), not
    /// a shell command. The selector and `..` prefix make a raw repo-root
    /// join miss on disk, so resolution mirrors the integrity gate: strip
    /// the selector, resolve against the spec file's own directory, then
    /// run the rubric in collect mode (`<script> --print-inputs`) to learn
    /// the paths its `judge_files` calls examine. One batch spawn maps every
    /// rubric the script defines; entries are cached per session keyed by
    /// `<script>#<fn>` so N criteria on one script spawn the harness once.
    /// A rubric that calls no `judge_files` declares nothing of its own, so
    /// it falls through to the *Conservative default* (always runs) per
    /// [`filter_by_files`].
    fn declared_for_judge(&mut self, annotation: &Annotation) -> Vec<PathBuf> {
        let Some(script) = crate::integrity::resolve_spec_relative_script_path(
            &annotation.target,
            &annotation.source_spec,
            &self.repo_root,
        ) else {
            return Vec::new();
        };
        if !script.is_file() {
            return Vec::new();
        }
        let Some(function) = target_selector(&annotation.target) else {
            return Vec::new();
        };
        let cache_key = judge_cache_key(&script, function);
        if let Some(cached) = self.print_inputs_cache.get(&cache_key) {
            return cached.clone();
        }
        let Some(stdout) = run_judge_collect(&self.repo_root, &script, None) else {
            return Vec::new();
        };
        let Some(batch) = parse_inputs_batch_json(&stdout) else {
            return Vec::new();
        };
        for (rubric, paths) in batch {
            self.print_inputs_cache
                .insert(judge_cache_key(&script, &rubric), paths);
        }
        self.print_inputs_cache
            .get(&cache_key)
            .cloned()
            .unwrap_or_default()
    }

    fn declared_for_test(&mut self, annotation: &Annotation) -> Vec<PathBuf> {
        if let Some(command) = self.inputs_for_test_command.clone()
            && let Some(paths) = self.invoke_inputs_helper(&command, &annotation.target)
        {
            return paths;
        }
        self.test_scope
            .as_ref()
            .map(|scope| scope.scope_for(annotation))
            .unwrap_or_default()
    }

    fn declared_for_command(&mut self, annotation: &Annotation) -> Vec<PathBuf> {
        let target = annotation.target.trim();
        let Some(tokens) = shlex::split(target) else {
            return Vec::new();
        };
        if tokens.is_empty() {
            return Vec::new();
        }

        if let Some(paths) = self.runner_owned_inputs(annotation) {
            return paths;
        }

        let cache_key = target.to_string();
        if let Some(cached) = self.print_inputs_cache.get(&cache_key) {
            return cached.clone();
        }
        if let Some(paths) = self.invoke_print_inputs(&tokens) {
            self.print_inputs_cache.insert(cache_key, paths.clone());
            return paths;
        }

        self.heuristic_extract(&tokens)
    }

    /// Resolve a runner-matched `[check]` / `[system]` target's inputs
    /// through the matched runner's `inputs` query template. Returns
    /// `Some` (terminal — the runner owns the annotation end to end, so no
    /// argv-mangling or heuristic fallback) when a runner matches the
    /// target, `None` when none does (the caller falls through to
    /// literal-command semantics). A matched runner with no `inputs` query
    /// yields `Some(empty)`: the conservative always-run default, never a
    /// `tokens[0]` probe. The query is spawned once per matched group; a
    /// batch response (`{"inputs": {"<target>": [...]}}`) primes the cache
    /// for every sibling, so discovery batches where execution batches.
    fn runner_owned_inputs(&mut self, annotation: &Annotation) -> Option<Vec<PathBuf>> {
        let (runner_name, rendered_target, query) = {
            let (groups, _) = group_by_runner(&self.runners, std::slice::from_ref(annotation));
            let group = groups.into_iter().next()?;
            let rendered_target = group.matched.first()?.rendered_target.clone();
            (
                group.spec.name.clone(),
                rendered_target,
                group.render_inputs_query(),
            )
        };
        let cache_key = runner_cache_key(&runner_name, &rendered_target);
        if let Some(cached) = self.print_inputs_cache.get(&cache_key) {
            return Some(cached.clone());
        }
        let Some(query) = query else {
            return Some(Vec::new());
        };
        let Some(stdout) = run_command_query(&self.repo_root, &query) else {
            return Some(Vec::new());
        };
        if let Some(batch) = parse_inputs_batch_json(&stdout) {
            for (target, paths) in batch {
                self.print_inputs_cache
                    .insert(runner_cache_key(&runner_name, &target), paths);
            }
        } else if let Some(paths) = parse_inputs_json(&stdout) {
            self.print_inputs_cache.insert(cache_key.clone(), paths);
        }
        Some(
            self.print_inputs_cache
                .get(&cache_key)
                .cloned()
                .unwrap_or_default(),
        )
    }

    fn invoke_print_inputs(&self, tokens: &[String]) -> Option<Vec<PathBuf>> {
        let head = tokens.first()?;
        let tail = &tokens[1..];
        let mut cmd = Command::new(head);
        cmd.arg("--print-inputs").args(tail);
        cmd.current_dir(&self.repo_root);
        let output = cmd.output().ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_inputs_json(&stdout)
    }

    fn invoke_inputs_helper(&self, command: &str, test_target: &str) -> Option<Vec<PathBuf>> {
        let mut tokens = shlex::split(command)?;
        tokens.push(test_target.to_string());
        let (head, tail) = tokens.split_first()?;
        let mut cmd = Command::new(head);
        cmd.args(tail);
        cmd.current_dir(&self.repo_root);
        let output = cmd.output().ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_inputs_json(&stdout)
    }

    fn heuristic_extract(&self, tokens: &[String]) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for path in heuristic_paths(tokens, &self.repo_root) {
            push_unique(&mut out, path);
        }
        if let Some(crate_name) = cargo_test_crate_name(tokens)
            && let Some(scope) = self.test_scope.as_ref()
        {
            let synthetic = Annotation {
                tier: Tier::Test,
                target: format!("{crate_name}::__heuristic"),
                source_spec: PathBuf::new(),
                line: 0,
                criterion_line: 0,
                pending: false,
            };
            for path in scope.scope_for(&synthetic) {
                push_unique(&mut out, path);
            }
        }
        out
    }
}

/// Retain annotations the scope `files` could affect. An empty `files`
/// slice short-circuits to the caller's input unchanged — "no `--files`
/// filter requested." Otherwise an annotation is kept when either its
/// declared inputs (per [`InputResolver`]) intersect `files`, or it
/// declares no inputs of its own — the *Conservative default* in
/// `specs/gate.md` § Verifier inputs, under which an undeterminable input
/// set always runs rather than being narrowed to the spec section.
/// Matches the `loom gate verify --files` contract in
/// `specs/pre-commit.md`.
pub fn filter_by_files(
    annotations: &[Annotation],
    files: &[PathBuf],
    resolver: &mut InputResolver,
) -> Vec<Annotation> {
    if files.is_empty() {
        return annotations.to_vec();
    }
    let file_set: HashSet<&Path> = files.iter().map(PathBuf::as_path).collect();
    annotations
        .iter()
        .filter(|ann| {
            let (inputs, declared_own) = resolver.resolve_with_provenance(ann);
            !declared_own || inputs.paths.iter().any(|p| file_set.contains(p.as_path()))
        })
        .cloned()
        .collect()
}

fn push_unique(buf: &mut Vec<PathBuf>, path: PathBuf) {
    if !buf.contains(&path) {
        buf.push(path);
    }
}

fn parse_inputs_json(stdout: &str) -> Option<Vec<PathBuf>> {
    for raw in stdout.lines().rev() {
        let line = raw.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        if let Ok(doc) = serde_json::from_str::<PrintInputsDoc>(line) {
            return Some(doc.inputs.into_iter().map(PathBuf::from).collect());
        }
    }
    None
}

/// Parse the judge collect-mode batch document — `{"inputs": {"<fn>":
/// ["glob", ...], ...}}` — into a per-rubric path map. Scans stdout
/// bottom-up like [`parse_inputs_json`] so leading helper chatter is
/// ignored; the object-valued `inputs` disambiguates it from the
/// array-valued single-target form.
fn parse_inputs_batch_json(stdout: &str) -> Option<HashMap<String, Vec<PathBuf>>> {
    for raw in stdout.lines().rev() {
        let line = raw.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        if let Ok(doc) = serde_json::from_str::<PrintInputsBatchDoc>(line) {
            return Some(
                doc.inputs
                    .into_iter()
                    .map(|(fn_name, globs)| {
                        (fn_name, globs.into_iter().map(PathBuf::from).collect())
                    })
                    .collect(),
            );
        }
    }
    None
}

/// Run a judge rubric script in collect mode under the loom judge-harness
/// preamble. With `function` set, returns the single-target document's
/// stdout (`{"inputs":[...]}`); with `None`, the batch map's stdout. Falls
/// through to `None` when the harness fails to spawn or exits non-zero, so
/// the resolver lands on the *Conservative default* rather than the gate
/// crashing over a malformed rubric.
fn run_judge_collect(repo_root: &Path, script: &Path, function: Option<&str>) -> Option<String> {
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(JUDGE_COLLECT_HARNESS)
        .arg("loom-judge-harness")
        .arg(script)
        .arg("--print-inputs");
    if let Some(function) = function {
        cmd.arg(function);
    }
    cmd.current_dir(repo_root);
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Per-session cache key for one rubric's collect-mode inputs. Shares
/// [`InputResolver::print_inputs_cache`] with the `[check]` / `[system]`
/// `--print-inputs` results; the `<script>#<fn>` shape keeps judge keys
/// distinct from raw command strings.
fn judge_cache_key(script: &Path, function: &str) -> String {
    format!("{}#{function}", script.display())
}

/// Per-session cache key for one runner-matched target's input-query
/// result. The `runner:` prefix keeps these distinct from raw command
/// strings and `<script>#<fn>` judge keys sharing the same map, and the
/// rendered target lets a batch response prime every sibling in the group.
fn runner_cache_key(runner: &str, rendered_target: &str) -> String {
    format!("runner:{runner}\u{1f}{rendered_target}")
}

/// Spawn a runner's rendered input-query command in `repo_root` and return
/// its stdout, or `None` when the command fails to spawn or exits
/// non-zero. A non-zero exit falls through to the conservative always-run
/// default here; surfacing it as a loud `inputs-protocol-error` is the
/// integrity gate's job (see `specs/gate.md` § Inputs-protocol error).
fn run_command_query(repo_root: &Path, command: &str) -> Option<String> {
    let mut tokens = shlex::split(command)?.into_iter();
    let head = tokens.next()?;
    let tail: Vec<String> = tokens.collect();
    let mut cmd = Command::new(head);
    cmd.args(&tail);
    cmd.current_dir(repo_root);
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The `#fn` / `::fn` selector trailing a script-path target, or `None`
/// when the target carries no selector. Mirrors the integrity gate's
/// selector handling so the same target resolves to the same rubric name.
fn target_selector(target: &str) -> Option<&str> {
    let trimmed = target.trim();
    let hash = trimmed.find('#');
    let colons = trimmed.find("::");
    let (pos, len) = match (hash, colons) {
        (Some(h), Some(c)) if h < c => (h, 1),
        (Some(h), None) => (h, 1),
        (_, Some(c)) => (c, 2),
        (None, None) => return None,
    };
    let selector = trimmed[pos + len..].trim();
    if selector.is_empty() {
        None
    } else {
        Some(selector)
    }
}

/// Extract `-p <crate>` (or `--package <crate>`) from a `cargo test`
/// invocation; returns `None` for non-cargo-test commands or when no
/// package is named.
fn cargo_test_crate_name(tokens: &[String]) -> Option<String> {
    if tokens.first().map(String::as_str) != Some("cargo") {
        return None;
    }
    if tokens.get(1).map(String::as_str) != Some("test")
        && tokens.get(1).map(String::as_str) != Some("nextest")
    {
        return None;
    }
    let mut iter = tokens.iter().skip(2);
    while let Some(tok) = iter.next() {
        if tok == "-p" || tok == "--package" {
            return iter.next().cloned();
        }
        if let Some(value) = tok.strip_prefix("--package=") {
            return Some(value.to_string());
        }
        if let Some(value) = tok.strip_prefix("-p=") {
            return Some(value.to_string());
        }
    }
    None
}

/// Pick tokens that look like paths and exist under `repo_root`. Skips
/// command-name tokens and flag-style tokens. The first positional
/// argument that resolves under the repo wins; subsequent matches
/// are returned in token order so multi-file commands round-trip.
fn heuristic_paths(tokens: &[String], repo_root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for (idx, tok) in tokens.iter().enumerate() {
        if idx == 0 {
            continue;
        }
        if tok.starts_with('-') {
            continue;
        }
        if !looks_like_path(tok) {
            continue;
        }
        let candidate = PathBuf::from(tok);
        let absolute = if candidate.is_absolute() {
            candidate.clone()
        } else {
            repo_root.join(&candidate)
        };
        if absolute.exists() {
            out.push(candidate);
        }
    }
    out
}

fn looks_like_path(tok: &str) -> bool {
    if tok.contains('/') {
        return true;
    }
    tok.contains('.') && tok.len() >= 4
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    use std::collections::HashMap;
    use std::fs;

    fn ann(tier: Tier, target: &str, spec: &str) -> Annotation {
        Annotation {
            tier,
            target: target.into(),
            source_spec: PathBuf::from(spec),
            line: 10,
            criterion_line: 9,
            pending: false,
        }
    }

    struct StubScope {
        map: HashMap<String, Vec<PathBuf>>,
    }

    impl StubScope {
        fn new(entries: &[(&str, &[&str])]) -> Self {
            let map = entries
                .iter()
                .map(|(t, fs)| {
                    (
                        (*t).to_string(),
                        fs.iter().map(PathBuf::from).collect::<Vec<_>>(),
                    )
                })
                .collect();
            Self { map }
        }
    }

    impl TestScope for StubScope {
        fn scope_for(&self, a: &Annotation) -> Vec<PathBuf> {
            // Match by the first `::` segment so the heuristic's
            // synthetic `<crate>::__heuristic` target still hits.
            let key = a.target.split("::").next().unwrap_or("");
            self.map.get(key).cloned().unwrap_or_default()
        }
    }

    #[test]
    fn test_tier_uses_cargo_metadata_scope_plus_spec_autoinclude() {
        let scope = Box::new(StubScope::new(&[(
            "loom_gate",
            &["crates/loom-gate/src/lib.rs"],
        )]));
        let mut resolver = InputResolver::new(PathBuf::from("/repo")).with_test_scope(scope);
        let a = ann(Tier::Test, "loom_gate::module::ok", "specs/gate.md");
        let got = resolver.resolve(&a);
        assert!(
            got.paths
                .contains(&PathBuf::from("crates/loom-gate/src/lib.rs")),
            "test scope source must appear: {:?}",
            got.paths,
        );
        assert!(
            got.paths.contains(&PathBuf::from("specs/gate.md")),
            "spec section auto-included: {:?}",
            got.paths,
        );
    }

    #[test]
    fn check_tier_ignores_in_script_loom_inputs_header() {
        // The retired in-script `# loom-inputs:` header is inert: its globs
        // must never surface, since inputs now come only by execution.
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("walk.sh");
        fs::write(
            &script_path,
            "#!/bin/sh\n# loom-inputs: src/walk/*.rs, src/lib.rs\necho hi\n",
        )
        .unwrap();

        let mut resolver = InputResolver::new(dir.path().to_path_buf());
        let target = format!("sh {}", script_path.display());
        let a = ann(Tier::Check, &target, "specs/gate.md");
        let got = resolver.resolve(&a);
        assert!(
            !got.paths.contains(&PathBuf::from("src/walk/*.rs")),
            "in-script header glob must not be read: {:?}",
            got.paths,
        );
        assert!(
            !got.paths.contains(&PathBuf::from("src/lib.rs")),
            "in-script header glob must not be read: {:?}",
            got.paths,
        );
        assert!(
            got.paths.contains(&PathBuf::from("specs/gate.md")),
            "spec section still auto-included: {:?}",
            got.paths,
        );
    }

    #[test]
    fn binary_print_inputs_protocol_parses_json_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("walk-helper.sh");
        fs::write(
            &helper,
            "#!/bin/sh\nif [ \"$1\" = \"--print-inputs\" ]; then\n  printf '{\"inputs\": [\"src/a.rs\", \"src/b.rs\"]}\\n'\n  exit 0\nfi\nexit 99\n",
        )
        .unwrap();
        let mut perms = fs::metadata(&helper).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            perms.set_mode(0o755);
        }
        fs::set_permissions(&helper, perms).unwrap();

        let mut resolver = InputResolver::new(dir.path().to_path_buf());
        // Target's first token resolves to a binary supporting the
        // protocol, so the unmatched-runner path falls through to the
        // `--print-inputs` probe on the command's first token.
        let target = format!("{} walks/foo", helper.display());
        let a = ann(Tier::Check, &target, "specs/x.md");
        let got = resolver.resolve(&a);
        assert!(got.paths.contains(&PathBuf::from("src/a.rs")));
        assert!(got.paths.contains(&PathBuf::from("src/b.rs")));
    }

    #[test]
    fn print_inputs_results_cached_across_resolutions() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("count.txt");
        fs::write(&counter, "0").unwrap();
        let helper = dir.path().join("count-helper.sh");
        let counter_path = counter.display().to_string();
        fs::write(
            &helper,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--print-inputs\" ]; then\n  n=$(cat {counter_path})\n  echo $((n + 1)) > {counter_path}\n  printf '{{\"inputs\": [\"x.rs\"]}}\\n'\n  exit 0\nfi\nexit 99\n",
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut perms = fs::metadata(&helper).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&helper, perms).unwrap();
        }

        let mut resolver = InputResolver::new(dir.path().to_path_buf());
        let target = format!("{} walks/foo", helper.display());
        let a = ann(Tier::Check, &target, "specs/x.md");
        let first = resolver.resolve(&a);
        let second = resolver.resolve(&a);
        assert_eq!(first.paths, second.paths);
        let observed = fs::read_to_string(&counter).unwrap();
        assert_eq!(observed.trim(), "1", "helper invoked exactly once");
    }

    #[test]
    fn heuristic_extracts_grep_file_argument() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("path/to/file.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "").unwrap();
        let mut resolver = InputResolver::new(dir.path().to_path_buf());
        let a = ann(Tier::Check, "grep -q 'X' path/to/file.rs", "specs/x.md");
        let got = resolver.resolve(&a);
        assert!(
            got.paths.contains(&PathBuf::from("path/to/file.rs")),
            "{:?}",
            got.paths,
        );
    }

    #[test]
    fn heuristic_routes_cargo_test_p_through_test_scope() {
        let scope = Box::new(StubScope::new(&[(
            "mycrate",
            &["crates/mycrate/src/lib.rs"],
        )]));
        let mut resolver = InputResolver::new(PathBuf::from("/repo")).with_test_scope(scope);
        let a = ann(
            Tier::Check,
            "cargo test -p mycrate --lib happy_name",
            "specs/x.md",
        );
        let got = resolver.resolve(&a);
        assert!(
            got.paths
                .contains(&PathBuf::from("crates/mycrate/src/lib.rs")),
            "cargo-test heuristic routes through test scope: {:?}",
            got.paths,
        );
    }

    #[test]
    fn heuristic_supports_cargo_test_package_equals_syntax() {
        let scope = Box::new(StubScope::new(&[("mycrate", &["crates/mycrate/src/x.rs"])]));
        let mut resolver = InputResolver::new(PathBuf::from("/repo")).with_test_scope(scope);
        let a = ann(Tier::Check, "cargo test --package=mycrate", "specs/x.md");
        let got = resolver.resolve(&a);
        assert!(
            got.paths
                .contains(&PathBuf::from("crates/mycrate/src/x.rs"))
        );
    }

    #[test]
    fn judge_tier_strips_selector_and_collects_relative_to_spec_dir() {
        let dir = tempfile::tempdir().unwrap();
        // Judge target is spec-relative with a `#fn` selector; the script
        // lives at <repo>/tests/judges/loom.sh while the spec is under
        // <repo>/specs, so the `../` must resolve against the spec dir.
        let script_dir = dir.path().join("tests/judges");
        fs::create_dir_all(&script_dir).unwrap();
        fs::write(
            script_dir.join("loom.sh"),
            "#!/usr/bin/env bash\njudge_x() { judge_files \"crates/loom-llm/src/**\" \"specs/harness.md\"; judge_criterion \"eval\"; }\n",
        )
        .unwrap();

        let mut resolver = InputResolver::new(dir.path().to_path_buf());
        let a = ann(
            Tier::Judge,
            "../tests/judges/loom.sh#judge_x",
            "specs/harness.md",
        );
        let got = resolver.resolve(&a);
        assert!(
            got.paths.contains(&PathBuf::from("crates/loom-llm/src/**")),
            "judge_files glob collected: {:?}",
            got.paths,
        );
        assert!(
            got.paths.contains(&PathBuf::from("specs/harness.md")),
            "judge_files glob + spec auto-include: {:?}",
            got.paths,
        );
        assert!(
            !resolver.declares_no_inputs(&a),
            "a judge rubric calling judge_files declares inputs",
        );
    }

    #[test]
    fn print_inputs_issued_through_command_template_not_argv_head() {
        let dir = tempfile::tempdir().unwrap();
        // Mimics `loom-walk`: the walk name is the responder's first arg
        // and `--print-inputs` is a *later* flag. If the flag were
        // prepended to the command's first token (the argv-head bug), the
        // walk name would be `--print-inputs` and the responder errors —
        // emitting no inputs document. Routed through the runner template,
        // the flag lands after the walk name and the responder answers.
        let responder = dir.path().join("responder.sh");
        fs::write(
            &responder,
            "#!/bin/sh\n\
             walk=$1\n\
             if [ \"$walk\" = \"--print-inputs\" ]; then\n\
               echo 'error: --print-inputs is not a walk name' >&2\n\
               exit 2\n\
             fi\n\
             shift\n\
             for arg in \"$@\"; do\n\
               if [ \"$arg\" = \"--print-inputs\" ]; then\n\
                 printf '{\"inputs\": [\"crates/loom-walk/src/foo.rs\"]}\\n'\n\
                 exit 0\n\
               fi\n\
             done\n\
             exit 1\n",
        )
        .unwrap();

        let spec = RunnerSpec::compile(
            "walk",
            Some(r"^cargo run -p loom-walk -- (\S+)$"),
            "cargo run -p loom-walk -- {targets}",
            "{capture_1}",
            " ",
            crate::runner::BuiltinParser::JsonLines,
            None,
        )
        .unwrap()
        .with_inputs(Some(format!(
            "sh {} {{targets}} {{print_inputs}}",
            responder.display()
        )));

        let mut resolver = InputResolver::new(dir.path().to_path_buf()).with_runners(vec![spec]);
        let a = ann(
            Tier::Check,
            "cargo run -p loom-walk -- foo",
            "specs/gate.md",
        );
        let got = resolver.resolve(&a);
        assert!(
            got.paths
                .contains(&PathBuf::from("crates/loom-walk/src/foo.rs")),
            "input-query routed through the runner template (flag after the \
             walk name), not prepended to tokens[0]: {:?}",
            got.paths,
        );
    }

    #[test]
    fn runner_matched_target_with_no_inputs_query_stays_always_run() {
        let dir = tempfile::tempdir().unwrap();
        // A runner matches the target but declares no `inputs` query, so the
        // runner owns the annotation end to end: no argv-mangling probe, and
        // the conservative always-run default applies (declares no inputs).
        let spec = RunnerSpec::compile(
            "walk",
            Some(r"^cargo run -p loom-walk -- (\S+)$"),
            "cargo run -p loom-walk -- {targets}",
            "{capture_1}",
            " ",
            crate::runner::BuiltinParser::JsonLines,
            None,
        )
        .unwrap();
        let mut resolver = InputResolver::new(dir.path().to_path_buf()).with_runners(vec![spec]);
        let a = ann(
            Tier::Check,
            "cargo run -p loom-walk -- foo",
            "specs/gate.md",
        );
        assert!(
            resolver.declares_no_inputs(&a),
            "matched runner with no inputs query relies on the always-run default",
        );
    }

    #[test]
    fn judge_tier_accepts_legacy_colon_selector() {
        let dir = tempfile::tempdir().unwrap();
        let script_dir = dir.path().join("tests/judges");
        fs::create_dir_all(&script_dir).unwrap();
        fs::write(
            script_dir.join("loom.sh"),
            "#!/usr/bin/env bash\njudge_x() { judge_files \"crates/loom-gate/src/lib.rs\"; }\n",
        )
        .unwrap();

        let mut resolver = InputResolver::new(dir.path().to_path_buf());
        let a = ann(
            Tier::Judge,
            "../tests/judges/loom.sh::judge_x",
            "specs/gate.md",
        );
        let got = resolver.resolve(&a);
        assert!(
            got.paths
                .contains(&PathBuf::from("crates/loom-gate/src/lib.rs")),
            "legacy `::fn` selector resolves the same rubric: {:?}",
            got.paths,
        );
    }

    #[test]
    fn judge_tier_without_judge_files_declares_no_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let script_dir = dir.path().join("tests/judges");
        fs::create_dir_all(&script_dir).unwrap();
        // Script exists and resolves, but the rubric calls no judge_files.
        fs::write(
            script_dir.join("loom.sh"),
            "#!/usr/bin/env bash\njudge_x() { judge_criterion \"no files examined\"; }\n",
        )
        .unwrap();

        let mut resolver = InputResolver::new(dir.path().to_path_buf());
        let a = ann(
            Tier::Judge,
            "../tests/judges/loom.sh#judge_x",
            "specs/harness.md",
        );
        assert!(
            resolver.declares_no_inputs(&a),
            "judge rubric calling no judge_files relies on the spec auto-include alone",
        );
    }

    #[test]
    fn judge_collect_mode_records_judge_files_paths() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("rubric.sh");
        // The rubric records two paths via judge_files; judge_criterion
        // carries a sentinel that must never leak into the inputs document,
        // proving the evaluation/LLM call is a no-op in collect mode.
        fs::write(
            &script,
            "#!/usr/bin/env bash\n\
             judge_x() {\n\
               judge_files \"crates/loom-llm/src/client.rs\" \"specs/llm.md\"\n\
               judge_criterion \"SENTINEL_CRITERION must never reach the inputs output\"\n\
             }\n",
        )
        .unwrap();

        let stdout =
            run_judge_collect(dir.path(), &script, Some("judge_x")).expect("collect mode runs");
        let paths = parse_inputs_json(&stdout).expect("single-target inputs document");
        assert_eq!(
            paths,
            vec![
                PathBuf::from("crates/loom-llm/src/client.rs"),
                PathBuf::from("specs/llm.md"),
            ],
            "judge_files arguments recorded in order: {stdout}",
        );
        assert!(
            !stdout.contains("SENTINEL_CRITERION"),
            "judge_criterion must be a no-op in collect mode: {stdout}",
        );
    }

    #[test]
    fn batch_print_inputs_maps_each_target_to_its_globs() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("rubric.sh");
        fs::write(
            &script,
            "#!/usr/bin/env bash\n\
             judge_a() { judge_files \"crates/a/src/**\"; judge_criterion \"a\"; }\n\
             judge_b() { judge_files \"crates/b/src/**\" \"specs/b.md\"; judge_criterion \"b\"; }\n",
        )
        .unwrap();

        let stdout = run_judge_collect(dir.path(), &script, None).expect("batch collect runs");
        let map = parse_inputs_batch_json(&stdout).expect("batch inputs document");
        assert_eq!(
            map.get("judge_a"),
            Some(&vec![PathBuf::from("crates/a/src/**")]),
            "judge_a mapped to its glob: {stdout}",
        );
        assert_eq!(
            map.get("judge_b"),
            Some(&vec![
                PathBuf::from("crates/b/src/**"),
                PathBuf::from("specs/b.md"),
            ]),
            "judge_b mapped to its globs: {stdout}",
        );
        assert_eq!(
            map.len(),
            2,
            "every rubric the script defines mapped in one spawn: {stdout}",
        );
    }

    #[test]
    fn judge_resolution_spawns_harness_once_per_script_via_cache() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("spawns.txt");
        fs::write(&counter, "0").unwrap();
        let script_dir = dir.path().join("tests/judges");
        fs::create_dir_all(&script_dir).unwrap();
        // Source-time code bumps a counter once per harness spawn; resolving
        // two rubrics of one script must batch into a single spawn.
        let counter_path = counter.display();
        fs::write(
            script_dir.join("loom.sh"),
            format!(
                "#!/usr/bin/env bash\n\
                 n=$(cat {counter_path}); echo $((n + 1)) > {counter_path}\n\
                 judge_a() {{ judge_files \"crates/a/src/**\"; }}\n\
                 judge_b() {{ judge_files \"crates/b/src/**\"; }}\n",
            ),
        )
        .unwrap();

        let mut resolver = InputResolver::new(dir.path().to_path_buf());
        let a = ann(Tier::Judge, "../tests/judges/loom.sh#judge_a", "specs/x.md");
        let b = ann(Tier::Judge, "../tests/judges/loom.sh#judge_b", "specs/x.md");
        let got_a = resolver.resolve(&a);
        let got_b = resolver.resolve(&b);
        assert!(
            got_a.paths.contains(&PathBuf::from("crates/a/src/**")),
            "first rubric resolved: {:?}",
            got_a.paths,
        );
        assert!(
            got_b.paths.contains(&PathBuf::from("crates/b/src/**")),
            "second rubric served from the batch cache: {:?}",
            got_b.paths,
        );
        assert_eq!(
            fs::read_to_string(&counter).unwrap().trim(),
            "1",
            "batch + per-session cache spawn the harness exactly once",
        );
    }

    #[test]
    fn spec_section_always_included_even_when_every_other_source_empty() {
        let mut resolver = InputResolver::new(PathBuf::from("/repo"));
        let a = ann(Tier::Check, "no-such-binary-anywhere", "specs/x.md");
        let got = resolver.resolve(&a);
        assert_eq!(got.paths, vec![PathBuf::from("specs/x.md")]);
    }

    #[test]
    fn parse_inputs_json_picks_last_inputs_object_in_stdout() {
        let stdout = "warning: ignored\n{\"inputs\": [\"a.rs\"]}\n";
        let got = parse_inputs_json(stdout).unwrap();
        assert_eq!(got, vec![PathBuf::from("a.rs")]);
    }

    #[test]
    fn parse_inputs_json_returns_none_when_stdout_has_no_inputs_object() {
        assert!(parse_inputs_json("warning only\nno JSON\n").is_none());
    }

    #[test]
    fn cargo_test_crate_name_handles_flag_variants() {
        let tok = |s: &str| -> Vec<String> { shlex::split(s).unwrap() };
        assert_eq!(
            cargo_test_crate_name(&tok("cargo test -p foo --lib bar")),
            Some("foo".into()),
        );
        assert_eq!(
            cargo_test_crate_name(&tok("cargo test --package bar")),
            Some("bar".into()),
        );
        assert_eq!(
            cargo_test_crate_name(&tok("cargo nextest run --package=qux")),
            Some("qux".into()),
        );
        assert_eq!(cargo_test_crate_name(&tok("cargo build")), None);
        assert_eq!(cargo_test_crate_name(&tok("rustc --version")), None);
    }

    #[test]
    fn inputs_for_test_override_replaces_test_scope_source() {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("inputs-helper.sh");
        fs::write(
            &helper,
            "#!/bin/sh\nprintf '{\"inputs\": [\"py/tests/test_x.py\"]}\\n'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut perms = fs::metadata(&helper).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&helper, perms).unwrap();
        }

        let scope = Box::new(StubScope::new(&[(
            "loom_gate",
            &["crates/loom-gate/src/lib.rs"],
        )]));
        let mut resolver = InputResolver::new(dir.path().to_path_buf())
            .with_test_scope(scope)
            .with_inputs_for_test_command(helper.display().to_string());

        let a = ann(Tier::Test, "loom_gate::tests::ok", "specs/gate.md");
        let got = resolver.resolve(&a);
        assert!(
            got.paths.contains(&PathBuf::from("py/tests/test_x.py")),
            "override result wins over TestScope: {:?}",
            got.paths,
        );
        assert!(
            !got.paths
                .contains(&PathBuf::from("crates/loom-gate/src/lib.rs")),
            "test scope must not fire when override succeeds: {:?}",
            got.paths,
        );
    }

    /// `looks_like_path` is a syntactic gate so unrelated tokens
    /// (regex patterns, flag values) don't get probed as files.
    #[test]
    fn looks_like_path_recognises_path_shaped_tokens() {
        assert!(looks_like_path("src/lib.rs"));
        assert!(looks_like_path("specs/x.md"));
        assert!(looks_like_path("crates/foo/src/main.rs"));
        assert!(!looks_like_path("happy_name"));
        assert!(!looks_like_path("X"));
        assert!(!looks_like_path("--lib"));
    }

    #[test]
    fn declares_no_inputs_true_when_every_source_empty() {
        let mut resolver = InputResolver::new(PathBuf::from("/repo"));
        // A bare command that is not a script, not a `--print-inputs`
        // binary, and yields no heuristic paths: declares nothing.
        let a = ann(Tier::Check, "no-such-binary-anywhere", "specs/x.md");
        assert!(
            resolver.declares_no_inputs(&a),
            "verifier with no resolvable inputs declares none",
        );
    }

    #[test]
    fn declares_no_inputs_false_when_a_source_yields_paths() {
        let scope = Box::new(StubScope::new(&[(
            "loom_gate",
            &["crates/loom-gate/src/lib.rs"],
        )]));
        let mut resolver = InputResolver::new(PathBuf::from("/repo")).with_test_scope(scope);
        let a = ann(Tier::Test, "loom_gate::module::ok", "specs/gate.md");
        assert!(
            !resolver.declares_no_inputs(&a),
            "the test-scope source yields the owning crate's sources",
        );
    }

    #[test]
    fn filter_by_files_empty_files_returns_all_annotations_unchanged() {
        let mut resolver = InputResolver::new(PathBuf::from("/repo"));
        let annotations = vec![
            ann(Tier::Check, "cargo run -p w", "specs/a.md"),
            ann(Tier::Test, "crate::a::ok", "specs/b.md"),
        ];
        let got = filter_by_files(&annotations, &[], &mut resolver);
        assert_eq!(got.len(), 2, "empty --files keeps every annotation");
    }

    #[test]
    fn filter_by_files_keeps_annotation_whose_spec_file_is_staged() {
        let scope = Box::new(StubScope::new(&[(
            "loom_gate",
            &["crates/loom-gate/src/lib.rs"],
        )]));
        let mut resolver = InputResolver::new(PathBuf::from("/repo")).with_test_scope(scope);
        // Declares an input (the owning crate's source, disjoint from the
        // staged set); kept only because the spec-section auto-include
        // intersects the staged file.
        let annotations = vec![ann(
            Tier::Test,
            "loom_gate::module::ok",
            "specs/pre-commit.md",
        )];
        let files = vec![PathBuf::from("specs/pre-commit.md")];
        let got = filter_by_files(&annotations, &files, &mut resolver);
        assert_eq!(
            got.len(),
            1,
            "spec-section auto-include means the annotation's own spec staged keeps it"
        );
    }

    #[test]
    fn filter_by_files_drops_annotation_when_inputs_disjoint_from_files() {
        let scope = Box::new(StubScope::new(&[(
            "loom_gate",
            &["crates/loom-gate/src/lib.rs"],
        )]));
        let mut resolver = InputResolver::new(PathBuf::from("/repo")).with_test_scope(scope);
        // Declares its owning crate's source plus the spec auto-include;
        // neither intersects the staged file, so it is dropped.
        let annotations = vec![ann(Tier::Test, "loom_gate::module::ok", "specs/gate.md")];
        let files = vec![PathBuf::from(".pre-commit-config.yaml")];
        let got = filter_by_files(&annotations, &files, &mut resolver);
        assert!(
            got.is_empty(),
            "declaring annotation with inputs disjoint from staged files is dropped"
        );
    }

    #[test]
    fn undeclared_verifier_always_runs_under_files_scope() {
        let mut resolver = InputResolver::new(PathBuf::from("/repo"));
        // Declares no inputs of its own (not a script, not a
        // `--print-inputs` binary, no heuristic path); its resolved set is
        // the spec-section auto-include alone, which the staged set does
        // not touch. The Conservative default retains it rather than
        // narrowing to the spec section.
        let annotations = vec![ann(Tier::Check, "no-such-binary-anywhere", "specs/gate.md")];
        let files = vec![PathBuf::from(".pre-commit-config.yaml")];
        let got = filter_by_files(&annotations, &files, &mut resolver);
        assert_eq!(
            got.len(),
            1,
            "an undeclared-input verifier always runs under a finite scope",
        );
    }

    #[test]
    fn filter_by_files_keeps_annotation_when_heuristic_finds_staged_file_as_command_arg() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".pre-commit-config.yaml");
        fs::write(&cfg, "").unwrap();
        let mut resolver = InputResolver::new(dir.path().to_path_buf());
        let annotations = vec![ann(
            Tier::Check,
            "grep -q 'verify-marker' .pre-commit-config.yaml",
            "specs/gate.md",
        )];
        let files = vec![PathBuf::from(".pre-commit-config.yaml")];
        let got = filter_by_files(&annotations, &files, &mut resolver);
        assert_eq!(
            got.len(),
            1,
            "heuristic should pull .pre-commit-config.yaml out of the grep command tokens",
        );
    }
}
