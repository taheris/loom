//! Determinism audit for real thread sleeps.

use super::clock_audit::{self, Operation};
use super::{Verdict, WalkInput};

const RULE: &str =
    "no_thread_sleep — inject Clock; only exact bounded test exceptions may use host sleeps";

pub fn run(input: &WalkInput) -> Verdict {
    clock_audit::run(input, Operation::ThreadSleep, RULE)
}
