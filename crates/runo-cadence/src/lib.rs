//! `CadenceSource`: fixed cadences in Core, read from the corpus when it has enough; Aegloss
//! Later behind the same trait (Song spec §5; technical spec §13). Every duration here is
//! `Millis`; `std::time::Duration` appears only at timer boundaries (technical decisions 6).

use std::collections::BTreeMap;

use runo_core::{Cadence, Millis, Promise};

/// A source of promised cadences. Provides, never sings.
pub trait CadenceSource: Send + Sync {
    /// The promise for a unit type in a state and area.
    fn promise(&self, unit: &str, state: &str, area: &str) -> Promise;
    /// A short version string carried in every promise.
    fn version(&self) -> String;
}

/// The Core escalation factors in tenths: 1x, 1.5x, 2x.
pub const CORE_ESCALATE_TENTHS: [u64; 3] = [10, 15, 20];

/// The fallback expectation for a state the profile does not name: thirty minutes.
pub const DEFAULT_EXPECTED: Millis = Millis(30 * 60 * 1000);

/// A cadence configuration that cannot produce a threshold.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CadenceError {
    /// `base × factor / 10` does not fit a `u64` of milliseconds.
    #[error("threshold overflows u64 milliseconds: {state} × {tenths} tenths")]
    Overflow {
        /// The state whose expectation overflowed.
        state: String,
        /// The factor that overflowed it.
        tenths: u64,
    },
    /// The factors are not rising, or the first is below 1x.
    #[error("escalation factors must be rising from at least 10 tenths: {0:?}")]
    Factors([u64; 3]),
}

/// `ceil(base × tenths / 10)` in a wider integer; `None` when it does not fit `u64`
/// (technical decisions 7). Fractional milliseconds round upward so an escalation never
/// precedes its calculated deadline.
#[must_use]
pub fn threshold(base: Millis, tenths: u64) -> Option<Millis> {
    let product = u128::from(base.0) * u128::from(tenths);
    let ceil = product.div_ceil(10);
    u64::try_from(ceil).ok().map(Millis)
}

/// Fixed cadences from the profile, with escalation factors in integer tenths.
#[derive(Debug, Clone)]
pub struct FixedCadence {
    per_state: BTreeMap<String, Millis>,
    escalate_tenths: [u64; 3],
}

impl FixedCadence {
    /// Build a fixed cadence, checking every threshold it can ever produce. Overflow is a
    /// configuration error and is refused here, never at claim time.
    ///
    /// # Errors
    /// `CadenceError::Overflow` names the state and factor; `CadenceError::Factors` refuses
    /// factors that are not rising from at least 1x.
    pub fn new(
        per_state: BTreeMap<String, Millis>,
        escalate_tenths: [u64; 3],
    ) -> Result<Self, CadenceError> {
        let [call, again, suspect] = escalate_tenths;
        if call < 10 || again < call || suspect < again {
            return Err(CadenceError::Factors(escalate_tenths));
        }
        for (state, base) in per_state
            .iter()
            .chain([(&"*".to_owned(), &DEFAULT_EXPECTED)])
        {
            for tenths in escalate_tenths {
                if threshold(*base, tenths).is_none() {
                    return Err(CadenceError::Overflow {
                        state: state.clone(),
                        tenths,
                    });
                }
            }
        }
        Ok(Self {
            per_state,
            escalate_tenths,
        })
    }

    /// The escalation factors in tenths.
    #[must_use]
    pub fn escalate_tenths(&self) -> [u64; 3] {
        self.escalate_tenths
    }

    fn cadence_for(&self, expected: Millis) -> Cadence {
        // `new` checked every state and the default against every factor.
        let ms = |tenths: u64| threshold(expected, tenths).unwrap_or(Millis(u64::MAX));
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
            .unwrap_or(DEFAULT_EXPECTED);
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

/// Cadences read from the pipeline's own corpus: the last ten or more successful, uncensored
/// tomes of the same unit and area, falling back to `FixedCadence` below ten. Core, as
/// specified (technical spec §13). Implemented in milestone 4.
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

#[cfg(test)]
mod tests {
    use super::{CORE_ESCALATE_TENTHS, CadenceError, CadenceSource, FixedCadence, threshold};
    use runo_core::Millis;
    use std::collections::BTreeMap;

    #[test]
    fn thresholds_round_upward() {
        // 1 ms × 1.5 = 1.5 ms → 2 ms, never 1.
        assert_eq!(threshold(Millis(1), 15), Some(Millis(2)));
        assert_eq!(threshold(Millis(10), 15), Some(Millis(15)));
        // 30 min at 1x, 1.5x, 2x.
        let base = Millis(30 * 60 * 1000);
        assert_eq!(threshold(base, 10), Some(base));
        assert_eq!(threshold(base, 15), Some(Millis(45 * 60 * 1000)));
        assert_eq!(threshold(base, 20), Some(Millis(60 * 60 * 1000)));
    }

    #[test]
    fn overflow_is_refused_at_construction() {
        assert_eq!(threshold(Millis(u64::MAX), 20), None);
        let mut per_state = BTreeMap::new();
        per_state.insert("building".to_owned(), Millis(u64::MAX));
        let err = FixedCadence::new(per_state, CORE_ESCALATE_TENTHS).unwrap_err();
        assert_eq!(
            err,
            CadenceError::Overflow {
                state: "building".to_owned(),
                tenths: 15
            }
        );
    }

    #[test]
    fn factors_must_rise_from_one() {
        let err = FixedCadence::new(BTreeMap::new(), [10, 9, 20]).unwrap_err();
        assert_eq!(err, CadenceError::Factors([10, 9, 20]));
        assert!(FixedCadence::new(BTreeMap::new(), [5, 15, 20]).is_err());
    }

    #[test]
    fn a_promise_carries_absolute_millis() {
        let mut per_state = BTreeMap::new();
        per_state.insert("building".to_owned(), Millis(30 * 60 * 1000));
        let fixed = FixedCadence::new(per_state, CORE_ESCALATE_TENTHS).unwrap();
        let p = fixed.promise("siege_tank", "building", "core");
        assert_eq!(p.cadence.call, Millis(30 * 60 * 1000));
        assert_eq!(p.cadence.again, Millis(45 * 60 * 1000));
        assert_eq!(p.cadence.suspect, Millis(60 * 60 * 1000));
        assert_eq!(p.source, "fixed-v1");
    }
}
