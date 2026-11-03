//! Durable episodes (technical spec §2, "Episodes"): delivery to a person is organised per
//! `(lane, clock)` stall, not per signal, so one stall is one conversation and a restart does not
//! resend everything.
//!
//! The ledger is plain state; the runtime drives it in a fixed order so delivery is idempotent:
//!
//! 1. The ladder sings a `Suspect` → [`Episodes::escalated`] with that verse's id; save.
//! 2. For each of [`Episodes::pending`]: deliver it under [`Notice::key`], then
//!    [`Episodes::delivered`]; save. A crash between deliver and save resends the *same* notice
//!    with the same key, never a new one; the channel uses the key to drop the duplicate.
//! 3. A clock resolves (`runo_clock::Resolution`) → [`Episodes::resolved`]; save.
//!
//! One reading taken where the table is silent: an episode that resolves before its notice was
//! delivered is not delivered at all. A stall that cleared itself is not news, and the replay
//! measured that most of them do.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use runo_clock::{Cleared, Level};
use runo_core::{Clock, SingerId, TaskId, Timestamp, VerseId};
use serde::{Deserialize, Serialize};

/// A lane: one singer's work on one task.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct Lane {
    /// The singer.
    pub singer: SingerId,
    /// The task.
    pub task: TaskId,
}

/// An episode's identity: `(lane, clock, opening verse hash)`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EpisodeId {
    /// The lane.
    pub lane: Lane,
    /// The clock.
    pub clock: Clock,
    /// The `Suspect` verse that opened it.
    pub opening: VerseId,
}

/// One episode and its lifecycle.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Episode {
    /// Identity.
    pub id: EpisodeId,
    /// Wall time it opened, for the view and the within-the-hour measures.
    pub opened_at: Timestamp,
    /// The highest level reached.
    pub level: Level,
    /// The highest level delivered to a person, if any.
    pub delivered: Option<Level>,
    /// When and why it resolved; `None` while open.
    pub resolved: Option<(Timestamp, Cleared)>,
}

impl Episode {
    /// Whether it is still open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.resolved.is_none()
    }
}

/// What an escalation did to the ledger.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
    /// A new episode opened.
    Opened,
    /// An open episode reached a higher level: one more notice.
    Raised,
    /// Nothing new for a person.
    Unchanged,
}

/// One message owed to a person: an episode at a level.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Notice {
    /// Which episode.
    pub id: EpisodeId,
    /// At which level.
    pub level: Level,
}

impl Notice {
    /// The idempotency key: the same episode and level always give the same key, so a resent
    /// notice can be recognised and dropped by the channel.
    #[must_use]
    pub fn key(&self) -> String {
        let mut opening = String::with_capacity(16);
        for b in &self.id.opening.0.0[..8] {
            let _ = write!(opening, "{b:02x}");
        }
        format!(
            "{}/{}/{:?}/{opening}/{:?}",
            self.id.lane.singer.0, self.id.lane.task.0, self.id.clock, self.level
        )
        .to_lowercase()
    }
}

/// Episode state failure.
#[derive(Debug, thiserror::Error)]
pub enum EpisodeError {
    /// The state file could not be read or written.
    #[error("episode state: {0}")]
    Io(#[from] std::io::Error),
    /// The state file is not valid episode state.
    #[error("episode state is malformed: {0}")]
    Format(#[from] serde_json::Error),
}

/// Every episode this voice has opened, open or resolved.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Episodes {
    episodes: Vec<Episode>,
}

impl Episodes {
    fn open_mut(&mut self, lane: &Lane, clock: Clock) -> Option<&mut Episode> {
        self.episodes
            .iter_mut()
            .find(|e| e.is_open() && e.id.lane == *lane && e.id.clock == clock)
    }

    /// The ladder sang `suspect` for `lane` and `clock` at `level`. Opens an episode if none is
    /// open for them, raises the open one if `level` is higher, and otherwise changes nothing.
    pub fn escalated(
        &mut self,
        lane: &Lane,
        clock: Clock,
        level: Level,
        suspect: VerseId,
        at: &Timestamp,
    ) -> Change {
        if let Some(e) = self.open_mut(lane, clock) {
            if level > e.level {
                e.level = level;
                return Change::Raised;
            }
            return Change::Unchanged;
        }
        self.episodes.push(Episode {
            id: EpisodeId {
                lane: lane.clone(),
                clock,
                opening: suspect,
            },
            opened_at: at.clone(),
            level,
            delivered: None,
            resolved: None,
        });
        Change::Opened
    }

    /// The clock cleared. Returns whether an open episode was resolved; a later crossing opens a
    /// new one with its own identity.
    pub fn resolved(&mut self, lane: &Lane, clock: Clock, why: Cleared, at: &Timestamp) -> bool {
        self.open_mut(lane, clock)
            .map(|e| e.resolved = Some((at.clone(), why)))
            .is_some()
    }

    /// Notices owed: each open episode whose level is above what was delivered, at its current
    /// level only (levels passed while undelivered are folded into one message).
    #[must_use]
    pub fn pending(&self) -> Vec<Notice> {
        self.episodes
            .iter()
            .filter(|e| e.is_open() && e.delivered.is_none_or(|d| e.level > d))
            .map(|e| Notice {
                id: e.id.clone(),
                level: e.level,
            })
            .collect()
    }

    /// A notice was delivered. Idempotent; a lower level than already recorded changes nothing.
    pub fn delivered(&mut self, n: &Notice) {
        if let Some(e) = self.episodes.iter_mut().find(|e| e.id == n.id) {
            e.delivered = Some(e.delivered.map_or(n.level, |d| d.max(n.level)));
        }
    }

    /// Every episode, oldest first.
    #[must_use]
    pub fn all(&self) -> &[Episode] {
        &self.episodes
    }

    /// Open episodes, oldest first.
    pub fn open(&self) -> impl Iterator<Item = &Episode> {
        self.episodes.iter().filter(|e| e.is_open())
    }

    /// Keep every open episode and the newest `keep` resolved ones.
    pub fn prune(&mut self, keep: usize) {
        let resolved = self.episodes.iter().filter(|e| !e.is_open()).count();
        let mut drop = resolved.saturating_sub(keep);
        self.episodes.retain(|e| {
            if drop > 0 && !e.is_open() {
                drop -= 1;
                return false;
            }
            true
        });
    }

    /// Load the state file; a missing file is an empty ledger.
    ///
    /// # Errors
    /// The file exists but cannot be read or parsed. A malformed ledger is an error, never an
    /// empty one: starting empty would resend every open episode.
    pub fn load(path: &Path) -> Result<Self, EpisodeError> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Save atomically: write a sibling temporary file, flush it to disk, rename it over the
    /// state file, and flush the directory.
    ///
    /// # Errors
    /// Any filesystem failure; the previous state file is then left intact.
    pub fn save(&self, path: &Path) -> Result<(), EpisodeError> {
        let tmp = path.with_extension("tmp");
        let mut f = std::fs::File::create(&tmp)?;
        serde_json::to_writer_pretty(&mut f, self)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Change, Episodes, Lane, Notice};
    use runo_clock::{Cleared, Level};
    use runo_core::{Clock, Hash, SingerId, TaskId, Timestamp, VerseId};

    fn lane() -> Lane {
        Lane {
            singer: SingerId("lane-1".into()),
            task: TaskId("HOD-43".into()),
        }
    }

    fn v(n: u8) -> VerseId {
        VerseId(Hash([n; 32]))
    }

    fn ts(s: &str) -> Timestamp {
        Timestamp(s.into())
    }

    fn deliver_all(e: &mut Episodes) -> Vec<Notice> {
        let p = e.pending();
        for n in &p {
            e.delivered(n);
        }
        p
    }

    #[test]
    fn one_stall_is_one_episode_and_a_higher_level_one_more_notice() {
        let mut e = Episodes::default();
        let t = ts("10:00");
        assert_eq!(
            e.escalated(&lane(), Clock::Progress, Level::Called, v(1), &t),
            Change::Opened
        );
        assert_eq!(deliver_all(&mut e).len(), 1);
        assert_eq!(
            e.escalated(&lane(), Clock::Progress, Level::Called, v(2), &t),
            Change::Unchanged
        );
        assert!(e.pending().is_empty());
        assert_eq!(
            e.escalated(&lane(), Clock::Progress, Level::Suspected, v(3), &t),
            Change::Raised
        );
        let sent = deliver_all(&mut e);
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].id.opening,
            v(1),
            "the identity keeps its opening verse"
        );
        assert_eq!(sent[0].level, Level::Suspected);
        // Another clock on the same lane is its own episode.
        assert_eq!(
            e.escalated(&lane(), Clock::Waiting, Level::Called, v(4), &t),
            Change::Opened
        );
        assert_eq!(e.open().count(), 2);
    }

    #[test]
    fn a_later_crossing_after_resolution_is_a_new_episode() {
        let mut e = Episodes::default();
        e.escalated(&lane(), Clock::Task, Level::Called, v(1), &ts("10:00"));
        deliver_all(&mut e);
        assert!(e.resolved(&lane(), Clock::Task, Cleared::Completed, &ts("10:30")));
        assert!(!e.resolved(&lane(), Clock::Task, Cleared::Completed, &ts("10:31")));
        assert_eq!(
            e.escalated(&lane(), Clock::Task, Level::Called, v(9), &ts("12:00")),
            Change::Opened
        );
        let sent = deliver_all(&mut e);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].id.opening, v(9));
        assert_eq!(e.all().len(), 2);
    }

    #[test]
    fn a_stall_that_clears_before_delivery_is_not_delivered() {
        let mut e = Episodes::default();
        e.escalated(&lane(), Clock::Progress, Level::Called, v(1), &ts("10:00"));
        e.resolved(&lane(), Clock::Progress, Cleared::Evidence, &ts("10:05"));
        assert!(e.pending().is_empty());
    }

    #[test]
    fn levels_passed_while_undelivered_fold_into_one_notice() {
        let mut e = Episodes::default();
        e.escalated(&lane(), Clock::Progress, Level::Called, v(1), &ts("10:00"));
        e.escalated(
            &lane(),
            Clock::Progress,
            Level::CalledAgain,
            v(2),
            &ts("10:15"),
        );
        e.escalated(
            &lane(),
            Clock::Progress,
            Level::Suspected,
            v(3),
            &ts("10:30"),
        );
        let sent = deliver_all(&mut e);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].level, Level::Suspected);
    }

    #[test]
    fn a_restart_resends_only_what_was_not_recorded_as_delivered_and_with_the_same_key() {
        let dir = std::env::temp_dir().join(format!("runo-episodes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("episodes.json");
        let _ = std::fs::remove_file(&path);
        assert_eq!(Episodes::load(&path).unwrap(), Episodes::default());

        let mut e = Episodes::default();
        e.escalated(&lane(), Clock::Progress, Level::Called, v(1), &ts("10:00"));
        e.escalated(&lane(), Clock::Waiting, Level::Called, v(2), &ts("10:00"));
        let owed = e.pending();
        e.delivered(&owed[0]);
        e.save(&path).unwrap();
        // Crash after delivering the second notice but before saving that it was delivered.

        let back = Episodes::load(&path).unwrap();
        assert_eq!(back, e);
        let again = back.pending();
        assert_eq!(again, [owed[1].clone()]);
        assert_eq!(again[0].key(), owed[1].key());
        assert_ne!(owed[0].key(), owed[1].key());
        assert_eq!(
            owed[0].key(),
            "lane-1/hod-43/progress/0101010101010101/called"
        );

        std::fs::write(&path, b"{ not json").unwrap();
        assert!(
            Episodes::load(&path).is_err(),
            "a malformed ledger never loads as empty"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prune_keeps_open_episodes_and_the_newest_resolved() {
        let mut e = Episodes::default();
        for n in 0..4 {
            e.escalated(&lane(), Clock::Task, Level::Called, v(n), &ts("t"));
            e.resolved(&lane(), Clock::Task, Cleared::Completed, &ts("t"));
        }
        e.escalated(&lane(), Clock::Task, Level::Called, v(9), &ts("t"));
        e.prune(1);
        let kept: Vec<_> = e.all().iter().map(|x| x.id.opening).collect();
        assert_eq!(kept, [v(3), v(9)]);
    }
}
