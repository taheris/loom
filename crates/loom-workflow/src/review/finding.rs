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
//!
//! [`WalkOutput`] is the typed product the review-phase classifier
//! consumes (per `specs/gate.md` § *Structural enforcement*) — its
//! `pub(crate)` constructor takes the agent's combined stdout plus a
//! [`FindingValidator`] and runs the parse pipeline once, so the
//! silent-loss failure class (production caller passing raw `&str` and
//! leaving `streamed_findings` at default empty) becomes structurally
//! unrepresentable.

use displaydoc::Display;
use thiserror::Error;

pub use loom_templates::finding::{
    ConcernToken, Finding, FindingParseError, FindingTarget, FindingValidator, LOOM_FINDING_PREFIX,
    TargetKind,
};
pub use loom_templates::previous_failure::TerminalSurface;

use crate::todo::{ExitSignal, parse_exit_signal};

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

/// Typed product the review-phase classifier consumes — a single
/// pre-parsed snapshot of the agent's stdout containing the typed
/// terminal surface, the well-formed [`Finding`] records, and any
/// per-line parse errors.
///
/// `WalkOutput`'s [`Self::from_stdout`] constructor is `pub(crate)` so
/// production callers cannot bypass parsing — the silent-loss failure
/// class (calling `classify_review_phase` with raw `&str` and leaving
/// `streamed_findings` at default empty) becomes structurally
/// unrepresentable per `specs/gate.md` § *Structural enforcement*.
/// Mirrors the sealed-`MarkerProof` pattern from `## Marker`: validated
/// construction through a `pub(crate)`-only mint authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkOutput {
    /// Typed terminal surface read from the final non-empty line of
    /// stdout — well-formed [`ExitSignal`] variants surface as the
    /// corresponding [`TerminalSurface`] variant; a malformed
    /// `LOOM_CONCERN:` payload surfaces as
    /// [`TerminalSurface::Malformed`]; absence of any marker surfaces
    /// as [`TerminalSurface::Missing`].
    pub terminal: TerminalSurface,
    /// Findings that passed strict per-layer validation. Order
    /// preserves stdout emission order.
    pub findings: Vec<Finding>,
    /// Per-line parse failures for `LOOM_FINDING:` substring matches
    /// that did not pass strict validation. Carries the offending
    /// 1-based line number and verbatim line text so the recovery
    /// prompt can quote it back.
    pub finding_errors: Vec<FindingParseError>,
}

impl WalkOutput {
    /// Parse the agent's combined stdout into a typed `WalkOutput`.
    /// Runs `LOOM_FINDING:` substring search, strict per-line
    /// validation against `validator`, and terminal-marker
    /// classification through [`parse_exit_signal`] — once, here, so
    /// downstream classifier code consumes the typed product and
    /// cannot accidentally re-derive it from `&str`.
    ///
    /// `pub(crate)` — production callers reach `WalkOutput` only via
    /// this constructor, never by raw struct literal. The classifier
    /// (`classify_review_phase`) takes `&WalkOutput` so the type
    /// signature itself rejects un-parsed input.
    pub(crate) fn from_stdout<V: FindingValidator + ?Sized>(output: &str, validator: &V) -> Self {
        let mut findings = Vec::new();
        let mut finding_errors = Vec::new();
        for (idx, line) in output.lines().enumerate() {
            let line_number = idx + 1;
            let Some(payload_start) = line.find(LOOM_FINDING_PREFIX) else {
                continue;
            };
            let payload = line[payload_start + LOOM_FINDING_PREFIX.len()..].trim_start();
            match Finding::parse_payload(payload, line_number, line)
                .and_then(|f| f.validate(line_number, line, validator).map(|()| f))
            {
                Ok(finding) => findings.push(finding),
                Err(e) => finding_errors.push(e),
            }
        }
        let terminal = terminal_surface_from_stdout(output);
        Self {
            terminal,
            findings,
            finding_errors,
        }
    }
}

/// Resolve the typed [`TerminalSurface`] from the agent's combined
/// stdout. Well-formed terminal markers route through
/// [`parse_exit_signal`] so the parser surface is shared with the
/// `LOOM_FINDING:` stream check; a malformed terminal surfaces as
/// [`TerminalSurface::Malformed`]; absence surfaces as
/// [`TerminalSurface::Missing`].
fn terminal_surface_from_stdout(output: &str) -> TerminalSurface {
    match parse_exit_signal(output) {
        Some(ExitSignal::Complete) => TerminalSurface::Complete,
        Some(ExitSignal::Noop) => TerminalSurface::Noop,
        Some(ExitSignal::Blocked { reason }) => TerminalSurface::Blocked { reason },
        Some(ExitSignal::Clarify { question }) => TerminalSurface::Clarify { question },
        Some(ExitSignal::Concern { summary }) => TerminalSurface::Concern { summary },
        Some(ExitSignal::BadWalk(loom_templates::previous_failure::BadWalk::Concern {
            payload,
            ..
        })) => TerminalSurface::Malformed { payload },
        Some(ExitSignal::BadWalk(_)) | None => TerminalSurface::Missing,
    }
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

    /// Per criterion
    /// `backtick_wrapped_loom_finding_line_routes_to_bad_walk_malformed_finding_with_terminal_preserved`:
    /// a `LOOM_FINDING:` line wrapped in markdown backticks (a common
    /// LLM-emit failure) is detected by the substring search but fails
    /// strict JSON validation; [`WalkOutput::from_stdout`] surfaces the
    /// failure on `WalkOutput.finding_errors`, and the well-formed
    /// terminator on `WalkOutput.terminal` rides through alongside it.
    /// The classifier feeds both into `BadWalk::MalformedFinding` so
    /// the recovery prompt names the failing line + preserved
    /// terminal.
    #[test]
    fn backtick_wrapped_loom_finding_line_routes_to_bad_walk_malformed_finding_with_terminal_preserved()
     {
        let good_line = finding_line(
            "spec-coherence-fail",
            &["gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}"#,
            "well-formed",
        );
        // Wrap the second LOOM_FINDING: in backticks (markdown fence-style).
        let bad_line = format!("`{LOOM_FINDING_PREFIX} {{not valid json — fenced in backticks}}`");
        let output = format!("{good_line}\n{bad_line}\nLOOM_COMPLETE\n");
        let walk = WalkOutput::from_stdout(&output, &AlwaysValid);
        assert_eq!(walk.findings.len(), 1, "well-formed line still parses");
        assert_eq!(walk.findings[0].token, ConcernToken::SpecCoherenceFail);
        assert_eq!(
            walk.finding_errors.len(),
            1,
            "backtick-wrapped line errored"
        );
        match &walk.finding_errors[0] {
            FindingParseError::Json { raw, .. } => {
                assert!(
                    raw.contains("not valid json"),
                    "raw payload preserved: {raw}",
                );
                assert!(
                    raw.starts_with('`'),
                    "raw line preserves the surrounding backticks: {raw}",
                );
            }
            other => panic!("expected Json error, got {other:?}"),
        }
        assert_eq!(
            walk.terminal,
            TerminalSurface::Complete,
            "well-formed terminator survives the per-line error",
        );
    }

    /// Per criterion
    /// `loom_finding_substring_match_requires_uppercase_and_colon_suffix`:
    /// the `LOOM_FINDING:` substring match is case-sensitive on the
    /// literal token plus the trailing colon. A bare-prose mention
    /// like `the LOOM_FINDING marker` (no colon) or `loom_finding:`
    /// (lowercase) does NOT trigger parsing. Pins the wire-format
    /// boundary from `specs/gate.md` § *Strict parse-time validation*.
    #[test]
    fn loom_finding_substring_match_requires_uppercase_and_colon_suffix() {
        let no_colon = "the LOOM_FINDING marker is mentioned in prose";
        let lowercase = format!("loom_finding: {}", "{\"token\":\"x\"}");
        let output = format!("{no_colon}\n{lowercase}\nLOOM_COMPLETE\n",);
        let walk = WalkOutput::from_stdout(&output, &AlwaysValid);
        assert!(
            walk.findings.is_empty(),
            "no findings parsed from bare-prose or lowercase mention: {:?}",
            walk.findings,
        );
        assert!(
            walk.finding_errors.is_empty(),
            "no errors either — those lines did not match the prefix",
        );
        assert_eq!(walk.terminal, TerminalSurface::Complete);
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

    /// Canonical [`FindingTarget`] for each [`ConcernToken`] variant —
    /// one representative shape per token. The exhaustive match is the
    /// closed-set guard: adding a new `ConcernToken` variant without
    /// updating this match is a compile error, which keeps the
    /// round-trip property test (below) honest as the enum grows.
    fn canonical_target(token: ConcernToken, gate: &SpecLabel) -> FindingTarget {
        match token {
            ConcernToken::SpecCoherenceFail => FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "verifier-honesty".to_owned(),
            },
            ConcernToken::VerifierTooNarrow => FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "cross-component-sufficiency".to_owned(),
            },
            ConcernToken::JudgeFlag => FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "judge-rubric".to_owned(),
            },
            ConcernToken::MultipleAnnotations => FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "atomic-acceptance".to_owned(),
            },
            ConcernToken::OrphanIntegration => FindingTarget::Contract {
                id: "molecule-lifecycle".to_owned(),
            },
            ConcernToken::StyleRuleViolation => FindingTarget::StyleRule {
                rule_id: "RS-12".to_owned(),
            },
            ConcernToken::VerifierBypass => FindingTarget::Annotation {
                target_string: "cargo test --lib verifier_bypass_case".to_owned(),
            },
            ConcernToken::WeakAssertion => FindingTarget::Annotation {
                target_string: "cargo test --lib weak_assertion_case".to_owned(),
            },
            ConcernToken::FabricatedResult => FindingTarget::Annotation {
                target_string: "cargo test --lib fabricated_result_case".to_owned(),
            },
            ConcernToken::CoincidentalPass => FindingTarget::Annotation {
                target_string: "cargo test --lib coincidental_pass_case".to_owned(),
            },
            ConcernToken::VerifierFailed => FindingTarget::Annotation {
                target_string: "cargo run -p loom-walk -- example".to_owned(),
            },
            ConcernToken::DispatchError => FindingTarget::Annotation {
                target_string: "missing-command --flag".to_owned(),
            },
            ConcernToken::UnresolvedAnnotation => FindingTarget::Annotation {
                target_string: "cargo run -p loom-walk -- unresolved".to_owned(),
            },
            ConcernToken::StubPointing => FindingTarget::Annotation {
                target_string: "cargo test --lib stub_pointing_case".to_owned(),
            },
            ConcernToken::UnneededPendingMarker => FindingTarget::Annotation {
                target_string: "cargo test --lib already_resolved".to_owned(),
            },
            ConcernToken::MockDiscipline => FindingTarget::TestPath {
                path: "crates/loom-gate/src/integrity.rs::mock_disciplined".to_owned(),
            },
            ConcernToken::ConcurrencyUntested => FindingTarget::LockSite {
                file: "crates/loom-workflow/src/run/runner.rs".to_owned(),
                line: 210,
            },
            ConcernToken::InvariantClash => FindingTarget::Invariant {
                spec: gate.clone(),
                section: "Out of Scope".to_owned(),
                tag: "loom-runs-podman".to_owned(),
            },
            ConcernToken::TemplateSpecDrift => FindingTarget::Template {
                path: "crates/loom-templates/templates/review.md".to_owned(),
            },
        }
    }

    /// Per criterion
    /// `every_finding_round_trips_through_wire_format_with_stable_fingerprint`:
    /// the closed set of `ConcernToken × canonical FindingTarget`
    /// pairings round-trips byte-equal through
    /// `serde_json::to_string` → `LOOM_FINDING:` line → synthetic walk
    /// output → [`parse_walk_output`], with fingerprint identical on
    /// either side. Pins the typed wire-format boundary at every cell
    /// of the closed token set so a future `ConcernToken` addition that
    /// drifts the wire shape (rename, target-variant swap, evidence
    /// reshape) breaks the cell that introduced the drift.
    #[test]
    fn every_finding_round_trips_through_wire_format_with_stable_fingerprint() {
        let gate = spec("gate");
        let tokens = [
            ConcernToken::SpecCoherenceFail,
            ConcernToken::OrphanIntegration,
            ConcernToken::StyleRuleViolation,
            ConcernToken::VerifierBypass,
            ConcernToken::WeakAssertion,
            ConcernToken::FabricatedResult,
            ConcernToken::CoincidentalPass,
            ConcernToken::MockDiscipline,
            ConcernToken::VerifierTooNarrow,
            ConcernToken::ConcurrencyUntested,
            ConcernToken::JudgeFlag,
            ConcernToken::InvariantClash,
            ConcernToken::TemplateSpecDrift,
            ConcernToken::VerifierFailed,
            ConcernToken::DispatchError,
            ConcernToken::UnresolvedAnnotation,
            ConcernToken::StubPointing,
            ConcernToken::MultipleAnnotations,
            ConcernToken::UnneededPendingMarker,
        ];
        let terminators = [
            "LOOM_COMPLETE",
            "LOOM_CONCERN: {\"summary\":\"round-trip\"}",
        ];

        for token in tokens {
            let target = canonical_target(token, &gate);
            assert_eq!(
                token.expected_target_kind(),
                target.kind(),
                "canonical pairing self-check for {}",
                token.as_wire(),
            );
            let bonds = match target.spec() {
                Some(s) => vec![s.clone()],
                None => vec![gate.clone()],
            };
            let input = Finding {
                token,
                bonds,
                target,
                evidence: format!("round-trip evidence for {}", token.as_wire()),
            };
            let payload = serde_json::to_string(&input).expect("serialize finding");

            for terminator in terminators {
                let output = format!(
                    "preamble\n{LOOM_FINDING_PREFIX} {payload}\nintermediate prose\n{terminator}\n",
                );
                let parsed = parse_walk_output(&output, &AlwaysValid).unwrap_or_else(|e| {
                    panic!(
                        "round-trip parse failed for {} with terminator `{terminator}`: {e}",
                        token.as_wire(),
                    )
                });
                let [round] = parsed.as_slice() else {
                    panic!(
                        "expected exactly one finding for {} with terminator `{terminator}`, got {parsed:?}",
                        token.as_wire(),
                    )
                };
                assert_eq!(
                    round,
                    &input,
                    "byte-equal struct round-trip for {} with terminator `{terminator}`",
                    token.as_wire(),
                );
                assert_eq!(
                    round.fingerprint(),
                    input.fingerprint(),
                    "fingerprint stability for {} with terminator `{terminator}`",
                    token.as_wire(),
                );
            }
        }
    }
}
