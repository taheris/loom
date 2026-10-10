//! Finite acceptance campaigns using the upstream Connect runner and comparison.

use std::cell::Cell;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use displaydoc::Display;
use loom_driver::clock::{Clock, SystemClock};
use quint_connect::{Driver, State, Step, runner};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFINITION: &str = include_str!("../../../../tests/fixtures/quint/campaign.json");

#[derive(Debug, Display, Error)]
pub enum Error {
    /// invalid campaign definition
    Definition(#[from] serde_json::Error),
    /// unsupported campaign or tool identity
    Unsupported,
    /// incomplete verification: {detail}
    Incomplete { detail: &'static str },
    /// Quint generation, adapter, or state comparison failed
    Connect(#[source] anyhow::Error),
    /// failed to read campaign input {path:?}
    Input {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Copy, Debug, Serialize)]
pub enum Mode {
    Routine,
    Deeper,
    Replay,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(try_from = "RawPlan")]
pub struct Plan {
    seeds: Vec<NonZeroU64>,
    traces: NonZeroUsize,
    steps: NonZeroUsize,
    seconds: NonZeroU64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlan {
    seeds: Vec<NonZeroU64>,
    traces: NonZeroUsize,
    steps: NonZeroUsize,
    seconds: NonZeroU64,
}

impl TryFrom<RawPlan> for Plan {
    type Error = Error;

    fn try_from(raw: RawPlan) -> Result<Self, Self::Error> {
        let seeds = u64::try_from(raw.seeds.len()).map_err(|_| Error::Unsupported)?;
        if raw.seeds.is_empty()
            || raw
                .seeds
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != raw.seeds.len()
            || raw
                .steps
                .get()
                .checked_add(1)
                .and_then(|states| states.checked_mul(raw.traces.get()))
                .is_none()
            || seeds.checked_mul(raw.seconds.get()).is_none()
        {
            return Err(Error::Incomplete {
                detail: "empty, duplicate, or overflowing campaign",
            });
        }
        Ok(Self {
            seeds: raw.seeds,
            traces: raw.traces,
            steps: raw.steps,
            seconds: raw.seconds,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    version: u32,
    quint: String,
    connect: String,
    evaluator: String,
    apalache: String,
    property: String,
    routine: Plan,
    deeper: Plan,
    replay: Plan,
}

impl Plan {
    pub fn shipped(mode: Mode) -> Result<Self, Error> {
        let definition: Definition = serde_json::from_str(DEFINITION)?;
        if definition.version != 1
            || definition.quint != "0.32.0"
            || definition.connect != "0.1.2"
            || definition.evaluator != "0.6.0"
            || definition.apalache != "0.56.1"
            || definition.property != "safety"
        {
            return Err(Error::Unsupported);
        }
        Ok(match mode {
            Mode::Routine => definition.routine,
            Mode::Deeper => definition.deeper,
            Mode::Replay => definition.replay,
        })
    }

    pub const fn seconds(&self) -> u64 {
        self.seconds.get()
    }

    pub fn total_seconds(&self) -> Result<u64, Error> {
        self.seconds()
            .checked_mul(u64::try_from(self.seeds.len()).map_err(|_| Error::Unsupported)?)
            .ok_or(Error::Unsupported)
    }

    /// Identity includes the complete acceptance input bundle, not machine paths.
    pub fn identity(&self, root: &Path, mode: Mode) -> Result<blake3::Hash, Error> {
        let mut hash = blake3::Hasher::new();
        hash.update(&serde_json::to_vec(&(mode, self))?);
        for path in [
            "Cargo.lock",
            "tests/fixtures/quint/campaign.json",
            "tests/fixtures/quint/bridge.qnt",
            "crates/loom-gate/tests/quint/campaign.rs",
            "crates/loom-gate/tests/quint/bridge.rs",
            "crates/loom-gate/examples/quint.rs",
            "crates/loom-gate/src/runner.rs",
            "crates/loom-driver/src/clock/mod.rs",
            "crates/loom-driver/src/clock/system.rs",
            "scripts/test-quint.sh",
            "tests/quint/harness.sh",
            "nix/flake/quint.nix",
            "nix/workspace.nix",
            "flake.nix",
            "flake.lock",
        ] {
            let input = std::fs::read(root.join(path)).map_err(|source| Error::Input {
                path: root.join(path),
                source,
            })?;
            hash.update(path.as_bytes());
            hash.update(&input.len().to_le_bytes());
            hash.update(&input);
        }
        Ok(hash.finalize())
    }
}

#[derive(Clone, Copy, Default, Debug, Serialize)]
struct Progress {
    traces: usize,
    current_states: usize,
    states: usize,
}

struct Budget {
    expected_traces: usize,
    states_per_trace: usize,
    state_limit: usize,
    deadline: Duration,
    start: Instant,
    clock: Rc<dyn Clock>,
    progress: Cell<Progress>,
}

impl Budget {
    fn new(plan: &Plan, clock: Rc<dyn Clock>) -> Result<Self, Error> {
        let states_per_trace = plan.steps.get().checked_add(1).ok_or(Error::Unsupported)?;
        let state_limit = states_per_trace
            .checked_mul(plan.traces.get())
            .ok_or(Error::Unsupported)?;
        Ok(Self {
            expected_traces: plan.traces.get(),
            states_per_trace,
            state_limit,
            deadline: Duration::from_secs(plan.seconds()),
            start: clock.now(),
            clock,
            progress: Cell::new(Progress::default()),
        })
    }

    fn record(&self, init: bool) -> Result<(), Error> {
        let mut progress = self.progress.get();
        if self.clock.now().saturating_duration_since(self.start) >= self.deadline {
            return Err(Error::Incomplete {
                detail: "deadline interrupted declared work",
            });
        }
        if progress.states >= self.state_limit {
            return Err(Error::Incomplete {
                detail: "state guard interrupted declared work",
            });
        }
        if init {
            if progress.traces > 0 && progress.current_states != self.states_per_trace {
                return Err(Error::Incomplete {
                    detail: "short trace",
                });
            }
            if progress.traces >= self.expected_traces {
                return Err(Error::Incomplete {
                    detail: "unexpected extra trace",
                });
            }
            progress.traces += 1;
            progress.current_states = 0;
        } else if progress.traces == 0 {
            return Err(Error::Incomplete {
                detail: "trace missing named init",
            });
        }
        progress.states += 1;
        progress.current_states += 1;
        self.progress.set(progress);
        Ok(())
    }

    fn finish(&self) -> Result<Progress, Error> {
        let progress = self.progress.get();
        if self.clock.now().saturating_duration_since(self.start) >= self.deadline {
            return Err(Error::Incomplete {
                detail: "deadline expired before completion",
            });
        }
        if progress.traces != self.expected_traces
            || progress.current_states != self.states_per_trace
            || progress.states != self.expected_traces * self.states_per_trace
        {
            return Err(Error::Incomplete {
                detail: "empty or incomplete trace batch",
            });
        }
        Ok(progress)
    }
}

struct Counted<D> {
    driver: D,
    budget: Rc<Budget>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
struct Projection<S>(S);

impl<D: Driver> State<Counted<D>> for Projection<D::State> {
    fn from_driver(driver: &Counted<D>) -> quint_connect::Result<Self> {
        Ok(Self(D::State::from_driver(&driver.driver)?))
    }

    fn from_spec(value: itf::Value) -> quint_connect::Result<Self> {
        Ok(Self(D::State::from_spec(value)?))
    }
}

impl<D: Driver> Driver for Counted<D> {
    type State = Projection<D::State>;

    fn config() -> quint_connect::Config {
        D::config()
    }

    fn step(&mut self, step: &Step) -> quint_connect::Result {
        self.budget.record(step.action_taken == "init")?;
        self.driver.step(step)
    }
}

#[derive(Serialize)]
enum Assurance {
    #[serde(rename = "bounded simulation and bridge conformance, not whole-gate proof")]
    BoundedBridge,
}

#[derive(Serialize)]
enum Checker {
    #[serde(rename = "Quint 0.32.0 / Connect 0.1.2 / Rust evaluator 0.6.0 / Apalache 0.56.1")]
    Pinned,
}

#[derive(Serialize)]
enum Property {
    #[serde(rename = "bridge::safety (state equality)")]
    StateEquality,
}

#[derive(Serialize)]
pub struct Receipt {
    assurance: Assurance,
    checker: Checker,
    property: Property,
    #[serde(serialize_with = "serialize_identity")]
    identity: blake3::Hash,
    mode: Mode,
    plan: Plan,
    batches: Vec<Batch>,
}

#[derive(Serialize)]
struct Batch {
    seed: NonZeroU64,
    progress: Progress,
}

/// Every seed is one upstream generation process; every state uses upstream comparison.
pub fn run<D: Driver>(
    root: &Path,
    mode: Mode,
    make_driver: impl Fn() -> D,
) -> Result<Receipt, Error> {
    let plan = Plan::shipped(mode)?;
    let identity = plan.identity(root, mode)?;
    let model = root.join("tests/fixtures/quint/bridge.qnt");
    let mut batches = Vec::new();
    for seed in &plan.seeds {
        let budget = Rc::new(Budget::new(&plan, Rc::new(SystemClock))?);
        let driver = Counted {
            driver: make_driver(),
            budget: Rc::clone(&budget),
        };
        let config = runner::Config {
            test_name: format!("bridge/{mode:?}/seed={seed}/identity={identity}"),
            gen_config: runner::RunConfig {
                spec: model.to_string_lossy().into_owned(),
                main: Some("bridge".into()),
                init: Some("init".into()),
                step: Some(
                    if matches!(mode, Mode::Replay) {
                        "replayStep"
                    } else {
                        "step"
                    }
                    .into(),
                ),
                max_samples: Some(plan.traces.get()),
                max_steps: Some(plan.steps.get()),
                seed: seed.to_string(),
            },
        };
        runner::run_test(driver, config).map_err(Error::Connect)?;
        batches.push(Batch {
            seed: *seed,
            progress: budget.finish()?,
        });
    }
    Ok(Receipt {
        assurance: Assurance::BoundedBridge,
        checker: Checker::Pinned,
        property: Property::StateEquality,
        identity,
        mode,
        plan,
        batches,
    })
}

fn serialize_identity<S: serde::Serializer>(
    identity: &blake3::Hash,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&identity.to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::clock::MockClock;
    use loom_test_support::proptest_config;
    use proptest::prelude::*;

    fn budget(plan: &Plan) -> Budget {
        Budget::new(plan, Rc::new(MockClock)).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn quint_incomplete_verification_is_not_success() {
        for definition in [
            r#"{"seeds":[],"traces":1,"steps":1,"seconds":1}"#,
            r#"{"seeds":[42,42],"traces":1,"steps":1,"seconds":1}"#,
            r#"{"seeds":[42],"traces":0,"steps":1,"seconds":1}"#,
        ] {
            assert!(serde_json::from_str::<Plan>(definition).is_err());
        }
        let plan = Plan::shipped(Mode::Replay).unwrap();
        let budget = budget(&plan);
        assert!(matches!(budget.finish(), Err(Error::Incomplete { .. })));
        budget.record(true).unwrap();
        assert!(matches!(budget.finish(), Err(Error::Incomplete { .. })));
        assert!(matches!(budget.record(true), Err(Error::Incomplete { .. })));
    }

    #[tokio::test(start_paused = true)]
    async fn quint_campaign_budget_exhaustion_is_explicit() {
        let plan = Plan::shipped(Mode::Replay).unwrap();
        let completed = budget(&plan);
        completed.record(true).unwrap();
        completed.record(false).unwrap();
        assert_eq!(completed.finish().unwrap().states, completed.state_limit);
        assert!(matches!(
            completed.record(false),
            Err(Error::Incomplete { .. })
        ));
        let early = Budget {
            state_limit: 1,
            ..budget(&plan)
        };
        early.record(true).unwrap();
        assert!(matches!(early.record(false), Err(Error::Incomplete { .. })));
        assert!(early.finish().is_err());
        let expired = budget(&plan);
        expired.record(true).unwrap();
        tokio::time::advance(Duration::from_secs(plan.seconds())).await;
        assert!(matches!(
            expired.record(false),
            Err(Error::Incomplete { .. })
        ));
        assert!(expired.finish().is_err());
        let routine = Plan::shipped(Mode::Routine).unwrap();
        let deeper = Plan::shipped(Mode::Deeper).unwrap();
        assert_eq!(routine.total_seconds().unwrap(), 60);
        assert!(deeper.steps > routine.steps && deeper.traces > routine.traces);
        assert!(routine.seeds.iter().all(|seed| deeper.seeds.contains(seed)));
    }

    #[derive(Debug, PartialEq, Eq, Deserialize)]
    struct Shifted(u64);

    struct Custom;

    impl State<Custom> for Shifted {
        fn from_driver(_: &Custom) -> quint_connect::Result<Self> {
            Ok(Self(1))
        }

        fn from_spec(value: itf::Value) -> quint_connect::Result<Self> {
            Ok(Self(u64::deserialize(value)? + 1))
        }
    }

    impl Driver for Custom {
        type State = Shifted;

        fn step(&mut self, _: &Step) -> quint_connect::Result {
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn counted_adapter_preserves_custom_upstream_projection() {
        let model =
            <Projection<Shifted> as State<Counted<Custom>>>::from_spec(itf::Value::Number(0))
                .unwrap();
        let driver = Counted {
            driver: Custom,
            budget: Rc::new(budget(&Plan::shipped(Mode::Replay).unwrap())),
        };
        assert_eq!(model, Projection::from_driver(&driver).unwrap());
    }

    proptest! {
        #![proptest_config(proptest_config())]

        #[test]
        fn parsed_campaigns_admit_only_finite_nonempty_unique_batches(
            seeds in proptest::collection::vec(1u64..100, 0..8),
            traces in 0usize..10,
            steps in 0usize..10,
            seconds in 0u64..100,
        ) {
            let definition = serde_json::json!({"seeds": seeds, "traces": traces, "steps": steps, "seconds": seconds});
            let parsed = serde_json::from_value::<Plan>(definition);
            let unique = seeds.iter().collect::<std::collections::BTreeSet<_>>().len() == seeds.len();
            prop_assert_eq!(parsed.is_ok(), !seeds.is_empty() && unique && traces > 0 && steps > 0 && seconds > 0);
        }
    }

    #[test]
    fn seed_change_invalidates_reproducible_identity() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut plan = Plan::shipped(Mode::Routine).unwrap();
        let identity = plan.identity(&root, Mode::Routine).unwrap();
        assert_eq!(identity, plan.identity(&root, Mode::Routine).unwrap());
        plan.seeds[0] = NonZeroU64::new(43).unwrap();
        assert_ne!(identity, plan.identity(&root, Mode::Routine).unwrap());
        assert_ne!(identity, plan.identity(&root, Mode::Deeper).unwrap());
    }
}
