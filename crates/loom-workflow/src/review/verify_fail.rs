//! Deterministic-tier verifier-failure aggregation for the verdict gate.
//!
//! After the verdict gate runs every deterministic-tier verifier
//! (`[check]` / `[test]` / `[system]`) attached to the bead's success
//! criteria (none short-circuit each other), the failures are
//! folded into a single `previous_failure` body that the recovery prompt
//! injects into the next agent attempt. Per `specs/harness.md` §"Recovery
//! context (`previous_failure`)" the body has a 4000-char budget, allocated
//! greedily left-to-right: earlier failures get their full block, later
//! failures get whatever remains, and once the budget is exhausted the rest
//! are dropped with a marker noting how many were truncated.
//!
//! When verify fails *and* review also flagged, the formatter appends the
//! review LLM's reasoning under a `Review notes:` heading with its own
//! [`REVIEW_NOTES_BUDGET`]-bounded slot — separate from the verify budget so
//! the agent gets verify failures *and* live-path/scope/judge feedback in one
//! prompt round trip (`specs/harness.md` §"Recovery context").
//!
//! Stderr is tailed to the last [`STDERR_TAIL_LINES`] lines per failure
//! before formatting — the full stream lives in the per-bead JSONL log.

use std::fmt::Write;
use std::path::PathBuf;

use super::phase_verdict::ReviewFlag;

/// 4000-char cap on the `previous_failure` body emitted for `verify-fail`
/// recovery (`specs/harness.md` table row "verify-fail").
pub const PREVIOUS_FAILURE_BUDGET: usize = 4000;

/// 1000-char cap on the appended `Review notes:` block. The spec calls this a
/// "separate budget, ~1000 chars" — separate from
/// [`PREVIOUS_FAILURE_BUDGET`] so review reasoning never crowds out the
/// mechanical failure detail (the cause label stays `verify-fail`).
pub const REVIEW_NOTES_BUDGET: usize = 1000;

/// "Last ~40 lines of stderr" per the spec table — we keep the most recent
/// lines because they hold the actual failure output, not the test setup.
pub const STDERR_TAIL_LINES: usize = 40;

/// One failing deterministic-tier verifier's outcome, as captured by
/// the gate's runner. Stderr is the raw stream; the formatter tails it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyFailure {
    pub script_path: PathBuf,
    pub exit_code: i32,
    pub stderr: String,
}

/// Format every failure into a single `previous_failure` body within
/// [`PREVIOUS_FAILURE_BUDGET`]. Earlier failures get their full block;
/// later failures truncate first when budget runs out. When `review_notes` is
/// `Some`, the review LLM's flag is appended under a `Review notes:` heading
/// inside its own [`REVIEW_NOTES_BUDGET`] (separate from the verify budget).
pub fn format_previous_failure(
    failures: &[VerifyFailure],
    review_notes: Option<&ReviewFlag>,
) -> String {
    let mut body = format_within_budget(failures, PREVIOUS_FAILURE_BUDGET);
    if let Some(flag) = review_notes {
        append_review_notes(&mut body, flag, REVIEW_NOTES_BUDGET);
    }
    body
}

fn format_within_budget(failures: &[VerifyFailure], budget: usize) -> String {
    let mut out = String::new();
    let mut remaining = budget;
    let mut included = 0usize;

    for failure in failures {
        let block = format_block(failure);
        if block.len() <= remaining {
            out.push_str(&block);
            remaining -= block.len();
            included += 1;
            continue;
        }
        // Truncate the block to whatever budget is left, leaving room for a
        // marker so the agent knows the tail was cut.
        const TRUNC_MARKER: &str = "[truncated]\n";
        if remaining > TRUNC_MARKER.len() {
            let allowance = remaining - TRUNC_MARKER.len();
            let cut = floor_char_boundary(&block, allowance);
            out.push_str(&block[..cut]);
            out.push_str(TRUNC_MARKER);
            included += 1;
        }
        break;
    }

    let omitted = failures.len() - included;
    if omitted > 0 {
        let _ = writeln!(out, "[+{omitted} more verify failure(s) omitted]");
    }
    out
}

const REVIEW_NOTES_HEADING: &str = "Review notes:\n";
const REVIEW_NOTES_TRUNC_MARKER: &str = "[truncated]\n";

/// Append the `Review notes:` block to `body`, sized to fit within `budget`
/// chars (heading + body inclusive). Truncates the detail char-boundary aware
/// when over budget; never panics on multi-byte stderr.
fn append_review_notes(body: &mut String, flag: &ReviewFlag, budget: usize) {
    if budget <= REVIEW_NOTES_HEADING.len() {
        return;
    }
    body.push_str(REVIEW_NOTES_HEADING);
    let mut remaining = budget - REVIEW_NOTES_HEADING.len();

    let line = format!("[{}] {}\n", flag.concern.as_str(), flag.detail);
    if line.len() <= remaining {
        body.push_str(&line);
        return;
    }

    if remaining > REVIEW_NOTES_TRUNC_MARKER.len() {
        remaining -= REVIEW_NOTES_TRUNC_MARKER.len();
        let cut = floor_char_boundary(&line, remaining);
        body.push_str(&line[..cut]);
        body.push_str(REVIEW_NOTES_TRUNC_MARKER);
    }
}

fn format_block(failure: &VerifyFailure) -> String {
    let tail = last_n_lines(&failure.stderr, STDERR_TAIL_LINES);
    format!(
        "── {} (exit {}) ──\n{}\n\n",
        failure.script_path.display(),
        failure.exit_code,
        tail.trim_end_matches('\n'),
    )
}

fn last_n_lines(s: &str, n: usize) -> &str {
    if n == 0 || s.is_empty() {
        return "";
    }
    let bytes = s.as_bytes();
    let end = bytes.len();
    // Skip a single trailing newline so the final line counts as a line.
    let mut search_end = if bytes[end - 1] == b'\n' {
        end - 1
    } else {
        end
    };
    let mut lines_seen = 0usize;
    while search_end > 0 {
        match bytes[..search_end].iter().rposition(|b| *b == b'\n') {
            Some(pos) => {
                lines_seen += 1;
                if lines_seen == n {
                    return &s[pos + 1..end];
                }
                search_end = pos;
            }
            None => break,
        }
    }
    &s[..end]
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
    use crate::review::phase_verdict::ReviewConcern;

    fn failure(path: &str, exit_code: i32, stderr: &str) -> VerifyFailure {
        VerifyFailure {
            script_path: PathBuf::from(path),
            exit_code,
            stderr: stderr.to_string(),
        }
    }

    fn flag(concern: ReviewConcern, detail: &str) -> ReviewFlag {
        ReviewFlag {
            concern,
            detail: detail.to_string(),
        }
    }

    #[test]
    fn empty_failures_returns_empty_string() {
        assert_eq!(format_previous_failure(&[], None), "");
    }

    #[test]
    fn single_failure_within_budget_includes_path_exit_code_and_stderr() {
        let f = failure("tests/a.sh", 1, "boom\n");
        let body = format_previous_failure(&[f], None);
        assert!(body.contains("tests/a.sh"), "{body}");
        assert!(body.contains("exit 1"), "{body}");
        assert!(body.contains("boom"), "{body}");
    }

    #[test]
    fn stderr_is_tailed_to_last_n_lines() {
        let stderr: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        let f = failure("tests/a.sh", 1, &stderr);
        let body = format_previous_failure(&[f], None);
        // Most recent lines retained, oldest dropped.
        assert!(body.contains("line 100"), "tail kept: {body}");
        assert!(body.contains(&format!("line {}", 100 - STDERR_TAIL_LINES + 1)));
        assert!(!body.contains("line 1\n"), "first line dropped: {body}");
    }

    #[test]
    fn multiple_failures_all_within_budget_are_all_included() {
        let failures = vec![
            failure("tests/a.sh", 1, "first failure stderr\n"),
            failure("tests/b.sh", 2, "second failure stderr\n"),
            failure("tests/c.sh", 3, "third failure stderr\n"),
        ];
        let body = format_previous_failure(&failures, None);
        assert!(body.contains("tests/a.sh"));
        assert!(body.contains("tests/b.sh"));
        assert!(body.contains("tests/c.sh"));
        assert!(body.contains("first failure stderr"));
        assert!(body.contains("second failure stderr"));
        assert!(body.contains("third failure stderr"));
        assert!(!body.contains("truncated"), "no truncation needed: {body}");
        assert!(!body.contains("omitted"));
    }

    #[test]
    fn later_failures_truncate_when_budget_exhausted() {
        // Earlier failures get fat stderr blocks; the last one's stderr
        // should be cut off.
        let big = "x".repeat(2000);
        let failures = vec![
            failure("tests/a.sh", 1, &big),
            failure("tests/b.sh", 2, &big),
            failure("tests/c.sh", 3, &big),
        ];
        let body = format_within_budget(&failures, 4000);
        // Earlier blocks present in full.
        assert!(body.contains("tests/a.sh"));
        assert!(body.contains("tests/b.sh"));
        // Last block was over-budget — either truncated mid-block or omitted.
        assert!(
            body.contains("truncated") || body.contains("omitted"),
            "later failure must signal it was cut: {body}",
        );
    }

    #[test]
    fn budget_is_respected_within_marker_overhead() {
        let big = "y".repeat(5000);
        let failures = vec![failure("tests/a.sh", 1, &big)];
        let body = format_within_budget(&failures, 4000);
        // The body itself fits (small overhead from the truncated marker is
        // accounted for inside the budget).
        assert!(
            body.len() <= 4000,
            "body={} exceeds budget=4000",
            body.len(),
        );
        assert!(body.contains("truncated"));
    }

    #[test]
    fn fully_omitted_failures_are_counted_in_marker() {
        // Tiny budget — only the first block fits; the rest are omitted.
        let failures = vec![
            failure("tests/a.sh", 1, "ok-ish\n"),
            failure("tests/b.sh", 2, "ok-ish\n"),
            failure("tests/c.sh", 3, "ok-ish\n"),
        ];
        let body = format_within_budget(&failures, 60);
        assert!(body.contains("tests/a.sh"));
        assert!(
            body.contains("more verify failure(s) omitted"),
            "must count omitted blocks: {body}",
        );
    }

    #[test]
    fn no_review_notes_section_when_review_flag_absent() {
        let f = failure("tests/a.sh", 1, "boom\n");
        let body = format_previous_failure(&[f], None);
        assert!(
            !body.contains("Review notes:"),
            "heading must only appear when review flagged: {body}",
        );
    }

    #[test]
    fn review_notes_appended_under_heading_when_review_flagged() {
        // Spec: when verify fails AND review also flagged, the review's
        // reasoning is appended to `previous_failure` under a separate
        // `Review notes:` heading.
        let f = failure("tests/a.sh", 1, "boom\n");
        let rf = flag(
            ReviewConcern::VerifierBypass,
            "test mocks the agent backend",
        );
        let body = format_previous_failure(&[f], Some(&rf));
        // Verify-fail block still present.
        assert!(body.contains("tests/a.sh"), "{body}");
        // Heading separates the two sections.
        assert!(body.contains("Review notes:\n"), "heading missing: {body}");
        // Concern + detail both surface so the agent knows which review rule
        // tripped, not just the prose.
        assert!(body.contains("[verifier-bypass]"), "concern token: {body}");
        assert!(
            body.contains("test mocks the agent backend"),
            "detail: {body}",
        );
        // Review notes come AFTER verify-fail blocks.
        let verify_idx = body.find("tests/a.sh").expect("verify block present");
        let notes_idx = body.find("Review notes:").expect("heading present");
        assert!(
            verify_idx < notes_idx,
            "Review notes must follow verify failures: {body}",
        );
    }

    /// Per `specs/templates.md` § Typed `PreviousFailure`,
    /// `review_notes` is populated **only** when `previous_failure` is
    /// `VerifyFailures` and the reviewer also raised a concern. The
    /// formatter is only ever entered for the verify-fail recovery cause,
    /// so this test pins the second arm of the conditional: with the
    /// verify-fail side held constant, the `Review notes:` block appears
    /// iff the `ReviewFlag` is `Some` and is absent when it is `None`.
    #[test]
    fn review_notes_populated_only_on_verify_fail_plus_review_concern() {
        let f = failure("tests/a.sh", 1, "boom\n");

        let with_concern = format_previous_failure(
            std::slice::from_ref(&f),
            Some(&flag(
                ReviewConcern::VerifierBypass,
                "test mocks the agent backend",
            )),
        );
        assert!(
            with_concern.contains(REVIEW_NOTES_HEADING),
            "verify-fail + review concern must populate Review notes: {with_concern}",
        );
        assert!(
            with_concern.contains("[verifier-bypass]"),
            "concern token must surface: {with_concern}",
        );

        let without_concern = format_previous_failure(&[f], None);
        assert!(
            !without_concern.contains(REVIEW_NOTES_HEADING),
            "verify-fail without review concern must NOT populate Review notes: {without_concern}",
        );
    }

    #[test]
    fn review_notes_truncated_when_detail_exceeds_budget() {
        let f = failure("tests/a.sh", 1, "boom\n");
        let huge = "z".repeat(REVIEW_NOTES_BUDGET * 2);
        let rf = flag(ReviewConcern::Mock, &huge);
        let body = format_previous_failure(&[f], Some(&rf));
        let notes_start = body.find("Review notes:").expect("heading present");
        let notes_section = &body[notes_start..];
        assert!(
            notes_section.len() <= REVIEW_NOTES_BUDGET,
            "Review notes section ({}) must fit within {REVIEW_NOTES_BUDGET}: {notes_section}",
            notes_section.len(),
        );
        assert!(
            notes_section.contains("truncated"),
            "over-budget detail must signal truncation: {notes_section}",
        );
    }

    #[test]
    fn review_notes_budget_is_separate_from_verify_budget() {
        // The two budgets are independent: a maxed-out verify section must
        // NOT crowd out the Review notes block.
        let big = "x".repeat(2000);
        let failures = vec![
            failure("tests/a.sh", 1, &big),
            failure("tests/b.sh", 2, &big),
        ];
        let rf = flag(ReviewConcern::Scope, "diff edits files outside spec");
        let body = format_previous_failure(&failures, Some(&rf));
        assert!(
            body.contains("Review notes:\n"),
            "review notes survive even when verify section is full: {body}",
        );
        assert!(
            body.contains("diff edits files outside spec"),
            "detail preserved: {body}",
        );
    }

    #[test]
    fn review_notes_truncation_does_not_split_utf8_codepoints() {
        let f = failure("tests/a.sh", 1, "boom\n");
        // Build a detail whose truncation point lands inside a multi-byte char.
        let detail = format!("{}🦀{}", "x".repeat(REVIEW_NOTES_BUDGET), "y".repeat(50));
        let rf = flag(ReviewConcern::Judge, &detail);
        // No panic ⇒ char-boundary respected.
        let body = format_previous_failure(&[f], Some(&rf));
        assert!(body.contains("Review notes:\n"));
    }

    #[test]
    fn last_n_lines_handles_no_trailing_newline() {
        let s = "a\nb\nc";
        assert_eq!(last_n_lines(s, 2), "b\nc");
    }

    #[test]
    fn last_n_lines_handles_trailing_newline() {
        let s = "a\nb\nc\n";
        assert_eq!(last_n_lines(s, 2), "b\nc\n");
    }

    #[test]
    fn last_n_lines_returns_whole_string_when_n_exceeds_lines() {
        let s = "a\nb\n";
        assert_eq!(last_n_lines(s, 99), "a\nb\n");
    }

    #[test]
    fn truncation_does_not_split_utf8_codepoints() {
        // 4-byte emoji + filler that puts the cut point mid-codepoint.
        let stderr = format!("{}🦀{}", "x".repeat(50), "y".repeat(50));
        let f = failure("tests/a.sh", 1, &stderr);
        // Tiny budget that lands inside the formatted block.
        let body = format_within_budget(&[f], 80);
        // No panic ⇒ char-boundary respected; assertion on body content
        // would be implementation-defined.
        assert!(body.len() <= 80);
    }
}
