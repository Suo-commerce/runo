//! Time as a trait, so the same code runs under the deterministic simulator.
//! Wall time appears only in `sung_at` and in the view.

use std::time::Duration;

pub mod wall;

/// A monotonic instant in nanoseconds, from a [`Clock`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Instant(pub u64);

impl Instant {
    /// Time elapsed since an earlier instant; zero if `earlier` is later.
    #[must_use]
    pub fn since(self, earlier: Instant) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }

    /// This instant plus a duration.
    #[must_use]
    pub fn plus(self, d: Duration) -> Instant {
        Instant(
            self.0
                .saturating_add(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)),
        )
    }
}

/// A source of monotonic time.
pub trait Clock: Send + Sync {
    /// The current instant.
    fn now(&self) -> Instant;
}

/// The standard library's monotonic clock.
#[derive(Debug)]
pub struct MonotonicClock {
    start: std::time::Instant,
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self {
            start: std::time::Instant::now(),
        }
    }
}

impl Clock for MonotonicClock {
    fn now(&self) -> Instant {
        Instant(u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX))
    }
}
