//! Claude Code stream-json backend.
//!
//! Three submodules split the wire surface from the process surface:
//!
//! - [`messages`] — typed protocol messages (the `ClaudeMessage` tagged
//!   enum that mirrors `claude --output-format stream-json`).
//! - [`parser`] — `LineParse` impl that turns JSONL lines from claude's
//!   stdout into [`AgentEvent`](loom_driver::agent::AgentEvent)s and encodes
//!   driver-side stream-json user messages (initial prompt, steering).
//! - [`backend`] — the [`ClaudeBackend`] zero-sized type plus its
//!   [`AgentBackend`](loom_driver::agent::AgentBackend) impl.

pub mod backend;
pub mod messages;
pub mod parser;

pub use backend::ClaudeBackend;
