//! Agent-loop observer module. Both observers implement the
//! `EventSink` trait defined in `loom-events` and are composed into
//! `Conversation`'s default sink chain so consumers driving via
//! `Conversation::run` get the safety nets out of the box; Loom's
//! binary composes the same observers when driving Pi / Claude /
//! Direct backends.

pub mod doom_loop;
pub mod duplicate_result;
mod result_hasher;

pub use doom_loop::{DoomLoopConfig, DoomLoopObserver, DoomLoopTripped};
pub use duplicate_result::{DuplicateDetection, DuplicateResultConfig, DuplicateResultObserver};
