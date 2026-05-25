/// Parsed exit signal from an agent session — the trailing line the agent
/// emits to signal the gate's verdict.
///
/// Markers are **mutually exclusive** and live on the final non-empty line
/// of the agent's last assistant message. [`parse_exit_signal`] enforces
/// the mechanical half of that rule: only the final line is inspected, and
/// a final line carrying more than one marker is treated as a
/// swallowed-marker (returned as `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitSignal {
    /// Agent finished cleanly; the driver advances per-spec cursors and
    /// commits the spec file.
    Complete,

    /// Agent finished cleanly but the phase intentionally produced an
    /// empty diff — the work was already done. Without this signal an
    /// empty diff is treated as zero-progress.
    Noop,

    /// Agent could not proceed; the driver surfaces the reason to the user
    /// without advancing state.
    Blocked { reason: String },

    /// Agent needs human input; the driver applies the `loom:clarify`
    /// label and bails.
    Clarify { question: String },

    /// Review-phase concern. Carries the structured payload emitted as
    /// `LOOM_CONCERN: <token> -- <reason>`. The verdict gate maps the
    /// token to a typed concern via [`super::super::review::ReviewConcern`]
    /// (or future typed equivalents); unknown tokens round-trip as the
    /// literal string so the wire-additive contract in `docs/style-rules.md`
    /// (RS-17 `Other(String)` fallback) holds. Review-phase-only — emitting
    /// `LOOM_CONCERN` from any other phase is a `wrong-phase-marker` error
    /// in the verdict gate per `specs/harness.md` § Marker definitions.
    Concern { token: String, reason: String },
}

const COMPLETE: &str = "LOOM_COMPLETE";
const NOOP: &str = "LOOM_NOOP";
const BLOCKED: &str = "LOOM_BLOCKED";
const CLARIFY: &str = "LOOM_CLARIFY";
const CONCERN: &str = "LOOM_CONCERN";
const CONCERN_SEPARATOR: &str = "--";

/// Scan the agent's combined output (or the `result` field of the final
/// stream-json line) for an exit signal.
///
/// The parser inspects **only the final non-empty line** of `output`. Any
/// marker emitted earlier in the session is treated as swallowed; multiple
/// markers on the final line likewise collapse to `None` per the
/// mutual-exclusivity rule in `specs/harness.md` § Marker definitions.
///
/// `LOOM_BLOCKED` and `LOOM_CLARIFY` are bare markers — no trailing colon,
/// no trailing payload. The reason / question is read from the text
/// **before** the marker on the final line, falling back to the most recent
/// non-empty line before the final line if the same-line prefix is empty.
///
/// `LOOM_CONCERN` carries a structured payload on the final line:
/// `LOOM_CONCERN: <token> -- <reason>`. The token and reason are extracted
/// from that line directly; the `--` separator is mandatory for a
/// well-formed marker, and a missing separator collapses to a swallowed
/// marker.
///
/// `None` means no signal was found on the final line and the caller
/// should surface [`super::TodoError::MissingExitSignal`] or the
/// equivalent swallowed-marker recovery cause.
pub fn parse_exit_signal(output: &str) -> Option<ExitSignal> {
    let lines: Vec<&str> = output.lines().collect();
    let final_idx = lines.iter().rposition(|line| !line.trim().is_empty())?;
    let final_line = lines[final_idx];
    let prior = &lines[..final_idx];

    if has_multiple_markers(final_line) {
        return None;
    }

    if let Some(idx) = final_line.find(CONCERN) {
        return parse_concern(&final_line[idx + CONCERN.len()..]);
    }
    if let Some(reason) = reason_for(BLOCKED, final_line, prior) {
        return Some(ExitSignal::Blocked { reason });
    }
    if let Some(question) = reason_for(CLARIFY, final_line, prior) {
        return Some(ExitSignal::Clarify { question });
    }
    if final_line.contains(COMPLETE) {
        return Some(ExitSignal::Complete);
    }
    if final_line.contains(NOOP) {
        return Some(ExitSignal::Noop);
    }
    None
}

/// Count distinct marker keywords on `line`. The keywords are matched as
/// substrings; `CONCERN` is a substring of nothing else in the set, and
/// the others are pairwise non-overlapping, so distinct hits map one-to-one
/// to distinct markers.
fn has_multiple_markers(line: &str) -> bool {
    let markers = [COMPLETE, NOOP, BLOCKED, CLARIFY, CONCERN];
    let mut hits = 0;
    for marker in markers {
        if line.contains(marker) {
            hits += 1;
            if hits > 1 {
                return true;
            }
        }
    }
    false
}

fn parse_concern(after_marker: &str) -> Option<ExitSignal> {
    let payload = after_marker.trim_start_matches(':').trim();
    let (token, reason) = payload.split_once(CONCERN_SEPARATOR)?;
    let token = token.trim();
    let reason = reason.trim();
    if token.is_empty() {
        return None;
    }
    Some(ExitSignal::Concern {
        token: token.to_string(),
        reason: reason.to_string(),
    })
}

fn reason_for(marker: &str, line: &str, prior: &[&str]) -> Option<String> {
    let idx = line.find(marker)?;
    let same_line = line[..idx].trim();
    if !same_line.is_empty() {
        return Some(same_line.to_string());
    }
    for prev in prior.iter().rev() {
        let trimmed = prev.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    Some(String::new())
}

#[cfg(test)]
#[expect(clippy::panic, reason = "tests use panicking helpers")]
mod tests {
    use super::*;

    #[test]
    fn complete_on_bare_marker() {
        assert_eq!(
            parse_exit_signal("ok\nLOOM_COMPLETE\n"),
            Some(ExitSignal::Complete)
        );
    }

    #[test]
    fn noop_on_bare_marker() {
        assert_eq!(
            parse_exit_signal("already done\nLOOM_NOOP\n"),
            Some(ExitSignal::Noop)
        );
    }

    #[test]
    fn blocked_carries_reason_from_prior_line() {
        let out = "doing things\nspec is missing the requirements section\nLOOM_BLOCKED\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Blocked { reason }) => {
                assert_eq!(reason, "spec is missing the requirements section");
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn clarify_carries_question_from_prior_line() {
        let out = "should the migration be additive only?\nLOOM_CLARIFY";
        match parse_exit_signal(out) {
            Some(ExitSignal::Clarify { question }) => {
                assert_eq!(question, "should the migration be additive only?");
            }
            other => panic!("expected Clarify, got {other:?}"),
        }
    }

    #[test]
    fn no_signal_returns_none() {
        assert_eq!(
            parse_exit_signal("just some output\nno marker here\n"),
            None
        );
    }

    #[test]
    fn marker_recognized_inside_a_longer_line() {
        let out = "Final result: missing schema LOOM_BLOCKED\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Blocked { reason }) => {
                assert_eq!(reason, "Final result: missing schema");
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn blank_lines_between_reason_and_marker_are_skipped() {
        let out = "the actual reason\n\n\nLOOM_BLOCKED\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Blocked { reason }) => assert_eq!(reason, "the actual reason"),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn marker_at_start_with_no_prior_lines_yields_empty_reason() {
        let out = "LOOM_BLOCKED";
        match parse_exit_signal(out) {
            Some(ExitSignal::Blocked { reason }) => assert!(reason.is_empty()),
            other => panic!("expected Blocked with empty reason, got {other:?}"),
        }
    }

    /// Final-line-only rule: a marker emitted earlier in the session is
    /// `swallowed-marker` territory, not a verdict.
    #[test]
    fn marker_on_non_final_line_is_swallowed() {
        let out = "LOOM_COMPLETE\nfollow-up prose that hides the marker\n";
        assert_eq!(parse_exit_signal(out), None);
    }

    /// Mutual exclusivity: an agent that emits two markers on the final
    /// line is treated as swallowed rather than letting the parser silently
    /// pick one.
    #[test]
    fn multiple_markers_on_final_line_swallow_the_signal() {
        let out = "LOOM_BLOCKED LOOM_COMPLETE\n";
        assert_eq!(parse_exit_signal(out), None);
    }

    /// The new "look at the final line only" rule replaces the prior
    /// "last match wins" sweep: a `LOOM_BLOCKED` followed by a separate
    /// `LOOM_COMPLETE` line resolves to `Complete` because the final line
    /// is the only one inspected — the earlier line is swallowed.
    #[test]
    fn final_line_is_authoritative_when_prior_line_also_has_a_marker() {
        let out = "tentative\nLOOM_BLOCKED\nactually nevermind\nLOOM_COMPLETE";
        assert_eq!(parse_exit_signal(out), Some(ExitSignal::Complete));
    }

    #[test]
    fn concern_with_structured_payload_parses_token_and_reason() {
        let out = "LOOM_CONCERN: verifier-bypass -- test mocks the agent backend";
        match parse_exit_signal(out) {
            Some(ExitSignal::Concern { token, reason }) => {
                assert_eq!(token, "verifier-bypass");
                assert_eq!(reason, "test mocks the agent backend");
            }
            other => panic!("expected Concern, got {other:?}"),
        }
    }

    #[test]
    fn concern_trims_whitespace_around_payload_components() {
        let out = "LOOM_CONCERN:   scope   --   diff edits files outside the bead\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Concern { token, reason }) => {
                assert_eq!(token, "scope");
                assert_eq!(reason, "diff edits files outside the bead");
            }
            other => panic!("expected Concern, got {other:?}"),
        }
    }

    #[test]
    fn concern_without_separator_collapses_to_none() {
        let out = "LOOM_CONCERN: malformed payload with no separator\n";
        assert_eq!(parse_exit_signal(out), None);
    }

    #[test]
    fn concern_with_empty_token_collapses_to_none() {
        let out = "LOOM_CONCERN: -- reason but no token\n";
        assert_eq!(parse_exit_signal(out), None);
    }

    #[test]
    fn concern_on_non_final_line_is_swallowed() {
        let out = "LOOM_CONCERN: scope -- bad diff\nclosing prose\n";
        assert_eq!(parse_exit_signal(out), None);
    }

    /// The legacy `LOOM_REVIEW_FLAG:` keyword is no longer a recognised
    /// marker; on a prior line it is plain prose and the final-line
    /// `LOOM_COMPLETE` is the verdict.
    #[test]
    fn legacy_review_flag_keyword_on_prior_line_does_not_shadow_final_complete() {
        let out =
            "LOOM_REVIEW_FLAG: verifier-bypass -- test mocks the agent backend\nLOOM_COMPLETE\n";
        assert_eq!(parse_exit_signal(out), Some(ExitSignal::Complete));
    }
}
