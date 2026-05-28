//! Typed [`Finding`] record consumed by the mint pipeline.
//!
//! Findings reach mint from two sources — the LLM rubric walk's
//! `LOOM_FINDING:` stdout lines and the deterministic verifier-runner's
//! normalised verdicts — and converge on this single in-driver
//! representation per `specs/gate.md` §"Concern tokens and target
//! variants".
//!
//! Identity is `(token, target)` only: the [`Finding::fingerprint`]
//! deliberately excludes [`Finding::bonds`] so the bonding lead can
//! shift across runs without invalidating the dedup key (`bonds[0]`'s
//! epic closing should not re-mint a finding that already has an open
//! fix-up bead).

use displaydoc::Display;
use loom_events::identifier::SpecLabel;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::todo::parse_exit_signal;

/// One concern raised by either the LLM rubric or a deterministic
/// verifier, in the shape the mint pipeline consumes.
///
/// `bonds` is bonding metadata (which spec molecules the fix-up should
/// route to); `target` is identity metadata (what the finding is
/// about). The two are kept structurally separate so the driver can
/// shift bonding without invalidating the fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub token: ConcernToken,
    pub bonds: Vec<SpecLabel>,
    pub target: FindingTarget,
    pub evidence: String,
}

impl Finding {
    /// 12-char lowercase-hex stable identifier from
    /// `blake3(token-wire || 0x1F || canonical_form(target))`.
    /// Stability across rubric runs is the load-bearing property;
    /// `bonds` is deliberately omitted so a bonding shift on re-walk
    /// dedups against the existing fix-up bead instead of re-minting.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut input = String::new();
        input.push_str(self.token.as_wire());
        input.push('\u{001F}');
        input.push_str(&self.target.canonical_form());
        let hash = blake3::hash(input.as_bytes());
        let hex = hash.to_hex();
        hex.as_str()[..FINGERPRINT_HEX_LEN].to_owned()
    }
}

/// Length of the [`Finding::fingerprint`] in hex characters. Chosen to
/// fit a bd label and to keep the fingerprint visually scannable
/// alongside bead ids; the only invariant is stability across runs.
const FINGERPRINT_HEX_LEN: usize = 12;

/// Closed-set concern tokens emitted by the rubric walk or normalised
/// from a deterministic verifier verdict.
///
/// The wire string (e.g. `spec-coherence-fail`) is the canonical name
/// across the `LOOM_FINDING:` JSON, bd labels, and log surfaces; the
/// Rust variant name is the same with kebab-case lowered to
/// PascalCase. `scope-creep` and `scope-shortfall` are deliberately
/// absent — they are per-bead tokens the tree-scope walk never emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConcernToken {
    #[serde(rename = "spec-coherence-fail")]
    SpecCoherenceFail,
    #[serde(rename = "orphan-integration")]
    OrphanIntegration,
    #[serde(rename = "style-rule-violation")]
    StyleRuleViolation,
    #[serde(rename = "verifier-bypass")]
    VerifierBypass,
    #[serde(rename = "weak-assertion")]
    WeakAssertion,
    #[serde(rename = "fabricated-result")]
    FabricatedResult,
    #[serde(rename = "coincidental-pass")]
    CoincidentalPass,
    #[serde(rename = "mock-discipline")]
    MockDiscipline,
    #[serde(rename = "verifier-too-narrow")]
    VerifierTooNarrow,
    #[serde(rename = "concurrency-untested")]
    ConcurrencyUntested,
    #[serde(rename = "judge-flag")]
    JudgeFlag,
    #[serde(rename = "invariant-clash")]
    InvariantClash,
    #[serde(rename = "template-spec-drift")]
    TemplateSpecDrift,
    #[serde(rename = "verifier-failed")]
    VerifierFailed,
    #[serde(rename = "dispatch-error")]
    DispatchError,
    #[serde(rename = "unresolved-annotation")]
    UnresolvedAnnotation,
    #[serde(rename = "stub-pointing")]
    StubPointing,
    #[serde(rename = "multiple-annotations")]
    MultipleAnnotations,
}

impl ConcernToken {
    /// Canonical wire string used in `LOOM_FINDING:` JSON, bd labels,
    /// and fingerprint input. Matches the leftmost column in
    /// `specs/gate.md` §"Concern tokens and target variants".
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::SpecCoherenceFail => "spec-coherence-fail",
            Self::OrphanIntegration => "orphan-integration",
            Self::StyleRuleViolation => "style-rule-violation",
            Self::VerifierBypass => "verifier-bypass",
            Self::WeakAssertion => "weak-assertion",
            Self::FabricatedResult => "fabricated-result",
            Self::CoincidentalPass => "coincidental-pass",
            Self::MockDiscipline => "mock-discipline",
            Self::VerifierTooNarrow => "verifier-too-narrow",
            Self::ConcurrencyUntested => "concurrency-untested",
            Self::JudgeFlag => "judge-flag",
            Self::InvariantClash => "invariant-clash",
            Self::TemplateSpecDrift => "template-spec-drift",
            Self::VerifierFailed => "verifier-failed",
            Self::DispatchError => "dispatch-error",
            Self::UnresolvedAnnotation => "unresolved-annotation",
            Self::StubPointing => "stub-pointing",
            Self::MultipleAnnotations => "multiple-annotations",
        }
    }

    /// Target-variant the token MUST carry per `specs/gate.md`
    /// §"Concern tokens and target variants". The parser rejects any
    /// `LOOM_FINDING:` payload whose `target.kind` does not equal the
    /// value returned here.
    #[must_use]
    pub fn expected_target_kind(self) -> TargetKind {
        match self {
            Self::SpecCoherenceFail
            | Self::VerifierTooNarrow
            | Self::JudgeFlag
            | Self::MultipleAnnotations => TargetKind::Criterion,
            Self::OrphanIntegration => TargetKind::Contract,
            Self::StyleRuleViolation => TargetKind::StyleRule,
            Self::VerifierBypass
            | Self::WeakAssertion
            | Self::FabricatedResult
            | Self::CoincidentalPass
            | Self::VerifierFailed
            | Self::DispatchError
            | Self::UnresolvedAnnotation
            | Self::StubPointing => TargetKind::Annotation,
            Self::MockDiscipline => TargetKind::TestPath,
            Self::ConcurrencyUntested => TargetKind::LockSite,
            Self::InvariantClash => TargetKind::Invariant,
            Self::TemplateSpecDrift => TargetKind::Template,
        }
    }
}

/// Discriminator tag for [`FindingTarget`]. Matches the `kind` field in
/// the wire JSON one-to-one; the parser compares
/// [`ConcernToken::expected_target_kind`] against [`FindingTarget::kind`]
/// to enforce the token/variant alignment from `specs/gate.md`
/// §"Concern tokens and target variants".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Criterion,
    Contract,
    StyleRule,
    Annotation,
    TestPath,
    LockSite,
    Invariant,
    Template,
}

impl TargetKind {
    /// Canonical wire string — matches the `#[serde(tag = "kind")]`
    /// variant name in [`FindingTarget`], so error messages and the wire
    /// payload agree byte-for-byte.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Criterion => "Criterion",
            Self::Contract => "Contract",
            Self::StyleRule => "StyleRule",
            Self::Annotation => "Annotation",
            Self::TestPath => "TestPath",
            Self::LockSite => "LockSite",
            Self::Invariant => "Invariant",
            Self::Template => "Template",
        }
    }
}

impl std::fmt::Display for TargetKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// Identity-bearing payload that narrows what the finding is about.
///
/// The variant is selected by `kind` in the wire JSON
/// (`#[serde(tag = "kind")]`). Each token in [`ConcernToken`] is
/// paired with a specific variant per `specs/gate.md`
/// §"Concern tokens and target variants" — the parser enforces
/// token/variant alignment so mismatch is rejected at the wire boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum FindingTarget {
    Criterion {
        spec: SpecLabel,
        anchor: String,
    },
    Contract {
        id: String,
    },
    StyleRule {
        rule_id: String,
    },
    Annotation {
        target_string: String,
    },
    TestPath {
        path: String,
    },
    LockSite {
        file: String,
        line: u32,
    },
    Invariant {
        spec: SpecLabel,
        section: String,
        tag: String,
    },
    Template {
        path: String,
    },
}

impl FindingTarget {
    /// Discriminator equivalent to the `kind` field in the wire JSON.
    /// Used by the parser to enforce token/variant alignment per
    /// `specs/gate.md` §"Concern tokens and target variants".
    #[must_use]
    pub fn kind(&self) -> TargetKind {
        match self {
            Self::Criterion { .. } => TargetKind::Criterion,
            Self::Contract { .. } => TargetKind::Contract,
            Self::StyleRule { .. } => TargetKind::StyleRule,
            Self::Annotation { .. } => TargetKind::Annotation,
            Self::TestPath { .. } => TargetKind::TestPath,
            Self::LockSite { .. } => TargetKind::LockSite,
            Self::Invariant { .. } => TargetKind::Invariant,
            Self::Template { .. } => TargetKind::Template,
        }
    }

    /// The spec field, when this variant carries one. `Criterion` and
    /// `Invariant` are the only variants whose target identity is bound
    /// to a spec; the parser uses this to enforce the
    /// "target.spec MUST appear in bonds" rule from `specs/gate.md`
    /// § *Findings and Minting*.
    #[must_use]
    pub fn spec(&self) -> Option<&SpecLabel> {
        match self {
            Self::Criterion { spec, .. } | Self::Invariant { spec, .. } => Some(spec),
            Self::Contract { .. }
            | Self::StyleRule { .. }
            | Self::Annotation { .. }
            | Self::TestPath { .. }
            | Self::LockSite { .. }
            | Self::Template { .. } => None,
        }
    }

    /// Variant-aware canonical string fed into the fingerprint hash.
    /// Round-trips the identity-bearing fields in a fixed shape so the
    /// same logical finding hashes to the same digest regardless of
    /// how the rubric phrased it.
    #[must_use]
    pub fn canonical_form(&self) -> String {
        match self {
            Self::Criterion { spec, anchor } => format!("criterion:{spec}:{anchor}"),
            Self::Contract { id } => format!("contract:{id}"),
            Self::StyleRule { rule_id } => format!("style:{rule_id}"),
            Self::Annotation { target_string } => format!("annotation:{target_string}"),
            Self::TestPath { path } => format!("test:{path}"),
            Self::LockSite { file, line } => format!("lock:{file}:{line}"),
            Self::Invariant { spec, section, tag } => {
                format!("invariant:{spec}:{section}:{tag}")
            }
            Self::Template { path } => format!("template:{path}"),
        }
    }
}

/// Wire-format prefix the LLM rubric emits before each finding's JSON
/// payload (per `specs/gate.md` § *Emit shape*). Matches the prefix
/// shape `parse_review_flag` / `parse_exit_signal` use for the marker
/// surface so consumers can scan a single agent-stdout buffer for both.
pub const LOOM_FINDING_PREFIX: &str = "LOOM_FINDING:";

/// Resolver injected into [`parse_walk_output`] / [`Finding::validate`]
/// for the I/O-bearing validation layers — Layer 3 (spec-label
/// resolution) and Layer 5 (target-content resolution). Wrapping these
/// in a trait keeps the parser unit-testable against synthetic fixtures
/// and lets the mint driver plug in the real on-disk implementations
/// (the integrity gate's forward-resolver for `Annotation`, a
/// spec-directory lookup for `Criterion`/`Invariant`, file-existence
/// checks for `TestPath` / `Template` / `LockSite`).
pub trait FindingValidator {
    /// Layer 3 — every element of `bonds` MUST be a known workspace
    /// spec label (the basename of a file under `specs/` minus `.md`).
    fn spec_label_is_known(&self, label: &SpecLabel) -> bool;

    /// Layer 5 — `Criterion { spec, anchor }` resolves when the spec
    /// file contains the named anchor.
    fn criterion_anchor_resolves(&self, spec: &SpecLabel, anchor: &str) -> bool;

    /// Layer 5 — `Annotation { target_string }` resolves when the
    /// integrity gate's forward-resolution would accept the same string
    /// as a valid annotation target (command on PATH, test name in
    /// scope, judge file on disk).
    fn annotation_resolves(&self, target_string: &str) -> bool;

    /// Layer 5 — `TestPath { path }` / `Template { path }` /
    /// `LockSite { file, .. }` resolve when the named file exists on
    /// disk (relative to repo root).
    fn file_exists(&self, path: &str) -> bool;

    /// Layer 5 — `Invariant { spec, section, tag }` resolves when the
    /// spec file declares an invariant matching `(section, tag)`.
    fn invariant_resolves(&self, spec: &SpecLabel, section: &str, tag: &str) -> bool;
}

/// Per-layer parse / validation failure. Every variant carries the
/// offending line's 1-based number and verbatim text so a re-run prompt
/// has the evidence it needs (per the bead's "Error type names the
/// offending line" deliverable and `specs/gate.md` § *Strict
/// parse-time validation*).
#[derive(Debug, Display, Error)]
pub enum FindingParseError {
    /// line {line_number}: LOOM_FINDING payload is not valid JSON ({source}) — `{raw}`
    Json {
        line_number: usize,
        raw: String,
        #[source]
        source: serde_json::Error,
    },
    /// line {line_number}: bonds element `{spec}` does not resolve to a workspace spec — `{raw}`
    UnknownBondSpec {
        line_number: usize,
        spec: String,
        raw: String,
    },
    /// line {line_number}: target.kind `{actual}` does not match token `{token}` (expected `{expected}`) — `{raw}`
    TokenVariantMismatch {
        line_number: usize,
        token: &'static str,
        expected: TargetKind,
        actual: TargetKind,
        raw: String,
    },
    /// line {line_number}: target.spec `{spec}` not present in bonds {bonds:?} — `{raw}`
    TargetSpecNotInBonds {
        line_number: usize,
        spec: String,
        bonds: Vec<String>,
        raw: String,
    },
    /// line {line_number}: target content does not resolve — {detail} — `{raw}`
    UnresolvedTarget {
        line_number: usize,
        detail: String,
        raw: String,
    },
}

/// Top-level error for [`parse_walk_output`]. Either a per-line
/// validation failure (above) or the terminal-marker enforcement —
/// a walk that emits `LOOM_FINDING:` lines without a terminal marker
/// per `specs/gate.md` § *Findings and Minting*.
#[derive(Debug, Display, Error)]
pub enum WalkOutputError {
    /// per-line parse failure: {0}
    Finding(#[from] FindingParseError),
    /// walk emitted {findings_count} LOOM_FINDING line(s) but no terminal marker (LOOM_COMPLETE / LOOM_CONCERN / LOOM_BLOCKED / LOOM_CLARIFY)
    MissingTerminalMarker { findings_count: usize },
}

impl Finding {
    /// Pure parse for a single `LOOM_FINDING:` payload: JSON syntax
    /// (Layer 1), token/kind closed-set membership (Layer 2; enforced
    /// by `serde` deserialization since both are `#[serde(rename = ...)]`
    /// enums), token/variant alignment (Layer 4), and the
    /// "target.spec ∈ bonds" rule. Layers 3 and 5 are deferred to
    /// [`Finding::validate`] because they require I/O context.
    ///
    /// `line_number` is the 1-based line offset in the agent's stdout
    /// buffer; included in every error variant so the caller can quote
    /// the offending line back to the agent on a re-run.
    pub fn parse_payload(
        payload: &str,
        line_number: usize,
        raw_line: &str,
    ) -> Result<Self, FindingParseError> {
        let finding: Finding =
            serde_json::from_str(payload).map_err(|source| FindingParseError::Json {
                line_number,
                raw: raw_line.to_owned(),
                source,
            })?;

        let expected_kind = finding.token.expected_target_kind();
        let actual_kind = finding.target.kind();
        if expected_kind != actual_kind {
            return Err(FindingParseError::TokenVariantMismatch {
                line_number,
                token: finding.token.as_wire(),
                expected: expected_kind,
                actual: actual_kind,
                raw: raw_line.to_owned(),
            });
        }

        if let Some(target_spec) = finding.target.spec()
            && !finding.bonds.contains(target_spec)
        {
            return Err(FindingParseError::TargetSpecNotInBonds {
                line_number,
                spec: target_spec.to_string(),
                bonds: finding.bonds.iter().map(ToString::to_string).collect(),
                raw: raw_line.to_owned(),
            });
        }

        Ok(finding)
    }

    /// I/O-bearing validation: Layer 3 (every bond resolves to a known
    /// workspace spec) and Layer 5 (the target's identity-bearing
    /// fields resolve on disk). Pure JSON / closed-set / variant /
    /// bonds-spec rules already fired in [`Finding::parse_payload`];
    /// this is the second stage that the mint driver runs once the
    /// resolver is wired.
    pub fn validate<V: FindingValidator + ?Sized>(
        &self,
        line_number: usize,
        raw_line: &str,
        validator: &V,
    ) -> Result<(), FindingParseError> {
        for bond in &self.bonds {
            if !validator.spec_label_is_known(bond) {
                return Err(FindingParseError::UnknownBondSpec {
                    line_number,
                    spec: bond.to_string(),
                    raw: raw_line.to_owned(),
                });
            }
        }

        let resolved = match &self.target {
            FindingTarget::Criterion { spec, anchor } => {
                validator.criterion_anchor_resolves(spec, anchor)
            }
            FindingTarget::Annotation { target_string } => {
                validator.annotation_resolves(target_string)
            }
            FindingTarget::TestPath { path } | FindingTarget::Template { path } => {
                validator.file_exists(path)
            }
            FindingTarget::LockSite { file, .. } => validator.file_exists(file),
            FindingTarget::Invariant { spec, section, tag } => {
                validator.invariant_resolves(spec, section, tag)
            }
            FindingTarget::Contract { .. } | FindingTarget::StyleRule { .. } => true,
        };
        if !resolved {
            return Err(FindingParseError::UnresolvedTarget {
                line_number,
                detail: target_unresolved_detail(&self.target),
                raw: raw_line.to_owned(),
            });
        }
        Ok(())
    }
}

fn target_unresolved_detail(target: &FindingTarget) -> String {
    match target {
        FindingTarget::Criterion { spec, anchor } => {
            format!("criterion `{anchor}` not present in spec `{spec}`")
        }
        FindingTarget::Annotation { target_string } => {
            format!("annotation target `{target_string}` does not resolve")
        }
        FindingTarget::TestPath { path } | FindingTarget::Template { path } => {
            format!("file `{path}` not found on disk")
        }
        FindingTarget::LockSite { file, line } => {
            format!("lock-site file `{file}` (line {line}) not found on disk")
        }
        FindingTarget::Invariant { spec, section, tag } => {
            format!("invariant `{tag}` in section `{section}` not present in spec `{spec}`",)
        }
        FindingTarget::Contract { .. } | FindingTarget::StyleRule { .. } => {
            "no resolver registered for this variant".to_owned()
        }
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

    fn spec(s: &str) -> SpecLabel {
        s.parse().expect("valid spec label")
    }

    fn finding(
        token: ConcernToken,
        bonds: Vec<SpecLabel>,
        target: FindingTarget,
        evidence: &str,
    ) -> Finding {
        Finding {
            token,
            bonds,
            target,
            evidence: evidence.to_owned(),
        }
    }

    fn sample_finding() -> Finding {
        finding(
            ConcernToken::SpecCoherenceFail,
            vec![spec("gate")],
            FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "verifier-honesty".to_owned(),
            },
            "criterion's annotation does not exercise the contract",
        )
    }

    #[test]
    fn fingerprint_is_twelve_lowercase_hex_chars() {
        let fp = sample_finding().fingerprint();
        assert_eq!(fp.len(), 12, "fingerprint width is 12 hex chars");
        assert!(
            fp.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "fingerprint is lowercase hex: {fp}",
        );
    }

    #[test]
    fn mint_fingerprint_is_stable_across_rubric_runs_for_same_finding() {
        // Spec contract (gate.md:1404-1408): the same `token` +
        // canonicalised `target` must produce the same 12-char hash
        // regardless of how `bonds` is ordered or which spec wins
        // lead-selection. The fingerprint is the dedup key, so any
        // instability would cause a re-mint on every walk.
        let a = finding(
            ConcernToken::SpecCoherenceFail,
            vec![spec("gate"), spec("harness")],
            FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "verifier-honesty".to_owned(),
            },
            "first walk phrasing",
        );
        let b = finding(
            ConcernToken::SpecCoherenceFail,
            vec![spec("harness"), spec("gate")],
            FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "verifier-honesty".to_owned(),
            },
            "second walk phrasing, identical identity",
        );
        assert_eq!(
            a.fingerprint(),
            b.fingerprint(),
            "same (token, target) hashes the same across runs",
        );
    }

    #[test]
    fn mint_fingerprint_excludes_bonds_so_bonding_shifts_do_not_remint() {
        // Spec contract (gate.md:1419-1424): the fingerprint depends on
        // `token` and `canonical_form(target)` only — never on `bonds`.
        // The same finding re-emitted with a different bonds ordering
        // or a different lead-spec must resolve to the same fingerprint
        // and dedup against the existing fix-up bead. This is the
        // smoking-gun mistake the architecture explicitly avoids.
        let identity = (
            ConcernToken::OrphanIntegration,
            FindingTarget::Contract {
                id: "molecule-lifecycle".to_owned(),
            },
        );
        let single_spec = finding(
            identity.0,
            vec![spec("harness")],
            identity.1.clone(),
            "single-spec bonding",
        );
        let multi_spec_one_order = finding(
            identity.0,
            vec![spec("gate"), spec("harness")],
            identity.1.clone(),
            "multi-spec bonding, order A",
        );
        let multi_spec_other_order = finding(
            identity.0,
            vec![spec("harness"), spec("gate")],
            identity.1.clone(),
            "multi-spec bonding, order B",
        );
        let single_other_spec = finding(
            identity.0,
            vec![spec("gate")],
            identity.1,
            "single-spec, different lead",
        );
        let fp = single_spec.fingerprint();
        assert_eq!(multi_spec_one_order.fingerprint(), fp);
        assert_eq!(multi_spec_other_order.fingerprint(), fp);
        assert_eq!(single_other_spec.fingerprint(), fp);
    }

    #[test]
    fn fingerprint_differs_when_token_changes() {
        let target = FindingTarget::Contract {
            id: "molecule-lifecycle".to_owned(),
        };
        let a = finding(
            ConcernToken::OrphanIntegration,
            vec![spec("harness")],
            target.clone(),
            "",
        );
        let b = finding(
            ConcernToken::WeakAssertion,
            vec![spec("harness")],
            target,
            "",
        );
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn fingerprint_differs_when_target_identity_changes() {
        let a = finding(
            ConcernToken::SpecCoherenceFail,
            vec![spec("gate")],
            FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "verifier-honesty".to_owned(),
            },
            "",
        );
        let b = finding(
            ConcernToken::SpecCoherenceFail,
            vec![spec("gate")],
            FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "molecule-lifecycle".to_owned(),
            },
            "",
        );
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn canonical_form_per_variant_matches_spec_shapes() {
        assert_eq!(
            FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "verifier-honesty".to_owned(),
            }
            .canonical_form(),
            "criterion:gate:verifier-honesty",
        );
        assert_eq!(
            FindingTarget::Contract {
                id: "molecule-lifecycle".to_owned(),
            }
            .canonical_form(),
            "contract:molecule-lifecycle",
        );
        assert_eq!(
            FindingTarget::StyleRule {
                rule_id: "RS-12".to_owned(),
            }
            .canonical_form(),
            "style:RS-12",
        );
        assert_eq!(
            FindingTarget::Annotation {
                target_string: "cargo run -- foo".to_owned(),
            }
            .canonical_form(),
            "annotation:cargo run -- foo",
        );
        assert_eq!(
            FindingTarget::TestPath {
                path: "crates/loom-gate/src/integrity.rs::test_x".to_owned(),
            }
            .canonical_form(),
            "test:crates/loom-gate/src/integrity.rs::test_x",
        );
        assert_eq!(
            FindingTarget::LockSite {
                file: "crates/loom-workflow/src/run/runner.rs".to_owned(),
                line: 210,
            }
            .canonical_form(),
            "lock:crates/loom-workflow/src/run/runner.rs:210",
        );
        assert_eq!(
            FindingTarget::Invariant {
                spec: spec("harness"),
                section: "Out of Scope".to_owned(),
                tag: "loom-runs-podman".to_owned(),
            }
            .canonical_form(),
            "invariant:harness:Out of Scope:loom-runs-podman",
        );
        assert_eq!(
            FindingTarget::Template {
                path: "crates/loom-templates/templates/review.md".to_owned(),
            }
            .canonical_form(),
            "template:crates/loom-templates/templates/review.md",
        );
    }

    #[test]
    fn concern_token_serde_uses_wire_kebab_case() {
        let token = ConcernToken::SpecCoherenceFail;
        let json = serde_json::to_string(&token).expect("serialize");
        assert_eq!(json, "\"spec-coherence-fail\"");
        let back: ConcernToken = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, token);
    }

    #[test]
    fn concern_token_wire_matches_as_wire_for_each_variant() {
        for token in [
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
        ] {
            let json = serde_json::to_string(&token).expect("serialize");
            let expected = format!("\"{}\"", token.as_wire());
            assert_eq!(json, expected, "{token:?}");
        }
    }

    #[test]
    fn finding_target_serde_uses_tagged_kind_discriminator() {
        let target = FindingTarget::Criterion {
            spec: spec("gate"),
            anchor: "verifier-honesty".to_owned(),
        };
        let json = serde_json::to_value(&target).expect("serialize");
        assert_eq!(json["kind"], "Criterion");
        assert_eq!(json["spec"], "gate");
        assert_eq!(json["anchor"], "verifier-honesty");
        let back: FindingTarget = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, target);
    }

    #[test]
    fn finding_round_trips_through_loom_finding_payload_shape() {
        let original = finding(
            ConcernToken::OrphanIntegration,
            vec![spec("harness"), spec("gate")],
            FindingTarget::Contract {
                id: "molecule-lifecycle".to_owned(),
            },
            "contract closure broken",
        );
        let json = serde_json::to_string(&original).expect("serialize");
        let back: Finding = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, original);
        assert_eq!(back.fingerprint(), original.fingerprint());
    }

    /// Lenient validator used by parser tests that care only about
    /// pure parse rules (Layers 1, 2, 4, and the target.spec ∈ bonds
    /// check). Every Layer 3 / Layer 5 check passes.
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

    /// Validator that knows a fixed set of workspace specs and otherwise
    /// resolves every Layer 5 check. Used to exercise the
    /// "bond does not resolve" branch.
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

    /// Validator that rejects every Layer 5 resolution. Used to
    /// exercise the "target content unresolved" branch.
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

    /// Spec contract `specs/gate.md` § *Findings and Minting*: the walk
    /// emits one JSON object per `LOOM_FINDING:` line and the parser
    /// surfaces each as a typed `Finding` in stdout order.
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

    /// Spec contract: a walk that emits `LOOM_FINDING:` lines without a
    /// terminal marker fails the mint run.
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

    /// Spec contract: zero `LOOM_FINDING:` lines + no terminal marker
    /// is NOT an error — the terminal-marker enforcement applies only
    /// when at least one finding has been emitted.
    #[test]
    fn mint_walk_without_findings_does_not_require_terminal_marker() {
        let output = "preamble with no findings and no markers\n";
        let findings = parse_walk_output(output, &AlwaysValid).expect("vacuous case");
        assert!(findings.is_empty());
    }

    /// Spec contract: a walk that terminates with `LOOM_COMPLETE` /
    /// `LOOM_BLOCKED` / `LOOM_CLARIFY` (not just `LOOM_CONCERN`) is
    /// accepted alongside its findings.
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

    /// Spec contract: the driver parses `LOOM_FINDING:` JSON payloads
    /// via `serde_json` into typed `Finding` records; `target`
    /// deserializes as an internally-tagged enum whose variant is
    /// selected by `kind`, validated against the `token`'s expected
    /// variant.
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
        assert_eq!(
            parsed.token.expected_target_kind(),
            parsed.target.kind(),
            "token's expected_target_kind matches the parsed target's kind",
        );
    }

    /// Spec contract: a malformed `LOOM_FINDING:` line (invalid JSON,
    /// unknown token, unknown spec, target variant mismatching token,
    /// or unresolved target content) fails the mint invocation with
    /// a typed parse error naming the offending line. No silent skip.
    #[test]
    fn mint_malformed_loom_finding_fails_run_with_typed_error() {
        let valid_terminal = "LOOM_CONCERN: orphan-integration -- summary";

        // Layer 1: invalid JSON.
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

        // Layer 2: unknown token (surfaces via the closed-set serde rename, so reaches Json).
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

        // Layer 3: unknown spec in bonds.
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

        // Layer 4: target variant mismatches token.
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

        // Layer 5: target content does not resolve.
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

    /// Spec contract: for target variants that carry a spec field
    /// (currently `Criterion` and `Invariant`), `target.spec` MUST
    /// appear in `bonds`; a finding that violates this is rejected as
    /// a typed parse error.
    #[test]
    fn mint_rejects_criterion_target_whose_spec_is_not_in_bonds() {
        // Criterion target.spec=gate but bonds list ["harness"] only.
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

        // The same rule applies to Invariant targets.
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

        // Sanity: when target.spec IS in bonds, the rule does not fire.
        let line = finding_line(
            "spec-coherence-fail",
            &["harness", "gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}"#,
            "criterion belongs to gate, bonds names both",
        );
        let output = format!("{line}\nLOOM_CONCERN: spec-coherence-fail -- ok\n");
        let findings = parse_walk_output(&output, &AlwaysValid).expect("should parse");
        assert_eq!(findings.len(), 1);
    }

    /// Every variant of [`ConcernToken`] returns an
    /// [`expected_target_kind`] aligned with the matching variant of
    /// [`FindingTarget`]. Pins the table from `specs/gate.md`
    /// §"Concern tokens and target variants".
    #[test]
    fn concern_token_expected_target_kind_matches_specs_gate_table() {
        for (token, expected) in [
            (ConcernToken::SpecCoherenceFail, TargetKind::Criterion),
            (ConcernToken::OrphanIntegration, TargetKind::Contract),
            (ConcernToken::StyleRuleViolation, TargetKind::StyleRule),
            (ConcernToken::VerifierBypass, TargetKind::Annotation),
            (ConcernToken::WeakAssertion, TargetKind::Annotation),
            (ConcernToken::FabricatedResult, TargetKind::Annotation),
            (ConcernToken::CoincidentalPass, TargetKind::Annotation),
            (ConcernToken::MockDiscipline, TargetKind::TestPath),
            (ConcernToken::VerifierTooNarrow, TargetKind::Criterion),
            (ConcernToken::ConcurrencyUntested, TargetKind::LockSite),
            (ConcernToken::JudgeFlag, TargetKind::Criterion),
            (ConcernToken::InvariantClash, TargetKind::Invariant),
            (ConcernToken::TemplateSpecDrift, TargetKind::Template),
            (ConcernToken::VerifierFailed, TargetKind::Annotation),
            (ConcernToken::DispatchError, TargetKind::Annotation),
            (ConcernToken::UnresolvedAnnotation, TargetKind::Annotation),
            (ConcernToken::StubPointing, TargetKind::Annotation),
            (ConcernToken::MultipleAnnotations, TargetKind::Criterion),
        ] {
            assert_eq!(
                token.expected_target_kind(),
                expected,
                "{token:?} expected_target_kind",
            );
        }
    }
}
