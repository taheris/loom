//! Typed task-reference boundaries are independent of snapshot and executable admission.

use loom_events::identifier::SpecLabel;
use loom_protocol::acceptance::{
    CriterionInput, PackageInput, ParseError, Reference, References, Snapshot, SnapshotError,
};
use loom_protocol::criterion::{
    AnnotationTarget, AnnotationTier, CriterionAnnotation, CriterionId,
};

#[test]
fn reference_parsing_distinguishes_malformed_label_and_criterion_id() {
    assert!(matches!(
        Reference::parse("Not-a-label", "criterion-0123456789abcdef"),
        Err(ParseError::Label(_))
    ));
    assert!(matches!(
        Reference::parse("specs", "not-a-criterion"),
        Err(ParseError::Criterion(_))
    ));
    assert!(Reference::parse("unknown-but-valid", "criterion-0123456789abcdef").is_ok());
}

#[test]
fn assignment_parsing_does_not_drop_invalid_members() {
    assert!(matches!(
        References::parse([
            ("specs", "criterion-0123456789abcdef"),
            ("specs", "invalid"),
        ]),
        Err(ParseError::Member { index: 1, .. })
    ));
}

#[test]
fn assignments_cannot_be_empty_or_duplicate() {
    assert!(matches!(References::new(vec![]), Err(ParseError::Empty)));
    let reference = Reference::parse("specs", "criterion-0123456789abcdef").unwrap();
    assert!(matches!(
        References::new(vec![reference.clone(), reference]),
        Err(ParseError::Duplicate { .. })
    ));
}

#[test]
fn external_assignment_json_parses_identifiers_and_nonempty_membership() {
    let input = r#"[{"spec_label":"specs","criterion_id":"criterion-0123456789abcdef"}]"#;
    let references: References = serde_json::from_str(input).unwrap();
    assert_eq!(serde_json::to_string(&references).unwrap(), input);
    for invalid in [
        "[]",
        r#"[{"spec_label":"UPPER","criterion_id":"criterion-0123456789abcdef"}]"#,
        r#"[{"spec_label":"specs","criterion_id":"invalid"}]"#,
        r#"[{"spec_label":"specs","criterion_id":"criterion-0123456789abcdef","resolved":true}]"#,
        r#"[{"spec_label":"specs"}]"#,
        r#"[{"spec_label":"specs","criterion_id":"criterion-0123456789abcdef"},{"spec_label":"specs","criterion_id":"criterion-0123456789abcdef"}]"#,
    ] {
        assert!(
            serde_json::from_str::<References>(invalid).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn task_acceptance_metadata_serde_preserves_reference_boundary() {
    use loom_protocol::acceptance::Candidates;
    let criteria = References::parse([("specs", "criterion-0123456789abcdef")]).unwrap();
    let candidates = Candidates::Implementation { criteria };
    let encoded = serde_json::to_string(&candidates).unwrap();
    assert_eq!(
        encoded,
        r#"{"purpose":"implementation","criteria":[{"spec_label":"specs","criterion_id":"criterion-0123456789abcdef"}]}"#
    );
    assert_eq!(
        Candidates::from_metadata(&serde_json::json!(encoded)).unwrap(),
        candidates
    );
    assert!(
        Snapshot::new([])
            .unwrap()
            .resolve(match &candidates {
                Candidates::Implementation { criteria } => criteria,
                Candidates::Remediation { .. } => panic!("wrong purpose"),
            })
            .is_err()
    );
}

#[test]
fn task_acceptance_refs_are_exclusive_nonempty_variants() {
    use loom_protocol::acceptance::Candidates;
    let criterion =
        serde_json::json!({"spec_label":"specs","criterion_id":"criterion-0123456789abcdef"});
    let goal = serde_json::json!({"batch":"lm-batch","finding_hash":"v1:012345abcdef"});
    for valid in [
        serde_json::json!({"purpose":"implementation","criteria":[criterion]}),
        serde_json::json!({"purpose":"remediation","goals":[goal]}),
    ] {
        let parsed: Candidates = serde_json::from_value(valid.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), valid);
        assert_eq!(
            Candidates::from_metadata(&serde_json::json!(valid.to_string())).unwrap(),
            parsed
        );
    }
    for invalid in [
        serde_json::json!({}),
        serde_json::json!({"purpose":"unknown","criteria":[criterion]}),
        serde_json::json!({"purpose":"implementation","criteria":[]}),
        serde_json::json!({"purpose":"remediation","goals":[]}),
        serde_json::json!({"purpose":"implementation","criteria":[criterion],"goals":[goal]}),
        serde_json::json!({"purpose":"remediation","goals":[goal],"criteria":[criterion]}),
        serde_json::json!({"purpose":"implementation","criteria":[criterion,criterion]}),
        serde_json::json!({"purpose":"remediation","goals":[goal,goal]}),
        serde_json::json!({"purpose":"remediation","goals":[{"batch":"bad batch","finding_hash":"v1:012345abcdef"}]}),
        serde_json::json!({"purpose":"remediation","goals":[{"batch":"lm-batch","finding_hash":"BAD"}]}),
        serde_json::json!({"purpose":"remediation","goals":[{"batch":"lm-batch","finding_hash":"012345abcdef"}]}),
        serde_json::json!({"purpose":"remediation","goals":[{"batch":"lm-batch","finding_hash":"v2:012345abcdef"}]}),
        serde_json::json!({"purpose":"remediation","goals":[{"batch":"lm-batch","finding_hash":"v1:012345ABCDEf"}]}),
        serde_json::json!({"purpose":"remediation","goals":[{"batch":"lm-batch","finding_hash":"v1:012345abcdef","evidence":"copied"}]}),
        serde_json::json!({"purpose":"implementation","criteria":[{"spec_label":"specs","criterion_id":"invalid"}]}),
        serde_json::json!({"purpose":"implementation","criteria":[{"spec_label":"specs","criterion_id":"criterion-0123456789abcdef","binding":"copied"}]}),
        serde_json::json!({"purpose":"implementation","criteria":[{"spec_label":"specs","criterion_id":"criterion-0123456789abcdef"}],"resolved":true}),
    ] {
        assert!(
            serde_json::from_value::<Candidates>(invalid.clone()).is_err(),
            "{invalid}"
        );
        assert!(
            Candidates::from_metadata(&serde_json::json!(invalid.to_string())).is_err(),
            "{invalid}"
        );
    }
    assert!(
        Candidates::from_metadata(&serde_json::json!({"purpose":"implementation","criteria":[]}))
            .is_err()
    );
}

#[test]
fn snapshot_construction_rejects_ambiguous_package_labels() {
    let label = SpecLabel::new("specs").unwrap();
    assert!(matches!(
        Snapshot::new([
            PackageInput {
                spec_label: label.clone(),
                criteria: vec![]
            },
            PackageInput {
                spec_label: label,
                criteria: vec![]
            },
        ]),
        Err(SnapshotError::DuplicatePackage { .. })
    ));
}

#[test]
fn snapshot_construction_rejects_empty_requirements() {
    let label = SpecLabel::new("specs").unwrap();
    assert!(matches!(
        Snapshot::new([PackageInput {
            spec_label: label,
            criteria: vec![CriterionInput {
                criterion_text: " \n ".into(),
                annotations: vec![CriterionAnnotation {
                    tier: AnnotationTier::Test,
                    target: AnnotationTarget::new("verifier"),
                    pending: true,
                }],
                source: "specs/specs/tests.md".into(),
                line: 1,
            }],
        }]),
        Err(SnapshotError::EmptyRequirement { .. })
    ));
}

#[test]
fn canonical_requirement_identity_depends_on_label_not_whitespace() {
    let label = SpecLabel::new("specs").unwrap();
    let first = CriterionId::for_spec_text(&label, "Exact \n requirement");
    let same = CriterionId::for_spec_text(&label, "Exact requirement");
    let other_label =
        CriterionId::for_spec_text(&SpecLabel::new("other").unwrap(), "Exact requirement");
    assert_eq!(first, same);
    assert_ne!(first, other_label);
}
