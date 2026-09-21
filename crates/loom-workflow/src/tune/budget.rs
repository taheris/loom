use std::future::Future;
use std::time::{Duration, Instant};

use loom_driver::clock::Clock;
use loom_tune::config::ChecksConfig;

use super::TuneError;

/// One deadline and evaluation-call allowance shared by all candidate checks.
pub(super) struct Budget<'a> {
    clock: &'a dyn Clock,
    started: Instant,
    wall_limit: Duration,
    call_limit: usize,
    calls: usize,
}

impl<'a> Budget<'a> {
    pub(super) fn new(clock: &'a dyn Clock, config: &ChecksConfig) -> Self {
        Self {
            clock,
            started: clock.now(),
            wall_limit: Duration::from_secs(config.max_wall_time_secs),
            call_limit: config.max_llm_judge_calls,
            calls: 0,
        }
    }

    pub(super) fn remaining(&self) -> Result<Duration, TuneError> {
        self.wall_limit
            .checked_sub(self.clock.now().saturating_duration_since(self.started))
            .filter(|remaining| !remaining.is_zero())
            .ok_or(TuneError::WallTimeExceeded {
                limit_secs: self.wall_limit.as_secs(),
            })
    }

    pub(super) fn reserve_call(&mut self) -> Result<(), TuneError> {
        self.remaining()?;
        if self.calls >= self.call_limit {
            return Err(TuneError::JudgeCallsExceeded {
                limit: self.call_limit,
            });
        }
        self.calls += 1;
        Ok(())
    }

    pub(super) const fn clock(&self) -> &dyn Clock {
        self.clock
    }

    pub(super) async fn run<T>(
        &self,
        future: impl Future<Output = Result<T, TuneError>>,
    ) -> Result<T, TuneError> {
        let remaining = self.remaining()?;
        let result = tokio::select! {
            biased;
            () = self.clock.sleep(remaining) => Err(TuneError::WallTimeExceeded { limit_secs: self.wall_limit.as_secs() }),
            result = future => result,
        };
        self.remaining()?;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::clock::MockClock;

    #[tokio::test(start_paused = true)]
    async fn wall_budget_is_shared_and_cancels_in_flight_work() {
        let clock = MockClock::new();
        let config = ChecksConfig {
            max_wall_time_secs: 10,
            ..ChecksConfig::default()
        };
        let budget = Budget::new(&clock, &config);
        budget
            .run(async {
                clock.sleep(Duration::from_secs(6)).await;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(budget.remaining().unwrap(), Duration::from_secs(4));
        assert!(matches!(
            budget
                .run(async {
                    clock.sleep(Duration::from_secs(5)).await;
                    Ok(())
                })
                .await,
            Err(TuneError::WallTimeExceeded { limit_secs: 10 })
        ));
        assert!(budget.remaining().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn exhausted_budget_never_polls_another_check() {
        let clock = MockClock::new();
        let config = ChecksConfig {
            max_wall_time_secs: 0,
            ..ChecksConfig::default()
        };
        let budget = Budget::new(&clock, &config);
        let mut called = false;
        let result = budget
            .run(async {
                called = true;
                Ok(())
            })
            .await;
        assert!(matches!(
            result,
            Err(TuneError::WallTimeExceeded { limit_secs: 0 })
        ));
        assert!(!called);
    }

    #[test]
    fn evaluation_budget_counts_both_sides_without_refunding_failures() {
        let clock = MockClock::new();
        let config = ChecksConfig {
            max_llm_judge_calls: 2,
            ..ChecksConfig::default()
        };
        let mut budget = Budget::new(&clock, &config);
        budget.reserve_call().unwrap();
        budget.reserve_call().unwrap();
        assert!(matches!(
            budget.reserve_call(),
            Err(TuneError::JudgeCallsExceeded { limit: 2 })
        ));
    }
}
