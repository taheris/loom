use std::future::Future;
use std::time::{Duration, Instant, SystemTime};

use super::{BoxFuture, Clock, Elapsed};

/// Production clock — delegates to the OS clock and tokio's real timers.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl SystemClock {
    pub const fn new() -> Self {
        Self
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wall_now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        Box::pin(tokio::time::sleep(duration))
    }

    fn timeout<'a, F>(
        &'a self,
        duration: Duration,
        future: F,
    ) -> BoxFuture<'a, Result<F::Output, Elapsed>>
    where
        Self: Sized,
        F: Future + Send + 'a,
        F::Output: Send,
    {
        Box::pin(async move {
            tokio::time::timeout(duration, future)
                .await
                .map_err(|_| Elapsed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::pending;

    #[tokio::test]
    async fn now_is_monotonic() {
        let clock = SystemClock::new();
        let a = clock.now();
        let b = clock.now();
        assert!(b >= a);
    }

    #[tokio::test(start_paused = true)]
    async fn sleep_returns_after_duration_under_paused_runtime() {
        let clock = SystemClock::new();
        // SystemClock::now() reads real wall time, so we measure progress
        // via tokio's paused instant rather than `clock.now()` here.
        let start = tokio::time::Instant::now();
        clock.sleep(Duration::from_secs(60)).await;
        let elapsed = tokio::time::Instant::now() - start;
        assert!(
            elapsed >= Duration::from_secs(60),
            "expected >=60s of paused-time advance, got {elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_fires_when_future_does_not_complete() {
        let clock = SystemClock::new();
        let result = clock.timeout(Duration::from_secs(5), pending::<()>()).await;
        assert_eq!(result, Err(Elapsed));
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_returns_value_when_future_completes_first() {
        let clock = SystemClock::new();
        let result = clock.timeout(Duration::from_secs(60), async { 42 }).await;
        assert_eq!(result.expect("future should complete"), 42);
    }
}
