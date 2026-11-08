//! The allocator against a real store (technical spec §8): relaunch, retry, closure, and the
//! crash the spec names (`close-crash`).

#![allow(
    clippy::many_single_char_names,
    reason = "store, chorus, lane, task and allocator read better short in tests"
)]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use runo_core::codec::decode;
use runo_core::fence::{Durability, Fence};
use runo_core::ingress::{Decision, RecordClass, TaskState, admit};
use runo_core::{ClosureSnapshot, Epoch, Payload, ResourceId, SingerId, TaskId, Timestamp};
use runo_fence::journal::MemJournal;
use runo_fence::{Allocator, AllocatorJournal, FenceError, FenceGuard, Outcome};
use runo_store::mem::MemStore;
use runo_store::{ResourceFenceStore, ScribeRead, SongStore, VerseReader, VerseWriter};
use runo_testkit::faults::Faulty;
use runo_testkit::fixture::{Chorus, lane};

fn task(s: &str) -> TaskId {
    TaskId(s.into())
}

fn store() -> Arc<MemStore> {
    Arc::new(MemStore::simulated(
        ResourceId("song".into()),
        Durability::Host,
    ))
}

fn allocator<J: AllocatorJournal>(
    c: &Chorus,
    store: Arc<dyn SongStore>,
    journal: J,
) -> Allocator<J> {
    let key = c.key(&SingerId("allocator".into())).clone();
    let roster = runo_core::ingress::RosterHistory::at(c, c.current())
        .unwrap()
        .clone();
    Allocator::new(
        key,
        roster,
        store,
        journal,
        Box::new(|| Timestamp("2026-11-04T12:00:00Z".into())),
    )
    .unwrap()
}

trait Current {
    fn current(&self) -> runo_core::RosterVersion;
}
impl Current for Chorus {
    fn current(&self) -> runo_core::RosterVersion {
        runo_core::ingress::RosterHistory::current(self)
    }
}

async fn put(store: &MemStore, c: &Chorus, v: &runo_core::Verse) -> Decision {
    let d = admit(v, c, store);
    match &d {
        Decision::Append { .. } if v.header.grant.is_some() => {
            store.append_fenced(v, Durability::Host).await.unwrap();
        }
        Decision::Append { .. } => {
            store.append(v, Durability::Host).await.unwrap();
        }
        Decision::History => {
            store.archive(v, Durability::Host).await.unwrap();
        }
        _ => {}
    }
    d
}

#[tokio::test]
async fn relaunch_fences_the_old_incarnation_out() {
    let (s, mut c) = (store(), Chorus::new(1));
    let (l, t) = (lane(0), task("HOD-42"));
    {
        let v = c.claim(&l, &t, Epoch(0), None);
        put(&s, &c, &v).await
    };
    let paused = c.work(&l, &t, Epoch(0), None);
    let a = allocator(&c, s.clone(), MemJournal::new());

    let Outcome::Activated { grant, activation } = a.relaunch(&l, &t).await.unwrap() else {
        panic!("one resource, the song, acknowledged")
    };
    assert_eq!(
        s.installed_fence(&l, &t).await.unwrap(),
        Some(Fence::Epoch(Epoch(1)))
    );
    let act = s.get(&activation).await.unwrap().unwrap().verse;
    assert!(
        matches!(&act.payload, Payload::Activation { grant: g, acks } if *g == grant && acks.len() == 1
            && acks[0].grant == grant && acks[0].fence == Fence::Epoch(Epoch(1))),
        "the activation carries the store's own ack: {act:?}"
    );
    // The allocator's verses pass ingress like anyone's.
    for id in [grant, activation] {
        let v = s.get(&id).await.unwrap().unwrap().verse;
        assert!(
            matches!(admit(&v, &c, &*s), Decision::Stored { .. }),
            "{v:?}"
        );
    }
    // The paused old incarnation's write is history; the replacement's is work.
    assert_eq!(put(&s, &c, &paused).await, Decision::History);
    let claim = c.claim(&l, &t, Epoch(1), Some(grant));
    assert!(matches!(put(&s, &c, &claim).await, Decision::Append { .. }));

    // Any other resource checks the same rule.
    let guard = FenceGuard {
        fences: &*s,
        verses: &*s,
        allocator: SingerId("allocator".into()),
    };
    guard
        .admit(&l, &t, Epoch(1), &grant, &activation)
        .await
        .unwrap();
    assert!(
        guard
            .admit(&l, &t, Epoch(0), &grant, &activation)
            .await
            .is_err(),
        "old epoch"
    );

    // A second relaunch is a new epoch.
    let Outcome::Activated { .. } = a.relaunch(&l, &t).await.unwrap() else {
        panic!()
    };
    assert_eq!(
        s.installed_fence(&l, &t).await.unwrap(),
        Some(Fence::Epoch(Epoch(2)))
    );
}

#[tokio::test]
async fn a_missing_ack_leaves_the_grant_unactivated_and_a_retry_reuses_it() {
    let c = Chorus::new(1);
    let (l, t) = (lane(0), task("HOD-43"));
    let s = Arc::new(Faulty::new(store()));
    s.faults.fail_installs.store(1, Ordering::SeqCst);
    let a = allocator(&c, s.clone(), MemJournal::new());
    let Outcome::NotActivated { grant, missing } = a.relaunch(&l, &t).await.unwrap() else {
        panic!("the install failed")
    };
    assert_eq!(missing, [ResourceId("song".into())]);
    let Outcome::Activated { grant: again, .. } = a.relaunch(&l, &t).await.unwrap() else {
        panic!("the retry installs")
    };
    assert_eq!(again, grant, "the same grant, never a second epoch");
    let grants = s
        .task_records(&t)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| matches!(r.verse.payload, Payload::Grant { .. }))
        .count();
    assert_eq!(grants, 1);
}

#[tokio::test]
async fn closure_closes_every_fence_and_names_one_snapshot() {
    let (s, mut c) = (store(), Chorus::new(2));
    let t = task("HOD-44");
    for i in 0..2 {
        {
            let v = c.claim(&lane(i), &t, Epoch(0), None);
            put(&s, &c, &v).await
        };
        {
            let v = c.work(&lane(i), &t, Epoch(0), None);
            put(&s, &c, &v).await
        };
    }
    let a = allocator(&c, s.clone(), MemJournal::new());
    let Outcome::Activated { grant, .. } = a.relaunch(&lane(1), &t).await.unwrap() else {
        panic!()
    };
    {
        let v = c.claim(&lane(1), &t, Epoch(1), Some(grant));
        put(&s, &c, &v).await
    };

    let close = a.close(&t).await.unwrap();
    assert_eq!(s.task_state(&t).await.unwrap(), TaskState::Closed);
    assert_eq!(
        s.get(&close.seal.sum).await.unwrap().unwrap().class,
        RecordClass::Control
    );
    let snap: ClosureSnapshot =
        decode(&s.snapshot(&close.seal.sum).await.unwrap().unwrap()).unwrap();
    assert_eq!(snap.grants, [grant]);
    let ends: Vec<(String, u64, Option<u64>)> = snap
        .chains
        .iter()
        .map(|e| (e.singer.0.clone(), e.epoch.0, e.last_seq))
        .collect();
    assert!(ends.contains(&("lane-0".into(), 0, Some(1))), "{ends:?}");
    assert!(ends.contains(&("lane-1".into(), 1, Some(0))), "{ends:?}");
    assert!(
        ends.iter().any(|(s, _, _)| s == "allocator"),
        "the authority trail is in: {ends:?}"
    );

    // Work after closure is history; relaunch is refused; closing again returns the same Close.
    assert_eq!(
        {
            let v = c.work(&lane(0), &t, Epoch(0), None);
            put(&s, &c, &v).await
        },
        Decision::History
    );
    assert!(matches!(
        a.relaunch(&lane(0), &t).await,
        Err(FenceError::Closing)
    ));
    assert_eq!(a.close(&t).await.unwrap().seal.sum, close.seal.sum);
}

/// `close-crash` (technical spec §12): the allocator crashes after persisting the snapshot hash
/// and before publishing `Close`; a late signed fork enters the archive before it restarts. The
/// published snapshot equals the persisted one, and `Close` is published exactly once.
#[tokio::test]
async fn close_crash_publishes_the_persisted_snapshot_once() {
    let mut c = Chorus::new(1);
    let (l, t) = (lane(0), task("HOD-45"));
    let s = Arc::new(Faulty::new(store()));
    let inner = s.inner().clone();
    {
        let v = c.claim(&l, &t, Epoch(0), None);
        put(&inner, &c, &v).await
    };
    let one = c.work(&l, &t, Epoch(0), None);
    put(&inner, &c, &one).await;

    let journal = MemJournal::new();
    let a = allocator(&c, s.clone(), journal);
    s.faults.fail_records.store(1, Ordering::SeqCst); // Close cannot be published
    assert!(a.close(&t).await.is_err(), "the crash");
    let persisted = a
        .journal()
        .read()
        .unwrap()
        .lifecycle(&t)
        .snapshot
        .expect("persisted first");

    // While it is down, a signed fork of the lane's last verse arrives and is archived.
    let mut fork = one.clone();
    fork.header.note = Some("a late variant".into());
    let fork = runo_core::sign::seal(c.key(&l), fork.header, fork.payload).unwrap();
    assert_eq!(put(&inner, &c, &fork).await, Decision::History);

    // Restart from the journal.
    let restarted = allocator(
        &c,
        s.clone(),
        MemJournal::from_events(&a.journal().events()),
    );
    let close = restarted.close(&t).await.unwrap();
    let Payload::Close { snapshot, .. } = &close.payload else {
        panic!()
    };
    assert_eq!(
        snapshot.hash, persisted,
        "the persisted snapshot, byte for byte"
    );
    let snap: ClosureSnapshot = decode(&inner.blob(&persisted).await.unwrap().unwrap()).unwrap();
    assert!(
        snap.forks.is_empty(),
        "the late fork is not in the snapshot"
    );
    let closes = inner
        .task_records(&t)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| matches!(r.verse.payload, Payload::Close { .. }))
        .count();
    assert_eq!(closes, 1, "Close is published exactly once");
}
