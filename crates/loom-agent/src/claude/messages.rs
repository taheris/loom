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
