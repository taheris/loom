//! Typed `previous_failure` retry context.
//!
//! `PreviousFailure` is the tagged-enum surface that the driver populates from
//! the verdict-gate cause classification and that `loop.md` renders into the
//! next agent attempt's prompt. The enum + its sub-types (`DriverNoticeCause`,
//! `BadWalk`, `VerifierFailure`) are part of the `templates` public contract —
//! consumers compose them into their own retry prompts. The per-finding
//! `Finding` record carried inside [`PreviousFailure::ReviewConcern`] is
//! spec-owned by `loom-workflow` (per `specs/gate.md` § Findings and Minting)
//! and re-exported from this crate to thread it through the typed
//! retry-context surface.
//!
//! Caps follow `specs/templates.md` § Typed `PreviousFailure`:
//!
//! - Total rendered body capped at [`PREVIOUS_FAILURE_MAX_LEN`] (4000 chars).
//! - Each [`VerifierFailure::stderr_tail`] capped per-block at
//!   [`STDERR_TAIL_PER_BLOCK`] (~1500 chars) before the per-variant total is
//!   split across failures; later failures truncate first when the total
//!   exceeds budget.

use std::fmt::{self, Display};
use std::path::PathBuf;

use crate::finding::Finding;

pub use loom_protocol::gate::{BadWalk, TerminalSurface};
pub use loom_protocol::oid::GitOid;

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

/// Typed retry context threaded into `loop.md` via `LoopContext.previous_failure`.
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
    /// Review LLM flagged one or more concerns. `summary` is the parsed
    /// `summary` field from the terminal `LOOM_CONCERN: {"summary": "..."}`
    /// marker; `findings` is the buffered list of streamed `LOOM_FINDING:`
    /// records (typed [`Finding`] per `specs/gate.md` § Findings and Minting).
    ReviewConcern {
        summary: String,
        findings: Vec<Finding>,
    },
    /// Review walk's terminal signal was malformed or mismatched with the
    /// streamed-findings count. Per-variant recovery-prompt framing lives on
    /// [`Display`].
    BadWalk(BadWalk),
    /// Pre-verifier build/compile failure (the agent's code did not compile).
    BuildFailure { stage: String, output: String },
    /// Worker emitted `LOOM_COMPLETE` / `LOOM_NOOP` but left the working tree
    /// dirty. `dirty_paths` is the already-capped list of dirty entries (the
    /// driver caps at 30 entries and appends a `"+N more"` marker as the
    /// final element when the underlying set was larger).
    TreeNotClean { dirty_paths: Vec<String> },
    /// Bead-workspace verify passed, but the loom-workspace per-bead
    /// integration step's `loom gate verify` against the integrated tree
    /// failed (cross-bead interaction, rebase-induced breakage). The
    /// integration was rolled back via `git reset --hard HEAD~1`. Per-bead
    /// does not run `loom gate review`, so review concerns are not a
    /// possible cause here.
    PostIntegrateFail { failures: Vec<VerifierFailure> },
    /// The driver-side rebase of the bead branch onto the integration
    /// branch hit a textual conflict that `git rerere` could not replay,
    /// so the rebase was aborted (`git rebase --abort`) and the loom
    /// workspace returned to its pre-rebase state. The bead's commits are
    /// intact on its own branch; the agent's next (single) retry must
    /// rebase its bead-workspace branch onto the new integration tip
    /// (`new_base_sha`), resolve the conflicting `files`, and re-commit.
    /// A second rebase-conflict on the retry escalates to `loom:clarify`
    /// (no `PreviousFailure` — the clarify path carries a synthesized
    /// Options block instead). Per `specs/harness.md` § Verdict Gate.
    IntegrationConflict {
        files: Vec<PathBuf>,
        new_base_sha: GitOid,
    },
    /// Worker phase emitted `LOOM_RETRY` — the agent self-reported that this
    /// attempt could not finish but a fresh dispatch is likely to succeed.
    /// `reason` is the prose the agent wrote on the line preceding the
    /// marker, captured verbatim. Consumes one slot in `[loop] max_retries`;
    /// exhaustion escalates to `loom:blocked` with cause `retry-exhausted`.
    AgentRetry { reason: String },
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
    UnbondedOrigin,
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
            Self::UnbondedOrigin => "unbonded-origin",
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
        PreviousFailure::ReviewConcern { summary, findings } => {
            render_review_concern(summary, findings)
        }
        PreviousFailure::BadWalk(badwalk) => render_bad_walk(badwalk),
        PreviousFailure::BuildFailure { stage, output } => {
            format!("Build failed at {stage}:\n{output}")
        }
        PreviousFailure::TreeNotClean { dirty_paths } => render_tree_not_clean(dirty_paths),
        PreviousFailure::PostIntegrateFail { failures } => render_post_integrate_fail(failures),
        PreviousFailure::IntegrationConflict {
            files,
            new_base_sha,
        } => render_integration_conflict(files, new_base_sha),
        PreviousFailure::AgentRetry { reason } => render_agent_retry(reason),
    }
}

fn render_agent_retry(reason: &str) -> String {
    format!(
        "Previous attempt requested retry — reason: {reason}\n\n\
         If the same problem persists after this attempt, escalate to LOOM_BLOCKED \
         (no candidate resolutions) or LOOM_CLARIFY (with a structured Options block) \
         rather than emitting LOOM_RETRY again.",
    )
}

fn render_integration_conflict(files: &[PathBuf], new_base_sha: &GitOid) -> String {
    let mut out = String::from(
        "Your bead branch could not be rebased onto the integration branch — \
         a textual conflict the driver could not replay aborted the rebase, \
         so the integration was left untouched.\n\n\
         Rebase your bead-workspace branch onto the new integration tip, \
         resolve the conflicts, and re-commit. New integration tip:\n",
    );
    out.push_str("  ");
    out.push_str(new_base_sha.as_str());
    out.push_str("\n\nConflicting files:\n");
    for file in files {
        out.push_str("  ");
        out.push_str(&file.display().to_string());
        out.push('\n');
    }
    out.push_str(
        "\nThis is the single integration-conflict retry — if the rebase \
         conflicts again, the bead escalates to loom:clarify for human \
         resolution.",
    );
    out
}

fn render_post_integrate_fail(failures: &[VerifierFailure]) -> String {
    let heading = "After rebasing onto the integration branch, \
                   the post-integration verify failed:\n\n";
    let footer = "\nReconcile the cross-bead interaction — your bead's verify passed \
                  at its own workspace; the failure is in the integrated tree.";
    let mut out = String::from(heading);
    // Reserve the footer + outer-truncation marker so the later-truncated-first
    // rule respects the same total cap as `VerifyFailures` even when many
    // failures stream through.
    let budget = PREVIOUS_FAILURE_MAX_LEN.saturating_sub(out.len() + footer.len());
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
        out.push_str(&format!("[+{omitted} more verify failure(s) omitted]\n"));
    }
    out.push_str(footer);
    out
}

fn render_review_concern(summary: &str, findings: &[Finding]) -> String {
    // Per `specs/gate.md` § *Findings and Minting*, the human-readable
    // concern label is derived from `findings[0].token` (or a
    // `multiple` label when the streamed tokens are heterogeneous), NOT
    // from the terminal `summary` payload. The summary still rides
    // through for the verdict log only.
    let label = concern_label_from_findings(findings);
    let mut out = format!("Review raised a concern ({label}): {summary}");
    for finding in findings {
        out.push_str("\n\n");
        out.push_str(finding.token.as_wire());
        out.push_str(" @ ");
        out.push_str(&finding.target.canonical_form());
        let evidence = finding.evidence.trim_end();
        if !evidence.is_empty() {
            out.push('\n');
            out.push_str(evidence);
        }
    }
    out
}

/// Derive a human-readable concern label from the streamed-findings
/// vec for `Display` of [`PreviousFailure::ReviewConcern`]. Returns the
/// finding token's wire string for the homogeneous case, `"multiple"`
/// for the heterogeneous case, and `"review-concern"` when no findings
/// streamed (the BadWalk path normally handles that, but the default is
/// safe). Per `specs/gate.md` § *Findings and Minting* — the human
/// label comes from `findings`, never from `summary`.
fn concern_label_from_findings(findings: &[Finding]) -> String {
    let Some(first) = findings.first() else {
        return "review-concern".to_owned();
    };
    if findings.iter().any(|f| f.token != first.token) {
        return "multiple".to_owned();
    }
    first.token.as_wire().to_owned()
}

fn render_bad_walk(badwalk: &BadWalk) -> String {
    match badwalk {
        BadWalk::Concern {
            payload,
            parsed_findings,
        } => {
            let mut out = format!(
                "Your LOOM_CONCERN payload did not parse as {{\"summary\": \"<non-empty>\"}}. \
                 Literal payload: {payload}",
            );
            let count = parsed_findings.len();
            if count > 0 {
                out.push_str(&format!(
                    "\n\n{count} finding(s) parsed cleanly before the malformed terminator:",
                ));
                for finding in parsed_findings {
                    append_finding_digest(&mut out, finding);
                }
            }
            out
        }
        BadWalk::ConcernWithoutFindings { summary } => format!(
            "You emitted LOOM_CONCERN ({summary}) but no LOOM_FINDING: lines streamed. \
             Either emit findings before the terminator or use LOOM_COMPLETE.",
        ),
        BadWalk::FindingsWithoutConcern {
            finding_count,
            findings,
        } => {
            let mut out = format!(
                "You streamed {finding_count} LOOM_FINDING line(s) but terminated with \
                 LOOM_COMPLETE. Use LOOM_CONCERN: {{\"summary\": \"...\"}} when findings are emitted.",
            );
            if !findings.is_empty() {
                out.push_str("\n\nParsed findings:");
                for finding in findings {
                    append_finding_digest(&mut out, finding);
                }
            }
            out
        }
        BadWalk::MalformedFinding { errors, terminal } => {
            let mut out = String::from(
                "One or more LOOM_FINDING: lines failed strict validation. \
                 Re-emit each finding as a single line: \
                 `LOOM_FINDING: {\"token\":\"...\",\"route\":\"blocking|deferred|clarify\",\"bonds\":[...],\"target\":{...},\"evidence\":\"...\"}`.",
            );
            for err in errors {
                out.push_str("\n\n");
                out.push_str(&err.to_string());
            }
            out.push_str(&format!("\n\nYour terminal was: {}", terminal.label()));
            if let TerminalSurface::Malformed { payload } = terminal {
                out.push_str(&format!(" — literal payload: {payload}"));
            }
            out
        }
    }
}

fn append_finding_digest(out: &mut String, finding: &Finding) {
    out.push_str("\n- ");
    out.push_str(finding.token.as_wire());
    out.push_str(" @ ");
    out.push_str(&finding.target.canonical_form());
    let evidence = finding.evidence.trim_end();
    if !evidence.is_empty() {
        out.push_str(" — ");
        out.push_str(evidence);
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
    use crate::finding::{ConcernToken, FindingParseError, FindingRoute, FindingTarget};
    use loom_events::identifier::SpecLabel;

    fn spec_label(s: &str) -> SpecLabel {
        s.parse().expect("valid spec label")
    }

    fn sample_finding(token: ConcernToken, evidence: &str) -> Finding {
        Finding {
            token,
            route: FindingRoute::Deferred,
            bonds: vec![spec_label("gate")],
            target: FindingTarget::Annotation {
                target_string: "cargo test --lib sample".into(),
            },
            evidence: evidence.to_owned(),
        }
    }

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
    fn review_concern_renders_with_summary_and_per_finding_block() {
        let pf = PreviousFailure::ReviewConcern {
            summary: "two findings".into(),
            findings: vec![
                sample_finding(ConcernToken::VerifierBypass, "test mocks the agent backend"),
                sample_finding(ConcernToken::WeakAssertion, "asserts only the prefix"),
            ],
        };
        let rendered = pf.to_string();
        // Per `specs/gate.md` § *Findings and Minting*, the human label
        // comes from `findings` (here heterogeneous → `multiple`), NOT
        // from `summary` — the summary still rides through for the
        // verdict-log surface.
        assert!(
            rendered.starts_with("Review raised a concern (multiple): two findings"),
            "label-prefixed framing missing: {rendered}",
        );
        assert!(
            rendered.contains("verifier-bypass @ annotation:cargo test --lib sample"),
            "first finding token+target missing: {rendered}",
        );
        assert!(
            rendered.contains("test mocks the agent backend"),
            "first finding evidence missing: {rendered}",
        );
        assert!(
            rendered.contains("weak-assertion @ annotation:cargo test --lib sample"),
            "second finding token+target missing: {rendered}",
        );
        assert!(
            rendered.contains("asserts only the prefix"),
            "second finding evidence missing: {rendered}",
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
            summary: "one finding".into(),
            findings: vec![sample_finding(ConcernToken::JudgeFlag, "judge said no")],
        }
        .to_string();
        assert!(
            review.starts_with("Review raised a concern (judge-flag): one finding"),
            "label-prefixed framing missing: {review}",
        );
        assert!(
            review.contains("judge-flag @ annotation:cargo test --lib sample"),
            "review finding token+target missing: {review}",
        );
        assert!(
            review.contains("judge said no"),
            "review finding evidence missing: {review}",
        );

        let bad_walk_concern = PreviousFailure::BadWalk(BadWalk::Concern {
            payload: "{not json".into(),
            parsed_findings: vec![],
        })
        .to_string();
        assert!(
            bad_walk_concern.starts_with("Your LOOM_CONCERN payload did not parse"),
            "{bad_walk_concern}",
        );

        let bad_walk_no_findings = PreviousFailure::BadWalk(BadWalk::ConcernWithoutFindings {
            summary: "claimed two".into(),
        })
        .to_string();
        assert!(
            bad_walk_no_findings.starts_with("You emitted LOOM_CONCERN (claimed two)"),
            "{bad_walk_no_findings}",
        );

        let bad_walk_no_concern = PreviousFailure::BadWalk(BadWalk::FindingsWithoutConcern {
            finding_count: 3,
            findings: vec![],
        })
        .to_string();
        assert!(
            bad_walk_no_concern.starts_with("You streamed 3 LOOM_FINDING line(s)"),
            "{bad_walk_no_concern}",
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

        let post_integrate = PreviousFailure::PostIntegrateFail {
            failures: vec![VerifierFailure::new("cargo test -p loom-gate", 1, "fail\n")],
        }
        .to_string();
        assert!(
            post_integrate.starts_with(
                "After rebasing onto the integration branch, the post-integration verify failed:"
            ),
            "{post_integrate}",
        );

        let agent_retry = PreviousFailure::AgentRetry {
            reason: "sandbox cwd unlinked".into(),
        }
        .to_string();
        assert!(
            agent_retry.starts_with("Previous attempt requested retry — reason: "),
            "{agent_retry}",
        );

        let integration_conflict = PreviousFailure::IntegrationConflict {
            files: vec![PathBuf::from("crates/loom-gate/src/marker.rs")],
            new_base_sha: GitOid::new("deadbeefcafe1234567890abcdef0123456789ab").expect("oid"),
        }
        .to_string();
        assert!(
            integration_conflict.starts_with("Your bead branch could not be rebased"),
            "{integration_conflict}",
        );
    }

    /// `Display` on `IntegrationConflict { files, new_base_sha }` names the
    /// new integration tip the agent must rebase onto, enumerates the
    /// conflicting files, and flags that this is the single
    /// integration-conflict retry before escalation to `loom:clarify`.
    /// Per `specs/harness.md` § Verdict Gate.
    #[test]
    fn integration_conflict_display_names_new_base_and_conflict_files() {
        let pf = PreviousFailure::IntegrationConflict {
            files: vec![
                PathBuf::from("crates/loom-gate/src/marker.rs"),
                PathBuf::from("crates/loom-templates/src/previous_failure.rs"),
            ],
            new_base_sha: GitOid::new("0123456789abcdef0123456789abcdef01234567").expect("oid"),
        };
        let rendered = pf.to_string();
        assert!(
            rendered.starts_with("Your bead branch could not be rebased"),
            "framing prefix missing: {rendered}",
        );
        assert!(
            rendered.contains("0123456789abcdef0123456789abcdef01234567"),
            "new integration tip missing: {rendered}",
        );
        assert!(
            rendered.contains("crates/loom-gate/src/marker.rs")
                && rendered.contains("crates/loom-templates/src/previous_failure.rs"),
            "conflicting files missing: {rendered}",
        );
        assert!(
            rendered.contains("single integration-conflict retry"),
            "one-retry framing missing: {rendered}",
        );
    }

    /// `Display` on `AgentRetry { reason }` surfaces the agent's verbatim
    /// `reason` and instructs the next attempt to escalate to
    /// `LOOM_BLOCKED` or `LOOM_CLARIFY` if the same problem persists,
    /// rather than emitting `LOOM_RETRY` again. Criterion:
    /// `agent_retry_display_renders_reason_and_escalation_guidance`.
    #[test]
    fn agent_retry_display_renders_reason_and_escalation_guidance() {
        let pf = PreviousFailure::AgentRetry {
            reason: "podman socket disappeared mid-session".into(),
        };
        let rendered = pf.to_string();
        assert!(
            rendered.starts_with("Previous attempt requested retry — reason: "),
            "framing prefix missing: {rendered}",
        );
        assert!(
            rendered.contains("podman socket disappeared mid-session"),
            "verbatim reason missing: {rendered}",
        );
        assert!(
            rendered.contains("LOOM_BLOCKED"),
            "escalation to LOOM_BLOCKED missing: {rendered}",
        );
        assert!(
            rendered.contains("LOOM_CLARIFY"),
            "escalation to LOOM_CLARIFY missing: {rendered}",
        );
    }

    /// `Display` on `PostIntegrateFail { failures }` enumerates each
    /// `VerifierFailure` block (target + exit + stderr_tail) and trails
    /// the cross-bead reconciliation hint so the agent knows the bead's
    /// own verify already passed at its workspace.
    #[test]
    fn post_integrate_fail_display_renders_blocks_and_reconciliation_hint() {
        let pf = PreviousFailure::PostIntegrateFail {
            failures: vec![
                VerifierFailure::new(
                    "cargo test -p loom-gate --lib mint::tests::ok",
                    101,
                    "panicked at 'index out of bounds'\n",
                ),
                VerifierFailure::new("tests/run-tests.sh", 1, "assertion failed\n"),
            ],
        };
        let rendered = pf.to_string();
        assert!(
            rendered.starts_with(
                "After rebasing onto the integration branch, the post-integration verify failed:"
            ),
            "framing prefix missing: {rendered}",
        );
        assert!(
            rendered.contains("cargo test -p loom-gate --lib mint::tests::ok"),
            "first target missing: {rendered}",
        );
        assert!(
            rendered.contains("exit 101"),
            "first exit missing: {rendered}"
        );
        assert!(
            rendered.contains("panicked at"),
            "first stderr missing: {rendered}",
        );
        assert!(
            rendered.contains("tests/run-tests.sh"),
            "second target missing: {rendered}",
        );
        assert!(
            rendered.contains("Reconcile the cross-bead interaction"),
            "reconciliation hint missing: {rendered}",
        );
        assert!(
            rendered.contains("your bead's verify passed"),
            "passed-at-own-workspace hint missing: {rendered}",
        );
    }

    /// `Display` on `BadWalk::Concern { parsed_findings: non-empty }`
    /// appends the per-finding digest (token + target + evidence) so the
    /// agent's diagnosis is not lost when only the terminator was
    /// malformed. Criterion:
    /// `bad_walk_concern_display_renders_parsed_findings_digest_when_present`.
    #[test]
    fn bad_walk_concern_display_renders_parsed_findings_digest_when_present() {
        let pf = PreviousFailure::BadWalk(BadWalk::Concern {
            payload: "{not valid json".into(),
            parsed_findings: vec![
                sample_finding(ConcernToken::VerifierBypass, "stubs the backend"),
                sample_finding(ConcernToken::JudgeFlag, "judge dissented"),
            ],
        });
        let rendered = pf.to_string();
        assert!(
            rendered.contains("2 finding(s) parsed cleanly before the malformed terminator:"),
            "digest preamble missing: {rendered}",
        );
        assert!(
            rendered.contains("verifier-bypass @ annotation:cargo test --lib sample"),
            "first finding digest missing: {rendered}",
        );
        assert!(
            rendered.contains("stubs the backend"),
            "first finding evidence missing: {rendered}",
        );
        assert!(
            rendered.contains("judge-flag @ annotation:cargo test --lib sample"),
            "second finding digest missing: {rendered}",
        );
    }

    /// `Display` on `BadWalk::FindingsWithoutConcern { findings: non-empty }`
    /// appends the per-finding digest so the agent's next iteration sees
    /// the diagnosis it just emitted even though the terminator was
    /// `LOOM_COMPLETE`. Criterion:
    /// `bad_walk_findings_without_concern_display_renders_findings_digest`.
    #[test]
    fn bad_walk_findings_without_concern_display_renders_findings_digest() {
        let findings = vec![
            sample_finding(ConcernToken::WeakAssertion, "asserts only prefix"),
            sample_finding(ConcernToken::JudgeFlag, "judge dissent"),
        ];
        let pf = PreviousFailure::BadWalk(BadWalk::FindingsWithoutConcern {
            finding_count: findings.len(),
            findings,
        });
        let rendered = pf.to_string();
        assert!(
            rendered.contains("2 LOOM_FINDING line(s)"),
            "count missing: {rendered}",
        );
        assert!(
            rendered.contains("weak-assertion @ annotation:cargo test --lib sample"),
            "first finding digest missing: {rendered}",
        );
        assert!(
            rendered.contains("asserts only prefix"),
            "first finding evidence missing: {rendered}",
        );
        assert!(
            rendered.contains("judge-flag @ annotation:cargo test --lib sample"),
            "second finding digest missing: {rendered}",
        );
    }

    /// `Display` on `BadWalk::MalformedFinding { errors, terminal }`
    /// enumerates per-line errors and surfaces the rendered terminal
    /// (`TerminalSurface::label()`) so the agent can fix the malformed
    /// lines without losing the well-formed surrounding context.
    /// Criterion:
    /// `bad_walk_malformed_finding_display_surfaces_terminal_and_per_line_errors`.
    #[test]
    fn bad_walk_malformed_finding_display_surfaces_terminal_and_per_line_errors() {
        let errors = vec![
            FindingParseError::Json {
                line_number: 4,
                raw: "`LOOM_FINDING: {oops}`".into(),
                message: "expected value at line 1 column 1".into(),
            },
            FindingParseError::Json {
                line_number: 9,
                raw: "LOOM_FINDING: {still bad}".into(),
                message: "expected ident at line 1 column 2".into(),
            },
        ];
        let pf = PreviousFailure::BadWalk(BadWalk::MalformedFinding {
            errors,
            terminal: TerminalSurface::Concern {
                summary: "two findings".into(),
            },
        });
        let rendered = pf.to_string();
        assert!(
            rendered.contains("\"route\":\"blocking|deferred|clarify\""),
            "malformed-finding guidance must include route field: {rendered}",
        );
        assert!(
            rendered.contains("line 4"),
            "first error line-number missing: {rendered}",
        );
        assert!(
            rendered.contains("line 9"),
            "second error line-number missing: {rendered}",
        );
        assert!(
            rendered.contains("Your terminal was: "),
            "terminal label prefix missing: {rendered}",
        );
        assert!(
            rendered.contains(
                TerminalSurface::Concern {
                    summary: String::new()
                }
                .label()
            ),
            "rendered terminal label missing: {rendered}",
        );
    }

    #[test]
    fn bad_walk_concern_renders_with_literal_payload() {
        let pf = PreviousFailure::BadWalk(BadWalk::Concern {
            payload: "{\"summery\": \"typo\"}".into(),
            parsed_findings: vec![],
        });
        let rendered = pf.to_string();
        assert!(
            rendered.contains("{\"summery\": \"typo\"}"),
            "literal payload missing: {rendered}",
        );
    }

    /// `BadWalk::Concern { parsed_findings, .. }` carries any findings
    /// that streamed cleanly before the malformed terminator — per
    /// `specs/gate.md` § *Maximum-context preservation invariant* —
    /// and `Display` surfaces them in the recovery prompt so the agent
    /// keeps the work even though the terminator was malformed.
    /// Criterion: `bad_walk_concern_preserves_well_formed_findings_alongside_malformed_payload`.
    #[test]
    fn bad_walk_concern_preserves_well_formed_findings_alongside_malformed_payload() {
        let findings = vec![
            sample_finding(ConcernToken::VerifierBypass, "test mocks the backend"),
            sample_finding(ConcernToken::WeakAssertion, "asserts only prefix"),
        ];
        let pf = PreviousFailure::BadWalk(BadWalk::Concern {
            payload: "{not json".into(),
            parsed_findings: findings,
        });
        let rendered = pf.to_string();
        assert!(
            rendered.contains("2 finding(s) parsed cleanly"),
            "digest preamble missing: {rendered}",
        );
        assert!(
            rendered.contains("verifier-bypass @ annotation:cargo test --lib sample"),
            "first finding digest missing: {rendered}",
        );
        assert!(
            rendered.contains("weak-assertion @ annotation:cargo test --lib sample"),
            "second finding digest missing: {rendered}",
        );
        assert!(
            rendered.contains("test mocks the backend"),
            "first finding evidence missing: {rendered}",
        );
    }

    /// `BadWalk::FindingsWithoutConcern { findings, .. }` carries the
    /// parsed Findings vec — per `specs/gate.md` § *Maximum-context
    /// preservation invariant* — so the next iteration's prompt names
    /// each finding even though the terminator was `LOOM_COMPLETE`.
    /// Criterion: `bad_walk_findings_without_concern_carries_parsed_findings_vec`.
    #[test]
    fn bad_walk_findings_without_concern_carries_parsed_findings_vec() {
        let findings = vec![sample_finding(ConcernToken::JudgeFlag, "judge said no")];
        let pf = PreviousFailure::BadWalk(BadWalk::FindingsWithoutConcern {
            finding_count: findings.len(),
            findings,
        });
        let rendered = pf.to_string();
        assert!(
            rendered.contains("1 LOOM_FINDING line(s)"),
            "count missing: {rendered}",
        );
        assert!(
            rendered.contains("judge-flag @ annotation:cargo test --lib sample"),
            "per-finding digest missing: {rendered}",
        );
        assert!(
            rendered.contains("judge said no"),
            "evidence missing: {rendered}",
        );
    }

    /// `BadWalk::MalformedFinding { errors, terminal }` carries the
    /// per-line errors *and* the typed terminal surface so the recovery
    /// prompt can name both pieces — per `specs/gate.md` § *Maximum-
    /// context preservation invariant*. Criteria:
    /// `bad_walk_malformed_finding_variant_carries_errors_and_terminal_by_struct_shape`
    /// and `backtick_wrapped_loom_finding_line_routes_to_bad_walk_malformed_finding_with_terminal_preserved`.
    #[test]
    fn bad_walk_malformed_finding_variant_carries_errors_and_terminal_by_struct_shape() {
        let errors = vec![FindingParseError::Json {
            line_number: 3,
            raw: "`LOOM_FINDING: {not json}`".into(),
            message: "expected value at line 1 column 1".into(),
        }];
        let terminal = TerminalSurface::Complete;
        let pf = PreviousFailure::BadWalk(BadWalk::MalformedFinding {
            errors: errors.clone(),
            terminal: terminal.clone(),
        });
        let rendered = pf.to_string();
        assert!(
            rendered.contains("LOOM_COMPLETE"),
            "terminal label missing: {rendered}",
        );
        assert!(
            rendered.contains("line 3"),
            "error line-number missing: {rendered}",
        );
        assert!(
            rendered.contains("not valid JSON"),
            "error detail missing: {rendered}",
        );
        // Struct-shape pin: the variant exists with these fields.
        match pf {
            PreviousFailure::BadWalk(BadWalk::MalformedFinding {
                errors: e,
                terminal: t,
            }) => {
                assert_eq!(e, errors);
                assert_eq!(t, terminal);
            }
            other => panic!("expected BadWalk::MalformedFinding, got {other:?}"),
        }
    }

    /// When the terminator itself failed to parse alongside the
    /// findings, `BadWalk::MalformedFinding` carries the literal
    /// terminal payload via `TerminalSurface::Malformed { payload }`.
    /// The Display surfaces both the per-finding errors and the
    /// malformed payload so the agent can fix both on retry.
    #[test]
    fn bad_walk_malformed_finding_with_malformed_terminal_renders_payload() {
        let errors = vec![FindingParseError::Json {
            line_number: 2,
            raw: "LOOM_FINDING: garbage".into(),
            message: "expected value".into(),
        }];
        let terminal = TerminalSurface::Malformed {
            payload: "{\"summery\": \"typo\"}".into(),
        };
        let pf = PreviousFailure::BadWalk(BadWalk::MalformedFinding { errors, terminal });
        let rendered = pf.to_string();
        assert!(
            rendered.contains("LOOM_CONCERN (malformed payload)"),
            "malformed terminal label missing: {rendered}",
        );
        assert!(
            rendered.contains("{\"summery\": \"typo\"}"),
            "literal malformed payload missing: {rendered}",
        );
    }

    #[test]
    fn bad_walk_concern_without_findings_renders_with_summary() {
        let pf = PreviousFailure::BadWalk(BadWalk::ConcernWithoutFindings {
            summary: "drift across the rubric".into(),
        });
        let rendered = pf.to_string();
        assert!(
            rendered.contains("drift across the rubric"),
            "summary missing: {rendered}",
        );
        assert!(
            rendered.contains("LOOM_FINDING"),
            "guidance missing: {rendered}",
        );
    }

    #[test]
    fn bad_walk_findings_without_concern_renders_with_count() {
        let pf = PreviousFailure::BadWalk(BadWalk::FindingsWithoutConcern {
            finding_count: 5,
            findings: vec![],
        });
        let rendered = pf.to_string();
        assert!(
            rendered.contains("5 LOOM_FINDING"),
            "count missing: {rendered}",
        );
        assert!(
            rendered.contains("LOOM_COMPLETE"),
            "guidance missing: {rendered}",
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
        let detail = format!(
            "{}🦀{}",
            "x".repeat(PREVIOUS_FAILURE_MAX_LEN),
            "y".repeat(50),
        );
        let pf = PreviousFailure::BuildFailure {
            stage: "cargo".into(),
            output: detail,
        };
        let _ = pf.to_string();
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
        assert_eq!(
            DriverNoticeCause::UnbondedOrigin.as_str(),
            "unbonded-origin"
        );
    }

    #[test]
    fn previous_failure_renders_unbonded_origin_context_for_next_attempt() {
        let pf = PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::UnbondedOrigin,
            detail: "Originating bead lm-orphan.5 has no molecule parent; \
                     refusing to spawn fix-up bead."
                .into(),
        };
        let rendered = pf.to_string();
        assert!(
            rendered.starts_with("Previous attempt: "),
            "framing prefix missing: {rendered}",
        );
        assert!(
            rendered.contains("lm-orphan.5"),
            "origin detail missing: {rendered}",
        );
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
