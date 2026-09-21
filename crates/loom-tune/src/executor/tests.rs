#[cfg(test)]
mod contract {
    use super::super::*;
    use crate::case::*;
    use loom_events::identifier::{SessionId, SpecLabel, ToolCallId};
    use loom_events::{EnvelopeBuilder, ParsedAgentEvent, SessionScope, Source as EventSource};

    struct AcceptAllFindings;
    impl FindingValidator for AcceptAllFindings {
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

    fn observed(output: &str, changes: &[&str], tools: &[(&str, &str, bool)]) -> Evidence {
        let mut builder = EnvelopeBuilder::new(
            SessionScope::phase(SessionId::generated("tune-test"), None),
            EventSource::Agent,
            || 0,
        );
        let mut events = Vec::new();
        for (index, (tool, argument, success)) in tools.iter().enumerate() {
            let id = ToolCallId::new(format!("call-{index}")).unwrap();
            let params = if *tool == "bash" {
                serde_json::json!({"command": argument})
            } else {
                serde_json::json!({"path": argument})
            };
            events.push(AgentEvent::from_parsed(
                ParsedAgentEvent::ToolCall {
                    id: id.clone(),
                    tool: (*tool).to_owned(),
                    params,
                    parent_tool_call_id: None,
                },
                builder.build(),
            ));
            events.push(AgentEvent::from_parsed(
                ParsedAgentEvent::ToolResult {
                    id,
                    output: "tool result".to_owned(),
                    is_error: !success,
                },
                builder.build(),
            ));
        }
        Evidence {
            output: output.to_owned(),
            recorded: Some(Recorded {
                events,
                changed_paths: changes.iter().map(|path| (*path).to_owned()).collect(),
                workspace: "/fixture/replay".into(),
            }),
        }
    }

    fn hard(evidence: &Evidence, expected: &Expected) -> u64 {
        score(evidence, expected, &AcceptAllFindings)
            .unwrap()
            .hard
            .get()
            .to_bits()
    }

    #[test]
    fn scope_scores_observed_paths_not_positive_or_forbidden_text() {
        let expected = Expected::LoopScopeDiscipline(ScopeDisciplineExpected {
            allowed_edit_paths: vec!["src/*.rs".into()],
            forbidden_edit_paths: vec!["src/forbidden.rs".into(), "docs/**".into()],
            max_changed_files: 1,
        });
        assert_eq!(
            hard(&observed("LOOM_COMPLETE", &["src/lib.rs"], &[]), &expected),
            1.0_f64.to_bits()
        );
        for paths in [
            &["src/forbidden.rs"][..],
            &["docs/nested/secret.md"][..],
            &["src/a.rs", "src/b.rs"][..],
            &[][..],
        ] {
            assert_eq!(
                hard(
                    &observed(
                        "Changed src/*.rs and src/forbidden.rs and docs/** and 500 other files\nLOOM_COMPLETE",
                        paths,
                        &[]
                    ),
                    &expected
                ),
                0.0_f64.to_bits()
            );
        }
        assert!(matches!(
            score(
                &Evidence::text("Only changed src/lib.rs\nLOOM_COMPLETE"),
                &expected,
                &AcceptAllFindings
            ),
            Err(Error::EvidenceUnavailable { .. })
        ));
    }

    #[test]
    fn verify_requires_successful_command_after_final_observed_edit() {
        let expected = Expected::LoopVerifyAfterEdit(VerifyAfterEditExpected {
            edited_paths: vec!["src/lib.rs".into()],
            verify_commands: vec!["cargo test".into()],
            marker: "LOOM_COMPLETE".into(),
        });
        let edit = ("edit", "src/lib.rs", true);
        let verify = ("bash", "cargo test", true);
        assert_eq!(
            hard(
                &observed("LOOM_COMPLETE", &["src/lib.rs"], &[edit, verify]),
                &expected
            ),
            1.0_f64.to_bits()
        );
        for tools in [
            vec![verify, edit],
            vec![edit, verify, edit],
            vec![edit, ("bash", "cargo test", false)],
        ] {
            assert_eq!(
                hard(
                    &observed(
                        "I edited src/lib.rs then ran cargo test\nLOOM_COMPLETE",
                        &["src/lib.rs"],
                        &tools
                    ),
                    &expected
                ),
                0.0_f64.to_bits()
            );
        }
        assert!(
            score(
                &observed(
                    "LOOM_COMPLETE",
                    &["src/lib.rs"],
                    &[edit, ("bash", "echo cargo test; touch hidden", true)]
                ),
                &expected,
                &AcceptAllFindings
            )
            .is_err()
        );
    }

    #[test]
    fn context_requires_completed_reads_before_the_first_edit() {
        let expected = Expected::AgentContextBeforeEdit(ContextBeforeEditExpected {
            must_read_before_edit: vec!["docs/style-rules.md".into()],
            edited_paths: vec!["src/lib.rs".into()],
        });
        let read = ("read", "/workspace/docs/style-rules.md", true);
        let edit = ("edit", "/fixture/replay/src/lib.rs", true);
        assert_eq!(
            hard(
                &observed("LOOM_COMPLETE", &["src/lib.rs"], &[read, edit]),
                &expected
            ),
            1.0_f64.to_bits()
        );
        for tools in [
            vec![edit, read],
            vec![("read", "docs/style-rules.md", false), edit],
            vec![edit],
        ] {
            assert_eq!(
                hard(
                    &observed(
                        "Read docs/style-rules.md before editing src/lib.rs\nLOOM_COMPLETE",
                        &["src/lib.rs"],
                        &tools
                    ),
                    &expected
                ),
                0.0_f64.to_bits()
            );
        }
        let mut concurrent = observed("LOOM_COMPLETE", &["src/lib.rs"], &[read, edit]);
        concurrent.recorded.as_mut().unwrap().events.swap(1, 2);
        assert_eq!(hard(&concurrent, &expected), 0.0_f64.to_bits());
    }

    #[test]
    fn unpaired_or_unknown_tool_events_are_unavailable_not_successful() {
        let expected = Expected::AgentContextBeforeEdit(ContextBeforeEditExpected {
            must_read_before_edit: vec!["src/lib.rs".into()],
            edited_paths: vec!["src/lib.rs".into()],
        });
        assert!(
            score(
                &observed(
                    "Read then edited src/lib.rs\nLOOM_COMPLETE",
                    &["src/lib.rs"],
                    &[]
                ),
                &expected,
                &AcceptAllFindings
            )
            .is_err()
        );
        let mut evidence = observed("LOOM_COMPLETE", &[], &[("read", "src/lib.rs", true)]);
        evidence.recorded.as_mut().unwrap().events.pop();
        assert!(operations(&evidence).is_err());
        let unknown = observed(
            "LOOM_COMPLETE",
            &[],
            &[("hidden_execution", "src/lib.rs", true)],
        );
        assert!(operations(&unknown).is_err());
    }

    #[test]
    fn inbox_safety_flags_never_accept_command_or_bead_update_claims() {
        let mut expected = InboxResolutionExpected {
            forbidden_commands: vec!["loom inbox resolve".into()],
            allowed_terminal_markers: vec!["LOOM_COMPLETE".into()],
            must_update_beads: false,
            must_not_push: true,
        };
        let terminal = "I did not push or use loom inbox resolve\nLOOM_COMPLETE";
        for command in ["git push origin main", "loom inbox resolve lm-test"] {
            assert_eq!(
                hard(
                    &observed(terminal, &[], &[("bash", command, true)]),
                    &Expected::InboxResolutionPath(expected.clone())
                ),
                0.0_f64.to_bits()
            );
        }
        assert_eq!(
            hard(
                &observed(terminal, &[], &[]),
                &Expected::InboxResolutionPath(expected.clone())
            ),
            1.0_f64.to_bits()
        );
        expected.must_update_beads = true;
        assert!(
            score(
                &observed("Updated Beads\nLOOM_COMPLETE", &[], &[]),
                &Expected::InboxResolutionPath(expected),
                &AcceptAllFindings
            )
            .is_err()
        );
    }

    #[test]
    fn apply_requires_parsed_ids_and_honors_push_and_integration_flags() {
        let expected = Expected::TuneApplyHandoff(TuneApplyExpected {
            apply_proposals: vec!["lm-fixture.1".parse().unwrap()],
            must_emit_apply: true,
            must_not_push: true,
            must_not_dirty_integration: true,
        });
        let valid = "LOOM_APPLY: {\"proposals\":[\"lm-fixture.1\"]}";
        assert_eq!(
            hard(&observed(valid, &[], &[]), &expected),
            1.0_f64.to_bits()
        );
        for invalid in [
            "LOOM_APPLY lm-fixture.1",
            "LOOM_COMPLETE",
            "LOOM_APPLY: {\"proposals\":[\"lm-other\"]}",
            "LOOM_APPLY: {\"proposals\":[\"lm-fixture.1\",\"lm-fixture.1\"]}",
        ] {
            assert_eq!(
                hard(&observed(invalid, &[], &[]), &expected),
                0.0_f64.to_bits()
            );
        }
        assert_eq!(
            hard(
                &observed(valid, &[".loom/integration/src/lib.rs"], &[]),
                &expected
            ),
            0.0_f64.to_bits()
        );
        assert_eq!(
            hard(
                &observed(valid, &[], &[("bash", "git push", true)]),
                &expected
            ),
            0.0_f64.to_bits()
        );
        assert!(
            score(
                &observed(valid, &[], &[("bash", "python push.py", true)]),
                &expected,
                &AcceptAllFindings
            )
            .is_err()
        );
    }

    fn todo_output(specs: &[(&str, &str)]) -> String {
        let specs = specs.iter().map(|(label, bead)| serde_json::json!({"label":label,"outcome":"decomposed","beads":[bead]})).collect::<Vec<_>>();
        format!(
            "LOOM_TODO: {}",
            serde_json::json!({"head":"a".repeat(40),"fingerprint":"b".repeat(64),"work_epic":"lm-work","title":"Task batch","specs":specs})
        )
    }

    #[test]
    fn todo_counts_parsed_beads_and_checks_required_and_forbidden_specs() {
        let expected = Expected::TodoDecomposition(TodoExpected {
            min_items: 2,
            max_items: 2,
            required_specs: vec!["skills".parse().unwrap(), "harness".parse().unwrap()],
            forbidden_specs: vec!["llm".parse().unwrap()],
        });
        let valid = todo_output(&[("skills", "lm-work.1"), ("harness", "lm-work.2")]);
        assert_eq!(hard(&Evidence::text(&valid), &expected), 1.0_f64.to_bits());
        for invalid in [
            "skills harness llm LOOM_TODO".to_owned(),
            format!("LOOM_COMPLETE\n{valid}"),
            format!("{valid}\n{valid}"),
            todo_output(&[("skills", "lm-work.1")]),
            todo_output(&[("skills", "lm-work.1"), ("llm", "lm-work.2")]),
        ] {
            assert_eq!(hard(&Evidence::text(invalid), &expected), 0.0_f64.to_bits());
        }
    }

    fn review_output() -> &'static str {
        "LOOM_FINDING: {\"token\":\"spec-coherence-fail\",\"route\":\"blocking\",\"bonds\":[\"skills\"],\"target\":{\"kind\":\"Criterion\",\"spec\":\"skills\",\"anchor\":\"fixture\"},\"evidence\":\"missing test\"}\nLOOM_CONCERN: {\"summary\":\"missing test\"}"
    }

    #[test]
    fn review_checker_requires_parsed_finding_terminal_pair_and_limits_extras() {
        let expected = Expected::ReviewFindingRecall(ReviewExpected {
            findings: vec![ReviewFinding {
                contains: vec!["missing test".into()],
                file: None,
            }],
            max_extra_findings: Some(0),
        });
        assert_eq!(
            hard(&Evidence::text(review_output()), &expected),
            1.0_f64.to_bits()
        );
        assert_eq!(
            hard(&Evidence::text("missing test\nLOOM_COMPLETE"), &expected),
            0.0_f64.to_bits()
        );
        assert_eq!(
            hard(
                &Evidence::text(review_output().split('\n').next().unwrap()),
                &expected
            ),
            0.0_f64.to_bits()
        );
        let empty = Expected::ReviewFindingRecall(ReviewExpected {
            findings: vec![],
            max_extra_findings: Some(0),
        });
        assert_eq!(
            hard(&Evidence::text(review_output()), &empty),
            0.0_f64.to_bits()
        );
    }

    #[test]
    fn mined_nonempty_text_is_explicitly_not_evaluated() {
        use crate::{checker::Level, config::TuneConfig, evidence, plan};
        let target = "skill:loom-context-before-edit".parse::<Target>().unwrap();
        let checker = "behavior.review.finding-recall".parse().unwrap();
        let item = evidence::Item::harvested(
            evidence::ItemId::new("mined").unwrap(),
            checker,
            vec![target.clone()],
            evidence::TextEvidence {
                root_kind: evidence::RootKind::Workspace,
                relative_path: "log.jsonl".into(),
                body: "nonempty claims".into(),
            },
        );
        let evidence = evidence::Snapshot {
            train: vec![],
            selection: vec![item],
            metadata: evidence::SplitMetadata {
                algorithm: "sha256-salt-v1".into(),
                salt_id: "test".into(),
                selection_fraction: crate::config::SelectionFraction::new(0.34).unwrap(),
            },
        };
        let cases = LoadedCases::new(vec![], vec![]);
        let registry = Registry::builtin().unwrap();
        let plan = plan::build(plan::Request {
            targets: vec![target],
            level: Level::Run,
            cases: &cases,
            evidence: &evidence,
            config: &TuneConfig::default(),
            registry: &registry,
            seed: 7,
        })
        .unwrap();
        assert!(!plan.selected_cases.is_empty());
        assert!(matches!(
            run(&plan, &cases, &[], &registry, &AcceptAllFindings),
            Err(Error::UncheckableSelection { .. })
        ));
    }
}
