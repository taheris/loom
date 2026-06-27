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
    AbortCommand, AssistantMessageDelta, ExtensionUiResponse, FollowUpCommand, PiEnvelope, PiEvent,
    PiResponse, PiUiRequest, PromptCommand, SteerCommand,
};

/// Pi-mono RPC line parser.
///
/// Owns JSONL framing, command encoding, per-session tool nesting, fallback
/// text capture, and Pi-private compaction retry policy state.
pub struct PiParser {
    task_stack: std::sync::Mutex<Vec<loom_events::identifier::ToolCallId>>,
    message_capture: std::sync::Mutex<MessageCapture>,
    compaction_policy: std::sync::Mutex<CompactionPolicy>,
}

#[derive(Debug, Default)]
struct MessageCapture {
    text_emitted: bool,
}

#[derive(Debug, Default)]
struct CompactionPolicy {
    active_reason: Option<NativeCompactionReason>,
    terminal_after_untrusted_retry: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeCompactionReason {
    Threshold,
    Overflow,
    Manual,
    Unknown,
}

impl NativeCompactionReason {
    fn from_wire(reason: Option<&str>) -> Self {
        match reason {
            Some("threshold") => Self::Threshold,
            Some("overflow") => Self::Overflow,
            Some("manual") => Self::Manual,
            _ => Self::Unknown,
        }
    }

    fn to_canonical(self) -> CompactionReason {
        match self {
            Self::Threshold | Self::Overflow => CompactionReason::ContextLimit,
            Self::Manual => CompactionReason::UserRequested,
            Self::Unknown => CompactionReason::Unknown,
        }
    }
}

impl PiParser {
    pub fn new() -> Self {
        Self {
            task_stack: std::sync::Mutex::new(Vec::new()),
            message_capture: std::sync::Mutex::new(MessageCapture::default()),
            compaction_policy: std::sync::Mutex::new(CompactionPolicy::default()),
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

    fn reset_message_capture(&self) -> Result<(), ProtocolError> {
        let mut capture = self
            .message_capture
            .lock()
            .map_err(|_| ProtocolError::LockPoisoned)?;
        capture.text_emitted = false;
        Ok(())
    }

    fn mark_text_emitted(&self) -> Result<(), ProtocolError> {
        let mut capture = self
            .message_capture
            .lock()
            .map_err(|_| ProtocolError::LockPoisoned)?;
        capture.text_emitted = true;
        Ok(())
    }

    fn fallback_text_events(
        &self,
        message: &serde_json::Value,
    ) -> Result<Vec<ParsedAgentEvent>, ProtocolError> {
        let text = assistant_message_text(message);
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let mut capture = self
            .message_capture
            .lock()
            .map_err(|_| ProtocolError::LockPoisoned)?;
        if capture.text_emitted {
            return Ok(Vec::new());
        }
        capture.text_emitted = true;
        Ok(vec![
            ParsedAgentEvent::TextDelta { text },
            ParsedAgentEvent::TextEnd,
        ])
    }

    fn observe_compaction_start(
        &self,
        reason: NativeCompactionReason,
    ) -> Result<(), ProtocolError> {
        let mut policy = self
            .compaction_policy
            .lock()
            .map_err(|_| ProtocolError::LockPoisoned)?;
        if !policy.terminal_after_untrusted_retry {
            policy.active_reason = Some(reason);
        }
        Ok(())
    }

    fn compaction_end_events(
        &self,
        aborted: bool,
        will_retry: bool,
        end_reason: NativeCompactionReason,
    ) -> Result<Vec<ParsedAgentEvent>, ProtocolError> {
        let mut policy = self
            .compaction_policy
            .lock()
            .map_err(|_| ProtocolError::LockPoisoned)?;
        let reason = match policy.active_reason.take() {
            Some(NativeCompactionReason::Unknown) | None => end_reason,
            Some(reason) => reason,
        };
        if reason == NativeCompactionReason::Overflow && !aborted && will_retry {
            policy.terminal_after_untrusted_retry = true;
            return Ok(vec![
                ParsedAgentEvent::CompactionEnd { aborted },
                ParsedAgentEvent::Error {
                    message: UNTRUSTED_OVERFLOW_RETRY_MESSAGE.to_string(),
                },
                ParsedAgentEvent::SessionComplete {
                    exit_code: 1,
                    cost_usd: None,
                },
            ]);
        }
        Ok(vec![ParsedAgentEvent::CompactionEnd { aborted }])
    }

    fn terminal_after_untrusted_retry(&self) -> Result<bool, ProtocolError> {
        let policy = self
            .compaction_policy
            .lock()
            .map_err(|_| ProtocolError::LockPoisoned)?;
        Ok(policy.terminal_after_untrusted_retry)
    }
}

impl Default for PiParser {
    fn default() -> Self {
        Self::new()
    }
}

const UNTRUSTED_OVERFLOW_RETRY_MESSAGE: &str = concat!(
    "pi overflow compaction requested auto-retry before the full re-pin was effective; ",
    "failing this session so the workflow can restart with the full prompt",
);

/// Empty `ParsedLine` — no events, no response.
fn empty() -> ParsedLine {
    ParsedLine {
        events: Vec::new(),
        response: None,
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

fn assistant_message_text(message: &serde_json::Value) -> String {
    if message.get("role").and_then(serde_json::Value::as_str) != Some("assistant") {
        return String::new();
    }
    content_text(message.get("content"))
}

fn content_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter_map(text_block_text)
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn text_block_text(block: &serde_json::Value) -> Option<&str> {
    if block.get("type").and_then(serde_json::Value::as_str) != Some("text") {
        return None;
    }
    block
        .get("text")
        .and_then(serde_json::Value::as_str)
        .or_else(|| block.get("content").and_then(serde_json::Value::as_str))
}

fn parse_event(parser: &PiParser, event: PiEvent) -> Result<ParsedLine, ProtocolError> {
    if parser.terminal_after_untrusted_retry()? {
        trace!("pi event ignored after unsafe overflow auto-retry fail-fast");
        return Ok(empty());
    }
    Ok(match event {
        PiEvent::MessageStart { .. } => {
            parser.reset_message_capture()?;
            empty()
        }
        PiEvent::MessageEnd { message } => ParsedLine {
            events: parser.fallback_text_events(&message)?,
            response: None,
        },
        PiEvent::MessageUpdate { delta } => match delta {
            AssistantMessageDelta::TextDelta { text } => {
                parser.mark_text_emitted()?;
                ParsedLine {
                    events: vec![ParsedAgentEvent::TextDelta { text }],
                    response: None,
                }
            }
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
            } => match tool_call_id {
                Some(tool_call_id) => ParsedLine {
                    events: vec![ParsedAgentEvent::ToolcallDelta {
                        id: tool_call_id,
                        delta,
                    }],
                    response: None,
                },
                None => {
                    trace!("assistant toolcall_delta without toolCallId ignored");
                    empty()
                }
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
        PiEvent::TurnEnd { message } => {
            let mut events = parser.fallback_text_events(&message)?;
            events.push(ParsedAgentEvent::TurnEnd);
            parser.reset_message_capture()?;
            ParsedLine {
                events,
                response: None,
            }
        }
        PiEvent::AgentEnd { .. } => ParsedLine {
            events: vec![ParsedAgentEvent::SessionComplete {
                exit_code: 0,
                cost_usd: None,
            }],
            response: None,
        },
        PiEvent::CompactionStart { reason } => {
            let native_reason = NativeCompactionReason::from_wire(reason.as_deref());
            parser.observe_compaction_start(native_reason)?;
            ParsedLine {
                events: vec![ParsedAgentEvent::CompactionStart {
                    reason: native_reason.to_canonical(),
                }],
                response: None,
            }
        }
        PiEvent::CompactionEnd {
            aborted,
            will_retry,
            reason,
        } => ParsedLine {
            events: parser.compaction_end_events(
                aborted,
                will_retry,
                NativeCompactionReason::from_wire(reason.as_deref()),
            )?,
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
                warn!(error = %err, preview = %line.escape_debug(), "pi line failed JSON envelope parse");
                return Err(ProtocolError::invalid_protocol_line(line, err));
            }
        };
        match env.msg_type.as_deref() {
            Some("response") => {
                let resp: PiResponse = serde_json::from_str(line)
                    .map_err(|err| ProtocolError::invalid_protocol_line(line, err))?;
                if resp.success {
                    debug!(id = ?resp.id, command = %resp.command, "pi response ok");
                } else {
                    debug!(
                        id = ?resp.id,
                        command = %resp.command,
                        error = ?resp.error,
                        "pi response failed",
                    );
                }
                Ok(empty())
            }
            Some("extension_ui_request") => {
                let req: PiUiRequest = serde_json::from_str(line)
                    .map_err(|err| ProtocolError::invalid_protocol_line(line, err))?;
                parse_ui_request(req)
            }
            _ if env.id.is_none() => {
                let evt: PiEvent = serde_json::from_str(line)
                    .map_err(|err| ProtocolError::invalid_protocol_line(line, err))?;
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

    fn encode_follow_up(&self, msg: &str) -> Result<String, ProtocolError> {
        encode_command(&FollowUpCommand {
            kind: "follow_up",
            message: msg,
        })
    }

    fn encode_abort(&self) -> Result<Option<String>, ProtocolError> {
        Ok(Some(encode_command(&AbortCommand { kind: "abort" })?))
    }
}

#[cfg(test)]
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
    fn idless_prompt_response_classifies_and_is_ignored() {
        // Current Pi emits command acknowledgements for prompt without `id`.
        // They are valid mid-session responses; only handshake correlation
        // requires ids.
        let line = r#"{"type":"response","command":"prompt","success":true}"#;
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
        let line = r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"hello"}}"#;
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
    fn message_end_without_streamed_text_yields_fallback_text() {
        let line = r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"LOOM_TODO: {}"}]}}"#;
        let p = parse(line);
        assert_eq!(p.events.len(), 2);
        match &p.events[0] {
            ParsedAgentEvent::TextDelta { text } => assert_eq!(text, "LOOM_TODO: {}"),
            other => panic!("expected fallback TextDelta, got {other:?}"),
        }
        assert!(matches!(p.events[1], ParsedAgentEvent::TextEnd));
    }

    #[test]
    fn message_end_after_streamed_text_does_not_duplicate_text() {
        let parser = PiParser::new();
        let start = r#"{"type":"message_start","message":{"role":"assistant","content":[]}}"#;
        assert!(
            parser
                .parse_line(start)
                .expect("start parses")
                .events
                .is_empty()
        );
        let delta = r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"hello"}}"#;
        assert!(matches!(
            parser.parse_line(delta).expect("delta parses").events[..],
            [ParsedAgentEvent::TextDelta { .. }]
        ));
        let end = r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#;
        assert!(
            parser
                .parse_line(end)
                .expect("end parses")
                .events
                .is_empty()
        );
    }

    #[test]
    fn message_update_thinking_delta_yields_thinking_delta_event() {
        // thinking_delta maps to ParsedAgentEvent::ThinkingDelta.
        let line = r#"{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","delta":"…"}}"#;
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
        // Legacy/idful toolcall_delta surfaces with the tool call id + raw chunk.
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
    fn message_update_idless_toolcall_delta_is_silent() {
        // Current Pi streams assistant-message tool-call argument chunks by
        // contentIndex before emitting the executable tool_execution_start
        // event. Without a toolCallId there is no stable AgentEvent id, so
        // the parser accepts and ignores the chunk.
        let line = r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_delta","contentIndex":1,"delta":"{\"file_path\":\"a\"}","partial":{}}}"#;
        let p = parse(line);
        assert!(p.events.is_empty());
        assert!(p.response.is_none());
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
    fn turn_end_without_message_end_yields_fallback_text_then_turn_end() {
        let line = r#"{"type":"turn_end","message":{"role":"assistant","content":[{"type":"text","text":"final answer"}]},"toolResults":[]}"#;
        let p = parse(line);
        assert_eq!(p.events.len(), 3);
        match &p.events[0] {
            ParsedAgentEvent::TextDelta { text } => assert_eq!(text, "final answer"),
            other => panic!("expected fallback TextDelta, got {other:?}"),
        }
        assert!(matches!(p.events[1], ParsedAgentEvent::TextEnd));
        assert!(matches!(p.events[2], ParsedAgentEvent::TurnEnd));
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

    /// The success-path `aborted: false` is distinct from the default-via-
    /// missing-field path; if the wire field were renamed, `#[serde(default)]`
    /// would return `false` for both polarities and this assertion would
    /// remain green only by accident. Pinning the false case alongside the
    /// true case (above) catches the rename.
    #[test]
    fn compaction_end_aborted_false_carries_through() {
        let line = r#"{"type":"compaction_end","aborted":false,"willRetry":true}"#;
        let p = parse(line);
        assert!(matches!(
            p.events[..],
            [ParsedAgentEvent::CompactionEnd { aborted: false }]
        ));
    }

    #[test]
    fn pi_overflow_retry_waits_for_effective_repin() {
        let parser = PiParser::new();
        let start = parser
            .parse_line(r#"{"type":"compaction_start","reason":"overflow"}"#)
            .expect("overflow compaction_start parses");
        assert!(matches!(
            start.events[..],
            [ParsedAgentEvent::CompactionStart {
                reason: CompactionReason::ContextLimit,
            }]
        ));

        let end = parser
            .parse_line(
                r#"{"type":"compaction_end","aborted":false,"reason":"overflow","willRetry":true}"#,
            )
            .expect("overflow compaction_end parses");
        assert_eq!(end.events.len(), 3);
        assert!(matches!(
            end.events[0],
            ParsedAgentEvent::CompactionEnd { aborted: false }
        ));
        match &end.events[1] {
            ParsedAgentEvent::Error { message } => {
                assert!(message.contains("re-pin"));
                assert!(message.contains("full prompt"));
            }
            other => panic!("expected Error event, got {other:?}"),
        }
        match &end.events[2] {
            ParsedAgentEvent::SessionComplete {
                exit_code,
                cost_usd,
            } => {
                assert_eq!(*exit_code, 1);
                assert!(cost_usd.is_none());
            }
            other => panic!("expected SessionComplete event, got {other:?}"),
        }

        for line in [
            r#"{"type":"auto_retry_start","attempt":1,"maxAttempts":3,"delayMs":0,"errorMessage":"overflow retry"}"#,
            r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"UNPINNED_RETRY_OUTPUT"}}"#,
            r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"POST_REPIN_OUTPUT"}}"#,
        ] {
            let parsed = parser.parse_line(line).expect("post-terminal line parses");
            assert!(
                parsed.events.is_empty(),
                "unsafe retry output must stay quarantined after line: {line}"
            );
        }
    }

    /// `error` delta with only `reason` present falls back to surfacing
    /// `reason` as the human-readable message. The parser's
    /// `message.or(reason).unwrap_or_default()` chain makes this contract;
    /// pin it so reordering the chain (or renaming either field) surfaces
    /// the regression.
    #[test]
    fn message_update_error_delta_falls_back_to_reason_when_message_absent() {
        let line = r#"{"type":"message_update","assistantMessageEvent":{"type":"error","reason":"aborted"}}"#;
        let p = parse(line);
        assert_eq!(p.events.len(), 1);
        match &p.events[0] {
            ParsedAgentEvent::Error { message, .. } => assert_eq!(message, "aborted"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// `error` delta with neither field present yields an empty-string
    /// message rather than dropping the event. Renderers expect *some*
    /// Error event so the failure shows up in the log even when pi
    /// couldn't supply detail.
    #[test]
    fn message_update_error_delta_yields_empty_message_when_both_fields_absent() {
        let line = r#"{"type":"message_update","assistantMessageEvent":{"type":"error"}}"#;
        let p = parse(line);
        assert_eq!(p.events.len(), 1);
        match &p.events[0] {
            ParsedAgentEvent::Error { message, .. } => assert!(message.is_empty()),
            other => panic!("expected Error, got {other:?}"),
        }
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
        match err {
            ProtocolError::InvalidProtocolLine { preview, .. } => {
                assert_eq!(preview, "not-json");
            }
            other => panic!("expected InvalidProtocolLine, got {other:?}"),
        }
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
    fn encode_follow_up_emits_follow_up_command() {
        let parser = PiParser::new();
        let line = parser
            .encode_follow_up("next")
            .expect("encoder should succeed");
        let v: serde_json::Value =
            serde_json::from_str(line.trim_end()).expect("encoded follow_up is valid JSON");
        assert_eq!(v["type"], "follow_up");
        assert_eq!(v["message"], "next");
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
