//! Agent backend implementations for Loom.
//!
//! Houses three zero-sized backends — [`PiBackend`] for pi-mono RPC,
//! [`ClaudeBackend`] for Claude Code stream-json, and [`DirectBackend`]
//! for the in-container `loom-direct-runner` — that implement the
//! [`AgentBackend`](loom_driver::agent::AgentBackend) trait declared in
//! `loom-driver`. The trait's job is process lifecycle only; conversation
//! driving (prompt, steer, abort, event streaming) lives on
//! [`AgentSession`](loom_driver::agent::AgentSession).

pub mod claude;
pub mod direct;
pub mod pi;

pub use claude::ClaudeBackend;
pub use direct::DirectBackend;
pub use pi::PiBackend;
