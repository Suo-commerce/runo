//! Escalation thresholds and the wait-one-cadence rule (Song spec §5, listening steps 4–7;
//! technical spec §5). The one home for them (CLAUDE.md, ownership table).
//!
//! A promise carries three absolute thresholds, `call`, `again` and `suspect` (1×, 1.5×, 2× the
//! expected cadence by default). Listening step 4 says to wait one cadence after a clock first
//! crosses, for the lane's own next event, before calling; most crossings clear themselves (58%
//! of unanswered suspects on the second replay). The reading taken here: the whole ladder moves
//! by that one cadence, so every level keeps its distance from the next one.
//!
//! | Level | Reached at | Default (cadence `c`) |
//! | --- | --- | --- |
//! | `Crossed` | `call` | 1× `c` |
//! | `Called` | `call` + `call` | 2× `c` |
//! | `CalledAgain` | `again` + `call` | 2.5× `c` |
//! | `Suspected` | `suspect` + `call` | 3× `c` |
//!
//! Kinds that never clear on their own (a failed run that needs a person, an allocator's
//! `NotActivated`) skip the wait. That is the singer's decision (the herald sings `Suspect` at
//! once), not this table's.

use runo_core::{Cadence, Millis};

use crate::Level;

/// The elapsed time at which each level is reached, in order: `Crossed`, `Called`,
/// `CalledAgain`, `Suspected`. Saturating, so a huge promise never wraps.
#[must_use]
pub fn thresholds(c: &Cadence) -> [Millis; 4] {
    let wait = c.call.0;
    [
        c.call,
        Millis(c.call.0.saturating_add(wait)),
        Millis(c.again.0.saturating_add(wait)),
        Millis(c.suspect.0.saturating_add(wait)),
    ]
}

/// The level a clock has reached after `elapsed`, and how far past that level's threshold it
/// is.
#[must_use]
pub fn level_at(c: &Cadence, elapsed: Millis) -> (Level, Millis) {
    const LEVELS: [Level; 4] = [
        Level::Crossed,
        Level::Called,
        Level::CalledAgain,
        Level::Suspected,
    ];
    let t = thresholds(c);
    (0..4)
        .rev()
        .find(|&i| elapsed >= t[i])
        .map_or((Level::Quiet, Millis(0)), |i| {
            (LEVELS[i], Millis(elapsed.0 - t[i].0))
        })
}

/// The threshold of one level; `Quiet` is zero.
#[must_use]
pub fn threshold_of(c: &Cadence, level: Level) -> Millis {
    let t = thresholds(c);
    match level {
        Level::Quiet => Millis(0),
        Level::Crossed => t[0],
        Level::Called => t[1],
        Level::CalledAgain => t[2],
        Level::Suspected => t[3],
    }
}

#[cfg(test)]
mod tests {
    use super::{level_at, thresholds};
    use crate::Level;
    use runo_core::{Cadence, Millis};

    const MIN: u64 = 60_000;

    fn thirty() -> Cadence {
        Cadence {
            call: Millis(30 * MIN),
            again: Millis(45 * MIN),
            suspect: Millis(60 * MIN),
        }
    }

    #[test]
    fn the_ladder_moves_by_one_cadence() {
        let t = thresholds(&thirty());
        assert_eq!(t.map(|m| m.0 / MIN), [30, 60, 75, 90]);
    }

    #[test]
    fn levels_and_overdue() {
        let c = thirty();
        assert_eq!(level_at(&c, Millis(29 * MIN)), (Level::Quiet, Millis(0)));
        assert_eq!(level_at(&c, Millis(30 * MIN)), (Level::Crossed, Millis(0)));
        assert_eq!(level_at(&c, Millis(61 * MIN)), (Level::Called, Millis(MIN)));
        assert_eq!(
            level_at(&c, Millis(80 * MIN)),
            (Level::CalledAgain, Millis(5 * MIN))
        );
        assert_eq!(level_at(&c, Millis(500 * MIN)).0, Level::Suspected);
    }

    #[test]
    fn a_huge_promise_saturates() {
        let c = Cadence {
            call: Millis(u64::MAX),
            again: Millis(u64::MAX),
            suspect: Millis(u64::MAX),
        };
        assert_eq!(thresholds(&c)[3], Millis(u64::MAX));
        assert_eq!(level_at(&c, Millis(u64::MAX - 1)).0, Level::Quiet);
    }
}
