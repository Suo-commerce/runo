#![allow(
    clippy::many_single_char_names,
    reason = "short names keep the scenarios readable"
)]

use std::collections::{BTreeMap, BTreeSet};

use runo_core::time::Instant;
use runo_core::{
    About, Cadence, Clock, Domains, Epoch, Hash, Header, Millis, Payload, Progress, Promise, Ref,
    Refrain, RosterVersion, Seal, SingerId, TaskId, TaskSpec, Timestamp, Verdict, Verse, VerseId,
};

use crate::{Chorus, Cleared, ClockState, Config, Escalation, Level, Observation, Resolution};

const MIN: u64 = 60_000_000_000; // nanoseconds

fn at(minutes: u64) -> Instant {
    Instant(minutes * MIN)
}

fn thirty() -> Cadence {
    let m = 60_000;
    Cadence {
        call: Millis(30 * m),
        again: Millis(45 * m),
        suspect: Millis(60 * m),
    }
}

fn s(x: &str) -> SingerId {
    SingerId(x.into())
}

fn t() -> TaskId {
    TaskId("t1".into())
}

fn verse(singer: &str, epoch: u64, height: u64, payload: Payload) -> Verse {
    Verse {
        header: Header {
            v: 3,
            singer: s(singer),
            roster: RosterVersion(1),
            task: t(),
            epoch: Epoch(epoch),
            grant: None,
            seq: height,
            prev: None,
            cause: None,
            progress: Progress { height, head: None },
            sung_at: Timestamp("2026-11-03T00:00:00Z".into()),
            note: None,
        },
        payload,
        seal: Seal {
            sum: VerseId(Hash::default()),
            sig: [0; 64],
        },
    }
}

fn claimed(singer: &str, epoch: u64, checkpoints: bool) -> Verse {
    verse(
        singer,
        epoch,
        0,
        Payload::Claimed {
            task: TaskSpec {
                deliver: "x".into(),
                scope: vec![],
                kind: "build".into(),
            },
            promise: Promise {
                unit: "build".into(),
                cadence: thirty(),
                checkpoints,
                source: "fixed".into(),
            },
        },
    )
}

fn boundary(singer: &str, height: u64, event: &str, blocked_on: Option<&str>) -> Verse {
    verse(
        singer,
        1,
        height,
        Payload::Boundary {
            event: event.into(),
            state: None,
            blocked_on: blocked_on.map(Into::into),
        },
    )
}

fn work(singer: &str, height: u64) -> Verse {
    verse(
        singer,
        1,
        height,
        Payload::Work {
            step: "edit".into(),
            changed: vec![],
            head: None,
        },
    )
}

fn refrain(singer: &str, n: u64) -> Refrain {
    Refrain {
        singer: s(singer),
        task: t(),
        epoch: Epoch(1),
        n,
        last: VerseId(Hash::default()),
        progress: Progress {
            height: 0,
            head: None,
        },
        sent_at: Timestamp("2026-11-03T00:00:00Z".into()),
        sig: [0; 64],
    }
}

fn config(reachability: bool) -> Config {
    Config {
        reachability: reachability.then(thirty),
        waiting: Some(thirty()),
        terminal: BTreeSet::from(["done".to_string(), "failed".to_string()]),
        owner: Some(s("owner")),
        domains: BTreeMap::new(),
    }
}

fn see(c: &mut Chorus, now: Instant, v: &Verse) {
    c.observe(now, &s("me"), Observation::Verse(v));
}

fn levels(e: &[Escalation], clock: Clock) -> Vec<Level> {
    e.iter()
        .filter(|x| x.clock == clock)
        .map(|x| x.level)
        .collect()
}

fn row_clock(c: &Chorus, now: Instant, clock: usize) -> ClockState {
    c.rows(now)[0].clocks[clock]
}

#[test]
fn silence_climbs_the_ladder_once_per_level() {
    let mut c = Chorus::new(config(false));
    see(&mut c, at(0), &claimed("a", 1, true));
    assert!(
        c.tick(at(29)).is_empty(),
        "nothing is due before the first threshold"
    );
    let e = c.tick(at(30));
    assert_eq!(levels(&e, Clock::Progress), [Level::Crossed]);
    assert!(
        c.tick(at(31)).is_empty(),
        "a crossing is emitted exactly once"
    );
    let e = c.tick(at(91));
    assert_eq!(
        levels(&e, Clock::Progress),
        [Level::Called, Level::CalledAgain, Level::Suspected]
    );
    let last = e.iter().rfind(|x| x.clock == Clock::Progress).unwrap();
    assert_eq!(last.overdue_by, Millis(60_000));
    assert!(c.tick(at(500)).is_empty(), "the top level is reached once");
}

#[test]
fn progress_resolves_and_restarts_the_progress_clock_only() {
    let mut c = Chorus::new(config(false));
    see(&mut c, at(0), &claimed("a", 1, true));
    c.tick(at(30));
    see(&mut c, at(40), &work("a", 1));
    assert_eq!(
        c.take_resolutions(),
        [Resolution {
            peer: s("a"),
            task: t(),
            clock: Clock::Progress,
            why: Cleared::Evidence
        }]
    );
    assert!(
        levels(&c.tick(at(69)), Clock::Progress).is_empty(),
        "progress restarted at 40, so nothing is due before 70"
    );
    assert_eq!(levels(&c.tick(at(70)), Clock::Progress), [Level::Crossed]);
    // The task clock never resets within an epoch: it kept climbing and is Called by now.
    assert!(matches!(
        row_clock(&c, at(70), 2),
        ClockState::Running {
            level: Level::Called,
            ..
        }
    ));
}

#[test]
fn work_at_the_same_height_is_not_progress() {
    let mut c = Chorus::new(config(false));
    see(&mut c, at(0), &claimed("a", 1, true));
    see(&mut c, at(10), &work("a", 0));
    assert_eq!(levels(&c.tick(at(30)), Clock::Progress), [Level::Crossed]);
}

/// Technical spec §5 acceptance: a lane blocked longer than the waiting cadence escalates the
/// waiting clock, not progress or task; the task clock is paused for the wait and resumes after.
#[test]
fn an_overdue_wait_escalates_waiting_only() {
    let mut c = Chorus::new(config(false));
    see(&mut c, at(0), &claimed("a", 1, true));
    see(
        &mut c,
        at(10),
        &boundary("a", 1, "waiting", Some("review by jarmo")),
    );
    let e = c.tick(at(40));
    assert_eq!(levels(&e, Clock::Waiting), [Level::Crossed]);
    assert!(
        levels(&e, Clock::Progress).is_empty(),
        "progress does not escalate while waiting"
    );
    assert!(
        levels(&e, Clock::Task).is_empty(),
        "the task clock is paused while waiting"
    );
    let r = &c.rows(at(40))[0];
    assert_eq!(r.blocked_on.as_deref(), Some("review by jarmo"));
    assert_eq!(
        r.clocks[1],
        ClockState::Idle,
        "progress does not run while waiting"
    );
    assert_eq!(
        r.clocks[2],
        ClockState::Running {
            elapsed: Millis(10 * 60_000),
            level: Level::Quiet
        }
    );

    // Unblocked at 100: waiting resolves, the task clock has 10 + (t - 100) minutes on it.
    see(&mut c, at(100), &boundary("a", 2, "working", None));
    assert_eq!(
        c.take_resolutions(),
        [Resolution {
            peer: s("a"),
            task: t(),
            clock: Clock::Waiting,
            why: Cleared::Evidence
        }]
    );
    assert!(
        levels(&c.tick(at(119)), Clock::Task).is_empty(),
        "the wait paused the task clock"
    );
    assert_eq!(levels(&c.tick(at(120)), Clock::Task), [Level::Crossed]);
    assert_eq!(row_clock(&c, at(120), 3), ClockState::Idle);
}

#[test]
fn watch_mode_shows_reachability_unknown_and_never_escalates_it() {
    let mut c = Chorus::new(config(false));
    see(&mut c, at(0), &claimed("a", 1, true));
    let e = c.tick(at(1000));
    assert!(
        levels(&e, Clock::Reachability).is_empty(),
        "watch mode never escalates reachability"
    );
    assert_eq!(row_clock(&c, at(1000), 0), ClockState::Unknown);
}

#[test]
fn a_fresh_refrain_resolves_reachability_and_a_stale_one_does_not() {
    let mut c = Chorus::new(config(true));
    see(&mut c, at(0), &claimed("a", 1, true));
    c.observe(at(5), &s("me"), Observation::Refrain(&refrain("a", 4)));
    assert_eq!(
        levels(&c.tick(at(35)), Clock::Reachability),
        [Level::Crossed]
    );
    c.observe(at(36), &s("me"), Observation::Refrain(&refrain("a", 4)));
    assert!(
        c.take_resolutions().is_empty(),
        "a repeated n proves nothing"
    );
    c.observe(at(37), &s("me"), Observation::Refrain(&refrain("a", 5)));
    assert_eq!(c.take_resolutions()[0].clock, Clock::Reachability);
}

#[test]
fn replay_rebuilds_tasks_but_never_proves_reachability() {
    let mut c = Chorus::new(config(true));
    c.set_replay(true);
    see(&mut c, at(0), &claimed("a", 1, true));
    c.set_replay(false);
    // reach_since was never set by the replayed claim, so reachability has no evidence to time.
    assert_eq!(row_clock(&c, at(0), 0), ClockState::Idle);
    c.observe(at(1), &s("me"), Observation::Refrain(&refrain("a", 1)));
    assert!(matches!(
        row_clock(&c, at(1), 0),
        ClockState::Running { .. }
    ));
}

fn domains(host: &str, provider: &str) -> Domains {
    Domains {
        host: host.into(),
        credential: Hash::default(),
        provider: provider.into(),
        network: "lan".into(),
    }
}

fn vouch(c: &mut Chorus, now: Instant, from: &str) {
    let about = About {
        singer: s("a"),
        task: None,
        epoch: None,
        hash: None,
    };
    c.observe(
        now,
        &s(from),
        Observation::Vouch {
            about: &about,
            from: &s(from),
        },
    );
}

#[test]
fn an_independent_vouch_lowers_reachability_one_level_and_a_shared_one_needs_company() {
    let mut cfg = config(true);
    cfg.domains = BTreeMap::from([
        (s("a"), domains("h1", "anthropic")),
        (s("same"), domains("h1", "anthropic")),
        (s("part"), domains("h2", "anthropic")),
        (s("free"), domains("h3", "openai")),
    ]);
    // `free` still shares credential and network with `a`: weight 1, like `part`.
    let mut c = Chorus::new(cfg);
    see(&mut c, at(0), &claimed("a", 1, true));
    c.tick(at(60));
    assert!(matches!(
        row_clock(&c, at(60), 0),
        ClockState::Running {
            level: Level::Called,
            ..
        }
    ));

    vouch(&mut c, at(61), "same");
    c.tick(at(61));
    assert!(matches!(
        row_clock(&c, at(61), 0),
        ClockState::Running {
            level: Level::Called,
            ..
        }
    ));

    vouch(&mut c, at(62), "part");
    c.tick(at(62));
    assert!(matches!(
        row_clock(&c, at(62), 0),
        ClockState::Running {
            level: Level::Called,
            ..
        }
    ));
    vouch(&mut c, at(63), "free");
    c.tick(at(63));
    assert!(matches!(
        row_clock(&c, at(63), 0),
        ClockState::Running {
            level: Level::Crossed,
            ..
        }
    ));

    vouch(&mut c, at(64), "a");
    c.tick(at(64));
    assert!(
        matches!(
            row_clock(&c, at(64), 0),
            ClockState::Running {
                level: Level::Crossed,
                ..
            }
        ),
        "a voice cannot vouch for itself"
    );
}

#[test]
fn a_terminal_boundary_completes_and_the_row_leaves_the_view() {
    let mut c = Chorus::new(config(false));
    see(&mut c, at(0), &claimed("a", 1, true));
    c.tick(at(30));
    see(&mut c, at(40), &boundary("a", 3, "done", None));
    let r = c.take_resolutions();
    assert!(
        r.iter()
            .any(|x| x.clock == Clock::Task && x.why == Cleared::Completed)
    );
    assert!(
        c.rows(at(40)).is_empty(),
        "a completed task leaves the view"
    );
    assert!(
        c.tick(at(500)).is_empty(),
        "a completed task never escalates"
    );
}

#[test]
fn a_newer_epoch_relaunches_and_an_older_one_is_ignored() {
    let mut c = Chorus::new(config(false));
    see(&mut c, at(0), &claimed("a", 2, true));
    c.tick(at(30));
    see(&mut c, at(35), &claimed("a", 1, true));
    assert!(
        c.take_resolutions().is_empty(),
        "an older epoch is not a relaunch"
    );
    see(&mut c, at(40), &claimed("a", 3, true));
    let r = c.take_resolutions();
    assert!(r.iter().all(|x| x.why == Cleared::Relaunched));
    assert_eq!(r.len(), 2, "progress and task were raised");
    assert_eq!(c.rows(at(40))[0].epoch, Epoch(3));
}

#[test]
fn an_owner_ruling_resolves_and_anyone_elses_does_not() {
    let mut c = Chorus::new(config(false));
    see(&mut c, at(0), &claimed("a", 1, true));
    c.tick(at(30));
    let ruling = |by: &str| {
        verse(
            by,
            1,
            0,
            Payload::Ruling {
                about: About {
                    singer: s("a"),
                    task: Some(t()),
                    epoch: None,
                    hash: None,
                },
                verdict: Verdict::Accept,
                reason: None,
            },
        )
    };
    see(&mut c, at(31), &ruling("b"));
    assert!(
        c.take_resolutions().is_empty(),
        "only the owner's ruling resolves"
    );
    see(&mut c, at(32), &ruling("owner"));
    assert!(c.take_resolutions().iter().all(|x| x.why == Cleared::Ruled));
}

#[test]
fn a_promise_without_checkpoints_has_no_progress_clock() {
    let mut c = Chorus::new(config(false));
    see(&mut c, at(0), &claimed("a", 1, false));
    let e = c.tick(at(30));
    assert!(
        levels(&e, Clock::Progress).is_empty(),
        "no checkpoints, no progress clock"
    );
    assert_eq!(levels(&e, Clock::Task), [Level::Crossed]);
}

#[test]
fn a_heard_suspect_is_shown_not_adopted() {
    let mut c = Chorus::new(config(false));
    see(&mut c, at(0), &claimed("a", 1, true));
    let about = About {
        singer: s("a"),
        task: Some(t()),
        epoch: None,
        hash: None,
    };
    c.observe(
        at(1),
        &s("b"),
        Observation::SuspectHeard {
            about: &about,
            clock: Clock::Progress,
            from: &s("b"),
        },
    );
    assert!(c.tick(at(2)).is_empty(), "a heard suspect is not adopted");
    assert_eq!(c.rows(at(2))[0].suspected_by, BTreeSet::from([s("b")]));
}

#[test]
fn a_close_completes_every_lane_on_the_task() {
    let mut c = Chorus::new(config(false));
    see(&mut c, at(0), &claimed("a", 1, true));
    c.tick(at(30));
    let close = verse(
        "alloc",
        1,
        0,
        Payload::Close {
            task: t(),
            snapshot: Ref {
                scheme: "blob".into(),
                path: "s".into(),
                hash: Hash::default(),
            },
        },
    );
    see(&mut c, at(31), &close);
    assert!(
        c.take_resolutions()
            .iter()
            .any(|x| x.peer == s("a") && x.why == Cleared::Completed)
    );
    assert!(c.rows(at(31)).is_empty(), "a closed task leaves the view");
}

#[test]
fn a_vouch_buys_time_and_the_clock_climbs_again() {
    let mut cfg = config(true);
    cfg.domains = BTreeMap::from([
        (s("a"), domains("h1", "anthropic")),
        (
            s("b"),
            Domains {
                credential: Hash([1; 32]),
                network: "wan".into(),
                ..domains("h2", "openai")
            },
        ),
    ]);
    let mut c = Chorus::new(cfg);
    see(&mut c, at(0), &claimed("a", 1, true));
    c.tick(at(30));
    vouch(&mut c, at(40), "b");
    assert!(
        levels(&c.tick(at(40)), Clock::Reachability).is_empty(),
        "the vouch bought one cadence"
    );
    assert_eq!(
        c.take_resolutions(),
        [Resolution {
            peer: s("a"),
            task: t(),
            clock: Clock::Reachability,
            why: Cleared::Vouched
        }]
    );
    assert!(
        levels(&c.tick(at(69)), Clock::Reachability).is_empty(),
        "the vouch bought one cadence"
    );
    assert_eq!(
        levels(&c.tick(at(70)), Clock::Reachability),
        [Level::Crossed]
    );
}

fn infer(c: &mut Chorus, now: Instant, what: crate::Evidence<'_>) {
    c.observe(
        now,
        &s("herald"),
        Observation::Inferred {
            singer: &s("a"),
            task: &t(),
            what,
        },
    );
}

fn advisory() -> Promise {
    Promise {
        unit: "issue".into(),
        cadence: thirty(),
        checkpoints: true,
        source: "board history".into(),
    }
}

/// The waiting acceptance test again, from board inference instead of the lane's own verses:
/// one set of clocks for both modes (technical spec §15).
#[test]
fn inferred_evidence_follows_the_same_clock_rules() {
    use crate::Evidence::{Blocked, Ended, Progressed, Started, Unblocked};
    let mut c = Chorus::new(config(false));
    let p = advisory();
    infer(&mut c, at(0), Started(&p));
    infer(&mut c, at(5), Started(&p));
    infer(&mut c, at(10), Blocked("review"));
    let e = c.tick(at(40));
    assert_eq!(levels(&e, Clock::Waiting), [Level::Crossed]);
    assert!(
        levels(&e, Clock::Task).is_empty(),
        "a wait pauses the task clock"
    );
    infer(&mut c, at(100), Unblocked);
    assert_eq!(c.take_resolutions()[0].clock, Clock::Waiting);
    assert_eq!(
        levels(&c.tick(at(120)), Clock::Task),
        [Level::Crossed],
        "a repeated start did not reset the task clock"
    );
    infer(&mut c, at(125), Progressed(1));
    infer(&mut c, at(126), Progressed(1));
    assert_eq!(
        row_clock(&c, at(129), 1),
        ClockState::Running {
            elapsed: Millis(4 * 60_000),
            level: Level::Quiet
        },
        "the same mark twice is one piece of progress, timed from the first"
    );
    infer(&mut c, at(130), Ended);
    assert!(c.rows(at(130)).is_empty(), "an ended lane leaves the view");
}

#[test]
fn inference_never_proves_reachability() {
    let mut c = Chorus::new(config(true));
    let p = advisory();
    infer(&mut c, at(0), crate::Evidence::Started(&p));
    infer(&mut c, at(1), crate::Evidence::Progressed(1));
    assert_eq!(row_clock(&c, at(500), 0), ClockState::Idle);
    assert!(
        levels(&c.tick(at(500)), Clock::Reachability).is_empty(),
        "no evidence of reachability, so nothing to time"
    );
}
