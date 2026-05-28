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

use loom_events::identifier::SpecLabel;
use serde::{Deserialize, Serialize};

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
}
