//! Terminal-marker parser for `loom inbox chat`.

use std::collections::BTreeSet;

use displaydoc::Display;
use loom_driver::identifier::BeadId;
use serde::Deserialize;
use thiserror::Error;

const COMPLETE: &str = "LOOM_COMPLETE";
const APPLY: &str = "LOOM_APPLY";
const NOOP: &str = "LOOM_NOOP";
const BLOCKED: &str = "LOOM_BLOCKED";
const CLARIFY: &str = "LOOM_CLARIFY";
const RETRY: &str = "LOOM_RETRY";
const CONCERN: &str = "LOOM_CONCERN";

const MARKERS: [&str; 7] = [COMPLETE, APPLY, NOOP, BLOCKED, CLARIFY, RETRY, CONCERN];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalMarker {
    Complete,
    Apply { proposals: Vec<BeadId> },
}

#[derive(Debug, Display, Error)]
pub enum TerminalMarkerError {
    /// inbox chat ended without LOOM_COMPLETE or LOOM_APPLY on the final non-empty line
    Missing,
    /// wrong-phase-marker: inbox chat emitted more than one terminal marker
    Paired,
    /// wrong-phase-marker: `{marker}` emitted from inbox chat
    WrongPhase { marker: &'static str },
    /// wrong-phase-marker: malformed LOOM_APPLY payload: {detail}
    MalformedApply { detail: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPayload {
    proposals: Vec<BeadId>,
}

pub fn parse(output: &str) -> Result<TerminalMarker, TerminalMarkerError> {
    let lines: Vec<&str> = output.lines().collect();
    let final_idx = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .ok_or(TerminalMarkerError::Missing)?;
    let final_line = lines[final_idx].trim();

    if prior_marker_lines(&lines[..final_idx]) > 0 || markers_on(final_line) > 1 {
        return Err(TerminalMarkerError::Paired);
    }

    if final_line == COMPLETE {
        return Ok(TerminalMarker::Complete);
    }
    if let Some(rest) = final_line.strip_prefix(APPLY) {
        return parse_apply(rest);
    }
    for marker in [NOOP, BLOCKED, CLARIFY, RETRY, CONCERN] {
        if final_line.starts_with(marker) {
            return Err(TerminalMarkerError::WrongPhase { marker });
        }
    }
    Err(TerminalMarkerError::Missing)
}

fn parse_apply(rest: &str) -> Result<TerminalMarker, TerminalMarkerError> {
    let payload = rest
        .strip_prefix(':')
        .ok_or_else(|| TerminalMarkerError::MalformedApply {
            detail: "missing `:` after LOOM_APPLY".to_string(),
        })?
        .trim();
    let parsed: ApplyPayload =
        serde_json::from_str(payload).map_err(|source| TerminalMarkerError::MalformedApply {
            detail: source.to_string(),
        })?;
    if parsed.proposals.is_empty() {
        return Err(TerminalMarkerError::MalformedApply {
            detail: "proposals array is empty".to_string(),
        });
    }
    reject_duplicate_ids(&parsed.proposals)?;
    Ok(TerminalMarker::Apply {
        proposals: parsed.proposals,
    })
}

fn reject_duplicate_ids(proposals: &[BeadId]) -> Result<(), TerminalMarkerError> {
    let mut seen = BTreeSet::new();
    for proposal in proposals {
        if !seen.insert(proposal.as_str()) {
            return Err(TerminalMarkerError::MalformedApply {
                detail: format!("duplicate proposal id `{proposal}`"),
            });
        }
    }
    Ok(())
}

fn prior_marker_lines(lines: &[&str]) -> usize {
    lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| MARKERS.iter().any(|marker| line.starts_with(marker)))
        .count()
}

fn markers_on(line: &str) -> usize {
    MARKERS
        .iter()
        .filter(|marker| line.contains(*marker))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_marker_parses_only_as_final_line() {
        assert_eq!(
            parse("done\nLOOM_COMPLETE\n").unwrap(),
            TerminalMarker::Complete
        );
        assert!(matches!(
            parse("LOOM_COMPLETE\nmore\n"),
            Err(TerminalMarkerError::Paired)
        ));
    }

    #[test]
    fn apply_marker_parses_typed_proposal_ids() {
        let parsed =
            parse("ok\nLOOM_APPLY: {\"proposals\":[\"lm-abc123\",\"lm-abc123.4\"]}\n").unwrap();
        match parsed {
            TerminalMarker::Apply { proposals } => {
                assert_eq!(proposals[0].as_str(), "lm-abc123");
                assert_eq!(proposals[1].as_str(), "lm-abc123.4");
            }
            TerminalMarker::Complete => panic!("expected apply marker"),
        }
    }

    #[test]
    fn malformed_apply_is_rejected() {
        assert!(matches!(
            parse("LOOM_APPLY: {\"proposal\":[]}"),
            Err(TerminalMarkerError::MalformedApply { .. })
        ));
        assert!(matches!(
            parse("LOOM_APPLY: {\"proposals\":[\"not a bead\"]}"),
            Err(TerminalMarkerError::MalformedApply { .. })
        ));
    }

    #[test]
    fn wrong_phase_inbox_markers_are_rejected() {
        for marker in [NOOP, BLOCKED, CLARIFY, RETRY, CONCERN] {
            assert!(matches!(
                parse(marker),
                Err(TerminalMarkerError::WrongPhase { .. })
            ));
        }
    }

    #[test]
    fn paired_markers_are_rejected() {
        assert!(matches!(
            parse("LOOM_COMPLETE\nLOOM_APPLY: {\"proposals\":[\"lm-abc123\"]}"),
            Err(TerminalMarkerError::Paired)
        ));
        assert!(matches!(
            parse("LOOM_COMPLETE LOOM_APPLY: {\"proposals\":[\"lm-abc123\"]}"),
            Err(TerminalMarkerError::Paired)
        ));
    }
}
