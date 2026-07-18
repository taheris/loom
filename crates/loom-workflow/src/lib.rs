//! Loom workflow engine.
//!
//! Implements the workflow phases (`plan`, `todo`, `loop`, `check`, `inbox`,
//! `spec`) on top of `loom-driver`'s typed surface and `templates`'
//! Askama-rendered prompts. Subsequent issues populate each phase module;
//! this crate currently exposes the skeleton only.
//!
//! The agent surface from `loom-driver` (`AgentBackend`, `AgentEvent`,
//! `AgentSession`, `RePinContent`, `SpawnConfig`) is re-exported through
//! this module index so workflow phases can import the symbols without
//! depending on `loom-driver` directly each time.

pub mod agent;
mod agent_input;
mod event_log;
pub mod gate_clarify;
pub mod inbox;
pub mod init;
pub mod logs_cmd;
pub mod r#loop;
pub mod mint;
pub mod observer;
mod pi_tui;
pub mod plan;
pub mod resolve;
pub mod review;
pub mod skill;
pub mod spawn;
pub mod spec;
pub mod status;
mod suppression;
pub mod todo;
pub mod tune;
pub mod use_spec;

pub use agent::{run_agent, run_agent_classified};
pub use loom_driver::agent::{
    Active, AgentBackend, AgentEvent, AgentKind, AgentSession, CompactionReason, Idle, JsonlReader,
    LineParse, MAX_LINE_BYTES, ParsedLine, ProtocolError, RePinContent, SessionOutcome,
    SpawnConfig,
};
pub use observer::{DefaultObserverChain, ObserverDriverEvent};

#[cfg(test)]
mod tests;
