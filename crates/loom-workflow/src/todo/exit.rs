//! Re-export of [`ExitSignal`] / [`parse_exit_signal`] owned by
//! [`loom_protocol::gate`].
//!
//! The canonical Rust home for the marker-surface wire format is
//! `crates/loom-protocol/src/gate.rs` per `specs/gate.md`
//! § *Canonical contract location*. This module re-exports so
//! existing call sites that imported from `loom_workflow::todo::exit`
//! continue to compile unchanged.

pub use loom_protocol::gate::{ExitSignal, parse_exit_signal};
