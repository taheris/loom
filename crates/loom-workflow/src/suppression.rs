use loom_driver::config::SuppressionConfig;
use loom_templates::finding::{ConcernToken, Finding};

pub fn suppresses_rubric_finding(suppressions: &[SuppressionConfig], finding: &Finding) -> bool {
    is_rubric_suppressible(finding.token()) && matching_suppression(suppressions, finding).is_some()
}

pub fn has_ineffective_suppression_match(
    suppressions: &[SuppressionConfig],
    finding: &Finding,
) -> bool {
    !is_rubric_suppressible(finding.token())
        && matching_suppression(suppressions, finding).is_some()
}

pub fn matching_suppression<'a>(
    suppressions: &'a [SuppressionConfig],
    finding: &Finding,
) -> Option<&'a SuppressionConfig> {
    suppressions
        .iter()
        .find(|entry| suppression_matches(entry, finding))
}

pub fn suppression_matches(entry: &SuppressionConfig, finding: &Finding) -> bool {
    entry.matches(&finding.id(), &finding.hash())
}

const fn is_rubric_suppressible(token: ConcernToken) -> bool {
    !matches!(
        token,
        ConcernToken::VerifierFailed
            | ConcernToken::DispatchError
            | ConcernToken::UnresolvedAnnotation
            | ConcernToken::StubPointing
            | ConcernToken::MultipleAnnotations
            | ConcernToken::UnneededPendingMarker
            | ConcernToken::InputsProtocolError
            | ConcernToken::PendingMarkerResolved
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_events::identifier::SpecLabel;
    use loom_templates::finding::FindingTarget;

    fn spec(s: &str) -> SpecLabel {
        s.parse().expect("valid spec label")
    }

    fn rubric_finding() -> Finding {
        loom_test_support::finding::resolve(loom_protocol::gate::RawFinding {
            token: ConcernToken::SpecCoherenceFail,
            route: crate::review::FindingRoute::Deferred,
            bonds: vec![spec("gate")],
            target: FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "verifier-honesty".to_owned(),
            },
            evidence: "evidence".to_owned(),
        })
        .expect("valid fixture finding")
    }

    fn deterministic_finding() -> Finding {
        loom_test_support::finding::resolve(loom_protocol::gate::RawFinding {
            token: ConcernToken::VerifierFailed,
            route: crate::review::FindingRoute::Deferred,
            bonds: vec![spec("gate")],
            target: FindingTarget::Annotation {
                target_string: "cargo test --lib failing_verifier".to_owned(),
            },
            evidence: "failed".to_owned(),
        })
        .expect("valid fixture finding")
    }

    #[test]
    fn suppressions_match_rubric_findings_by_id_or_hash() {
        let finding = rubric_finding();
        let by_id = SuppressionConfig::new(
            loom_driver::config::SuppressionSelector::Id(finding.id()),
            "false positive".to_owned(),
        )
        .unwrap();
        let by_hash = SuppressionConfig::new(
            loom_driver::config::SuppressionSelector::Hash(finding.hash()),
            "false positive".to_owned(),
        )
        .unwrap();
        assert!(suppresses_rubric_finding(&[by_id], &finding));
        assert!(suppresses_rubric_finding(&[by_hash], &finding));
    }

    #[test]
    fn suppressions_do_not_filter_deterministic_or_integrity_findings() {
        let finding = deterministic_finding();
        let entry = SuppressionConfig::new(
            loom_driver::config::SuppressionSelector::Id(finding.id()),
            "do not apply".to_owned(),
        )
        .unwrap();
        assert!(suppression_matches(&entry, &finding));
        assert!(!suppresses_rubric_finding(&[entry], &finding));
    }
}
