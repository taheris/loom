//! Typed `previous_failure` retry context.
//!
//! `PreviousFailure` is the tagged-enum surface that the driver populates from
//! the verdict-gate cause classification and that `run.md` renders into the
//! next agent attempt's prompt. The enum + its sub-types (`DriverNoticeCause`,
//! `ReviewConcernKind`, `VerifierFailure`) are part of the `templates`
//! public contract — consumers compose them into their own retry prompts.
//!
//! Caps follow `specs/templates.md` § Typed `PreviousFailure`:
//!
//! - Total rendered body capped at [`PREVIOUS_FAILURE_MAX_LEN`] (4000 chars).
//! - Each [`VerifierFailure::stderr_tail`] capped per-block at
//!   [`STDERR_TAIL_PER_BLOCK`] (~1500 chars) before the per-variant total is
//!   split across failures; later failures truncate first when the total
//!   exceeds budget.

use std::fmt::{self, Display};

/// Maximum length of the rendered `previous_failure` body. The render path
/// truncates anything past this at a char boundary so multi-byte stderr does
/// not panic.
pub const PREVIOUS_FAILURE_MAX_LEN: usize = 4000;

/// Per-block cap on [`VerifierFailure::stderr_tail`] before the per-variant
/// budget split. Mirrors `specs/templates.md` § Typed `PreviousFailure`
/// ("Each `VerifierFailure.stderr_tail` capped individually (~1500 chars)").
pub const STDERR_TAIL_PER_BLOCK: usize = 1500;

/// Marker appended to a rendered failure body when truncation drops content.
const TRUNC_MARKER: &str = "[truncated]";

/// Typed retry context threaded into `run.md` via `LoopContext.previous_failure`.
/// Variants carry the cause-appropriate detail so the template can render each
/// with its documented framing (see [`Display`] impl).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviousFailure {
    /// Fixed-shape driver-procedural failure (no LLM-flagged content).
    DriverNotice {
        cause: DriverNoticeCause,
        detail: String,
    },
    /// One or more `[check]` / `[test]` / `[system]` verifier failures.
    VerifyFailures(Vec<VerifierFailure>),
    /// Review LLM flagged a semantic concern.
    ReviewConcern {
        concern: ReviewConcernKind,
        reason: String,
    },
    /// Pre-verifier build/compile failure (the agent's code did not compile).
    BuildFailure { stage: String, output: String },
    /// Worker emitted `LOOM_COMPLETE` / `LOOM_NOOP` but left the working tree
    /// dirty. `dirty_paths` is the already-capped list of dirty entries (the
    /// driver caps at 30 entries and appends a `"+N more"` marker as the
    /// final element when the underlying set was larger).
    TreeNotClean { dirty_paths: Vec<String> },
}

/// Driver-procedural failure causes that map to `DriverNotice`. Mirrors the
/// `RecoveryCause` variants the driver emits for non-LLM failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverNoticeCause {
    SwallowedMarker,
    IncompleteSignaling,
    ZeroProgress,
    ObserverAbort,
    RetryExhausted,
}

impl DriverNoticeCause {
    /// Stable spec-table label used in user-facing surfaces (logs, notes).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SwallowedMarker => "swallowed-marker",
            Self::IncompleteSignaling => "incomplete-signaling",
            Self::ZeroProgress => "zero-progress",
            Self::ObserverAbort => "observer-abort",
            Self::RetryExhausted => "retry-exhausted",
        }
    }
}

/// Concrete review-rubric concerns the reviewer can flag. Defined in
/// `specs/gate.md` § Per-diff stage checks; the `Other` arm keeps the
/// type forward-compatible when gate.md grows new flag causes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewConcernKind {
    SpecCoherence,
    OrphanIntegration,
    VerifierBypass,
    FabricatedResult,
    WeakAssertion,
    CoincidentalPass,
    MockDiscipline,
    VerifierTooNarrow,
    ConcurrencyUntested,
    ScopeCreep,
    ScopeShortfall,
    JudgeFlag,
    Other(String),
}

impl ReviewConcernKind {
    /// Token form used in `LOOM_CONCERN:` payloads, `bd update --notes`, and
    /// the rendered framing prefix.
    pub fn as_token(&self) -> &str {
        match self {
            Self::SpecCoherence => "spec-coherence",
            Self::OrphanIntegration => "orphan-integration",
            Self::VerifierBypass => "verifier-bypass",
            Self::FabricatedResult => "fabricated-result",
            Self::WeakAssertion => "weak-assertion",
            Self::CoincidentalPass => "coincidental-pass",
            Self::MockDiscipline => "mock-discipline",
            Self::VerifierTooNarrow => "verifier-too-narrow",
            Self::ConcurrencyUntested => "concurrency-untested",
            Self::ScopeCreep => "scope-creep",
            Self::ScopeShortfall => "scope-shortfall",
            Self::JudgeFlag => "judge-flag",
            Self::Other(s) => s.as_str(),
        }
    }
}

/// One failing verifier captured by the gate. `stderr_tail` is the tail of
/// the verifier's stderr stream, pre-capped at [`STDERR_TAIL_PER_BLOCK`] by
/// [`VerifierFailure::new`] so callers can hand it raw stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierFailure {
    pub target: String,
    pub exit_code: i32,
    pub stderr_tail: String,
}

impl VerifierFailure {
    /// Construct a `VerifierFailure`, capping `stderr_tail` to
    /// [`STDERR_TAIL_PER_BLOCK`] chars at a char boundary.
    pub fn new(target: impl Into<String>, exit_code: i32, stderr_tail: impl Into<String>) -> Self {
        let mut stderr_tail: String = stderr_tail.into();
        truncate_at_char_boundary(&mut stderr_tail, STDERR_TAIL_PER_BLOCK);
        Self {
            target: target.into(),
            exit_code,
            stderr_tail,
        }
    }
}

impl PreviousFailure {
    /// Wrap an opaque error string into a `PreviousFailure`. Used at the seam
    /// between the run loop's untyped `AgentOutcome::Failure { error }` body
    /// and the typed retry context — the agent error becomes a `BuildFailure`
    /// with `stage = "agent"` so the next prompt still gets framing.
    pub fn from_agent_error(error: impl Into<String>) -> Self {
        Self::BuildFailure {
            stage: "agent".to_string(),
            output: error.into(),
        }
    }
}

impl Display for PreviousFailure {
    /// Render the variant with its documented framing, then truncate the full
    /// body to [`PREVIOUS_FAILURE_MAX_LEN`] at a char boundary. The template
    /// prints this via `{{ failure }}` so the framing rides through askama
    /// without per-template logic.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut body = render_body(self);
        truncate_at_char_boundary_with_marker(&mut body, PREVIOUS_FAILURE_MAX_LEN);
        f.write_str(&body)
    }
}

fn render_body(failure: &PreviousFailure) -> String {
    match failure {
        PreviousFailure::DriverNotice { detail, .. } => {
            format!("Previous attempt: {detail}")
        }
        PreviousFailure::VerifyFailures(failures) => render_verify_failures(failures),
        PreviousFailure::ReviewConcern { concern, reason } => {
            format!(
                "Review raised a concern ({token}): {reason}",
                token = concern.as_token(),
            )
        }
        PreviousFailure::BuildFailure { stage, output } => {
            format!("Build failed at {stage}:\n{output}")
        }
        PreviousFailure::TreeNotClean { dirty_paths } => render_tree_not_clean(dirty_paths),
    }
}

fn render_tree_not_clean(dirty_paths: &[String]) -> String {
    let mut out = String::from("Working tree was not clean after the bead committed:\n\n");
    for path in dirty_paths {
        out.push_str(path);
        out.push('\n');
    }
    out.push_str("\nStage these into a follow-up commit or revert them.");
    out
}

fn render_verify_failures(failures: &[VerifierFailure]) -> String {
    let mut out = String::from("Verifier failures from previous attempt:\n\n");
    // Greedy left-to-right fill within PREVIOUS_FAILURE_MAX_LEN minus the
    // heading — later failures truncate first when the budget runs out, with
    // a marker noting how many were dropped.
    let budget = PREVIOUS_FAILURE_MAX_LEN.saturating_sub(out.len());
    let mut remaining = budget;
    let mut included = 0usize;
    for failure in failures {
        let block = format_verifier_block(failure);
        if block.len() <= remaining {
            out.push_str(&block);
            remaining -= block.len();
            included += 1;
            continue;
        }
        let marker_with_nl = format!("{TRUNC_MARKER}\n");
        if remaining > marker_with_nl.len() {
            let allowance = remaining - marker_with_nl.len();
            let cut = floor_char_boundary(&block, allowance);
            out.push_str(&block[..cut]);
            out.push_str(&marker_with_nl);
            included += 1;
        }
        break;
    }
    let omitted = failures.len() - included;
    if omitted > 0 {
        out.push_str(&format!("[+{omitted} more verify failure(s) omitted]\n",));
    }
    out
}

fn format_verifier_block(failure: &VerifierFailure) -> String {
    format!(
        "── {target} (exit {exit}) ──\n{tail}\n\n",
        target = failure.target,
        exit = failure.exit_code,
        tail = failure.stderr_tail.trim_end_matches('\n'),
    )
}

fn truncate_at_char_boundary(s: &mut String, max: usize) {
    if s.len() > max {
        let cut = floor_char_boundary(s, max);
        s.truncate(cut);
    }
}

fn truncate_at_char_boundary_with_marker(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let marker = format!("\n{TRUNC_MARKER}");
    if max <= marker.len() {
        let cut = floor_char_boundary(s, max);
        s.truncate(cut);
        return;
    }
    let allowance = max - marker.len();
    let cut = floor_char_boundary(s, allowance);
    s.truncate(cut);
    s.push_str(&marker);
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_notice_renders_with_previous_attempt_prefix() {
        let pf = PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::SwallowedMarker,
            detail: "Last phase ended without a `LOOM_*` exit marker.".into(),
        };
        let rendered = pf.to_string();
        assert!(
            rendered.starts_with("Previous attempt: "),
            "framing prefix missing: {rendered}",
        );
        assert!(
            rendered.contains("Last phase ended without"),
            "detail missing: {rendered}",
        );
    }

    #[test]
    fn verify_failures_render_with_collective_prefix() {
        let pf = PreviousFailure::VerifyFailures(vec![VerifierFailure::new(
            "tests/sample.sh",
            1,
            "boom\n",
        )]);
        let rendered = pf.to_string();
        assert!(
            rendered.starts_with("Verifier failures from previous attempt:"),
            "framing prefix missing: {rendered}",
        );
        assert!(
            rendered.contains("tests/sample.sh"),
            "target missing: {rendered}",
        );
        assert!(rendered.contains("exit 1"), "exit code missing: {rendered}");
        assert!(rendered.contains("boom"), "stderr tail missing: {rendered}");
    }

    #[test]
    fn review_concern_renders_with_concern_token_in_parens() {
        let pf = PreviousFailure::ReviewConcern {
            concern: ReviewConcernKind::VerifierBypass,
            reason: "test mocks the agent backend".into(),
        };
        let rendered = pf.to_string();
        assert!(
            rendered.starts_with("Review raised a concern (verifier-bypass):"),
            "framing prefix missing token: {rendered}",
        );
        assert!(
            rendered.contains("test mocks the agent backend"),
            "reason missing: {rendered}",
        );
    }

    #[test]
    fn build_failure_renders_with_stage_prefix() {
        let pf = PreviousFailure::BuildFailure {
            stage: "cargo check".into(),
            output: "error[E0382]: borrow of moved value".into(),
        };
        let rendered = pf.to_string();
        assert!(
            rendered.starts_with("Build failed at cargo check:\n"),
            "framing prefix missing: {rendered}",
        );
        assert!(
            rendered.contains("E0382"),
            "compiler output missing: {rendered}",
        );
    }

    #[test]
    fn previous_failure_variant_framings_match_spec() {
        // Pin every variant's framing string in one shot so a future refactor
        // cannot silently shift a prefix.
        let driver = PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::IncompleteSignaling,
            detail: "x".into(),
        }
        .to_string();
        assert!(driver.starts_with("Previous attempt: "), "{driver}");

        let verify =
            PreviousFailure::VerifyFailures(vec![VerifierFailure::new("t", 1, "y")]).to_string();
        assert!(
            verify.starts_with("Verifier failures from previous attempt:"),
            "{verify}",
        );

        let review = PreviousFailure::ReviewConcern {
            concern: ReviewConcernKind::JudgeFlag,
            reason: "z".into(),
        }
        .to_string();
        assert!(
            review.starts_with("Review raised a concern (judge-flag): "),
            "{review}",
        );

        let build = PreviousFailure::BuildFailure {
            stage: "link".into(),
            output: "out".into(),
        }
        .to_string();
        assert!(build.starts_with("Build failed at link:\n"), "{build}");

        let tree = PreviousFailure::TreeNotClean {
            dirty_paths: vec!["src/lib.rs".into()],
        }
        .to_string();
        assert!(
            tree.starts_with("Working tree was not clean after the bead committed:\n\n"),
            "{tree}",
        );
    }

    #[test]
    fn tree_not_clean_renders_path_list_one_per_line() {
        let pf = PreviousFailure::TreeNotClean {
            dirty_paths: vec![
                "src/lib.rs".into(),
                "crates/loom-templates/src/previous_failure.rs".into(),
                "docs/style-rules.md".into(),
            ],
        };
        let rendered = pf.to_string();
        assert!(
            rendered.starts_with("Working tree was not clean after the bead committed:\n\n"),
            "framing prefix missing: {rendered}",
        );
        assert!(
            rendered.contains(
                "src/lib.rs\ncrates/loom-templates/src/previous_failure.rs\ndocs/style-rules.md\n"
            ),
            "paths not rendered one-per-line: {rendered}",
        );
        assert!(
            rendered.ends_with("\nStage these into a follow-up commit or revert them."),
            "closing instruction missing: {rendered}",
        );
    }

    #[test]
    fn tree_not_clean_renders_path_list_with_truncation_suffix() {
        let pf = PreviousFailure::TreeNotClean {
            dirty_paths: vec![
                "src/a.rs".into(),
                "src/b.rs".into(),
                "src/c.rs".into(),
                "+27 more".into(),
            ],
        };
        let rendered = pf.to_string();
        assert!(
            rendered.contains("\n+27 more\n"),
            "+N more suffix line missing: {rendered}",
        );
        let stage_idx = rendered
            .find("Stage these into a follow-up commit")
            .expect("closing instruction present");
        let suffix_idx = rendered.find("+27 more").expect("suffix present");
        assert!(
            suffix_idx < stage_idx,
            "+N more must precede the closing instruction: {rendered}",
        );
    }

    #[test]
    fn rendered_body_is_capped_at_previous_failure_max_len() {
        let huge = "x".repeat(PREVIOUS_FAILURE_MAX_LEN * 2);
        let pf = PreviousFailure::BuildFailure {
            stage: "cargo".into(),
            output: huge,
        };
        let rendered = pf.to_string();
        assert!(
            rendered.len() <= PREVIOUS_FAILURE_MAX_LEN,
            "rendered length {} exceeds cap {PREVIOUS_FAILURE_MAX_LEN}",
            rendered.len(),
        );
    }

    #[test]
    fn rendered_body_truncation_does_not_split_multibyte_codepoints() {
        // Build an output whose truncation point lands inside a multi-byte char.
        let detail = format!(
            "{}🦀{}",
            "x".repeat(PREVIOUS_FAILURE_MAX_LEN),
            "y".repeat(50),
        );
        let pf = PreviousFailure::BuildFailure {
            stage: "cargo".into(),
            output: detail,
        };
        let _ = pf.to_string(); // must not panic
    }

    #[test]
    fn verifier_failure_stderr_tail_capped_per_block() {
        let big = "x".repeat(STDERR_TAIL_PER_BLOCK * 3);
        let vf = VerifierFailure::new("tests/big.sh", 1, big);
        assert!(
            vf.stderr_tail.len() <= STDERR_TAIL_PER_BLOCK,
            "stderr_tail {} exceeds STDERR_TAIL_PER_BLOCK={STDERR_TAIL_PER_BLOCK}",
            vf.stderr_tail.len(),
        );
    }

    #[test]
    fn verify_failures_split_budget_truncates_later_first() {
        // Each block is ~STDERR_TAIL_PER_BLOCK + framing overhead ~ 1530 chars;
        // three of them blow PREVIOUS_FAILURE_MAX_LEN (4000) by ~600 chars.
        let big = "x".repeat(STDERR_TAIL_PER_BLOCK);
        let failures = vec![
            VerifierFailure::new("tests/a.sh", 1, big.clone()),
            VerifierFailure::new("tests/b.sh", 2, big.clone()),
            VerifierFailure::new("tests/c.sh", 3, big),
        ];
        let pf = PreviousFailure::VerifyFailures(failures);
        let body = pf.to_string();
        assert!(
            body.len() <= PREVIOUS_FAILURE_MAX_LEN,
            "body {} exceeds cap {PREVIOUS_FAILURE_MAX_LEN}",
            body.len(),
        );
        assert!(
            body.contains("tests/a.sh"),
            "first block fully included: {body}",
        );
        // Later failures must signal cut (either inline truncation or omitted count).
        assert!(
            body.contains(TRUNC_MARKER) || body.contains("omitted"),
            "later failures must signal truncation: tail=…{tail}",
            tail = body
                .rsplit_once('\n')
                .map(|(_, t)| t)
                .unwrap_or(body.as_str()),
        );
    }

    #[test]
    fn driver_notice_cause_labels_match_spec_strings() {
        assert_eq!(
            DriverNoticeCause::SwallowedMarker.as_str(),
            "swallowed-marker"
        );
        assert_eq!(
            DriverNoticeCause::IncompleteSignaling.as_str(),
            "incomplete-signaling",
        );
        assert_eq!(DriverNoticeCause::ZeroProgress.as_str(), "zero-progress");
        assert_eq!(DriverNoticeCause::ObserverAbort.as_str(), "observer-abort");
        assert_eq!(
            DriverNoticeCause::RetryExhausted.as_str(),
            "retry-exhausted"
        );
    }

    #[test]
    fn review_concern_kind_tokens_match_spec_vocabulary() {
        assert_eq!(
            ReviewConcernKind::SpecCoherence.as_token(),
            "spec-coherence"
        );
        assert_eq!(
            ReviewConcernKind::OrphanIntegration.as_token(),
            "orphan-integration",
        );
        assert_eq!(
            ReviewConcernKind::VerifierBypass.as_token(),
            "verifier-bypass",
        );
        assert_eq!(
            ReviewConcernKind::FabricatedResult.as_token(),
            "fabricated-result",
        );
        assert_eq!(
            ReviewConcernKind::WeakAssertion.as_token(),
            "weak-assertion"
        );
        assert_eq!(
            ReviewConcernKind::CoincidentalPass.as_token(),
            "coincidental-pass",
        );
        assert_eq!(
            ReviewConcernKind::MockDiscipline.as_token(),
            "mock-discipline",
        );
        assert_eq!(
            ReviewConcernKind::VerifierTooNarrow.as_token(),
            "verifier-too-narrow",
        );
        assert_eq!(
            ReviewConcernKind::ConcurrencyUntested.as_token(),
            "concurrency-untested",
        );
        assert_eq!(ReviewConcernKind::ScopeCreep.as_token(), "scope-creep");
        assert_eq!(
            ReviewConcernKind::ScopeShortfall.as_token(),
            "scope-shortfall",
        );
        assert_eq!(ReviewConcernKind::JudgeFlag.as_token(), "judge-flag");
        let other = ReviewConcernKind::Other("brand-new-rule".into());
        assert_eq!(other.as_token(), "brand-new-rule");
    }

    #[test]
    fn from_agent_error_wraps_into_build_failure() {
        let pf = PreviousFailure::from_agent_error("boom");
        let PreviousFailure::BuildFailure { stage, output } = &pf else {
            panic!("expected BuildFailure, got {pf:?}");
        };
        assert_eq!(stage, "agent");
        assert_eq!(output, "boom");
    }
}
