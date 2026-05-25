//! Claude Code stream-json protocol message types.
//!
//! Unlike pi, claude's JSONL messages follow a clean tagged union: every
//! line carries a `type` field that uniquely identifies the variant. Serde's
//! internally-tagged enum handles dispatch directly, with `#[serde(other)]`
//! catching any future variants without breaking the build.
//!
//! Inner content blocks (`assistant` text/tool_use, `user` tool_result) are
//! also tagged unions; the same `#[serde(tag = "type")]` pattern dispatches
//! into [`AssistantBlock`] and [`UserBlock`] respectively. Unknown content
//! types are absorbed by `#[serde(other)]` so a forward-compatible block
//! shape (e.g. `thinking`) does not fail the parse.

use loom_driver::identifier::{RequestId, SessionId, ToolCallId};
use serde::Deserialize;

/// Tagged union of every line type emitted by `claude --output-format
/// stream-json`. Discriminated by the `type` field on each JSONL message.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClaudeMessage {
    /// Session metadata. Subtype `init` carries the `session_id`; other
    /// subtypes are forwarded for debug logging.
    #[serde(rename = "system")]
    System {
        subtype: String,
        session_id: Option<SessionId>,
    },

    /// Assistant turn payload — text deltas and tool-use entries live in
    /// `message.content`.
    #[serde(rename = "assistant")]
    Assistant { message: AssistantContent },

    /// User turn payload — tool results echoed back live in `message.content`.
    #[serde(rename = "user")]
    User { message: UserContent },

    /// Final-result line. `subtype: "success"` maps to `TurnEnd` followed by
    /// `SessionComplete`; `subtype: "error"` maps to `Error` followed by
    /// `SessionComplete`. `total_cost_usd` is captured into
    /// [`SessionOutcome::cost_usd`](loom_driver::agent::SessionOutcome::cost_usd).
    #[serde(rename = "result")]
    Result {
        subtype: String,
        result: Option<String>,
        total_cost_usd: Option<f64>,
        duration_ms: Option<u64>,
        num_turns: Option<u32>,
        is_error: Option<bool>,
    },

    /// Tool permission probe. With `--permission-prompt-tool stdio`, claude
    /// emits these and expects a `control_response` on stdin. Loom auto-
    /// approves (sandbox is the trust boundary) but logs every approval at
    /// `info!` for an audit trail.
    #[serde(rename = "control_request")]
    ControlRequest {
        id: RequestId,
        tool: String,
        input: serde_json::Value,
    },

    /// Forward-compatibility catch-all so a new claude message type does not
    /// fail the parse.
    #[serde(other)]
    Unknown,
}

/// Body of an `assistant` line — only `content` matters for event mapping;
/// other fields (`role`, `id`, `usage`, …) are dropped by serde.
#[derive(Debug, Deserialize)]
pub struct AssistantContent {
    pub content: Vec<AssistantBlock>,
}

/// One entry in an assistant message's `content` array. `text` blocks become
/// [`AgentEvent::TextDelta`](loom_driver::agent::AgentEvent::TextDelta);
/// `tool_use` blocks become
/// [`AgentEvent::ToolCall`](loom_driver::agent::AgentEvent::ToolCall).
/// `thinking` blocks — emitted when the model uses extended thinking —
/// map to [`AgentEvent::ThinkingDelta`](loom_driver::agent::AgentEvent::ThinkingDelta)
/// when the model uses extended thinking. Anything else lands in the
/// catch-all and is logged at `trace!`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: ToolCallId,
        name: String,
        input: serde_json::Value,
    },
    /// Extended-thinking content block. Claude streams the model's
    /// pre-reply reasoning here when the model supports it.
    Thinking {
        thinking: String,
    },
    #[serde(other)]
    Unknown,
}

/// Body of a `user` line — only `content` matters; the role field is dropped.
#[derive(Debug, Deserialize)]
pub struct UserContent {
    pub content: Vec<UserBlock>,
}

/// One entry in a user message's `content` array. `tool_result` blocks become
/// [`AgentEvent::ToolResult`](loom_driver::agent::AgentEvent::ToolResult).
/// `content` may be a plain string or a nested block array — the parser
/// stringifies whichever shape arrives.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserBlock {
    ToolResult {
        tool_use_id: ToolCallId,
        #[serde(default)]
        content: serde_json::Value,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;

    fn parse(line: &str) -> ClaudeMessage {
        serde_json::from_str(line).expect("fixture line should deserialize")
    }

    // -- ClaudeMessage variants ---------------------------------------------

    /// Acceptance test (per `specs/tests.md` Success Criteria): a brand-new
    /// `type` value not in the variant set lands in `Unknown` via
    /// `#[serde(other)]` and does not surface as an error. Forward-compat
    /// across upstream Claude releases hinges on this.
    #[test]
    fn claude_unknown_event_type_does_not_error() {
        let line = r#"{"type":"new_feature_event","data":"something"}"#;
        let msg: ClaudeMessage = serde_json::from_str(line).expect("parse");
        assert!(matches!(msg, ClaudeMessage::Unknown));
    }

    #[test]
    fn system_variant_carries_subtype_and_session_id() {
        match parse(r#"{"type":"system","subtype":"init","session_id":"sess-1"}"#) {
            ClaudeMessage::System {
                subtype,
                session_id,
            } => {
                assert_eq!(subtype, "init");
                assert_eq!(session_id.expect("session_id").as_str(), "sess-1");
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    #[test]
    fn system_variant_accepts_absent_session_id() {
        match parse(r#"{"type":"system","subtype":"context_warning"}"#) {
            ClaudeMessage::System {
                subtype,
                session_id,
            } => {
                assert_eq!(subtype, "context_warning");
                assert!(session_id.is_none());
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    #[test]
    fn assistant_variant_dispatches_into_blocks() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#;
        match parse(line) {
            ClaudeMessage::Assistant { message } => {
                assert_eq!(message.content.len(), 1);
                match &message.content[0] {
                    AssistantBlock::Text { text } => assert_eq!(text, "hi"),
                    other => panic!("expected Text block, got {other:?}"),
                }
            }
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    #[test]
    fn user_variant_dispatches_into_tool_result_blocks() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu-1","content":"ok","is_error":false}]}}"#;
        match parse(line) {
            ClaudeMessage::User { message } => {
                assert_eq!(message.content.len(), 1);
                match &message.content[0] {
                    UserBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        assert_eq!(tool_use_id.as_str(), "tu-1");
                        assert_eq!(content, &serde_json::Value::String("ok".into()));
                        assert!(!is_error);
                    }
                    other => panic!("expected ToolResult, got {other:?}"),
                }
            }
            other => panic!("expected User, got {other:?}"),
        }
    }

    /// `Result` has six documented fields. Every one is pinned so a silent
    /// upstream rename (e.g. `total_cost_usd` → `cost_total`) surfaces as a
    /// deserialization failure rather than a dropped value.
    #[test]
    fn result_variant_round_trips_every_documented_field() {
        let line = r#"{"type":"result","subtype":"success","result":"final","total_cost_usd":0.5,"duration_ms":2000,"num_turns":3,"is_error":false}"#;
        match parse(line) {
            ClaudeMessage::Result {
                subtype,
                result,
                total_cost_usd,
                duration_ms,
                num_turns,
                is_error,
            } => {
                assert_eq!(subtype, "success");
                assert_eq!(result.as_deref(), Some("final"));
                assert_eq!(total_cost_usd, Some(0.5));
                assert_eq!(duration_ms, Some(2000));
                assert_eq!(num_turns, Some(3));
                assert_eq!(is_error, Some(false));
            }
            other => panic!("expected Result, got {other:?}"),
        }
    }

    #[test]
    fn result_variant_treats_omitted_optional_fields_as_none() {
        let line = r#"{"type":"result","subtype":"error"}"#;
        match parse(line) {
            ClaudeMessage::Result {
                subtype,
                result,
                total_cost_usd,
                duration_ms,
                num_turns,
                is_error,
            } => {
                assert_eq!(subtype, "error");
                assert!(result.is_none());
                assert!(total_cost_usd.is_none());
                assert!(duration_ms.is_none());
                assert!(num_turns.is_none());
                assert!(is_error.is_none());
            }
            other => panic!("expected Result, got {other:?}"),
        }
    }

    #[test]
    fn control_request_variant_carries_id_tool_and_input() {
        let line =
            r#"{"type":"control_request","id":"req-9","tool":"Bash","input":{"command":"ls"}}"#;
        match parse(line) {
            ClaudeMessage::ControlRequest { id, tool, input } => {
                assert_eq!(id.as_str(), "req-9");
                assert_eq!(tool, "Bash");
                assert_eq!(input["command"], "ls");
            }
            other => panic!("expected ControlRequest, got {other:?}"),
        }
    }

    // -- AssistantBlock variants --------------------------------------------

    #[test]
    fn assistant_block_text_carries_text_field() {
        let line = r#"{"type":"text","text":"reply"}"#;
        let block: AssistantBlock = serde_json::from_str(line).expect("parse");
        match block {
            AssistantBlock::Text { text } => assert_eq!(text, "reply"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn assistant_block_tool_use_carries_id_name_input() {
        let line = r#"{"type":"tool_use","id":"tu-a","name":"Read","input":{"path":"/x"}}"#;
        let block: AssistantBlock = serde_json::from_str(line).expect("parse");
        match block {
            AssistantBlock::ToolUse { id, name, input } => {
                assert_eq!(id.as_str(), "tu-a");
                assert_eq!(name, "Read");
                assert_eq!(input["path"], "/x");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn assistant_block_thinking_carries_text() {
        let line = r#"{"type":"thinking","thinking":"weighing options"}"#;
        let block: AssistantBlock = serde_json::from_str(line).expect("parse");
        match block {
            AssistantBlock::Thinking { thinking } => assert_eq!(thinking, "weighing options"),
            other => panic!("expected Thinking, got {other:?}"),
        }
    }

    #[test]
    fn assistant_block_unknown_type_falls_through_serde_other() {
        let line = r#"{"type":"future_block","payload":"x"}"#;
        let block: AssistantBlock = serde_json::from_str(line).expect("parse");
        assert!(matches!(block, AssistantBlock::Unknown));
    }

    // -- UserBlock variants -------------------------------------------------

    #[test]
    fn user_block_tool_result_field_mapping() {
        let line = r#"{"type":"tool_result","tool_use_id":"tu-b","content":"out","is_error":true}"#;
        let block: UserBlock = serde_json::from_str(line).expect("parse");
        match block {
            UserBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_use_id.as_str(), "tu-b");
                assert_eq!(content, serde_json::Value::String("out".into()));
                assert!(is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn user_block_tool_result_defaults_missing_optional_fields() {
        let line = r#"{"type":"tool_result","tool_use_id":"tu-c"}"#;
        let block: UserBlock = serde_json::from_str(line).expect("parse");
        match block {
            UserBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_use_id.as_str(), "tu-c");
                assert_eq!(content, serde_json::Value::Null);
                assert!(!is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn user_block_unknown_type_falls_through_serde_other() {
        let line = r#"{"type":"future_block","payload":"x"}"#;
        let block: UserBlock = serde_json::from_str(line).expect("parse");
        assert!(matches!(block, UserBlock::Unknown));
    }

    // -- Malformed shape coverage ------------------------------------------

    /// A line whose JSON parses fine but lacks the `type` tag fails
    /// deserialization — `#[serde(tag = "type")]` requires the tag to be
    /// present; `#[serde(other)]` only catches unknown *values* of the tag,
    /// not its absence.
    #[test]
    fn missing_type_tag_returns_serde_error() {
        let err = serde_json::from_str::<ClaudeMessage>(r#"{"foo":42}"#).expect_err("parse fails");
        assert!(err.to_string().contains("type"));
    }

    /// Escaped `\n` inside a JSON string value is parsed as a single line
    /// and the value carries a literal `\n`. Confirms the JSONL framing
    /// contract: only un-escaped `\n` terminates a line; escaped sequences
    /// stay inside the string value.
    #[test]
    fn escaped_newline_in_string_value_preserves_literal_newline() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"line1\nline2"}]}}"#;
        match parse(line) {
            ClaudeMessage::Assistant { message } => match &message.content[0] {
                AssistantBlock::Text { text } => assert_eq!(text, "line1\nline2"),
                other => panic!("expected Text, got {other:?}"),
            },
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    /// U+2028 (LINE SEPARATOR) and U+2029 (PARAGRAPH SEPARATOR) appear
    /// verbatim inside JSON string values; they are not JSONL line
    /// terminators. The parser must accept them without misframing.
    #[test]
    fn unicode_line_separators_pass_through_string_values() {
        let line = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"a\u{2028}b\u{2029}c\"}]}}";
        match parse(line) {
            ClaudeMessage::Assistant { message } => match &message.content[0] {
                AssistantBlock::Text { text } => assert_eq!(text, "a\u{2028}b\u{2029}c"),
                other => panic!("expected Text, got {other:?}"),
            },
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    /// Truncated JSON: a partial line surfaces as a parse error. Mirrors
    /// the parser-level test but at the messages layer so a future
    /// serde-derive change can't accept partial input.
    #[test]
    fn truncated_json_returns_serde_error() {
        let err =
            serde_json::from_str::<ClaudeMessage>(r#"{"type":"message_del"#).expect_err("fails");
        assert!(err.is_eof() || err.is_syntax());
    }
}
