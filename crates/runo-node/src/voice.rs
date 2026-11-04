//! One voice in a chorus (technical spec §6): sing, refrain, probe. Time comes from the caller,
//! so the same code runs under `runo node`'s timers, in tests, and later in the simulator.
//!
//! - **Sing** listens first (one probe), checks the transition against the profile's state
//!   machine, builds the next verse on this voice's chain, admits it against the local mirror,
//!   then appends it to the store at the profile's durability. Nothing that ingress would refuse
//!   leaves the voice.
//! - **Refrain** signs `n + 1` while the voice's state is one it refrains in (`refrain_while`); a
//!   sleeping machine is silent by design.
//! - **Probe** reads the store from its cursor, admits every verse against the mirror (a forged or
//!   broken verse is an incident, never an observation), observes the voices this one listens
//!   to, verifies and observes their latest refrains, and ticks the clocks.
//!
//! For Goliath's presence (4 Nov 2026) each start is a boot session: the task is
//! `boot-<start time>`, claimed at epoch 0 (no grant needed, ingress step 11), with a promise
//! that has no deadline and no checkpoints, so only reachability and waiting can escalate.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use runo_clock::{Chorus, Config, Escalation, Evidence, Observation, Resolution, Row};
use runo_core::fence::Durability;
use runo_core::ingress::{Decision, RecordClass, RejectReason, RosterHistory, admit};
use runo_core::sign::{seal, sign_refrain, verify_refrain};
use runo_core::time::Instant;
use runo_core::{
    Cadence, Epoch, Header, Millis, Payload, Progress, Promise, Refrain, RosterDoc, RosterVersion,
    SingerId, TaskId, TaskSpec, Timestamp, Verse, VerseId,
};
use runo_profile::Profile;
use runo_store::mem::MemStore;
use runo_store::{Cursor, RefrainSlot, StoreError, VerseReader, VerseWriter};

/// A voice's failure.
#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    /// The store could not be reached or refused.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The profile's state machine does not allow this transition.
    #[error("{from} → {to} is not a transition this profile allows")]
    Transition {
        /// The current state.
        from: String,
        /// The state asked for.
        to: String,
    },
    /// Ingress refused this voice's own verse: a bug or a stale roster, never sent.
    #[error("ingress refused this voice's verse: {0:?}")]
    Refused(Decision),
    /// The voice is not in the roster, or the profile is not usable.
    #[error("{0}")]
    Setup(String),
}

/// What one probe found.
#[derive(Debug, Default)]
pub struct Heard {
    /// Clock crossings since the last probe.
    pub escalations: Vec<Escalation>,
    /// Clocks that went quiet again.
    pub resolutions: Vec<Resolution>,
    /// Verses that failed ingress: who claimed to sing them, and why they were refused.
    pub incidents: Vec<(SingerId, RejectReason)>,
}

/// The state a voice declares when it is blocked; it refrains through it.
const WAITING: &str = "waiting";

/// A roster history of one version: Core's runs are fixed for their duration.
struct Rosters(RosterDoc);

impl RosterHistory for Rosters {
    fn at(&self, v: RosterVersion) -> Option<&RosterDoc> {
        (v == self.0.version).then_some(&self.0)
    }
    fn current(&self) -> RosterVersion {
        self.0.version
    }
}

/// `"90s"`, `"15m"`, `"2h"`, `"1d"` to milliseconds.
///
/// # Errors
/// Anything else.
pub fn interval(s: &str) -> Result<Millis, VoiceError> {
    let s = s.trim();
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = n
        .parse()
        .map_err(|_| VoiceError::Setup(format!("bad interval {s:?}")))?;
    let per = match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => {
            return Err(VoiceError::Setup(format!(
                "bad interval {s:?}: use ms, s, m, h or d"
            )));
        }
    };
    n.checked_mul(per)
        .map(Millis)
        .ok_or_else(|| VoiceError::Setup(format!("interval {s:?} overflows")))
}

/// A cadence ladder from a base and the profile's escalation tenths, rounding up.
fn ladder(base: Millis, tenths: &[u64]) -> Result<Cadence, VoiceError> {
    let t = |i: usize| {
        let tenth = tenths.get(i).copied().unwrap_or(10 + 5 * i as u64);
        runo_cadence::threshold(base, tenth).ok_or_else(|| {
            VoiceError::Setup(format!("cadence {} ms × {tenth} tenths overflows", base.0))
        })
    };
    Ok(Cadence {
        call: t(0)?,
        again: t(1)?,
        suspect: t(2)?,
    })
}

fn minutes(profile: &Profile, key: &str) -> Option<Millis> {
    profile
        .cadence
        .per_state
        .get(key)?
        .as_integer()
        .and_then(|m| u64::try_from(m).ok())
        .map(|m| Millis(m * 60_000))
}

/// The clock configuration a profile implies (technical spec §5, §10).
///
/// - Reachability: the profile's `reachability` minutes, else twice its refrain interval.
/// - Declared states: every state the voice does not refrain in (`sleep`) that has a cadence;
///   that cadence becomes the reachability expectation while a peer declares it. `waiting` is
///   never one: it has its own clock, and a waiting voice keeps refraining.
/// - Waiting: the profile's `waiting` minutes.
///
/// # Errors
/// A bad interval or an overflowing cadence.
pub fn clock_config(profile: &Profile, roster: &RosterDoc) -> Result<Config, VoiceError> {
    let tenths = &profile.cadence.escalate;
    let refrain = profile
        .refrain_interval
        .as_deref()
        .map(interval)
        .transpose()?;
    let reach = minutes(profile, "reachability").or_else(|| refrain.map(|r| Millis(r.0 * 2)));
    let mut declared = BTreeMap::new();
    if let Some(awake) = &profile.refrain_while {
        for state in profile.cadence.per_state.keys() {
            let is_state = profile
                .states
                .transitions
                .iter()
                .any(|(a, b)| a == state || b == state);
            // `waiting` has its own clock; a waiting voice is awake and keeps refraining.
            if is_state
                && state != WAITING
                && !awake.contains(state)
                && let Some(base) = minutes(profile, state)
            {
                declared.insert(state.clone(), ladder(base, tenths)?);
            }
        }
    }
    Ok(Config {
        reachability: reach.map(|r| ladder(r, tenths)).transpose()?,
        waiting: minutes(profile, "waiting")
            .map(|w| ladder(w, tenths))
            .transpose()?,
        terminal: profile.states.terminal.iter().cloned().collect(),
        owner: Some(roster.owner.clone()),
        domains: roster
            .voices
            .iter()
            .map(|v| (v.singer.clone(), v.domains.clone()))
            .collect(),
        declared,
    })
}

/// One voice.
pub struct Voice<S: ?Sized> {
    store: Arc<S>,
    key: SigningKey,
    me: SingerId,
    task: TaskId,
    profile: Profile,
    durability: Durability,
    rosters: Rosters,
    roles: BTreeMap<SingerId, String>,
    listen: BTreeSet<String>,
    mirror: MemStore,
    cursor: Cursor,
    chorus: Chorus,
    seq: u64,
    last: Option<VerseId>,
    refrain_n: u64,
    state: Option<String>,
    pending: Vec<Verse>,
    /// The highest refrain verified per peer and task, so an unchanged slot is not re-verified.
    verified: BTreeMap<(SingerId, TaskId), u64>,
    /// Each peer's latest boot session: a machine runs one at a time.
    sessions: BTreeMap<SingerId, TaskId>,
}

impl<S: VerseReader + VerseWriter + RefrainSlot + ?Sized> Voice<S> {
    /// A voice for `singer` on `task` (its boot session), with its key, profile and roster.
    ///
    /// # Errors
    /// The singer is not in the roster, its key is not the roster's, or the profile is unusable.
    pub fn new(
        store: Arc<S>,
        key: SigningKey,
        singer: &str,
        task: &str,
        profile: Profile,
        roster: RosterDoc,
    ) -> Result<Self, VoiceError> {
        let me = SingerId(singer.to_owned());
        let entry = roster
            .voices
            .iter()
            .find(|v| v.singer == me)
            .ok_or_else(|| {
                VoiceError::Setup(format!("{singer} is not in roster v{}", roster.version.0))
            })?;
        if entry.key != key.verifying_key().to_bytes() {
            return Err(VoiceError::Setup(format!(
                "this host's key for {singer} is not the one in the roster"
            )));
        }
        let durability = match profile.durability.as_str() {
            "process" => Durability::Process,
            _ => Durability::Host,
        };
        let config = clock_config(&profile, &roster)?;
        Ok(Self {
            store,
            key,
            me,
            task: TaskId(task.to_owned()),
            listen: profile.listen.to.iter().cloned().collect(),
            roles: roster
                .voices
                .iter()
                .map(|v| (v.singer.clone(), v.role.clone()))
                .collect(),
            profile,
            durability,
            rosters: Rosters(roster),
            mirror: MemStore::new(),
            cursor: Cursor(0),
            chorus: Chorus::new(config),
            seq: 0,
            last: None,
            refrain_n: 0,
            state: None,
            pending: Vec::new(),
            verified: BTreeMap::new(),
            sessions: BTreeMap::new(),
        })
    }

    /// The boot session this voice sings on.
    #[must_use]
    pub fn task(&self) -> &TaskId {
        &self.task
    }

    /// The voice's current declared state.
    #[must_use]
    pub fn state(&self) -> Option<&str> {
        self.state.as_deref()
    }

    /// Catch up on the store without treating history as live (startup replay, §4), then claim
    /// this boot session and declare the profile's initial state.
    ///
    /// # Errors
    /// The store fails, or ingress refuses the claim.
    pub async fn start(&mut self, now: Instant, wall: &Timestamp) -> Result<Heard, VoiceError> {
        self.chorus.set_replay(true);
        let replayed = self.read(now).await?;
        self.chorus.set_replay(false);
        let promise = Promise {
            unit: "boot-session".into(),
            cadence: ladder(Millis(30 * 86_400_000), &self.profile.cadence.escalate)?,
            checkpoints: false,
            source: "presence: a boot session has no deadline".into(),
        };
        let claim = Payload::Claimed {
            task: TaskSpec {
                deliver: "presence".into(),
                scope: vec![],
                kind: self.profile.role.clone(),
            },
            promise,
        };
        self.put(claim, wall).await?;
        let initial = self.profile.states.initial.clone();
        self.put(
            Payload::Boundary {
                event: initial.clone(),
                state: None,
                blocked_on: None,
            },
            wall,
        )
        .await?;
        self.state = Some(initial);
        let mut heard = self.probe(now).await?;
        heard.incidents.splice(0..0, replayed.incidents);
        Ok(heard)
    }

    /// Declare a new state (`sleep`, `wake`, `docked`, `going-offline`, `waiting` …). Listens
    /// first, so the verse is built on the latest chain head.
    ///
    /// # Errors
    /// The transition is not allowed, the store fails, or ingress refuses the verse.
    pub async fn sing(
        &mut self,
        now: Instant,
        wall: &Timestamp,
        event: &str,
        blocked_on: Option<String>,
    ) -> Result<VerseId, VoiceError> {
        self.read(now).await?;
        let from = self.state.clone().unwrap_or_default();
        if !self.allowed(&from, event) {
            return Err(VoiceError::Transition {
                from,
                to: event.to_owned(),
            });
        }
        let id = self
            .put(
                Payload::Boundary {
                    event: event.to_owned(),
                    state: None,
                    blocked_on,
                },
                wall,
            )
            .await?;
        self.state = Some(event.to_owned());
        Ok(id)
    }

    /// Sing the refrain if this voice refrains in its current state. Returns whether it did.
    ///
    /// # Errors
    /// Signing or the store fails.
    pub async fn refrain(&mut self, wall: &Timestamp) -> Result<bool, VoiceError> {
        let Some(state) = &self.state else {
            return Ok(false);
        };
        let terminal = self.profile.states.terminal.contains(state);
        let awake = state == WAITING
            || self
                .profile
                .refrain_while
                .as_ref()
                .is_none_or(|w| w.contains(state));
        let (Some(last), false, true) = (self.last, terminal, awake) else {
            return Ok(false);
        };
        self.refrain_n += 1;
        let r = sign_refrain(
            &self.key,
            Refrain {
                singer: self.me.clone(),
                task: self.task.clone(),
                epoch: Epoch(0),
                n: self.refrain_n,
                last,
                progress: Progress {
                    height: self.seq,
                    head: None,
                },
                sent_at: wall.clone(),
                sig: [0; 64],
            },
        )
        .map_err(|e| VoiceError::Setup(e.to_string()))?;
        self.store.refrain(&r).await?;
        Ok(true)
    }

    /// Read what is new, observe it, and tick the clocks to `now`.
    ///
    /// # Errors
    /// The store fails.
    pub async fn probe(&mut self, now: Instant) -> Result<Heard, VoiceError> {
        let mut heard = self.read(now).await?;
        for r in self.store.latest_refrains().await? {
            let slot = (r.singer.clone(), r.task.clone());
            if r.singer == self.me
                || !self.listens_to(&r.singer)
                || self.verified.get(&slot).is_some_and(|&n| r.n <= n)
            {
                continue;
            }
            let Some(v) = self.rosters.0.voices.iter().find(|v| v.singer == r.singer) else {
                heard
                    .incidents
                    .push((r.singer.clone(), RejectReason::UnknownSinger));
                continue;
            };
            if verify_refrain(&r, &v.key).is_err() {
                heard
                    .incidents
                    .push((r.singer.clone(), RejectReason::BadSig));
                continue;
            }
            self.verified.insert(slot, r.n);
            self.chorus.observe(now, &self.me, Observation::Refrain(&r));
        }
        heard.escalations = self.chorus.tick(now);
        heard.resolutions = self.chorus.take_resolutions();
        Ok(heard)
    }

    /// The clocks, one row per peer and boot session.
    #[must_use]
    pub fn rows(&self, now: Instant) -> Vec<Row> {
        self.chorus.rows(now)
    }

    fn listens_to(&self, singer: &SingerId) -> bool {
        self.listen.contains("*")
            || self
                .roles
                .get(singer)
                .is_some_and(|r| self.listen.contains(r))
    }

    fn allowed(&self, from: &str, to: &str) -> bool {
        self.profile
            .states
            .transitions
            .iter()
            .any(|(a, b)| (a == from || a == "*") && (b == to || (b == "*" && from == "waiting")))
    }

    /// Admit pending and new records against the mirror; observe the listened ones.
    async fn read(&mut self, now: Instant) -> Result<Heard, VoiceError> {
        let mut heard = Heard::default();
        loop {
            let (records, next) = self.store.read_since(self.cursor).await?;
            if records.is_empty() {
                break;
            }
            self.cursor = next;
            let mut batch = std::mem::take(&mut self.pending);
            batch.extend(records.into_iter().map(|r| r.verse));
            for verse in batch {
                self.take(now, verse, &mut heard).await?;
            }
        }
        Ok(heard)
    }

    async fn take(
        &mut self,
        now: Instant,
        verse: Verse,
        heard: &mut Heard,
    ) -> Result<(), VoiceError> {
        let decision = admit(&verse, &self.rosters, &self.mirror);
        let class = match decision {
            Decision::Stored { .. } => return Ok(()),
            Decision::Pending { .. } => {
                self.pending.push(verse);
                return Ok(());
            }
            Decision::Reject(why) => {
                heard.incidents.push((verse.header.singer.clone(), why));
                return Ok(());
            }
            Decision::Append { .. } => RecordClass::Current,
            Decision::History => RecordClass::History,
            Decision::Control => RecordClass::Control,
        };
        match class {
            RecordClass::Current => self.mirror.append(&verse, Durability::Process).await?,
            RecordClass::History => self.mirror.archive(&verse, Durability::Process).await?,
            RecordClass::Control => self.mirror.record(&verse, Durability::Process).await?,
        };
        let singer = verse.header.singer.clone();
        if singer != self.me && class == RecordClass::Current && self.listens_to(&singer) {
            self.chorus
                .observe(now, &self.me, Observation::Verse(&verse));
            if matches!(verse.payload, Payload::Claimed { .. }) {
                self.new_session(now, &singer, &verse.header.task);
            }
        }
        Ok(())
    }

    /// A peer claimed a new boot session: its previous one ended, whether or not it sang
    /// `going-offline` (a crash or a power cut does not), so the old session's silence is not
    /// called while the new one sings.
    fn new_session(&mut self, now: Instant, singer: &SingerId, task: &TaskId) {
        if let Some(old) = self.sessions.insert(singer.clone(), task.clone())
            && old != *task
        {
            let ended = Observation::Inferred {
                singer,
                task: &old,
                what: Evidence::Ended,
            };
            self.chorus.observe(now, &self.me, ended);
        }
    }

    /// Build, seal, admit and append this voice's next verse.
    async fn put(&mut self, payload: Payload, wall: &Timestamp) -> Result<VerseId, VoiceError> {
        let header = Header {
            v: 3,
            singer: self.me.clone(),
            roster: self.rosters.0.version,
            task: self.task.clone(),
            epoch: Epoch(0),
            grant: None,
            seq: self.seq,
            prev: self.last,
            cause: None,
            progress: Progress {
                height: self.seq,
                head: None,
            },
            sung_at: wall.clone(),
            note: None,
        };
        let verse =
            seal(&self.key, header, payload).map_err(|e| VoiceError::Setup(e.to_string()))?;
        match admit(&verse, &self.rosters, &self.mirror) {
            Decision::Append { .. } => {}
            other => return Err(VoiceError::Refused(other)),
        }
        self.store.append(&verse, self.durability).await?;
        self.mirror.append(&verse, Durability::Process).await?;
        self.seq += 1;
        self.last = Some(verse.seal.sum);
        Ok(verse.seal.sum)
    }
}

#[cfg(test)]
mod tests;
