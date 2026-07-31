//! Built-in event observers for conversation safety and usage signals.

pub mod doom_loop;
pub mod duplicate_result;
pub mod result_hasher;

pub use doom_loop::{DoomLoopConfig, DoomLoopObserver, DoomLoopStage, DoomLoopTripped};
pub use duplicate_result::{DuplicateDetection, DuplicateResultConfig, DuplicateResultObserver};
