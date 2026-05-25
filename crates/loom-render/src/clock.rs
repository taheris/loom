//! Minimal monotonic-clock surface the renderer needs.
//!
//! `loom-driver` ships a richer [`Clock`](loom_driver::clock::Clock) trait
//! that also exposes async `sleep` / `timeout` via `tokio`. The renderer
//! only needs the monotonic `now()` instant for the elapsed-time line at
//! finish, so this crate defines its own tiny trait and stays free of the
//! `tokio` dep. `loom-driver`'s `SystemClock` implements this trait too,
//! which lets the driver hand the same clock object to both surfaces.

use std::sync::Arc;
use std::time::Instant;

/// Source of monotonic `Instant`s used by the renderer's elapsed-time line.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Instant;
}

/// Default implementation backed by `std::time::Instant::now`. Tests in
/// `loom-driver` substitute the driver-side `MockClock` (which also impls
/// this trait); the renderer never knows which one is in flight.
pub struct SystemClock;

impl SystemClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
