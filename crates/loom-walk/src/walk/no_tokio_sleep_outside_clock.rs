//! Determinism audit for direct tokio sleeps.

use super::clock_audit::{self, Operation};
use super::{Verdict, WalkInput};

const RULE: &str = "no_tokio_sleep_outside_clock — direct tokio sleeps require a clock implementation, paused test, or exact bounded exception";

pub fn run(input: &WalkInput) -> Verdict {
    clock_audit::run(input, Operation::TokioSleep, RULE)
}
