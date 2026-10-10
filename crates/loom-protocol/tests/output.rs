#![cfg(test)]

use std::collections::HashSet;

use loom_events::identifier::BeadId;
use loom_protocol::gate::{ConcernToken, FindingRoute, FindingTarget, RawFinding};
use loom_protocol::output::{
    Decisions, Error, Message, Phase, Proposals, Reason, Role, Summary, decode,
};
use loom_protocol::todo::{
    NonEmptyString, NonEmptyVec, TodoFingerprint, TodoSpecOutcome, TodoSpecSuccess, TodoSuccess,
};

const FINDING: &str = r#"LOOM_FINDING: {"token":"verifier-bypass","route":"deferred","bonds":["protocol"],"target":{"kind":"Annotation","target_string":"cargo test output"},"evidence":"Observed failure"}"#;
const CLARIFY: &str = r#"LOOM_CLARIFY: {"decisions":["lm-decision.1","lm-decision.2"]}"#;
const CONCERN: &str = r#"LOOM_CONCERN: {"summary":"Observed failure"}"#;
const RETRY: &str = r#"LOOM_RETRY: {"reason":"Transient failure"}"#;
const BLOCKED: &str = r#"LOOM_BLOCKED: {"reason":"No safe options"}"#;
const APPLY: &str = r#"LOOM_APPLY: {"proposals":["lm-tune.1","lm-tune.2"]}"#;
const TODO: &str = r#"LOOM_TODO: {"head":"0123456789abcdef0123456789abcdef01234567","fingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","work_epic":"lm-work.1","title":"Protocol work","specs":[{"label":"protocol","outcome":"decomposed","beads":["lm-task.1"]}]}"#;
const TODO_NO_WORK: &str = r#"LOOM_TODO: {"head":"0123456789abcdef0123456789abcdef01234567","fingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","work_epic":"lm-work.1","title":"Protocol work","specs":[{"label":"protocol","outcome":"no-work","reason":"Already covered"}]}"#;

fn text(value: &str) -> NonEmptyString {
    NonEmptyString::new(value).unwrap()
}

fn ids(values: &[&str]) -> NonEmptyVec<BeadId> {
    NonEmptyVec::new(values.iter().map(|value| value.parse().unwrap()).collect()).unwrap()
}

fn finding() -> Message {
    Message::Finding(RawFinding {
        token: ConcernToken::VerifierBypass,
        route: FindingRoute::Deferred,
        bonds: vec!["protocol".parse().unwrap()],
        target: FindingTarget::Annotation {
            target_string: "cargo test output".into(),
        },
        evidence: "Observed failure".into(),
    })
}

fn todo(outcome: TodoSpecOutcome) -> Message {
    Message::Todo(TodoSuccess {
        head: "0123456789abcdef0123456789abcdef01234567".parse().unwrap(),
        fingerprint: TodoFingerprint::new(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap(),
        work_epic: "lm-work.1".parse().unwrap(),
        title: text("Protocol work"),
        specs: NonEmptyVec::new(vec![TodoSpecSuccess {
            label: "protocol".parse().unwrap(),
            outcome,
        }])
        .unwrap(),
    })
}

struct Fixture {
    name: &'static str,
    wire: &'static str,
    message: Message,
    role: Role,
    phase: Phase,
}

fn fixtures() -> Vec<Fixture> {
    vec![
        Fixture {
            name: "finding_record",
            role: Role::Record,
            wire: FINDING,
            message: finding(),
            phase: Phase::Review,
        },
        Fixture {
            name: "clarify_record",
            role: Role::Record,
            wire: CLARIFY,
            message: Message::Clarify(Decisions {
                decisions: ids(&["lm-decision.1", "lm-decision.2"]),
            }),
            phase: Phase::Loop,
        },
        Fixture {
            name: "complete_unit",
            role: Role::Terminal,
            wire: "LOOM_COMPLETE",
            message: Message::Complete,
            phase: Phase::Plan,
        },
        Fixture {
            name: "noop_unit",
            role: Role::Terminal,
            wire: "LOOM_NOOP",
            message: Message::Noop,
            phase: Phase::Loop,
        },
        Fixture {
            name: "waiting_unit",
            role: Role::Terminal,
            wire: "LOOM_WAITING",
            message: Message::Waiting,
            phase: Phase::Todo,
        },
        Fixture {
            name: "concern_summary",
            role: Role::Terminal,
            wire: CONCERN,
            message: Message::Concern(Summary {
                summary: text("Observed failure"),
            }),
            phase: Phase::Review,
        },
        Fixture {
            name: "retry_reason",
            role: Role::Terminal,
            wire: RETRY,
            message: Message::Retry(Reason {
                reason: text("Transient failure"),
            }),
            phase: Phase::Todo,
        },
        Fixture {
            name: "blocked_reason",
            role: Role::Terminal,
            wire: BLOCKED,
            message: Message::Blocked(Reason {
                reason: text("No safe options"),
            }),
            phase: Phase::Loop,
        },
        Fixture {
            name: "todo_decomposed",
            role: Role::Terminal,
            wire: TODO,
            message: todo(TodoSpecOutcome::Decomposed {
                beads: ids(&["lm-task.1"]),
            }),
            phase: Phase::Todo,
        },
        Fixture {
            name: "todo_no_work",
            role: Role::Terminal,
            wire: TODO_NO_WORK,
            message: todo(TodoSpecOutcome::NoWork {
                reason: text("Already covered"),
            }),
            phase: Phase::Todo,
        },
        Fixture {
            name: "apply_proposals",
            role: Role::Terminal,
            wire: APPLY,
            message: Message::Apply(Proposals {
                proposals: ids(&["lm-tune.1", "lm-tune.2"]),
            }),
            phase: Phase::Inbox,
        },
    ]
}

fn session_wire(fixture: &Fixture, phase: Phase) -> String {
    if fixture.role == Role::Record {
        let terminal = if phase == Phase::Todo {
            "LOOM_WAITING"
        } else {
            "LOOM_COMPLETE"
        };
        format!("{}\n{terminal}", fixture.wire)
    } else {
        fixture.wire.to_owned()
    }
}

#[test]
fn canonical_agent_output_message_contract_is_constructible() {
    for fixture in fixtures() {
        let wire = fixture.message.to_wire().unwrap();
        assert_eq!(fixture.message.role(), fixture.role, "{}", fixture.name);
        assert!(
            wire.starts_with(fixture.message.marker()),
            "{}",
            fixture.name
        );
        let session = decode(&session_wire(&fixture, fixture.phase), fixture.phase).unwrap();
        assert_eq!(
            session.context().messages()[0].message,
            fixture.message,
            "{}",
            fixture.name
        );
    }
    assert!(NonEmptyString::new(" \n\t").is_err());
    assert!(NonEmptyVec::<BeadId>::new(vec![]).is_err());
    assert!(BeadId::new("not a bead").is_err());
    assert!(serde_json::from_str::<Decisions>(r#"{"decisions":[]}"#).is_err());
    assert!(serde_json::from_str::<Proposals>(r#"{"proposals":["bad bead"]}"#).is_err());
    assert!(serde_json::from_str::<Reason>(r#"{"reason":" "}"#).is_err());
    assert!(serde_json::from_str::<Summary>(r#"{"summary":""}"#).is_err());
}

#[test]
fn canonical_agent_output_wire_fixtures_pin_variant_shapes() {
    for fixture in fixtures() {
        let session = decode(&session_wire(&fixture, fixture.phase), fixture.phase).unwrap();
        assert_eq!(
            session.context().messages()[0].message,
            fixture.message,
            "{}",
            fixture.name
        );
        assert_eq!(
            fixture.message.to_wire().unwrap(),
            fixture.wire,
            "{}",
            fixture.name
        );
    }
    let invalid = [
        ("finding_missing_payload", "LOOM_FINDING"),
        (
            "finding_missing_evidence",
            r#"LOOM_FINDING: {"token":"verifier-bypass","route":"deferred","bonds":["protocol"],"target":{"kind":"Annotation","target_string":"cargo test output"}}"#,
        ),
        (
            "finding_invalid_spec",
            r#"LOOM_FINDING: {"token":"verifier-bypass","route":"deferred","bonds":["bad spec"],"target":{"kind":"Annotation","target_string":"cargo test output"},"evidence":"failure"}"#,
        ),
        (
            "finding_unknown_token",
            r#"LOOM_FINDING: {"token":"verifier-bypas","route":"deferred","bonds":["protocol"],"target":{"kind":"Annotation","target_string":"cargo test output"},"evidence":"failure"}"#,
        ),
        ("clarify_missing_payload", "LOOM_CLARIFY"),
        ("clarify_missing_decisions", "LOOM_CLARIFY: {}"),
        (
            "clarify_empty_decisions",
            r#"LOOM_CLARIFY: {"decisions":[]}"#,
        ),
        (
            "clarify_bad_id",
            r#"LOOM_CLARIFY: {"decisions":["lm-ok","bad bead"]}"#,
        ),
        ("clarify_null", r#"LOOM_CLARIFY: {"decisions":null}"#),
        (
            "clarify_extra_field",
            r#"LOOM_CLARIFY: {"decisions":["lm-ok"],"question":"legacy"}"#,
        ),
        ("complete_extra_payload", "LOOM_COMPLETE: {}"),
        ("noop_extra_payload", "LOOM_NOOP: {}"),
        ("waiting_extra_payload", "LOOM_WAITING: {}"),
        ("concern_missing_payload", "LOOM_CONCERN"),
        ("concern_missing_summary", "LOOM_CONCERN: {}"),
        (
            "concern_blank_summary",
            r#"LOOM_CONCERN: {"summary":" \t"}"#,
        ),
        ("concern_wrong_type", r#"LOOM_CONCERN: {"summary":42}"#),
        ("retry_missing_payload", "LOOM_RETRY"),
        ("retry_missing_reason", "LOOM_RETRY: {}"),
        ("retry_blank_reason", r#"LOOM_RETRY: {"reason":"\n"}"#),
        ("retry_legacy_prose", "temporary failure\nLOOM_RETRY"),
        ("retry_wrong_field", r#"LOOM_RETRY: {"summary":"failure"}"#),
        ("blocked_missing_payload", "LOOM_BLOCKED"),
        ("blocked_missing_reason", "LOOM_BLOCKED: {}"),
        ("blocked_blank_reason", r#"LOOM_BLOCKED: {"reason":" "}"#),
        ("blocked_legacy_prose", "no options\nLOOM_BLOCKED"),
        ("blocked_wrong_type", r#"LOOM_BLOCKED: {"reason":false}"#),
        ("todo_missing_payload", "LOOM_TODO"),
        ("todo_missing_fields", "LOOM_TODO: {}"),
        (
            "todo_bad_sha",
            r#"LOOM_TODO: {"head":"bad","fingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","work_epic":"lm-work","title":"work","specs":[{"label":"protocol","outcome":"no-work","reason":"covered"}]}"#,
        ),
        (
            "todo_empty_specs",
            r#"LOOM_TODO: {"head":"0123456789abcdef0123456789abcdef01234567","fingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","work_epic":"lm-work","title":"work","specs":[]}"#,
        ),
        ("apply_missing_payload", "LOOM_APPLY"),
        ("apply_missing_proposals", "LOOM_APPLY: {}"),
        ("apply_empty_proposals", r#"LOOM_APPLY: {"proposals":[]}"#),
        ("apply_bad_id", r#"LOOM_APPLY: {"proposals":["bad bead"]}"#),
        (
            "apply_extra_field",
            r#"LOOM_APPLY: {"proposals":["lm-tune"],"protocol":1}"#,
        ),
    ];
    for (name, wire) in invalid {
        let failure = decode(wire, Phase::Loop).expect_err(name);
        assert!(
            failure.context().messages().is_empty(),
            "{name}: {failure:?}"
        );
        assert!(
            failure.diagnostics().iter().any(|diagnostic| matches!(
                diagnostic.error,
                Error::ExpectedObject | Error::UnexpectedPayload | Error::Json { .. }
            )),
            "{name}: {failure:?}"
        );
    }
}

#[test]
fn shared_decoder_enforces_root_line_framing_and_strict_json() {
    let excluded = [
        ("inline_mention", "Use `LOOM_COMPLETE` when done."),
        ("prose_prefix", "Result: LOOM_COMPLETE"),
        ("unclosed_inline_delimiter", "An unmatched ` is prose."),
        ("indented", " LOOM_COMPLETE"),
        ("bullet", "- LOOM_COMPLETE"),
        ("blockquote", "> LOOM_COMPLETE"),
        ("bold", "**LOOM_COMPLETE**"),
        ("heading", "# LOOM_COMPLETE"),
        (
            "backtick_fence",
            "```text\nLOOM_FINDNG: {}\nLOOM_COMPLETE\n```",
        ),
        ("tilde_fence", "~~~json\nLOOM_COMPLETE\n~~~"),
        (
            "indented_long_fence",
            "   ````text\n```\nLOOM_COMPLETE\n   ````",
        ),
        ("multiline_inline", "Example `\nLOOM_COMPLETE\n` end."),
        ("double_tick_inline", "Example ``\nLOOM_FINDNG: {}\n`` end."),
        (
            "event_wrapper",
            r#"{"type":"text_delta","delta":"LOOM_COMPLETE"}"#,
        ),
    ];
    for (name, example) in excluded {
        let wire = format!("{example}\nLOOM_COMPLETE\n");
        let session =
            decode(&wire, Phase::Loop).unwrap_or_else(|failure| panic!("{name}: {failure:?}"));
        assert_eq!(session.context().messages().len(), 1, "{name}");
        assert_eq!(session.terminal().message, Message::Complete, "{name}");
    }
    let unknown = decode("LOOM_FINDNG: {}\nLOOM_COMPLETE", Phase::Review).unwrap_err();
    assert!(
        matches!(&unknown.diagnostics()[0].error, Error::UnknownMarker { marker } if marker == "LOOM_FINDNG")
    );
    assert_eq!(
        unknown.context().terminal().unwrap().message,
        Message::Complete
    );

    let invalid = [
        ("raw_newline", "LOOM_RETRY: {\"reason\":\"first\nsecond\"}"),
        ("raw_tab", "LOOM_RETRY: {\"reason\":\"first\tsecond\"}"),
        (
            "raw_control",
            "LOOM_RETRY: {\"reason\":\"first\u{0001}second\"}",
        ),
        ("bad_escape", r#"LOOM_RETRY: {"reason":"bad\qescape"}"#),
        ("trailing_comma", r#"LOOM_RETRY: {"reason":"fail",}"#),
        (
            "duplicate_field",
            r#"LOOM_RETRY: {"reason":"a","reason":"b"}"#,
        ),
        ("array_not_object", r#"LOOM_RETRY: ["failure"]"#),
        ("scalar_not_object", r#"LOOM_RETRY: "failure""#),
        ("null_not_object", "LOOM_RETRY: null"),
        ("missing_colon", r#"LOOM_RETRY {"reason":"failure"}"#),
        ("space_before_colon", r#"LOOM_RETRY : {"reason":"failure"}"#),
        ("suffix_prose", r#"LOOM_RETRY: {"reason":"failure"} prose"#),
        ("suffix_object", r#"LOOM_RETRY: {"reason":"failure"} {}"#),
        ("suffix_fence", r#"LOOM_RETRY: {"reason":"failure"}```"#),
        (
            "suffix_marker",
            r#"LOOM_RETRY: {"reason":"failure"} LOOM_COMPLETE"#,
        ),
        (
            "pretty_suffix",
            "LOOM_RETRY: {\n\"reason\": \"failure\"\n} prose",
        ),
        ("wrong_case", r#"LOOM_retry: {"reason":"failure"}"#),
        ("unknown_namespace", "LOOM_FUTURE"),
    ];
    for (name, wire) in invalid {
        let failure = decode(wire, Phase::Loop).expect_err(name);
        assert!(
            failure.context().messages().is_empty(),
            "{name}: {failure:?}"
        );
        assert!(failure.context().terminal().is_none(), "{name}");
    }
    let escaped = decode(r#"LOOM_RETRY:{"reason":"line\nquote \" brace } backslash \\ tab\tcontrol\u0001 LOOM_COMPLETE"}"#, Phase::Loop).unwrap();
    assert_eq!(
        escaped.terminal().message,
        Message::Retry(Reason {
            reason: text("line\nquote \" brace } backslash \\ tab\tcontrol\u{0001} LOOM_COMPLETE")
        })
    );
}

#[test]
fn shared_decoder_treats_multiline_payload_as_one_logical_message() {
    let compact = r#"LOOM_FINDING: {"token":"verifier-bypass","route":"deferred","bonds":["protocol"],"target":{"kind":"Annotation","target_string":"cargo test output"},"evidence":"first\nLOOM_COMPLETE\nquote \" }"}"#;
    let pretty = "LOOM_FINDING:\n{\n  \"token\": \"verifier-bypass\",\n  \"route\": \"deferred\",\n  \"bonds\": [\"protocol\"],\n  \"target\": {\"kind\": \"Annotation\", \"target_string\": \"cargo test output\"},\n  \"evidence\": \"first\\nLOOM_COMPLETE\\nquote \\\" }\"\n}";
    let compact_session =
        decode(&format!("{compact}\ncommentary\n{CONCERN}"), Phase::Review).unwrap();
    let pretty_session = decode(
        &format!(
            "{pretty}\ncommentary\nLOOM_CONCERN: {{\n  \"summary\": \"Observed failure\"\n}}\t \n\n"
        ),
        Phase::Review,
    )
    .unwrap();
    assert_eq!(
        compact_session
            .context()
            .messages()
            .iter()
            .map(|located| &located.message)
            .collect::<Vec<_>>(),
        pretty_session
            .context()
            .messages()
            .iter()
            .map(|located| &located.message)
            .collect::<Vec<_>>()
    );
    let context = pretty_session.context();
    let record = &context.messages()[0];
    assert_eq!(&context.raw()[record.span.bytes.clone()], pretty);
    assert_eq!(record.span.line, 1);
    assert_eq!(pretty_session.terminal().span.line, 10);
    assert!(matches!(
        pretty_session.terminal().message,
        Message::Concern(_)
    ));

    let crlf = decode(
        "LOOM_APPLY:\r\n{\r\n  \"proposals\": [\"lm-tune.1\", \"lm-tune.2\"]\r\n}\t \r\n",
        Phase::Inbox,
    )
    .unwrap();
    assert_eq!(
        crlf.terminal().message,
        Message::Apply(Proposals {
            proposals: ids(&["lm-tune.1", "lm-tune.2"])
        })
    );
    let unicode = decode(
        "LOOM_RETRY: {\"reason\":\"unicode\u{2028}\u{2029}\"}",
        Phase::Loop,
    )
    .unwrap();
    assert_eq!(
        unicode.terminal().message,
        Message::Retry(Reason {
            reason: text("unicode\u{2028}\u{2029}")
        })
    );
}

#[test]
fn shared_decoder_enforces_terminal_cardinality_and_position() {
    let invalid = [
        ("empty", ""),
        ("commentary_only", "done"),
        ("record_only", CLARIFY),
        ("duplicate", "LOOM_COMPLETE\nLOOM_COMPLETE"),
        ("distinct_terminals", "LOOM_NOOP\nLOOM_COMPLETE"),
        ("earlier_terminal", "LOOM_COMPLETE\nmore commentary"),
        ("terminal_inline_suffix", "LOOM_COMPLETE done"),
        ("two_on_one_line", "LOOM_COMPLETE LOOM_NOOP"),
        (
            "record_after_terminal",
            "LOOM_COMPLETE\nLOOM_CLARIFY: {\"decisions\":[\"lm-decision\"]}",
        ),
        ("fence_after_terminal", "LOOM_COMPLETE\n```\nexample\n```"),
        (
            "data_terminal_then_commentary",
            "LOOM_RETRY: {\n\"reason\":\"failure\"\n}\nmore",
        ),
        ("terminal_inside_unterminated_fence", "```\nLOOM_COMPLETE"),
    ];
    for (name, wire) in invalid {
        assert!(decode(wire, Phase::Loop).is_err(), "{name}");
    }
    let session = decode("commentary\nLOOM_CLARIFY: {\"decisions\":[\"lm-decision\"]}\nmore commentary\nLOOM_COMPLETE\t \n \n", Phase::Loop).unwrap();
    assert_eq!(session.terminal().message, Message::Complete);
    assert_eq!(session.context().messages().len(), 2);
}

#[test]
fn shared_decoder_retains_valid_context_with_raw_errors() {
    let wire = "α commentary\nLOOM_CLARIFY: {\"decisions\":[\"lm-one\"]}\nLOOM_CLARIFY: {\"decisions\":[]}\nLOOM_FINDNG: {}\nLOOM_CLARIFY: {\n\"decisions\": [\"lm-two\"]\n}\nLOOM_COMPLETE\n";
    let failure = decode(wire, Phase::Loop).unwrap_err();
    assert_eq!(failure.context().raw(), wire);
    assert_eq!(failure.context().messages().len(), 3);
    assert_eq!(
        failure.context().terminal().unwrap().message,
        Message::Complete
    );
    assert_eq!(
        failure.context().decisions(),
        ["lm-one".parse().unwrap(), "lm-two".parse().unwrap()]
            .into_iter()
            .collect()
    );
    assert_eq!(failure.diagnostics().len(), 2);
    assert_eq!(failure.diagnostics()[0].span.line, 3);
    assert_eq!(failure.diagnostics()[1].span.line, 4);
    for diagnostic in failure.diagnostics() {
        let raw = &wire[diagnostic.span.bytes.clone()];
        assert!(raw.starts_with("LOOM_"));
        assert!(!raw.contains('\n'));
    }
    assert_eq!(
        &wire[failure.context().messages()[1].span.bytes.clone()],
        "LOOM_CLARIFY: {\n\"decisions\": [\"lm-two\"]\n}"
    );

    let ambiguous = [
        (
            "missing_colon_unterminated_object",
            "LOOM_CLARIFY {\"decisions\":[\"lm-one\"]\nLOOM_COMPLETE\n",
        ),
        (
            "unterminated_top_level_string",
            "LOOM_RETRY: \"failure\nLOOM_COMPLETE\n",
        ),
        (
            "unterminated_top_level_array",
            "LOOM_RETRY: [\nLOOM_COMPLETE\n",
        ),
        (
            "marker_inside_closed_top_level_string",
            "LOOM_RETRY: \"failure\nLOOM_COMPLETE\nmore\"",
        ),
        (
            "unterminated_string",
            "LOOM_CLARIFY: {\"decisions\":[\"lm-one\nLOOM_COMPLETE\n",
        ),
        (
            "unterminated_object",
            "LOOM_CLARIFY: {\"decisions\":[\"lm-one\"]\nLOOM_COMPLETE\n",
        ),
        (
            "mismatched_nesting",
            "LOOM_CLARIFY: {\"decisions\":[}\nLOOM_COMPLETE\n",
        ),
        (
            "marker_inside_raw_string",
            "LOOM_RETRY: {\"reason\":\"failure\nLOOM_COMPLETE\nmore\"}",
        ),
        (
            "ambiguous_suffix_terminal",
            "LOOM_CLARIFY: {\"decisions\":[\"lm-one\"]} LOOM_COMPLETE",
        ),
    ];
    for (name, malformed) in ambiguous {
        let wire = format!("{CLARIFY}\n{malformed}");
        let failure = decode(&wire, Phase::Loop).expect_err(name);
        assert_eq!(failure.context().raw(), wire, "{name}");
        assert_eq!(failure.context().messages().len(), 1, "{name}");
        assert!(failure.context().terminal().is_none(), "{name}");
        let raw = &wire[failure.diagnostics()[0].span.bytes.clone()];
        assert_eq!(raw, malformed, "{name}");
    }
}

#[test]
fn canonical_phase_admission_rejects_wrong_message_roles() {
    let phases = [
        Phase::Plan,
        Phase::Todo,
        Phase::Loop,
        Phase::Review,
        Phase::Inbox,
    ];
    let expected = [
        ("finding_record", [false, false, false, true, false]),
        ("clarify_record", [false, true, true, false, false]),
        ("complete_unit", [true, false, true, true, true]),
        ("noop_unit", [false, false, true, false, false]),
        ("waiting_unit", [false, true, true, false, false]),
        ("concern_summary", [false, false, false, true, false]),
        ("retry_reason", [false, true, true, true, false]),
        ("blocked_reason", [false, true, true, true, false]),
        ("todo_decomposed", [false, true, false, false, false]),
        ("todo_no_work", [false, true, false, false, false]),
        ("apply_proposals", [false, false, false, false, true]),
    ];
    for (fixture, (name, allowed)) in fixtures().into_iter().zip(expected) {
        assert_eq!(fixture.name, name);
        for (phase, allowed) in phases.into_iter().zip(allowed) {
            let result = decode(&session_wire(&fixture, phase), phase);
            assert_eq!(result.is_ok(), allowed, "{name} in {phase:?}: {result:?}");
            if let Err(failure) = result {
                assert_eq!(failure.context().messages()[0].message, fixture.message);
                assert!(
                    failure
                        .diagnostics()
                        .iter()
                        .any(|diagnostic| matches!(diagnostic.error, Error::WrongPhase { .. })),
                    "{name} in {phase:?}"
                );
            }
        }
    }
}

#[test]
fn clarify_records_aggregate_exact_decision_ids() {
    let records = "LOOM_CLARIFY: {\"decisions\":[\"lm-decision.1\",\"lm-decision.1\",\"lm-decision.2\"]}\nLOOM_CLARIFY: {\n\"decisions\":[\"lm-decision.2\",\"lm-other\"]\n}";
    let expected: HashSet<BeadId> = ["lm-decision.1", "lm-decision.2", "lm-other"]
        .into_iter()
        .map(|id| id.parse().unwrap())
        .collect();
    for phase in [Phase::Todo, Phase::Loop] {
        for fixture in fixtures()
            .into_iter()
            .filter(|fixture| fixture.role == Role::Terminal && phase.admits(&fixture.message))
        {
            let wire = format!("{records}\n{}", fixture.wire);
            let session = decode(&wire, phase).unwrap();
            assert_eq!(
                session.context().decisions(),
                expected,
                "{} in {phase:?}",
                fixture.name
            );
            assert_eq!(session.terminal().message, fixture.message);
        }
    }
    let wire = format!(
        "{records}\nLOOM_CLARIFY: {{\"decisions\":[\"lm-valid\",\"not a bead\"]}}\n{RETRY}"
    );
    let failure = decode(&wire, Phase::Loop).unwrap_err();
    assert_eq!(failure.context().decisions(), expected);
    assert_eq!(failure.context().messages().len(), 3);
    assert!(matches!(
        failure.context().terminal().unwrap().message,
        Message::Retry(_)
    ));
    assert!(matches!(failure.diagnostics()[0].error, Error::Json { .. }));
}

#[test]
fn canonical_messages_have_no_payload_version_or_language_field() {
    for fixture in fixtures() {
        if let Some((_, json)) = fixture.message.to_wire().unwrap().split_once(':') {
            let payload: serde_json::Value = serde_json::from_str(json).unwrap();
            let fields = payload.as_object().unwrap();
            for field in ["protocol", "version", "language"] {
                assert!(!fields.contains_key(field), "{} has {field}", fixture.name);
            }
        }
    }
}

#[test]
fn unknown_namespace_errors_preserve_safe_boundaries_without_resynchronizing() {
    let closed = [
        "LOOM_FINDNG",
        "LOOM_FINDNG prose",
        "LOOM_FINDNG: {} trailing",
        "LOOM_FINDNG: {\n\"evidence\":\"LOOM_COMPLETE\"\n}",
    ];
    for unknown in closed {
        let wire = format!("{unknown}\nLOOM_COMPLETE");
        let failure = decode(&wire, Phase::Review).unwrap_err();
        assert!(
            matches!(&failure.diagnostics()[0].error, Error::UnknownMarker { marker } if marker == "LOOM_FINDNG")
        );
        assert_eq!(
            failure.context().terminal().unwrap().message,
            Message::Complete
        );
    }
    let ambiguous = "LOOM_FINDNG: {\"evidence\":\"unterminated\nLOOM_COMPLETE";
    let failure = decode(ambiguous, Phase::Review).unwrap_err();
    assert!(matches!(
        failure.diagnostics()[0].error,
        Error::UnknownMarker { .. }
    ));
    assert_eq!(
        &failure.context().raw()[failure.diagnostics()[0].span.bytes.clone()],
        ambiguous
    );
    assert!(failure.context().terminal().is_none());
}

#[test]
fn closing_line_suffix_cannot_supply_a_terminal_or_hide_an_error() {
    let suffixes = [
        ("prose", " prose"),
        ("object", " {}"),
        ("fence", "```"),
        ("marker", " LOOM_COMPLETE"),
    ];
    for (name, suffix) in suffixes {
        let wire = format!("{CLARIFY}{suffix}\nLOOM_COMPLETE");
        let failure = decode(&wire, Phase::Loop).unwrap_err();
        assert!(
            matches!(failure.diagnostics()[0].error, Error::ObjectSuffix),
            "{name}"
        );
        assert_eq!(failure.context().messages().len(), 1, "{name}");
        assert_eq!(
            failure.context().terminal().unwrap().message,
            Message::Complete,
            "{name}"
        );
        assert_eq!(
            &wire[failure.diagnostics()[0].span.bytes.clone()],
            format!("{CLARIFY}{suffix}")
        );
    }
}

#[test]
fn terminal_errors_keep_independently_decoded_records_and_terminals() {
    let misplaced = decode(&format!("LOOM_COMPLETE\n{CLARIFY}"), Phase::Loop).unwrap_err();
    assert_eq!(misplaced.context().messages().len(), 2);
    assert_eq!(
        misplaced.context().terminal().unwrap().message,
        Message::Complete
    );
    assert!(
        misplaced
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic.error, Error::RecordAfterTerminal))
    );
    assert!(
        misplaced
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic.error, Error::TrailingText))
    );

    let duplicate = decode("LOOM_COMPLETE\nLOOM_NOOP", Phase::Loop).unwrap_err();
    assert_eq!(duplicate.context().messages().len(), 2);
    assert!(duplicate.context().terminal().is_none());
    assert!(
        duplicate
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic.error, Error::DuplicateTerminal))
    );

    let missing = decode(CLARIFY, Phase::Loop).unwrap_err();
    assert_eq!(missing.context().messages().len(), 1);
    assert!(missing.context().terminal().is_none());
    assert!(matches!(
        missing.diagnostics()[0].error,
        Error::MissingTerminal
    ));
    assert_eq!(
        missing.diagnostics()[0].span.bytes,
        CLARIFY.len()..CLARIFY.len()
    );
}

mod properties {
    use super::*;
    use loom_test_support::proptest_config;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(proptest_config())]

        #[test]
        fn bounded_output_preserves_raw_text_and_valid_source_spans(
            text in prop_oneof![
                ".{0,512}",
                ".{0,256}".prop_map(|tail| format!("LOOM_RETRY: {{\"reason\":{tail}")),
                ".{0,256}".prop_map(|tail| format!("LOOM_COMPLETE\n{tail}")),
                ".{0,256}".prop_map(|tail| format!("LOOM_CLARIFY: {tail}\nLOOM_COMPLETE")),
            ], phase_index in 0usize..5,
        ) {
            let phase = [Phase::Plan, Phase::Todo, Phase::Loop, Phase::Review, Phase::Inbox][phase_index];
            let result = decode(&text, phase);
            let context = match &result {
                Ok(session) => session.context(),
                Err(failure) => {
                    prop_assert!(!failure.diagnostics().is_empty());
                    for diagnostic in failure.diagnostics() {
                        prop_assert!(text.get(diagnostic.span.bytes.clone()).is_some());
                        prop_assert_eq!(diagnostic.span.line, text[..diagnostic.span.bytes.start].bytes().filter(|byte| *byte == b'\n').count() + 1);
                    }
                    failure.context()
                }
            };
            prop_assert_eq!(context.raw(), &text);
            for located in context.messages() {
                prop_assert!(text.get(located.span.bytes.clone()).is_some());
                prop_assert_eq!(located.span.line, text[..located.span.bytes.start].bytes().filter(|byte| *byte == b'\n').count() + 1);
            }
            if let Ok(session) = result {
                prop_assert_eq!(session.terminal().message.role(), Role::Terminal);
                prop_assert!(text[session.terminal().span.bytes.end..].trim().is_empty());
                prop_assert!(session.context().messages().iter().all(|located| phase.admits(&located.message)));
            }
        }
    }
}

#[test]
fn cargo_metadata_coordinates_workspace_version_and_dependents() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--no-deps", "--locked", "--format-version", "1"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = metadata["packages"].as_array().unwrap();
    let protocol = packages
        .iter()
        .find(|package| package["name"] == "loom-protocol")
        .unwrap();
    assert_eq!(protocol["version"], "0.1.0");
    for package in packages {
        assert_eq!(package["version"], "0.1.0");
        for dependency in package["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|dependency| dependency["name"] == "loom-protocol")
        {
            assert_eq!(dependency["req"], "^0.1.0", "{}", package["name"]);
        }
    }
}
