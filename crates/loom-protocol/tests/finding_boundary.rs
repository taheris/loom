//! External consumers can only obtain trusted findings through resolution.
#[cfg(test)]
mod contract {
    use loom_events::identifier::SpecLabel;
    use loom_protocol::gate::{
        ConcernToken, DispatchScope, FindingParseError, FindingRoute, FindingTarget,
        FindingValidator, RawFinding, TerminalSurface, WalkOutput,
    };

    struct Workspace;
    impl FindingValidator for Workspace {
        fn spec_label_is_known(&self, spec: &SpecLabel) -> bool {
            matches!(spec.as_str(), "gate" | "harness")
        }
        fn criterion_anchor_resolves(&self, spec: &SpecLabel, anchor: &str) -> bool {
            spec.as_str() == "gate" && anchor == "known"
        }
        fn annotation_resolves(&self, target: &str) -> bool {
            target == "cargo test known"
        }
        fn annotation_is_declared(&self, target: &str) -> bool {
            self.annotation_resolves(target) || target == "missing-verifier"
        }
        fn file_exists(&self, path: &str) -> bool {
            path == "src/lib.rs"
        }
        fn invariant_resolves(&self, _: &SpecLabel, _: &str, _: &str) -> bool {
            false
        }
    }

    fn raw() -> RawFinding {
        RawFinding {
            token: ConcernToken::SpecCoherenceFail,
            route: FindingRoute::Deferred,
            bonds: vec!["gate".parse().unwrap()],
            target: FindingTarget::Criterion {
                spec: "gate".parse().unwrap(),
                anchor: "known".into(),
            },
            evidence: "observed failure".into(),
        }
    }

    #[test]
    fn deserialized_raw_input_cannot_bypass_token_target_checks() {
        let raw: RawFinding = serde_json::from_str(r#"{"token":"spec-coherence-fail","route":"deferred","bonds":["gate"],"target":{"kind":"Annotation","target_string":"cargo test known"},"evidence":""}"#).unwrap();
        assert!(matches!(
            raw.resolve(DispatchScope::Tree, &Workspace),
            Err(FindingParseError::TokenVariantMismatch { .. })
        ));
    }

    #[test]
    fn resolution_rejects_empty_unknown_and_unbonded_specs() {
        let mut record = raw();
        record.bonds.clear();
        assert!(matches!(
            record.resolve(DispatchScope::Tree, &Workspace),
            Err(FindingParseError::EmptyBonds { .. })
        ));
        let mut record = raw();
        record.bonds.push("missing".parse().unwrap());
        assert!(matches!(
            record.resolve(DispatchScope::Tree, &Workspace),
            Err(FindingParseError::UnknownBondSpec { .. })
        ));
        let mut record = raw();
        record.bonds = vec!["harness".parse().unwrap()];
        assert!(record.resolve(DispatchScope::Tree, &Workspace).is_err());
    }

    #[test]
    fn resolution_requires_context_even_after_valid_wire_deserialization() {
        let mut record = raw();
        record.target = FindingTarget::Criterion {
            spec: "gate".parse().unwrap(),
            anchor: "missing".into(),
        };
        let record: RawFinding =
            serde_json::from_value(serde_json::to_value(record).unwrap()).unwrap();
        assert!(matches!(
            record.resolve(DispatchScope::Tree, &Workspace),
            Err(FindingParseError::UnresolvedTarget { .. })
        ));
    }

    #[test]
    fn mutating_unsealed_data_requires_scope_resolution_again() {
        let trusted = raw().resolve(DispatchScope::Tree, &Workspace).unwrap();
        let mut record = trusted.into_raw();
        record.token = ConcernToken::CrossSpecClash;
        assert!(matches!(
            record.resolve(DispatchScope::PushGate, &Workspace),
            Err(FindingParseError::TokenScopeMismatch { .. })
        ));
    }

    #[test]
    fn wire_roundtrip_preserves_canonical_identity_and_payload() {
        let canonical_raw = RawFinding {
            token: ConcernToken::VerifierBypass,
            target: FindingTarget::Annotation {
                target_string: "cargo test known".into(),
            },
            ..raw()
        };
        let record = RawFinding {
            target: FindingTarget::Annotation {
                target_string: "[test](cargo test known)".into(),
            },
            ..canonical_raw.clone()
        };
        let trusted = record.resolve(DispatchScope::Tree, &Workspace).unwrap();
        let canonical = canonical_raw
            .clone()
            .resolve(DispatchScope::Tree, &Workspace)
            .unwrap();
        assert_eq!(trusted.id(), canonical.id());
        assert_eq!(trusted.hash(), canonical.hash());
        let json = serde_json::to_string(&trusted).unwrap();
        let roundtrip: RawFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(
            roundtrip.resolve(DispatchScope::Tree, &Workspace).unwrap(),
            trusted
        );
        assert_eq!(
            serde_json::to_value(&trusted).unwrap(),
            serde_json::to_value(canonical_raw).unwrap()
        );
    }

    #[test]
    fn mixed_stream_keeps_valid_findings_errors_and_terminal() {
        let json = serde_json::to_string(&raw()).unwrap();
        let output = format!("LOOM_FINDING: {json}\nLOOM_FINDING: {{bad json}}\nLOOM_COMPLETE\n");
        let parsed = WalkOutput::from_stdout(&output, DispatchScope::Tree, &Workspace);
        assert_eq!(
            parsed.findings(),
            &[raw().resolve(DispatchScope::Tree, &Workspace).unwrap()]
        );
        assert_eq!(parsed.finding_errors().len(), 1);
        assert_eq!(parsed.terminal(), &TerminalSurface::Complete);
    }

    #[test]
    fn deterministic_failures_resolve_declared_not_executable_annotations() {
        let record = RawFinding {
            token: ConcernToken::UnresolvedAnnotation,
            target: FindingTarget::Annotation {
                target_string: "missing-verifier".into(),
            },
            ..raw()
        };
        assert!(
            record
                .clone()
                .resolve(DispatchScope::Tree, &Workspace)
                .is_ok()
        );
        let rubric = RawFinding {
            token: ConcernToken::VerifierBypass,
            ..record
        };
        assert!(matches!(
            rubric.resolve(DispatchScope::Tree, &Workspace),
            Err(FindingParseError::UnresolvedTarget { .. })
        ));
    }
}
