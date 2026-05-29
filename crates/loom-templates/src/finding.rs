//! Re-export of the typed [`Finding`] contract owned by
//! [`loom_protocol::gate`].
//!
//! The canonical Rust home for the wire format is
//! `crates/loom-protocol/src/gate.rs` per `specs/gate.md`
//! § *Canonical contract location*. This module re-exports the public
//! surface so existing call sites that imported from
//! `loom_templates::finding` continue to compile unchanged — the typed
//! retry-context surface (`PreviousFailure::ReviewConcern { findings:
//! Vec<Finding> }`) carries [`Finding`] as a field and the templates
//! crate threads it through without re-declaring the type.

pub use loom_protocol::gate::{
    ConcernToken, Finding, FindingParseError, FindingTarget, FindingValidator, LOOM_FINDING_PREFIX,
    TargetKind,
};
