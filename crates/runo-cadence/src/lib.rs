//! `CadenceSource`: fixed cadences in Core, read from the corpus when it has enough; Aegloss
//! Later behind the same trait (Song spec §5; technical spec §13).

use std::time::Duration;

use runo_core::{Cadence, Millis, Promise};

/// A source of promised cadences. Provides, never sings.
pub trait CadenceSource: Send + Sync {
    /// The promise for a unit type in a state and area.
    fn promise(&self, unit: &str, state: &str, area: &str) -> Promise;
    /// A short version string carried in every promise.
    fn version(&self) -> String;
}

/// Fixed cadences from the profile, with the Core escalation 1x, 1.5x, 2x.
#[derive(Debug, Clone)]
pub struct FixedCadence {
    /// Expected duration per state.
    pub per_state: std::collections::BTreeMap<String, Duration>,
    /// Escalation multipliers in tenths: Core is `[10, 15, 20]` for 1x, 1.5x, 2x. Integer on
    /// purpose; the wire carries absolute durations, never floats (technical spec review).
    pub escalate_tenths: [u64; 3],
}

impl FixedCadence {
    fn cadence_for(&self, expected: Duration) -> Cadence {
        let base = u64::try_from(expected.as_millis()).unwrap_or(u64::MAX) / 10;
        let ms = |tenths: u64| Millis(base.saturating_mul(tenths));
        Cadence {
            call: ms(self.escalate_tenths[0]),
            again: ms(self.escalate_tenths[1]),
            suspect: ms(self.escalate_tenths[2]),
        }
    }
}

impl CadenceSource for FixedCadence {
    fn promise(&self, unit: &str, state: &str, _area: &str) -> Promise {
        let expected = self
            .per_state
            .get(state)
            .copied()
            .unwrap_or(Duration::from_secs(30 * 60));
        Promise {
            unit: unit.to_owned(),
            cadence: self.cadence_for(expected),
            checkpoints: true,
            source: self.version(),
        }
    }

    fn version(&self) -> String {
        "fixed-v1".to_owned()
    }
}

/// Cadences read from the pipeline's own corpus: the last ten closed chains of the same unit
/// and area, falling back to `FixedCadence` below ten. Implemented in milestone 4.
#[derive(Debug)]
pub struct CorpusCadence {
    /// Fallback.
    pub fallback: FixedCadence,
}

impl CadenceSource for CorpusCadence {
    fn promise(&self, unit: &str, state: &str, area: &str) -> Promise {
        // Milestone 4: read tomes for (unit, area); below ten, fall back.
        self.fallback.promise(unit, state, area)
    }

    fn version(&self) -> String {
        "corpus-v0".to_owned()
    }
}
