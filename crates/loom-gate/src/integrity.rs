//! Annotation integrity gate.
//!
//! Runs as part of `loom gate check`. Two directions per
//! `specs/gate.md`:
//!
//! 1. **Forward** — every annotation's target is valid for its tier:
//!    `[check](cmd)` and `[system](cmd)` resolve via a runner that
//!    `match`es the target (the runner owns it), falling back to the
//!    first token resolving on PATH or as a file in the repo only when no
//!    runner matches; `[test](path)` resolves to a `#[test]` /
//!    `#[tokio::test]` / `proptest!` function in the workspace;
//!    `[judge](path)` resolves to a file on disk.
//! 2. **Atomic acceptance** — each criterion carries exactly one
//!    annotation. N→1 sharing (multiple criteria pointing at the same
//!    verifier) is allowed.
//!
//! Findings render in the form prescribed by the spec:
//! `<spec>:<line>: annotation [tier](<target>) — does not resolve` or
//! `<spec>:<line>: criterion carries N annotations, expected 1`.
//!
//! A third direction — **stub-pointing annotations** — flags any
//! annotation whose target Rust test function body invokes the
//! `_pending_stub` sigil. Applies to `[test]` annotations and to
//! `[check](cargo test ... <name>)` annotations that embed a test name.
//! Under the verifier-driven-status invariant (`docs/spec-conventions.md`)
//! a stubbed verifier means the criterion has no real evidence; the
//! deterministic gate flags it without waiting for `loom gate review`'s
//! verifier-honesty rubric.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use displaydoc::Display;
use thiserror::Error;
use walkdir::WalkDir;

use loom_events::identifier::SpecLabel;
use loom_protocol::gate::{ConcernToken, Finding, FindingTarget};

use crate::annotation::{Annotation, Tier};
use crate::dispatch::{DispatchOptions, TierCwds, run_with_runners};
use crate::runner::RunnerSpec;

/// One finding surfaced by the integrity gate.
///
/// Variants line up with the directions the gate enforces: forward
/// resolution (annotation target invalid for its tier), embedded
/// cargo-test-name resolution (`[check](cargo test ... <name>)`'s test
/// name missing), and atomic acceptance (criterion carries more than one
/// annotation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityFinding {
    /// Annotation's target does not resolve for its tier.
    UnresolvedAnnotation {
        spec: PathBuf,
        line: u32,
        tier: Tier,
        target: String,
    },
    /// `[check](cargo test ... <name>)` annotation's first token resolves
    /// but `<name>` does not match any `#[test]` / `#[tokio::test]` /
    /// `proptest!` function in the workspace. Caught separately so the
    /// message can name the unresolved test rather than the whole command.
    UnresolvedCargoTestName {
        spec: PathBuf,
        line: u32,
        target: String,
        test_name: String,
    },
    /// Criterion carries more than one annotation; atomic-acceptance
    /// violated. `count` is the number of annotations attached.
    MultipleAnnotations {
        spec: PathBuf,
        line: u32,
        count: usize,
    },
    /// Annotation's target Rust test function body invokes the
    /// `_pending_stub` sigil. The annotation resolves (so it is not an
    /// [`Self::UnresolvedAnnotation`]) but the verifier it points at is
    /// a placeholder that produces no real evidence. Applies to `[test]`
    /// annotations and to `[check](cargo test ... <name>)` annotations
    /// whose embedded test name resolves.
    StubTestFunction {
        spec: PathBuf,
        line: u32,
        tier: Tier,
        target: String,
        test_name: String,
    },
    /// Annotation carries the `?` pending modifier but its target now
    /// resolves (for `[test?]`, also has a non-stub body). The marker is
    /// stale — the implementer landed the verifier but did not drop the
    /// `?` in the same diff. Self-cleaning semantics per
    /// `specs/gate.md` § Pending modifier: the marker itself becomes the
    /// finding the moment the condition it suppressed resolves.
    UnneededPendingMarker {
        spec: PathBuf,
        line: u32,
        tier: Tier,
        target: String,
    },
}

impl IntegrityFinding {
    /// True iff this finding is terminal at the push gate per
    /// `specs/gate.md` § Integrity gate. [`Self::UnresolvedAnnotation`],
    /// [`Self::StubTestFunction`], and [`Self::UnneededPendingMarker`]
    /// refuse the push and raise `loom:clarify`. The remaining variants
    /// surface as warnings elsewhere.
    pub fn is_push_gate_terminal(&self) -> bool {
        matches!(
            self,
            Self::UnresolvedAnnotation { .. }
                | Self::StubTestFunction { .. }
                | Self::UnneededPendingMarker { .. }
        )
    }

    /// Normalize a push-gate-terminal finding into the typed
    /// [`Finding`] the mint pipeline consumes, per `specs/gate.md`
    /// § *Concern tokens and target variants* (integrity-gate rows). The
    /// token follows that table (`unresolved-annotation` / `stub-pointing`
    /// / `unneeded-pending-marker`); the target is always
    /// [`FindingTarget::Annotation`]; `bonds` carries the lead spec label
    /// derived from the finding's spec-file stem; `evidence` is the
    /// finding's own [`std::fmt::Display`]. Non-terminal variants
    /// ([`Self::is_push_gate_terminal`] false) and findings whose spec
    /// path has no file stem return `None`.
    #[must_use]
    pub fn to_finding(&self) -> Option<Finding> {
        let (token, spec, target) = match self {
            Self::UnresolvedAnnotation { spec, target, .. } => {
                (ConcernToken::UnresolvedAnnotation, spec, target)
            }
            Self::StubTestFunction { spec, target, .. } => {
                (ConcernToken::StubPointing, spec, target)
            }
            Self::UnneededPendingMarker { spec, target, .. } => {
                (ConcernToken::UnneededPendingMarker, spec, target)
            }
            Self::UnresolvedCargoTestName { .. } | Self::MultipleAnnotations { .. } => {
                return None;
            }
        };
        let label = spec.file_stem().and_then(|s| s.to_str())?;
        Some(Finding {
            token,
            bonds: vec![SpecLabel::new(label)],
            target: FindingTarget::Annotation {
                target_string: target.clone(),
            },
            evidence: self.to_string(),
        })
    }
}

impl std::fmt::Display for IntegrityFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnresolvedAnnotation {
                spec,
                line,
                tier,
                target,
            } => write!(
                f,
                "{}:{}: annotation [{}]({}) — does not resolve",
                spec.display(),
                line,
                tier,
                target
            ),
            Self::UnresolvedCargoTestName {
                spec,
                line,
                target,
                test_name,
            } => write!(
                f,
                "{}:{}: annotation [check]({}) — cargo test name `{}` does not resolve",
                spec.display(),
                line,
                target,
                test_name
            ),
            Self::MultipleAnnotations { spec, line, count } => write!(
                f,
                "{}:{}: criterion carries {} annotations, expected 1",
                spec.display(),
                line,
                count
            ),
            Self::StubTestFunction {
                spec,
                line,
                tier,
                target,
                test_name,
            } => write!(
                f,
                "{}:{}: annotation [{}]({}) — test function `{}` calls _pending_stub",
                spec.display(),
                line,
                tier,
                target,
                test_name
            ),
            Self::UnneededPendingMarker {
                spec,
                line,
                tier,
                target,
            } => write!(
                f,
                "{}:{}: annotation [{}?]({}) is now resolved — drop the ? marker",
                spec.display(),
                line,
                tier,
                target
            ),
        }
    }
}

/// Errors surfaced from running the integrity gate. Resolution itself
/// returns boolean verdicts via the resolver traits; this enum is reserved
/// for failures wiring those resolvers up.
#[derive(Debug, Display, Error)]
pub enum IntegrityError {
    /// failed to walk workspace under `{root}`: {source}
    WalkWorkspace {
        root: PathBuf,
        #[source]
        source: walkdir::Error,
    },
}

/// Resolver for `[check]` / `[system]` annotation targets.
///
/// The integrity gate decides whether the first token of the annotation
/// command resolves; the trait abstracts how that lookup happens so tests
/// can swap in deterministic fixtures.
pub trait CommandResolver {
    /// True iff `first_token` resolves on the consumer's PATH or as a
    /// file in the consumer's repo.
    fn resolves(&self, first_token: &str) -> bool;
}

/// Resolver for `[test]` annotation targets.
///
/// Production implementations walk the consumer's workspace (cargo
/// metadata + source files); tests substitute a deterministic membership
/// check.
pub trait TestPathResolver {
    /// True iff `target` names a real test function in the workspace.
    fn resolves(&self, target: &str) -> bool;
}

/// Scanner answering whether a Rust test function is a stub.
///
/// A test function is a stub when its body invokes the `_pending_stub`
/// sigil. The integrity gate calls into this trait once an annotation's
/// target has resolved, so the membership check covers only real
/// functions. Production implementations walk source files; tests
/// substitute a deterministic membership check.
pub trait StubScanner {
    /// True iff the test function with leaf name `leaf` calls
    /// `_pending_stub` in its body.
    fn is_stub(&self, leaf: &str) -> bool;
}

/// Forward-resolver for `[check?]` / `[system?]` pending annotations.
///
/// Per `specs/gate.md` § Pending modifier the integrity gate's
/// forward-resolution check runs the annotation's command in the same
/// dispatch environment as the non-pending form and inspects the exit
/// code: exit 0 → assertion holds → emit
/// [`IntegrityFinding::UnneededPendingMarker`]; non-zero exit (or any
/// spawn failure) → silent pass (still pending). This broader check is
/// what makes the `?` modifier honor author intent uniformly across
/// binary-pending and assertion-pending — both fail-modes produce
/// non-zero exit, both silent-pass under the modifier.
///
/// Production wires this through the same dispatcher path the
/// non-pending `[check]` / `[system]` form uses ([`DispatchPendingExecutor`]),
/// so the integrity gate can never advise "drop the `?`" on an
/// annotation that the verify-lane `[check]` would reject. Tests
/// substitute a deterministic stub.
///
/// The full [`Annotation`] is passed rather than just the command
/// string so impls can route per tier (Check uses the runner-spec
/// dispatcher; System falls back to per-annotation spawn).
pub trait PendingCommandExecutor {
    /// True iff `annotation.target` runs to completion in the dispatch
    /// environment and exits with status 0. Spawn failures and
    /// non-zero exits return false.
    fn executes_zero(&self, annotation: &Annotation) -> bool;
}

/// Filesystem-backed implementation of [`CommandResolver`].
///
/// Resolution order matches the spec's "first token resolves on PATH or
/// as a file in the repo (best-effort)" wording: absolute paths and paths
/// rooted under the repo are checked first, then the `PATH` environment
/// is walked. The `PATH` lookup is snapshot-on-construction so tests can
/// pin a deterministic value via [`FsCommandResolver::with_path`].
pub struct FsCommandResolver {
    repo_root: PathBuf,
    path_entries: Vec<PathBuf>,
}

impl FsCommandResolver {
    /// Construct a resolver rooted at `repo_root` using the process's
    /// current `PATH` environment variable.
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        let path_env = std::env::var_os("PATH");
        let path_entries = path_env
            .as_deref()
            .map(|p| std::env::split_paths(p).collect::<Vec<_>>())
            .unwrap_or_default();
        Self {
            repo_root: repo_root.into(),
            path_entries,
        }
    }

    /// Construct a resolver with an explicit `PATH` value; used by tests
    /// to keep results independent of the host environment.
    pub fn with_path(repo_root: impl Into<PathBuf>, path: &str) -> Self {
        let path_entries = std::env::split_paths(path).collect();
        Self {
            repo_root: repo_root.into(),
            path_entries,
        }
    }
}

impl CommandResolver for FsCommandResolver {
    fn resolves(&self, first_token: &str) -> bool {
        if first_token.is_empty() {
            return false;
        }
        let candidate = Path::new(first_token);
        if candidate.is_absolute() {
            return candidate.exists();
        }
        if first_token.contains('/') {
            let joined = self.repo_root.join(candidate);
            if joined.exists() {
                return true;
            }
        }
        let direct = self.repo_root.join(first_token);
        if direct.exists() {
            return true;
        }
        self.path_entries
            .iter()
            .any(|dir| dir.join(first_token).exists())
    }
}

/// Dispatcher-backed implementation of [`PendingCommandExecutor`].
///
/// Routes the pending annotation through the same code path the
/// non-pending tier would use at runtime: [`run_with_runners`] for
/// `[check?]`, which honours runner-spec batching and `tier_cwds`;
/// `[system?]` flows through the same call (system annotations
/// fall through to per-annotation spawn since the runner-spec
/// matcher targets the check tier). Using one dispatcher path
/// eliminates the failure mode where the integrity gate's verdict
/// disagrees with what the verify lane would compute on the same
/// predicate.
///
/// Exit 0 → `UnneededPendingMarker`; non-zero exit, spawn failure,
/// or dispatch error → silent pass (still pending).
pub struct DispatchPendingExecutor<'a> {
    specs: &'a [RunnerSpec],
    options: DispatchOptions,
    repo_root: PathBuf,
    tier_cwds: TierCwds,
}

impl<'a> DispatchPendingExecutor<'a> {
    /// Construct an executor that mirrors the dispatcher context
    /// `run_check` / `run_system` would receive at verify time.
    pub fn new(
        specs: &'a [RunnerSpec],
        options: DispatchOptions,
        repo_root: impl Into<PathBuf>,
        tier_cwds: TierCwds,
    ) -> Self {
        Self {
            specs,
            options,
            repo_root: repo_root.into(),
            tier_cwds,
        }
    }
}

impl PendingCommandExecutor for DispatchPendingExecutor<'_> {
    fn executes_zero(&self, annotation: &Annotation) -> bool {
        let mut ann = annotation.clone();
        ann.pending = false;
        let results = run_with_runners(
            std::slice::from_ref(&ann),
            self.specs,
            &self.options,
            &self.repo_root,
            &self.tier_cwds,
        );
        matches!(
            results.into_iter().next(),
            Some(Ok(outcome)) if outcome.verdict.pass
        )
    }
}

/// Component check for `target/` and `.loom/` skip, relative to the
/// scan root. Anchoring on the relative path keeps the skip correct
/// when the workspace itself lives under a directory named `.loom/`
/// (e.g. `loom gate verify` run from inside `.loom/beads/<id>/`): only
/// children of the scan root should be excluded, not parent components
/// of the absolute prefix.
fn is_excluded_under_root(path: &Path, repo_root: &Path) -> bool {
    let rel = path.strip_prefix(repo_root).unwrap_or(path);
    rel.components()
        .any(|c| matches!(c.as_os_str().to_str(), Some("target" | ".loom")))
}

/// Walk `repo_root` once and produce both the test-leaf index and the
/// stub-leaf index in a single pass — every `.rs` file is read exactly
/// once and both extractors run on the same string. Callers that need
/// both resolvers (the integrity gate, `loom gate review`, the mint
/// walker) should use this instead of calling each `::scan` separately,
/// since back-to-back scans walk the same 300+ workspace files twice
/// and re-read every byte.
pub fn scan_workspace_pair(
    repo_root: &Path,
) -> Result<(RustWorkspaceTestResolver, RustWorkspaceStubScanner), IntegrityError> {
    let mut known_leaves: HashSet<String> = HashSet::new();
    let mut stub_leaves: HashSet<String> = HashSet::new();
    for entry in WalkDir::new(repo_root).follow_links(false) {
        let entry = entry.map_err(|e| IntegrityError::WalkWorkspace {
            root: repo_root.to_path_buf(),
            source: e,
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        if is_excluded_under_root(path, repo_root) {
            continue;
        }
        let Ok(body) = fs::read_to_string(path) else {
            continue;
        };
        extract_test_fn_leaves(&body, &mut known_leaves);
        extract_stub_test_leaves(&body, &mut stub_leaves);
    }
    Ok((
        RustWorkspaceTestResolver { known_leaves },
        RustWorkspaceStubScanner { stub_leaves },
    ))
}

/// Workspace-scanning implementation of [`TestPathResolver`].
///
/// Walks every `.rs` file under `repo_root`, eagerly indexing every
/// function name introduced by `#[test]`, `#[tokio::test]`, or a
/// `proptest!` block. The resolver matches annotation targets by their
/// trailing path segment — `crate::module::test_name` resolves iff some
/// scanned file defines a test function named `test_name`. Per the spec
/// this is best-effort: full module-path resolution would require cargo
/// metadata plus parsing module declarations, which this scanner
/// deliberately skips in favour of zero subprocess cost.
pub struct RustWorkspaceTestResolver {
    known_leaves: HashSet<String>,
}

impl RustWorkspaceTestResolver {
    /// Walk `repo_root` and index every test function leaf name.
    pub fn scan(repo_root: &Path) -> Result<Self, IntegrityError> {
        let mut known_leaves: HashSet<String> = HashSet::new();
        for entry in WalkDir::new(repo_root).follow_links(false) {
            let entry = entry.map_err(|e| IntegrityError::WalkWorkspace {
                root: repo_root.to_path_buf(),
                source: e,
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            if is_excluded_under_root(path, repo_root) {
                continue;
            }
            let Ok(body) = fs::read_to_string(path) else {
                continue;
            };
            extract_test_fn_leaves(&body, &mut known_leaves);
        }
        Ok(Self { known_leaves })
    }

    /// Construct a resolver pre-seeded with `leaves`. Useful for tests
    /// and for callers that compute the index by other means.
    pub fn from_leaves<I, S>(leaves: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            known_leaves: leaves.into_iter().map(Into::into).collect(),
        }
    }
}

impl TestPathResolver for RustWorkspaceTestResolver {
    fn resolves(&self, target: &str) -> bool {
        let Some(leaf) = test_target_leaf(target) else {
            return false;
        };
        self.known_leaves.contains(leaf)
    }
}

/// Workspace-scanning implementation of [`StubScanner`].
///
/// Walks every `.rs` file under `repo_root`, indexing the leaf names of
/// test functions (attribute-marked or `proptest!`-block-enclosed) whose
/// body invokes the `_pending_stub` sigil as a word-boundary token.
/// Matches `RustWorkspaceTestResolver`'s walking discipline: skips
/// `target/`, reads each file once, accepts the same heuristic
/// `proptest!` recognition. Stub recognition is best-effort — strings or
/// comments containing the sigil would also flag; tests covering the
/// scanner pin the contract.
pub struct RustWorkspaceStubScanner {
    stub_leaves: HashSet<String>,
}

impl RustWorkspaceStubScanner {
    /// Walk `repo_root` and index every test function whose body calls
    /// `_pending_stub`.
    pub fn scan(repo_root: &Path) -> Result<Self, IntegrityError> {
        let mut stub_leaves: HashSet<String> = HashSet::new();
        for entry in WalkDir::new(repo_root).follow_links(false) {
            let entry = entry.map_err(|e| IntegrityError::WalkWorkspace {
                root: repo_root.to_path_buf(),
                source: e,
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            if is_excluded_under_root(path, repo_root) {
                continue;
            }
            let Ok(body) = fs::read_to_string(path) else {
                continue;
            };
            extract_stub_test_leaves(&body, &mut stub_leaves);
        }
        Ok(Self { stub_leaves })
    }

    /// Construct a scanner pre-seeded with the given stub leaf names.
    /// Useful for tests and for callers that compute the index by other
    /// means.
    pub fn from_leaves<I, S>(leaves: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            stub_leaves: leaves.into_iter().map(Into::into).collect(),
        }
    }
}

impl StubScanner for RustWorkspaceStubScanner {
    fn is_stub(&self, leaf: &str) -> bool {
        self.stub_leaves.contains(leaf)
    }
}

/// Return the trailing path segment of a `[test]` target.
///
/// Targets use language-native path syntax: `crate::module::test_name`
/// for Rust, `tests/file.py::test_name` for Python. Both shapes carry
/// the leaf at the final segment after splitting on `::` and then on
/// `/`.
pub(crate) fn test_target_leaf(target: &str) -> Option<&str> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return None;
    }
    let after_colons = trimmed.rsplit("::").next().unwrap_or(trimmed);
    let after_slash = after_colons.rsplit('/').next().unwrap_or(after_colons);
    if after_slash.is_empty() {
        None
    } else {
        Some(after_slash)
    }
}

/// Scan `source` for Rust test function names and add their leaf names
/// to `sink`. The scanner is line-oriented and intentionally light —
/// pulling in `syn` for one leaf-extraction pass is overkill given the
/// resolver only needs membership lookup.
pub(crate) fn extract_test_fn_leaves(source: &str, sink: &mut HashSet<String>) {
    let mut last_attr: Option<usize> = None;
    let mut proptest_depth: usize = 0;
    let mut bare_brace_depth_for_proptest: usize = 0;
    for (i, raw_line) in source.lines().enumerate() {
        let line = raw_line.trim_start();
        if line.starts_with("#[test]")
            || line.starts_with("#[tokio::test")
            || line.starts_with("#[tokio_test")
        {
            last_attr = Some(i);
        }
        if line.starts_with("proptest!") && line.contains('{') {
            proptest_depth = proptest_depth.saturating_add(1);
            bare_brace_depth_for_proptest = bare_brace_depth_for_proptest.saturating_add(1);
        } else if proptest_depth > 0 {
            for c in line.chars() {
                if c == '{' {
                    bare_brace_depth_for_proptest = bare_brace_depth_for_proptest.saturating_add(1);
                } else if c == '}' {
                    bare_brace_depth_for_proptest = bare_brace_depth_for_proptest.saturating_sub(1);
                    if bare_brace_depth_for_proptest == 0 {
                        proptest_depth = proptest_depth.saturating_sub(1);
                    }
                }
            }
        }
        let attr_attached = matches!(last_attr, Some(j) if i.saturating_sub(j) <= 8);
        let in_proptest = proptest_depth > 0;
        if !attr_attached && !in_proptest {
            continue;
        }
        if let Some(name) = parse_fn_name(line) {
            sink.insert(name.to_string());
            if attr_attached {
                last_attr = None;
            }
        }
    }
}

/// Extract the identifier following `fn ` on a line, if any.
fn parse_fn_name(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(|c: char| c.is_whitespace() || c == '|');
    let stripped = trimmed
        .strip_prefix("pub ")
        .or_else(|| trimmed.strip_prefix("async "))
        .unwrap_or(trimmed);
    let stripped = stripped
        .strip_prefix("async ")
        .or_else(|| stripped.strip_prefix("pub "))
        .unwrap_or(stripped);
    let rest = stripped.strip_prefix("fn ")?;
    let end = rest
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    let name = &rest[..end];
    if name.is_empty() { None } else { Some(name) }
}

/// Byte-level body scan. For every test function in `source` (an
/// attribute-marked function or one enclosed by a `proptest!` block)
/// whose body invokes `_pending_stub` as a word-boundary token, insert
/// the function's leaf name into `sink`.
fn extract_stub_test_leaves(source: &str, sink: &mut HashSet<String>) {
    let bytes = source.as_bytes();
    let proptest_bodies = find_proptest_bodies(bytes);

    let mut last_test_attr_at: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        if is_test_attr_start(&bytes[i..]) {
            last_test_attr_at = Some(i);
            i = skip_attr_block(bytes, i);
            continue;
        }
        if starts_with_fn_keyword(bytes, i) {
            let attr_attached = matches!(
                last_test_attr_at,
                Some(at) if bytes[at..i].iter().all(|b| !is_fn_separator(*b))
            );
            let in_proptest = proptest_bodies.iter().any(|r| r.contains(&i));
            if (attr_attached || in_proptest)
                && let Some((name, body)) = extract_fn_name_and_body(bytes, i)
                && body_calls_pending_stub(&bytes[body.clone()])
            {
                sink.insert(name.to_string());
            }
            if attr_attached {
                last_test_attr_at = None;
            }
        }
        i += 1;
    }
}

/// True iff the body bytes contain `_pending_stub` as a word-boundary
/// token (not part of a longer identifier).
fn body_calls_pending_stub(body: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"_pending_stub";
    let mut k = 0;
    while k + NEEDLE.len() <= body.len() {
        if &body[k..k + NEEDLE.len()] == NEEDLE {
            let before_ok = k == 0 || !is_ident_continue(body[k - 1]);
            let after_idx = k + NEEDLE.len();
            let after_ok = after_idx >= body.len() || !is_ident_continue(body[after_idx]);
            if before_ok && after_ok {
                return true;
            }
        }
        k += 1;
    }
    false
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// True iff `s` begins with a Rust test attribute that marks the next
/// function as a test (`#[test]`, `#[tokio::test]`, `#[tokio_test]`).
fn is_test_attr_start(s: &[u8]) -> bool {
    s.starts_with(b"#[test]") || s.starts_with(b"#[tokio::test") || s.starts_with(b"#[tokio_test")
}

/// Skip past a `#[...]` attribute starting at byte `i`. Balances `[]`
/// pairs so attribute arguments containing nested brackets round-trip.
/// Returns the index of the byte after the closing `]`, or `bytes.len()`
/// when the attribute never closes.
fn skip_attr_block(bytes: &[u8], i: usize) -> usize {
    let mut depth = 0usize;
    let mut j = i;
    while j < bytes.len() {
        match bytes[j] {
            b'[' => depth = depth.saturating_add(1),
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return j + 1;
                }
            }
            _ => {}
        }
        j += 1;
    }
    bytes.len()
}

/// True iff `bytes[i..]` begins with `fn` as a keyword (followed by
/// whitespace).
fn starts_with_fn_keyword(bytes: &[u8], i: usize) -> bool {
    if !bytes[i..].starts_with(b"fn") {
        return false;
    }
    if i > 0 && is_ident_continue(bytes[i - 1]) {
        return false;
    }
    let after = i + 2;
    after < bytes.len() && (bytes[after] == b' ' || bytes[after] == b'\t' || bytes[after] == b'\n')
}

/// A separator byte between a `#[test]` attribute and the function it
/// applies to: a closing `}` or a semicolon means the attribute is no
/// longer in scope for a following `fn`.
fn is_fn_separator(b: u8) -> bool {
    b == b'}' || b == b';'
}

/// Given that `bytes[fn_at..]` starts with `fn`, parse the function's
/// name and the byte-range of its body (the content between the
/// outermost `{` and matching `}`). Returns `None` when the byte stream
/// runs out before the parser finds a balanced body — typically a
/// truncated source file or a malformed declaration.
fn extract_fn_name_and_body(bytes: &[u8], fn_at: usize) -> Option<(&str, Range<usize>)> {
    let after_fn = fn_at + 2;
    let mut k = after_fn;
    while k < bytes.len() && bytes[k].is_ascii_whitespace() {
        k += 1;
    }
    let name_start = k;
    while k < bytes.len() && is_ident_continue(bytes[k]) {
        k += 1;
    }
    if k == name_start {
        return None;
    }
    let name = std::str::from_utf8(&bytes[name_start..k]).ok()?;
    while k < bytes.len() && bytes[k] != b'(' {
        k += 1;
    }
    if k >= bytes.len() {
        return None;
    }
    k = balance_close(bytes, k, b'(', b')')?;
    while k < bytes.len() && bytes[k] != b'{' {
        k += 1;
    }
    if k >= bytes.len() {
        return None;
    }
    let body_start = k + 1;
    let body_end = balance_close(bytes, k, b'{', b'}')?;
    Some((name, body_start..body_end))
}

/// Balance an opening bracket at `lparen` with its closing counterpart.
/// Returns the index of the matching closer, or `None` when the bracket
/// never balances before end-of-input. Naïve byte-counter — strings and
/// comments may produce false matches; integrity-gate parsing has
/// accepted that trade-off elsewhere.
fn balance_close(bytes: &[u8], lparen: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0usize;
    let mut k = lparen;
    while k < bytes.len() {
        let b = bytes[k];
        if b == open {
            depth = depth.saturating_add(1);
        } else if b == close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(k);
            }
        }
        k += 1;
    }
    None
}

/// Locate every `proptest! { ... }` block in `bytes` and return the
/// byte-range covered by each block's body. The integrity gate's stub
/// scan walks these ranges in addition to attribute-marked functions
/// because the `proptest!` macro turns inner `fn ident(...) { body }`
/// declarations into test cases — matching
/// [`RustWorkspaceTestResolver`]'s recognition rule.
fn find_proptest_bodies(bytes: &[u8]) -> Vec<Range<usize>> {
    const MACRO: &[u8] = b"proptest!";
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut k = 0;
    while k + MACRO.len() <= bytes.len() {
        if &bytes[k..k + MACRO.len()] == MACRO {
            let before_ok = k == 0 || !is_ident_continue(bytes[k - 1]);
            if before_ok {
                let mut m = k + MACRO.len();
                while m < bytes.len() && bytes[m].is_ascii_whitespace() {
                    m += 1;
                }
                if m < bytes.len()
                    && bytes[m] == b'{'
                    && let Some(end) = balance_close(bytes, m, b'{', b'}')
                {
                    out.push((m + 1)..end);
                    k = end + 1;
                    continue;
                }
            }
        }
        k += 1;
    }
    out
}

/// Compose the cap-exhausted `loom:clarify` notes block for a
/// molecule's integrity findings, per `specs/gate.md` § *Integrity gate*
/// (Cap-exhausted fallback). Emits **one** composed `## Options — …`
/// block: one `### Option N` per integrity finding kind present, in the
/// spec's kind order (`UnresolvedAnnotation`, `StubTestFunction`,
/// `UnneededPendingMarker`), each drawn from that kind's primary
/// (Option 1) auto-option template and scoped to the affected
/// `spec:line` locations; the block closes with one final `### Option N`
/// for *"Mixed resolution via `msg -c` chat"*. Non-terminal variants are
/// skipped. Returns an empty string when no terminal finding is present.
///
/// One block per clarify bead preserves the *Options Format Contract*
/// invariant while keeping each present kind's resolution path visible.
/// The string is what `bd update <epic> --notes` consumes and what
/// `loom msg` parses to populate the option-reply menu.
#[must_use]
pub fn compose_clarify_options(findings: &[IntegrityFinding]) -> String {
    use std::fmt::Write;

    let mut unresolved: Vec<String> = Vec::new();
    let mut stub: Vec<String> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    for finding in findings.iter().filter(|f| f.is_push_gate_terminal()) {
        match finding {
            IntegrityFinding::UnresolvedAnnotation {
                spec, line, target, ..
            } => unresolved.push(format!("{}:{line} (`{target}`)", spec.display())),
            IntegrityFinding::StubTestFunction {
                spec,
                line,
                test_name,
                ..
            } => stub.push(format!("{}:{line} (`{test_name}`)", spec.display())),
            IntegrityFinding::UnneededPendingMarker {
                spec, line, target, ..
            } => pending.push(format!("{}:{line} (`{target}`)", spec.display())),
            _ => {}
        }
    }

    let total = unresolved.len() + stub.len() + pending.len();
    if total == 0 {
        return String::new();
    }

    let mut out = format!(
        "## Options — Integrity gate refused the push after the iteration cap \
         ({total} finding(s))\n\n",
    );
    let mut n = 1u32;
    if !unresolved.is_empty() {
        let _ = write!(
            out,
            "### Option {n} — Implement the missing verifier(s)\n\
             Add the verifier so each target resolves, at: {}. Pick this when \
             the criteria are correct and the verifiers have not been written \
             yet.\n\n",
            unresolved.join(", "),
        );
        n += 1;
    }
    if !stub.is_empty() {
        let _ = write!(
            out,
            "### Option {n} — Implement the stub test body(ies)\n\
             Replace the `_pending_stub` sigil with a real assertion that \
             exercises the criterion, at: {}. Pick this when the criteria are \
             correct and the tests are owed.\n\n",
            stub.join(", "),
        );
        n += 1;
    }
    if !pending.is_empty() {
        let _ = write!(
            out,
            "### Option {n} — Drop the `?` marker(s)\n\
             The implementation has caught up to the claim; change \
             `[tier?](target)` to `[tier](target)` at: {}. This is the \
             expected resolution and almost always the right one.\n\n",
            pending.join(", "),
        );
        n += 1;
    }
    let _ = write!(
        out,
        "### Option {n} — Mixed resolution via `msg -c` chat\n\
         Reply with `loom msg -c` when the findings need different \
         resolutions across kinds, or you want options beyond each kind's \
         primary (retarget, mark the annotation pending with `?`, or remove \
         the criterion).\n",
    );
    out
}

/// Run every integrity direction and return all findings: forward
/// resolution, atomic acceptance, and stub-pointing.
pub fn check(
    annotations: &[Annotation],
    runner_specs: &[RunnerSpec],
    repo_root: &Path,
    command_resolver: &dyn CommandResolver,
    test_resolver: &dyn TestPathResolver,
    stub_scanner: &dyn StubScanner,
    pending_executor: &dyn PendingCommandExecutor,
) -> Vec<IntegrityFinding> {
    let mut findings = check_forward(
        annotations,
        runner_specs,
        repo_root,
        command_resolver,
        test_resolver,
        stub_scanner,
        pending_executor,
    );
    findings.extend(check_atomic_acceptance(annotations));
    findings
}

/// Forward direction: every annotation's target must resolve for its
/// tier, and any resolved Rust test target must not be a stub. Emits
/// at most one finding per annotation: an
/// [`IntegrityFinding::UnresolvedAnnotation`] if the target fails to
/// resolve, otherwise (for `[check](cargo test ... <name>)`) an
/// [`IntegrityFinding::UnresolvedCargoTestName`] if the embedded test
/// name does not resolve, otherwise an
/// [`IntegrityFinding::StubTestFunction`] if the resolved test function
/// is a stub. Distinct annotations on distinct criteria are independent
/// — flagging one does not suppress findings on the others.
///
/// Annotations carrying the `?` pending modifier flip the outcome per
/// `specs/gate.md` § Pending modifier: unresolved targets pass silently
/// (the implementer is allowed to defer the verifier) and resolved
/// targets emit [`IntegrityFinding::UnneededPendingMarker`] — the
/// self-cleaning marker has gone stale. For `[test?]`, a `_pending_stub`
/// body counts as "implementation not yet present" and passes silently
/// the same way; the marker only fires once the body becomes real.
///
/// For `[check?]` and `[system?]`, "resolved" means "the command spawns
/// and exits with status 0" via [`PendingCommandExecutor`] — not the
/// narrow first-token-on-PATH check used for the non-pending form. This
/// uniformly honors author intent across binary-pending (the executable
/// doesn't exist yet) and assertion-pending (the executable exists but
/// the asserted condition isn't true yet); both fail-modes produce
/// non-zero exit, both silent-pass under the modifier.
pub fn check_forward(
    annotations: &[Annotation],
    runner_specs: &[RunnerSpec],
    repo_root: &Path,
    command_resolver: &dyn CommandResolver,
    test_resolver: &dyn TestPathResolver,
    stub_scanner: &dyn StubScanner,
    pending_executor: &dyn PendingCommandExecutor,
) -> Vec<IntegrityFinding> {
    let mut out = Vec::new();
    for ann in annotations {
        if ann.pending {
            let resolved = match ann.tier {
                Tier::Check | Tier::System => pending_executor.executes_zero(ann),
                Tier::Test => test_resolver.resolves(&ann.target),
                Tier::Judge => resolves_judge_path(&ann.target, &ann.source_spec, repo_root),
            };
            if let Some(finding) =
                pending_forward_finding(ann, resolved, test_resolver, stub_scanner)
            {
                out.push(finding);
            }
            continue;
        }
        let resolved = match ann.tier {
            Tier::Check | Tier::System => {
                runner_owns_target(runner_specs, &ann.target)
                    || resolves_command(&ann.target, command_resolver)
            }
            Tier::Test => test_resolver.resolves(&ann.target),
            Tier::Judge => resolves_judge_path(&ann.target, &ann.source_spec, repo_root),
        };
        if !resolved {
            out.push(IntegrityFinding::UnresolvedAnnotation {
                spec: ann.source_spec.clone(),
                line: ann.line,
                tier: ann.tier,
                target: ann.target.clone(),
            });
            continue;
        }
        if ann.tier == Tier::Test
            && let Some(leaf) = test_target_leaf(&ann.target)
            && stub_scanner.is_stub(leaf)
        {
            out.push(IntegrityFinding::StubTestFunction {
                spec: ann.source_spec.clone(),
                line: ann.line,
                tier: ann.tier,
                target: ann.target.clone(),
                test_name: leaf.to_string(),
            });
            continue;
        }
        if ann.tier == Tier::Check
            && let Some(test_name) = extract_cargo_test_name(&ann.target)
        {
            if !test_resolver.resolves(test_name) {
                out.push(IntegrityFinding::UnresolvedCargoTestName {
                    spec: ann.source_spec.clone(),
                    line: ann.line,
                    target: ann.target.clone(),
                    test_name: test_name.to_string(),
                });
            } else if stub_scanner.is_stub(test_name) {
                out.push(IntegrityFinding::StubTestFunction {
                    spec: ann.source_spec.clone(),
                    line: ann.line,
                    tier: ann.tier,
                    target: ann.target.clone(),
                    test_name: test_name.to_string(),
                });
            }
        }
    }
    out
}

/// Outcome for a `[tier?](target)` annotation: `None` (silent pass)
/// when the marker is still load-bearing — the target does not resolve,
/// or `[test?]`'s body is still a `_pending_stub`, or
/// `[check?](cargo test … name)`'s embedded test name is missing or
/// stubbed. `Some(UnneededPendingMarker)` when every condition the
/// marker was suppressing has resolved, so the marker itself is the
/// finding.
fn pending_forward_finding(
    ann: &Annotation,
    resolved: bool,
    test_resolver: &dyn TestPathResolver,
    stub_scanner: &dyn StubScanner,
) -> Option<IntegrityFinding> {
    if !resolved {
        return None;
    }
    if ann.tier == Tier::Test
        && let Some(leaf) = test_target_leaf(&ann.target)
        && stub_scanner.is_stub(leaf)
    {
        return None;
    }
    if ann.tier == Tier::Check
        && let Some(test_name) = extract_cargo_test_name(&ann.target)
        && (!test_resolver.resolves(test_name) || stub_scanner.is_stub(test_name))
    {
        return None;
    }
    Some(IntegrityFinding::UnneededPendingMarker {
        spec: ann.source_spec.clone(),
        line: ann.line,
        tier: ann.tier,
        target: ann.target.clone(),
    })
}

/// Atomic-acceptance direction: each criterion carries exactly one
/// annotation. Returns one [`IntegrityFinding::MultipleAnnotations`] per
/// criterion carrying two or more annotations.
pub fn check_atomic_acceptance(annotations: &[Annotation]) -> Vec<IntegrityFinding> {
    let mut by_criterion: BTreeMap<(PathBuf, u32), Vec<&Annotation>> = BTreeMap::new();
    for ann in annotations {
        by_criterion
            .entry((ann.source_spec.clone(), ann.criterion_line))
            .or_default()
            .push(ann);
    }
    let mut out = Vec::new();
    for ((spec, line), anns) in by_criterion {
        if anns.len() > 1 {
            out.push(IntegrityFinding::MultipleAnnotations {
                spec,
                line,
                count: anns.len(),
            });
        }
    }
    out
}

/// True iff some runner in `specs` `match`es `target` — the runner owns
/// the annotation, so forward-resolution succeeds because a runner claims
/// it, not because `tokens[0]` is on PATH. Reuses [`RunnerSpec::matches`]
/// (the same compiled match regex `dispatch::group_by_runner` keys on) so
/// resolution and dispatch agree on ownership. An unmatched target falls
/// through to the [`resolves_command`] `tokens[0]`-on-PATH check per
/// `specs/gate.md` § Runners — *Runner-owned resolution*.
fn runner_owns_target(specs: &[RunnerSpec], target: &str) -> bool {
    specs.iter().any(|spec| spec.matches(target))
}

fn resolves_command(target: &str, command_resolver: &dyn CommandResolver) -> bool {
    let Some(first) = first_token(target) else {
        return false;
    };
    let path_part = first.split_once("::").map_or(first, |(p, _)| p);
    if path_part.is_empty() {
        return false;
    }
    command_resolver.resolves(path_part)
}

/// True iff `target`'s first token is a bare binary name (no path
/// separators) that does not resolve via `command_resolver`. Path-shaped
/// first tokens (containing `/` or absolute) never qualify: those name
/// a concrete file in the repo and a missing file is a real finding.
///
/// Mirrors the `skip-if-missing` wrapper used by pre-commit hooks for
/// `nix` / `cargo`: in environments where the binary is absent (notably
/// the bead container), the annotation silently no-ops rather than
/// failing the gate. Used by `loom gate verify --files` to keep the
/// agent's feedback-only commit flow green when host binaries are
/// unavailable.
pub fn is_missing_binary_target(target: &str, command_resolver: &dyn CommandResolver) -> bool {
    let Some(first) = first_token(target) else {
        return false;
    };
    let path_part = first.split_once("::").map_or(first, |(p, _)| p);
    if path_part.is_empty() {
        return false;
    }
    if path_part.contains('/') || Path::new(path_part).is_absolute() {
        return false;
    }
    !command_resolver.resolves(path_part)
}

fn first_token(command: &str) -> Option<&str> {
    command.split_whitespace().next()
}

/// Return the explicit test-name positional in a `cargo test [...] <name>`
/// command, or `None` when the command is not `cargo test`, has no
/// positional after the flags, or places the test name after `--`.
///
/// Flag-arity is data-driven: long flags listed in `LONG_FLAGS_WITH_ARG`
/// consume one following token; short flags in `SHORT_FLAGS_WITH_ARG` do
/// the same. `--flag=value` is one token. The first non-flag positional
/// is the test name. `--` ends scanning — args after it are runner args
/// for the test binary, not the test name itself.
fn extract_cargo_test_name(command: &str) -> Option<&str> {
    const LONG_FLAGS_WITH_ARG: &[&str] = &[
        "package",
        "bin",
        "example",
        "bench",
        "test",
        "target",
        "target-dir",
        "manifest-path",
        "features",
        "jobs",
        "profile",
        "lockfile-path",
        "config",
        "exclude",
    ];
    const SHORT_FLAGS_WITH_ARG: &[&str] = &["p", "F", "j"];

    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.first() != Some(&"cargo") || tokens.get(1) != Some(&"test") {
        return None;
    }
    let mut i = 2;
    while i < tokens.len() {
        let tok = tokens[i];
        if tok == "--" {
            return None;
        }
        if let Some(long) = tok.strip_prefix("--") {
            if long.is_empty() {
                return None;
            }
            if long.contains('=') || !LONG_FLAGS_WITH_ARG.contains(&long) {
                i += 1;
            } else {
                i += 2;
            }
            continue;
        }
        if let Some(short) = tok.strip_prefix('-')
            && !short.is_empty()
        {
            if SHORT_FLAGS_WITH_ARG.contains(&short) {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        return Some(tok);
    }
    None
}

/// Resolves a `[judge]` target's path part against the spec file's own
/// directory, matching the markdown renderer's relative-link resolution.
/// Accepts `#fn` (canonical, standard URL-fragment syntax) or `::fn`
/// (legacy) as the function-selector separator. Absolute paths are
/// honoured as-is; relative paths are joined with `source_spec.parent()`
/// when present, falling back to `repo_root` only when the spec has no
/// parent component.
fn resolves_judge_path(target: &str, source_spec: &Path, repo_root: &Path) -> bool {
    resolve_spec_relative_script_path(target, source_spec, repo_root).is_some_and(|p| p.exists())
}

/// Lexically resolve a `[judge]` target that *is* a script-file path to
/// the on-disk path it points at: selector stripped, spec-relative path
/// joined against the spec file's own directory, `..`/`.` collapsed.
/// Shared by the integrity gate and the input resolver so the existence
/// check and the input-query invocation cannot disagree about where the
/// judge script lives. `[check]` / `[system]` targets do not use this
/// helper — they resolve by runner match or the `tokens[0]` fallback.
/// Returns `None` only when the target (or its path part) is empty;
/// existence is *not* checked here so callers can choose between an
/// existence test (integrity gate) and reading the script body (input
/// resolver). The returned path may not exist.
pub(crate) fn resolve_spec_relative_script_path(
    target: &str,
    source_spec: &Path,
    repo_root: &Path,
) -> Option<PathBuf> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path_part = strip_target_selector(trimmed);
    if path_part.is_empty() {
        return None;
    }
    let p = Path::new(path_part);
    let raw = if p.is_absolute() {
        p.to_path_buf()
    } else {
        spec_relative_base(source_spec, repo_root).join(p)
    };
    Some(normalize_path(&raw))
}

/// Collapse `..` and `.` lexically. Intermediate components in a
/// spec-relative target like `specs/../tests/...` may not exist as real
/// directories (the spec dir does; an arbitrary intermediate may not), so
/// the kernel's path walker would return ENOENT before reaching the real
/// target. Markdown renderers do the same lexical collapse, so this keeps
/// the gate aligned with what a reader sees on click.
fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `#` is the canonical selector separator (URL-fragment syntax, clickable
/// in markdown renderers); `::` is accepted during migration. Whichever
/// appears first wins so paths containing the other character are handled
/// predictably.
fn strip_target_selector(target: &str) -> &str {
    let hash = target.find('#');
    let colons = target.find("::");
    match (hash, colons) {
        (Some(h), Some(c)) if h < c => &target[..h],
        (Some(h), None) => &target[..h],
        (_, Some(c)) => &target[..c],
        (None, None) => target,
    }
}

fn spec_relative_base(source_spec: &Path, repo_root: &Path) -> PathBuf {
    match source_spec.parent() {
        Some(parent) if parent.as_os_str().is_empty() => repo_root.to_path_buf(),
        Some(parent) if parent.is_absolute() => parent.to_path_buf(),
        Some(parent) => repo_root.join(parent),
        None => repo_root.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    use std::collections::HashSet;
    use std::fs;

    use tempfile::tempdir;

    fn ann(tier: Tier, target: &str, spec: &str, line: u32, criterion_line: u32) -> Annotation {
        Annotation {
            tier,
            target: target.into(),
            source_spec: PathBuf::from(spec),
            line,
            criterion_line,
            pending: false,
        }
    }

    struct StubCommands {
        ok: HashSet<String>,
    }

    impl StubCommands {
        fn with(items: &[&str]) -> Self {
            Self {
                ok: items.iter().map(|s| (*s).to_string()).collect(),
            }
        }
    }

    impl CommandResolver for StubCommands {
        fn resolves(&self, first_token: &str) -> bool {
            self.ok.contains(first_token)
        }
    }

    struct StubTests {
        ok: HashSet<String>,
    }

    impl StubTests {
        fn with(items: &[&str]) -> Self {
            Self {
                ok: items.iter().map(|s| (*s).to_string()).collect(),
            }
        }
    }

    impl TestPathResolver for StubTests {
        fn resolves(&self, target: &str) -> bool {
            self.ok.contains(target)
        }
    }

    struct StubLeaves {
        stub_leaves: HashSet<String>,
    }

    impl StubLeaves {
        fn with(items: &[&str]) -> Self {
            Self {
                stub_leaves: items.iter().map(|s| (*s).to_string()).collect(),
            }
        }
        fn none() -> Self {
            Self {
                stub_leaves: HashSet::new(),
            }
        }
    }

    impl StubScanner for StubLeaves {
        fn is_stub(&self, leaf: &str) -> bool {
            self.stub_leaves.contains(leaf)
        }
    }

    struct StubExecutor {
        zero_exit_commands: HashSet<String>,
    }

    impl StubExecutor {
        fn none() -> Self {
            Self {
                zero_exit_commands: HashSet::new(),
            }
        }
    }

    impl PendingCommandExecutor for StubExecutor {
        fn executes_zero(&self, annotation: &Annotation) -> bool {
            self.zero_exit_commands.contains(&annotation.target)
        }
    }

    #[test]
    fn unresolved_annotation_renders_per_spec_format() {
        let f = IntegrityFinding::UnresolvedAnnotation {
            spec: PathBuf::from("specs/gate.md"),
            line: 42,
            tier: Tier::Check,
            target: "cargo run -p loom-gate -- self".into(),
        };
        assert_eq!(
            f.to_string(),
            "specs/gate.md:42: annotation [check](cargo run -p loom-gate -- self) — does not resolve"
        );
    }

    #[test]
    fn multiple_annotations_renders_per_spec_format() {
        let f = IntegrityFinding::MultipleAnnotations {
            spec: PathBuf::from("specs/tests.md"),
            line: 7,
            count: 2,
        };
        assert_eq!(
            f.to_string(),
            "specs/tests.md:7: criterion carries 2 annotations, expected 1"
        );
    }

    #[test]
    fn atomic_acceptance_passes_when_each_criterion_has_one_annotation() {
        let annotations = vec![
            ann(Tier::Test, "crate::a::ok", "specs/a.md", 5, 4),
            ann(Tier::Check, "cargo run", "specs/a.md", 8, 7),
        ];
        assert!(check_atomic_acceptance(&annotations).is_empty());
    }

    #[test]
    fn atomic_acceptance_flags_two_annotations_on_one_criterion() {
        let annotations = vec![
            ann(Tier::Test, "crate::a::t", "specs/a.md", 5, 4),
            ann(Tier::Check, "cargo run", "specs/a.md", 6, 4),
        ];
        let findings = check_atomic_acceptance(&annotations);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0],
            IntegrityFinding::MultipleAnnotations {
                spec: PathBuf::from("specs/a.md"),
                line: 4,
                count: 2,
            }
        );
    }

    #[test]
    fn atomic_acceptance_counts_three_annotations_correctly() {
        let annotations = vec![
            ann(Tier::Test, "crate::a::t", "specs/a.md", 5, 4),
            ann(Tier::Check, "cargo run", "specs/a.md", 6, 4),
            ann(Tier::Judge, "rubrics/x.md", "specs/a.md", 7, 4),
        ];
        let findings = check_atomic_acceptance(&annotations);
        assert_eq!(findings.len(), 1);
        match &findings[0] {
            IntegrityFinding::MultipleAnnotations { count, .. } => assert_eq!(*count, 3),
            other => panic!("expected MultipleAnnotations, got {other:?}"),
        }
    }

    #[test]
    fn n_to_one_sharing_across_criteria_is_allowed() {
        let annotations = vec![
            ann(Tier::Test, "crate::shared::t", "specs/a.md", 5, 4),
            ann(Tier::Test, "crate::shared::t", "specs/a.md", 8, 7),
            ann(Tier::Test, "crate::shared::t", "specs/b.md", 3, 2),
        ];
        assert!(
            check_atomic_acceptance(&annotations).is_empty(),
            "different criteria pointing at the same verifier is allowed"
        );
    }

    #[test]
    fn forward_passes_when_every_annotation_resolves() {
        let dir = tempdir().unwrap();
        let rubric = dir.path().join("rubric.md");
        fs::write(&rubric, "rubric body").unwrap();

        let annotations = vec![
            ann(Tier::Check, "cargo run -p w", "specs/a.md", 1, 1),
            ann(Tier::System, "nix run .#x", "specs/a.md", 2, 2),
            ann(Tier::Test, "crate::a::ok", "specs/a.md", 3, 3),
            ann(Tier::Judge, "../rubric.md", "specs/a.md", 4, 4),
        ];
        let cmds = StubCommands::with(&["cargo", "nix"]);
        let tests = StubTests::with(&["crate::a::ok"]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert!(findings.is_empty(), "got findings: {findings:?}");
    }

    #[test]
    fn forward_flags_check_with_unknown_first_token() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::Check,
            "not-on-path --do-thing",
            "specs/a.md",
            10,
            10,
        )];
        let cmds = StubCommands::with(&["cargo"]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0],
            IntegrityFinding::UnresolvedAnnotation {
                spec: PathBuf::from("specs/a.md"),
                line: 10,
                tier: Tier::Check,
                target: "not-on-path --do-thing".into(),
            }
        );
    }

    #[test]
    fn forward_flags_system_with_unknown_first_token() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::System,
            "not-on-path --boot",
            "specs/a.md",
            11,
            11,
        )];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert_eq!(findings.len(), 1);
        assert!(matches!(
            findings[0],
            IntegrityFinding::UnresolvedAnnotation {
                tier: Tier::System,
                ..
            }
        ));
    }

    #[test]
    fn runner_matched_target_resolves_via_runner_not_token_path_check() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::Check,
            "loom-walk inputs-check",
            "specs/a.md",
            20,
            20,
        )];
        let no_path = StubCommands::with(&[]);
        let tests = StubTests::with(&[]);

        let unmatched = check_forward(
            &annotations,
            &[],
            dir.path(),
            &no_path,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert_eq!(
            unmatched.len(),
            1,
            "unmatched target falls back to the tokens[0]-on-PATH check"
        );
        assert!(matches!(
            unmatched[0],
            IntegrityFinding::UnresolvedAnnotation {
                tier: Tier::Check,
                ..
            }
        ));

        let runner = RunnerSpec::compile(
            "walk",
            Some(r"^loom-walk "),
            "loom-walk {targets}",
            "{name}",
            " ",
            crate::runner::BuiltinParser::JsonLines,
            None,
        )
        .unwrap();
        let matched = check_forward(
            &annotations,
            std::slice::from_ref(&runner),
            dir.path(),
            &no_path,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert!(
            matched.is_empty(),
            "runner-matched target resolves via runner ownership, not tokens[0]: {matched:?}"
        );
    }

    #[test]
    fn forward_flags_test_with_unknown_path() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::Test,
            "crate::missing::nowhere",
            "specs/a.md",
            12,
            12,
        )];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&["crate::a::ok"]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert_eq!(findings.len(), 1);
        assert!(matches!(
            findings[0],
            IntegrityFinding::UnresolvedAnnotation {
                tier: Tier::Test,
                ..
            }
        ));
    }

    #[test]
    fn forward_flags_judge_when_file_absent() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(Tier::Judge, "../missing.md", "specs/a.md", 13, 13)];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert_eq!(findings.len(), 1);
        assert!(matches!(
            findings[0],
            IntegrityFinding::UnresolvedAnnotation {
                tier: Tier::Judge,
                ..
            }
        ));
    }

    #[test]
    fn forward_judge_accepts_absolute_path() {
        let dir = tempdir().unwrap();
        let rubric = dir.path().join("rubric.md");
        fs::write(&rubric, "body").unwrap();
        let target = rubric.to_string_lossy().into_owned();

        let annotations = vec![ann(Tier::Judge, &target, "specs/a.md", 14, 14)];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(
            &annotations,
            &[],
            Path::new("/this/is/ignored"),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert!(findings.is_empty(), "absolute judge path resolved");
    }

    #[test]
    fn forward_judge_accepts_script_with_fn_selector() {
        let dir = tempdir().unwrap();
        let script_dir = dir.path().join("tests/judges");
        fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("loom.sh");
        fs::write(&script, "#!/usr/bin/env bash\n").unwrap();

        let annotations = vec![ann(
            Tier::Judge,
            "../tests/judges/loom.sh::judge_tool_trait_ecosystem_compat",
            "specs/a.md",
            15,
            15,
        )];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert!(
            findings.is_empty(),
            "judge target with ::fn selector should resolve to leading path: {findings:?}"
        );
    }

    #[test]
    fn forward_judge_accepts_script_with_hash_fn_selector() {
        let dir = tempdir().unwrap();
        let script_dir = dir.path().join("tests/judges");
        fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("loom.sh");
        fs::write(&script, "#!/usr/bin/env bash\n").unwrap();

        let annotations = vec![ann(
            Tier::Judge,
            "../tests/judges/loom.sh#judge_tool_trait_ecosystem_compat",
            "specs/a.md",
            15,
            15,
        )];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert!(
            findings.is_empty(),
            "judge target with #fn selector should resolve to leading path: {findings:?}"
        );
    }

    #[test]
    fn forward_judge_resolves_relative_to_spec_dir() {
        let dir = tempdir().unwrap();
        let script_dir = dir.path().join("tests/judges");
        fs::create_dir_all(&script_dir).unwrap();
        fs::write(script_dir.join("x.sh"), "#!/usr/bin/env bash\n").unwrap();

        let annotations = vec![ann(Tier::Judge, "tests/judges/x.sh#fn", "specs/a.md", 5, 5)];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert_eq!(
            findings.len(),
            1,
            "spec-relative path: a target without ../ should not resolve from inside specs/, \
             got: {findings:?}"
        );
        assert!(matches!(
            findings[0],
            IntegrityFinding::UnresolvedAnnotation {
                tier: Tier::Judge,
                ..
            }
        ));
    }

    #[test]
    fn forward_system_accepts_path_with_attr_selector() {
        let dir = tempdir().unwrap();
        let nix_dir = dir.path().join("tests");
        fs::create_dir_all(&nix_dir).unwrap();
        let unit_nix = nix_dir.join("unit.nix");
        fs::write(&unit_nix, "{ }\n").unwrap();

        let annotations = vec![
            ann(
                Tier::System,
                "tests/unit.nix::eval-smoke",
                "specs/a.md",
                17,
                17,
            ),
            ann(
                Tier::System,
                "tests/unit.nix::Wait for worker worktree",
                "specs/a.md",
                18,
                18,
            ),
        ];
        let cmds = FsCommandResolver::with_path(dir.path(), "");
        let tests = StubTests::with(&[]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert!(
            findings.is_empty(),
            "system target with ::attr selector should resolve to leading path: {findings:?}"
        );
    }

    #[test]
    fn forward_system_flags_missing_path_with_attr_selector() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::System,
            "tests/absent.nix::some-attr",
            "specs/a.md",
            19,
            19,
        )];
        let cmds = FsCommandResolver::with_path(dir.path(), "");
        let tests = StubTests::with(&[]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert_eq!(findings.len(), 1);
        assert!(matches!(
            findings[0],
            IntegrityFinding::UnresolvedAnnotation {
                tier: Tier::System,
                ..
            }
        ));
    }

    #[test]
    fn forward_judge_flags_missing_script_with_fn_selector() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::Judge,
            "../tests/judges/absent.sh::some_fn",
            "specs/a.md",
            16,
            16,
        )];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert_eq!(findings.len(), 1);
        assert!(matches!(
            findings[0],
            IntegrityFinding::UnresolvedAnnotation {
                tier: Tier::Judge,
                ..
            }
        ));
    }

    #[test]
    fn check_combines_forward_and_atomic_acceptance() {
        let dir = tempdir().unwrap();
        let annotations = vec![
            ann(Tier::Test, "crate::a::t", "specs/a.md", 5, 4),
            ann(Tier::Check, "missing-cmd", "specs/a.md", 6, 4),
        ];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&["crate::a::t"]);
        let findings = check(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, IntegrityFinding::UnresolvedAnnotation { .. })),
            "forward flag present"
        );
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, IntegrityFinding::MultipleAnnotations { .. })),
            "atomic-acceptance flag present"
        );
    }

    #[test]
    fn fs_command_resolver_finds_binary_on_pinned_path() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join("my-walk");
        fs::write(&bin, "#!/bin/sh\n").unwrap();

        let resolver = FsCommandResolver::with_path(dir.path(), &bin_dir.to_string_lossy());
        assert!(resolver.resolves("my-walk"));
        assert!(!resolver.resolves("not-installed"));
    }

    #[test]
    fn fs_command_resolver_finds_file_under_repo_root() {
        let dir = tempdir().unwrap();
        let scripts = dir.path().join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        let script = scripts.join("walk.sh");
        fs::write(&script, "#!/bin/sh\n").unwrap();

        let resolver = FsCommandResolver::with_path(dir.path(), "");
        assert!(resolver.resolves("scripts/walk.sh"));
        assert!(!resolver.resolves("scripts/missing.sh"));
    }

    #[test]
    fn fs_command_resolver_accepts_absolute_path() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("walk.sh");
        fs::write(&script, "#!/bin/sh\n").unwrap();

        let resolver = FsCommandResolver::with_path("/elsewhere", "");
        assert!(resolver.resolves(&script.to_string_lossy()));
    }

    #[test]
    fn fs_command_resolver_rejects_empty_token() {
        let resolver = FsCommandResolver::with_path("/repo", "/usr/bin");
        assert!(!resolver.resolves(""));
    }

    #[test]
    fn rust_workspace_test_resolver_finds_attribute_test() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.rs");
        fs::write(
            &src,
            "#[test]\nfn alpha_works() { assert!(true); }\n\n#[tokio::test]\nasync fn beta_runs() {}\n",
        )
        .unwrap();

        let resolver = RustWorkspaceTestResolver::scan(dir.path()).unwrap();
        assert!(resolver.resolves("crate::module::alpha_works"));
        assert!(resolver.resolves("crate::module::beta_runs"));
        assert!(!resolver.resolves("crate::module::gamma_missing"));
    }

    #[test]
    fn rust_workspace_test_resolver_finds_proptest_function() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("props.rs");
        fs::write(
            &src,
            "proptest! {\n    fn parses_arbitrary_bytes(bytes in any::<Vec<u8>>()) {\n        // body\n    }\n}\n",
        )
        .unwrap();

        let resolver = RustWorkspaceTestResolver::scan(dir.path()).unwrap();
        assert!(resolver.resolves("crate::props::parses_arbitrary_bytes"));
    }

    #[test]
    fn rust_workspace_test_resolver_skips_target_directory() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target/debug/build/foo.rs");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "#[test]\nfn should_not_be_indexed() {}\n").unwrap();

        let resolver = RustWorkspaceTestResolver::scan(dir.path()).unwrap();
        assert!(!resolver.resolves("anything::should_not_be_indexed"));
    }

    #[test]
    fn rust_workspace_test_resolver_indexes_when_repo_root_lives_under_dotloom() {
        // Mirrors the prek pre-push case where `loom gate verify` runs
        // from inside `.loom/beads/<id>/`: the absolute path's prefix
        // contains `.loom` but the relative-to-root path does not.
        let outer = tempdir().unwrap();
        let repo_root = outer.path().join(".loom/beads/lm-test/clone");
        let src = repo_root.join("crates/foo/tests/it.rs");
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::write(&src, "#[test]\nfn should_be_indexed() {}\n").unwrap();

        let resolver = RustWorkspaceTestResolver::scan(&repo_root).unwrap();
        assert!(resolver.resolves("crate::foo::it::should_be_indexed"));
    }

    #[test]
    fn rust_workspace_test_resolver_misses_plain_fn() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.rs");
        fs::write(&src, "fn helper() { }\n").unwrap();
        let resolver = RustWorkspaceTestResolver::scan(dir.path()).unwrap();
        assert!(!resolver.resolves("crate::module::helper"));
    }

    #[test]
    fn scan_workspace_pair_indexes_both_test_and_stub_leaves_in_one_walk() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.rs");
        fs::write(
            &src,
            "#[test]\nfn passes() { assert!(true); }\n\
             #[test]\nfn pending() { _pending_stub(); }\n",
        )
        .unwrap();

        let (resolver, scanner) = scan_workspace_pair(dir.path()).unwrap();
        assert!(resolver.resolves("crate::src::passes"));
        assert!(resolver.resolves("crate::src::pending"));
        assert!(!scanner.is_stub("passes"));
        assert!(scanner.is_stub("pending"));
    }

    #[test]
    fn test_target_leaf_handles_rust_and_python_shapes() {
        assert_eq!(
            test_target_leaf("crate::module::test_name"),
            Some("test_name")
        );
        assert_eq!(
            test_target_leaf("tests/test_foo.py::test_bar"),
            Some("test_bar")
        );
        assert_eq!(test_target_leaf("solo"), Some("solo"));
        assert_eq!(test_target_leaf(""), None);
    }

    #[test]
    fn from_leaves_constructor_round_trips() {
        let resolver = RustWorkspaceTestResolver::from_leaves(["one", "two"]);
        assert!(resolver.resolves("crate::a::one"));
        assert!(resolver.resolves("crate::b::two"));
        assert!(!resolver.resolves("crate::a::three"));
    }

    #[test]
    fn extract_cargo_test_name_returns_positional_after_lib() {
        assert_eq!(
            extract_cargo_test_name(
                "cargo test -p loom-events --lib serde_round_trips_as_plain_string"
            ),
            Some("serde_round_trips_as_plain_string")
        );
    }

    #[test]
    fn extract_cargo_test_name_returns_positional_after_named_suite() {
        assert_eq!(
            extract_cargo_test_name("cargo test -p loom --test cli_help help_snapshot"),
            Some("help_snapshot")
        );
    }

    #[test]
    fn extract_cargo_test_name_returns_none_when_suite_value_is_only_positional() {
        assert_eq!(
            extract_cargo_test_name("cargo test -p loom-templates --test snapshots"),
            None
        );
    }

    #[test]
    fn extract_cargo_test_name_returns_none_for_non_cargo_test_command() {
        assert_eq!(
            extract_cargo_test_name("cargo run -p loom-walk -- single_event_channel"),
            None
        );
        assert_eq!(extract_cargo_test_name("nix run .#test-loom"), None);
        assert_eq!(extract_cargo_test_name("rg pattern"), None);
    }

    #[test]
    fn extract_cargo_test_name_stops_at_double_dash() {
        assert_eq!(
            extract_cargo_test_name("cargo test -p foo -- --nocapture"),
            None
        );
    }

    #[test]
    fn extract_cargo_test_name_handles_long_flag_with_equals() {
        assert_eq!(
            extract_cargo_test_name("cargo test --package=foo --lib my_test"),
            Some("my_test")
        );
    }

    #[test]
    fn extract_cargo_test_name_skips_long_arg_flags() {
        assert_eq!(
            extract_cargo_test_name(
                "cargo test --manifest-path /tmp/Cargo.toml --features ci my_test"
            ),
            Some("my_test")
        );
    }

    #[test]
    fn forward_flags_check_cargo_test_with_missing_test_name() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::Check,
            "cargo test -p loom-events --lib does_not_exist",
            "specs/a.md",
            20,
            20,
        )];
        let cmds = StubCommands::with(&["cargo"]);
        let tests = StubTests::with(&["other_name"]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0],
            IntegrityFinding::UnresolvedCargoTestName {
                spec: PathBuf::from("specs/a.md"),
                line: 20,
                target: "cargo test -p loom-events --lib does_not_exist".into(),
                test_name: "does_not_exist".into(),
            }
        );
    }

    #[test]
    fn forward_passes_when_cargo_test_name_resolves() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::Check,
            "cargo test -p loom-gate --test integrity end_to_end_specs_dir_check_combines_both_directions",
            "specs/a.md",
            21,
            21,
        )];
        let cmds = StubCommands::with(&["cargo"]);
        let tests = StubTests::with(&["end_to_end_specs_dir_check_combines_both_directions"]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert!(findings.is_empty(), "got findings: {findings:?}");
    }

    #[test]
    fn forward_skips_cargo_test_name_check_when_no_explicit_name() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::Check,
            "cargo test -p loom-templates --test snapshots",
            "specs/a.md",
            22,
            22,
        )];
        let cmds = StubCommands::with(&["cargo"]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert!(
            findings.is_empty(),
            "no positional => no name check, got: {findings:?}"
        );
    }

    #[test]
    fn forward_does_not_apply_cargo_test_check_to_system_tier() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::System,
            "cargo test -p loom-events --lib does_not_exist",
            "specs/a.md",
            23,
            23,
        )];
        let cmds = StubCommands::with(&["cargo"]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &StubLeaves::none(),
            &StubExecutor::none(),
        );
        assert!(
            findings.is_empty(),
            "system tier ignores embedded cargo-test names, got: {findings:?}"
        );
    }

    #[test]
    fn unresolved_cargo_test_name_renders_per_spec_format() {
        let f = IntegrityFinding::UnresolvedCargoTestName {
            spec: PathBuf::from("specs/tests.md"),
            line: 692,
            target: "cargo test -p loom --test cli_help help_snapshot".into(),
            test_name: "help_snapshot".into(),
        };
        assert_eq!(
            f.to_string(),
            "specs/tests.md:692: annotation [check](cargo test -p loom --test cli_help help_snapshot) — cargo test name `help_snapshot` does not resolve"
        );
    }

    #[test]
    fn stub_test_function_renders_per_spec_format() {
        let f = IntegrityFinding::StubTestFunction {
            spec: PathBuf::from("specs/harness.md"),
            line: 100,
            tier: Tier::Test,
            target: "crate::a::is_stub".into(),
            test_name: "is_stub".into(),
        };
        assert_eq!(
            f.to_string(),
            "specs/harness.md:100: annotation [test](crate::a::is_stub) — test function `is_stub` calls _pending_stub"
        );
    }

    #[test]
    fn forward_flags_test_annotation_whose_target_body_calls_pending_stub() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(Tier::Test, "crate::a::stub_me", "specs/a.md", 30, 30)];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&["crate::a::stub_me"]);
        let stubs = StubLeaves::with(&["stub_me"]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &stubs,
            &StubExecutor::none(),
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0],
            IntegrityFinding::StubTestFunction {
                spec: PathBuf::from("specs/a.md"),
                line: 30,
                tier: Tier::Test,
                target: "crate::a::stub_me".into(),
                test_name: "stub_me".into(),
            }
        );
    }

    #[test]
    fn forward_passes_test_annotation_when_body_does_not_call_pending_stub() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(Tier::Test, "crate::a::real", "specs/a.md", 31, 31)];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&["crate::a::real"]);
        let stubs = StubLeaves::none();
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &stubs,
            &StubExecutor::none(),
        );
        assert!(
            findings.is_empty(),
            "real test should not flag: {findings:?}"
        );
    }

    #[test]
    fn forward_flags_check_cargo_test_annotation_when_target_test_is_stub() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::Check,
            "cargo test -p foo --lib stub_me",
            "specs/a.md",
            40,
            40,
        )];
        let cmds = StubCommands::with(&["cargo"]);
        let tests = StubTests::with(&["stub_me"]);
        let stubs = StubLeaves::with(&["stub_me"]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &stubs,
            &StubExecutor::none(),
        );
        assert_eq!(findings.len(), 1);
        match &findings[0] {
            IntegrityFinding::StubTestFunction {
                tier,
                test_name,
                target,
                ..
            } => {
                assert_eq!(*tier, Tier::Check);
                assert_eq!(test_name, "stub_me");
                assert_eq!(target, "cargo test -p foo --lib stub_me");
            }
            other => panic!("expected StubTestFunction, got {other:?}"),
        }
    }

    #[test]
    fn forward_does_not_flag_judge_or_system_for_stub() {
        let dir = tempdir().unwrap();
        let rubric = dir.path().join("r.md");
        fs::write(&rubric, "ok").unwrap();
        let annotations = vec![
            ann(Tier::Judge, "../r.md", "specs/a.md", 50, 50),
            ann(Tier::System, "nix run .#foo", "specs/a.md", 51, 51),
        ];
        let cmds = StubCommands::with(&["nix"]);
        let tests = StubTests::with(&[]);
        let stubs = StubLeaves::with(&["foo", "r"]);
        let findings = check_forward(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &stubs,
            &StubExecutor::none(),
        );
        assert!(
            findings.is_empty(),
            "judge/system are not test functions: {findings:?}"
        );
    }

    #[test]
    fn body_calls_pending_stub_requires_word_boundary() {
        assert!(body_calls_pending_stub(b"_pending_stub();"));
        assert!(body_calls_pending_stub(b"    _pending_stub!();"));
        assert!(body_calls_pending_stub(b"_pending_stub"));
        assert!(!body_calls_pending_stub(b"prefix_pending_stub();"));
        assert!(!body_calls_pending_stub(b"_pending_stub_extended();"));
        assert!(!body_calls_pending_stub(b"empty body"));
    }

    #[test]
    fn extract_stub_test_leaves_attr_marked_function_with_pending_stub() {
        let src = "\
#[test]
fn alpha_stub() {
    _pending_stub();
}

#[test]
fn alpha_real() {
    assert_eq!(2 + 2, 4);
}
";
        let mut sink = HashSet::new();
        extract_stub_test_leaves(src, &mut sink);
        assert!(sink.contains("alpha_stub"), "got {sink:?}");
        assert!(!sink.contains("alpha_real"), "got {sink:?}");
    }

    #[test]
    fn extract_stub_test_leaves_tokio_test_with_pending_stub() {
        let src = "\
#[tokio::test]
async fn beta_stub() {
    _pending_stub();
}
";
        let mut sink = HashSet::new();
        extract_stub_test_leaves(src, &mut sink);
        assert!(sink.contains("beta_stub"), "got {sink:?}");
    }

    #[test]
    fn extract_stub_test_leaves_proptest_block_with_pending_stub() {
        let src = "\
proptest! {
    fn gamma_stub(_x in any::<u8>()) {
        _pending_stub();
    }

    fn gamma_real(x in any::<u8>()) {
        prop_assert!(x as u16 + 1 > 0);
    }
}
";
        let mut sink = HashSet::new();
        extract_stub_test_leaves(src, &mut sink);
        assert!(sink.contains("gamma_stub"), "got {sink:?}");
        assert!(!sink.contains("gamma_real"), "got {sink:?}");
    }

    #[test]
    fn extract_stub_test_leaves_ignores_non_test_function() {
        let src = "\
fn delta_helper() {
    _pending_stub();
}
";
        let mut sink = HashSet::new();
        extract_stub_test_leaves(src, &mut sink);
        assert!(
            sink.is_empty(),
            "non-test helper must not be indexed: {sink:?}"
        );
    }

    #[test]
    fn rust_workspace_stub_scanner_indexes_stub_leaves_from_directory() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.rs");
        fs::write(
            &src,
            "#[test]\nfn epsilon_stub() {\n    _pending_stub();\n}\n\n#[test]\nfn epsilon_real() {\n    assert!(true);\n}\n",
        )
        .unwrap();
        let scanner = RustWorkspaceStubScanner::scan(dir.path()).unwrap();
        assert!(scanner.is_stub("epsilon_stub"));
        assert!(!scanner.is_stub("epsilon_real"));
    }

    #[test]
    fn rust_workspace_stub_scanner_skips_target_directory() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target/debug/build/foo.rs");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(
            &target,
            "#[test]\nfn target_stub() {\n    _pending_stub();\n}\n",
        )
        .unwrap();
        let scanner = RustWorkspaceStubScanner::scan(dir.path()).unwrap();
        assert!(!scanner.is_stub("target_stub"));
    }

    #[test]
    fn check_combines_stub_with_atomic_acceptance_findings() {
        let dir = tempdir().unwrap();
        let annotations = vec![
            ann(Tier::Test, "crate::a::stub_me", "specs/a.md", 5, 4),
            ann(Tier::Check, "cargo run", "specs/a.md", 6, 4),
        ];
        let cmds = StubCommands::with(&["cargo"]);
        let tests = StubTests::with(&["crate::a::stub_me"]);
        let stubs = StubLeaves::with(&["stub_me"]);
        let findings = check(
            &annotations,
            &[],
            dir.path(),
            &cmds,
            &tests,
            &stubs,
            &StubExecutor::none(),
        );
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, IntegrityFinding::StubTestFunction { .. })),
            "stub finding present: {findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, IntegrityFinding::MultipleAnnotations { .. })),
            "atomic-acceptance finding present: {findings:?}"
        );
    }

    #[test]
    fn is_push_gate_terminal_holds_for_unresolved_stub_and_unneeded_marker() {
        let spec = PathBuf::from("specs/a.md");
        assert!(
            IntegrityFinding::UnresolvedAnnotation {
                spec: spec.clone(),
                line: 1,
                tier: Tier::Check,
                target: "missing".into(),
            }
            .is_push_gate_terminal()
        );
        assert!(
            IntegrityFinding::StubTestFunction {
                spec: spec.clone(),
                line: 1,
                tier: Tier::Test,
                target: "crate::x".into(),
                test_name: "x".into(),
            }
            .is_push_gate_terminal()
        );
        assert!(
            IntegrityFinding::UnneededPendingMarker {
                spec: spec.clone(),
                line: 1,
                tier: Tier::Check,
                target: "true".into(),
            }
            .is_push_gate_terminal()
        );
        assert!(
            !IntegrityFinding::UnresolvedCargoTestName {
                spec: spec.clone(),
                line: 1,
                target: "cargo test foo".into(),
                test_name: "foo".into(),
            }
            .is_push_gate_terminal()
        );
        assert!(
            !IntegrityFinding::MultipleAnnotations {
                spec,
                line: 1,
                count: 2,
            }
            .is_push_gate_terminal()
        );
    }

    #[test]
    fn unneeded_pending_marker_renders_per_spec_format() {
        let f = IntegrityFinding::UnneededPendingMarker {
            spec: PathBuf::from("specs/gate.md"),
            line: 803,
            tier: Tier::Check,
            target: "true".into(),
        };
        assert_eq!(
            f.to_string(),
            "specs/gate.md:803: annotation [check?](true) is now resolved — drop the ? marker"
        );
    }

    #[test]
    fn compose_clarify_options_emits_one_block_per_present_kind_plus_mixed() {
        let findings = vec![
            IntegrityFinding::UnresolvedAnnotation {
                spec: PathBuf::from("specs/harness.md"),
                line: 42,
                tier: Tier::Check,
                target: "loom-walk surface".into(),
            },
            IntegrityFinding::StubTestFunction {
                spec: PathBuf::from("specs/gate.md"),
                line: 100,
                tier: Tier::Test,
                target: "crate::a::stub_me".into(),
                test_name: "stub_me".into(),
            },
            IntegrityFinding::UnneededPendingMarker {
                spec: PathBuf::from("specs/templates.md"),
                line: 88,
                tier: Tier::Test,
                target: "crate::a::landed".into(),
            },
        ];
        let out = compose_clarify_options(&findings);

        // One composed block: exactly one `## Options — …` heading.
        assert_eq!(
            out.matches("## Options — ").count(),
            1,
            "one composed block per clarify bead: {out}"
        );
        assert!(out.starts_with("## Options — "), "options heading: {out}");

        // One primary per present kind, then the mixed escape hatch, in
        // sequential N order matching the spec's kind ordering.
        assert!(out.contains("### Option 1 — Implement"), "kind 1: {out}");
        assert!(
            out.contains("### Option 2 — Implement the stub"),
            "kind 2: {out}"
        );
        assert!(out.contains("### Option 3 — Drop the `?`"), "kind 3: {out}");
        assert!(
            out.contains("### Option 4 — Mixed resolution via `msg -c` chat"),
            "mixed escape hatch closes the block: {out}"
        );

        // Each option is scoped to the affected location(s).
        assert!(out.contains("specs/harness.md:42"), "unresolved loc: {out}");
        assert!(out.contains("specs/gate.md:100"), "stub loc: {out}");
        assert!(out.contains("specs/templates.md:88"), "pending loc: {out}");

        assert!(
            loom_protocol::gate::options::has_well_formed_block(&out),
            "composed block satisfies the Options Format Contract: {out}"
        );
    }

    #[test]
    fn compose_clarify_options_numbers_only_present_kinds() {
        let findings = vec![IntegrityFinding::UnneededPendingMarker {
            spec: PathBuf::from("specs/harness.md"),
            line: 88,
            tier: Tier::Test,
            target: "crate::a::landed".into(),
        }];
        let out = compose_clarify_options(&findings);
        // Only the pending kind is present, so its primary is Option 1 and
        // the mixed escape hatch is Option 2 — no gaps for absent kinds.
        assert!(
            out.contains("### Option 1 — Drop the `?`"),
            "pending primary: {out}"
        );
        assert!(
            out.contains("### Option 2 — Mixed resolution via `msg -c` chat"),
            "mixed is option 2: {out}"
        );
        assert!(
            !out.contains("### Option 3"),
            "no gap for absent kinds: {out}"
        );
        assert!(out.contains("specs/harness.md:88"), "spec:line: {out}");
    }

    #[test]
    fn compose_clarify_options_skips_non_terminal_findings() {
        let f = IntegrityFinding::MultipleAnnotations {
            spec: PathBuf::from("specs/a.md"),
            line: 1,
            count: 2,
        };
        let out = compose_clarify_options(&[f]);
        assert!(
            out.is_empty(),
            "non-terminal variants produce no options block: {out:?}"
        );
    }

    #[test]
    fn to_finding_maps_terminal_variants_to_typed_findings() {
        let cases = [
            (
                IntegrityFinding::UnresolvedAnnotation {
                    spec: PathBuf::from("specs/harness.md"),
                    line: 42,
                    tier: Tier::Check,
                    target: "missing-runner".into(),
                },
                ConcernToken::UnresolvedAnnotation,
            ),
            (
                IntegrityFinding::StubTestFunction {
                    spec: PathBuf::from("specs/gate.md"),
                    line: 100,
                    tier: Tier::Test,
                    target: "crate::a::stub_me".into(),
                    test_name: "stub_me".into(),
                },
                ConcernToken::StubPointing,
            ),
            (
                IntegrityFinding::UnneededPendingMarker {
                    spec: PathBuf::from("specs/templates.md"),
                    line: 88,
                    tier: Tier::Test,
                    target: "crate::a::landed".into(),
                },
                ConcernToken::UnneededPendingMarker,
            ),
        ];
        for (finding, expected_token) in cases {
            let mapped = finding.to_finding().expect("terminal finding maps");
            assert_eq!(mapped.token, expected_token, "token for {finding:?}");
            assert!(
                matches!(mapped.target, FindingTarget::Annotation { .. }),
                "target is Annotation for {finding:?}",
            );
            assert_eq!(mapped.bonds.len(), 1, "single lead-spec bond: {finding:?}");
            assert_eq!(
                mapped.evidence,
                finding.to_string(),
                "evidence is the finding's Display: {finding:?}",
            );
        }
    }

    #[test]
    fn to_finding_derives_lead_spec_from_path_stem() {
        let f = IntegrityFinding::UnresolvedAnnotation {
            spec: PathBuf::from("specs/templates.md"),
            line: 1,
            tier: Tier::Check,
            target: "x".into(),
        };
        let mapped = f.to_finding().expect("terminal finding maps");
        assert_eq!(mapped.bonds[0].as_str(), "templates");
        assert_eq!(
            mapped.target,
            FindingTarget::Annotation {
                target_string: "x".into(),
            },
        );
    }

    #[test]
    fn to_finding_returns_none_for_non_terminal_variants() {
        let multi = IntegrityFinding::MultipleAnnotations {
            spec: PathBuf::from("specs/a.md"),
            line: 1,
            count: 2,
        };
        assert!(multi.to_finding().is_none(), "non-terminal maps to None");
        let cargo = IntegrityFinding::UnresolvedCargoTestName {
            spec: PathBuf::from("specs/a.md"),
            line: 1,
            target: "cargo test --lib x".into(),
            test_name: "x".into(),
        };
        assert!(cargo.to_finding().is_none(), "non-terminal maps to None");
    }

    #[test]
    fn is_missing_binary_target_flags_bare_binary_not_on_path() {
        let cmds = StubCommands::with(&["cargo"]);
        assert!(
            is_missing_binary_target("cargooo run -p foo", &cmds),
            "bare binary name missing from PATH counts as skip-if-missing"
        );
    }

    #[test]
    fn is_missing_binary_target_passes_when_bare_binary_resolves() {
        let cmds = StubCommands::with(&["cargo"]);
        assert!(
            !is_missing_binary_target("cargo run -p foo", &cmds),
            "resolved bare binary is not missing"
        );
    }

    #[test]
    fn is_missing_binary_target_rejects_path_shaped_first_token() {
        let cmds = StubCommands::with(&[]);
        assert!(
            !is_missing_binary_target("scripts/walk.sh foo", &cmds),
            "first token with `/` names a file; missing path is a real finding"
        );
        assert!(
            !is_missing_binary_target("/usr/local/bin/foo", &cmds),
            "absolute path is not a skip-if-missing candidate"
        );
    }

    #[test]
    fn is_missing_binary_target_rejects_empty_target() {
        let cmds = StubCommands::with(&[]);
        assert!(!is_missing_binary_target("", &cmds));
        assert!(!is_missing_binary_target("   ", &cmds));
    }
}
