//! Builders, a fixture chorus, `SimClock`, `FailurePlan` and the conformance suites. A
//! dev-dependency everywhere; never ships in a binary. The one home for test fixtures.

pub mod conformance;

use std::sync::atomic::{AtomicU64, Ordering};

use runo_core::time::{Clock, Instant};

/// A clock the simulator advances by hand.
#[derive(Debug, Default)]
pub struct SimClock {
    now: AtomicU64,
}

impl SimClock {
    /// Advance by `nanos`.
    pub fn advance(&self, nanos: u64) {
        self.now.fetch_add(nanos, Ordering::SeqCst);
    }
}

impl Clock for SimClock {
    fn now(&self) -> Instant {
        Instant(self.now.load(Ordering::SeqCst))
    }
}

/// Which failures a `MemStore` or simulated transport should inject.
#[derive(Debug, Default, Clone)]
pub struct FailurePlan {
    /// Fail the next N appends.
    pub fail_appends: u32,
    /// Drop every k-th refrain.
    pub drop_refrain_every: Option<u32>,
    /// Forget fences on "restart" (a bug the tests must catch).
    pub forget_fences_on_restart: bool,
}

/// A fresh Ed25519 signing key for a fixture voice.
#[must_use]
pub fn fixture_key(seed: u8) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
}
