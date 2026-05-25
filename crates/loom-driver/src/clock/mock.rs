use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{BoxFuture, Clock};

/// Test clock backed by tokio's paused-time runtime.
///
/// Use under `#[tokio::test(start_paused = true)]`: tokio's runtime
/// auto-advances paused time when all tasks are blocked on a timer, so
/// `MockClock::sleep(d).await` resolves deterministically without real
/// wall-clock waits. [`MockClock::now`] reports the current paused-time
/// instant so callers see [`Clock::sleep`] advances reflected in
/// successive `now()` reads.
///
/// To advance time manually (for tests that drive a polling loop past a
/// deadline without a pending sleep), call `tokio::time::advance(d).await`
/// directly — that helper is gated behind tokio's `test-util` feature,
/// which downstream test targets already enable.
///
/// Constructible via [`MockClock::new`] or
/// [`crate::testing::with_mock_clock`].
#[derive(Debug, Default, Clone, Copy)]
pub struct MockClock;

impl MockClock {
    pub const fn new() -> Self {
        Self
    }
}

impl Clock for MockClock {
    fn now(&self) -> Instant {
        // tokio's instant respects start_paused / advance; converting to
        // std::time::Instant preserves the paused-time view at the public
        // surface.
        tokio::time::Instant::now().into_std()
    }

    fn wall_now(&self) -> SystemTime {
        UNIX_EPOCH
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::pending;

    #[tokio::test(start_paused = true)]
    async fn now_advances_under_paused_runtime() {
        let clock = MockClock::new();
        let start = clock.now();
        clock.sleep(Duration::from_secs(60)).await;
        let elapsed = clock.now().saturating_duration_since(start);
        assert!(
            elapsed >= Duration::from_secs(60),
            "expected >=60s of paused-time advance, got {elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn manual_advance_moves_clock_without_pending_sleep() {
        let clock = MockClock::new();
        let start = clock.now();
        tokio::time::advance(Duration::from_secs(3600)).await;
        let elapsed = clock.now().saturating_duration_since(start);
        assert!(
            elapsed >= Duration::from_secs(3600),
            "expected >=1h of paused-time advance, got {elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_fires_when_future_does_not_complete() {
        let clock = MockClock::new();
        let result = clock.timeout(Duration::from_secs(5), pending::<()>()).await;
        assert!(result.is_err(), "pending future should time out");
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_returns_value_when_future_completes_first() {
        let clock = MockClock::new();
        let result = clock.timeout(Duration::from_secs(60), async { 42 }).await;
        assert_eq!(result.expect("future should complete"), 42);
    }

    #[tokio::test(start_paused = true)]
    async fn clone_is_independent_handle_to_same_paused_runtime() {
        let a = MockClock::new();
        let b = a;
        let start = a.now();
        b.sleep(Duration::from_secs(10)).await;
        // Both observe the same paused-time view.
        assert!(a.now().saturating_duration_since(start) >= Duration::from_secs(10));
        assert!(b.now().saturating_duration_since(start) >= Duration::from_secs(10));
    }
}
