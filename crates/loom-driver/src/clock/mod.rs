//! Abstraction over the system clock and async timers.
//!
//! Production code uses [`SystemClock`]; tests use [`MockClock`] under
//! `#[tokio::test(start_paused = true)]` so wall-clock dependence stays out
//! of unit tests. Components that touch time accept `&dyn Clock` or
//! `impl Clock` so a deterministic implementation can be substituted in
//! tests without per-call-site refactoring.
//!
//! # Filesystem mtime in tests
//!
//! Functions that compare against external filesystem timestamps (e.g. the
//! log retention sweep against file mtime) take `now: Instant` as a
//! parameter rather than calling [`Clock::now`] internally. Tests set file
//! mtimes via the [`filetime`] crate to express "this file is N days old"
//! without sleeping or aging real wall time.
//!
//! [`filetime`]: https://crates.io/crates/filetime

mod mock;
mod system;

pub use mock::MockClock;
pub use system::SystemClock;

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant, SystemTime};

/// Returned by [`Clock::timeout`] when the deadline elapses before the
/// inner future resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("operation timed out")]
pub struct Elapsed;

/// Boxed-future alias used by [`Clock::sleep`] and [`Clock::timeout`] so
/// the trait can stay object-safe without pulling in `async_trait`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Time-related operations factored out so production and tests can share
/// the same call sites.
///
/// Implementations are `Send + Sync + 'static` so a clock can be shared via
/// `Arc<dyn Clock>` and crossed across task boundaries.
pub trait Clock: Send + Sync + 'static {
    /// Current monotonic instant. [`SystemClock::now`] delegates to
    /// `std::time::Instant::now`; [`MockClock::now`] returns a tokio paused
    /// instant so successive calls reflect [`Clock::sleep`] advances under
    /// `#[tokio::test(start_paused = true)]`.
    fn now(&self) -> Instant;

    /// Current wall-clock [`SystemTime`]. Used for filenames or other
    /// surfaces that need to round-trip through the OS clock (e.g. log file
    /// paths). [`SystemClock::wall_now`] delegates to
    /// `std::time::SystemTime::now`; [`MockClock::wall_now`] returns
    /// [`UNIX_EPOCH`](std::time::UNIX_EPOCH) so deterministic tests stay
    /// pinned to a known stamp.
    fn wall_now(&self) -> SystemTime;

    /// Resolve after at least `duration` has elapsed on this clock.
    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()>;

    /// Run `future` to completion, or return [`Elapsed`] if `duration`
    /// elapses first.
    ///
    /// `Self: Sized` keeps the rest of the trait object-safe; callers that
    /// only have `&dyn Clock` should accept `<C: Clock>` instead so they
    /// can call `timeout`. The default implementation uses [`Clock::sleep`]
    /// in a `tokio::select!`; concrete impls (notably [`SystemClock`])
    /// override only when a more direct timer integration matters.
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
            tokio::select! {
                out = future => Ok(out),
                () = self.sleep(duration) => Err(Elapsed),
            }
        })
    }
}
