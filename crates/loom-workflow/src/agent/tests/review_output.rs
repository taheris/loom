#[cfg(test)]
mod contract {
    use super::super::controlled_outputs;
    use crate::review::{
        DispatchScope, FindingParseError, PhaseVerdict, RecoveryCause, WalkOutput,
        WorkspaceFindingValidator, cause_to_previous_failure, classify_review_phase,
        phase_verdict_from_walk,
    };
    use loom_protocol::gate::{BadWalk, TerminalSurface, WalkOutputError, parse_walk_output};
    use loom_protocol::output::{Error, Phase, decode};
    use loom_templates::previous_failure::{PREVIOUS_FAILURE_MAX_LEN, PreviousFailure};

    const FIRST: &str = r#"LOOM_FINDING: {"token":"mock-discipline","route":"deferred","bonds":["alpha"],"target":{"kind":"TestPath","path":"tests/check.rs"},"evidence":"first\nsecond 🧶"}"#;
    const LAST: &str = r#"LOOM_FINDING: {"token":"mock-discipline","route":"deferred","bonds":["alpha"],"target":{"kind":"TestPath","path":"tests/check.rs"},"evidence":"last finding"}"#;
    const BAD_JSON: &str = "LOOM_FINDING: {not json}";
    const BAD_TARGET: &str = r#"LOOM_FINDING: {"token":"mock-discipline","route":"deferred","bonds":["alpha"],"target":{"kind":"TestPath","path":"tests/missing.rs"},"evidence":"missing target"}"#;
    const CONCERN: &str = r#"LOOM_CONCERN: {"summary":"review summary"}"#;
    const TODO: &str = r#"LOOM_TODO: {"head":"0123456789abcdef0123456789abcdef01234567","fingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","work_epic":"lm-work","title":"Work","specs":[{"label":"alpha","outcome":"no-work","reason":"already implemented"}]}"#;

    fn workspace() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("tests")).unwrap();
        std::fs::write(
            directory.path().join("tests/check.rs"),
            "#[test] fn check() {}\n",
        )
        .unwrap();
        std::fs::create_dir(directory.path().join("specs")).unwrap();
        std::fs::write(
            directory.path().join("specs/alpha.md"),
            "# Alpha\n\n## Success Criteria\n\n- check [test](check)\n",
        )
        .unwrap();
        directory
    }

    fn assert_bad_walk_transfer(walk: &WalkOutput, bad_walk: &BadWalk) {
        assert_eq!(bad_walk.context(), walk.context());
        assert_eq!(bad_walk.context().raw(), walk.context().raw());
        assert_eq!(bad_walk.parsed_findings(), walk.findings());
        assert_eq!(bad_walk.finding_errors(), walk.finding_errors());
        assert_eq!(&bad_walk.terminal(), walk.terminal());
        let diagnostics = match walk.decoded() {
            Ok(_) => &[][..],
            Err(failure) => failure.diagnostics(),
        };
        assert_eq!(bad_walk.diagnostics(), diagnostics);
        for located in bad_walk.context().messages() {
            let raw = &walk.context().raw()[located.span.bytes.clone()];
            assert!(raw.starts_with(located.message.marker()));
            assert_eq!(
                located.span.line,
                1 + walk.context().raw()[..located.span.bytes.start]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count()
            );
        }
    }

    #[tokio::test]
    async fn review_failure_matrix_preserves_all_shared_decoder_context() {
        let directory = workspace();
        let validator = WorkspaceFindingValidator::new(directory.path());
        for (stream, valid, errors) in [
            ("No findings reported".to_owned(), 0, 0),
            (format!("{FIRST}\n{LAST}"), 2, 0),
            (format!("{FIRST}\n{BAD_JSON}\n{LAST}"), 2, 1),
            (format!("{FIRST}\n{BAD_TARGET}\n{LAST}"), 2, 1),
            (format!("{BAD_JSON}\n{BAD_TARGET}"), 0, 2),
        ] {
            for terminal in [
                "LOOM_COMPLETE",
                CONCERN,
                "LOOM_RETRY: {\"reason\":\"retry reason\"}",
                "LOOM_BLOCKED: {\"reason\":\"blocked reason\"}",
                "LOOM_NOOP",
                "LOOM_WAITING",
                "LOOM_APPLY: {\"proposals\":[\"lm-proposal\"]}",
                TODO,
                "LOOM_CLARIFY: {\"decisions\":[\"lm-decision\"]}\nLOOM_COMPLETE",
                "LOOM_CONCERN: {\"summary\":\"\"}",
                "LOOM_CONCERN: {\"summary\":\"raw\nnewline\"}",
                "LOOM_RETRY",
                "LOOM_RETRY: {}",
                "LOOM_RETRY: {\"reason\":\"\"}",
                "LOOM_RETRY: {\"reason\":\"raw\nnewline\"}",
                "LOOM_BLOCKED",
                "LOOM_BLOCKED: {}",
                "LOOM_BLOCKED: {\"reason\":\"\"}",
                "LOOM_BLOCKED: {\"reason\":\"raw\nnewline\"}",
                "",
                "LOOM_COMPLETE\nLOOM_COMPLETE",
                "LOOM_COMPLETE\ntrailing commentary",
            ] {
                for text in controlled_outputs(&stream, terminal).await {
                    let walk = WalkOutput::from_stdout(&text, DispatchScope::Tree, &validator);
                    assert_eq!(walk.context().raw(), text);
                    assert_eq!(walk.findings().len(), valid, "{text}");
                    assert_eq!(walk.finding_errors().len(), errors, "{text}");
                    let verdict = phase_verdict_from_walk(&walk);
                    for exit_code in [0, 17] {
                        let outcome = classify_review_phase(&walk, exit_code);
                        assert_eq!(
                            matches!(outcome, crate::review::ReviewOutcome::Complete),
                            matches!(verdict, PhaseVerdict::Done) && exit_code == 0,
                            "{text}: {outcome:?}",
                        );
                        if let crate::review::ReviewOutcome::Incomplete { detail } = outcome {
                            assert!(!detail.is_empty());
                            if let PhaseVerdict::Recovery {
                                cause: RecoveryCause::AgentRetry { reason },
                            }
                            | PhaseVerdict::Blocked { reason } = &verdict
                            {
                                assert!(detail.contains(reason), "{text}: {detail}");
                            }
                        }
                    }
                    match &verdict {
                        PhaseVerdict::Done => {
                            assert_eq!(terminal, "LOOM_COMPLETE");
                            assert_eq!((valid, errors), (0, 0));
                            assert!(
                                parse_walk_output(&text, DispatchScope::Tree, &validator).is_ok()
                            );
                        }
                        PhaseVerdict::Blocked { reason } => {
                            assert_eq!(terminal, "LOOM_BLOCKED: {\"reason\":\"blocked reason\"}");
                            assert_eq!(reason, "blocked reason");
                            assert_eq!((valid, errors), (0, 0));
                        }
                        PhaseVerdict::Recovery { cause } => {
                            let previous = cause_to_previous_failure(cause);
                            match &previous {
                                PreviousFailure::BadWalk(bad_walk) => {
                                    assert_bad_walk_transfer(&walk, bad_walk);
                                    let Err(WalkOutputError::BadWalk { bad_walk: parsed }) =
                                        parse_walk_output(&text, DispatchScope::Tree, &validator)
                                    else {
                                        panic!("parser and classifier disagree: {text}");
                                    };
                                    assert_eq!(&parsed, bad_walk);
                                    if walk.decoded().is_err() {
                                        assert!(matches!(bad_walk, BadWalk::Protocol { .. }));
                                        if terminal == TODO {
                                            assert!(bad_walk.diagnostics().iter().any(
                                                |diagnostic| matches!(
                                                    diagnostic.error,
                                                    Error::WrongPhase {
                                                        marker: "LOOM_TODO",
                                                        phase: Phase::Review,
                                                    }
                                                )
                                            ));
                                        }
                                    } else if errors > 0 {
                                        assert!(matches!(
                                            bad_walk,
                                            BadWalk::MalformedFinding { .. }
                                        ));
                                    } else if valid == 0 {
                                        assert!(matches!(
                                            bad_walk,
                                            BadWalk::ConcernWithoutFindings { .. }
                                        ));
                                    } else {
                                        assert!(matches!(
                                            bad_walk,
                                            BadWalk::FindingsWithoutConcern { .. }
                                        ));
                                    }
                                }
                                PreviousFailure::ReviewConcern { summary, findings } => {
                                    assert_eq!(terminal, CONCERN);
                                    assert_eq!(errors, 0);
                                    assert!(valid > 0);
                                    assert_eq!(summary, "review summary");
                                    assert_eq!(findings, walk.findings());
                                }
                                PreviousFailure::AgentRetry { reason } => {
                                    assert_eq!(
                                        terminal,
                                        "LOOM_RETRY: {\"reason\":\"retry reason\"}"
                                    );
                                    assert_eq!(reason, "retry reason");
                                    assert_eq!((valid, errors), (0, 0));
                                }
                                other => panic!("lost review context: {other:?}: {text}"),
                            }
                            let saved = previous.clone();
                            let rendered = previous.to_string();
                            assert!(!rendered.is_empty());
                            assert!(rendered.len() <= PREVIOUS_FAILURE_MAX_LEN);
                            assert_eq!(previous, saved);
                        }
                        other => panic!("unexpected review verdict: {other:?}: {text}"),
                    }
                }
            }
            for text in controlled_outputs("LOOM_COMPLETE", &format!("{stream}\ncommentary")).await
            {
                let walk = WalkOutput::from_stdout(&text, DispatchScope::Tree, &validator);
                assert_eq!(walk.findings().len(), valid);
                assert_eq!(walk.finding_errors().len(), errors);
                let PhaseVerdict::Recovery {
                    cause: RecoveryCause::BadWalk(bad_walk),
                } = phase_verdict_from_walk(&walk)
                else {
                    panic!("misplaced terminal admitted: {text}");
                };
                assert_bad_walk_transfer(&walk, &bad_walk);
                assert!(
                    bad_walk
                        .diagnostics()
                        .iter()
                        .any(|diagnostic| matches!(diagnostic.error, Error::TrailingText))
                );
            }
        }
    }

    #[test]
    fn malformed_finding_recovery_retains_valid_findings_errors_and_terminal() {
        let directory = workspace();
        let validator = WorkspaceFindingValidator::new(directory.path());
        for terminal in [CONCERN, "LOOM_CONCERN: {\"summary\":\"\"}", ""] {
            let text = format!("{FIRST}\n{BAD_TARGET}\n{BAD_JSON}\n{LAST}\n{terminal}\n");
            let walk = WalkOutput::from_stdout(&text, DispatchScope::Tree, &validator);
            assert_eq!(walk.findings().len(), 2);
            assert_eq!(
                walk.finding_errors()
                    .iter()
                    .map(FindingParseError::line_number)
                    .collect::<Vec<_>>(),
                [2, 3]
            );
            let PhaseVerdict::Recovery {
                cause: RecoveryCause::BadWalk(bad_walk),
            } = phase_verdict_from_walk(&walk)
            else {
                panic!("mixed stream must recover");
            };
            assert_bad_walk_transfer(&walk, &bad_walk);
            if terminal == CONCERN {
                assert!(matches!(bad_walk, BadWalk::Protocol { .. }));
                assert!(matches!(
                    bad_walk.terminal(),
                    TerminalSurface::Concern { .. }
                ));
            } else {
                assert_eq!(bad_walk.terminal(), TerminalSurface::Missing);
            }
            assert_eq!(
                cause_to_previous_failure(&RecoveryCause::BadWalk(bad_walk.clone())),
                PreviousFailure::BadWalk(bad_walk)
            );
        }
    }

    #[test]
    fn strict_finding_json_preserves_escaped_evidence_and_rejects_raw_newlines() {
        let directory = workspace();
        let validator = WorkspaceFindingValidator::new(directory.path());
        let raw: serde_json::Value =
            serde_json::from_str(FIRST.strip_prefix("LOOM_FINDING: ").unwrap()).unwrap();
        let pretty = format!(
            "LOOM_FINDING: {}",
            serde_json::to_string_pretty(&raw).unwrap()
        );
        let compact = parse_walk_output(
            &format!("{FIRST}\n{CONCERN}"),
            DispatchScope::Tree,
            &validator,
        )
        .unwrap();
        let formatted = parse_walk_output(
            &format!("{pretty}\n{CONCERN}"),
            DispatchScope::Tree,
            &validator,
        )
        .unwrap();
        assert_eq!(formatted, compact);
        assert_eq!(compact[0].evidence(), "first\nsecond 🧶");
        assert_eq!(compact[0].id(), formatted[0].id());
        assert_eq!(compact[0].hash(), formatted[0].hash());
        let invalid = FIRST.replace("first\\nsecond", "first\nsecond");
        let text = format!("{invalid}\n{CONCERN}");
        let Err(WalkOutputError::BadWalk { bad_walk }) =
            parse_walk_output(&text, DispatchScope::Tree, &validator)
        else {
            panic!("raw string newline repaired");
        };
        assert_eq!(bad_walk.context().raw(), text);
        assert!(bad_walk.parsed_findings().is_empty());
        assert_eq!(bad_walk.finding_errors()[0].line_number(), 1);
    }

    #[test]
    fn shared_finding_decode_and_resolution_fail_with_record_context() {
        let directory = workspace();
        let validator = WorkspaceFindingValidator::new(directory.path());
        for record in [
            FIRST.replace("mock-discipline", "unknown-token"),
            FIRST.replace("alpha", "missing-spec"),
            FIRST.replace("mock-discipline", "verifier-bypass"),
            BAD_TARGET.to_owned(),
            FIRST.replace("mock-discipline", "cross-spec-clash"),
            BAD_JSON.to_owned(),
        ] {
            let text = format!("commentary\n{FIRST}\n{record}\n{LAST}\n{CONCERN}");
            let walk = WalkOutput::from_stdout(&text, DispatchScope::Tree, &validator);
            assert_eq!(walk.findings().len(), 2);
            assert_eq!(walk.finding_errors().len(), 1);
            assert_eq!(walk.finding_errors()[0].line_number(), 3);
            assert!(walk.finding_errors()[0].to_string().contains(&record));
            assert_bad_walk_transfer(&walk, &walk.bad_walk().unwrap());
        }
        let text = format!("{FIRST}\nLOOM_CLARIFY: {{\"decisions\":[\"lm-decision\"]}}\n{CONCERN}");
        let bad_walk = WalkOutput::from_stdout(&text, DispatchScope::Tree, &validator)
            .bad_walk()
            .unwrap();
        assert!(
            bad_walk
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.span.line == 2
                    && matches!(
                        diagnostic.error,
                        Error::WrongPhase {
                            phase: Phase::Review,
                            ..
                        }
                    ))
        );
        assert!(bad_walk.finding_errors().is_empty());
    }

    #[test]
    fn review_pairing_does_not_promote_examples_or_payload_marker_text() {
        let directory = workspace();
        let validator = WorkspaceFindingValidator::new(directory.path());
        for example in [
            format!("```json\n{FIRST}\n```"),
            format!("`{FIRST}`"),
            format!("prose {FIRST}"),
            format!("  {FIRST}"),
            FIRST.replace("first\\nsecond 🧶", "LOOM_FINDING: {} LOOM_COMPLETE"),
        ] {
            let text = format!("{example}\n{CONCERN}");
            let walk = WalkOutput::from_stdout(&text, DispatchScope::Tree, &validator);
            if example.starts_with("LOOM_FINDING:") {
                assert_eq!(walk.findings().len(), 1);
                assert_eq!(walk.context().messages().len(), 2);
            } else {
                assert!(walk.findings().is_empty());
                assert!(matches!(
                    walk.bad_walk(),
                    Some(BadWalk::ConcernWithoutFindings { .. })
                ));
            }
        }
    }

    #[test]
    fn review_domain_entry_points_delegate_to_shared_output_decoder() {
        let directory = workspace();
        let validator = WorkspaceFindingValidator::new(directory.path());
        for text in [
            format!("{FIRST}\nLOOM_FINDNG: {{}}\n{CONCERN}"),
            format!("{FIRST}\nLOOM_NOOP"),
            format!("{FIRST}\nLOOM_RETRY"),
            format!("{FIRST}\n{CONCERN} trailing fence```"),
            format!("{FIRST}\nLOOM_CONCERN: {{\"summary\":\"unterminated\nLOOM_COMPLETE"),
        ] {
            let canonical = decode(&text, Phase::Review).unwrap_err();
            let walk = WalkOutput::from_stdout(&text, DispatchScope::Tree, &validator);
            assert_eq!(walk.context(), canonical.context());
            assert_eq!(walk.decoded().as_ref().unwrap_err(), &canonical);
            let bad_walk = walk.bad_walk().unwrap();
            assert_eq!(bad_walk.diagnostics(), canonical.diagnostics());
            assert_bad_walk_transfer(&walk, &bad_walk);
        }
    }

    #[test]
    fn review_message_admission_preserves_inspection_and_requires_terminal() {
        let directory = workspace();
        let validator = WorkspaceFindingValidator::new(directory.path());
        for text in [
            FIRST.to_owned(),
            format!("{FIRST}\nLOOM_CLARIFY: {{\"decisions\":[\"lm-decision\"]}}\n{CONCERN}"),
        ] {
            assert!(parse_walk_output(&text, DispatchScope::Tree, &validator).is_err());
            let walk = WalkOutput::from_stdout(&text, DispatchScope::Tree, &validator);
            assert_eq!(walk.findings().len(), 1);
            assert!(matches!(
                phase_verdict_from_walk(&walk),
                PhaseVerdict::Recovery {
                    cause: RecoveryCause::BadWalk(_)
                }
            ));
        }
        let clarify = FIRST.replace("deferred", "clarify");
        assert_eq!(
            parse_walk_output(
                &format!("{clarify}\n{CONCERN}"),
                DispatchScope::Tree,
                &validator
            )
            .unwrap()
            .len(),
            1
        );
        assert!(!directory.path().join(".beads").exists());
    }
}
