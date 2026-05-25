//! Disk-retention sweep for log files plus re-exports from `loom-render`.
//!
//! The renderer, log sink, and path helpers now live in `loom-render`
//! (F3, wx-9y7cq) — keeping them out of the driver lets `loom logs`,
//! SSE bridges, and external log analyzers depend on the renderer
//! without pulling in the driver runtime. This module re-exports them
//! so existing call sites (`use loom_driver::logging::LogSink`) keep
//! resolving.
//!
//! `sweep_retention_at` stays driver-side — it scans the filesystem,
//! deletes stale files, and uses `tracing` for failure reporting; it
//! is the one logging concern that's tied to the driver runtime rather
//! than to event rendering.

mod retention;

pub use retention::{RetentionReport, sweep_retention_at};

// Re-export from loom-render so the driver's logging module surface is
// unchanged for existing call sites.
pub use loom_render::{
    BeadOutcome, LogError, LogSink, Redacted, RenderMode, TerminalRenderer, bead_log_path,
    format_utc_timestamp, phase_log_path,
};
