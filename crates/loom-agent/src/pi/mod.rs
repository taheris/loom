//! Pi-mono RPC backend (`pi --mode rpc`).
//!
//! Three submodules split the wire surface from the process surface:
//!
//! - [`messages`] — typed protocol messages (envelope, response, event,
//!   extension UI request, command bodies).
//! - [`parser`] — `LineParse` impl that turns JSONL lines from pi's stdout
//!   into [`AgentEvent`](loom_driver::agent::AgentEvent)s and encodes
//!   driver-side commands.
//! - [`backend`] — the [`PiBackend`] zero-sized type plus its
//!   [`AgentBackend`](loom_driver::agent::AgentBackend) impl.

pub mod backend;
pub mod messages;
pub mod parser;

pub use backend::PiBackend;
