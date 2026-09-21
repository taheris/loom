//! Fixture-only symbol context; intrinsic finding invariants still run.
use loom_events::identifier::SpecLabel;
use loom_protocol::gate::{
    DispatchScope, Finding, FindingParseError, FindingValidator, RawFinding,
};

pub struct FixtureSymbols;

impl FindingValidator for FixtureSymbols {
    fn spec_label_is_known(&self, _: &SpecLabel) -> bool {
        true
    }
    fn criterion_anchor_resolves(&self, _: &SpecLabel, _: &str) -> bool {
        true
    }
    fn annotation_resolves(&self, _: &str) -> bool {
        true
    }
    fn file_exists(&self, _: &str) -> bool {
        true
    }
    fn invariant_resolves(&self, _: &SpecLabel, _: &str, _: &str) -> bool {
        true
    }
}

/// Resolve a test record against fixture-declared symbols at tree scope.
///
/// # Errors
/// Rejects malformed token/target/bond/route/evidence combinations.
pub fn resolve(raw: RawFinding) -> Result<Finding, FindingParseError> {
    raw.resolve(DispatchScope::Tree, &FixtureSymbols)
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_protocol::gate::{ConcernToken, FindingRoute, FindingTarget};

    #[test]
    fn fixture_resolution_never_bypasses_intrinsic_finding_invariants() {
        let raw = RawFinding {
            token: ConcernToken::SpecCoherenceFail,
            route: FindingRoute::Deferred,
            bonds: vec!["gate".parse().unwrap()],
            target: FindingTarget::Criterion {
                spec: "gate".parse().unwrap(),
                anchor: "fixture".into(),
            },
            evidence: "observed failure".into(),
        };
        assert!(resolve(raw.clone()).is_ok());
        let invalid = RawFinding {
            target: FindingTarget::Annotation {
                target_string: "fixture".into(),
            },
            ..raw
        };
        assert!(resolve(invalid).is_err());
    }
}
