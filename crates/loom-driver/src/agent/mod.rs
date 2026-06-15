//! Agent backend abstraction surface owned by `loom-driver`.
//!
//! Defines the public types and traits that backends in `agent`
//! implement and that `loom-workflow` orchestrates over:
//!
//! - [`AgentBackend`] — minimal trait whose only job is to spawn an
//!   [`AgentSession`] in the [`Idle`] state.
//! - [`AgentSession`] — typestate-guarded conversation handle (`Idle` →
//!   `Active` after `prompt`).
//! - [`AgentEvent`] — the backend-neutral event enum the session yields.
//! - [`LineParse`] / [`ParsedLine`] — backend-specific protocol bridge: line
//!   parsing plus command encoding for prompt/steer/abort.
//! - [`JsonlReader`] — shared stdin line framing (10 MB max line cap).
//! - [`RePinContent`] — re-pin payload format used by both backends after
//!   compaction.
//! - [`SpawnConfig`] / [`SessionOutcome`] — the contract between loom and
//!   `wrix spawn`.
//!
//! Protocol-parsing of backend-specific message types (`PiMessage`,
//! `ClaudeMessage`) lives in `agent`, not here.

mod backend;
mod error;
mod jsonl;
mod kind;
mod parse;
mod repin;
mod session;

pub use backend::{
    AgentBackend, DEFAULT_HANDSHAKE_TIMEOUT_SECS, DEFAULT_STALL_WARN_SECS, LOOM_INSIDE_ENV,
    ModelSelection, MountSpec, OutputLimits, SessionOutcome, SpawnConfig, ThinkingLevel,
    set_loom_inside,
};
pub use error::ProtocolError;
pub use jsonl::{JsonlReader, MAX_LINE_BYTES};
pub use kind::{AgentKind, AgentRuntime, ParseAgentRuntimeError};
pub use parse::{LineParse, ParsedLine};
pub use repin::RePinContent;
pub use session::{Active, AgentSession, Idle};

/// `AgentEvent` + `CompactionReason` live in `loom-events` now, since
/// they're part of the public contract leaf consumers depend on. The
/// driver re-exports them so the `agent::event::AgentEvent` path keeps
/// resolving for existing call sites.
pub mod event {
    pub use loom_events::event::*;
}
pub use loom_events::event::{AgentEvent, CompactionReason};

/// The agent-driver contract — `Session`, `EventSink`, and friends —
/// lives in `loom-events` so frontends and external `llm`
/// consumers can implement and compose sinks without pulling in the
/// driver runtime. Re-exported here so existing call sites resolve.
pub use loom_events::{
    EventSink, EventSinkExt, EventStream, Session, SessionCommand, SessionMode, TeeSink,
};
