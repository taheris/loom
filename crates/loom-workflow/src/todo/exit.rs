//! Compatibility names for canonical messages and the phase-aware decoding adapter.
//! [`ExitSignal`] is an alias of [`loom_protocol::output::Message`], not a terminal vocabulary.

pub use loom_protocol::gate::{ExitSignal, parse_exit_signal};
