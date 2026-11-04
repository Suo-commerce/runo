//! Builders, a fixture chorus, `SimClock`, `FailurePlan` and the conformance suites. A
//! dev-dependency everywhere; never ships in a binary. The one home for test fixtures.

pub mod conformance;
pub mod faults;
pub mod fixture;

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

/// A signed refrain for a lane's chain at epoch 0.
///
/// # Panics
/// If encoding fails, which it cannot for fixture values.
#[must_use]
pub fn refrain(
    chorus: &fixture::Chorus,
    singer: &runo_core::SingerId,
    task: &runo_core::TaskId,
    last: runo_core::VerseId,
    n: u64,
) -> runo_core::Refrain {
    use ed25519_dalek::Signer;
    let mut r = runo_core::Refrain {
        singer: singer.clone(),
        task: task.clone(),
        epoch: runo_core::Epoch(0),
        n,
        last,
        progress: runo_core::Progress::default(),
        sent_at: runo_core::Timestamp("2026-11-03T12:00:00Z".into()),
        sig: [0; 64],
    };
    let bytes = runo_core::codec::canonical(&r).expect("fixture encodes");
    r.sig = chorus.key(singer).sign(&bytes).to_bytes();
    r
}
