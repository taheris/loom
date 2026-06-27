//! Pi-mono RPC protocol message types.
//!
//! Pi messages do not follow a clean tagged-union shape: correlated responses
//! carry `type: "response"` plus an `id`, some command-ack responses omit the
//! `id`, events carry their own `type` values
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

    /// Present on correlated responses and extension UI requests; absent on
    /// events and on some command-ack responses (notably prompt acks). The
    /// parser uses `type`, not id-presence alone, for response classification.
    pub id: Option<RequestId>,
}

/// Response envelope — one of these is emitted for commands sent on stdin.
/// Request/response handshake commands carry `id`; current Pi prompt acks do
/// not. The `command` field echoes back the command name; `success`
/// discriminates between a successful `data` payload and a failure carried
/// in `error`.
#[derive(Debug, Deserialize)]
pub struct PiResponse {
    #[serde(default)]
    pub id: Option<RequestId>,
    pub command: String,
    pub success: bool,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Streaming event from pi (no `id` field). Discriminated by the wire
/// `type` value via serde's internally-tagged enum form. Variants whose
/// payload Loom does not consume (retry telemetry, extension errors) are
/// unit forms — serde drops their extra fields.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PiEvent {
    /// Assistant message lifecycle start. The payload is kept as raw JSON
    /// because Loom only needs a fallback text extraction path when the
    /// provider emits final content without streaming `text_delta` chunks.
    MessageStart {
        message: serde_json::Value,
    },

    /// Streaming assistant message update; the inner
    /// [`AssistantMessageDelta`] dispatch determines what (if anything) is
    /// emitted as an [`AgentEvent`](loom_driver::agent::AgentEvent).
    MessageUpdate {
        #[serde(rename = "assistantMessageEvent")]
        delta: AssistantMessageDelta,
    },

    /// Assistant message lifecycle end. Carries the complete message.
    MessageEnd {
        message: serde_json::Value,
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

    /// Turn start boundary — payload is dropped.
    TurnStart,

    /// Turn completion boundary. Carries the complete assistant message and
    /// tool results; Loom uses the message as a fallback final-text source
    /// when `message_end`/streaming deltas did not surface it.
    TurnEnd {
        #[serde(default)]
        message: serde_json::Value,
    },

    /// Agent lifecycle.
    AgentStart,
    AgentEnd {
        #[serde(default)]
        messages: Vec<serde_json::Value>,
    },

    /// Compaction lifecycle. The reason string is one of `"threshold"`,
    /// `"overflow"`, `"manual"` as of pi v0.72.
    CompactionStart {
        #[serde(default)]
        reason: Option<String>,
    },
    CompactionEnd {
        #[serde(default)]
        aborted: bool,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default, rename = "willRetry")]
        will_retry: bool,
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
    /// Streaming text fragment. Pi 0.73 uses `delta`; older mocks/tests used
    /// `text`, so the parser accepts both wire field names.
    TextDelta {
        #[serde(alias = "delta")]
        text: String,
    },

    /// Closes a `text_delta` stream — paired terminator.
    TextEnd,

    /// Streaming "thinking" fragment (extended-thinking models). Pi 0.73
    /// uses `delta`; older mocks/tests used `text`, so both are accepted.
    ThinkingDelta {
        #[serde(alias = "delta")]
        text: String,
    },

    /// Closes a `thinking_delta` stream.
    ThinkingEnd,

    /// Streaming tool-call argument fragment in the assistant message stream.
    /// Current Pi identifies these by `contentIndex` and may omit
    /// `toolCallId`; Loom does not need these chunks to drive tools because
    /// executable tool lifecycle arrives separately via `tool_execution_*`.
    ToolcallDelta {
        #[serde(default, rename = "toolCallId")]
        tool_call_id: Option<ToolCallId>,
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

/// `follow_up` command body — post-turn interactive reply.
#[derive(Debug, Serialize)]
pub struct FollowUpCommand<'a> {
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
        assert_eq!(resp.id.as_ref().map(|id| id.as_str()), Some("r-1"));
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
        assert_eq!(resp.id.as_ref().map(|id| id.as_str()), Some("r-2"));
        assert_eq!(resp.command, "set_model");
        assert!(!resp.success);
        assert_eq!(resp.error.as_deref(), Some("unsupported provider"));
        assert!(resp.data.is_none());
    }

    /// `data` and `error` are both optional via `#[serde(default)]` so a
    /// minimal response parses without either populated.
    #[test]
    fn pi_response_minimal_shape_omits_data_and_error() {
        let line = r#"{"type":"response","id":"r-3","command":"abort","success":true}"#;
        let resp: PiResponse = serde_json::from_str(line).expect("parse");
        assert_eq!(resp.id.as_ref().map(|id| id.as_str()), Some("r-3"));
        assert!(resp.data.is_none());
        assert!(resp.error.is_none());
    }

    /// Current Pi prompt acknowledgements omit `id`; mid-session parsing
    /// must accept and drop them while handshake correlation remains strict
    /// in the backend's `await_response` loop.
    #[test]
    fn pi_response_prompt_ack_allows_missing_id() {
        let line = r#"{"type":"response","command":"prompt","success":true}"#;
        let resp: PiResponse = serde_json::from_str(line).expect("parse");
        assert!(resp.id.is_none());
        assert_eq!(resp.command, "prompt");
        assert!(resp.success);
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

    /// Two-phase classification: a `response` envelope populates BOTH
    /// `msg_type` and `id`. Loom's parser inspects this pair before
    /// re-deserializing into the concrete type; if pi renamed either field
    /// the classification path would silently misroute the line.
    #[test]
    fn pi_envelope_response_line_populates_msg_type_and_id() {
        let line = r#"{"type":"response","id":"r-7","command":"prompt","success":true}"#;
        let env: PiEnvelope = serde_json::from_str(line).expect("parse envelope");
        assert_eq!(env.msg_type.as_deref(), Some("response"));
        assert_eq!(env.id.as_ref().map(RequestId::as_str), Some("r-7"));
    }

    /// Two-phase classification: an event envelope populates `msg_type` but
    /// leaves `id` as `None`. Loom uses id-absence to fall through to the
    /// event branch, so a stray `id` on an event would misclassify it as
    /// an unknown response.
    #[test]
    fn pi_envelope_event_line_omits_id() {
        let line = r#"{"type":"turn_start","extra":1}"#;
        let env: PiEnvelope = serde_json::from_str(line).expect("parse envelope");
        assert_eq!(env.msg_type.as_deref(), Some("turn_start"));
        assert!(env.id.is_none());
    }

    /// Envelope tolerates a missing `type` field — the parser then falls
    /// through to `UnknownMessageType`. Field-level pin: `msg_type` must
    /// be `Option<String>`, not `String`, to round-trip the pathological
    /// shape.
    #[test]
    fn pi_envelope_without_type_field_yields_none_msg_type() {
        let line = r#"{"id":"x"}"#;
        let env: PiEnvelope = serde_json::from_str(line).expect("parse envelope");
        assert!(env.msg_type.is_none());
        assert_eq!(env.id.as_ref().map(RequestId::as_str), Some("x"));
    }

    /// `ExtensionUiResponse` (the auto-cancel reply pi expects when an
    /// extension blocks the agent) serializes with every field on the
    /// wire: `type`, `id`, `cancelled`. Pin each field so a rename or
    /// missed serialization on the host side fails this test before pi
    /// silently hangs waiting for a reply.
    #[test]
    fn extension_ui_response_serializes_with_all_fields() {
        let id = RequestId::new("u-42");
        let payload = ExtensionUiResponse {
            kind: "extension_ui_response",
            id: &id,
            cancelled: true,
        };
        let json = serde_json::to_string(&payload).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(v["type"], "extension_ui_response");
        assert_eq!(v["id"], "u-42");
        assert_eq!(v["cancelled"], true);
    }

    /// `message_update` carries a nested `assistantMessageEvent` — the
    /// outer envelope's only documented field is `delta` (renamed from
    /// `assistantMessageEvent` on the wire). The two-phase parser reaches
    /// the inner delta via this rename; if pi dropped the camelCase the
    /// envelope would fail to populate.
    #[test]
    fn pi_event_message_update_carries_assistant_message_event() {
        let line =
            r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","text":"x"}}"#;
        let event: PiEvent = serde_json::from_str(line).expect("parse");
        match event {
            PiEvent::MessageUpdate { delta } => match delta {
                AssistantMessageDelta::TextDelta { text } => assert_eq!(text, "x"),
                other => panic!("expected TextDelta, got {other:?}"),
            },
            other => panic!("expected MessageUpdate, got {other:?}"),
        }
    }

    /// `tool_execution_update` field mapping — `toolCallId` (camelCase
    /// rename) and `partialResult` round-trip. Long-running tools emit
    /// these; a silent rename would drop progress events on the floor.
    #[test]
    fn pi_event_tool_execution_update_maps_all_fields() {
        let line =
            r#"{"type":"tool_execution_update","toolCallId":"tc-5","partialResult":"halfway"}"#;
        let event: PiEvent = serde_json::from_str(line).expect("parse");
        match event {
            PiEvent::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
            } => {
                assert_eq!(tool_call_id.as_str(), "tc-5");
                assert_eq!(partial_result, serde_json::Value::String("halfway".into()));
            }
            other => panic!("expected ToolExecutionUpdate, got {other:?}"),
        }
    }

    /// `compaction_start` field mapping: the `reason` token round-trips as
    /// an opaque string so the parser can map it to a typed
    /// `CompactionReason` downstream. Pin the field name so a rename on
    /// pi's side fails the deserialization here.
    #[test]
    fn pi_event_compaction_start_maps_reason_field() {
        let line = r#"{"type":"compaction_start","reason":"threshold"}"#;
        let event: PiEvent = serde_json::from_str(line).expect("parse");
        match event {
            PiEvent::CompactionStart { reason } => assert_eq!(reason.as_deref(), Some("threshold")),
            other => panic!("expected CompactionStart, got {other:?}"),
        }
    }

    /// `compaction_end` field mapping: the `aborted` boolean round-trips
    /// in both polarities. The success-path event (`aborted: false`) is
    /// the default — easy to silently break when the wire field is
    /// renamed because `#[serde(default)]` would then return `false`
    /// regardless. Pinning both polarities catches the rename.
    #[test]
    fn pi_event_compaction_end_maps_aborted_field_true() {
        let line = r#"{"type":"compaction_end","aborted":true}"#;
        let event: PiEvent = serde_json::from_str(line).expect("parse");
        match event {
            PiEvent::CompactionEnd {
                aborted,
                reason,
                will_retry,
            } => {
                assert!(aborted);
                assert!(reason.is_none());
                assert!(!will_retry);
            }
            other => panic!("expected CompactionEnd, got {other:?}"),
        }
    }

    #[test]
    fn pi_event_compaction_end_maps_aborted_field_false() {
        let line = r#"{"type":"compaction_end","aborted":false,"willRetry":true}"#;
        let event: PiEvent = serde_json::from_str(line).expect("parse");
        match event {
            PiEvent::CompactionEnd {
                aborted,
                reason,
                will_retry,
            } => {
                assert!(!aborted);
                assert!(will_retry);
                assert!(reason.is_none());
            }
            other => panic!("expected CompactionEnd, got {other:?}"),
        }
    }

    #[test]
    fn pi_event_compaction_end_maps_reason_and_will_retry_fields() {
        let line =
            r#"{"type":"compaction_end","aborted":false,"reason":"overflow","willRetry":true}"#;
        let event: PiEvent = serde_json::from_str(line).expect("parse");
        match event {
            PiEvent::CompactionEnd {
                aborted,
                reason,
                will_retry,
            } => {
                assert!(!aborted);
                assert_eq!(reason.as_deref(), Some("overflow"));
                assert!(will_retry);
            }
            other => panic!("expected CompactionEnd, got {other:?}"),
        }
    }

    /// `auto_retry_start` field mapping: all four documented fields
    /// (`attempt`, `maxAttempts`, `delayMs`, `errorMessage`) round-trip
    /// with their camelCase wire names. Loom surfaces these to the
    /// renderer as transient-failure progress — silent renames would
    /// blank the retry UI.
    #[test]
    fn pi_event_auto_retry_start_maps_all_fields() {
        let line = r#"{"type":"auto_retry_start","attempt":3,"maxAttempts":7,"delayMs":2500,"errorMessage":"network"}"#;
        let event: PiEvent = serde_json::from_str(line).expect("parse");
        match event {
            PiEvent::AutoRetryStart {
                attempt,
                max_attempts,
                delay_ms,
                error_message,
            } => {
                assert_eq!(attempt, 3);
                assert_eq!(max_attempts, 7);
                assert_eq!(delay_ms, 2500);
                assert_eq!(error_message, "network");
            }
            other => panic!("expected AutoRetryStart, got {other:?}"),
        }
    }

    /// Lifecycle events tolerate any extension fields pi may add. Pin every
    /// fieldless variant individually so an accidental change in serde
    /// representation surfaces as a parse failure here.
    #[test]
    fn pi_event_lifecycle_variants_drop_extension_fields() {
        for (line, expected) in [
            (r#"{"type":"turn_start","ignored":1}"#, "TurnStart"),
            (r#"{"type":"turn_end","message":{"x":1}}"#, "TurnEnd"),
            (r#"{"type":"agent_start","ignored":1}"#, "AgentStart"),
            (r#"{"type":"agent_end","messages":[]}"#, "AgentEnd"),
            (
                r#"{"type":"queue_update","steering":[],"followUp":[]}"#,
                "QueueUpdate",
            ),
            (
                r#"{"type":"auto_retry_end","success":true}"#,
                "AutoRetryEnd",
            ),
            (
                r#"{"type":"extension_error","extensionPath":"/x","event":"y","error":"z"}"#,
                "ExtensionError",
            ),
        ] {
            let event: PiEvent = serde_json::from_str(line).expect("parse unit variant");
            let actual = match event {
                PiEvent::TurnStart => "TurnStart",
                PiEvent::TurnEnd { .. } => "TurnEnd",
                PiEvent::AgentStart => "AgentStart",
                PiEvent::AgentEnd { .. } => "AgentEnd",
                PiEvent::QueueUpdate => "QueueUpdate",
                PiEvent::AutoRetryEnd => "AutoRetryEnd",
                PiEvent::ExtensionError => "ExtensionError",
                other => panic!("expected {expected}, got {other:?}"),
            };
            assert_eq!(actual, expected);
        }
    }

    /// Forward-compatibility for brand-new pi event types: the
    /// `#[serde(other)]` catch-all variant absorbs any unknown `type`
    /// value without failing the deserialization. A regression here
    /// would force every pi version bump to ship a Loom-side parser
    /// change before the new build could speak to it.
    #[test]
    fn pi_event_unknown_type_falls_through_serde_other() {
        let line = r#"{"type":"brand_new_event_in_v0_99","novel":42}"#;
        let event: PiEvent = serde_json::from_str(line).expect("parse");
        assert!(matches!(event, PiEvent::Unknown));
    }

    /// Inner `assistantMessageEvent` delta variants: `text_delta` round-trips
    /// the current pi `delta` field; the parser later surfaces this as
    /// `ParsedAgentEvent::TextDelta`.
    #[test]
    fn assistant_delta_text_delta_maps_delta_field() {
        let line = r#"{"type":"text_delta","delta":"hello"}"#;
        let delta: AssistantMessageDelta = serde_json::from_str(line).expect("parse");
        match delta {
            AssistantMessageDelta::TextDelta { text } => assert_eq!(text, "hello"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
    }

    /// `text_delta` also accepts the legacy `text` field used by older
    /// mocks, preserving compatibility with existing fixtures.
    #[test]
    fn assistant_delta_text_delta_accepts_legacy_text_field() {
        let line = r#"{"type":"text_delta","text":"hello"}"#;
        let delta: AssistantMessageDelta = serde_json::from_str(line).expect("parse");
        match delta {
            AssistantMessageDelta::TextDelta { text } => assert_eq!(text, "hello"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
    }

    /// `thinking_delta` round-trips the current pi `delta` field.
    #[test]
    fn assistant_delta_thinking_delta_maps_delta_field() {
        let line = r#"{"type":"thinking_delta","delta":"thought"}"#;
        let delta: AssistantMessageDelta = serde_json::from_str(line).expect("parse");
        match delta {
            AssistantMessageDelta::ThinkingDelta { text } => assert_eq!(text, "thought"),
            other => panic!("expected ThinkingDelta, got {other:?}"),
        }
    }

    /// `toolcall_delta` round-trips legacy idful chunks: `toolCallId`
    /// (camelCase rename) and `delta` (raw chunk of streaming tool-call JSON).
    #[test]
    fn assistant_delta_toolcall_delta_maps_both_fields() {
        let line = r#"{"type":"toolcall_delta","toolCallId":"tc-1","delta":"chunk"}"#;
        let delta: AssistantMessageDelta = serde_json::from_str(line).expect("parse");
        match delta {
            AssistantMessageDelta::ToolcallDelta {
                tool_call_id,
                delta,
            } => {
                assert_eq!(tool_call_id.as_ref().map(|id| id.as_str()), Some("tc-1"));
                assert_eq!(delta, "chunk");
            }
            other => panic!("expected ToolcallDelta, got {other:?}"),
        }
    }

    /// Current Pi assistant-message `toolcall_delta` chunks are indexed by
    /// `contentIndex` and omit `toolCallId`; executable tool lifecycle still
    /// arrives later via `tool_execution_*` events.
    #[test]
    fn assistant_delta_toolcall_delta_allows_missing_tool_call_id() {
        let line = r#"{"type":"toolcall_delta","contentIndex":1,"delta":"chunk","partial":{}}"#;
        let delta: AssistantMessageDelta = serde_json::from_str(line).expect("parse");
        match delta {
            AssistantMessageDelta::ToolcallDelta {
                tool_call_id,
                delta,
            } => {
                assert!(tool_call_id.is_none());
                assert_eq!(delta, "chunk");
            }
            other => panic!("expected ToolcallDelta, got {other:?}"),
        }
    }

    /// `error` delta round-trips BOTH `reason` and `message` — they're
    /// optional via `#[serde(default)]` because pi may emit either or
    /// both. The parser layer uses `message.or(reason)` to surface a
    /// single string; this test pins the wire fields.
    #[test]
    fn assistant_delta_error_maps_both_fields() {
        let line = r#"{"type":"error","reason":"aborted","message":"user cancelled"}"#;
        let delta: AssistantMessageDelta = serde_json::from_str(line).expect("parse");
        match delta {
            AssistantMessageDelta::Error { reason, message } => {
                assert_eq!(reason.as_deref(), Some("aborted"));
                assert_eq!(message.as_deref(), Some("user cancelled"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// `text_end` / `thinking_end` are unit-form deltas — paired
    /// terminators carrying no fields. A drift in either's wire token
    /// would fail this test before silently mis-pairing with their
    /// `*_delta` openers.
    #[test]
    fn assistant_delta_unit_variants_deserialize_as_units() {
        let text_end: AssistantMessageDelta =
            serde_json::from_str(r#"{"type":"text_end"}"#).expect("parse text_end");
        assert!(matches!(text_end, AssistantMessageDelta::TextEnd));

        let thinking_end: AssistantMessageDelta =
            serde_json::from_str(r#"{"type":"thinking_end"}"#).expect("parse thinking_end");
        assert!(matches!(thinking_end, AssistantMessageDelta::ThinkingEnd));
    }

    /// Forward-compatibility: a brand-new delta type pi adds later
    /// falls through to `Unknown` rather than failing the parse.
    #[test]
    fn assistant_delta_unknown_type_falls_through_serde_other() {
        let line = r#"{"type":"brand_new_delta","novel":1}"#;
        let delta: AssistantMessageDelta = serde_json::from_str(line).expect("parse");
        assert!(matches!(delta, AssistantMessageDelta::Unknown));
    }
}
