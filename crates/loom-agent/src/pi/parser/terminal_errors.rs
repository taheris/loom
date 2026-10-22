//! Final assistant errors are authoritative even without streaming error deltas.

use super::{AgentEndMode, PiParser};
use loom_driver::agent::LineParse;
use loom_events::ParsedAgentEvent;

#[cfg(test)]
fn replay(parser: &PiParser, lines: &str) -> Vec<ParsedAgentEvent> {
    lines
        .lines()
        .flat_map(|line| {
            parser
                .parse_line(line)
                .expect("valid Pi wire fixture")
                .events
        })
        .collect()
}

fn errors(events: &[ParsedAgentEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            ParsedAgentEvent::Error { message } => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn pi_terminal_provider_error_preserves_diagnostic_and_fails_completion() {
    // Captured from real Pi against an owned HTTP fixture, with unrelated messages removed.
    let events = replay(
        &PiParser::new(),
        include_str!("terminal_provider_error.jsonl"),
    );
    assert_eq!(errors(&events).len(), 1);
    assert!(errors(&events)[0].contains("deliberate local diagnostic refusal"));
    assert!(matches!(
        events.as_slice(),
        [
            ParsedAgentEvent::Error { .. },
            ParsedAgentEvent::TurnEnd,
            ParsedAgentEvent::SessionComplete { exit_code: 1, .. }
        ]
    ));
}

#[test]
fn pi_turn_end_error_without_message_end_is_not_success() {
    let events = replay(
        &PiParser::new(),
        concat!(
            "{\"type\":\"turn_end\",\"message\":{\"role\":\"assistant\",\"stopReason\":\"error\",\"errorMessage\":\"provider refused\"}}\n",
            "{\"type\":\"agent_end\",\"messages\":[]}\n",
        ),
    );
    assert_eq!(errors(&events), ["provider refused"]);
    assert!(matches!(
        events.last(),
        Some(ParsedAgentEvent::SessionComplete { exit_code: 1, .. })
    ));
}

#[test]
fn pi_agent_end_error_without_prior_message_events_is_not_success() {
    let events = replay(
        &PiParser::new(),
        r#"{"type":"agent_end","messages":[{"role":"assistant","stopReason":"error","errorMessage":"final failure"}]}"#,
    );
    assert_eq!(errors(&events), ["final failure"]);
    assert!(matches!(
        events.last(),
        Some(ParsedAgentEvent::SessionComplete { exit_code: 1, .. })
    ));
}

#[test]
fn pi_terminal_abort_without_detail_still_fails_completion() {
    let events = replay(
        &PiParser::new(),
        concat!(
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"stopReason\":\"aborted\",\"errorMessage\":\"  \"}}\n",
            "{\"type\":\"agent_end\",\"messages\":[]}\n",
        ),
    );
    assert_eq!(
        errors(&events),
        ["Pi assistant terminated with stopReason=aborted"]
    );
    assert!(matches!(
        events.last(),
        Some(ParsedAgentEvent::SessionComplete { exit_code: 1, .. })
    ));
}

#[test]
fn pi_streamed_error_is_deduplicated_but_final_detail_is_not_lost() {
    let events = replay(
        &PiParser::new(),
        concat!(
            "{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"error\",\"reason\":\"error\"}}\n",
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"stopReason\":\"error\",\"errorMessage\":\"specific failure\"}}\n",
            "{\"type\":\"turn_end\",\"message\":{\"role\":\"assistant\",\"stopReason\":\"error\",\"errorMessage\":\"specific failure\"}}\n",
            "{\"type\":\"agent_end\",\"messages\":[]}\n",
        ),
    );
    assert_eq!(errors(&events), ["error", "specific failure"]);
    assert!(matches!(
        events.last(),
        Some(ParsedAgentEvent::SessionComplete { exit_code: 1, .. })
    ));
}

#[test]
fn pi_error_does_not_accept_partial_terminal_text_as_success() {
    let events = replay(
        &PiParser::new(),
        concat!(
            "{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\",\"delta\":\"LOOM_COMPLETE\"}}\n",
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"stopReason\":\"error\",\"errorMessage\":\"stream failed\"}}\n",
            "{\"type\":\"agent_end\",\"messages\":[]}\n",
        ),
    );
    assert_eq!(errors(&events), ["stream failed"]);
    assert!(
        matches!(events.first(), Some(ParsedAgentEvent::TextDelta { text }) if text == "LOOM_COMPLETE")
    );
    assert!(matches!(
        events.last(),
        Some(ParsedAgentEvent::SessionComplete { exit_code: 1, .. })
    ));
}

#[test]
fn pi_successful_retry_retains_diagnostics_without_failing_the_prompt() {
    for successful in [
        concat!(
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"stopReason\":\"stop\"}}\n",
            "{\"type\":\"agent_end\",\"messages\":[]}\n",
        ),
        r#"{"type":"agent_end","messages":[{"role":"assistant","stopReason":"stop"}]}"#,
    ] {
        let parser = PiParser::new();
        let mut events = replay(
            &parser,
            concat!(
                "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"stopReason\":\"error\",\"errorMessage\":\"transient failure\"}}\n",
                "{\"type\":\"auto_retry_start\",\"attempt\":1,\"maxAttempts\":3,\"delayMs\":0,\"errorMessage\":\"transient failure\"}\n",
            ),
        );
        events.extend(replay(&parser, successful));
        assert_eq!(errors(&events), ["transient failure"]);
        assert!(matches!(
            events.last(),
            Some(ParsedAgentEvent::SessionComplete { exit_code: 0, .. })
        ));
    }
}

#[test]
fn pi_bridge_error_keeps_cycle_boundary_and_next_prompt_independent() {
    let parser = PiParser::with_agent_end_mode(AgentEndMode::AgentEnd);
    let events = replay(&parser, include_str!("terminal_provider_error.jsonl"));
    assert_eq!(errors(&events).len(), 1);
    assert!(matches!(events.last(), Some(ParsedAgentEvent::AgentEnd)));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ParsedAgentEvent::SessionComplete { .. }))
    );
    let next = replay(
        &parser,
        concat!(
            "{\"type\":\"message_start\",\"message\":{\"role\":\"assistant\"}}\n",
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"stopReason\":\"stop\",\"content\":[{\"type\":\"text\",\"text\":\"ready\"}]}}\n",
            "{\"type\":\"agent_end\",\"messages\":[]}\n",
        ),
    );
    assert_eq!(errors(&next), Vec::<&str>::new());
    assert!(
        matches!(next.as_slice(), [ParsedAgentEvent::TextDelta { text }, ParsedAgentEvent::TextEnd, ParsedAgentEvent::AgentEnd] if text == "ready")
    );
}

#[test]
fn pi_nested_error_delta_preserves_the_provider_message() {
    let events = replay(
        &PiParser::new(),
        concat!(
            "{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"error\",\"reason\":\"error\",\"error\":{\"role\":\"assistant\",\"stopReason\":\"error\",\"errorMessage\":\"nested provider failure\"}}}\n",
            "{\"type\":\"agent_end\",\"messages\":[]}\n",
        ),
    );
    assert_eq!(errors(&events), ["nested provider failure"]);
    assert!(matches!(
        events.last(),
        Some(ParsedAgentEvent::SessionComplete { exit_code: 1, .. })
    ));
}

#[test]
fn pi_prompt_outcome_resets_after_the_cycle_boundary() {
    let parser = PiParser::new();
    let failed = replay(&parser, include_str!("terminal_provider_error.jsonl"));
    assert!(matches!(
        failed.last(),
        Some(ParsedAgentEvent::SessionComplete { exit_code: 1, .. })
    ));
    let next = replay(&parser, r#"{"type":"agent_end","messages":[]}"#);
    assert!(matches!(
        next.as_slice(),
        [ParsedAgentEvent::SessionComplete { exit_code: 0, .. }]
    ));
}

#[test]
fn pi_tool_and_user_errors_are_not_assistant_failures() {
    let events = replay(
        &PiParser::new(),
        concat!(
            "{\"type\":\"message_end\",\"message\":{\"role\":\"user\",\"stopReason\":\"error\",\"errorMessage\":\"quoted failure\"}}\n",
            "{\"type\":\"agent_end\",\"messages\":[{\"role\":\"toolResult\",\"stopReason\":\"error\",\"errorMessage\":\"tool failed\"}]}\n",
        ),
    );
    assert_eq!(errors(&events), Vec::<&str>::new());
    assert!(matches!(
        events.as_slice(),
        [ParsedAgentEvent::SessionComplete { exit_code: 0, .. }]
    ));
}
