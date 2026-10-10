#[cfg(test)]
mod tests {
    use loom_driver::spec::package;
    use loom_protocol::gate::{
        ConcernToken, DispatchScope, FindingParseError, FindingRoute, FindingTarget, RawFinding,
        WalkOutput,
    };

    use super::super::*;

    const CONTRACT: &str = "# Billing\n\n## Architecture\n\nContract guard must hold.\n";
    const ACCEPTANCE: &str = "# Billing acceptance\n\n## Success Criteria\n\n### Billing checks\n\n- Invoice rounding and signed refunds [test](billing_rounding)\n\n### Acceptance only\n\nAcceptance guard.\n";
    const INDEX: &str = "## Specs\n\n- [discovery](../specs/discovery/spec.md)\n- [billing](../specs/billing/spec.md)\n";

    fn canonical_workspace(root: &Path) {
        for (label, contract, acceptance) in [
            ("discovery", "# Discovery\n", "## Success Criteria\n"),
            ("billing", CONTRACT, ACCEPTANCE),
        ] {
            let dir = root.join("specs").join(label);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("spec.md"), contract).unwrap();
            std::fs::write(dir.join("tests.md"), acceptance).unwrap();
        }
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/README.md"), INDEX).unwrap();
    }

    fn raw_finding(target: FindingTarget) -> RawFinding {
        RawFinding {
            token: match &target {
                FindingTarget::Criterion { .. } => ConcernToken::VerifierTooNarrow,
                FindingTarget::Invariant { .. } => ConcernToken::InvariantClash,
                other => panic!("unsupported test target: {other:?}"),
            },
            route: FindingRoute::Deferred,
            bonds: vec![
                SpecLabel::new("discovery").unwrap(),
                SpecLabel::new("billing").unwrap(),
            ],
            target,
            evidence: "An unchanged sibling obligation needs remediation.".to_owned(),
        }
    }

    fn criterion(anchor: &str) -> RawFinding {
        raw_finding(FindingTarget::Criterion {
            spec: SpecLabel::new("billing").unwrap(),
            anchor: anchor.to_owned(),
        })
    }

    fn invariant(section: &str, tag: &str) -> RawFinding {
        raw_finding(FindingTarget::Invariant {
            spec: SpecLabel::new("billing").unwrap(),
            section: section.to_owned(),
            tag: tag.to_owned(),
        })
    }

    fn walk(root: &Path, raw: &RawFinding) -> (String, WalkOutput) {
        let stdout = format!(
            "Reviewing siblings\nLOOM_FINDING: {}\nLOOM_CONCERN: {{\"summary\":\"Sibling obligation\"}}\n",
            serde_json::to_string(raw).unwrap(),
        );
        let walk = WalkOutput::from_stdout(
            &stdout,
            DispatchScope::PerBead,
            &WorkspaceFindingValidator::new(root),
        );
        (stdout, walk)
    }

    #[test]
    fn canonical_sibling_criterion_findings_resolve_acceptance_and_contract_anchors() {
        let dir = tempfile::tempdir().unwrap();
        canonical_workspace(dir.path());
        let id = annotation::criterion_id_for(
            &SpecLabel::new("billing").unwrap(),
            "Invoice rounding and signed refunds",
        );
        for anchor in [
            "billing-checks",
            "billing_rounding",
            "Invoice rounding and signed refunds",
            &id,
            "architecture",
        ] {
            let raw = criterion(anchor);
            let (_, walk) = walk(dir.path(), &raw);
            assert!(walk.finding_errors().is_empty(), "{walk:?}");
            assert_eq!(walk.findings().len(), 1);
            assert_eq!(walk.findings()[0].clone().into_raw(), raw);
        }
    }

    #[test]
    fn canonical_sibling_invariant_findings_resolve_contract_sections_and_tags() {
        let dir = tempfile::tempdir().unwrap();
        canonical_workspace(dir.path());
        let raw = invariant("architecture", "contract-guard");
        let (_, walk) = walk(dir.path(), &raw);
        assert!(walk.finding_errors().is_empty(), "{walk:?}");
        assert_eq!(walk.findings().len(), 1);
        assert_eq!(walk.findings()[0].clone().into_raw(), raw);
    }

    #[test]
    fn finding_identity_and_wire_payload_survive_package_relocation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("specs")).unwrap();
        std::fs::write(dir.path().join("specs/discovery.md"), "# Discovery\n").unwrap();
        std::fs::write(
            dir.path().join("specs/billing.md"),
            format!("{CONTRACT}\n{ACCEPTANCE}"),
        )
        .unwrap();
        let raws = [
            criterion("billing_rounding"),
            invariant("architecture", "contract-guard"),
        ];
        let bootstrap: Vec<_> = raws
            .iter()
            .map(|raw| {
                let (_, walk) = walk(dir.path(), raw);
                assert!(walk.finding_errors().is_empty(), "{walk:?}");
                walk.findings()[0].clone()
            })
            .collect();
        for label in ["discovery", "billing"] {
            std::fs::remove_file(dir.path().join(format!("specs/{label}.md"))).unwrap();
        }
        canonical_workspace(dir.path());
        for (raw, before) in raws.iter().zip(bootstrap) {
            let (_, walk) = walk(dir.path(), raw);
            assert!(walk.finding_errors().is_empty(), "{walk:?}");
            let after = &walk.findings()[0];
            assert_eq!(after, &before);
            assert_eq!(after.id(), before.id());
            assert_eq!(after.hash(), before.hash());
            assert_eq!(
                serde_json::to_string(after).unwrap(),
                serde_json::to_string(&before).unwrap()
            );
        }
    }

    #[test]
    fn canonical_packages_reject_unresolved_or_wrong_document_targets() {
        let dir = tempfile::tempdir().unwrap();
        canonical_workspace(dir.path());
        std::fs::write(
            dir.path().join("specs/billing/support.md"),
            "## Supporting heading\n## Success Criteria\n- Supporting claim [test](support_only)\n",
        )
        .unwrap();
        for raw in [
            criterion("absent-anchor"),
            criterion("supporting-heading"),
            criterion("support_only"),
            invariant("absent-section", "contract-guard"),
            invariant("architecture", "absent-tag"),
            invariant("acceptance-only", "acceptance-guard"),
        ] {
            let (stdout, walk) = walk(dir.path(), &raw);
            assert_eq!(walk.findings(), []);
            assert!(
                matches!(walk.finding_errors(), [FindingParseError::UnresolvedTarget { line_number: 2, raw, .. }] if raw == stdout.lines().nth(1).unwrap())
            );
        }
    }

    #[test]
    fn canonical_findings_reject_unknown_bond_labels() {
        let dir = tempfile::tempdir().unwrap();
        canonical_workspace(dir.path());
        for mut raw in [
            criterion("billing-checks"),
            invariant("architecture", "contract-guard"),
        ] {
            raw.bonds.push(SpecLabel::new("unknown").unwrap());
            let (stdout, walk) = walk(dir.path(), &raw);
            assert_eq!(walk.findings(), []);
            assert!(
                matches!(walk.finding_errors(), [FindingParseError::UnknownBondSpec { line_number: 2, spec, raw }] if spec == "unknown" && raw == stdout.lines().nth(1).unwrap())
            );
        }
    }

    fn assert_inventory_refuses_findings(root: &Path) {
        let validator = WorkspaceFindingValidator::new(root);
        for label in ["discovery", "billing"] {
            let label = SpecLabel::new(label).unwrap();
            assert!(!validator.spec_label_is_known(&label));
            assert!(!validator.criterion_anchor_resolves(&label, "billing-checks"));
            assert!(!validator.invariant_resolves(&label, "architecture", "contract-guard"));
        }
        assert!(!validator.annotation_is_declared("billing_rounding"));
        for raw in [
            criterion("billing-checks"),
            invariant("architecture", "contract-guard"),
        ] {
            let (stdout, walk) = walk(root, &raw);
            assert_eq!(walk.findings(), []);
            assert!(
                matches!(walk.finding_errors(), [FindingParseError::UnknownBondSpec { line_number: 2, spec, raw }] if spec == "discovery" && raw == stdout.lines().nth(1).unwrap())
            );
        }
    }

    #[test]
    fn mixed_inventory_refuses_even_complete_sibling_findings() {
        let dir = tempfile::tempdir().unwrap();
        canonical_workspace(dir.path());
        std::fs::write(
            dir.path().join("specs/billing.md"),
            format!("{CONTRACT}\n{ACCEPTANCE}"),
        )
        .unwrap();
        assert!(matches!(
            package::discover_workspace(dir.path()),
            Err(package::Error::MixedLayout { .. })
        ));
        assert_inventory_refuses_findings(dir.path());
    }

    #[test]
    fn incomplete_inventory_refuses_even_complete_sibling_findings() {
        for document in ["spec.md", "tests.md"] {
            let dir = tempfile::tempdir().unwrap();
            canonical_workspace(dir.path());
            std::fs::remove_file(dir.path().join("specs/billing").join(document)).unwrap();
            assert!(matches!(
                package::discover_workspace(dir.path()),
                Err(package::Error::Incomplete { .. })
            ));
            assert_inventory_refuses_findings(dir.path());
        }
    }

    #[test]
    fn inconsistent_index_refuses_even_complete_sibling_findings() {
        for index in [
            "- [discovery](../specs/discovery/spec.md)\n",
            "- [discovery](../specs/discovery/spec.md)\n- [billing](../specs/billing/spec.md)\n- [unknown](../specs/unknown/spec.md)\n",
            "- [discovery](../specs/discovery/spec.md)\n- [billing](../specs/billing/spec.md)\n- [billing](../specs/billing/spec.md)\n",
            "- [discovery](../specs/discovery/spec.md)\n- [billing](../specs/discovery/spec.md)\n",
        ] {
            let dir = tempfile::tempdir().unwrap();
            canonical_workspace(dir.path());
            std::fs::write(dir.path().join("docs/README.md"), index).unwrap();
            assert!(matches!(
                package::discover_workspace(dir.path()),
                Err(package::Error::Index { .. })
            ));
            assert_inventory_refuses_findings(dir.path());
        }
    }

    #[test]
    fn missing_canonical_index_refuses_findings() {
        let dir = tempfile::tempdir().unwrap();
        canonical_workspace(dir.path());
        std::fs::remove_file(dir.path().join("docs/README.md")).unwrap();
        assert!(matches!(
            package::discover_workspace(dir.path()),
            Err(package::Error::ReadFile { .. })
        ));
        assert_inventory_refuses_findings(dir.path());
    }
}
