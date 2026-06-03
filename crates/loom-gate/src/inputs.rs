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
//! 2. **`[check]` / `[system]` / `[judge]` script header** — the first
//!    `~10` lines of the referenced script are scanned (literal
//!    string-search, language-agnostic) for a
//!    `# loom-inputs: <comma-separated globs>` line.
//! 3. **`[check]` / `[system]` binary `--print-inputs` protocol** — the
//!    first token is spawned with `--print-inputs` prepended to the
//!    remaining argv; stdout is parsed as `{"inputs": ["glob1", ...]}`.
//!    Results are cached per session keyed by the command string.
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
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use displaydoc::Display;
use serde::Deserialize;
use thiserror::Error;

use crate::annotation::{Annotation, Tier};
use crate::dispatch::TestScope;

/// Header tag the resolver searches for inside a script's first lines.
const LOOM_INPUTS_HEADER: &str = "# loom-inputs:";

/// How many lines from the top of a script are scanned for the header.
const SCRIPT_HEADER_LINE_BUDGET: usize = 10;

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

/// Stateful resolver — `--print-inputs` invocations are cached per
/// session so the same binary is not spawned twice for two annotations
/// that share a command prefix.
pub struct InputResolver {
    repo_root: PathBuf,
    test_scope: Option<Box<dyn TestScope>>,
    inputs_for_test_command: Option<String>,
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
            print_inputs_cache: HashMap::new(),
        }
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
    /// declaration source (test-framework metadata, script header,
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
    /// read the script's `# loom-inputs:` header (Source 2). No
    /// `--print-inputs` / heuristic fallback — a judge script with no
    /// header declares nothing of its own, so it falls through to the
    /// *Conservative default* (always runs) per [`filter_by_files`].
    fn declared_for_judge(&mut self, annotation: &Annotation) -> Vec<PathBuf> {
        crate::integrity::resolve_spec_relative_script_path(
            &annotation.target,
            &annotation.source_spec,
            &self.repo_root,
        )
        .and_then(|script| read_script_header(&script))
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

        if let Some(script_path) = self.script_in_repo(&tokens, &annotation.source_spec)
            && let Some(paths) = read_script_header(&script_path)
        {
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

    /// Find the first command token that names a script file on disk,
    /// resolved through the shared spec-relative helper the integrity gate
    /// uses for `[judge]` targets — selector-stripped and joined against
    /// the spec file's own directory — so the input resolver and integrity
    /// gate cannot disagree about where the script lives.
    fn script_in_repo(&self, tokens: &[String], source_spec: &Path) -> Option<PathBuf> {
        for tok in tokens {
            if let Some(resolved) = crate::integrity::resolve_spec_relative_script_path(
                tok,
                source_spec,
                &self.repo_root,
            ) && resolved.is_file()
            {
                return Some(resolved);
            }
        }
        None
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

fn read_script_header(path: &Path) -> Option<Vec<PathBuf>> {
    let body = fs::read_to_string(path).ok()?;
    for line in body.lines().take(SCRIPT_HEADER_LINE_BUDGET) {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(LOOM_INPUTS_HEADER) {
            let globs: Vec<PathBuf> = rest
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect();
            if globs.is_empty() {
                return None;
            }
            return Some(globs);
        }
    }
    None
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
    fn check_tier_reads_loom_inputs_header_from_script() {
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
        assert!(got.paths.contains(&PathBuf::from("src/walk/*.rs")));
        assert!(got.paths.contains(&PathBuf::from("src/lib.rs")));
        assert!(got.paths.contains(&PathBuf::from("specs/gate.md")));
    }

    #[test]
    fn script_header_ignored_past_line_budget() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("late.sh");
        let mut body = String::from("#!/bin/sh\n");
        for _ in 0..15 {
            body.push_str("# padding line\n");
        }
        body.push_str("# loom-inputs: never-seen.rs\n");
        fs::write(&script_path, body).unwrap();

        let mut resolver = InputResolver::new(dir.path().to_path_buf());
        let target = format!("sh {}", script_path.display());
        let a = ann(Tier::Check, &target, "specs/x.md");
        let got = resolver.resolve(&a);
        assert!(
            !got.paths.contains(&PathBuf::from("never-seen.rs")),
            "header past line budget must be ignored: {:?}",
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
        // protocol. The script-header source must skip it (the script
        // body has no `# loom-inputs:` line) so we fall through to
        // --print-inputs.
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
    fn judge_tier_strips_selector_and_reads_header_relative_to_spec_dir() {
        let dir = tempfile::tempdir().unwrap();
        // Judge target is spec-relative with a `#fn` selector; the script
        // lives at <repo>/tests/judges/loom.sh while the spec is under
        // <repo>/specs, so the `../` must resolve against the spec dir.
        let script_dir = dir.path().join("tests/judges");
        fs::create_dir_all(&script_dir).unwrap();
        fs::write(
            script_dir.join("loom.sh"),
            "#!/usr/bin/env bash\n# loom-inputs: crates/loom-llm/src/**, specs/harness.md\njudge_x() { :; }\n",
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
            "judge header glob resolved: {:?}",
            got.paths,
        );
        assert!(
            got.paths.contains(&PathBuf::from("specs/harness.md")),
            "judge header + spec auto-include: {:?}",
            got.paths,
        );
        assert!(
            !resolver.declares_no_inputs(&a),
            "a judge script with a header declares inputs",
        );
    }

    #[test]
    fn script_target_resolved_via_shared_spec_relative_helper() {
        let dir = tempfile::tempdir().unwrap();
        // [check] target IS a script-file path with a `#fn` selector and a
        // spec-relative `..`; resolution must strip the selector and join
        // against the spec dir (shared judge helper), not repo-root-join.
        let script_dir = dir.path().join("tests/checks");
        fs::create_dir_all(&script_dir).unwrap();
        fs::write(
            script_dir.join("walk.sh"),
            "#!/bin/sh\n# loom-inputs: crates/loom-walk/src/**, specs/walk.md\necho hi\n",
        )
        .unwrap();

        let mut resolver = InputResolver::new(dir.path().to_path_buf());
        let a = ann(
            Tier::Check,
            "../tests/checks/walk.sh#check_fn",
            "specs/harness.md",
        );
        let got = resolver.resolve(&a);
        assert!(
            got.paths
                .contains(&PathBuf::from("crates/loom-walk/src/**")),
            "check script header glob resolved spec-relative with selector stripped: {:?}",
            got.paths,
        );
        assert!(
            got.paths.contains(&PathBuf::from("specs/walk.md")),
            "header glob + spec auto-include: {:?}",
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
            "#!/usr/bin/env bash\n# loom-inputs: crates/loom-gate/src/lib.rs\n",
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
            "legacy `::fn` selector resolves the same script: {:?}",
            got.paths,
        );
    }

    #[test]
    fn judge_tier_without_header_declares_no_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let script_dir = dir.path().join("tests/judges");
        fs::create_dir_all(&script_dir).unwrap();
        // Script exists and resolves, but carries no `# loom-inputs:` line.
        fs::write(
            script_dir.join("loom.sh"),
            "#!/usr/bin/env bash\njudge_x() { :; }\n",
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
            "judge script with no header relies on the spec auto-include alone",
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
