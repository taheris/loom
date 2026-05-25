//! Pi-mono RPC line parser.
//!
//! Two-phase JSONL deserialization (envelope peek → typed re-parse),
//! event mapping per the spec table, command encoding for stdin
//! (`prompt`/`steer`/`abort`), and the `extension_ui_request` auto-cancel
//! reply that protects loom from a stalled extension.

use loom_driver::agent::{CompactionReason, LineParse, ParsedLine, ProtocolError};
use loom_events::ParsedAgentEvent;
use serde::Serialize;
use tracing::{debug, trace, warn};

use super::messages::{
    AbortCommand, AssistantMessageDelta, ExtensionUiResponse, PiEnvelope, PiEvent, PiResponse,
    PiUiRequest, PromptCommand, SteerCommand,
};

/// Pi-mono RPC line parser.
///
/// Stateless dispatch layer between
/// [`AgentSession`](loom_driver::agent::AgentSession) and
/// [`messages`](super::messages). The parser owns JSONL framing on
/// stdout (line in → [`ParsedLine`]) and command encoding for stdin
/// (`encode_prompt`/`encode_steer`/`encode_abort`).
/// Pi-mono line parser. Holds a per-session `Task` stack so nested
/// tool calls (inside a `Task` subagent) carry the parent's id. The
/// stack uses interior mutability (`Mutex`) because
/// [`LineParse::parse_line`] is `&self`; one parser instance per agent
/// session, so contention is non-existent.
pub struct PiParser {
    task_stack: std::sync::Mutex<Vec<loom_events::identifier::ToolCallId>>,
}

impl PiParser {
    pub fn new() -> Self {
        Self {
            task_stack: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn current_parent(&self) -> Result<Option<loom_events::identifier::ToolCallId>, ProtocolError> {
        let stack = self
            .task_stack
            .lock()
            .map_err(|_| ProtocolError::LockPoisoned)?;
        Ok(stack.last().cloned())
    }

    fn push_task(&self, id: loom_events::identifier::ToolCallId) -> Result<(), ProtocolError> {
        let mut stack = self
            .task_stack
            .lock()
            .map_err(|_| ProtocolError::LockPoisoned)?;
        stack.push(id);
        Ok(())
    }

    fn pop_task_if_matches(
        &self,
        id: &loom_events::identifier::ToolCallId,
    ) -> Result<(), ProtocolError> {
        let mut stack = self
            .task_stack
            .lock()
            .map_err(|_| ProtocolError::LockPoisoned)?;
        if stack.last() == Some(id) {
            stack.pop();
        }
        Ok(())
    }
}

impl Default for PiParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Empty `ParsedLine` — no events, no response.
fn empty() -> ParsedLine {
    ParsedLine {
        events: Vec::new(),
        response: None,
    }
}

/// Map a pi `compaction_start.reason` string to the neutral
/// [`CompactionReason`]. `"threshold"`/`"overflow"` → `ContextLimit`
/// (both signal context pressure); `"manual"` → `UserRequested`;
/// anything else → `Unknown`.
fn map_compaction_reason(reason: Option<&str>) -> CompactionReason {
    match reason {
        Some("threshold") | Some("overflow") => CompactionReason::ContextLimit,
        Some("manual") => CompactionReason::UserRequested,
        _ => CompactionReason::Unknown,
    }
}

/// True when a pi extension UI method requires a host response. If loom
/// does not reply, the extension's pending promise hangs and the agent
/// stalls — the parser auto-cancels these.
fn ui_method_requires_response(method: &str) -> bool {
    matches!(method, "select" | "confirm" | "input" | "editor")
}

fn encode_command<T: Serialize>(payload: &T) -> Result<String, ProtocolError> {
    let mut line = serde_json::to_string(payload)?;
    line.push('\n');
    Ok(line)
}

fn parse_event(parser: &PiParser, event: PiEvent) -> Result<ParsedLine, ProtocolError> {
    Ok(match event {
        PiEvent::MessageUpdate { delta } => match delta {
            AssistantMessageDelta::TextDelta { text } => ParsedLine {
                events: vec![ParsedAgentEvent::TextDelta { text }],
                response: None,
            },
            AssistantMessageDelta::TextEnd => ParsedLine {
                events: vec![ParsedAgentEvent::TextEnd],
                response: None,
            },
            AssistantMessageDelta::ThinkingDelta { text } => ParsedLine {
                events: vec![ParsedAgentEvent::ThinkingDelta { text }],
                response: None,
            },
            AssistantMessageDelta::ThinkingEnd => ParsedLine {
                events: vec![ParsedAgentEvent::ThinkingEnd],
                response: None,
            },
            AssistantMessageDelta::ToolcallDelta {
                tool_call_id,
                delta,
            } => ParsedLine {
                events: vec![ParsedAgentEvent::ToolcallDelta {
                    id: tool_call_id,
                    delta,
                }],
                response: None,
            },
            AssistantMessageDelta::Error { reason, message } => {
                let message = message.or(reason).unwrap_or_default();
                ParsedLine {
                    events: vec![ParsedAgentEvent::Error { message }],
                    response: None,
                }
            }
            AssistantMessageDelta::Unknown => {
                trace!("unmapped assistantMessageEvent delta");
                empty()
            }
        },
        PiEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => {
            // Snapshot the current parent BEFORE pushing — a `Task`
            // tool_call is a child of whatever Task (if any) was open at
            // the time of its emission, not its own child.
            let parent = parser.current_parent()?;
            let event = ParsedAgentEvent::ToolCall {
                id: tool_call_id.clone(),
                tool: tool_name.clone(),
                params: args,
                parent_tool_call_id: parent,
            };
            if tool_name == "Task" {
                parser.push_task(tool_call_id)?;
            }
            ParsedLine {
                events: vec![event],
                response: None,
            }
        }
        PiEvent::ToolExecutionEnd {
            tool_call_id,
            result,
            is_error,
        } => {
            let output = match result {
                serde_json::Value::String(s) => s,
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            // Pop the Task stack BEFORE building the event — a Task's
            // own tool_result closes that subagent, and subsequent tool
            // calls are siblings of the Task, not children.
            parser.pop_task_if_matches(&tool_call_id)?;
            ParsedLine {
                events: vec![ParsedAgentEvent::ToolResult {
                    id: tool_call_id,
                    output,
                    is_error,
                }],
                response: None,
            }
        }
        PiEvent::TurnEnd => ParsedLine {
            events: vec![ParsedAgentEvent::TurnEnd],
            response: None,
        },
        PiEvent::AgentEnd => ParsedLine {
            events: vec![ParsedAgentEvent::SessionComplete {
                exit_code: 0,
                cost_usd: None,
            }],
            response: None,
        },
        PiEvent::CompactionStart { reason } => ParsedLine {
            events: vec![ParsedAgentEvent::CompactionStart {
                reason: map_compaction_reason(reason.as_deref()),
            }],
            response: None,
        },
        PiEvent::CompactionEnd { aborted } => ParsedLine {
            events: vec![ParsedAgentEvent::CompactionEnd { aborted }],
            response: None,
        },
        PiEvent::ToolExecutionUpdate {
            tool_call_id,
            partial_result,
        } => {
            let text = match partial_result {
                serde_json::Value::String(s) => s,
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            ParsedLine {
                events: vec![ParsedAgentEvent::ToolProgress {
                    id: tool_call_id,
                    text,
                }],
                response: None,
            }
        }
        PiEvent::AutoRetryStart {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => ParsedLine {
            events: vec![ParsedAgentEvent::AutoRetry {
                attempt,
                max_attempts,
                delay_ms,
                error_message,
            }],
            response: None,
        },
        PiEvent::TurnStart | PiEvent::AgentStart | PiEvent::QueueUpdate => {
            trace!("pi event ignored");
            empty()
        }
        PiEvent::AutoRetryEnd | PiEvent::ExtensionError => {
            debug!("pi event ignored");
            empty()
        }
        PiEvent::Unknown => {
            trace!("unknown pi event type");
            empty()
        }
    })
}

fn parse_ui_request(req: PiUiRequest) -> Result<ParsedLine, ProtocolError> {
    if !ui_method_requires_response(&req.method) {
        debug!(method = %req.method, "extension_ui_request ignored");
        return Ok(empty());
    }
    let payload = ExtensionUiResponse {
        kind: "extension_ui_response",
        id: &req.id,
        cancelled: true,
    };
    let response = encode_command(&payload)?;
    debug!(method = %req.method, "extension_ui_request auto-cancelled");
    Ok(ParsedLine {
        events: Vec::new(),
        response: Some(response),
    })
}

impl LineParse for PiParser {
    fn parse_line(&self, line: &str) -> Result<ParsedLine, ProtocolError> {
        let env: PiEnvelope = match serde_json::from_str(line) {
            Ok(env) => env,
            Err(err) => {
                warn!(error = %err, "pi line failed JSON envelope parse");
                return Err(ProtocolError::InvalidJson(err));
            }
        };
        match env.msg_type.as_deref() {
            Some("response") => {
                let resp: PiResponse = serde_json::from_str(line)?;
                if resp.success {
                    debug!(id = %resp.id, command = %resp.command, "pi response ok");
                } else {
                    debug!(
                        id = %resp.id,
                        command = %resp.command,
                        error = ?resp.error,
                        "pi response failed",
                    );
                }
                Ok(empty())
            }
            Some("extension_ui_request") => {
                let req: PiUiRequest = serde_json::from_str(line)?;
                parse_ui_request(req)
            }
            _ if env.id.is_none() => {
                let evt: PiEvent = serde_json::from_str(line)?;
                parse_event(self, evt)
            }
            other => Err(ProtocolError::UnknownMessageType(
                other.unwrap_or("").to_string(),
            )),
        }
    }

    fn encode_prompt(&self, msg: &str) -> Result<String, ProtocolError> {
        encode_command(&PromptCommand {
            kind: "prompt",
            message: msg,
        })
    }

    fn encode_steer(&self, msg: &str) -> Result<String, ProtocolError> {
        encode_command(&SteerCommand {
            kind: "steer",
            message: msg,
        })
    }

    fn encode_abort(&self) -> Result<Option<String>, ProtocolError> {
        Ok(Some(encode_command(&AbortCommand { kind: "abort" })?))
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;
    use loom_driver::agent::{CompactionReason, ProtocolError};
    use loom_events::ParsedAgentEvent;

    fn parse(line: &str) -> ParsedLine {
        PiParser::new()
            .parse_line(line)
            .expect("fixture line should parse cleanly")
    }

    /// G4 — when a `Task` tool_call is open, subsequent tool_calls
    /// carry `parent_tool_call_id = Some(<task-id>)`. The matching
    /// tool_result closes the stack.
    #[test]
    fn task_subagent_nesting_threads_parent_tool_call_id() {
        let parser = PiParser::new();

        // Open a Task — its own tool_call has no parent.
        let task_start =
            r#"{"type":"tool_execution_start","toolCallId":"tc-task","toolName":"Task","args":{}}"#;
        let p = parser.parse_line(task_start).expect("task start");
        match &p.events[0] {
            ParsedAgentEvent::ToolCall {
                tool,
                parent_tool_call_id,
                ..
            } => {
                assert_eq!(tool, "Task");
                assert!(parent_tool_call_id.is_none());
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }

        // Tool call inside the Task — parent is the Task's id.
        let nested = r#"{"type":"tool_execution_start","toolCallId":"tc-read","toolName":"Read","args":{"file_path":"a"}}"#;
        let p = parser.parse_line(nested).expect("nested");
        match &p.events[0] {
            ParsedAgentEvent::ToolCall {
                tool,
                parent_tool_call_id,
                ..
            } => {
                assert_eq!(tool, "Read");
                assert_eq!(
                    parent_tool_call_id.as_ref().map(|id| id.as_str()),
                    Some("tc-task"),
                );
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }

        // Close the Task — its matching tool_result pops the stack.
        let task_end = r#"{"type":"tool_execution_end","toolCallId":"tc-task","toolName":"Task","result":"ok","isError":false}"#;
        parser.parse_line(task_end).expect("task end");

        // Sibling tool_call after Task closed — back to no parent.
        let sibling = r#"{"type":"tool_execution_start","toolCallId":"tc-bash","toolName":"Bash","args":{"command":"ls"}}"#;
        let p = parser.parse_line(sibling).expect("sibling");
        match &p.events[0] {
            ParsedAgentEvent::ToolCall {
                tool,
                parent_tool_call_id,
                ..
            } => {
                assert_eq!(tool, "Bash");
                assert!(
                    parent_tool_call_id.is_none(),
                    "stack should be empty post-Task"
                );
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    fn parse_err(line: &str) -> ProtocolError {
        match PiParser::new().parse_line(line) {
            Ok(_) => panic!("fixture line should fail to parse"),
            Err(e) => e,
        }
    }

    // -- test_pi_two_phase_deser ------------------------------------------

    #[test]
    fn envelope_only_with_unknown_extras_classifies_as_event() {
        // Bare event-shaped line with extra unknown fields — envelope peek
        // ignores them, then the second pass deserializes into PiEvent.
        let line = r#"{"type":"turn_start","novel":42,"extra":{"x":1}}"#;
        let p = parse(line);
        assert!(p.events.is_empty());
        assert!(p.response.is_none());
    }

    #[test]
    fn full_response_classifies_and_re_deserializes() {
        // type=response → second pass populates PiResponse.command/success.
        let line = r#"{"type":"response","id":"r1","command":"prompt","success":true}"#;
        let p = parse(line);
        assert!(p.events.is_empty());
        assert!(p.response.is_none());
    }

    #[test]
    fn full_event_classifies_via_id_absent_path() {
        // type=tool_execution_start has no id → envelope falls through to
        // the event branch and the second pass populates PiEvent.
        let line = r#"{"type":"tool_execution_start","toolCallId":"tc-1","toolName":"Read","args":{"path":"/a"}}"#;
        let p = parse(line);
        assert_eq!(p.events.len(), 1);
        match &p.events[0] {
            ParsedAgentEvent::ToolCall {
                id, tool, params, ..
            } => {
                assert_eq!(id.as_str(), "tc-1");
                assert_eq!(tool, "Read");
                assert_eq!(params["path"], "/a");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn full_ui_request_classifies_as_extension_ui_request() {
        // type=extension_ui_request → second pass populates PiUiRequest;
        // method=select drives the auto-cancel response branch.
        let line =
            r#"{"type":"extension_ui_request","id":"u1","method":"select","payload":{"opt":1}}"#;
        let p = parse(line);
        assert!(p.events.is_empty());
        assert!(p.response.is_some());
    }

    #[test]
    fn unknown_envelope_type_with_id_is_unknown_message_type() {
        // type is set, id is set, but type is not recognised — envelope
        // dispatch returns UnknownMessageType rather than treating it as
        // an event (events have no id).
        let line = r#"{"type":"mystery","id":"x","extra":1}"#;
        let err = parse_err(line);
        match err {
            ProtocolError::UnknownMessageType(t) => assert_eq!(t, "mystery"),
            other => panic!("expected UnknownMessageType, got {other:?}"),
        }
    }

    // -- test_pi_event_mapping --------------------------------------------

    #[test]
    fn message_update_text_delta_yields_message_delta() {
        let line = r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","text":"hello"}}"#;
        let p = parse(line);
        assert_eq!(p.events.len(), 1);
        match &p.events[0] {
            ParsedAgentEvent::TextDelta { text, .. } => assert_eq!(text, "hello"),
            other => panic!("expected MessageDelta, got {other:?}"),
        }
    }

    #[test]
    fn message_update_error_delta_yields_error_event() {
        let line = r#"{"type":"message_update","assistantMessageEvent":{"type":"error","reason":"aborted","message":"user aborted"}}"#;
        let p = parse(line);
        assert_eq!(p.events.len(), 1);
        match &p.events[0] {
            ParsedAgentEvent::Error { message, .. } => assert_eq!(message, "user aborted"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn message_update_thinking_delta_yields_thinking_delta_event() {
        // thinking_delta maps to ParsedAgentEvent::ThinkingDelta.
        let line = r#"{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","text":"…"}}"#;
        let p = parse(line);
        assert_eq!(p.events.len(), 1);
        match &p.events[0] {
            ParsedAgentEvent::ThinkingDelta { text, .. } => assert_eq!(text, "…"),
            other => panic!("expected ThinkingDelta, got {other:?}"),
        }
    }

    #[test]
    fn message_update_text_end_yields_text_end_event() {
        // text_end is the paired terminator for text_delta.
        let line = r#"{"type":"message_update","assistantMessageEvent":{"type":"text_end"}}"#;
        let p = parse(line);
        assert!(matches!(p.events[..], [ParsedAgentEvent::TextEnd]));
    }

    #[test]
    fn message_update_thinking_end_yields_thinking_end_event() {
        // thinking_end pairs with thinking_delta.
        let line = r#"{"type":"message_update","assistantMessageEvent":{"type":"thinking_end"}}"#;
        let p = parse(line);
        assert!(matches!(p.events[..], [ParsedAgentEvent::ThinkingEnd]));
    }

    #[test]
    fn message_update_toolcall_delta_yields_toolcall_delta_event() {
        // toolcall_delta surfaces with the tool call id + raw chunk.
        let line = r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_delta","toolCallId":"tc-9","delta":"{\"file_path\":\"a\"}"}}"#;
        let p = parse(line);
        assert_eq!(p.events.len(), 1);
        match &p.events[0] {
            ParsedAgentEvent::ToolcallDelta { id, delta, .. } => {
                assert_eq!(id.as_str(), "tc-9");
                assert_eq!(delta, r#"{"file_path":"a"}"#);
            }
            other => panic!("expected ToolcallDelta, got {other:?}"),
        }
    }

    #[test]
    fn message_update_genuinely_unknown_delta_is_silent() {
        // Forward-compat: a brand-new delta type does not fail the parse.
        let line = r#"{"type":"message_update","assistantMessageEvent":{"type":"mystery_delta","field":1}}"#;
        let p = parse(line);
        assert!(p.events.is_empty());
    }

    #[test]
    fn tool_execution_end_yields_tool_result() {
        let line = r#"{"type":"tool_execution_end","toolCallId":"tc-2","toolName":"Read","result":"ok","isError":false}"#;
        let p = parse(line);
        assert_eq!(p.events.len(), 1);
        match &p.events[0] {
            ParsedAgentEvent::ToolResult {
                id,
                output,
                is_error,
                ..
            } => {
                assert_eq!(id.as_str(), "tc-2");
                assert_eq!(output, "ok");
                assert!(!is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn tool_execution_end_stringifies_non_string_result() {
        let line = r#"{"type":"tool_execution_end","toolCallId":"tc-3","toolName":"Read","result":{"x":1},"isError":true}"#;
        let p = parse(line);
        match &p.events[0] {
            ParsedAgentEvent::ToolResult {
                output, is_error, ..
            } => {
                assert!(output.contains("\"x\""));
                assert!(*is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn turn_end_yields_turn_end_event() {
        let line = r#"{"type":"turn_end","message":{"x":1},"toolResults":[]}"#;
        let p = parse(line);
        assert!(matches!(p.events[..], [ParsedAgentEvent::TurnEnd]));
    }

    #[test]
    fn agent_end_yields_session_complete_with_synthesized_zero() {
        let line = r#"{"type":"agent_end","messages":[]}"#;
        let p = parse(line);
        assert_eq!(p.events.len(), 1);
        match &p.events[0] {
            ParsedAgentEvent::SessionComplete {
                exit_code,
                cost_usd,
                ..
            } => {
                assert_eq!(*exit_code, 0);
                assert!(cost_usd.is_none());
            }
            other => panic!("expected SessionComplete, got {other:?}"),
        }
    }

    #[test]
    fn compaction_start_threshold_maps_to_context_limit() {
        let line = r#"{"type":"compaction_start","reason":"threshold"}"#;
        let p = parse(line);
        assert!(matches!(
            p.events[..],
            [ParsedAgentEvent::CompactionStart {
                reason: CompactionReason::ContextLimit,
            }]
        ));
    }

    #[test]
    fn compaction_start_overflow_maps_to_context_limit() {
        let line = r#"{"type":"compaction_start","reason":"overflow"}"#;
        let p = parse(line);
        assert!(matches!(
            p.events[..],
            [ParsedAgentEvent::CompactionStart {
                reason: CompactionReason::ContextLimit,
            }]
        ));
    }

    #[test]
    fn compaction_start_manual_maps_to_user_requested() {
        let line = r#"{"type":"compaction_start","reason":"manual"}"#;
        let p = parse(line);
        assert!(matches!(
            p.events[..],
            [ParsedAgentEvent::CompactionStart {
                reason: CompactionReason::UserRequested,
            }]
        ));
    }

    #[test]
    fn compaction_start_unknown_reason_maps_to_unknown() {
        let line = r#"{"type":"compaction_start","reason":"other"}"#;
        let p = parse(line);
        assert!(matches!(
            p.events[..],
            [ParsedAgentEvent::CompactionStart {
                reason: CompactionReason::Unknown,
            }]
        ));
    }

    #[test]
    fn compaction_end_carries_aborted_flag() {
        let line =
            r#"{"type":"compaction_end","aborted":true,"reason":"manual","willRetry":false}"#;
        let p = parse(line);
        assert!(matches!(
            p.events[..],
            [ParsedAgentEvent::CompactionEnd { aborted: true }]
        ));
    }

    #[test]
    fn observability_only_events_yield_no_agent_events() {
        // tool_execution_update and auto_retry_start surface as
        // ParsedAgentEvent::ToolProgress and ParsedAgentEvent::AutoRetry
        // respectively. The pinned set here is the residual
        // observability-only PiEvents.
        for line in [
            r#"{"type":"turn_start"}"#,
            r#"{"type":"agent_start"}"#,
            r#"{"type":"queue_update","steering":[],"followUp":[]}"#,
            r#"{"type":"auto_retry_end","success":true,"attempt":1}"#,
            r#"{"type":"extension_error","extensionPath":"/x","event":"y","error":"z"}"#,
        ] {
            let p = parse(line);
            assert!(p.events.is_empty(), "expected no events for {line}");
            assert!(p.response.is_none(), "expected no response for {line}");
        }
    }

    #[test]
    fn pi_tool_execution_update_yields_tool_progress() {
        // Long-running tools emit `tool_execution_update` with a
        // partial result; surface as ParsedAgentEvent::ToolProgress so
        // the renderer can keep the user oriented.
        let line = r#"{"type":"tool_execution_update","toolCallId":"tc-7","partialResult":"50%"}"#;
        let p = parse(line);
        assert_eq!(p.events.len(), 1);
        match &p.events[0] {
            ParsedAgentEvent::ToolProgress { id, text, .. } => {
                assert_eq!(id.as_str(), "tc-7");
                assert_eq!(text, "50%");
            }
            other => panic!("expected ToolProgress, got {other:?}"),
        }
    }

    #[test]
    fn pi_auto_retry_start_yields_auto_retry_event() {
        // pi auto_retry telemetry surfaces so the renderer can show
        // transient-failure progress (attempt/N + delay + reason).
        let line = r#"{"type":"auto_retry_start","attempt":2,"maxAttempts":5,"delayMs":1500,"errorMessage":"transient"}"#;
        let p = parse(line);
        assert_eq!(p.events.len(), 1);
        match &p.events[0] {
            ParsedAgentEvent::AutoRetry {
                attempt,
                max_attempts,
                delay_ms,
                error_message,
                ..
            } => {
                assert_eq!(*attempt, 2);
                assert_eq!(*max_attempts, 5);
                assert_eq!(*delay_ms, 1500);
                assert_eq!(error_message, "transient");
            }
            other => panic!("expected AutoRetry, got {other:?}"),
        }
    }

    #[test]
    fn unknown_event_type_via_serde_other_yields_no_events() {
        // Forward-compatibility: a brand-new event name does not fail the
        // parse — the catch-all variant is hit and the parser logs at trace.
        let line = r#"{"type":"mystery_event","payload":1}"#;
        let p = parse(line);
        assert!(p.events.is_empty());
        assert!(p.response.is_none());
    }

    // -- test_pi_malformed_jsonl -----------------------------------------

    #[test]
    fn malformed_json_returns_invalid_json_error() {
        let err = parse_err("not-json");
        assert!(matches!(err, ProtocolError::InvalidJson(_)));
    }

    // -- test_pi_extension_ui_passthrough ---------------------------------

    #[test]
    fn extension_ui_select_yields_auto_cancel_response() {
        let line = r#"{"type":"extension_ui_request","id":"u-42","method":"select","payload":{}}"#;
        let p = parse(line);
        assert!(p.events.is_empty());
        let resp = p.response.expect("auto-cancel response present");
        assert!(resp.contains(r#""type":"extension_ui_response""#));
        assert!(resp.contains(r#""id":"u-42""#));
        assert!(resp.contains(r#""cancelled":true"#));
        assert!(resp.ends_with('\n'));
    }

    #[test]
    fn extension_ui_confirm_yields_auto_cancel_response() {
        let line = r#"{"type":"extension_ui_request","id":"u-1","method":"confirm","payload":{}}"#;
        let p = parse(line);
        assert!(p.response.is_some());
    }

    #[test]
    fn extension_ui_input_yields_auto_cancel_response() {
        let line = r#"{"type":"extension_ui_request","id":"u-2","method":"input","payload":{}}"#;
        let p = parse(line);
        assert!(p.response.is_some());
    }

    #[test]
    fn extension_ui_editor_yields_auto_cancel_response() {
        let line = r#"{"type":"extension_ui_request","id":"u-3","method":"editor","payload":{}}"#;
        let p = parse(line);
        assert!(p.response.is_some());
    }

    #[test]
    fn extension_ui_notify_leaves_response_none() {
        // notify-style methods do not block the agent — no auto-cancel.
        let line = r#"{"type":"extension_ui_request","id":"u-9","method":"notify","payload":{}}"#;
        let p = parse(line);
        assert!(p.events.is_empty());
        assert!(p.response.is_none());
    }

    #[test]
    fn extension_ui_set_status_leaves_response_none() {
        let line =
            r#"{"type":"extension_ui_request","id":"u-10","method":"setStatus","payload":{}}"#;
        let p = parse(line);
        assert!(p.response.is_none());
    }

    // -- encoder shape ----------------------------------------------------

    #[test]
    fn encode_prompt_emits_prompt_command() {
        let parser = PiParser::new();
        let line = parser
            .encode_prompt("hello")
            .expect("encoder should succeed");
        assert!(line.ends_with('\n'));
        let v: serde_json::Value =
            serde_json::from_str(line.trim_end()).expect("encoded prompt is valid JSON");
        assert_eq!(v["type"], "prompt");
        assert_eq!(v["message"], "hello");
    }

    #[test]
    fn encode_steer_emits_steer_command() {
        let parser = PiParser::new();
        let line = parser.encode_steer("hi").expect("encoder should succeed");
        let v: serde_json::Value =
            serde_json::from_str(line.trim_end()).expect("encoded steer is valid JSON");
        assert_eq!(v["type"], "steer");
        assert_eq!(v["message"], "hi");
    }

    #[test]
    fn encode_abort_emits_abort_command_some() {
        let parser = PiParser::new();
        let result = parser.encode_abort().expect("encoder should succeed");
        let line = result.expect("abort command present");
        let v: serde_json::Value =
            serde_json::from_str(line.trim_end()).expect("encoded abort is valid JSON");
        assert_eq!(v["type"], "abort");
    }
}
