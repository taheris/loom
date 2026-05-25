//! Pi-mono RPC protocol message types.
//!
//! Pi messages do not follow a clean tagged-union shape: responses carry
//! `type: "response"` plus an `id`, events carry their own `type` values
//! (`message_update`, `tool_execution_start`, …) without an `id`, and
//! extension UI requests carry `type: "extension_ui_request"`. The parser
//! therefore peeks at `(type, id)` via [`PiEnvelope`] and re-deserializes the
//! line into the matched concrete type.

use loom_driver::identifier::{RequestId, ToolCallId};
use serde::{Deserialize, Serialize};

/// First-pass peek of a pi JSONL line. Carries only the discriminating
/// fields; the parser re-deserializes into the appropriate concrete type
/// once the message category is known.
#[derive(Debug, Deserialize)]
pub struct PiEnvelope {
    /// Either `"response"`, `"extension_ui_request"`, or one of the event
    /// names. `None` for pathological lines that survive JSON parse without
    /// a `type` field.
    #[serde(rename = "type")]
    pub msg_type: Option<String>,

    /// Present on responses and extension UI requests; absent on events.
    /// The parser uses id-absence as the fallback that classifies the
    /// remainder as events.
    pub id: Option<RequestId>,
}

/// Response envelope — one of these is emitted for every command sent on
/// stdin. The `command` field echoes back the command name; `success`
/// discriminates between a successful `data` payload and a failure carried
/// in `error`.
#[derive(Debug, Deserialize)]
pub struct PiResponse {
    pub id: RequestId,
    pub command: String,
    pub success: bool,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Streaming event from pi (no `id` field). Discriminated by the wire
/// `type` value via serde's internally-tagged enum form. Variants whose
/// payload Loom does not consume (turn boundaries, retry telemetry,
/// extension errors) are unit forms — serde drops their extra fields.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PiEvent {
    /// Streaming assistant message update; the inner
    /// [`AssistantMessageDelta`] dispatch determines what (if anything) is
    /// emitted as an [`AgentEvent`](loom_driver::agent::AgentEvent).
    MessageUpdate {
        #[serde(rename = "assistantMessageEvent")]
        delta: AssistantMessageDelta,
    },

    /// Pi started executing a tool call.
    ToolExecutionStart {
        #[serde(rename = "toolCallId")]
        tool_call_id: ToolCallId,
        #[serde(rename = "toolName")]
        tool_name: String,
        #[serde(default)]
        args: serde_json::Value,
    },

    /// Pi finished executing a tool call.
    ToolExecutionEnd {
        #[serde(rename = "toolCallId")]
        tool_call_id: ToolCallId,
        #[serde(default)]
        result: serde_json::Value,
        #[serde(default, rename = "isError")]
        is_error: bool,
    },

    /// Streaming tool-call progress update — long-running tools emit
    /// these to report partial output. Surfaced as
    /// [`AgentEvent::ToolProgress`](loom_driver::agent::AgentEvent::ToolProgress)
    /// so renderers can keep the user oriented.
    ToolExecutionUpdate {
        #[serde(rename = "toolCallId")]
        tool_call_id: ToolCallId,
        /// Pi sends the partial as a JSON value; the parser stringifies
        /// when it isn't already a string for the renderer body.
        #[serde(default, rename = "partialResult")]
        partial_result: serde_json::Value,
    },

    /// Turn boundaries — payload is dropped.
    TurnStart,
    TurnEnd,

    /// Agent lifecycle.
    AgentStart,
    AgentEnd,

    /// Compaction lifecycle. The reason string is one of `"threshold"`,
    /// `"overflow"`, `"manual"` as of pi v0.72.
    CompactionStart {
        #[serde(default)]
        reason: Option<String>,
    },
    CompactionEnd {
        #[serde(default)]
        aborted: bool,
    },

    /// Per-stream queue change — observability only.
    QueueUpdate,

    /// Auto-retry telemetry — surfaced as
    /// [`AgentEvent::AutoRetry`](loom_driver::agent::AgentEvent::AutoRetry)
    /// so the renderer can show transient-failure progress.
    /// `attempt`/`maxAttempts` are 1-indexed; `delayMs` is the back-off
    /// the next try will wait.
    AutoRetryStart {
        #[serde(default)]
        attempt: u32,
        #[serde(default, rename = "maxAttempts")]
        max_attempts: u32,
        #[serde(default, rename = "delayMs")]
        delay_ms: u64,
        #[serde(default, rename = "errorMessage")]
        error_message: String,
    },
    AutoRetryEnd,

    /// Extension reported an error — observability only.
    ExtensionError,

    /// Forward-compatibility catch-all so a new pi event type does not
    /// fail the parse. Logged at trace level by the parser.
    #[serde(other)]
    Unknown,
}

/// Inner `assistantMessageEvent` delta carried by
/// [`PiEvent::MessageUpdate`]. Dispatched on the nested `type` field.
/// Each variant maps to an [`AgentEvent`](loom_driver::agent::AgentEvent)
/// the renderer / log replayer consumes; the `Unknown` catch-all keeps
/// forward-compat for delta types pi adds later.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantMessageDelta {
    /// Streaming text fragment.
    TextDelta { text: String },

    /// Closes a `text_delta` stream — paired terminator.
    TextEnd,

    /// Streaming "thinking" fragment (extended-thinking models).
    ThinkingDelta { text: String },

    /// Closes a `thinking_delta` stream.
    ThinkingEnd,

    /// Streaming tool-call argument fragment — pi has decided to call
    /// a tool but is still emitting its JSON params one chunk at a
    /// time. The `toolCallId` field comes camelCased on the wire.
    ToolcallDelta {
        #[serde(rename = "toolCallId")]
        tool_call_id: ToolCallId,
        delta: String,
    },

    /// Mid-stream error from the agent. Pi populates `reason`
    /// (`"aborted"` / `"error"`) and may include a `message` with
    /// human-readable detail.
    Error {
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        message: Option<String>,
    },

    /// Forward-compatibility catch-all for delta types Loom does not
    /// yet consume (`start`, `text_start`, `toolcall_start`,
    /// `toolcall_end`, `done`, …). Logged at trace level by the parser.
    #[serde(other)]
    Unknown,
}

/// Extension UI request (`type: "extension_ui_request"`). Loom replies
/// with an auto-cancel for response-required methods (`select`,
/// `confirm`, `input`, `editor`); methods that do not need a response
/// (`notify`, `setStatus`, `setWidget`, `setTitle`, `set_editor_text`)
/// are skipped silently.
#[derive(Debug, Deserialize)]
pub struct PiUiRequest {
    pub id: RequestId,
    pub method: String,
}

/// Auto-cancel reply for [`PiUiRequest`] methods that block the agent
/// awaiting a host response. The shape matches pi's `extension_ui_response`
/// — `cancelled: true` tells the extension the host declined.
#[derive(Debug, Serialize)]
pub struct ExtensionUiResponse<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: &'a RequestId,
    pub cancelled: bool,
}

/// `prompt` command body — opens the session.
#[derive(Debug, Serialize)]
pub struct PromptCommand<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub message: &'a str,
}

/// `steer` command body — mid-session course correction.
#[derive(Debug, Serialize)]
pub struct SteerCommand<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub message: &'a str,
}

/// `abort` command body — terminates the in-flight operation.
#[derive(Debug, Serialize)]
pub struct AbortCommand {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

/// `set_thinking_level` command body — best-effort post-handshake hint
/// per `specs/agent.md`'s Pi command table. The `level` field carries
/// the lowercase wire token; pi rejection is downgraded to a `warn!` in
/// the driver so providers without thinking support continue uninterrupted.
#[derive(Debug, Serialize)]
pub struct SetThinkingLevelCommand<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: &'a str,
    pub level: &'a str,
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;

    /// Successful response carries `id`, `command`, `success: true`, and an
    /// optional `data` payload — every documented field round-trips into the
    /// typed struct so a silent rename in pi v0.72+ surfaces as a test
    /// failure rather than dropping data on the floor.
    #[test]
    fn pi_response_success_populates_data_field() {
        let line =
            r#"{"type":"response","id":"r-1","command":"prompt","success":true,"data":{"k":"v"}}"#;
        let resp: PiResponse = serde_json::from_str(line).expect("parse");
        assert_eq!(resp.id.as_str(), "r-1");
        assert_eq!(resp.command, "prompt");
        assert!(resp.success);
        let data = resp.data.expect("data present on success");
        assert_eq!(data["k"], "v");
        assert!(resp.error.is_none());
    }

    /// Failure response carries `success: false` and an `error` string
    /// (`data` may or may not be present).
    #[test]
    fn pi_response_failure_populates_error_field() {
        let line = r#"{"type":"response","id":"r-2","command":"set_model","success":false,"error":"unsupported provider"}"#;
        let resp: PiResponse = serde_json::from_str(line).expect("parse");
        assert_eq!(resp.id.as_str(), "r-2");
        assert_eq!(resp.command, "set_model");
        assert!(!resp.success);
        assert_eq!(resp.error.as_deref(), Some("unsupported provider"));
        assert!(resp.data.is_none());
    }

    /// `data` and `error` are both optional via `#[serde(default)]` so a
    /// minimal response (only the four required fields) parses without
    /// either populated.
    #[test]
    fn pi_response_minimal_shape_omits_data_and_error() {
        let line = r#"{"type":"response","id":"r-3","command":"abort","success":true}"#;
        let resp: PiResponse = serde_json::from_str(line).expect("parse");
        assert!(resp.data.is_none());
        assert!(resp.error.is_none());
    }

    /// `tool_execution_start` field mapping: every documented field
    /// (`toolCallId`, `toolName`, `args`) round-trips into the typed enum
    /// variant. Pinning the wire names — including the camelCase rename —
    /// catches a silent rename on pi's side.
    #[test]
    fn pi_event_tool_execution_start_maps_all_fields() {
        let line = r#"{"type":"tool_execution_start","toolCallId":"tc-9","toolName":"Read","args":{"path":"/x"}}"#;
        let event: PiEvent = serde_json::from_str(line).expect("parse");
        match event {
            PiEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                assert_eq!(tool_call_id.as_str(), "tc-9");
                assert_eq!(tool_name, "Read");
                assert_eq!(args["path"], "/x");
            }
            other => panic!("expected ToolExecutionStart, got {other:?}"),
        }
    }

    /// `tool_execution_end` field mapping: `toolCallId`, `result`, `isError`.
    #[test]
    fn pi_event_tool_execution_end_maps_all_fields() {
        let line =
            r#"{"type":"tool_execution_end","toolCallId":"tc-9","result":"ok","isError":true}"#;
        let event: PiEvent = serde_json::from_str(line).expect("parse");
        match event {
            PiEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
            } => {
                assert_eq!(tool_call_id.as_str(), "tc-9");
                assert_eq!(result, serde_json::Value::String("ok".into()));
                assert!(is_error);
            }
            other => panic!("expected ToolExecutionEnd, got {other:?}"),
        }
    }

    /// `extension_ui_request` carries `id` and `method`; the `payload` is
    /// dropped because Loom only needs the method to decide auto-cancel.
    #[test]
    fn pi_ui_request_maps_id_and_method() {
        let line = r#"{"type":"extension_ui_request","id":"u-1","method":"select","payload":{}}"#;
        let req: PiUiRequest = serde_json::from_str(line).expect("parse");
        assert_eq!(req.id.as_str(), "u-1");
        assert_eq!(req.method, "select");
    }

    /// Every command struct serializes to a JSONL line whose `type` field
    /// matches the wire contract.
    #[test]
    fn command_structs_serialize_to_expected_type_field() {
        let prompt = serde_json::to_string(&PromptCommand {
            kind: "prompt",
            message: "x",
        })
        .expect("serialize prompt");
        let prompt_v: serde_json::Value = serde_json::from_str(&prompt).expect("parse");
        assert_eq!(prompt_v["type"], "prompt");
        assert_eq!(prompt_v["message"], "x");

        let steer = serde_json::to_string(&SteerCommand {
            kind: "steer",
            message: "y",
        })
        .expect("serialize steer");
        let steer_v: serde_json::Value = serde_json::from_str(&steer).expect("parse");
        assert_eq!(steer_v["type"], "steer");
        assert_eq!(steer_v["message"], "y");

        let abort =
            serde_json::to_string(&AbortCommand { kind: "abort" }).expect("serialize abort");
        let abort_v: serde_json::Value = serde_json::from_str(&abort).expect("parse");
        assert_eq!(abort_v["type"], "abort");
    }

    /// `set_thinking_level` serializes to a JSONL line with the documented
    /// shape: `type` discriminator, request `id`, and lowercase `level`
    /// token. Pin every field so a rename on either side surfaces here.
    #[test]
    fn set_thinking_level_command_serializes_to_expected_shape() {
        let cmd = SetThinkingLevelCommand {
            kind: "set_thinking_level",
            id: "loom-pi-set-thinking-level",
            level: "high",
        };
        let json = serde_json::to_string(&cmd).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(v["type"], "set_thinking_level");
        assert_eq!(v["id"], "loom-pi-set-thinking-level");
        assert_eq!(v["level"], "high");
    }
}
