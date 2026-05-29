use loom_templates::previous_failure::BadWalk;
use serde::Deserialize;

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

    /// Review-phase concern. Carries the parsed `summary` field from the
    /// terminal `LOOM_CONCERN: {"summary": "..."}` marker. The summary is
    /// for the verdict log only; per-finding routing is decided on each
    /// streamed `LOOM_FINDING:` line's token per `specs/gate.md` §
    /// LOOM_CONCERN payload. Review-phase-only — emitting `LOOM_CONCERN`
    /// from any other phase is a `wrong-phase-marker` error in the verdict
    /// gate per `specs/harness.md` § Marker definitions.
    Concern { summary: String },

    /// Review walk's terminal `LOOM_CONCERN:` payload was malformed —
    /// invalid JSON, missing `summary`, or empty `summary`. Wraps the
    /// typed [`BadWalk`] variant so the verdict gate routes to
    /// `RecoveryCause::BadWalk` per `specs/gate.md` § LOOM_CONCERN payload
    /// — JSON shape and parse discipline. Only the
    /// [`BadWalk::Concern`] sub-variant is produced here; the
    /// stream/terminator pairing-rule variants are owned by the verdict
    /// gate.
    BadWalk(BadWalk),
}

const COMPLETE: &str = "LOOM_COMPLETE";
const NOOP: &str = "LOOM_NOOP";
const BLOCKED: &str = "LOOM_BLOCKED";
const CLARIFY: &str = "LOOM_CLARIFY";
const CONCERN: &str = "LOOM_CONCERN";

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
/// `LOOM_CONCERN` carries a JSON payload on the final line:
/// `LOOM_CONCERN: {"summary": "<non-empty string>"}`. A well-formed payload
/// surfaces as [`ExitSignal::Concern`]; a malformed payload (invalid JSON,
/// missing `summary`, or empty `summary`) surfaces as
/// [`ExitSignal::BadWalk`] carrying [`BadWalk::Concern`] with the literal
/// post-marker text so the verdict gate can route to recovery without
/// silently collapsing.
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
        return Some(parse_concern(&final_line[idx + CONCERN.len()..]));
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

#[derive(Deserialize)]
struct ConcernPayload {
    summary: String,
}

/// Parse the post-`LOOM_CONCERN` payload into a typed [`ExitSignal`].
///
/// Returns [`ExitSignal::Concern`] with the JSON-parsed `summary` when the
/// payload is a well-formed `{"summary": "<non-empty>"}` object. Any parse
/// failure (invalid JSON, missing field, empty summary) returns
/// [`ExitSignal::BadWalk`] carrying [`BadWalk::Concern`] with the literal
/// post-marker text so the verdict gate can render the recovery prompt
/// with the original payload intact.
fn parse_concern(after_marker: &str) -> ExitSignal {
    let payload = after_marker.trim_start_matches(':').trim();
    match serde_json::from_str::<ConcernPayload>(payload) {
        Ok(body) if !body.summary.is_empty() => ExitSignal::Concern {
            summary: body.summary,
        },
        _ => ExitSignal::BadWalk(BadWalk::Concern {
            payload: payload.to_string(),
        }),
    }
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
    fn concern_payload_parses_as_json_with_summary_field() {
        let out = r#"LOOM_CONCERN: {"summary": "verifier-bypass on the agent backend mock"}"#;
        match parse_exit_signal(out) {
            Some(ExitSignal::Concern { summary }) => {
                assert_eq!(summary, "verifier-bypass on the agent backend mock");
            }
            other => panic!("expected Concern, got {other:?}"),
        }
    }

    #[test]
    fn concern_trims_whitespace_around_payload() {
        let out = "LOOM_CONCERN:    {\"summary\":\"scope drift\"}   \n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Concern { summary }) => assert_eq!(summary, "scope drift"),
            other => panic!("expected Concern, got {other:?}"),
        }
    }

    #[test]
    fn concern_malformed_payload_routes_to_bad_walk_concern_with_literal_payload() {
        let out = "LOOM_CONCERN: malformed payload with no separator\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::BadWalk(BadWalk::Concern { payload })) => {
                assert_eq!(payload, "malformed payload with no separator");
            }
            other => panic!("expected BadWalk::Concern, got {other:?}"),
        }
    }

    #[test]
    fn concern_with_empty_summary_routes_to_bad_walk_concern() {
        let out = r#"LOOM_CONCERN: {"summary": ""}"#;
        match parse_exit_signal(out) {
            Some(ExitSignal::BadWalk(BadWalk::Concern { payload })) => {
                assert_eq!(payload, r#"{"summary": ""}"#);
            }
            other => panic!("expected BadWalk::Concern, got {other:?}"),
        }
    }

    #[test]
    fn concern_with_missing_summary_field_routes_to_bad_walk_concern() {
        let out = r#"LOOM_CONCERN: {"summery": "typo in the field name"}"#;
        match parse_exit_signal(out) {
            Some(ExitSignal::BadWalk(BadWalk::Concern { payload })) => {
                assert_eq!(payload, r#"{"summery": "typo in the field name"}"#);
            }
            other => panic!("expected BadWalk::Concern, got {other:?}"),
        }
    }

    #[test]
    fn concern_on_non_final_line_is_swallowed() {
        let out = "LOOM_CONCERN: {\"summary\": \"scope drift\"}\nclosing prose\n";
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

    /// Backward-compat: existing review logs with the old
    /// `<token> -- <reason>` payload no longer match the JSON shape, so
    /// they surface as `BadWalk::Concern` carrying the literal payload.
    /// One-time wire-format migration: old logs become typed observable
    /// failures rather than silent `SwallowedMarker` collapse.
    #[test]
    fn legacy_token_reason_payload_routes_to_bad_walk_concern() {
        let out = "LOOM_CONCERN: verifier-bypass -- test mocks the agent backend\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::BadWalk(BadWalk::Concern { payload })) => {
                assert_eq!(payload, "verifier-bypass -- test mocks the agent backend");
            }
            other => panic!("expected BadWalk::Concern, got {other:?}"),
        }
    }
}
