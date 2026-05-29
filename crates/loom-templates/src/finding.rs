//! Typed [`Finding`] record carried by [`crate::PreviousFailure::ReviewConcern`].
//!
//! The Finding surface is spec-owned by `loom-workflow` per
//! [`specs/gate.md` § Findings and Minting](../../../specs/gate.md). It
//! lives in `loom-templates` because the typed retry-context surface
//! (`PreviousFailure::ReviewConcern { findings: Vec<Finding> }`) carries
//! it as a field; `loom-workflow` re-exports the types from here so
//! the mint pipeline references a single canonical location.
//!
//! Identity is `(token, target)` only: [`Finding::fingerprint`]
//! deliberately excludes [`Finding::bonds`] so the bonding lead can
//! shift across runs without invalidating the dedup key.

use displaydoc::Display;
use loom_events::identifier::SpecLabel;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    #[serde(rename = "unneeded-pending-marker")]
    UnneededPendingMarker,
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
            Self::UnneededPendingMarker => "unneeded-pending-marker",
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
            | Self::StubPointing
            | Self::UnneededPendingMarker => TargetKind::Annotation,
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

/// Resolver injected into the walk-level parser for the I/O-bearing
/// validation layers — Layer 3 (spec-label resolution) and Layer 5
/// (target-content resolution). Wrapping these in a trait keeps the
/// parser unit-testable against synthetic fixtures and lets the mint
/// driver plug in the real on-disk implementations.
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
/// has the evidence it needs (per `specs/gate.md` § *Strict
/// parse-time validation*).
///
/// Clone / PartialEq / Eq are required so this error type can ride
/// inside `BadWalk::MalformedFinding { errors: Vec<FindingParseError>,
/// .. }` (and through `ExitSignal::BadWalk`); for the `Json` variant the
/// `source` field is the rendered `serde_json::Error` message rather
/// than the typed error, so the chain via `std::error::Error::source`
/// is the rendered string.
#[derive(Debug, Display, Error, Clone, PartialEq, Eq)]
pub enum FindingParseError {
    /// line {line_number}: LOOM_FINDING payload is not valid JSON ({message}) — `{raw}`
    Json {
        line_number: usize,
        raw: String,
        message: String,
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
                message: source.to_string(),
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
    fn fingerprint_is_stable_across_runs_for_same_finding() {
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
        assert_eq!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn fingerprint_excludes_bonds() {
        let identity = (
            ConcernToken::OrphanIntegration,
            FindingTarget::Contract {
                id: "molecule-lifecycle".to_owned(),
            },
        );
        let single_spec = finding(identity.0, vec![spec("harness")], identity.1.clone(), "");
        let multi_spec = finding(
            identity.0,
            vec![spec("gate"), spec("harness")],
            identity.1,
            "",
        );
        assert_eq!(single_spec.fingerprint(), multi_spec.fingerprint());
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
    fn concern_token_expected_target_kind_table() {
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
            (ConcernToken::UnneededPendingMarker, TargetKind::Annotation),
        ] {
            assert_eq!(token.expected_target_kind(), expected, "{token:?}");
        }
    }
}
