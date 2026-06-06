use loom_driver::config::SuppressionConfig;
use loom_templates::finding::{ConcernToken, Finding};

pub(crate) fn suppresses_rubric_finding(
    suppressions: &[SuppressionConfig],
    finding: &Finding,
) -> bool {
    is_rubric_suppressible(finding.token) && matching_suppression(suppressions, finding).is_some()
}

pub(crate) fn has_ineffective_suppression_match(
    suppressions: &[SuppressionConfig],
    finding: &Finding,
) -> bool {
    !is_rubric_suppressible(finding.token) && matching_suppression(suppressions, finding).is_some()
}

pub(crate) fn matching_suppression<'a>(
    suppressions: &'a [SuppressionConfig],
    finding: &Finding,
) -> Option<&'a SuppressionConfig> {
    suppressions
        .iter()
        .find(|entry| suppression_matches(entry, finding))
}

pub(crate) fn suppression_matches(entry: &SuppressionConfig, finding: &Finding) -> bool {
    entry.id.as_deref().is_some_and(|id| id == finding.id())
        || entry
            .hash
            .as_deref()
            .is_some_and(|hash| hash == finding.hash())
}

fn is_rubric_suppressible(token: ConcernToken) -> bool {
    !matches!(
        token,
        ConcernToken::VerifierFailed
            | ConcernToken::DispatchError
            | ConcernToken::UnresolvedAnnotation
            | ConcernToken::StubPointing
            | ConcernToken::MultipleAnnotations
            | ConcernToken::UnneededPendingMarker
            | ConcernToken::InputsProtocolError
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
        Finding {
            token: ConcernToken::SpecCoherenceFail,
            bonds: vec![spec("gate")],
            target: FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "verifier-honesty".to_owned(),
            },
            evidence: "evidence".to_owned(),
        }
    }

    fn deterministic_finding() -> Finding {
        Finding {
            token: ConcernToken::VerifierFailed,
            bonds: vec![spec("gate")],
            target: FindingTarget::Annotation {
                target_string: "cargo test --lib failing_verifier".to_owned(),
            },
            evidence: "failed".to_owned(),
        }
    }

    #[test]
    fn suppressions_match_rubric_findings_by_id_or_hash() {
        let finding = rubric_finding();
        let by_id = SuppressionConfig {
            id: Some(finding.id()),
            hash: None,
            reason: "false positive".to_owned(),
        };
        let by_hash = SuppressionConfig {
            id: None,
            hash: Some(finding.hash()),
            reason: "false positive".to_owned(),
        };
        assert!(suppresses_rubric_finding(&[by_id], &finding));
        assert!(suppresses_rubric_finding(&[by_hash], &finding));
    }

    #[test]
    fn suppressions_do_not_filter_deterministic_or_integrity_findings() {
        let finding = deterministic_finding();
        let entry = SuppressionConfig {
            id: Some(finding.id()),
            hash: None,
            reason: "do not apply".to_owned(),
        };
        assert!(suppression_matches(&entry, &finding));
        assert!(!suppresses_rubric_finding(&[entry], &finding));
    }
}
