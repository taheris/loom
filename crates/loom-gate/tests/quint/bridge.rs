//! Compatibility smoke for named actions, nondeterministic arguments, and production observations.

use loom_gate::runner::check_zero_match;
use quint_connect::{Driver, State, Step, switch};
use serde::Deserialize;

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct Observation {
    pub count: usize,
    pub accepted: bool,
}

impl State<Adapter> for Observation {
    fn from_driver(driver: &Adapter) -> quint_connect::Result<Self> {
        Ok(Self {
            count: driver.count,
            accepted: driver.accepted,
        })
    }
}

#[derive(Default)]
pub struct Adapter {
    count: usize,
    accepted: bool,
}

impl Adapter {
    fn observe(&mut self, tests: usize) {
        self.count = tests;
        let output = format!("running {tests} tests\ntest result: ok. {tests} passed; 0 failed");
        self.accepted = check_zero_match("cargo test", &output, "").is_ok();
    }
}

impl Driver for Adapter {
    type State = Observation;

    fn step(&mut self, step: &Step) -> quint_connect::Result {
        switch!(step {
            init => self.observe(0),
            observe(tests: usize) => self.observe(tests),
            replayObserve(tests: usize) => self.observe(tests),
        })
    }
}
