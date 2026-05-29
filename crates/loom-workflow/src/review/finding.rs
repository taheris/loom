//! Typed [`Finding`] record consumed by the mint pipeline.
//!
//! Findings reach mint from two sources — the LLM rubric walk's
//! `LOOM_FINDING:` stdout lines and the deterministic verifier-runner's
//! normalised verdicts — and converge on this single in-driver
//! representation per `specs/gate.md` §"Concern tokens and target
//! variants".
//!
//! The `Finding` struct (with `ConcernToken`, `FindingTarget`,
//! `TargetKind`, `FindingValidator`, `FindingParseError`,
//! `LOOM_FINDING_PREFIX`) is defined in `loom-templates::finding` because
//! the typed retry-context surface
//! (`PreviousFailure::ReviewConcern { findings: Vec<Finding> }`) carries
//! it as a field; this module re-exports them and adds the
//! walk-level parser ([`parse_walk_output`]) plus its
//! [`WalkOutputError`], both of which depend on
//! [`crate::todo::parse_exit_signal`] for the terminal-marker check.

use displaydoc::Display;
use thiserror::Error;

pub use loom_templates::finding::{
    ConcernToken, Finding, FindingParseError, FindingTarget, FindingValidator, LOOM_FINDING_PREFIX,
    TargetKind,
};

use crate::todo::parse_exit_signal;

/// Top-level error for [`parse_walk_output`]. Either a per-line
/// validation failure or the terminal-marker enforcement — a walk that
/// emits `LOOM_FINDING:` lines without a terminal marker per
/// `specs/gate.md` § *Findings and Minting*.
#[derive(Debug, Display, Error)]
pub enum WalkOutputError {
    /// per-line parse failure: {0}
    Finding(#[from] FindingParseError),
    /// walk emitted {findings_count} LOOM_FINDING line(s) but no terminal marker (LOOM_COMPLETE / LOOM_CONCERN / LOOM_BLOCKED / LOOM_CLARIFY)
    MissingTerminalMarker { findings_count: usize },
}

/// Scan `output` for `LOOM_FINDING:` lines, parse and fully-validate
/// each (Layers 1–5 plus the `target.spec ∈ bonds` rule), and enforce
/// the terminal-marker rule: an output that emits one or more findings
/// but no terminal marker (`LOOM_COMPLETE` / `LOOM_CONCERN` /
/// `LOOM_BLOCKED` / `LOOM_CLARIFY`) is rejected with
/// [`WalkOutputError::MissingTerminalMarker`].
///
/// Findings interleave with markers in stdout order — the returned
/// vector preserves emission order, and the terminal-marker check
/// reads through [`parse_exit_signal`] so the parser surface used by
/// `LOOM_CONCERN` / `LOOM_COMPLETE` is the same one consulted here
/// (no separate channel).
pub fn parse_walk_output<V: FindingValidator + ?Sized>(
    output: &str,
    validator: &V,
) -> Result<Vec<Finding>, WalkOutputError> {
    let mut findings = Vec::new();
    for (idx, line) in output.lines().enumerate() {
        let line_number = idx + 1;
        let Some(payload_start) = line.find(LOOM_FINDING_PREFIX) else {
            continue;
        };
        let payload = line[payload_start + LOOM_FINDING_PREFIX.len()..].trim_start();
        let finding = Finding::parse_payload(payload, line_number, line)?;
        finding.validate(line_number, line, validator)?;
        findings.push(finding);
    }
    if !findings.is_empty() && parse_exit_signal(output).is_none() {
        return Err(WalkOutputError::MissingTerminalMarker {
            findings_count: findings.len(),
        });
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_events::identifier::SpecLabel;

    fn spec(s: &str) -> SpecLabel {
        s.parse().expect("valid spec label")
    }

    struct AlwaysValid;

    impl FindingValidator for AlwaysValid {
        fn spec_label_is_known(&self, _label: &SpecLabel) -> bool {
            true
        }
        fn criterion_anchor_resolves(&self, _spec: &SpecLabel, _anchor: &str) -> bool {
            true
        }
        fn annotation_resolves(&self, _target_string: &str) -> bool {
            true
        }
        fn file_exists(&self, _path: &str) -> bool {
            true
        }
        fn invariant_resolves(&self, _spec: &SpecLabel, _section: &str, _tag: &str) -> bool {
            true
        }
    }

    struct KnownSpecs<'a>(&'a [&'a str]);

    impl FindingValidator for KnownSpecs<'_> {
        fn spec_label_is_known(&self, label: &SpecLabel) -> bool {
            self.0.iter().any(|s| *s == label.as_str())
        }
        fn criterion_anchor_resolves(&self, _spec: &SpecLabel, _anchor: &str) -> bool {
            true
        }
        fn annotation_resolves(&self, _target_string: &str) -> bool {
            true
        }
        fn file_exists(&self, _path: &str) -> bool {
            true
        }
        fn invariant_resolves(&self, _spec: &SpecLabel, _section: &str, _tag: &str) -> bool {
            true
        }
    }

    struct NothingResolves;

    impl FindingValidator for NothingResolves {
        fn spec_label_is_known(&self, _label: &SpecLabel) -> bool {
            true
        }
        fn criterion_anchor_resolves(&self, _spec: &SpecLabel, _anchor: &str) -> bool {
            false
        }
        fn annotation_resolves(&self, _target_string: &str) -> bool {
            false
        }
        fn file_exists(&self, _path: &str) -> bool {
            false
        }
        fn invariant_resolves(&self, _spec: &SpecLabel, _section: &str, _tag: &str) -> bool {
            false
        }
    }

    fn payload(token: &str, bonds: &[&str], target_json: &str, evidence: &str) -> String {
        let bonds_json = bonds
            .iter()
            .map(|b| format!("\"{b}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"token":"{token}","bonds":[{bonds_json}],"target":{target_json},"evidence":"{evidence}"}}"#,
        )
    }

    fn finding_line(token: &str, bonds: &[&str], target_json: &str, evidence: &str) -> String {
        format!(
            "{} {}",
            LOOM_FINDING_PREFIX,
            payload(token, bonds, target_json, evidence)
        )
    }

    #[test]
    fn mint_walk_emits_loom_finding_json_lines_streamed_per_finding() {
        let line_a = finding_line(
            "spec-coherence-fail",
            &["gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}"#,
            "first finding",
        );
        let line_b = finding_line(
            "orphan-integration",
            &["harness"],
            r#"{"kind":"Contract","id":"molecule-lifecycle"}"#,
            "second finding",
        );
        let output = format!(
            "preamble\n{line_a}\nintermediate prose\n{line_b}\nLOOM_CONCERN: verifier-bypass -- two findings"
        );
        let findings = parse_walk_output(&output, &AlwaysValid).expect("parses cleanly");
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].token, ConcernToken::SpecCoherenceFail);
        assert_eq!(findings[0].evidence, "first finding");
        assert_eq!(findings[1].token, ConcernToken::OrphanIntegration);
        assert_eq!(findings[1].evidence, "second finding");
    }

    #[test]
    fn mint_walk_without_terminal_marker_fails_run() {
        let line = finding_line(
            "spec-coherence-fail",
            &["gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}"#,
            "no terminal marker follows",
        );
        let output = format!("preamble\n{line}\ntrailing prose without a marker\n");
        match parse_walk_output(&output, &AlwaysValid) {
            Err(WalkOutputError::MissingTerminalMarker { findings_count }) => {
                assert_eq!(findings_count, 1);
            }
            other => panic!("expected MissingTerminalMarker, got {other:?}"),
        }
    }

    #[test]
    fn mint_walk_without_findings_does_not_require_terminal_marker() {
        let output = "preamble with no findings and no markers\n";
        let findings = parse_walk_output(output, &AlwaysValid).expect("vacuous case");
        assert!(findings.is_empty());
    }

    #[test]
    fn mint_walk_accepts_each_terminal_marker_variant() {
        let line = finding_line(
            "orphan-integration",
            &["harness"],
            r#"{"kind":"Contract","id":"x"}"#,
            "",
        );
        for terminal in ["LOOM_COMPLETE", "LOOM_BLOCKED", "LOOM_CLARIFY"] {
            let output = format!("{line}\nreason for {terminal}\n{terminal}\n");
            parse_walk_output(&output, &AlwaysValid)
                .unwrap_or_else(|e| panic!("{terminal} should accept: {e}"));
        }
    }

    #[test]
    fn mint_parses_loom_finding_json_into_typed_record_with_tagged_target() {
        let line = finding_line(
            "orphan-integration",
            &["harness"],
            r#"{"kind":"Contract","id":"molecule-lifecycle"}"#,
            "contract is dangling",
        );
        let output = format!("{line}\nLOOM_CONCERN: orphan-integration -- found one\n");
        let findings = parse_walk_output(&output, &AlwaysValid).expect("parses cleanly");
        let [parsed] = findings.as_slice() else {
            panic!("expected exactly one finding, got {findings:?}")
        };
        assert_eq!(parsed.token, ConcernToken::OrphanIntegration);
        assert!(
            matches!(parsed.target, FindingTarget::Contract { ref id } if id == "molecule-lifecycle")
        );
        assert_eq!(parsed.target.kind(), TargetKind::Contract);
        assert_eq!(parsed.token.expected_target_kind(), parsed.target.kind(),);
    }

    #[test]
    fn mint_malformed_loom_finding_fails_run_with_typed_error() {
        let valid_terminal = "LOOM_CONCERN: orphan-integration -- summary";

        let line = format!("{LOOM_FINDING_PREFIX} {{not valid json");
        let output = format!("{line}\n{valid_terminal}\n");
        match parse_walk_output(&output, &AlwaysValid) {
            Err(WalkOutputError::Finding(FindingParseError::Json {
                line_number, raw, ..
            })) => {
                assert_eq!(line_number, 1);
                assert!(raw.contains("not valid json"), "raw: {raw}");
            }
            other => panic!("expected Json error, got {other:?}"),
        }

        let line = finding_line(
            "not-a-known-token",
            &["gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"x"}"#,
            "",
        );
        let output = format!("{line}\n{valid_terminal}\n");
        match parse_walk_output(&output, &AlwaysValid) {
            Err(WalkOutputError::Finding(FindingParseError::Json {
                line_number, raw, ..
            })) => {
                assert_eq!(line_number, 1);
                assert!(raw.contains("not-a-known-token"), "raw: {raw}");
            }
            other => panic!("expected Json error for unknown token, got {other:?}"),
        }

        let line = finding_line(
            "orphan-integration",
            &["not-a-real-spec"],
            r#"{"kind":"Contract","id":"x"}"#,
            "",
        );
        let output = format!("{line}\n{valid_terminal}\n");
        let known = KnownSpecs(&["gate", "harness"]);
        match parse_walk_output(&output, &known) {
            Err(WalkOutputError::Finding(FindingParseError::UnknownBondSpec {
                line_number,
                spec: bad,
                raw,
            })) => {
                assert_eq!(line_number, 1);
                assert_eq!(bad, "not-a-real-spec");
                assert!(raw.contains("not-a-real-spec"));
            }
            other => panic!("expected UnknownBondSpec, got {other:?}"),
        }

        let line = finding_line(
            "orphan-integration",
            &["gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"x"}"#,
            "",
        );
        let output = format!("{line}\n{valid_terminal}\n");
        match parse_walk_output(&output, &AlwaysValid) {
            Err(WalkOutputError::Finding(FindingParseError::TokenVariantMismatch {
                line_number,
                token,
                expected,
                actual,
                raw,
            })) => {
                assert_eq!(line_number, 1);
                assert_eq!(token, "orphan-integration");
                assert_eq!(expected, TargetKind::Contract);
                assert_eq!(actual, TargetKind::Criterion);
                assert!(raw.contains("orphan-integration"));
            }
            other => panic!("expected TokenVariantMismatch, got {other:?}"),
        }

        let line = finding_line(
            "judge-flag",
            &["gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"missing-anchor"}"#,
            "",
        );
        let output = format!("{line}\n{valid_terminal}\n");
        match parse_walk_output(&output, &NothingResolves) {
            Err(WalkOutputError::Finding(FindingParseError::UnresolvedTarget {
                line_number,
                detail,
                raw,
            })) => {
                assert_eq!(line_number, 1);
                assert!(detail.contains("missing-anchor"), "detail: {detail}");
                assert!(raw.contains("missing-anchor"));
            }
            other => panic!("expected UnresolvedTarget, got {other:?}"),
        }
    }

    #[test]
    fn mint_rejects_criterion_target_whose_spec_is_not_in_bonds() {
        let line = finding_line(
            "spec-coherence-fail",
            &["harness"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}"#,
            "criterion belongs to gate but bonds names harness only",
        );
        let output = format!("{line}\nLOOM_CONCERN: spec-coherence-fail -- bad bonds\n");
        match parse_walk_output(&output, &AlwaysValid) {
            Err(WalkOutputError::Finding(FindingParseError::TargetSpecNotInBonds {
                line_number,
                spec: missing,
                bonds,
                raw,
            })) => {
                assert_eq!(line_number, 1);
                assert_eq!(missing, "gate");
                assert_eq!(bonds, vec!["harness".to_owned()]);
                assert!(raw.contains("\"spec\":\"gate\""));
            }
            other => panic!("expected TargetSpecNotInBonds, got {other:?}"),
        }

        let line = finding_line(
            "invariant-clash",
            &["gate"],
            r#"{"kind":"Invariant","spec":"harness","section":"Out of Scope","tag":"loom-runs-podman"}"#,
            "invariant target spec missing from bonds",
        );
        let output = format!("{line}\nLOOM_CONCERN: invariant-clash -- bad bonds\n");
        match parse_walk_output(&output, &AlwaysValid) {
            Err(WalkOutputError::Finding(FindingParseError::TargetSpecNotInBonds {
                spec: missing,
                ..
            })) => {
                assert_eq!(missing, "harness");
            }
            other => panic!("expected TargetSpecNotInBonds, got {other:?}"),
        }

        let line = finding_line(
            "spec-coherence-fail",
            &["harness", "gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}"#,
            "criterion belongs to gate, bonds names both",
        );
        let output = format!("{line}\nLOOM_CONCERN: spec-coherence-fail -- ok\n");
        let findings = parse_walk_output(&output, &AlwaysValid).expect("should parse");
        assert_eq!(findings.len(), 1);

        // Pin sample reference so spec helper is exercised.
        let _ = spec("gate");
    }
}
