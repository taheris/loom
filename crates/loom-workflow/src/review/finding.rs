//! Re-export of the typed walk-output contract owned by
//! [`loom_protocol::gate`].
//!
//! The canonical Rust home for the wire format is
//! `crates/loom-protocol/src/gate.rs` per `specs/gate.md`
//! § *Canonical contract location*. This module re-exports the public
//! surface so existing call sites that imported from
//! `loom_workflow::review::finding` continue to compile unchanged.
//!
//! [`WalkOutput`]'s fields are private at the `loom-protocol` crate
//! boundary; consumers read state via [`WalkOutput::terminal`] /
//! [`WalkOutput::findings`] / [`WalkOutput::finding_errors`] accessors.

pub use loom_protocol::gate::{
    ConcernToken, Finding, FindingParseError, FindingTarget, FindingValidator, LOOM_FINDING_PREFIX,
    TargetKind, TerminalSurface, WalkOutput, WalkOutputError, parse_walk_output,
};
