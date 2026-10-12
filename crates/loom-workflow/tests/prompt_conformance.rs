use std::collections::BTreeSet;

use anyhow::{Context as _, Result, ensure};
use askama::Template;
use loom_protocol::{
    gate::{ConcernToken, DispatchScope, FindingTarget, WalkOutput},
    output::{self, Arity, Message, Phase, Role},
};
use loom_workflow::review::WorkspaceFindingValidator;

#[path = "prompt_conformance/extract.rs"]
mod extract;
#[path = "prompt_conformance/fixture.rs"]
mod fixture;

const PHASES: [Phase; 5] = [
    Phase::Plan,
    Phase::Todo,
    Phase::Loop,
    Phase::Review,
    Phase::Inbox,
];
const FINDINGS: &[(&str, &str, &str)] = &[
    ("spec-coherence-fail", "Criterion", "any-scope"),
    ("orphan-integration", "Contract", "any-scope"),
    ("style-rule-violation", "StyleRule", "any-scope"),
    ("verifier-bypass", "Annotation", "any-scope"),
    ("weak-assertion", "Annotation", "any-scope"),
    ("fabricated-result", "Annotation", "any-scope"),
    ("coincidental-pass", "Annotation", "any-scope"),
    ("mock-discipline", "TestPath", "any-scope"),
    ("verifier-too-narrow", "Criterion", "any-scope"),
    ("concurrency-untested", "LockSite", "any-scope"),
    ("judge-flag", "Criterion", "any-scope"),
    ("invariant-clash", "Invariant", "any-scope"),
    ("template-spec-drift", "Template", "tree-only"),
    ("cross-spec-clash", "Criterion", "tree-only"),
    ("spec-conventions-violation", "Criterion", "tree-only"),
    ("scope-creep", "Criterion", "per-bead-only"),
    ("scope-shortfall", "Criterion", "per-bead-only"),
];

fn expected_syntax(phase: Phase) -> Vec<Vec<String>> {
    let rows: &[(&str, &str)] = match phase {
        Phase::Plan => &[("LOOM_COMPLETE", "Terminal")],
        Phase::Todo => &[
            ("LOOM_CLARIFY: <JSON object>", "Record"),
            ("LOOM_WAITING", "Terminal"),
            ("LOOM_RETRY: <JSON object>", "Terminal"),
            ("LOOM_BLOCKED: <JSON object>", "Terminal"),
            ("LOOM_TODO: <JSON object>", "Terminal"),
        ],
        Phase::Loop => &[
            ("LOOM_CLARIFY: <JSON object>", "Record"),
            ("LOOM_COMPLETE", "Terminal"),
            ("LOOM_NOOP", "Terminal"),
            ("LOOM_WAITING", "Terminal"),
            ("LOOM_RETRY: <JSON object>", "Terminal"),
            ("LOOM_BLOCKED: <JSON object>", "Terminal"),
        ],
        Phase::Review => &[
            ("LOOM_FINDING: <JSON object>", "Record"),
            ("LOOM_COMPLETE", "Terminal"),
            ("LOOM_CONCERN: <JSON object>", "Terminal"),
            ("LOOM_RETRY: <JSON object>", "Terminal"),
            ("LOOM_BLOCKED: <JSON object>", "Terminal"),
        ],
        Phase::Inbox => &[
            ("LOOM_COMPLETE", "Terminal"),
            ("LOOM_APPLY: <JSON object>", "Terminal"),
        ],
    };
    rows.iter()
        .map(|(wire, role)| vec![(*wire).into(), (*role).into()])
        .collect()
}

fn expected_findings() -> Vec<Vec<String>> {
    FINDINGS
        .iter()
        .map(|(token, kind, scope)| vec![(*token).into(), (*kind).into(), (*scope).into()])
        .collect()
}

fn check_prompt(body: &str, phase: Phase, validator: &WorkspaceFindingValidator) -> Result<()> {
    ensure!(
        extract::table(body, &["Wire spelling", "Role"])? == expected_syntax(phase),
        "syntax drift"
    );
    if phase == Phase::Review {
        ensure!(
            extract::table(body, &["Token", "Target kind", "Scope"])? == expected_findings(),
            "finding metadata drift"
        );
    }
    let examples = extract::examples(body)?;
    let mut covered = BTreeSet::new();
    let mut finding_coverage = BTreeSet::new();
    for example in examples {
        let decoded =
            output::decode(&example.body, phase).context("rendered executable example")?;
        for located in decoded.context().messages() {
            covered.insert(located.message.marker());
            if let Message::Finding(raw) = &located.message {
                let (_, kind, scope_class) = FINDINGS
                    .iter()
                    .find(|(token, _, _)| *token == raw.token.as_wire())
                    .context("not a prompt-visible token")?;
                ensure!(
                    raw.target.kind().as_wire() == *kind,
                    "literal target mismatch"
                );
                for scope in &example.scopes {
                    let permitted = match *scope_class {
                        "any-scope" => true,
                        "tree-only" => *scope == DispatchScope::Tree,
                        "per-bead-only" => *scope == DispatchScope::PerBead,
                        _ => false,
                    };
                    ensure!(permitted, "literal scope mismatch");
                    finding_coverage.insert((raw.token.as_wire(), scope.label()));
                }
            }
        }
        if phase == Phase::Review {
            for scope in example.scopes {
                let walk = WalkOutput::from_stdout(&example.body, scope, validator);
                ensure!(
                    walk.bad_walk().is_none(),
                    "workspace resolution/pairing: {:?}",
                    walk.bad_walk()
                );
            }
        }
    }
    let expected: BTreeSet<_> = phase.syntax().map(|entry| entry.marker).collect();
    ensure!(
        covered == expected,
        "incomplete phase example coverage: {covered:?}"
    );
    if phase == Phase::Review {
        let mut expected = BTreeSet::new();
        for (token, _, class) in FINDINGS {
            let scopes: &[&str] = match *class {
                "tree-only" => &["tree"],
                "per-bead-only" => &["per-bead"],
                _ => &["per-bead", "tree", "push-gate"],
            };
            for scope in scopes {
                expected.insert((*token, *scope));
            }
        }
        ensure!(
            finding_coverage == expected,
            "incomplete finding/scope examples"
        );
    }
    Ok(())
}

#[test]
fn rendered_phase_message_examples_decode_under_shared_contract() -> Result<()> {
    let workspace = fixture::workspace()?;
    let validator = WorkspaceFindingValidator::new(workspace.path());
    for phase in PHASES {
        check_prompt(&fixture::render(phase)?, phase, &validator)?;
    }
    Ok(())
}

#[test]
fn rendered_finding_examples_resolve_prompt_visible_token_target_scope_subset() -> Result<()> {
    let workspace = fixture::workspace()?;
    let validator = WorkspaceFindingValidator::new(workspace.path());
    let body = fixture::render(Phase::Review)?;
    check_prompt(&body, Phase::Review, &validator)?;
    for example in extract::examples(&body)? {
        let decoded = output::decode(&example.body, Phase::Review)?;
        for located in decoded.context().messages() {
            if let Message::Finding(raw) = &located.message {
                for scope in [
                    DispatchScope::PerBead,
                    DispatchScope::Tree,
                    DispatchScope::PushGate,
                ] {
                    assert_eq!(
                        raw.clone().resolve(scope, &validator).is_ok(),
                        raw.token.scope_kind().admits(scope)
                            && !(raw.route == loom_protocol::gate::FindingRoute::Blocking
                                && scope == DispatchScope::PushGate)
                    );
                }
                let mut wrong = raw.clone();
                wrong.target = if raw.target.kind().as_wire() == "Criterion" {
                    FindingTarget::Contract {
                        id: "example-contract".into(),
                    }
                } else {
                    FindingTarget::Criterion {
                        spec: "example".parse()?,
                        anchor: "acceptance".into(),
                    }
                };
                assert!(wrong.resolve(example.scopes[0], &validator).is_err());
                if raw.token == ConcernToken::InvariantClash {
                    assert!(raw.evidence.contains('\n'));
                    assert!(raw.evidence.contains("LOOM_COMPLETE"));
                    assert!(
                        loom_protocol::gate::options::has_well_formed_block(&raw.evidence),
                        "escaped Options evidence survives real decode"
                    );
                }
            }
        }
    }
    for (from, to) in [
        ("\"anchor\":\"acceptance\"", "\"anchor\":\"invented\""),
        ("\"bonds\":[\"example\"]", "\"bonds\":[\"absent\"]"),
        ("\"section\":\"Invariant\"", "\"section\":\"Absent\""),
        ("\"tag\":\"single-writer\"", "\"tag\":\"invented\""),
        ("tests/example.rs", "tests/absent.rs"),
        ("crate::example::test_live", "crate::example::test_absent"),
        ("templates/example.md", "templates/absent.md"),
    ] {
        assert!(
            check_prompt(&body.replace(from, to), Phase::Review, &validator).is_err(),
            "unresolved fixture identity {from}"
        );
    }
    Ok(())
}

#[test]
fn protocol_metadata_controls_prompt_variant_coverage() -> Result<()> {
    for phase in PHASES {
        let canonical: Vec<Vec<String>> = phase
            .syntax()
            .map(|entry| {
                let wire = match entry.arity {
                    Arity::Unit => entry.marker.into(),
                    Arity::Data => format!("{}: <JSON object>", entry.marker),
                };
                vec![wire, format!("{:?}", entry.role)]
            })
            .collect();
        assert_eq!(canonical, expected_syntax(phase));
        assert_eq!(
            extract::table(&fixture::render(phase)?, &["Wire spelling", "Role"])?,
            canonical
        );
    }
    let canonical: Vec<Vec<String>> = ConcernToken::prompt_visible()
        .iter()
        .map(|token| {
            vec![
                token.as_wire().into(),
                token.expected_target_kind().as_wire().into(),
                token.scope_kind().label().into(),
            ]
        })
        .collect();
    assert_eq!(canonical, expected_findings());
    Ok(())
}

#[test]
fn prompt_parser_conformance_keeps_independent_negative_wire_fixtures() -> Result<()> {
    let positive = [
        (Phase::Plan, "LOOM_COMPLETE", "Complete", Role::Terminal),
        (Phase::Loop, "LOOM_NOOP", "Noop", Role::Terminal),
        (Phase::Todo, "LOOM_WAITING", "Waiting", Role::Terminal),
        (
            Phase::Review,
            "LOOM_CONCERN: {\"summary\":\"gap\"}",
            "Concern",
            Role::Terminal,
        ),
        (
            Phase::Loop,
            "LOOM_RETRY: {\"reason\":\"transient IO\"}",
            "Retry",
            Role::Terminal,
        ),
        (
            Phase::Review,
            "LOOM_BLOCKED: {\"reason\":\"lost intent makes all options speculative\"}",
            "Blocked",
            Role::Terminal,
        ),
        (
            Phase::Inbox,
            "LOOM_APPLY: {\"proposals\":[\"lm-tune.1\"]}",
            "Apply",
            Role::Terminal,
        ),
        (
            Phase::Todo,
            "LOOM_TODO: {\"head\":\"0123456789abcdef0123456789abcdef01234567\",\"fingerprint\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"work_epic\":\"lm-work\",\"title\":\"Audit contract\",\"specs\":[{\"label\":\"example\",\"outcome\":\"no-work\",\"reason\":\"wording-only change\"}]}",
            "Todo",
            Role::Terminal,
        ),
        (
            Phase::Loop,
            "LOOM_CLARIFY: {\"decisions\":[\"lm-decision.1\"]}\nLOOM_WAITING",
            "Clarify",
            Role::Record,
        ),
        (
            Phase::Review,
            "LOOM_FINDING: {\"token\":\"judge-flag\",\"route\":\"deferred\",\"bonds\":[\"example\"],\"target\":{\"kind\":\"Criterion\",\"spec\":\"example\",\"anchor\":\"acceptance\"},\"evidence\":\"quote \\\"escaped\\\"\\nLOOM_COMPLETE is evidence\"}\nLOOM_CONCERN: {\"summary\":\"gap\"}",
            "Finding",
            Role::Record,
        ),
    ];
    for (phase, wire, expected, role) in positive {
        let session = output::decode(wire, phase)?;
        let message = &session.context().messages()[0].message;
        let variant = match message {
            Message::Complete => "Complete",
            Message::Noop => "Noop",
            Message::Waiting => "Waiting",
            Message::Concern(_) => "Concern",
            Message::Retry(_) => "Retry",
            Message::Blocked(_) => "Blocked",
            Message::Apply(_) => "Apply",
            Message::Todo(_) => "Todo",
            Message::Clarify(_) => "Clarify",
            Message::Finding(_) => "Finding",
        };
        assert_eq!(variant, expected);
        assert_eq!(message.role(), role);
        if let Some((marker, _)) = wire.split_once(':') {
            let missing_payload = if role == Role::Record {
                format!("{marker}\n{}", session.terminal().message.to_wire()?)
            } else {
                marker.to_owned()
            };
            let error = output::decode(&missing_payload, phase).expect_err("data arity");
            assert!(
                error
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| matches!(diagnostic.error, output::Error::ExpectedObject))
            );
        } else {
            let error = output::decode(&format!("{wire}: {{}}"), phase).expect_err("unit arity");
            assert!(
                error
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| matches!(diagnostic.error, output::Error::UnexpectedPayload))
            );
        }
    }
    for wire in [
        "LOOM_COMPLETE: {}",
        "LOOM_NOOP: {}",
        "LOOM_WAITING: {}",
        "a prior line reason\nLOOM_RETRY",
        "a prior question\nLOOM_CLARIFY",
        "LOOM_RETRY: {}",
        "LOOM_RETRY: {\"reason\":\" \"}",
        "LOOM_BLOCKED: {\"reason\":\"\"}",
        "LOOM_CLARIFY: {\"decisions\":[]}",
        "LOOM_CLARIFY: {\"decisions\":[\"not an id\"]}\nLOOM_WAITING",
        "LOOM_CLARIFY: {\"decisions\":[\"lm-decision.1\"]}",
        "LOOM_RETRY: {\"reason\":\"raw\nnewline\"}",
        "LOOM_RETRY: {\"reason\":\"raw\tcontrol\"}",
        "LOOM_RETRY: {\"reason\":\"io\"} suffix",
        "LOOM_RETRY: {\"reason\":\"io\"}\ncommentary",
        "LOOM_COMPLETE\nLOOM_CLARIFY: {\"decisions\":[\"lm-decision.1\"]}",
        "LOOM_COMPLETE\nLOOM_NOOP",
        "LOOM_FINISH\nLOOM_COMPLETE",
        "LOOM_CONCERN: {\"summary\":\"gap\"}",
        "LOOM_APPLY: {\"proposals\":[\"lm-tune.1\"]}",
    ] {
        assert!(
            output::decode(wire, Phase::Loop).is_err(),
            "literal negative: {wire}"
        );
    }
    for (phase, wire) in [
        (Phase::Review, "LOOM_FINDING: {}\nLOOM_COMPLETE"),
        (Phase::Review, "LOOM_CONCERN: {\"summary\":\" \"}"),
        (Phase::Review, "reason in prose\nLOOM_BLOCKED"),
        (Phase::Inbox, "LOOM_APPLY: {\"proposals\":[]}"),
        (Phase::Inbox, "LOOM_APPLY: {\"proposals\":[\"not an id\"]}"),
        (Phase::Todo, "LOOM_TODO: {}"),
        (
            Phase::Todo,
            "LOOM_CLARIFY: {\"decisions\":[]}\nLOOM_WAITING",
        ),
    ] {
        assert!(
            output::decode(wire, phase).is_err(),
            "domain negative: {wire}"
        );
    }
    let workspace = fixture::workspace()?;
    let validator = WorkspaceFindingValidator::new(workspace.path());
    for phase in PHASES {
        check_prompt(&fixture::render(phase)?, phase, &validator)?;
    }
    for (phase, from, to) in [
        (Phase::Loop, "LOOM_COMPLETE", "LOOM_FINISH"),
        (
            Phase::Loop,
            "| `LOOM_WAITING` |",
            "| `LOOM_WAITING: <JSON object>` |",
        ),
        (Phase::Loop, "| Record |", "| Terminal |"),
        (Phase::Review, "| `Criterion` |", "| `Contract` |"),
        (Phase::Review, "| `tree-only` |", "| `any-scope` |"),
        (
            Phase::Review,
            "\"kind\":\"Criterion\"",
            "\"kind\":\"Contract\"",
        ),
        (Phase::Review, "loom-example tree", "loom-example any-scope"),
        (
            Phase::Loop,
            "```text loom-example\nLOOM_NOOP\n```",
            "```text\nLOOM_NOOP\n```",
        ),
    ] {
        let body = fixture::render(phase)?;
        let mutated = body.replace(from, to);
        assert_ne!(body, mutated, "mutation must hit rendered surface");
        assert!(
            check_prompt(&mutated, phase, &validator).is_err(),
            "conformance missed {from} -> {to}"
        );
    }
    Ok(())
}

#[test]
fn todo_prompt_proposes_bindings_for_driver_persistence() -> Result<()> {
    let context = fixture::todo_context()?;
    let body = context.render()?;
    for instruction in [
        "using only the injected criterion IDs",
        "never compute IDs",
        "or write acceptance metadata with `bd`",
        "Rust resolves proposals against preflight acceptance",
        "persists reference-only metadata",
        "Ordinary task creation, labels, descriptions, and dependency edges remain agent-owned",
    ] {
        assert!(
            body.contains(instruction),
            "missing binding ownership: {instruction}"
        );
    }
    let mut proposals = 0;
    for example in extract::examples(&body)? {
        let session = output::decode(&example.body, Phase::Todo)?;
        if let Message::Todo(success) = &session.terminal().message {
            assert_eq!(success.head, context.todo_head);
            assert_eq!(success.fingerprint, context.todo_fingerprint);
            assert_eq!(success.work_epic, context.work_epic);
            assert_eq!(success.specs.len(), context.changed_specs.len());
            for spec in success.specs.iter() {
                assert!(
                    context
                        .changed_specs
                        .iter()
                        .any(|changed| changed.label == spec.label)
                );
                if let loom_protocol::todo::TodoSpecOutcome::Decomposed { beads, assignments } =
                    &spec.outcome
                {
                    for assignment in assignments.iter() {
                        assert!(beads.contains(&assignment.task));
                        for reference in assignment.criteria.as_slice() {
                            assert!(
                                context
                                    .criterion_status
                                    .iter()
                                    .any(|row| &row.spec_label == reference.spec_label()
                                        && &row.criterion_id == reference.criterion_id())
                            );
                            proposals += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(
        proposals > 0,
        "real rendered Todo handoff proposes supplied references"
    );
    Ok(())
}

#[test]
fn todo_prompt_separates_decisions_waiting_and_implementation_handoff() -> Result<()> {
    for dossier in [false, true] {
        let mut context = fixture::todo_context()?;
        if dossier {
            context.evidence_delivery = loom_templates::todo::EvidenceDelivery::Dossier {
                manifest_path: "/scratch/manifest.md".into(),
            };
        }
        let body = context.render()?;
        for instruction in [
            "dedicated child decision brief and nonterminal Clarify reporting",
            "**not implementation tasks**",
            "They cannot satisfy `Decomposed.beads`",
            "Account for all owned candidates separately",
            "an unreported candidate prevents finalization",
            "Do not copy briefs onto the epic",
            "only implementation tasks block those tasks",
            "**decomposition itself**",
            "Waiting does not",
            "advance spec cursors",
        ] {
            assert!(
                body.contains(instruction),
                "missing decomposition boundary: {instruction}"
            );
        }
        assert!(!body.contains("structured clarify question to the work epic"));
        let mut independent = false;
        for example in extract::examples(&body)? {
            let session = output::decode(&example.body, Phase::Todo)?;
            if matches!(session.terminal().message, Message::Todo(_))
                && !session.context().decisions().is_empty()
            {
                independent = true;
            }
        }
        assert!(
            independent,
            "rendered Todo example reports a decision and independently finalizes"
        );
    }
    Ok(())
}

#[test]
fn loop_and_todo_render_attributed_waits_and_nonterminal_decisions() -> Result<()> {
    for phase in [Phase::Loop, Phase::Todo] {
        let body = fixture::render(phase)?;
        for instruction in [
            "status=blocked",
            "all owned persisted candidates",
            "including persisted candidates",
            "without `loom:clarify`, `loom:blocked`, or",
            "`loom:infra` queue labels",
            "producer/source and work-root provenance",
            "worker",
            "`bd dep add <affected-work> <decision>`",
            "Parentage alone is not a prerequisite",
            "the source's",
            "outcome remains independent",
            "Leave the source open",
            "at least one declared blocker is still active",
            "driver records an attributed wait",
            "selectively releases only its own",
            "never unrelated human blocking state",
        ] {
            assert!(
                body.to_lowercase().contains(&instruction.to_lowercase()),
                "missing wait/decision boundary: {instruction}"
            );
        }
        let mut waiting = 0;
        let mut independent = 0;
        let mut record = None;
        let mut expected_decisions = None;
        let mut terminals = Vec::new();
        for example in extract::examples(&body)? {
            let session = output::decode(&example.body, phase)?;
            terminals.push(session.terminal().message.clone());
            if !session.context().decisions().is_empty() {
                if record.is_none() {
                    record = Some(session.context().messages()[0].message.to_wire()?);
                    expected_decisions = Some(session.context().decisions());
                }
                if matches!(session.terminal().message, Message::Waiting) {
                    waiting += 1;
                } else {
                    independent += 1;
                }
            }
        }
        assert!(waiting > 0 && independent > 0);
        let record = record.context("rendered Clarify record")?;
        let expected = expected_decisions.context("reported decision set")?;
        for terminal in terminals {
            let combined = format!("{record}\n{record}\n{}", terminal.to_wire()?);
            let session = output::decode(&combined, phase)?;
            assert_eq!(session.context().decisions(), expected);
            assert_eq!(session.terminal().message, terminal);
        }
    }
    Ok(())
}

#[test]
fn review_prompt_uses_strict_shared_messages_and_domain_pairing() -> Result<()> {
    let workspace = fixture::workspace()?;
    let validator = WorkspaceFindingValidator::new(workspace.path());
    let body = fixture::render(Phase::Review)?;
    for instruction in [
        "inspection-only",
        "never invoke `bd create`, `bd update`, or `bd mol bond`",
        "Do not duplicate the evidence in adjacent prose",
        "newlines are **not repaired**",
        "No preceding prose supplies fields or questions",
        "one or more",
        "findings with Concern",
    ] {
        assert!(
            body.contains(instruction),
            "missing strict review boundary: {instruction}"
        );
    }
    let examples = extract::examples(&body)?;
    let finding = examples
        .iter()
        .find(|example| example.body.starts_with("LOOM_FINDING"))
        .expect("rendered findings");
    let walk = WalkOutput::from_stdout(&finding.body, DispatchScope::PerBead, &validator);
    assert!(walk.bad_walk().is_none());
    let raw_newlines = finding.body.replace("\\n\\n", "\n\n");
    assert_ne!(raw_newlines, finding.body);
    let malformed = WalkOutput::from_stdout(&raw_newlines, DispatchScope::PerBead, &validator);
    assert!(malformed.decoded().is_err());
    assert_eq!(malformed.context().raw(), raw_newlines);
    assert_eq!(malformed.findings().len(), walk.findings().len() - 1);
    assert!(malformed.bad_walk().is_some());
    let wrong = finding.body.replace(
        "LOOM_CONCERN: {\"summary\":\"The contract and its verification have gaps.\"}",
        "LOOM_COMPLETE",
    );
    assert!(matches!(
        WalkOutput::from_stdout(&wrong, DispatchScope::PerBead, &validator).bad_walk(),
        Some(loom_protocol::gate::BadWalk::FindingsWithoutConcern { .. })
    ));
    assert!(matches!(
        WalkOutput::from_stdout(
            "LOOM_CONCERN: {\"summary\":\"unreported\"}",
            DispatchScope::PerBead,
            &validator
        )
        .bad_walk(),
        Some(loom_protocol::gate::BadWalk::ConcernWithoutFindings { .. })
    ));
    Ok(())
}

#[test]
fn interactive_examples_keep_final_turn_and_phase_rights() -> Result<()> {
    for phase in [Phase::Plan, Phase::Inbox] {
        let body = fixture::render(phase)?;
        assert!(body.contains("final turn only"));
        assert!(body.contains("Do **NOT** append"));
        assert!(body.contains("markers to intermediate turns"));
        for example in extract::examples(&body)? {
            let session = output::decode(&example.body, phase)?;
            assert!(session.context().decisions().is_empty());
            assert!(matches!(
                session.terminal().message,
                Message::Complete | Message::Apply(_)
            ));
            if phase == Phase::Plan {
                assert!(matches!(session.terminal().message, Message::Complete));
            }
        }
        let turn =
            output::decode("Which trade-off matters most?", phase).expect_err("nonfinal turn");
        assert!(turn.is_conversation());
    }
    Ok(())
}

#[test]
fn executable_example_extraction_is_test_only_and_rejects_broken_annotations() -> Result<()> {
    let body = fixture::render(Phase::Review)?;
    let decoded = output::decode(&body, Phase::Review)
        .expect_err("production must not promote prompt fences");
    assert!(decoded.context().messages().is_empty());
    assert!(!extract::examples(&body)?.is_empty());
    for malformed in [
        "```text loom-example\n```",
        "```text loom-example guessed-scope\nLOOM_COMPLETE\n```",
        "```bash loom-example\nLOOM_COMPLETE\n```",
        "ordinary prose LOOM_COMPLETE",
    ] {
        assert!(extract::examples(malformed).is_err());
    }
    Ok(())
}
