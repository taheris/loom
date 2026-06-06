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
//!    is probed with `--print-inputs`.
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use displaydoc::Display;
use glob::Pattern;
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

/// Verdict of probing one annotation's input-query for the integrity
/// gate's inputs-protocol direction (`specs/gate.md` § Inputs-protocol
/// error). Enforcement is gated on an **explicit** opt-in, so only
/// [`Self::Errored`] is a finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputQueryProbe {
    /// loom does not own this verifier's input-query contract — an
    /// unregistered literal command, a matched runner with no `inputs`
    /// query, or a `[test]` annotation. The verifier falls through to the
    /// conservative always-run default; never a finding.
    NotOptedIn,
    /// An opted-in query ran and emitted a well-formed inputs document —
    /// possibly the deliberate-narrow empty `{"inputs":[]}`. Honoured
    /// as-is; never a finding.
    Honoured,
    /// An opted-in query exited non-zero or emitted a malformed inputs
    /// document — the loud `inputs-protocol-error`. `detail` records the
    /// failure mode for diagnostics.
    Errored { detail: String },
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
  local s="$1"
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
__loom_script="$1"
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

    /// Probe one annotation's input-query for the integrity gate's
    /// inputs-protocol direction. Enforcement is gated on an explicit
    /// opt-in (`specs/gate.md` § Inputs-protocol error): a `[judge]` (the
    /// harness preamble owns its collect mode) or a `[check]` / `[system]`
    /// whose matched runner declares an `inputs` query. Everything else —
    /// unregistered commands, runners without an `inputs` query, `[test]`
    /// annotations — resolves to [`InputQueryProbe::NotOptedIn`] and is
    /// never faulted. An opted-in query that fails to spawn, exits
    /// non-zero, or returns a malformed document is
    /// [`InputQueryProbe::Errored`].
    pub fn probe_input_query(&mut self, annotation: &Annotation) -> InputQueryProbe {
        match annotation.tier {
            Tier::Judge => self.probe_judge_query(annotation),
            Tier::Check | Tier::System => self.probe_command_query(annotation),
            Tier::Test => InputQueryProbe::NotOptedIn,
        }
    }

    /// A `[judge]` opts in unconditionally — the harness preamble owns
    /// `<script> --print-inputs`. A script that does not resolve to a file
    /// is the forward direction's [`UnresolvedAnnotation`], not ours, so it
    /// stays [`InputQueryProbe::NotOptedIn`] here to avoid a double finding.
    fn probe_judge_query(&self, annotation: &Annotation) -> InputQueryProbe {
        let Some(script) = crate::integrity::resolve_spec_relative_script_path(
            &annotation.target,
            &annotation.source_spec,
            &self.repo_root,
        ) else {
            return InputQueryProbe::NotOptedIn;
        };
        if !script.is_file() {
            return InputQueryProbe::NotOptedIn;
        }
        classify_query_run(
            run_query_capturing(judge_collect_command(&self.repo_root, &script, None)),
            &format!("loom-judge-harness {}", script.display()),
        )
    }

    /// A `[check]` / `[system]` opts in only when a runner `match`es the
    /// target *and* declares an `inputs` query. An unmatched target (a
    /// literal command loom never registered) or a matched runner with no
    /// `inputs` query stays [`InputQueryProbe::NotOptedIn`] — the gate
    /// never faults a bare `grep` / `nix` for declining a protocol it never
    /// opted into.
    fn probe_command_query(&self, annotation: &Annotation) -> InputQueryProbe {
        let query = {
            let (groups, _) = group_by_runner(&self.runners, std::slice::from_ref(annotation));
            let Some(group) = groups.into_iter().next() else {
                return InputQueryProbe::NotOptedIn;
            };
            match group.render_inputs_query() {
                Some(query) => query,
                None => return InputQueryProbe::NotOptedIn,
            }
        };
        let Some(command) = command_query_command(&self.repo_root, &query) else {
            return InputQueryProbe::Errored {
                detail: "input-query command could not be parsed".to_owned(),
            };
        };
        classify_query_run(run_query_capturing(command), &query)
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
    /// `tokens[0]` probe.
    ///
    /// This is the per-annotation path: it renders the query for one
    /// annotation and serves the primed cache without re-querying. The
    /// **group** spawn that satisfies the "discovery batches exactly where
    /// execution batches" invariant (`specs/gate.md` § Verifier inputs →
    /// Input-query protocol) — one query naming every sibling in a matched
    /// `[check]` group — is issued up front by
    /// [`Self::prime_runner_inputs`]; a sibling whose group was primed hits
    /// the cache here and never spawns.
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

    /// Prime the runner-owned input-query cache for `annotations` with
    /// **one query spawn per matched `[check]` group**, honouring the
    /// "discovery batches exactly where execution batches" invariant
    /// (`specs/gate.md` § Verifier inputs → Input-query protocol). The group
    /// query names every sibling target, so a runner whose `inputs` template
    /// filters by `{targets}` answers for the whole group in a single spawn;
    /// the per-target batch map primes the cache that [`Self::resolve`] then
    /// hits without re-querying.
    ///
    /// `[system]` annotations are excluded — per the spec carve-out their
    /// discovery stays per-annotation, matching their per-annotation
    /// execution. Groups already fully cached, runners without an `inputs`
    /// template, and unmatched annotations are skipped. Priming is
    /// best-effort: a failed or single-target response to a multi-sibling
    /// query leaves the group to the per-annotation path in
    /// [`Self::resolve`], preserving correctness at the per-annotation spawn
    /// cost.
    pub fn prime_runner_inputs(&mut self, annotations: &[Annotation]) {
        let check: Vec<Annotation> = annotations
            .iter()
            .filter(|a| a.tier == Tier::Check)
            .cloned()
            .collect();
        if check.is_empty() {
            return;
        }
        let plans: Vec<(String, String, Option<String>)> = {
            let (groups, _) = group_by_runner(&self.runners, &check);
            groups
                .into_iter()
                .filter_map(|group| {
                    let fully_cached = group.matched.iter().all(|m| {
                        self.print_inputs_cache
                            .contains_key(&runner_cache_key(&group.spec.name, &m.rendered_target))
                    });
                    if fully_cached {
                        return None;
                    }
                    let query = group.render_inputs_query()?;
                    let single = (group.matched.len() == 1)
                        .then(|| group.matched.first().map(|m| m.rendered_target.clone()))
                        .flatten();
                    Some((group.spec.name.clone(), query, single))
                })
                .collect()
        };
        for (runner_name, query, single) in plans {
            let Some(stdout) = run_command_query(&self.repo_root, &query) else {
                continue;
            };
            if let Some(batch) = parse_inputs_batch_json(&stdout) {
                for (target, paths) in batch {
                    self.print_inputs_cache
                        .insert(runner_cache_key(&runner_name, &target), paths);
                }
            } else if let Some(paths) = parse_inputs_json(&stdout)
                && let Some(target) = single
            {
                self.print_inputs_cache
                    .insert(runner_cache_key(&runner_name, &target), paths);
            }
        }
    }

    fn invoke_print_inputs(&self, tokens: &[String]) -> Option<Vec<PathBuf>> {
        let head = tokens.first()?;
        let tail = &tokens[1..];
        let mut cmd = Command::new(head);
        cmd.arg("--print-inputs").args(tail);
        cmd.current_dir(&self.repo_root);
        let output = match cmd.output() {
            Ok(output) => output,
            Err(source) => {
                let err = InputsError::Spawn {
                    command: head.clone(),
                    source,
                };
                tracing::warn!(err = ?err, "--print-inputs probe spawn failed; conservative always-run default applies");
                return None;
            }
        };
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
        let output = match cmd.output() {
            Ok(output) => output,
            Err(source) => {
                let err = InputsError::Spawn {
                    command: command.to_string(),
                    source,
                };
                tracing::warn!(err = ?err, "inputs_for_test helper spawn failed; conservative always-run default applies");
                return None;
            }
        };
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
    resolver.prime_runner_inputs(annotations);
    let repo_root = resolver.repo_root.clone();
    annotations
        .iter()
        .filter(|ann| {
            let (inputs, declared_own) = resolver.resolve_with_provenance(ann);
            !declared_own || inputs_intersect_files(&inputs.paths, files, &repo_root)
        })
        .cloned()
        .collect()
}

fn inputs_intersect_files(inputs: &[PathBuf], files: &[PathBuf], repo_root: &Path) -> bool {
    inputs.iter().any(|input| {
        files
            .iter()
            .any(|file| input_matches_file(input, file, repo_root))
    })
}

fn input_matches_file(input: &Path, file: &Path, repo_root: &Path) -> bool {
    let scoped = repo_relative_file(file, repo_root);
    input == scoped.as_path()
        || Pattern::new(&slash_path(input))
            .is_ok_and(|pattern| pattern.matches(&slash_path(&scoped)))
}

fn repo_relative_file(file: &Path, repo_root: &Path) -> PathBuf {
    if file.is_absolute()
        && let Ok(stripped) = file.strip_prefix(repo_root)
    {
        return stripped.to_path_buf();
    }
    file.to_path_buf()
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
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

/// Build the judge collect-mode `Command` under the loom judge-harness
/// preamble. With `function` set the harness emits the single-target
/// document (`{"inputs":[...]}`); with `None`, the batch map. Shared by the
/// silent resolve path ([`run_judge_collect`]) and the integrity gate's
/// inputs-protocol probe so both spawn byte-identical argv.
fn judge_collect_command(repo_root: &Path, script: &Path, function: Option<&str>) -> Command {
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
    cmd
}

/// Run a judge rubric script in collect mode under the loom judge-harness
/// preamble. With `function` set, returns the single-target document's
/// stdout (`{"inputs":[...]}`); with `None`, the batch map's stdout. Falls
/// through to `None` when the harness fails to spawn or exits non-zero, so
/// the resolver lands on the *Conservative default* rather than the gate
/// crashing over a malformed rubric.
fn run_judge_collect(repo_root: &Path, script: &Path, function: Option<&str>) -> Option<String> {
    let mut cmd = judge_collect_command(repo_root, script, function);
    let output = match cmd.output() {
        Ok(output) => output,
        Err(source) => {
            let err = InputsError::Spawn {
                command: format!("loom-judge-harness {}", script.display()),
                source,
            };
            tracing::warn!(err = ?err, "judge collect-mode spawn failed; conservative always-run default applies");
            return None;
        }
    };
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

/// Build the `Command` for a runner's rendered input-query string, or
/// `None` when the string has no leading token. Shared by the silent
/// resolve path ([`run_command_query`]) and the integrity gate's
/// inputs-protocol probe so both spawn byte-identical argv.
fn command_query_command(repo_root: &Path, command: &str) -> Option<Command> {
    let mut tokens = shlex::split(command)?.into_iter();
    let head = tokens.next()?;
    let tail: Vec<String> = tokens.collect();
    let mut cmd = Command::new(head);
    cmd.args(&tail).current_dir(repo_root);
    Some(cmd)
}

/// Spawn a runner's rendered input-query command in `repo_root` and return
/// its stdout, or `None` when the command fails to spawn or exits
/// non-zero. A non-zero exit falls through to the conservative always-run
/// default here; surfacing it as a loud `inputs-protocol-error` is the
/// integrity gate's job (see `specs/gate.md` § Inputs-protocol error).
fn run_command_query(repo_root: &Path, command: &str) -> Option<String> {
    let mut cmd = command_query_command(repo_root, command)?;
    let output = match cmd.output() {
        Ok(output) => output,
        Err(source) => {
            let err = InputsError::Spawn {
                command: command.to_string(),
                source,
            };
            tracing::warn!(err = ?err, "input-query spawn failed; conservative always-run default applies");
            return None;
        }
    };
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Outcome of spawning an opted-in input-query for the integrity gate's
/// inputs-protocol probe: the process ran (carrying its exit success and
/// captured stdout) or could not be spawned at all.
enum QueryRun {
    Ran { success: bool, stdout: String },
    SpawnFailed { source: std::io::Error },
}

/// Spawn `cmd`, capturing exit success and stdout. Unlike
/// [`run_command_query`] this does *not* collapse a non-zero exit into the
/// silent always-run default — the inputs-protocol probe needs the exit
/// status to tell a protocol error from a well-formed narrow.
fn run_query_capturing(mut cmd: Command) -> QueryRun {
    match cmd.output() {
        Ok(output) => QueryRun::Ran {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        },
        Err(source) => QueryRun::SpawnFailed { source },
    }
}

/// Map a captured input-query run to an [`InputQueryProbe`] verdict. A
/// spawn failure, a non-zero exit, or stdout that parses as neither the
/// single (`{"inputs":[...]}`) nor batch (`{"inputs":{...}}`) document is
/// the loud `inputs-protocol-error`. A well-formed document is honoured,
/// including the deliberate narrow `{"inputs":[]}` shape.
fn classify_query_run(run: QueryRun, command: &str) -> InputQueryProbe {
    match run {
        QueryRun::SpawnFailed { source } => {
            let err = InputsError::Spawn {
                command: command.to_string(),
                source,
            };
            tracing::warn!(err = ?err, "opted-in input-query spawn failed");
            InputQueryProbe::Errored {
                detail: "input-query failed to spawn".to_owned(),
            }
        }
        QueryRun::Ran { success: false, .. } => InputQueryProbe::Errored {
            detail: "input-query exited non-zero".to_owned(),
        },
        QueryRun::Ran {
            success: true,
            stdout,
        } => {
            if parse_inputs_batch_json(&stdout).is_some() || parse_inputs_json(&stdout).is_some() {
                InputQueryProbe::Honoured
            } else {
                InputQueryProbe::Errored {
                    detail: "input-query emitted a malformed inputs document".to_owned(),
                }
            }
        }
    }
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

    fn wait_for_executable(path: &Path) {
        for _ in 0..100 {
            if Command::new(path)
                .arg("--ready")
                .output()
                .is_ok_and(|out| out.status.success())
            {
                return;
            }
            std::thread::yield_now();
        }
        let ready = Command::new(path).arg("--ready").output();
        assert!(
            ready.is_ok_and(|out| out.status.success()),
            "helper executable did not become spawnable",
        );
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
            self.map
                .get(crate_key(&a.target))
                .cloned()
                .unwrap_or_default()
        }
    }

    fn crate_key(target: &str) -> &str {
        target.split("::").next().unwrap_or("")
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
        // The in-script `# loom-inputs:` header is inert; inputs come only by execution.
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
                "#!/bin/sh\nif [ \"$1\" = \"--ready\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"--print-inputs\" ]; then\n  n=$(cat \"{counter_path}\")\n  echo $((n + 1)) > \"{counter_path}\"\n  printf '{{\"inputs\": [\"x.rs\"]}}\\n'\n  exit 0\nfi\nexit 99\n",
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
        wait_for_executable(&helper);

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
        // Responder errors if `--print-inputs` lands as its first arg (the argv-head bug); answers only when the flag follows the walk name.
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
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        // The responder answers `--print-inputs` when probed as a bare literal command.
        let responder = dir.path().join("responder.sh");
        fs::write(
            &responder,
            "#!/bin/sh\n\
             if [ \"$1\" = \"--print-inputs\" ]; then\n\
               printf '{\"inputs\": [\"crates/loom-walk/src/probed.rs\"]}\\n'\n\
               exit 0\n\
             fi\n\
             exit 1\n",
        )
        .unwrap();
        fs::set_permissions(&responder, fs::Permissions::from_mode(0o755)).unwrap();

        let head = responder.display().to_string();
        let target = format!("{head} foo");
        let pattern = format!("^{} (\\S+)$", regex::escape(&head));
        let a = ann(Tier::Check, &target, "specs/gate.md");

        // Control: with no runner, the bare command IS probed — tokens[0]
        // answers `--print-inputs`, so the verifier declares its own inputs.
        let mut bare = InputResolver::new(dir.path().to_path_buf());
        assert!(
            !bare.declares_no_inputs(&a),
            "control: the command head answers --print-inputs when probed literally",
        );

        // With the matched runner (no inputs query), the probe is suppressed:
        // the runner owns the annotation and yields the conservative
        // always-run default even though the head would answer if probed.
        let spec = RunnerSpec::compile(
            "walk",
            Some(pattern.as_str()),
            "{targets}",
            "{capture_1}",
            " ",
            crate::runner::BuiltinParser::JsonLines,
            None,
        )
        .unwrap();
        let mut resolver = InputResolver::new(dir.path().to_path_buf()).with_runners(vec![spec]);
        assert!(
            resolver.declares_no_inputs(&a),
            "matched runner with no inputs query suppresses the tokens[0] probe \
             and relies on the always-run default",
        );
    }

    /// The runner input-query seam (`render_inputs_query` →
    /// `run_command_query` → `parse_inputs_batch_json` cache-prime) batches
    /// across siblings: a batch response (`{"inputs":{"<target>":[...]}}`)
    /// primes the cache for every sibling in the matched group, so two
    /// `[check]` siblings sharing one runner spawn the query once — the
    /// "discovery batches where execution batches" contract from
    /// `specs/gate.md` § Runners. A counting responder pins the spawn count.
    #[test]
    fn runner_input_query_batch_response_primes_cache_for_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("queries.txt");
        fs::write(&counter, "0").unwrap();
        let counter_path = counter.display();
        let responder = dir.path().join("batch-responder.sh");
        fs::write(
            &responder,
            format!(
                "#!/bin/sh\n\
                 n=$(cat \"{counter_path}\"); echo $((n + 1)) > \"{counter_path}\"\n\
                 printf '{{\"inputs\":{{\"alpha\":[\"crates/a/src/x.rs\"],\"beta\":[\"crates/b/src/y.rs\"]}}}}\\n'\n",
            ),
        )
        .unwrap();

        let spec = RunnerSpec::compile(
            "walk",
            Some(r"^walk -- (\S+)$"),
            "walk -- {targets}",
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
        let alpha = ann(Tier::Check, "walk -- alpha", "specs/gate.md");
        let beta = ann(Tier::Check, "walk -- beta", "specs/gate.md");

        let got_alpha = resolver.resolve(&alpha);
        let got_beta = resolver.resolve(&beta);

        assert!(
            got_alpha
                .paths
                .contains(&PathBuf::from("crates/a/src/x.rs")),
            "alpha resolves its own glob from the batch response: {:?}",
            got_alpha.paths,
        );
        assert!(
            got_beta.paths.contains(&PathBuf::from("crates/b/src/y.rs")),
            "beta is served from the sibling cache-prime, not a second query: {:?}",
            got_beta.paths,
        );
        assert_eq!(
            fs::read_to_string(&counter).unwrap().trim(),
            "1",
            "one input-query spawn covers the matched group; the sibling hits the primed cache",
        );
    }

    /// A realistic runner `inputs` responder answers only for the
    /// `{targets}` on its argv — it does NOT volunteer globs for targets it
    /// was never asked about. [`InputResolver::prime_runner_inputs`] still
    /// resolves a two-sibling `[check]` group in a single query spawn
    /// because the group query names every sibling, so the responder answers
    /// for both at once. This pins the "discovery batches exactly where
    /// execution batches" invariant (`specs/gate.md` § Verifier inputs →
    /// Input-query protocol) against the filtering responder that
    /// [`runner_input_query_batch_response_primes_cache_for_siblings`]'s
    /// volunteer-the-whole-map responder cannot distinguish.
    #[test]
    fn prime_runner_inputs_batches_group_query_for_filtering_responder() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("queries.txt");
        fs::write(&counter, "0").unwrap();
        let counter_path = counter.display();
        let responder = dir.path().join("filter-responder.sh");
        fs::write(
            &responder,
            format!(
                "#!/bin/sh\n\
                 n=$(cat \"{counter_path}\"); echo $((n + 1)) > \"{counter_path}\"\n\
                 printf '{{\"inputs\":{{'\n\
                 first=1\n\
                 for arg in \"$@\"; do\n\
                   [ \"$arg\" = \"--print-inputs\" ] && continue\n\
                   [ \"$first\" = 1 ] || printf ','\n\
                   printf '\"%s\":[\"crates/%s/src.rs\"]' \"$arg\" \"$arg\"\n\
                   first=0\n\
                 done\n\
                 printf '}}}}\\n'\n",
            ),
        )
        .unwrap();

        let spec = RunnerSpec::compile(
            "walk",
            Some(r"^walk -- (\S+)$"),
            "walk -- {targets}",
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
        let alpha = ann(Tier::Check, "walk -- alpha", "specs/gate.md");
        let beta = ann(Tier::Check, "walk -- beta", "specs/gate.md");
        let annotations = vec![alpha.clone(), beta.clone()];

        resolver.prime_runner_inputs(&annotations);

        let got_alpha = resolver.resolve(&alpha);
        let got_beta = resolver.resolve(&beta);

        assert!(
            got_alpha
                .paths
                .contains(&PathBuf::from("crates/alpha/src.rs")),
            "alpha resolves from the primed group query: {:?}",
            got_alpha.paths,
        );
        assert!(
            got_beta
                .paths
                .contains(&PathBuf::from("crates/beta/src.rs")),
            "beta resolves from the primed group query, not a second spawn: {:?}",
            got_beta.paths,
        );
        assert_eq!(
            fs::read_to_string(&counter).unwrap().trim(),
            "1",
            "one group query spawn covers both siblings even though the \
             responder answers only for the targets named on its argv",
        );
    }

    /// `[system]` discovery stays per-annotation per the spec carve-out:
    /// [`InputResolver::prime_runner_inputs`] must not batch a `[system]`
    /// group even when a matching runner declares an `inputs` template. The
    /// counting responder would fire once if the system group were primed;
    /// it stays at zero because priming skips the `[system]` tier.
    #[test]
    fn prime_runner_inputs_excludes_system_tier() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("queries.txt");
        fs::write(&counter, "0").unwrap();
        let counter_path = counter.display();
        let responder = dir.path().join("sys-count-responder.sh");
        fs::write(
            &responder,
            format!(
                "#!/bin/sh\n\
                 n=$(cat \"{counter_path}\"); echo $((n + 1)) > \"{counter_path}\"\n\
                 printf '{{\"inputs\":[\"crates/loom-sys/src/probe.rs\"]}}\\n'\n",
            ),
        )
        .unwrap();

        let spec = RunnerSpec::compile(
            "nix",
            Some(r"^nix run \.#(\S+)$"),
            "nix run .#{targets}",
            "{capture_1}",
            " ",
            crate::runner::BuiltinParser::NixBuildStatus,
            None,
        )
        .unwrap()
        .with_inputs(Some(format!(
            "sh {} {{targets}} {{print_inputs}}",
            responder.display()
        )));

        let mut resolver = InputResolver::new(dir.path().to_path_buf()).with_runners(vec![spec]);
        let a = ann(Tier::System, "nix run .#test-loom", "specs/gate.md");

        resolver.prime_runner_inputs(std::slice::from_ref(&a));

        assert_eq!(
            fs::read_to_string(&counter).unwrap().trim(),
            "0",
            "[system] discovery stays per-annotation; the prime pass skips it",
        );
    }

    /// `[system]` input-query is runner-owned exactly as `[check]` is: a
    /// matched `[runner.system.<name>]` routes the query through the
    /// runner's `inputs` template (flag after the target), never a
    /// `tokens[0]` probe. Per `specs/gate.md` § Runners this is the
    /// load-bearing `[system]` carve-out — inputs are runner-owned even
    /// though execution stays per-annotation.
    #[test]
    fn system_tier_input_query_routed_through_runner_template() {
        let dir = tempfile::tempdir().unwrap();
        // Responder errors if `--print-inputs` lands as its first arg (the
        // argv-head bug); answers only when the flag follows the target.
        let responder = dir.path().join("sys-responder.sh");
        fs::write(
            &responder,
            "#!/bin/sh\n\
             target=$1\n\
             if [ \"$target\" = \"--print-inputs\" ]; then\n\
               echo 'error: --print-inputs is not a target' >&2\n\
               exit 2\n\
             fi\n\
             shift\n\
             for arg in \"$@\"; do\n\
               if [ \"$arg\" = \"--print-inputs\" ]; then\n\
                 printf '{\"inputs\": [\"crates/loom-sys/src/probe.rs\"]}\\n'\n\
                 exit 0\n\
               fi\n\
             done\n\
             exit 1\n",
        )
        .unwrap();

        let spec = RunnerSpec::compile(
            "nix",
            Some(r"^nix run \.#(\S+)$"),
            "nix run .#{targets}",
            "{capture_1}",
            " ",
            crate::runner::BuiltinParser::NixBuildStatus,
            None,
        )
        .unwrap()
        .with_inputs(Some(format!(
            "sh {} {{targets}} {{print_inputs}}",
            responder.display()
        )));

        let mut resolver = InputResolver::new(dir.path().to_path_buf()).with_runners(vec![spec]);
        let a = ann(Tier::System, "nix run .#test-loom", "specs/gate.md");
        let got = resolver.resolve(&a);
        assert!(
            got.paths
                .contains(&PathBuf::from("crates/loom-sys/src/probe.rs")),
            "[system] input-query routed through the runner template (flag after \
             the target), not prepended to tokens[0]: {:?}",
            got.paths,
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

    /// End-to-end runner-owned scope filtering: a matched `[check]`
    /// group's queried inputs decide `--files` inclusion. [`filter_by_files`]
    /// primes the group in one query spawn, then keeps the sibling whose
    /// queried glob is staged and drops the sibling whose queried glob is
    /// not — the production seam (`with_runners` → `filter_by_files`) every
    /// other runner test sidesteps by calling `resolve` directly.
    #[test]
    fn filter_by_files_keeps_runner_matched_check_sibling_whose_queried_input_is_staged() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("queries.txt");
        fs::write(&counter, "0").unwrap();
        let counter_path = counter.display();
        let responder = dir.path().join("scope-responder.sh");
        fs::write(
            &responder,
            format!(
                "#!/usr/bin/env bash\n\
                 set -euo pipefail\n\
                 n=$(< \"{counter_path}\")\n\
                 printf '%s\\n' \"$((n + 1))\" > \"{counter_path}\"\n\
                 printf '{{\"inputs\":{{'\n\
                 first=1\n\
                 for arg in \"$@\"; do\n\
                   [[ \"$arg\" == \"--print-inputs\" ]] && continue\n\
                   if [[ \"$first\" -eq 1 ]]; then\n\
                     first=0\n\
                   else\n\
                     printf ','\n\
                   fi\n\
                   printf '\"%s\":[\"crates/%s/src/*.rs\"]' \"$arg\" \"$arg\"\n\
                 done\n\
                 printf '}}}}\\n'\n",
            ),
        )
        .unwrap();

        let spec = RunnerSpec::compile(
            "walk",
            Some(r"^walk -- (\S+)$"),
            "walk -- {targets}",
            "{capture_1}",
            " ",
            crate::runner::BuiltinParser::JsonLines,
            None,
        )
        .unwrap()
        .with_inputs(Some(format!(
            "bash {} {{targets}} {{print_inputs}}",
            responder.display()
        )));

        let mut resolver = InputResolver::new(dir.path().to_path_buf()).with_runners(vec![spec]);
        let alpha = ann(Tier::Check, "walk -- alpha", "specs/gate.md");
        let beta = ann(Tier::Check, "walk -- beta", "specs/gate.md");
        let annotations = vec![alpha, beta];

        let files = vec![dir.path().join("crates/alpha/src/lib.rs")];
        let got = filter_by_files(&annotations, &files, &mut resolver);

        assert_eq!(
            got.iter().map(|a| a.target.as_str()).collect::<Vec<_>>(),
            vec!["walk -- alpha"],
            "only the runner-matched sibling whose queried glob matches the absolute scoped file survives",
        );
        assert_eq!(
            fs::read_to_string(&counter).unwrap().trim(),
            "1",
            "one group query spawn primes both siblings for the scope decision",
        );
    }

    /// `[system]` discovery is per-annotation (excluded from group priming),
    /// yet a matched `[system]` runner's queried inputs still decide
    /// `--files` inclusion: the per-annotation runner-owned query during
    /// resolution feeds the scope filter, keeping the verifier whose queried
    /// glob is staged and dropping the one whose glob is not.
    #[test]
    fn filter_by_files_keeps_runner_matched_system_verifier_whose_queried_input_is_staged() {
        let dir = tempfile::tempdir().unwrap();
        // Responder answers only when --print-inputs follows the target,
        // echoing a per-target glob keyed by the queried target name.
        let responder = dir.path().join("sys-scope-responder.sh");
        fs::write(
            &responder,
            "#!/bin/sh\n\
             target=$1\n\
             shift\n\
             for arg in \"$@\"; do\n\
               if [ \"$arg\" = \"--print-inputs\" ]; then\n\
                 printf '{\"inputs\": [\"crates/%s/sys.rs\"]}\\n' \"$target\"\n\
                 exit 0\n\
               fi\n\
             done\n\
             exit 1\n",
        )
        .unwrap();

        let spec = RunnerSpec::compile(
            "nix",
            Some(r"^nix run \.#(\S+)$"),
            "nix run .#{targets}",
            "{capture_1}",
            " ",
            crate::runner::BuiltinParser::NixBuildStatus,
            None,
        )
        .unwrap()
        .with_inputs(Some(format!(
            "sh {} {{targets}} {{print_inputs}}",
            responder.display()
        )));

        let mut resolver = InputResolver::new(dir.path().to_path_buf()).with_runners(vec![spec]);
        let staged = ann(Tier::System, "nix run .#alpha", "specs/gate.md");
        let unstaged = ann(Tier::System, "nix run .#beta", "specs/gate.md");
        let annotations = vec![staged, unstaged];

        let files = vec![PathBuf::from("crates/alpha/sys.rs")];
        let got = filter_by_files(&annotations, &files, &mut resolver);

        assert_eq!(
            got.iter().map(|a| a.target.as_str()).collect::<Vec<_>>(),
            vec!["nix run .#alpha"],
            "the runner-matched [system] verifier whose queried glob is staged survives; \
             the disjoint sibling drops",
        );
    }
}
