//! The fifteen scenarios of technical spec §12. Each runs the real code under one seed, checks
//! the model's invariants after every step, and ends with its own "must hold". The seed decides
//! the interleaving: which request runs first, where delayed writes land, how much unrelated
//! work and replication happens in between.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;

use runo_clock::escalate::threshold_of;
use runo_clock::{Chorus as Clocks, Config, Level, Observation};
use runo_core::ingress::{Decision, RecordClass};
use runo_core::time::Instant;
use runo_core::{
    About, Cadence, Clock, Epoch, Hash, Header, Millis, Payload, Progress, Promise, Refrain,
    RosterVersion, Seal, SingerId, TaskId, TaskSpec, Timestamp, Verse, VerseId,
};
use runo_fence::{Dedup, OpKey, Outcome};
use runo_testkit::fixture::lane;

use crate::rng::Rng;
use crate::world::World;

/// A scenario's result: `Err` names the broken rule; the caller adds the trace.
pub type Run = Result<(), String>;

/// Every scenario by its file name under `scenarios/`.
pub const ALL: [&str; 15] = [
    "relaunch-race",
    "paused-resumes",
    "partitioned-install",
    "fence-restart",
    "reorder",
    "replayed-refrain",
    "refrain-no-progress",
    "effect-retry",
    "allocator-dark",
    "late-verse",
    "close-vs-relaunch",
    "after-close",
    "two-readers",
    "wait-overdue",
    "close-crash",
];

/// Run one scenario under one seed. Returns the trace with the result.
pub async fn run(name: &str, seed: u64) -> (Run, Vec<String>) {
    let mut trace = Vec::new();
    let r = match name {
        "relaunch-race" => relaunch_race(seed, &mut trace).await,
        "paused-resumes" => paused_resumes(seed, &mut trace).await,
        "partitioned-install" => partitioned_install(seed, &mut trace).await,
        "fence-restart" => fence_restart(seed, &mut trace).await,
        "reorder" => reorder(seed, &mut trace).await,
        "replayed-refrain" => replayed_refrain(seed, &mut trace),
        "refrain-no-progress" => refrain_no_progress(seed, &mut trace),
        "effect-retry" => effect_retry(seed, &mut trace).await,
        "allocator-dark" => allocator_dark(seed, &mut trace).await,
        "late-verse" => late_verse(seed, &mut trace).await,
        "close-vs-relaunch" => close_vs_relaunch(seed, &mut trace).await,
        "after-close" => after_close(seed, &mut trace).await,
        "two-readers" => two_readers(seed, &mut trace),
        "wait-overdue" => wait_overdue(seed, &mut trace),
        "close-crash" => close_crash(seed, &mut trace).await,
        other => Err(format!("no scenario named {other}")),
    };
    (r, trace)
}

fn ensure(ok: bool, why: impl Into<String>) -> Run {
    if ok { Ok(()) } else { Err(why.into()) }
}

fn t(name: &str) -> TaskId {
    TaskId(name.into())
}

/// Unrelated work and replication between the steps that matter, as much as the seed says.
async fn noise(w: &mut World) -> Run {
    for _ in 0..w.rng.below(3) {
        match w.rng.below(3) {
            0 => {
                let other = lane(2);
                let n = t("NOISE");
                let v = if w
                    .records(0, &n)
                    .await
                    .iter()
                    .any(|r| r.verse.header.singer == other)
                {
                    w.c.work(&other, &n, Epoch(0), None)
                } else {
                    w.c.claim(&other, &n, Epoch(0), None)
                };
                w.deliver(&v).await;
            }
            1 => w.sync().await,
            _ => {}
        }
        w.check().await?;
    }
    Ok(())
}

async fn step(w: &mut World) -> Run {
    w.check().await?;
    noise(w).await
}

fn grants_for(records: &[runo_store::Record], holder: &SingerId) -> usize {
    records
        .iter()
        .filter(|r| matches!(&r.verse.payload, Payload::Grant { holder: h, .. } if h == holder))
        .count()
}

/// Two peers suspect the same lane and both ask the allocator: exactly one epoch is granted.
async fn relaunch_race(seed: u64, trace: &mut Vec<String>) -> Run {
    let mut w = World::new(seed, 3, 1);
    let (l, task) = (lane(0), t("T"));
    let claim = w.c.claim(&l, &task, Epoch(0), None);
    w.deliver(&claim).await;
    step(&mut w).await?;
    let first_first = w.rng.chance(50);
    let (a, b) = {
        let one = w.alloc.relaunch_stale(&l, &task, Epoch(0));
        let two = w.alloc.relaunch_stale(&l, &task, Epoch(0));
        if first_first {
            tokio::join!(one, two)
        } else {
            let (b, a) = tokio::join!(two, one);
            (a, b)
        }
    };
    w.after_relaunch(&l, &task, &a);
    w.after_relaunch(&l, &task, &b);
    step(&mut w).await?;
    let records = w.records(0, &task).await;
    let result = match (a, b) {
        (Ok(Outcome::Activated { grant: ga, .. }), Ok(Outcome::Activated { grant: gb, .. })) => {
            ensure(
                ga == gb && grants_for(&records, &l) == 1,
                format!("{} grants for one suspicion", grants_for(&records, &l)),
            )
        }
        other => Err(format!(
            "both requests should be answered with the activation: {other:?}"
        )),
    };
    *trace = w.trace;
    result
}

/// The old incarnation sleeps through its replacement and writes afterwards: its writes are
/// history everywhere.
async fn paused_resumes(seed: u64, trace: &mut Vec<String>) -> Run {
    let mut w = World::new(seed, 3, 1);
    let (l, task) = (lane(0), t("T"));
    let claim = w.c.claim(&l, &task, Epoch(0), None);
    w.deliver(&claim).await;
    let paused: Vec<Verse> = (0..=w.rng.below(4))
        .map(|_| w.c.work(&l, &task, Epoch(0), None))
        .collect();
    step(&mut w).await?;
    let Ok(Outcome::Activated { grant, .. }) = w.relaunch(&l, &task, Epoch(0)).await else {
        *trace = w.trace;
        return Err("the relaunch did not activate".into());
    };
    step(&mut w).await?;
    let mut fresh = vec![w.c.claim(&l, &task, Epoch(1), Some(grant))];
    for _ in 0..w.rng.below(4) {
        fresh.push(w.c.work(&l, &task, Epoch(1), Some(grant)));
    }
    // The paused writes land at random points among the replacement's.
    let mut order: Vec<(bool, usize)> = (0..paused.len()).map(|i| (true, i)).collect();
    let mut f: Vec<(bool, usize)> = (0..fresh.len()).map(|i| (false, i)).collect();
    w.rng.shuffle(&mut order);
    let mut merged = Vec::new();
    while !order.is_empty() || !f.is_empty() {
        if !f.is_empty() && (order.is_empty() || w.rng.chance(50)) {
            merged.push(f.remove(0));
        } else {
            merged.push(order.remove(0));
        }
    }
    for (old, i) in merged {
        let v = if old {
            paused[i].clone()
        } else {
            fresh[i].clone()
        };
        w.deliver(&v).await;
        step(&mut w).await?;
    }
    for v in &paused {
        let class = w.class(0, &v.seal.sum).await;
        if class != Some(RecordClass::History) {
            *trace = w.trace;
            return Err(format!("a paused write was filed as {class:?}"));
        }
    }
    *trace = w.trace;
    Ok(())
}

/// One replica is unreachable during install: the replacement stays inactive, the dark replica
/// may still take the old epoch until it gets the fence, and nothing under the new epoch is
/// admitted anywhere until activation.
async fn partitioned_install(seed: u64, trace: &mut Vec<String>) -> Run {
    let mut w = World::new(seed, 3, 2);
    let (l, task) = (lane(0), t("T"));
    let claim = w.c.claim(&l, &task, Epoch(0), None);
    w.deliver(&claim).await;
    step(&mut w).await?;
    w.replicas[1].faults.dark.store(true, Ordering::SeqCst);
    w.note("replica 1 goes dark");
    let out = w.relaunch(&l, &task, Epoch(0)).await;
    let Ok(Outcome::NotActivated { grant, missing }) = out else {
        *trace = w.trace;
        return Err(format!("expected not activated, got {out:?}"));
    };
    if missing.len() != 1 {
        *trace = w.trace;
        return Err(format!("missing {missing:?}"));
    }
    let early = w.c.claim(&l, &task, Epoch(1), Some(grant));
    let d = w.deliver(&early).await;
    if d.iter()
        .flatten()
        .any(|d| matches!(d, Decision::Append { .. }))
    {
        *trace = w.trace;
        return Err("a write under the unactivated epoch was admitted".into());
    }
    let old = w.c.work(&l, &task, Epoch(0), None);
    w.deliver(&old).await;
    step(&mut w).await?;
    w.replicas[1].faults.dark.store(false, Ordering::SeqCst);
    w.note("replica 1 is back");
    if w.rng.chance(50) {
        let late_old = w.c.work(&l, &task, Epoch(0), None);
        w.deliver(&late_old).await; // allowed: replica 1 has no fence yet
    }
    step(&mut w).await?;
    let again = w.relaunch(&l, &task, Epoch(0)).await;
    let Ok(Outcome::Activated { grant: g2, .. }) = again else {
        *trace = w.trace;
        return Err(format!("the retry should activate: {again:?}"));
    };
    if g2 != grant {
        *trace = w.trace;
        return Err("the retry granted a second epoch".into());
    }
    // The early claim, re-delivered now, is admitted; a new old write is history everywhere.
    w.deliver(&early).await;
    let stale = w.c.work(&l, &task, Epoch(0), None);
    w.deliver(&stale).await;
    w.sync().await;
    let mut classes = Vec::new();
    for i in 0..w.replicas.len() {
        classes.push(w.class(i, &stale.seal.sum).await);
    }
    let result = ensure(
        classes.iter().all(|c| *c == Some(RecordClass::History)),
        format!("after activation an old write must be history everywhere: {classes:?}"),
    );
    step(&mut w).await?;
    *trace = w.trace;
    result
}

/// A resource restarts after acknowledging a fence: the fence persists.
async fn fence_restart(seed: u64, trace: &mut Vec<String>) -> Run {
    use std::sync::Arc;

    use runo_core::fence::{Durability, Fence};
    use runo_core::{ResourceId, Timestamp};
    use runo_fence::Allocator;
    use runo_fence::journal::MemJournal;
    use runo_store::ResourceFenceStore;
    use runo_store::jsonl::JsonlStore;
    use runo_testkit::fixture::{Chorus, route};

    let mut rng = Rng::new(seed);
    let dir = std::env::temp_dir().join(format!(
        "runo-sim-fence-restart-{}-{seed}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let open = || JsonlStore::open(&dir, ResourceId("song".into()), Durability::Host);
    let mut c = Chorus::new(1);
    let (l, task) = (lane(0), t("T"));
    let journal_events;
    let relaunches = 1 + rng.below(3);
    {
        let store: Arc<JsonlStore> = Arc::new(open().map_err(|e| e.to_string())?);
        let claim = c.claim(&l, &task, Epoch(0), None);
        let d = runo_core::ingress::admit(&claim, &c, &*store);
        route(&*store, &claim, &d)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let a = Allocator::new(
            c.key(&SingerId("allocator".into())).clone(),
            c.roster().clone(),
            store.clone(),
            MemJournal::new(),
            Box::new(|| Timestamp("2026-11-03T12:30:00Z".into())),
        )
        .map_err(|e| e.to_string())?;
        for _ in 0..relaunches {
            a.relaunch(&l, &task).await.map_err(|e| e.to_string())?;
        }
        journal_events = a.journal().events();
        trace.push(format!("{relaunches} relaunches, then the store restarts"));
    }
    let store = open().map_err(|e| e.to_string())?;
    let fence = store
        .installed_fence(&l, &task)
        .await
        .map_err(|e| e.to_string())?;
    let expected = Fence::Epoch(Epoch(relaunches));
    let stale = c.work(&l, &task, Epoch(relaunches - 1), None);
    let d = runo_core::ingress::admit(&stale, &c, &store);
    let _ = std::fs::remove_dir_all(&dir);
    trace.push(format!(
        "{} journal events; after restart: {fence:?}, a stale write: {d:?}",
        journal_events.len()
    ));
    ensure(
        fence == Some(expected),
        format!("fence after restart {fence:?}, expected {expected:?}"),
    )?;
    ensure(
        d == Decision::History || matches!(d, Decision::Reject(_)),
        format!("a stale write was {d:?}"),
    )
}

/// Verses delivered in random order: every verse appended once, in seq order.
async fn reorder(seed: u64, trace: &mut Vec<String>) -> Run {
    let mut w = World::new(seed, 3, 1);
    let (l, task) = (lane(0), t("T"));
    let mut chain = vec![w.c.claim(&l, &task, Epoch(0), None)];
    for _ in 0..2 + w.rng.below(8) {
        chain.push(w.c.work(&l, &task, Epoch(0), None));
    }
    let mut order = chain.clone();
    w.rng.shuffle(&mut order);
    let mut pending: Vec<Verse> = Vec::new();
    for v in order {
        if w.rng.chance(20) {
            // A duplicate arrives too.
            w.deliver(&v).await;
        }
        let d = w.deliver(&v).await;
        if matches!(d[0], Some(Decision::Pending { .. })) {
            pending.push(v);
        }
        loop {
            let mut progressed = false;
            let mut still = Vec::new();
            for p in std::mem::take(&mut pending) {
                let d = w.deliver(&p).await;
                if matches!(d[0], Some(Decision::Pending { .. })) {
                    still.push(p);
                } else {
                    progressed = true;
                }
            }
            pending = still;
            if !progressed {
                break;
            }
        }
        step(&mut w).await?;
    }
    let seqs: Vec<u64> = w
        .records(0, &task)
        .await
        .iter()
        .filter(|r| r.verse.header.singer == l)
        .map(|r| r.verse.header.seq)
        .collect();
    let want: Vec<u64> = (0..chain.len() as u64).collect();
    *trace = w.trace;
    ensure(seqs == want, format!("appended {seqs:?}, want {want:?}"))
}

const MIN: u64 = 60_000_000_000;

fn thirty() -> Cadence {
    Cadence {
        call: Millis(30 * 60_000),
        again: Millis(45 * 60_000),
        suspect: Millis(60 * 60_000),
    }
}

fn clock_config() -> Config {
    Config {
        reachability: Some(thirty()),
        waiting: Some(thirty()),
        terminal: BTreeSet::from(["done".to_string()]),
        owner: None,
        domains: BTreeMap::new(),
        declared: BTreeMap::new(),
        peers: BTreeMap::new(),
    }
}

fn raw(singer: &str, height: u64, payload: Payload) -> Verse {
    Verse {
        header: Header {
            v: 3,
            singer: SingerId(singer.into()),
            roster: RosterVersion(1),
            task: t("T"),
            epoch: Epoch(0),
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
            sum: VerseId(Hash([u8::try_from(height % 251).unwrap_or(0); 32])),
            sig: [0; 64],
        },
    }
}

fn claimed(singer: &str, checkpoints: bool) -> Verse {
    raw(
        singer,
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

fn refrain(singer: &str, n: u64) -> Refrain {
    Refrain {
        singer: SingerId(singer.into()),
        task: t("T"),
        epoch: Epoch(0),
        n,
        last: VerseId(Hash::default()),
        progress: Progress::default(),
        sent_at: Timestamp("2026-11-03T00:00:00Z".into()),
        sig: [0; 64],
    }
}

/// Minute by minute: a lane refrains on time with no new work; old refrains are redelivered at
/// random. Returns when each level of the progress clock was first reached.
fn refrain_minutes(rng: &mut Rng, minutes: u64, vouches: bool) -> BTreeMap<Level, u64> {
    let me = SingerId("watcher".into());
    let mut clocks = Clocks::new(clock_config());
    clocks.observe(Instant(0), &me, Observation::Verse(&claimed("lane", true)));
    let mut first: BTreeMap<Level, u64> = BTreeMap::new();
    let mut n = 0;
    for m in 1..=minutes {
        let now = Instant(m * MIN);
        if m % 5 == 0 {
            n += 1;
            clocks.observe(now, &me, Observation::Refrain(&refrain("lane", n)));
        }
        if n > 1 && rng.chance(30) {
            let old = 1 + rng.below(n - 1);
            clocks.observe(now, &me, Observation::Refrain(&refrain("lane", old)));
        }
        if vouches && rng.chance(20) {
            let about = About {
                singer: SingerId("lane".into()),
                task: Some(t("T")),
                epoch: None,
                hash: None,
            };
            let from = SingerId(format!("peer-{}", rng.below(3)));
            clocks.observe(
                now,
                &me,
                Observation::Vouch {
                    about: &about,
                    from: &from,
                },
            );
        }
        for e in clocks.tick(now) {
            if e.clock == Clock::Progress {
                first.entry(e.level).or_insert(m);
            }
            if e.clock == Clock::Reachability {
                first.entry(Level::Quiet).or_insert(m); // marks a reachability escalation
            }
        }
    }
    first
}

fn minutes_of(level: Level) -> u64 {
    threshold_of(&thirty(), level).0 / 60_000
}

/// Old refrains redelivered: no clock reset, and progress escalation fires on time.
#[allow(clippy::unnecessary_wraps, reason = "every scenario returns a Run")]
fn replayed_refrain(seed: u64, trace: &mut Vec<String>) -> Run {
    let mut rng = Rng::new(seed);
    let first = refrain_minutes(&mut rng, 70, false);
    trace.push(format!("progress levels first reached at {first:?}"));
    ensure(
        !first.contains_key(&Level::Quiet),
        "refrains on time must keep reachability quiet",
    )?;
    ensure(
        first.get(&Level::Crossed) == Some(&minutes_of(Level::Crossed)),
        format!("progress must cross at {} min", minutes_of(Level::Crossed)),
    )
}

/// A lane refrains with a fixed height: progress is suspected on time, and vouches from other
/// voices do not suppress it (they lower reachability only).
#[allow(clippy::unnecessary_wraps, reason = "every scenario returns a Run")]
fn refrain_no_progress(seed: u64, trace: &mut Vec<String>) -> Run {
    let mut rng = Rng::new(seed);
    let first = refrain_minutes(&mut rng, minutes_of(Level::Suspected) + 5, true);
    trace.push(format!("progress levels first reached at {first:?}"));
    ensure(
        first.get(&Level::Suspected) == Some(&minutes_of(Level::Suspected)),
        format!(
            "progress must be suspected at {} min: {first:?}",
            minutes_of(Level::Suspected)
        ),
    )
}

/// A keyed effect retried across epochs with a lost ack: the consumer's `Dedup` sees one effect.
async fn effect_retry(seed: u64, trace: &mut Vec<String>) -> Run {
    #[derive(Default)]
    struct Seen(std::sync::Mutex<std::collections::HashSet<OpKey>>);
    impl Dedup for Seen {
        fn seen(&self, key: &OpKey) -> bool {
            self.0.lock().is_ok_and(|s| s.contains(key))
        }
        fn mark(&self, key: &OpKey) {
            if let Ok(mut s) = self.0.lock() {
                s.insert(key.clone());
            }
        }
    }
    let mut w = World::new(seed, 3, 1);
    let (l, task) = (lane(0), t("T"));
    let claim = w.c.claim(&l, &task, Epoch(0), None);
    w.deliver(&claim).await;
    let dedup = Seen::default();
    let key = OpKey {
        task: task.clone(),
        operation_id: "open-pull-request".into(),
    };
    let mut effects = 0;
    let mut epoch = Epoch(0);
    for _ in 0..=w.rng.below(3) {
        if !dedup.seen(&key) {
            effects += 1;
            dedup.mark(&key);
        }
        w.note(format!(
            "epoch {} tries the effect; the ack is lost",
            epoch.0
        ));
        if let Ok(Outcome::Activated { .. }) = w.relaunch(&l, &task, epoch).await {
            epoch = Epoch(epoch.0 + 1);
        }
        step(&mut w).await?;
    }
    *trace = w.trace;
    ensure(effects == 1, format!("{effects} effects for one key"))
}

/// The allocator is unreachable during a suspicion: nothing is relaunched, and the suspicion
/// stays visible.
async fn allocator_dark(seed: u64, trace: &mut Vec<String>) -> Run {
    let mut w = World::new(seed, 3, 1);
    let (l, task) = (lane(0), t("T"));
    let claim = w.c.claim(&l, &task, Epoch(0), None);
    w.deliver(&claim).await;
    step(&mut w).await?;
    let me = SingerId("watcher".into());
    let mut clocks = Clocks::new(clock_config());
    clocks.observe(Instant(0), &me, Observation::Verse(&claimed(&l.0, false)));
    let mut suspected = false;
    for m in 1..=minutes_of(Level::Suspected) {
        suspected |= clocks
            .tick(Instant(m * MIN))
            .iter()
            .any(|e| e.level == Level::Suspected);
    }
    w.replicas[0].faults.dark.store(true, Ordering::SeqCst);
    w.note("the allocator's store goes dark");
    let tries = 1 + w.rng.below(3);
    let mut refused = 0;
    for _ in 0..tries {
        if w.relaunch(&l, &task, Epoch(0)).await.is_err() {
            refused += 1;
        }
    }
    w.replicas[0].faults.dark.store(false, Ordering::SeqCst);
    let grants = grants_for(&w.records(0, &task).await, &l);
    w.check().await?;
    *trace = w.trace;
    ensure(suspected, "the silence must reach suspected")?;
    ensure(
        refused == tries && grants == 0,
        format!("{refused}/{tries} refused, {grants} grants"),
    )
}

/// A signed verse arrives after closure: archived unchanged, an addendum recorded, the tome
/// unchanged and still verifying.
async fn late_verse(seed: u64, trace: &mut Vec<String>) -> Run {
    let mut w = World::new(seed, 3, 1);
    let task = t("T");
    for i in 0..2 {
        let v = w.c.claim(&lane(i), &task, Epoch(0), None);
        w.deliver(&v).await;
        for _ in 0..w.rng.below(3) {
            let v = w.c.work(&lane(i), &task, Epoch(0), None);
            w.deliver(&v).await;
        }
        step(&mut w).await?;
    }
    let (close, tome) = w.close_and_seal(&task).await?;
    step(&mut w).await?;
    let late = w.c.work(&lane(w.rng.pick(2)), &task, Epoch(0), None);
    let d = w.deliver(&late).await;
    ensure(
        d[0] == Some(Decision::History),
        format!("the late verse was {d:?}"),
    )?;
    let tome_verse = w
        .records(0, &task)
        .await
        .into_iter()
        .find(|r| matches!(r.verse.payload, Payload::Tome { .. }))
        .ok_or("no Tome verse")?
        .verse;
    w.alloc
        .addendum(&task, tome_verse.seal.sum, late.seal.sum)
        .await
        .map_err(|e| e.to_string())?;
    step(&mut w).await?;
    let primary = w.replicas[0].clone();
    runo_scribe::verify(&*primary, &tome)
        .await
        .map_err(|e| e.to_string())?;
    let again = runo_scribe::seal(&*primary, &close)
        .await
        .map_err(|e| e.to_string())?;
    *trace = w.trace;
    ensure(again.tome == tome, "the tome changed after a late verse")
}

/// Closure races a relaunch, for the same or another singer: no activation escapes the closing
/// task, and the snapshot's grant set is the one closed.
async fn close_vs_relaunch(seed: u64, trace: &mut Vec<String>) -> Run {
    let mut w = World::new(seed, 3, 1);
    let task = t("T");
    for i in 0..2 {
        let v = w.c.claim(&lane(i), &task, Epoch(0), None);
        w.deliver(&v).await;
    }
    if w.rng.chance(50) {
        w.relaunch(&lane(0), &task, Epoch(0))
            .await
            .map_err(|e| e.to_string())?;
    }
    step(&mut w).await?;
    let who = lane(w.rng.pick(2));
    let close_first = w.rng.chance(50);
    let (closed, relaunched) = {
        let c = w.alloc.close(&task);
        let r = w.alloc.relaunch(&who, &task);
        if close_first {
            tokio::join!(c, r)
        } else {
            let (r, c) = tokio::join!(r, c);
            (c, r)
        }
    };
    w.note(format!(
        "close first: {close_first}; relaunch {}: {relaunched:?}",
        who.0
    ));
    let close = closed.map_err(|e| e.to_string())?;
    let Payload::Close { snapshot, .. } = &close.payload else {
        return Err("not a Close".into());
    };
    let snap: runo_core::ClosureSnapshot = runo_core::codec::decode(
        &runo_store::ScribeRead::blob(&*w.replicas[0], &snapshot.hash)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("snapshot blob missing")?,
    )
    .map_err(|e| e.to_string())?;
    let records = w.records(0, &task).await;
    let closing_seq = records
        .iter()
        .find(|r| matches!(r.verse.payload, Payload::Closing { .. }))
        .map(|r| r.verse.header.seq)
        .ok_or("no Closing verse")?;
    let late_activation = records.iter().any(|r| {
        matches!(r.verse.payload, Payload::Activation { .. }) && r.verse.header.seq > closing_seq
    });
    let granted: BTreeSet<VerseId> = records
        .iter()
        .filter(|r| matches!(r.verse.payload, Payload::Grant { .. }))
        .map(|r| r.verse.seal.sum)
        .collect();
    let in_snapshot: BTreeSet<VerseId> = snap.grants.iter().copied().collect();
    step(&mut w).await?;
    *trace = w.trace;
    ensure(!late_activation, "an activation escaped the closing task")?;
    ensure(
        granted == in_snapshot,
        format!("granted {granted:?}, snapshot {in_snapshot:?}"),
    )?;
    ensure(
        close_first
            .then_some(())
            .is_none_or(|()| matches!(relaunched, Err(runo_fence::FenceError::Closing))),
        "a relaunch after Closing must be refused",
    )
}

/// Work, a delayed signed verse, then tome and addendum on a closed task: the work is refused,
/// the delayed verse archived, tome and addendum recorded.
async fn after_close(seed: u64, trace: &mut Vec<String>) -> Run {
    let mut w = World::new(seed, 3, 1);
    let (l, task) = (lane(0), t("T"));
    let claim = w.c.claim(&l, &task, Epoch(0), None);
    w.deliver(&claim).await;
    let delayed = w.c.work(&l, &task, Epoch(0), None);
    step(&mut w).await?;
    let (_, _) = w.close_and_seal(&task).await?;
    step(&mut w).await?;
    let work = w.c.work(&l, &task, Epoch(0), None);
    let mut outcomes = Vec::new();
    let mut both = [delayed, work];
    w.rng.shuffle(&mut both);
    for v in &both {
        w.deliver(v).await;
        step(&mut w).await?;
    }
    for v in &both {
        outcomes.push(w.class(0, &v.seal.sum).await);
    }
    let tome = w
        .records(0, &task)
        .await
        .into_iter()
        .find(|r| matches!(r.verse.payload, Payload::Tome { .. }))
        .ok_or("no Tome")?;
    let add = w
        .alloc
        .addendum(&task, tome.verse.seal.sum, both[0].seal.sum)
        .await
        .map_err(|e| e.to_string())?;
    let classes: Vec<RecordClass> = w
        .records(0, &task)
        .await
        .into_iter()
        .filter(|r| r.verse.seal.sum == tome.verse.seal.sum || r.verse.seal.sum == add.seal.sum)
        .map(|r| r.class)
        .collect();
    w.check().await?;
    *trace = w.trace;
    ensure(
        outcomes.iter().all(|c| *c == Some(RecordClass::History)),
        format!("after closure: {outcomes:?}"),
    )?;
    ensure(
        classes == [RecordClass::Control, RecordClass::Control],
        format!("tome and addendum: {classes:?}"),
    )
}

/// Two nodes read one stored `Claimed`, then re-read it: each establishes the task once and
/// neither resets it.
#[allow(clippy::unnecessary_wraps, reason = "every scenario returns a Run")]
fn two_readers(seed: u64, trace: &mut Vec<String>) -> Run {
    let mut rng = Rng::new(seed);
    let claim = claimed("lane", false);
    let mut readers = [Clocks::new(clock_config()), Clocks::new(clock_config())];
    let me = SingerId("reader".into());
    let first = [rng.below(5), rng.below(5)];
    for (i, r) in readers.iter_mut().enumerate() {
        r.observe(Instant(first[i] * MIN), &me, Observation::Verse(&claim));
    }
    let reread = 10 + rng.below(10);
    for r in &mut readers {
        r.observe(Instant(reread * MIN), &me, Observation::Verse(&claim));
    }
    let now = 25;
    for (i, r) in readers.iter().enumerate() {
        let task_elapsed = r.rows(Instant(now * MIN)).first().map(|row| row.clocks[2]);
        trace.push(format!(
            "reader {i}: first read at {} min, re-read at {reread}: {task_elapsed:?}",
            first[i]
        ));
        let want = Millis((now - first[i]) * 60_000);
        let ok = matches!(task_elapsed, Some(runo_clock::ClockState::Running { elapsed, .. }) if elapsed == want);
        ensure(
            ok,
            format!("reader {i} task clock {task_elapsed:?}, want {want:?}"),
        )?;
    }
    Ok(())
}

/// A lane blocks for longer than its task deadline, then resumes: only the waiting clock
/// escalates, and task time resumes from its pre-wait value.
#[allow(clippy::unnecessary_wraps, reason = "every scenario returns a Run")]
fn wait_overdue(seed: u64, trace: &mut Vec<String>) -> Run {
    let mut rng = Rng::new(seed);
    let me = SingerId("watcher".into());
    let mut c = Clocks::new(Config {
        reachability: None,
        ..clock_config()
    });
    c.observe(Instant(0), &me, Observation::Verse(&claimed("lane", false)));
    let block_at = 5 + rng.below(10);
    let block_for = minutes_of(Level::Suspected) + 5 + rng.below(30);
    let blocked = raw(
        "lane",
        1,
        Payload::Boundary {
            event: "waiting".into(),
            state: None,
            blocked_on: Some("review".into()),
        },
    );
    let resumed = raw(
        "lane",
        2,
        Payload::Boundary {
            event: "working".into(),
            state: None,
            blocked_on: None,
        },
    );
    let mut task_escalated = false;
    let mut waiting_escalated = false;
    for m in 1..=block_at + block_for {
        let now = Instant(m * MIN);
        if m == block_at {
            c.observe(now, &me, Observation::Verse(&blocked));
        }
        for e in c.tick(now) {
            task_escalated |= e.clock == Clock::Task;
            waiting_escalated |= e.clock == Clock::Waiting;
        }
    }
    let end = block_at + block_for;
    c.observe(Instant(end * MIN), &me, Observation::Verse(&resumed));
    let later = end + 3;
    let task = c.rows(Instant(later * MIN)).first().map(|r| r.clocks[2]);
    trace.push(format!(
        "blocked at {block_at} for {block_for} min; task clock {task:?}"
    ));
    ensure(
        waiting_escalated && !task_escalated,
        "only the waiting clock may escalate while blocked",
    )?;
    let want = Millis((block_at + 3) * 60_000);
    ensure(
        matches!(task, Some(runo_clock::ClockState::Running { elapsed, .. }) if elapsed == want),
        format!("task time {task:?}, want {want:?}"),
    )
}

/// The allocator crashes after persisting the snapshot hash and before publishing `Close`; a
/// late signed fork enters the archive before restart. The published snapshot equals the
/// persisted one byte for byte, the fork is not in it, and `Close` is published once.
async fn close_crash(seed: u64, trace: &mut Vec<String>) -> Run {
    use std::sync::Arc;

    use runo_core::Timestamp;
    use runo_fence::Allocator;
    use runo_fence::journal::MemJournal;
    use runo_store::SongStore;

    let mut w = World::new(seed, 3, 1);
    let (l, task) = (lane(0), t("T"));
    let claim = w.c.claim(&l, &task, Epoch(0), None);
    w.deliver(&claim).await;
    let mut last = claim;
    for _ in 0..=w.rng.below(3) {
        last = w.c.work(&l, &task, Epoch(0), None);
        w.deliver(&last).await;
    }
    step(&mut w).await?;
    w.replicas[0].faults.fail_records.store(1, Ordering::SeqCst);
    let crashed = w.alloc.close(&task).await;
    ensure(crashed.is_err(), "the crash should stop Close")?;
    let state = runo_fence::AllocatorJournal::read(w.alloc.journal()).map_err(|e| e.to_string())?;
    let persisted = state
        .lifecycle(&task)
        .snapshot
        .ok_or("the snapshot hash was not persisted first")?;
    w.note("the allocator crashed before Close");
    let mut fork = last.clone();
    fork.header.note = Some(format!("late variant {seed}"));
    let fork =
        runo_core::sign::seal(w.c.key(&l), fork.header, fork.payload).map_err(|e| e.to_string())?;
    let d = w.deliver(&fork).await;
    ensure(
        d[0] == Some(Decision::History),
        format!("the late fork was {d:?}"),
    )?;
    step(&mut w).await?;
    let restarted = Allocator::new(
        w.c.key(&SingerId("allocator".into())).clone(),
        w.c.roster().clone(),
        w.replicas[0].clone() as Arc<dyn SongStore>,
        MemJournal::from_events(&w.alloc.journal().events()),
        Box::new(|| Timestamp("2026-11-03T12:31:00Z".into())),
    )
    .map_err(|e| e.to_string())?;
    let close = restarted.close(&task).await.map_err(|e| e.to_string())?;
    let Payload::Close { snapshot, .. } = &close.payload else {
        return Err("not a Close".into());
    };
    let closes = w
        .records(0, &task)
        .await
        .iter()
        .filter(|r| matches!(r.verse.payload, Payload::Close { .. }))
        .count();
    let snap: runo_core::ClosureSnapshot = runo_core::codec::decode(
        &runo_store::ScribeRead::blob(&*w.replicas[0], &persisted)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("persisted snapshot blob missing")?,
    )
    .map_err(|e| e.to_string())?;
    w.check().await?;
    *trace = w.trace;
    ensure(
        snapshot.hash == persisted,
        "the published snapshot is not the persisted one",
    )?;
    ensure(snap.forks.is_empty(), "the late fork entered the snapshot")?;
    ensure(closes == 1, format!("Close published {closes} times"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use runo_core::fence::Durability;
    use runo_core::{Epoch, ResourceId, TaskId};
    use runo_fence::Outcome;
    use runo_store::mem::MemStore;
    use runo_testkit::faults::Faulty;
    use runo_testkit::fixture::lane;

    use crate::world::World;

    /// The checks are not vacuous: a replica that forgets its fences on restart (the bug
    /// `FailurePlan::forget_fences_on_restart` names) lets a stale write in, and S1 says so.
    #[tokio::test]
    async fn a_replica_that_forgets_its_fences_is_caught() {
        let mut w = World::new(7, 3, 2);
        let (l, t) = (lane(0), TaskId("T".into()));
        let claim = w.c.claim(&l, &t, Epoch(0), None);
        w.deliver(&claim).await;
        let Ok(Outcome::Activated { .. }) = w.relaunch(&l, &t, Epoch(0)).await else {
            panic!("activates")
        };
        w.check().await.unwrap();
        w.replicas[1] = Arc::new(Faulty::new(Arc::new(MemStore::simulated(
            ResourceId("replica-1".into()),
            Durability::Host,
        ))));
        w.deliver(&claim).await;
        let stale = w.c.work(&l, &t, Epoch(0), None);
        w.deliver(&stale).await;
        let err = w.check().await.unwrap_err();
        assert!(err.starts_with("S1:"), "{err}");
    }

    /// Every scenario under a handful of seeds, in `cargo test`; the full sweep is
    /// `cargo run -p runo-sim --release` (1,000 seeds each).
    #[tokio::test]
    async fn every_scenario_holds_for_a_few_seeds() {
        for name in super::ALL {
            for seed in 0..4 {
                let (r, trace) = super::run(name, seed).await;
                assert!(r.is_ok(), "{name} seed {seed}: {r:?}\n{}", trace.join("\n"));
            }
        }
    }
}
