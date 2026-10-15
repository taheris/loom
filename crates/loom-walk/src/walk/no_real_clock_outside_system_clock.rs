//! Determinism audit for direct host-clock reads.

use super::clock_audit::{self, Operation};
use super::{Verdict, WalkInput};

const RULE: &str = "no_real_clock_outside_system_clock — inject Clock; only clock implementations and exact bounded test exceptions may read host time";

fn rule(input: &WalkInput) -> Verdict {
    clock_audit::run(input, Operation::RealClockRead, RULE)
}

/// Standalone and checked dispatch execute the identical rule body.
pub fn run(input: &WalkInput) -> Verdict {
    input.finish(rule(input))
}

super::source::registration!("no_real_clock_outside_system_clock");
