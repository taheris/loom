//! Manual logical time for real-process replay fixtures; OS scheduling never spends the budget.

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use loom_driver::clock::{BoxFuture, Clock, MockClock};
use tokio::sync::Notify;

pub struct ManualClock {
    origin: Instant,
    elapsed: Mutex<Duration>,
    changed: Notify,
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            origin: MockClock::new().now(),
            elapsed: Mutex::new(Duration::ZERO),
            changed: Notify::new(),
        }
    }

    pub fn advance(&self, duration: Duration) {
        *self.elapsed.lock().unwrap() += duration;
        self.changed.notify_waiters();
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.origin + *self.elapsed.lock().unwrap()
    }

    fn wall_now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + *self.elapsed.lock().unwrap()
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        let deadline = self.now() + duration;
        Box::pin(async move {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.now() >= deadline {
                    return;
                }
                changed.await;
            }
        })
    }
}

#[tokio::test]
async fn manual_clock_wakes_only_sleepers_whose_deadline_elapsed() {
    let clock = ManualClock::new();
    let start = clock.now();
    let mut short = clock.sleep(Duration::from_secs(10));
    let mut long = clock.sleep(Duration::from_secs(30));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(short.as_mut().poll(cx).is_pending()))
            .await
    );
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(long.as_mut().poll(cx).is_pending()))
            .await
    );
    clock.advance(Duration::from_secs(10));
    short.await;
    assert_eq!(clock.now().duration_since(start), Duration::from_secs(10));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(long.as_mut().poll(cx).is_pending()))
            .await
    );
    clock.advance(Duration::from_secs(20));
    long.await;
}
