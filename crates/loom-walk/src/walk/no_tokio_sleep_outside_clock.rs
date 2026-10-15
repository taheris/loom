//! Determinism audit for direct tokio sleeps.

use super::clock_audit::{self, Operation};
use super::{Verdict, WalkInput};

const RULE: &str = "no_tokio_sleep_outside_clock — direct tokio sleeps require a clock implementation, paused test, or exact bounded exception";

fn rule(input: &WalkInput) -> Verdict {
    clock_audit::run(input, Operation::TokioSleep, RULE)
}

/// Standalone and checked dispatch execute the identical rule body.
pub fn run(input: &WalkInput) -> Verdict {
    input.finish(rule(input))
}

super::source::registration!("no_tokio_sleep_outside_clock");
