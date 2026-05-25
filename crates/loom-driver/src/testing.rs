//! Test-only helpers exposed to downstream crates.
//!
//! Anything in this module is intentionally part of the public surface so
//! integration tests under `crates/*/tests/` can reuse it without
//! re-implementing scaffolding. Production code should not reach here.

use std::future::Future;

use crate::clock::MockClock;

/// Run `f` with a fresh [`MockClock`].
///
/// Convenience scaffolding for tests that just need a one-line setup:
///
/// ```
/// use loom_driver::clock::Clock;
/// use loom_driver::testing::with_mock_clock;
/// use std::time::Duration;
///
/// #[tokio::test(start_paused = true)]
/// async fn it_works() {
///     let elapsed = with_mock_clock(|clock| async move {
///         let start = clock.now();
///         clock.sleep(Duration::from_secs(5)).await;
///         clock.now().saturating_duration_since(start)
///     })
///     .await;
///     assert!(elapsed >= Duration::from_secs(5));
/// }
/// ```
pub async fn with_mock_clock<F, Fut, T>(f: F) -> T
where
    F: FnOnce(MockClock) -> Fut,
    Fut: Future<Output = T>,
{
    f(MockClock::new()).await
}
