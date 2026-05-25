//! `loom-render` — terminal renderer + log sink + path helpers.
//!
//! Sits between `loom-events` (public event contract) and the
//! `loom-driver` runtime. `loom-driver` opens a [`LogSink`] per bead
//! spawn; the sink tees the [`AgentEvent`](loom_events::AgentEvent)
//! stream into a per-bead JSONL file and the [`TerminalRenderer`] that
//! draws progress lines on stdout.
//!
//! The crate is **runtime-agnostic**: synchronous I/O, no `tokio`, no
//! `rusqlite`, no `gix`. The disk-retention sweep (which uses async
//! filesystem APIs for parallelism) stays in `loom-driver`. Consumers
//! that want the renderer without the driver — `loom logs`, SSE
//! bridges, external log analyzers — depend on this crate directly.

pub mod clock;
pub mod in_place;
pub mod osc8;
mod path;
mod redacted;
mod renderer;
mod sink;
mod time;
pub mod tool_body;

pub use clock::{Clock, SystemClock};
pub use path::{bead_log_path, format_utc_timestamp, phase_log_path};
pub use redacted::Redacted;
pub use renderer::{
    BeadOutcome, JsonRenderer, PlainRenderer, PrettyRenderer, RawRenderer, RenderMode, Renderer,
    TerminalRenderer, build_renderer,
};
pub use sink::{LogError, LogSink};
