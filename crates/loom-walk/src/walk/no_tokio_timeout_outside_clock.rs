//! Determinism audit for direct tokio timeouts.

use super::clock_audit::{self, Operation};
use super::{Verdict, WalkInput};

const RULE: &str = "no_tokio_timeout_outside_clock — direct tokio timeouts require a clock implementation, paused test, or exact bounded exception";

pub fn run(input: &WalkInput) -> Verdict {
    clock_audit::run(input, Operation::TokioTimeout, RULE)
}
