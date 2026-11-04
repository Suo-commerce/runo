//! Goliath's presence chorus in one process: three voices share one store, each with a real
//! key and roster version 1 made from the example manifest.

use std::collections::BTreeMap;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use runo_clock::Level;
use runo_core::sign::{generate, seal};
use runo_core::time::Instant;
use runo_core::{Clock, Epoch, Header, Payload, Progress, RosterDoc, SingerId, TaskId, Timestamp};
use runo_profile::manifest::{Enrolment, load_manifest, roster};
use runo_profile::{Profile, load_profile};
use runo_store::mem::MemStore;
use runo_store::{Durability, VerseWriter};

use super::{Heard, Voice, VoiceError, clock_config, interval};

const MIN: u64 = 60_000;

/// An instant `ms` milliseconds after the test's start (instants are nanoseconds).
fn at(ms: u64) -> Instant {
    Instant(ms * 1_000_000)
}

fn profiles() -> BTreeMap<String, Profile> {
    [
        (
            "../../profiles/goliath.toml",
            include_str!("../../../../profiles/goliath.toml"),
        ),
        (
            "../../profiles/sentinel.toml",
            include_str!("../../../../profiles/sentinel.toml"),
        ),
        (
            "../../profiles/owner.toml",
            include_str!("../../../../profiles/owner.toml"),
        ),
    ]
    .into_iter()
    .map(|(p, t)| (p.to_owned(), load_profile(t).unwrap()))
    .collect()
}

struct Fleet {
    store: Arc<MemStore>,
    keys: BTreeMap<String, SigningKey>,
    roster: RosterDoc,
    profiles: BTreeMap<String, Profile>,
    paths: BTreeMap<String, String>,
}

fn fleet() -> Fleet {
    let m = load_manifest(include_str!("../../../../examples/goliath/chorus.toml")).unwrap();
    let profiles = profiles();
    let salt = generate().to_bytes();
    let keys: BTreeMap<String, SigningKey> = m
        .voices
        .iter()
        .map(|v| (v.singer.clone(), generate()))
        .collect();
    let enrolments: Vec<Enrolment> = m
        .voices
        .iter()
        .map(|v| {
            Enrolment::new(
                &v.singer,
                keys[&v.singer].verifying_key().to_bytes(),
                &salt,
                &v.credential,
            )
        })
        .collect();
    let roster = roster(&m, &profiles, &enrolments).unwrap();
    let paths = m
        .voices
        .iter()
        .map(|v| (v.singer.clone(), v.profile.clone()))
        .collect();
    Fleet {
        store: Arc::new(MemStore::new()),
        keys,
        roster,
        profiles,
        paths,
    }
}

impl Fleet {
    /// The in-memory store offers only process durability, so the tests run the profiles at
    /// that level; `runo node` uses the file store at the profile's own.
    fn voice(&self, singer: &str) -> Voice<MemStore> {
        let mut profile = self.profiles[&self.paths[singer]].clone();
        "process".clone_into(&mut profile.durability);
        Voice::new(
            Arc::clone(&self.store),
            self.keys[singer].clone(),
            singer,
            "boot-2026-11-04T09:00:00Z",
            profile,
            self.roster.clone(),
        )
        .unwrap()
    }
}

/// The test's start, 2026-11-04 09:00 UTC, as a wall instant.
fn base() -> Instant {
    runo_core::time::wall::parse("2026-11-04T09:00:00Z").unwrap()
}

/// The wall time `t` after the test's start.
fn wall(t: Instant) -> Timestamp {
    Timestamp(runo_core::time::wall::format(Instant(base().0 + t.0)))
}

fn about(heard: &Heard, peer: &str) -> Vec<(Clock, Level)> {
    heard
        .escalations
        .iter()
        .filter(|e| e.peer.0 == peer)
        .map(|e| (e.clock, e.level))
        .collect()
}

/// Starts all three machines at t = 0 and returns them in order thinkpad, desk, sentinel.
async fn started(f: &Fleet) -> [Voice<MemStore>; 3] {
    let mut v = [f.voice("thinkpad"), f.voice("desk"), f.voice("sentinel")];
    for voice in &mut v {
        let heard = voice.start(at(0), &wall(at(0))).await.unwrap();
        assert!(
            heard.incidents.is_empty(),
            "a clean start has no incidents: {:?}",
            heard.incidents
        );
    }
    v
}

/// Runs steps of `step` minutes up to `until`, letting `sings(i, t)` decide which voices refrain.
/// Returns what the thinkpad heard about `peer`.
async fn run(
    v: &mut [Voice<MemStore>; 3],
    from: u64,
    until: u64,
    step: u64,
    sings: impl Fn(usize, u64) -> bool,
    peer: &str,
) -> Vec<(u64, Clock, Level)> {
    let mut heard = Vec::new();
    let mut t = from;
    while t <= until {
        let now = at(t);
        for (i, voice) in v.iter_mut().enumerate() {
            if sings(i, t) {
                voice.refrain(&wall(now)).await.unwrap();
            }
        }
        for voice in v.iter_mut() {
            let h = voice.probe(now).await.unwrap();
            assert!(
                h.incidents.is_empty(),
                "no incidents in an honest chorus: {:?}",
                h.incidents
            );
            if voice.me.0 == "thinkpad" {
                heard.extend(about(&h, peer).into_iter().map(|(c, l)| (t, c, l)));
            }
        }
        t += step * MIN;
    }
    heard
}

/// Appends a verse claiming to be desk's next, signed by a key that is not desk's.
async fn forge(f: &Fleet) {
    let impostor = generate();
    let header = Header {
        v: 3,
        singer: SingerId("desk".into()),
        roster: f.roster.version,
        task: TaskId("boot-2026-11-04T09:00:00Z".into()),
        epoch: Epoch(0),
        grant: None,
        seq: 2,
        prev: None,
        cause: None,
        progress: Progress {
            height: 2,
            head: None,
        },
        sung_at: wall(at(MIN)),
        note: None,
    };
    let forged = seal(
        &impostor,
        header,
        Payload::Boundary {
            event: "going-offline".into(),
            state: None,
            blocked_on: None,
        },
    )
    .unwrap();
    f.store.append(&forged, Durability::Process).await.unwrap();
}

#[test]
fn intervals_parse_and_refuse() {
    assert_eq!(interval("15m").unwrap().0, 15 * MIN);
    assert_eq!(interval("30s").unwrap().0, 30_000);
    assert_eq!(interval("1d").unwrap().0, 1_440 * MIN);
    assert!(interval("15 minutes").is_err(), "units are ms, s, m, h, d");
    assert!(interval("m").is_err(), "a number is required");
}

#[test]
fn the_goliath_profile_declares_sleep_but_not_waiting() {
    let f = fleet();
    let c = clock_config(&f.profiles["../../profiles/goliath.toml"], &f.roster).unwrap();
    assert_eq!(
        c.reachability.unwrap().call.0,
        30 * MIN,
        "twice the 15-minute refrain"
    );
    assert_eq!(c.declared["sleep"].call.0, 1_440 * MIN);
    assert!(
        !c.declared.contains_key("waiting"),
        "waiting has its own clock: {:?}",
        c.declared.keys()
    );
    assert!(
        !c.declared.contains_key("online"),
        "a voice refrains while online"
    );
}

#[tokio::test]
async fn an_honest_chorus_is_quiet() {
    let f = fleet();
    let mut v = started(&f).await;
    let heard = run(&mut v, 0, 180 * MIN, 5, |_, _| true, "desk").await;
    assert!(
        heard.is_empty(),
        "three hours of refrains raise nothing: {heard:?}"
    );
    let rows = v[0].rows(at(180 * MIN));
    let desk = rows
        .iter()
        .find(|r| r.peer.0 == "desk")
        .expect("thinkpad hears desk");
    assert_eq!(desk.state.as_deref(), Some("online"));
}

#[tokio::test]
async fn undeclared_silence_is_called_within_the_reachability_cadence() {
    let f = fleet();
    let mut v = started(&f).await;
    // desk stops at t = 0 without a word.
    let heard = run(&mut v, 0, 60 * MIN, 5, |i, _| i != 1, "desk").await;
    let first = heard
        .iter()
        .find(|(_, c, _)| *c == Clock::Reachability)
        .expect("desk's silence is noticed");
    assert!(
        (30 * MIN..35 * MIN).contains(&first.0),
        "noticed at 30 minutes, twice the refrain interval: {heard:?}"
    );
}

#[tokio::test]
async fn a_declared_sleep_is_not_silence() {
    let f = fleet();
    let mut v = started(&f).await;
    v[1].sing(at(0), &wall(at(0)), "sleep", None).await.unwrap();
    assert_eq!(v[1].state(), Some("sleep"));
    assert!(
        !v[1].refrain(&wall(at(MIN))).await.unwrap(),
        "a sleeping machine is silent by design"
    );
    let heard = run(&mut v, 0, 23 * 60 * MIN, 15, |i, _| i != 1, "desk").await;
    assert!(
        heard.is_empty(),
        "23 hours of declared sleep raise nothing: {heard:?}"
    );
    let later = run(
        &mut v,
        23 * 60 * MIN + 15 * MIN,
        25 * 60 * MIN,
        15,
        |i, _| i != 1,
        "desk",
    )
    .await;
    assert!(
        later
            .iter()
            .any(|(t, c, _)| *c == Clock::Reachability && *t >= 24 * 60 * MIN),
        "a sleep past the sleep cadence is called: {later:?}"
    );
}

#[tokio::test]
async fn a_wake_resumes_refrains() {
    let f = fleet();
    let mut v = started(&f).await;
    v[1].sing(at(0), &wall(at(0)), "sleep", None).await.unwrap();
    v[1].sing(at(8 * 60 * MIN), &wall(at(8 * 60 * MIN)), "wake", None)
        .await
        .unwrap();
    assert!(
        v[1].refrain(&wall(at(8 * 60 * MIN))).await.unwrap(),
        "awake again"
    );
    let heard = run(&mut v, 8 * 60 * MIN, 10 * 60 * MIN, 5, |_, _| true, "desk").await;
    assert!(heard.is_empty(), "{heard:?}");
}

#[tokio::test]
async fn a_transition_the_profile_does_not_allow_is_refused_before_it_is_sung() {
    let f = fleet();
    let mut v = started(&f).await;
    let err = v[0]
        .sing(at(MIN), &wall(at(MIN)), "wake", None)
        .await
        .unwrap_err();
    assert!(matches!(err, VoiceError::Transition { .. }), "{err}");
    assert_eq!(v[0].state(), Some("online"));
}

#[tokio::test]
async fn a_forged_verse_is_an_incident_not_an_observation() {
    let f = fleet();
    let mut v = started(&f).await;
    forge(&f).await;
    let heard = v[0].probe(at(MIN)).await.unwrap();
    assert_eq!(heard.incidents.len(), 1, "{:?}", heard.incidents);
    assert_eq!(heard.incidents[0].0.0, "desk");
    let rows = v[0].rows(at(MIN));
    let desk = rows
        .iter()
        .find(|r| r.peer.0 == "desk")
        .expect("desk is still heard");
    assert_eq!(
        desk.state.as_deref(),
        Some("online"),
        "the forgery did not take desk offline"
    );
}

#[tokio::test]
async fn the_control_socket_sings_and_refuses() {
    use crate::run::{SingReply, SingRequest, Timers, ask, bind_control, run};
    use runo_core::time::MonotonicClock;
    use std::time::Duration;

    let f = fleet();
    let mut desk = f.voice("desk");
    let dir = std::env::temp_dir().join(format!("runo-node-test-{}", std::process::id()));
    let path = dir.join("desk.sock");
    let control = bind_control(&path).unwrap();
    assert!(
        bind_control(&path).is_err(),
        "a live node's socket is not taken over"
    );
    let clock = MonotonicClock::default();
    let wall = || wall(at(0));
    let mut lines = Vec::new();
    let mut log = |l: String| lines.push(l);
    let (done, stop) = tokio::sync::oneshot::channel::<()>();
    let timers = Timers {
        refrain: Duration::from_secs(3600),
        probe: Duration::from_secs(3600),
    };
    let node = run(&mut desk, &clock, &wall, control, timers, &mut log, async {
        let _ = stop.await;
    });
    let client = async {
        let sleep = SingRequest {
            event: "sleep".into(),
            blocked_on: None,
        };
        let docked = SingRequest {
            event: "docked".into(),
            blocked_on: None,
        };
        let a = ask(&path, &sleep).await.unwrap();
        let b = ask(&path, &docked).await.unwrap();
        done.send(()).unwrap();
        (a, b)
    };
    let (ran, (a, b)) = tokio::join!(node, client);
    ran.unwrap();
    assert!(matches!(a, SingReply::Sung(_)), "{a:?}");
    assert!(
        matches!(&b, SingReply::Refused(why) if why.contains("sleep → docked")),
        "{b:?}"
    );
    assert_eq!(desk.state(), Some("sleep"));
    assert!(lines.iter().any(|l| l.ends_with("sang sleep")), "{lines:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_reboot_ends_the_old_session_without_a_call() {
    let f = fleet();
    let mut v = started(&f).await;
    let _ = run(&mut v, 0, 10 * MIN, 5, |_, _| true, "desk").await;
    // desk loses power at 10 minutes and comes back at 15 with a new boot session.
    let mut again = Voice::new(
        Arc::clone(&f.store),
        f.keys["desk"].clone(),
        "desk",
        "boot-2026-11-04T09:15:00Z",
        {
            let mut p = f.profiles["../../profiles/goliath.toml"].clone();
            "process".clone_into(&mut p.durability);
            p
        },
        f.roster.clone(),
    )
    .unwrap();
    again
        .start(at(15 * MIN), &wall(at(15 * MIN)))
        .await
        .unwrap();
    v[1] = again;
    let heard = run(&mut v, 15 * MIN, 120 * MIN, 5, |_, _| true, "desk").await;
    assert!(
        heard.is_empty(),
        "the old session's silence is not called: {heard:?}"
    );
    let rows = v[0].rows(at(120 * MIN));
    let desk: Vec<_> = rows
        .iter()
        .filter(|r| r.peer.0 == "desk")
        .map(|r| r.task.0.as_str())
        .collect();
    assert_eq!(
        desk,
        ["boot-2026-11-04T09:15:00Z"],
        "one session per machine in the view"
    );
}

mod view {
    use std::collections::BTreeMap;

    use runo_clock::Level;
    use runo_core::time::Instant;

    use super::{MIN, at, base, fleet, run, started, wall};
    use crate::fleet::{Fleet, Line, Verdict, render};
    use crate::listener::Listener;

    fn view(f: &super::Fleet) -> Fleet {
        let profiles = f
            .roster
            .voices
            .iter()
            .map(|v| (v.singer.clone(), f.profiles[&f.paths[&v.singer.0]].clone()))
            .collect::<BTreeMap<_, _>>();
        Fleet::new(Listener::new(f.roster.clone()), profiles).unwrap()
    }

    fn now(ms: u64) -> Instant {
        Instant(base().0 + at(ms).0)
    }

    fn verdicts(lines: &[Line]) -> Vec<(&str, &Verdict)> {
        lines
            .iter()
            .map(|l| (l.singer.0.as_str(), &l.verdict))
            .collect()
    }

    #[tokio::test]
    async fn problems_first_then_up_then_resting_then_offline() {
        let f = fleet();
        let mut v = started(&f).await;
        v[1].sing(at(0), &wall(at(0)), "sleep", None).await.unwrap();
        // thinkpad refrains; sentinel goes silent at the start without a word.
        let _ = run(&mut v, 0, 60 * MIN, 5, |i, _| i == 0, "desk").await;
        let mut fv = view(&f);
        fv.update(&*f.store).await.unwrap();
        let lines = fv.lines(now(60 * MIN));
        assert_eq!(
            lines.len(),
            3,
            "the owner has no machine cadence: {lines:?}"
        );
        let got = verdicts(&lines);
        assert!(
            matches!(got[0], ("sentinel", Verdict::Quiet { level, .. }) if *level >= Level::Called),
            "a silent sentinel is first: {got:?}"
        );
        assert_eq!(got[1], ("thinkpad", &Verdict::Up));
        assert!(
            matches!(got[2], ("desk", Verdict::Resting { .. })),
            "{got:?}"
        );

        v[0].sing(at(61 * MIN), &wall(at(61 * MIN)), "state-saved", None)
            .await
            .unwrap();
        v[0].sing(at(61 * MIN), &wall(at(61 * MIN)), "going-offline", None)
            .await
            .unwrap();
        fv.update(&*f.store).await.unwrap();
        let lines = fv.lines(now(62 * MIN));
        assert_eq!(verdicts(&lines)[2], ("thinkpad", &Verdict::Offline));
        let text = render("goliath", &lines, fv.incidents(), now(62 * MIN));
        assert!(
            text.starts_with("runo view · goliath · 2026-11-04 10:02 UTC"),
            "{text}"
        );
        assert!(text.contains("sentinel  QUIET 1 h 02 min"), "{text}");
        assert!(text.contains("sleep since 09:00, as it said"), "{text}");
        assert!(text.contains("offline since 10:01, as it said"), "{text}");
        assert!(text.ends_with("incidents: none\n"), "{text}");
    }

    #[tokio::test]
    async fn a_long_sleep_is_called_and_a_machine_never_heard_says_so() {
        let f = fleet();
        let mut desk = f.voice("desk");
        desk.start(at(0), &wall(at(0))).await.unwrap();
        desk.sing(at(0), &wall(at(0)), "sleep", None).await.unwrap();
        let mut fv = view(&f);
        fv.update(&*f.store).await.unwrap();
        let lines = fv.lines(now(25 * 60 * MIN));
        let got = verdicts(&lines);
        assert!(
            matches!(got[0], ("desk", Verdict::Quiet { .. })),
            "a day and an hour asleep: {got:?}"
        );
        assert_eq!(
            got[1..],
            [
                ("sentinel", &Verdict::NeverSung),
                ("thinkpad", &Verdict::NeverSung)
            ]
        );
    }

    #[tokio::test]
    async fn a_forged_record_is_an_incident_on_the_view() {
        let f = fleet();
        let _v = started(&f).await;
        super::forge(&f).await;
        let mut fv = view(&f);
        fv.update(&*f.store).await.unwrap();
        let lines = fv.lines(now(MIN));
        assert!(
            lines.iter().all(|l| l.verdict == Verdict::Up),
            "the forgery took nobody offline: {lines:?}"
        );
        let text = render("goliath", &lines, fv.incidents(), now(MIN));
        assert!(text.contains("incidents: 1 records refused"), "{text}");
        assert!(text.contains("claiming to be desk"), "{text}");
    }
}

#[tokio::test]
async fn each_peer_is_judged_by_its_own_profile() {
    let f = fleet();
    let peers: BTreeMap<SingerId, Profile> = f
        .roster
        .voices
        .iter()
        .map(|v| (v.singer.clone(), f.profiles[&f.paths[&v.singer.0]].clone()))
        .collect();
    let thinkpad = f.voice("thinkpad").with_peers(&peers).unwrap();
    let mut v = [thinkpad, f.voice("desk"), f.voice("sentinel")];
    for voice in &mut v {
        voice.start(at(0), &wall(at(0))).await.unwrap();
    }
    // The sentinel refrains every 5 minutes, so its silence is called after 10, not 30.
    let heard = run(&mut v, 0, 20 * MIN, 5, |i, _| i != 2, "sentinel").await;
    let first = heard
        .iter()
        .find(|(_, c, _)| *c == Clock::Reachability)
        .expect("the sentinel's silence is noticed");
    assert_eq!(first.0, 10 * MIN, "{heard:?}");
}
